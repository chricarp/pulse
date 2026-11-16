//! Glob matching over the parsed patterns from `pulse_contracts::pattern`.
//!
//! Path globs only match clean absolute paths (see [`crate::path`]); anything else is no match.
//! Matching is case-sensitive and treats dotfiles like any other name.

use pulse_contracts::pattern::{Glob, GlobAlternative, GlobToken, PathSegment};

use crate::path::is_clean_absolute;

pub fn matches(glob: &Glob, s: &str) -> bool {
    glob.alternatives.iter().any(|alt| match alt {
        GlobAlternative::Text(tokens) => match_tokens(tokens, s),
        GlobAlternative::Path(segments) => match_path(segments, s),
    })
}

fn match_path(pattern: &[PathSegment], path: &str) -> bool {
    if !is_clean_absolute(path) {
        return false;
    }
    let parts: Vec<&str> = if path == "/" { Vec::new() } else { path[1..].split('/').collect() };
    // dp[i][j]: pattern[i..] matches parts[j..]. Filled from the end; O(pattern × path).
    let (p, n) = (pattern.len(), parts.len());
    let mut dp = vec![vec![false; n + 1]; p + 1];
    dp[p][n] = true;
    for i in (0..p).rev() {
        for j in (0..=n).rev() {
            dp[i][j] = match &pattern[i] {
                PathSegment::AnyDepth => dp[i + 1][j] || (j < n && dp[i][j + 1]),
                PathSegment::Tokens(t) => j < n && dp[i + 1][j + 1] && match_tokens(t, parts[j]),
            };
        }
    }
    dp[0][0]
}

/// Wildcard match where `*` crosses every character (segments never contain `/`).
fn match_tokens(tokens: &[GlobToken], s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    // Backtracking over the last `*`: linear in practice, O(tokens × chars) worst case.
    let (mut ti, mut ci) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    loop {
        if ti < tokens.len() {
            match &tokens[ti] {
                GlobToken::Star => {
                    star = Some((ti, ci));
                    ti += 1;
                    continue;
                }
                GlobToken::Any if ci < chars.len() => {
                    ti += 1;
                    ci += 1;
                    continue;
                }
                GlobToken::Literal(lit) => {
                    let len = lit.chars().count();
                    if ci + len <= chars.len()
                        && chars[ci..ci + len].iter().copied().eq(lit.chars())
                    {
                        ti += 1;
                        ci += len;
                        continue;
                    }
                }
                GlobToken::Any => {}
            }
        } else if ci == chars.len() {
            return true;
        }
        match star {
            Some((st, sc)) if sc < chars.len() => {
                star = Some((st, sc + 1));
                ti = st + 1;
                ci = sc + 1;
            }
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use pulse_contracts::pattern::{parse_path_glob, parse_text_glob, parse_type_glob};

    use super::*;

    #[test]
    fn path_globs() {
        let home = Path::new("/home/me");
        // (glob, path, matches)
        let cases = [
            ("~/University/**", "/home/me/University", true),
            ("~/University/**", "/home/me/University/a.pdf", true),
            ("~/University/**", "/home/me/University/x/y/z.pdf", true),
            ("~/University/**", "/home/me/Universityx/a.pdf", false),
            ("~/University/**", "/home/me", false),
            ("~/University/**/*.pdf", "/home/me/University/a.pdf", true),
            ("~/University/**/*.pdf", "/home/me/University/x/y/a.pdf", true),
            ("~/University/**/*.pdf", "/home/me/University/a.pdf.exe", false),
            ("~/University/*.pdf", "/home/me/University/x/a.pdf", false),
            ("~/University/*", "/home/me/University/.git", true),
            ("/a/**/b/**/c", "/a/b/c", true),
            ("/a/**/b/**/c", "/a/x/b/y/z/c", true),
            ("/a/**/b/**/c", "/a/x/y/c", false),
            ("/a/?.txt", "/a/1.txt", true),
            ("/a/?.txt", "/a/12.txt", false),
            ("/a/*.{pdf,pptx}", "/a/x.pptx", true),
            ("/a/*.{pdf,pptx}", "/a/x.doc", false),
            ("/**", "/", true),
            ("/**", "/anything/at/all", true),
            ("/a/b", "/a/b", true),
            ("/a/b", "/a/B", false),
            // Unclean inputs never match, so they cannot walk out of a granted tree.
            ("/home/me/University/**", "/home/me/University/../.ssh/id_rsa", false),
            ("/home/me/University/**", "/home/me/University/./a", false),
            ("/home/me/University/**", "/home/me/University//a", false),
            ("/home/me/University/**", "home/me/University/a", false),
            ("/home/me/University/**", "/home/me/University/a/", false),
            ("/a/*", "/a/\u{0}b", false),
        ];
        for (glob, path, want) in cases {
            let g = parse_path_glob(glob, home).unwrap();
            assert_eq!(matches(&g, path), want, "{glob} vs {path}");
        }
    }

    #[test]
    fn text_and_type_globs() {
        let cases = [
            (parse_type_glob("fs.*"), "fs.file.created", true),
            (parse_type_glob("fs.*"), "fs.burst", true),
            (parse_type_glob("fs.*"), "fsx.file", false),
            (parse_type_glob("fs.file.*"), "fs.burst", false),
            (parse_type_glob("*"), "anything.at.all", true),
            (parse_type_glob("*.created"), "fs.file.created", true),
            (parse_type_glob("git.commit.created"), "git.commit.created", true),
            (parse_type_glob("git.commit.created"), "git.commit.created2", false),
            (parse_text_glob("release/*"), "release/1.2/rc", true),
            (parse_text_glob("main"), "main", true),
            (parse_text_glob("ma?n"), "main", true),
            (parse_text_glob("*fix*"), "a hotfix branch", true),
            (parse_text_glob("été*"), "été 2026", true),
            (parse_text_glob(""), "", true),
            (parse_text_glob(""), "x", false),
            (parse_text_glob("a*b*c"), "aXbYbZc", true),
            (parse_text_glob("a*b*c"), "aXbYbZ", false),
        ];
        for (glob, s, want) in cases {
            let g = glob.unwrap();
            assert_eq!(matches(&g, s), want, "{} vs {s}", g.pattern);
        }
    }
}
