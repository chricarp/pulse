//! Which paths under a watched root are ignored.
//!
//! A path is ignored when any of its components below the root is ignored, so an ignored
//! directory hides its whole subtree (and is not watched at all where the platform watches per
//! directory). Built-in rules cover version-control internals, dependency and cache directories,
//! editor swap and backup files, download temp files and OS metadata files.
//!
//! User patterns (`ignore = [...]`) follow gitignore's intuition:
//! - no `/` (`*.log`, `build`): matched against every component's name, at any depth;
//! - starting with `/` or `~`: an absolute path glob;
//! - otherwise (`docs/drafts/**`, `**/out`): a path glob relative to the root.
//!
//! A path glob matches a directory and everything under it when it matches the directory's path,
//! so `build/**`, `build` and `**/build` all hide the `build` directory.

use std::cell::RefCell;
use std::path::Path;

use pulse_contracts::pattern::{Glob, IgnorePattern, parse_ignore};

/// Names ignored wherever they appear.
const EXACT: &[&str] = &[
    // Version control internals. The git source watches `.git` itself.
    ".git",
    ".hg",
    ".svn",
    ".jj",
    ".bzr",
    "_darcs",
    // Dependencies, caches and tool state that change in storms and mean nothing to a user.
    "node_modules",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    ".nox",
    ".direnv",
    ".idea",
    // OS metadata.
    ".DS_Store",
    ".AppleDouble",
    ".LSOverride",
    ".Spotlight-V100",
    ".fseventsd",
    ".TemporaryItems",
    ".Trashes",
    ".DocumentRevisions-V100",
    "Thumbs.db",
    "ehthumbs.db",
    "desktop.ini",
    ".directory",
    // Vim writes this to test whether a directory is writable.
    "4913",
];

const PREFIXES: &[&str] = &[
    "._",              // AppleDouble resource forks
    ".#",              // Emacs lock files
    ".~lock.",         // LibreOffice lock files
    "~$",              // Microsoft Office owner files
    ".goutputstream-", // GLib atomic-save temp files
    ".Trash",          // .Trash, .Trash-1000
];

/// Suffixes of editor swap/backup files, generic temp files and in-progress downloads.
const SUFFIXES: &[&str] = &[
    "~",
    ".swp",
    ".swo",
    ".swx",
    ".swpx",
    ".kate-swp",
    ".tmp",
    ".temp",
    "___jb_tmp___",
    "___jb_old___",
];

/// In-progress download suffixes. Also ignored; while such a file exists, the file it will
/// become is held back (see the coalescer), so an empty placeholder is not reported as created.
pub(crate) const DOWNLOAD_SUFFIXES: &[&str] =
    &[".part", ".crdownload", ".download", ".partial", ".opdownload"];

fn builtin_name(name: &str) -> bool {
    EXACT.contains(&name)
        || PREFIXES.iter().any(|p| name.starts_with(p))
        || SUFFIXES.iter().chain(DOWNLOAD_SUFFIXES).any(|s| name.ends_with(s) && name.len() > s.len())
        // Emacs auto-save files: `#name#`.
        || (name.len() > 2 && name.starts_with('#') && name.ends_with('#'))
}

/// The file a download temp file will become: `/d/a.pdf.part` → `/d/a.pdf`.
pub(crate) fn download_target(path: &str) -> Option<&str> {
    let name = path.rsplit('/').next()?;
    DOWNLOAD_SUFFIXES.iter().find_map(|s| {
        (name.len() > s.len() && name.ends_with(s)).then(|| &path[..path.len() - s.len()])
    })
}

#[derive(Debug)]
pub(crate) struct Ignore {
    root: String,
    names: Vec<Glob>,
    paths: Vec<Glob>,
    /// `recursive = false`: everything below the root's direct children is ignored.
    recursive: bool,
    /// The last directory checked and whether it is ignored: signals arrive in runs per
    /// directory, and the glob check per component is the expensive part.
    last_dir: RefCell<Option<(String, bool)>>,
}

impl Ignore {
    /// `root` is a clean absolute path; `home` expands `~` in patterns.
    pub(crate) fn new(root: &str, patterns: &[String], home: &Path) -> Result<Self, String> {
        let mut names = Vec::new();
        let mut paths = Vec::new();
        for raw in patterns {
            match parse_ignore(raw, root, home) {
                Ok(IgnorePattern::Name(g)) => names.push(g),
                Ok(IgnorePattern::Path(g)) => paths.push(g),
                Err(e) => return Err(format!("ignore pattern {raw:?}: {e}")),
            }
        }
        Ok(Ignore {
            root: root.to_owned(),
            names,
            paths,
            recursive: true,
            last_dir: RefCell::new(None),
        })
    }

    pub(crate) fn recursive(mut self, recursive: bool) -> Self {
        self.recursive = recursive;
        self
    }

    /// Whether subdirectories are watched at all.
    pub(crate) fn descends(&self) -> bool {
        self.recursive
    }

    fn component_ignored(&self, prefix: &str, name: &str) -> bool {
        builtin_name(name)
            || self.names.iter().any(|g| pulse_core::glob::matches(g, name))
            || self.paths.iter().any(|g| pulse_core::glob::matches(g, prefix))
    }

    /// Whether `path` (under the root) is ignored. Paths outside the root are ignored.
    pub(crate) fn is_ignored(&self, path: &str) -> bool {
        let Some(rest) = self.relative(path) else { return true };
        if rest.is_empty() {
            return false;
        }
        if !self.recursive && rest.contains('/') {
            return true;
        }
        let (dir_len, name) = match rest.rfind('/') {
            Some(i) => (self.root_len() + 1 + i, &rest[i + 1..]),
            None => (self.root.len(), rest),
        };
        self.dir_ignored(&path[..dir_len]) || self.component_ignored(path, name)
    }

    fn root_len(&self) -> usize {
        if self.root == "/" { 0 } else { self.root.len() }
    }

    fn relative<'a>(&self, path: &'a str) -> Option<&'a str> {
        if path == self.root {
            return Some("");
        }
        if self.root == "/" {
            return path.strip_prefix('/');
        }
        path.strip_prefix(self.root.as_str())?.strip_prefix('/')
    }

    fn dir_ignored(&self, dir: &str) -> bool {
        if let Some((d, ignored)) = self.last_dir.borrow().as_ref() {
            if d == dir {
                return *ignored;
            }
        }
        let ignored = self.walk_dir(dir);
        *self.last_dir.borrow_mut() = Some((dir.to_owned(), ignored));
        ignored
    }

    fn walk_dir(&self, dir: &str) -> bool {
        let Some(rest) = self.relative(dir) else { return true };
        if rest.is_empty() {
            return false;
        }
        let base = self.root_len();
        let mut end = base;
        for name in rest.split('/') {
            end += 1 + name.len();
            if self.component_ignored(&dir[..end], name) {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_and_user_rules() {
        let ig = Ignore::new(
            "/w",
            &["*.log".into(), "build".into(), "docs/drafts/**".into(), "~/elsewhere/**".into()],
            Path::new("/home/me"),
        )
        .unwrap();
        let ignored = [
            "/w/.git/HEAD",
            "/w/a/node_modules/x/index.js",
            "/w/.notes.md.swp",
            "/w/notes.md~",
            "/w/.#notes.md",
            "/w/#notes.md#",
            "/w/4913",
            "/w/a/.DS_Store",
            "/w/._a.pdf",
            "/w/a.pdf.crdownload",
            "/w/a.pdf.part",
            "/w/x___jb_tmp___",
            "/w/.goutputstream-ABC123",
            "/w/server.log",
            "/w/a/build/out.o",
            "/w/build",
            "/w/docs/drafts",
            "/w/docs/drafts/one.md",
            "/elsewhere/a",
        ];
        for p in ignored {
            assert!(ig.is_ignored(p), "{p} should be ignored");
        }
        let kept = [
            "/w",
            "/w/a.pdf",
            "/w/.gitignore",
            "/w/docs/final.md",
            "/w/builder/x",
            "/w/a/notes.md",
            "/w/.swp",
            "/w/~",
            "/w/#",
            "/w/part",
        ];
        for p in kept {
            assert!(!ig.is_ignored(p), "{p} should be kept");
        }
        // The per-directory cache must not leak a result across directories.
        assert!(ig.is_ignored("/w/build/a"));
        assert!(!ig.is_ignored("/w/a/b"));
        assert!(ig.is_ignored("/w/build/b"));
    }

    #[test]
    fn download_targets() {
        assert_eq!(download_target("/d/a.pdf.part"), Some("/d/a.pdf"));
        assert_eq!(download_target("/d/a.pdf.crdownload"), Some("/d/a.pdf"));
        assert_eq!(download_target("/d/.part"), None);
        assert_eq!(download_target("/d/a.pdf"), None);
    }

    #[test]
    fn bad_patterns_are_config_errors() {
        assert!(Ignore::new("/w", &["a/**b".into()], Path::new("/h")).is_err());
        assert!(Ignore::new("/w", &["/".into()], Path::new("/h")).is_err());
    }
}
