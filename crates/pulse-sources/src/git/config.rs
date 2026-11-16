//! Remote names from a repository's `config` file, to tell which remote a remote-tracking ref
//! belongs to. A remote's name may contain `/` (`team/fork`), so `refs/remotes/team/fork/main`
//! cannot be split at its first slash.
//!
//! The file is text from a cloned repository: someone else may have written it. It is read
//! here, never by running `git`; `include` and `includeIf` are not followed (they could name any
//! file); its size is capped, the parse is one pass over it, and the number and length of the
//! names kept are bounded. Only section headers matter: `[remote "<name>"]` (section names are
//! case-insensitive, the quoted name is not, `\"` and `\\` are escapes, and a backslash before
//! any other character is dropped, as git does) and the deprecated `[remote.<name>]`, whose name
//! git lowercases. A value continued onto the next line with a trailing backslash is followed,
//! so text in a value never reads as a header.

use std::io::Read;
use std::path::Path;

/// Larger config files are not read: no real one comes near.
pub(crate) const MAX_CONFIG: u64 = 1 << 20;
/// Remotes kept; the rest are ignored.
const MAX_REMOTES: usize = 1000;
/// Longer names are ignored (and could not be ref path components anyway).
const MAX_REMOTE_NAME: usize = 1024;

/// The remote names configured in the config file at `path`. `Err` says why none could be
/// read (a missing file is no remotes, not an error).
pub(crate) fn read_remotes(path: &Path) -> Result<Vec<String>, String> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() as u64 > MAX_CONFIG {
        return Err(format!("{} is larger than {MAX_CONFIG} bytes; not read", path.display()));
    }
    Ok(remotes(&String::from_utf8_lossy(&bytes)))
}

/// The remote names in config text, in order, without duplicates.
pub(crate) fn remotes(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut continued = false;
    for line in text.lines() {
        if continued {
            continued = ends_continued(line);
            continue;
        }
        let rest = line.trim_start();
        let rest = match rest.strip_prefix('[') {
            Some(header) => match header_remote(header) {
                Some((name, after)) => {
                    if out.len() < MAX_REMOTES
                        && !name.is_empty()
                        && name.len() <= MAX_REMOTE_NAME
                        && !out.contains(&name)
                    {
                        out.push(name);
                    }
                    after
                }
                // Not a remote section, or not a valid header (git would refuse the file;
                // nothing after it on this line is a header either).
                None => match header.find(']') {
                    Some(end) => &header[end + 1..],
                    None => "",
                },
            },
            None => rest,
        };
        // A `key = value` (on its own line, or after a header) may continue on the next line.
        continued = ends_continued(rest);
    }
    out
}

/// For the text after `[`: the remote name if this is a remote section header, and the rest
/// of the line after the closing `]`.
fn header_remote(header: &str) -> Option<(String, &str)> {
    let section_end =
        header.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.'))?;
    let section = &header[..section_end];
    let rest = &header[section_end..];
    if let Some(after) = rest.strip_prefix(']') {
        // `[remote.name]`: the old syntax, lowercased by git.
        let (sec, name) = section.split_once('.')?;
        return sec.eq_ignore_ascii_case("remote").then(|| (name.to_ascii_lowercase(), after));
    }
    if !section.eq_ignore_ascii_case("remote") {
        return None;
    }
    // Git requires whitespace between the section and the quoted name.
    let quoted =
        rest.strip_prefix([' ', '\t'])?.trim_start_matches([' ', '\t']).strip_prefix('"')?;
    let mut name = String::new();
    let mut chars = quoted.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => name.push(chars.next()?.1),
            '"' => {
                let after = quoted[i + 1..].strip_prefix(']')?;
                return Some((name, after));
            }
            c => name.push(c),
        }
        if name.len() > MAX_REMOTE_NAME {
            return None;
        }
    }
    None
}

/// Whether a line of `key = value` text ends in a backslash that continues it onto the next
/// line: outside quotes, not escaped, and before any comment (`#` or `;` outside quotes).
fn ends_continued(line: &str) -> bool {
    let mut quoted = false;
    let mut chars = line.chars();
    let mut last_escape = false;
    while let Some(c) = chars.next() {
        last_escape = false;
        match c {
            '\\' => {
                if chars.next().is_none() {
                    last_escape = true;
                }
            }
            '"' => quoted = !quoted,
            '#' | ';' if !quoted => return false,
            _ => {}
        }
    }
    last_escape
}

/// The configured remote that `rel` (a path below `refs/remotes/`) belongs to: the longest
/// name followed by `/` and a non-empty branch. `None` when no configured remote prefixes it.
pub(crate) fn split_remote<'a>(remotes: &[String], rel: &'a str) -> Option<(&'a str, &'a str)> {
    remotes
        .iter()
        .filter_map(|r| {
            let branch = rel.strip_prefix(r.as_str())?.strip_prefix('/')?;
            (!branch.is_empty()).then(|| (&rel[..r.len()], branch))
        })
        .max_by_key(|(remote, _)| remote.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_names_from_hostile_config_text() {
        // (config text, the remotes it configures)
        let cases: &[(&str, &[&str])] = &[
            ("[remote \"origin\"]\n\turl = x\n", &["origin"]),
            ("[remote \"team/fork\"]\n[remote \"team\"]\n", &["team/fork", "team"]),
            // Section names are case-insensitive, quoted names are not; whitespace is allowed.
            ("  [Remote\t \"Up\"]\n", &["Up"]),
            // Escapes, and a header followed by a key on the same line.
            ("[remote \"a\\\"b\\\\c\\d\"] url = x\n", &["a\"b\\cd"]),
            // The old syntax, lowercased as git does.
            ("[remote.Old]\n", &["old"]),
            // Not remotes: other sections, subsections of them, malformed headers.
            ("[core]\n[branch \"remote\"]\n[remotes \"x\"]\n[remote]\n[remote \"x\"\n", &[]),
            ("[remote \"unterminated]\n[remote x]\n[remote\"x\"]\n", &[]),
            // Text in a value is not a header, however it is continued or quoted.
            ("[core]\n\tx = a \\\n[remote \"evil\"]\n[remote \"ok\"]\n", &["ok"]),
            ("[core]\n\tx = \"a \\\\\" \\\n[remote \"evil\"]\n", &[]),
            ("[core]\n\tx = a # comment \\\n[remote \"ok\"]\n", &["ok"]),
            ("[core]\n\tx = \"a # b\" \\\n[remote \"evil\"]\n", &[]),
            ("[core]\n\tx = a \\\\\n[remote \"ok\"]\n", &["ok"]),
            ("[remote \"a\"] x = \\\n[remote \"evil\"]\n", &["a"]),
            // Includes are not followed; duplicates count once; an empty name is no remote.
            (
                "[include]\n\tpath = /etc/passwd\n[remote \"a\"]\n[remote \"a\"]\n[remote \"\"]\n",
                &["a"],
            ),
        ];
        for (text, want) in cases {
            assert_eq!(remotes(text), *want, "{text:?}");
        }
        // Bounded: a huge name is ignored, and so are remotes past the limit.
        let long = format!("[remote \"{}\"]\n[remote \"ok\"]\n", "x".repeat(MAX_REMOTE_NAME + 1));
        assert_eq!(remotes(&long), ["ok"]);
        let many: String = (0..MAX_REMOTES + 5).map(|i| format!("[remote \"r{i}\"]\n")).collect();
        assert_eq!(remotes(&many).len(), MAX_REMOTES);
    }

    #[test]
    fn the_longest_configured_remote_wins() {
        let remotes = ["team".to_owned(), "team/fork".to_owned(), "origin".to_owned()];
        assert_eq!(split_remote(&remotes, "team/fork/main"), Some(("team/fork", "main")));
        assert_eq!(split_remote(&remotes, "team/feature/x"), Some(("team", "feature/x")));
        assert_eq!(split_remote(&remotes, "origin/main"), Some(("origin", "main")));
        assert_eq!(split_remote(&remotes, "originals/main"), None);
        assert_eq!(split_remote(&remotes, "ghost/main"), None);
        assert_eq!(split_remote(&remotes, "team/"), None);
    }
}
