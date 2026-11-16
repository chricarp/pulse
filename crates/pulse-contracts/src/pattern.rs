//! Glob syntax and path expansion. Matching lives in `pulse-core`; this module only parses and
//! validates, so config errors are reported at load time with the offending text.
//!
//! Syntax, shared by every glob in Pulse:
//! - `*` matches any run of characters. In a **path glob** it does not cross `/`.
//! - `**` (path globs only) is a whole segment and matches zero or more segments, so
//!   `/a/**` matches `/a`, `/a/b` and `/a/b/c`.
//! - `?` matches one character (not `/` in path globs).
//! - `{x,y}` matches either alternative. No nesting; at most 64 expansions.
//! - `\` escapes the next character.
//!
//! **Type globs** (`on = "fs.*"`, scopes) are text globs over the restricted event type
//! alphabet: `fs.*` matches `fs.file.created` and `fs.burst`.
//!
//! **Path globs** are absolute after expansion, and lexically clean: no empty, `.` or `..`
//! segments. Expansion is explicit: `~` and `~/` take the home directory the caller passes in;
//! relative paths take a base directory only where the caller allows one (project files).

use std::path::Path;

/// One element of a segment (or of a whole text glob).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlobToken {
    Literal(String),
    /// `*`
    Star,
    /// `?`
    Any,
}

/// One `/`-separated part of a path glob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathSegment {
    /// `**`
    AnyDepth,
    Tokens(Vec<GlobToken>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlobAlternative {
    Text(Vec<GlobToken>),
    /// The segments after the leading `/`.
    Path(Vec<PathSegment>),
}

/// A parsed glob: a match against any alternative is a match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glob {
    /// The pattern after expansion (what matching uses), for display.
    pub pattern: String,
    pub alternatives: Vec<GlobAlternative>,
}

impl Glob {
    pub fn is_path(&self) -> bool {
        matches!(self.alternatives.first(), Some(GlobAlternative::Path(_)))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PatternError {
    #[error("unclosed '{{' in {0:?}")]
    UnclosedBrace(String),
    #[error("nested '{{' is not supported in {0:?}")]
    NestedBrace(String),
    #[error("unmatched '}}' in {0:?}")]
    UnmatchedBrace(String),
    #[error("{0:?} expands to more than 64 alternatives")]
    TooManyAlternatives(String),
    #[error("trailing '\\' in {0:?}")]
    TrailingEscape(String),
    #[error("'**' must be a whole path segment (like \"/a/**/b\") in {0:?}")]
    PartialDoubleStar(String),
    #[error("path {0:?} must not contain empty, '.' or '..' segments")]
    UncleanPath(String),
    #[error("path {0:?} must be absolute or start with '~/'")]
    NotAbsolute(String),
    #[error("'~user' paths are not supported: {0:?}")]
    TildeUser(String),
    #[error("home directory {0:?} is not an absolute UTF-8 path")]
    BadHome(String),
    #[error(
        "{0:?} is not a valid event type glob: use lowercase letters, digits, '.', '_', '-' and '*'"
    )]
    BadTypeGlob(String),
    #[error("empty pattern")]
    Empty,
}

/// Expands `~`, `~/…` and (when `base` is given) relative paths, then checks the result is a
/// clean absolute path. Trailing slashes are removed. The home directory is a parameter: this
/// crate never reads the environment.
pub fn expand_path(raw: &str, home: &Path, base: Option<&Path>) -> Result<String, PatternError> {
    let home = home
        .to_str()
        .filter(|h| h.starts_with('/'))
        .ok_or_else(|| PatternError::BadHome(home.display().to_string()))?;
    let joined = if raw == "~" {
        home.to_owned()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        join(home, rest)
    } else if raw.starts_with('~') {
        return Err(PatternError::TildeUser(raw.to_owned()));
    } else if raw.starts_with('/') {
        raw.to_owned()
    } else if let Some(base) = base {
        let base = base
            .to_str()
            .filter(|b| b.starts_with('/'))
            .ok_or_else(|| PatternError::NotAbsolute(base.display().to_string()))?;
        join(base, raw.strip_prefix("./").unwrap_or(raw))
    } else {
        return Err(PatternError::NotAbsolute(raw.to_owned()));
    };
    clean_absolute(&joined).ok_or(PatternError::UncleanPath(raw.to_owned()))
}

fn join(base: &str, rest: &str) -> String {
    if rest.is_empty() || rest == "." {
        return base.to_owned();
    }
    format!("{}/{}", base.trim_end_matches('/'), rest)
}

/// `Some(path without trailing slash)` if `p` is absolute with no empty, `.` or `..` segments.
pub fn clean_absolute(p: &str) -> Option<String> {
    let rest = p.strip_prefix('/')?;
    if rest.is_empty() {
        return Some("/".to_owned());
    }
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.split('/').any(|s| s.is_empty() || s == "." || s == "..") || p.contains('\0') {
        return None;
    }
    Some(format!("/{rest}"))
}

/// Parses a path glob. `raw` may start with `~`; relative globs are rejected.
pub fn parse_path_glob(raw: &str, home: &Path) -> Result<Glob, PatternError> {
    if raw.is_empty() {
        return Err(PatternError::Empty);
    }
    let expanded = expand_path(raw, home, None)?;
    let mut alternatives = Vec::new();
    for alt in expand_braces(&expanded)? {
        let body =
            alt.strip_prefix('/').ok_or_else(|| PatternError::NotAbsolute(raw.to_owned()))?;
        let mut segments = Vec::new();
        if !body.is_empty() {
            for seg in split_unescaped(body, '/') {
                if seg.is_empty() || seg == "." || seg == ".." {
                    return Err(PatternError::UncleanPath(raw.to_owned()));
                }
                if seg == "**" {
                    segments.push(PathSegment::AnyDepth);
                } else if seg.contains("**") {
                    return Err(PatternError::PartialDoubleStar(raw.to_owned()));
                } else {
                    segments.push(PathSegment::Tokens(tokenize(&seg, raw)?));
                }
            }
        }
        alternatives.push(GlobAlternative::Path(segments));
    }
    Ok(Glob { pattern: expanded, alternatives })
}

/// One `ignore` entry of an `fs` source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IgnorePattern {
    /// No `/`: a text glob matched against each name at any depth (`*.log`, `build`).
    Name(Glob),
    /// A path glob: absolute, `~/…`, or (containing a `/`) relative to the watched directory.
    Path(Glob),
}

/// Parses an `ignore` entry of an `fs` source watching `root` (a clean absolute path). A
/// trailing `/` is dropped. The validator and the source both read entries through this, so
/// what loads is what the source accepts.
pub fn parse_ignore(raw: &str, root: &str, home: &Path) -> Result<IgnorePattern, PatternError> {
    let pat = raw.trim_end_matches('/');
    if pat.is_empty() {
        return Err(PatternError::Empty);
    }
    if pat.starts_with('/') || pat.starts_with('~') {
        parse_path_glob(pat, home).map(IgnorePattern::Path)
    } else if pat.contains('/') {
        let rel = pat.strip_prefix("./").unwrap_or(pat);
        let joined = if root == "/" { format!("/{rel}") } else { format!("{root}/{rel}") };
        parse_path_glob(&joined, home).map(IgnorePattern::Path)
    } else {
        parse_text_glob(pat).map(IgnorePattern::Name)
    }
}

/// Parses a text glob, where `*` crosses every character.
pub fn parse_text_glob(raw: &str) -> Result<Glob, PatternError> {
    let alternatives = expand_braces(raw)?
        .iter()
        .map(|a| tokenize(a, raw).map(GlobAlternative::Text))
        .collect::<Result<_, _>>()?;
    Ok(Glob { pattern: raw.to_owned(), alternatives })
}

/// Parses an event type glob such as `fs.*`, `fs.file.*`, `ext.ci.*`, `*`.
pub fn parse_type_glob(raw: &str) -> Result<Glob, PatternError> {
    let ok = !raw.is_empty()
        && raw.len() <= 200
        && raw.bytes().all(|b| {
            b.is_ascii_lowercase()
                || b.is_ascii_digit()
                || matches!(b, b'.' | b'_' | b'-' | b'*' | b'{' | b'}' | b',')
        });
    if !ok {
        return Err(PatternError::BadTypeGlob(raw.to_owned()));
    }
    parse_text_glob(raw)
}

/// Whether a `where` string value is a path glob (starts with `/` or `~`).
pub fn looks_like_path(raw: &str) -> bool {
    raw.starts_with('/') || raw.starts_with('~')
}

fn expand_braces(raw: &str) -> Result<Vec<String>, PatternError> {
    let mut out = vec![String::new()];
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let next =
                    chars.next().ok_or_else(|| PatternError::TrailingEscape(raw.to_owned()))?;
                for o in &mut out {
                    o.push('\\');
                    o.push(next);
                }
            }
            '{' => {
                let mut options = vec![String::new()];
                loop {
                    match chars.next() {
                        None => return Err(PatternError::UnclosedBrace(raw.to_owned())),
                        Some('}') => break,
                        Some('{') => return Err(PatternError::NestedBrace(raw.to_owned())),
                        Some(',') => options.push(String::new()),
                        Some('\\') => {
                            let next = chars
                                .next()
                                .ok_or_else(|| PatternError::TrailingEscape(raw.to_owned()))?;
                            if let Some(last) = options.last_mut() {
                                last.push('\\');
                                last.push(next);
                            }
                        }
                        Some(ch) => {
                            if let Some(last) = options.last_mut() {
                                last.push(ch);
                            }
                        }
                    }
                }
                if out.len() * options.len() > 64 {
                    return Err(PatternError::TooManyAlternatives(raw.to_owned()));
                }
                out = out
                    .iter()
                    .flat_map(|prefix| options.iter().map(move |opt| format!("{prefix}{opt}")))
                    .collect();
            }
            '}' => return Err(PatternError::UnmatchedBrace(raw.to_owned())),
            ch => {
                for o in &mut out {
                    o.push(ch);
                }
            }
        }
    }
    Ok(out)
}

/// Splits on `sep`, keeping escapes intact for `tokenize`.
fn split_unescaped(s: &str, sep: char) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let (Some(next), Some(last)) = (chars.next(), parts.last_mut()) {
                last.push('\\');
                last.push(next);
            }
        } else if c == sep {
            parts.push(String::new());
        } else if let Some(last) = parts.last_mut() {
            last.push(c);
        }
    }
    parts
}

fn tokenize(s: &str, raw: &str) -> Result<Vec<GlobToken>, PatternError> {
    let mut out = Vec::new();
    let mut lit = String::new();
    let mut chars = s.chars();
    let flush = |lit: &mut String, out: &mut Vec<GlobToken>| {
        if !lit.is_empty() {
            out.push(GlobToken::Literal(std::mem::take(lit)));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                lit.push(chars.next().ok_or_else(|| PatternError::TrailingEscape(raw.to_owned()))?)
            }
            '*' => {
                flush(&mut lit, &mut out);
                // `**` in a text glob means the same as `*`.
                if out.last() != Some(&GlobToken::Star) {
                    out.push(GlobToken::Star);
                }
            }
            '?' => {
                flush(&mut lit, &mut out);
                out.push(GlobToken::Any);
            }
            ch => lit.push(ch),
        }
    }
    flush(&mut lit, &mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/home/me";

    #[test]
    fn expand_paths() {
        let home = Path::new(HOME);
        let base = Path::new("/repo");
        let cases: &[(&str, Option<&Path>, Result<&str, ()>)] = &[
            ("~", None, Ok("/home/me")),
            ("~/", None, Ok("/home/me")),
            ("~/University", None, Ok("/home/me/University")),
            ("~/University/", None, Ok("/home/me/University")),
            ("/abs/x", None, Ok("/abs/x")),
            ("/", None, Ok("/")),
            ("rel", None, Err(())),
            ("rel/x", Some(base), Ok("/repo/rel/x")),
            ("./rel", Some(base), Ok("/repo/rel")),
            (".", Some(base), Ok("/repo")),
            ("../escape", Some(base), Err(())),
            ("~/a/../b", None, Err(())),
            ("/a/./b", None, Err(())),
            ("/a//b", None, Err(())),
            ("~root/x", None, Err(())),
        ];
        for (raw, base, want) in cases {
            let got = expand_path(raw, home, *base).map_err(|_| ());
            assert_eq!(got.as_deref(), want.as_deref(), "{raw}");
        }
        assert!(expand_path("~/x", Path::new("relative-home"), None).is_err());
    }

    #[test]
    fn path_globs_parse_and_reject() {
        let home = Path::new(HOME);
        let g = parse_path_glob("~/University/**/*.{pdf,pptx}", home).unwrap();
        assert_eq!(g.pattern, "/home/me/University/**/*.{pdf,pptx}");
        assert_eq!(g.alternatives.len(), 2);
        assert!(g.is_path());
        for bad in
            ["", "rel/**", "/a/**b", "/a/x**", "/a/../b", "/a/{b", "/a/b}", "/a/{b,{c}}", "/a\\"]
        {
            assert!(parse_path_glob(bad, home).is_err(), "{bad:?}");
        }
        let many = "/{a,b}{c,d}{e,f}{g,h}{i,j}{k,l}{m,n}";
        assert!(matches!(parse_path_glob(many, home), Err(PatternError::TooManyAlternatives(_))));
    }

    #[test]
    fn type_globs() {
        for ok in ["fs.*", "fs.file.*", "*", "ext.ci.build-*", "git.commit.created", "{fs,git}.*"] {
            assert!(parse_type_glob(ok).is_ok(), "{ok}");
        }
        for bad in ["", "FS.*", "fs/*", "fs.{a,b", "fs file"] {
            assert!(parse_type_glob(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn escapes_are_literal() {
        let g = parse_text_glob(r"a\*b\{c\}").unwrap();
        assert_eq!(
            g.alternatives,
            vec![GlobAlternative::Text(vec![GlobToken::Literal("a*b{c}".into())])]
        );
    }
}
