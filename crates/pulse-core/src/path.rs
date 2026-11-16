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

#[cfg(test)]
mod tests {
    use super::*;

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
