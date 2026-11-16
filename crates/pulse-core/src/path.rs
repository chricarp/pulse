//! Lexical path rules.
//!
//! The core never touches the filesystem, so it cannot resolve symlinks or `..`. Instead it
//! requires **clean absolute paths**: starting with `/`, with no empty, `.` or `..` segments, no
//! trailing slash (except `/` itself) and no NUL. Anything else fails closed: it matches no path
//! glob and is outside every scope. Sources and the daemon are responsible for emitting clean
//! paths (canonicalizing where the platform allows).

/// Whether `p` is a clean absolute path.
pub fn is_clean_absolute(p: &str) -> bool {
    if p == "/" {
        return true;
    }
    let Some(rest) = p.strip_prefix('/') else { return false };
    !p.contains('\0') && rest.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
}

/// Whether clean absolute `path` is `dir` or inside it. Both must be clean; otherwise `false`.
pub fn is_within(path: &str, dir: &str) -> bool {
    if !is_clean_absolute(path) || !is_clean_absolute(dir) {
        return false;
    }
    if dir == "/" {
        return true;
    }
    path == dir || path.strip_prefix(dir).is_some_and(|rest| rest.starts_with('/'))
}

/// Longest path an agent may report; anything longer is not believed (PATH_MAX on Linux).
const REPORTED_MAX: usize = 4096;

/// A path an agent reported writing, made clean and absolute: a relative path is joined to
/// `base` (the run's working directory), and empty and `.` segments are dropped. `None` when
/// that cannot be done without the filesystem: a `..` segment (lexically wrong across
/// symlinks), a `~`, NUL, an unclean `base`, or more than 4096 bytes. The text is untrusted.
pub fn resolve_reported(raw: &str, base: &str) -> Option<String> {
    if raw.is_empty() || raw.len() > REPORTED_MAX || raw.starts_with('~') || raw.contains('\0') {
        return None;
    }
    let joined = if raw.starts_with('/') {
        raw.to_owned()
    } else if is_clean_absolute(base) {
        format!("{base}/{raw}")
    } else {
        return None;
    };
    let mut out = String::with_capacity(joined.len());
    for segment in joined.split('/').filter(|s| !s.is_empty() && *s != ".") {
        if segment == ".." {
            return None;
        }
        out.push('/');
        out.push_str(segment);
    }
    if out.is_empty() {
        out.push('/');
    }
    Some(out)
}

/// `path` spelled under `to` instead of `from`, if it is `from` or inside it. All must be clean.
pub fn rebase(path: &str, from: &str, to: &str) -> Option<String> {
    if !is_within(path, from) || !is_clean_absolute(to) {
        return None;
    }
    let rest = if from == "/" { path } else { &path[from.len()..] };
    Some(match (to, rest) {
        (_, "") => to.to_owned(),
        ("/", rest) => rest.to_owned(),
        (to, rest) => format!("{to}{rest}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reported_paths() {
        let cases = [
            ("/w/a.md", "/w", Some("/w/a.md")),
            ("a.md", "/w", Some("/w/a.md")),
            ("./notes//a.md", "/w", Some("/w/notes/a.md")),
            ("/w/./a.md/", "/w", Some("/w/a.md")),
            ("/", "/w", Some("/")),
            ("../a.md", "/w", None),
            ("/w/x/../a.md", "/w", None),
            ("~/a.md", "/w", None),
            ("a\0b", "/w", None),
            ("", "/w", None),
            ("a.md", "w", None),
            ("a.md", "/w/../v", None),
        ];
        for (raw, base, want) in cases {
            assert_eq!(resolve_reported(raw, base).as_deref(), want, "{raw:?} in {base:?}");
        }
        assert_eq!(resolve_reported(&format!("/{}", "a".repeat(4096)), "/"), None);
    }

    #[test]
    fn rebased() {
        let cases = [
            ("/private/tmp/w/a", "/private/tmp/w", "/tmp/w", Some("/tmp/w/a")),
            ("/private/tmp/w", "/private/tmp/w", "/tmp/w", Some("/tmp/w")),
            ("/a/b", "/", "/x", Some("/x/a/b")),
            ("/x/a", "/x", "/", Some("/a")),
            ("/private/tmp/wx", "/private/tmp/w", "/tmp/w", None),
        ];
        for (path, from, to, want) in cases {
            assert_eq!(rebase(path, from, to).as_deref(), want, "{path} {from} {to}");
        }
    }

    #[test]
    fn clean_paths() {
        for ok in ["/", "/a", "/a/b.c", "/a/.hidden", "/a/..b", "/a/b c"] {
            assert!(is_clean_absolute(ok), "{ok:?}");
        }
        for bad in ["", "a", "a/b", "/a/", "//a", "/a//b", "/a/./b", "/a/../b", "/..", "/a/\0"] {
            assert!(!is_clean_absolute(bad), "{bad:?}");
        }
    }

    #[test]
    fn within() {
        let cases = [
            ("/a/b", "/a", true),
            ("/a", "/a", true),
            ("/ab", "/a", false),
            ("/a/b", "/", true),
            ("/a/../etc", "/a", false),
            ("/a/b", "/a/", false),
        ];
        for (path, dir, want) in cases {
            assert_eq!(is_within(path, dir), want, "{path} in {dir}");
        }
    }
}
