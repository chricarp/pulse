//! Reading a repository's files directly: where its git directories are, which branch HEAD is
//! on, whether a name is a branch (loose or packed), a commit's message and author from its
//! loose object, and the conflicted paths in the index.
//!
//! Everything read here is bounded: files have size caps, and strings taken from them are
//! untrusted data and cut to a fixed length.

use std::path::{Path, PathBuf};

/// The directories of one working tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    /// Per-worktree state: HEAD, index, MERGE_HEAD, logs/HEAD.
    pub git_dir: PathBuf,
    /// Shared state: refs, packed-refs, objects, branch and remote reflogs. Equal to `git_dir`
    /// except in linked worktrees.
    pub common_dir: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum LayoutError {
    #[error("{0} has no .git")]
    Missing(PathBuf),
    #[error("{path}: {message}")]
    Invalid { path: PathBuf, message: String },
}

/// Finds the git directories of the working tree at `work`. `.git` may be a directory, or a file
/// (`gitdir: <path>`) in linked worktrees and submodules.
pub(crate) fn discover(work: &Path) -> Result<Layout, LayoutError> {
    let dot = work.join(".git");
    let meta = std::fs::metadata(&dot).map_err(|_| LayoutError::Missing(work.to_owned()))?;
    let git_dir = if meta.is_dir() {
        dot
    } else {
        let text = read_small(&dot, 4096).ok_or_else(|| LayoutError::Invalid {
            path: dot.clone(),
            message: "unreadable".into(),
        })?;
        let target = text.trim().strip_prefix("gitdir:").map(str::trim).ok_or_else(|| {
            LayoutError::Invalid { path: dot.clone(), message: "expected `gitdir: <path>`".into() }
        })?;
        work.join(target)
    };
    if !git_dir.join("HEAD").is_file() {
        return Err(LayoutError::Invalid {
            path: git_dir,
            message: "no HEAD; not a git directory".into(),
        });
    }
    let common_dir = match read_small(&git_dir.join("commondir"), 4096) {
        Some(rel) => git_dir.join(rel.trim()),
        None => git_dir.clone(),
    };
    let git_dir = std::fs::canonicalize(&git_dir).unwrap_or(git_dir);
    let common_dir = std::fs::canonicalize(&common_dir).unwrap_or(common_dir);
    Ok(Layout { git_dir, common_dir })
}

pub(crate) fn read_small(path: &Path, max: u64) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > max {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    String::from_utf8(bytes).ok()
}

/// Cuts `s` to at most `max` bytes on a character boundary.
pub(crate) fn bounded(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

pub(crate) const MAX_NAME: usize = 256;
pub(crate) const MAX_MESSAGE: usize = 4096;
pub(crate) const MAX_IDENT: usize = 256;

impl Layout {
    /// The branch HEAD points at, or `None` when detached (or unreadable).
    pub(crate) fn head_branch(&self) -> Option<String> {
        let head = read_small(&self.git_dir.join("HEAD"), 4096)?;
        head.trim().strip_prefix("ref: refs/heads/").map(|b| bounded(b, MAX_NAME))
    }

    /// Whether `name` is a local branch, loose or packed.
    pub(crate) fn is_branch(&self, name: &str) -> bool {
        // Ref names never contain these; refusing them keeps the path inside `refs/heads`.
        if name.is_empty() || name.contains("..") || name.starts_with('/') || name.contains('\0') {
            return false;
        }
        if self.common_dir.join("refs/heads").join(name).is_file() {
            return true;
        }
        let Some(packed) = read_small(&self.common_dir.join("packed-refs"), 64 << 20) else {
            return false;
        };
        let want = format!("refs/heads/{name}");
        packed.lines().any(|l| {
            !l.starts_with('#')
                && !l.starts_with('^')
                && l.split_once(' ').is_some_and(|(_, r)| r == want)
        })
    }

    /// The object hash size in hex characters: 40 for SHA-1, 64 for SHA-256 repositories.
    pub(crate) fn hash_bytes(&self) -> usize {
        let config = read_small(&self.common_dir.join("config"), 1 << 20).unwrap_or_default();
        let sha256 = config.lines().any(|l| {
            let l = l.trim().to_ascii_lowercase();
            l.replace(' ', "") == "objectformat=sha256"
        });
        if sha256 { 32 } else { 20 }
    }

    /// A commit's author and full message from its loose object. New commits are written as
    /// loose objects; once packed (after `git gc`) this returns `None` and callers fall back to
    /// the reflog's subject and identity.
    pub(crate) fn commit_info(&self, sha: &str) -> Option<(String, String)> {
        if sha.len() < 3 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let path = self.common_dir.join("objects").join(&sha[..2]).join(&sha[2..]);
        let meta = std::fs::metadata(&path).ok()?;
        if meta.len() > 1 << 20 {
            return None;
        }
        let compressed = std::fs::read(&path).ok()?;
        let raw = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&compressed, 256 * 1024)
            .ok()?;
        let nul = raw.iter().position(|&b| b == 0)?;
        if !raw.starts_with(b"commit ") {
            return None;
        }
        let body = String::from_utf8_lossy(&raw[nul + 1..]);
        let (headers, message) = body.split_once("\n\n").unwrap_or((&body, ""));
        let author = headers.lines().find_map(|l| l.strip_prefix("author "))?;
        // `Name <email> 1794794481 +0100`: drop the time and zone.
        let author = author.rsplitn(3, ' ').nth(2).unwrap_or(author);
        Some((bounded(author, MAX_IDENT), bounded(message.trim_end(), MAX_MESSAGE)))
    }

    /// Whether a merge, rebase, cherry-pick or revert is in progress (the states that can stop
    /// with conflicts).
    pub(crate) fn in_merge_state(&self) -> bool {
        ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD"]
            .iter()
            .any(|f| self.git_dir.join(f).is_file())
            || ["rebase-merge", "rebase-apply"].iter().any(|d| self.git_dir.join(d).is_dir())
    }

    pub(crate) fn index_path(&self) -> PathBuf {
        self.git_dir.join("index")
    }
}

/// The largest index we read. Larger ones are skipped (with a health message upstream).
pub(crate) const MAX_INDEX: u64 = 256 << 20;

/// Conflicted paths (any stage above 0) in an index file, deduplicated, in index order, at most
/// `max`. Supports index versions 2, 3 and 4.
pub(crate) fn conflicted_paths(index: &[u8], hash_bytes: usize, max: usize) -> Option<Vec<String>> {
    let be32 = |b: &[u8], at: usize| -> Option<u32> {
        Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
    };
    if index.get(..4)? != b"DIRC" {
        return None;
    }
    let version = be32(index, 4)?;
    if !(2..=4).contains(&version) {
        return None;
    }
    let count = be32(index, 8)? as usize;
    let mut at = 12;
    let mut out: Vec<String> = Vec::new();
    let mut prev: Vec<u8> = Vec::new();
    // ctime, mtime (8 each), dev, ino, mode, uid, gid, size (4 each), then the object id.
    let fixed = 40 + hash_bytes;
    for _ in 0..count {
        let start = at;
        let flags =
            u16::from_be_bytes(index.get(start + fixed..start + fixed + 2)?.try_into().ok()?);
        at = start + fixed + 2;
        if version >= 3 && flags & 0x4000 != 0 {
            at += 2;
        }
        let stage = (flags >> 12) & 0x3;
        let path: Vec<u8> = if version == 4 {
            // Prefix compression: a varint of bytes to drop from the previous path, then the
            // NUL-terminated rest.
            let (strip, n) = varint(index.get(at..)?)?;
            at += n;
            let end = at + index.get(at..)?.iter().position(|&b| b == 0)?;
            let keep = prev.len().checked_sub(usize::try_from(strip).ok()?)?;
            let mut p = prev[..keep].to_vec();
            p.extend_from_slice(&index[at..end]);
            at = end + 1;
            p
        } else {
            let end = at + index.get(at..)?.iter().position(|&b| b == 0)?;
            let p = index[at..end].to_vec();
            // Entries are padded with 1-8 NULs to a multiple of 8 bytes.
            let len = end - start;
            at = start + (len + 8) / 8 * 8;
            p
        };
        if stage > 0 {
            if let Ok(s) = std::str::from_utf8(&path) {
                if out.last().map(String::as_str) != Some(s) && out.len() < max {
                    out.push(s.to_owned());
                }
            }
        }
        prev = path;
    }
    Some(out)
}

/// Git's offset varint (used by index v4): 7 bits per byte, each continuation adds one.
fn varint(b: &[u8]) -> Option<(u64, usize)> {
    let mut i = 0;
    let mut c = *b.first()?;
    let mut v = u64::from(c & 0x7f);
    while c & 0x80 != 0 {
        i += 1;
        c = *b.get(i)?;
        v = v.checked_add(1)?.checked_mul(128)? + u64::from(c & 0x7f);
    }
    Some((v, i + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints() {
        assert_eq!(varint(&[0x05]), Some((5, 1)));
        assert_eq!(varint(&[0x80, 0x00]), Some((128, 2)));
        assert_eq!(varint(&[0x81, 0x01]), Some((257, 2)));
        assert_eq!(varint(&[0x80]), None);
    }

    #[test]
    fn bounded_cuts_on_char_boundaries() {
        assert_eq!(bounded("héllo", 2), "h");
        assert_eq!(bounded("abc", 10), "abc");
    }

    #[test]
    fn index_parsing_rejects_garbage() {
        assert_eq!(conflicted_paths(b"nope", 20, 10), None);
        assert_eq!(conflicted_paths(b"DIRC\0\0\0\x09\0\0\0\0", 20, 10), None);
        assert_eq!(conflicted_paths(b"DIRC\0\0\0\x02\0\0\0\0", 20, 10), Some(vec![]));
        // Claims an entry that is not there.
        assert_eq!(conflicted_paths(b"DIRC\0\0\0\x02\0\0\0\x01", 20, 10), None);
    }
}
