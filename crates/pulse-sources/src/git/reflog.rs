//! Reflog lines: `<old> <new> <name> <<email>> <unix time> <tz>\t<message>`.
//!
//! The message says what moved the ref. The prefixes below were checked against git 2.43
//! (`commit:`, `commit (initial):`, `commit (amend):`, `commit (merge):`, `cherry-pick:`,
//! `revert:`, `am:`, `merge <x>: Merge made by …` / `Fast-forward`, `pull: …`,
//! `checkout: moving from <a> to <b>`, `rebase (start|pick|continue|finish|abort): …`,
//! `reset: moving to …`; on remote-tracking refs `update by push` versus `fetch: …`).

/// One parsed reflog line. Borrowed from the file's bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Line<'a> {
    pub old: &'a str,
    pub new: &'a str,
    /// `Name <email>`.
    pub ident: &'a str,
    pub time: i64,
    pub message: &'a str,
}

pub(crate) fn parse(line: &str) -> Option<Line<'_>> {
    let (head, message) = line.split_once('\t').unwrap_or((line, ""));
    let (old, rest) = head.split_once(' ')?;
    let (new, rest) = rest.split_once(' ')?;
    if !is_hex_id(old) || !is_hex_id(new) {
        return None;
    }
    // The identity may contain spaces; the time and zone are the last two fields.
    let (rest, _tz) = rest.rsplit_once(' ')?;
    let (ident, time) = rest.rsplit_once(' ')?;
    Some(Line { old, new, ident, time: time.parse().ok()?, message })
}

fn is_hex_id(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(crate) fn is_zero(id: &str) -> bool {
    id.bytes().all(|b| b == b'0')
}

/// What a line in `logs/HEAD` means for us.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HeadMove<'a> {
    /// A new commit was made here (including amend, merge commits, cherry-pick, revert, am).
    Commit {
        subject: &'a str,
    },
    /// `git checkout` / `git switch` between two different things.
    Checkout {
        from: &'a str,
        to: &'a str,
    },
    /// A rebase started (HEAD detaches) or ended on a branch.
    RebaseStart,
    RebaseEnd {
        branch: &'a str,
    },
    /// Anything else (reset, fast-forward merge or pull, rebase steps, clone, ...).
    Other,
}

pub(crate) fn classify_head(message: &str) -> HeadMove<'_> {
    let subject = |prefix_end: usize| message[prefix_end..].trim_start();
    if let Some(rest) = message.strip_prefix("checkout: moving from ") {
        return match rest.split_once(" to ") {
            Some((from, to)) if from != to => HeadMove::Checkout { from, to },
            _ => HeadMove::Other,
        };
    }
    if message.starts_with("rebase") {
        if let Some((_, rest)) = message.split_once("): returning to refs/heads/") {
            return HeadMove::RebaseEnd { branch: rest.trim() };
        }
        if message.contains("(start)") {
            return HeadMove::RebaseStart;
        }
        return HeadMove::Other;
    }
    for prefix in ["commit:", "cherry-pick:", "revert:", "am:"] {
        if message.starts_with(prefix) {
            return HeadMove::Commit { subject: subject(prefix.len()) };
        }
    }
    if let Some(rest) = message.strip_prefix("commit (") {
        if let Some(i) = rest.find("):") {
            return HeadMove::Commit { subject: rest[i + 2..].trim_start() };
        }
    }
    // A merge or pull that made a merge commit (not a fast-forward).
    if message.starts_with("merge ") || message.starts_with("pull") {
        if let Some((_, rest)) = message.split_once(": ") {
            if rest.starts_with("Merge made by") {
                return HeadMove::Commit { subject: rest };
            }
        }
    }
    HeadMove::Other
}

/// Whether a line in `logs/refs/remotes/<remote>/<branch>` records a push from here.
pub(crate) fn is_push(message: &str) -> bool {
    message == "update by push"
}

#[cfg(test)]
mod tests {
    use super::*;

    const Z: &str = "0000000000000000000000000000000000000000";
    const A: &str = "70e6a4b9c74a07a885b1b0abe6c8f8e610a2395a";

    #[test]
    fn parses_real_lines() {
        let line = format!(
            "{Z} {A} Ada Lovelace <ada@example.com> 1794794481 +0100\tcommit (initial): first"
        );
        let l = parse(&line).unwrap();
        assert_eq!(
            (l.old, l.new, l.ident, l.time),
            (Z, A, "Ada Lovelace <ada@example.com>", 1_794_794_481)
        );
        assert_eq!(l.message, "commit (initial): first");
        assert!(parse("garbage").is_none());
        assert!(parse(&format!("{Z} nothex Ada <a> 1 +0000\tx")).is_none());
        // No message at all (some tools write none).
        assert_eq!(parse(&format!("{Z} {A} Ada <a> 1 +0000")).unwrap().message, "");
    }

    #[test]
    fn classifies_head_messages_from_git() {
        let cases = [
            ("commit (initial): first", HeadMove::Commit { subject: "first" }),
            ("commit: second line", HeadMove::Commit { subject: "second line" }),
            ("commit (amend): second amended", HeadMove::Commit { subject: "second amended" }),
            (
                "commit (merge): Merge branch 'feat'",
                HeadMove::Commit { subject: "Merge branch 'feat'" },
            ),
            ("cherry-pick: feat y", HeadMove::Commit { subject: "feat y" }),
            ("revert: Revert \"main x\"", HeadMove::Commit { subject: "Revert \"main x\"" }),
            ("am: Fix the thing", HeadMove::Commit { subject: "Fix the thing" }),
            (
                "merge feat: Merge made by the 'ort' strategy.",
                HeadMove::Commit { subject: "Merge made by the 'ort' strategy." },
            ),
            (
                "pull: Merge made by the 'ort' strategy.",
                HeadMove::Commit { subject: "Merge made by the 'ort' strategy." },
            ),
            ("merge feat: Fast-forward", HeadMove::Other),
            ("pull: Fast-forward", HeadMove::Other),
            ("checkout: moving from main to feat", HeadMove::Checkout { from: "main", to: "feat" }),
            (
                "checkout: moving from feat to HEAD~1",
                HeadMove::Checkout { from: "feat", to: "HEAD~1" },
            ),
            ("checkout: moving from main to main", HeadMove::Other),
            ("rebase (start): checkout main", HeadMove::RebaseStart),
            ("rebase -i (start): checkout main", HeadMove::RebaseStart),
            ("rebase (pick): feat y", HeadMove::Other),
            (
                "rebase (finish): returning to refs/heads/feat",
                HeadMove::RebaseEnd { branch: "feat" },
            ),
            (
                "rebase (abort): returning to refs/heads/feat",
                HeadMove::RebaseEnd { branch: "feat" },
            ),
            ("reset: moving to HEAD~1", HeadMove::Other),
            ("clone: from https://example.com/r.git", HeadMove::Other),
        ];
        for (msg, want) in cases {
            assert_eq!(classify_head(msg), want, "{msg}");
        }
        assert!(is_push("update by push"));
        assert!(!is_push("fetch: fast-forward"));
        assert!(!is_push("pull: fast-forward"));
    }
}
