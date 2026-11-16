//! Where output goes. Under `--json`, stdout carries only what serde wrote; otherwise it carries
//! text for people, with terminal control escaped ([`crate::format::terminal_safe`]). A command
//! gets exactly one of the two printers from [`crate::Ctx::answer`] or [`crate::Ctx::out`], so
//! it cannot write text under `--json`, and JSON can only be written from a value, never from a
//! string through the text printer.
//!
//! Stderr is for people in both modes ([`note`]); nothing a program needs is written there.

use std::io::Write;

use serde::Serialize;

use crate::format::terminal_safe;

/// The printer for this invocation.
#[derive(Debug)]
pub enum Out {
    Text(Text),
    Json(Json),
}

/// Text on stdout, for people. Only [`Out`] makes one.
#[derive(Debug)]
pub struct Text(());

/// JSON on stdout, for programs. Only [`Out`] makes one.
#[derive(Debug)]
pub struct Json(());

impl Out {
    pub(crate) fn new(json: bool) -> Out {
        if json { Out::Json(Json(())) } else { Out::Text(Text(())) }
    }
}

impl Text {
    /// One line. Ignores a closed pipe (`pulse events | head`). Text carries event content
    /// (file names, commit messages, agent output), so terminal control is escaped.
    pub fn line(&self, s: &str) {
        let _ = writeln!(std::io::stdout().lock(), "{}", terminal_safe(s));
    }
}

impl Json {
    /// The whole answer of a command that answers once, pretty-printed.
    pub fn value<T: Serialize>(&self, value: &T) {
        write_json(serde_json::to_string_pretty(value));
    }

    /// One item of a stream (`tail`, `runs logs`): one compact object per line.
    pub fn line<T: Serialize>(&self, value: &T) {
        write_json(serde_json::to_string(value));
    }
}

fn write_json(s: serde_json::Result<String>) {
    match s {
        Ok(s) => drop(writeln!(std::io::stdout().lock(), "{s}")),
        // Only a map with non-string keys fails to serialize; the contracts have none.
        Err(e) => note(&format!("error: cannot write JSON: {e}")),
    }
}

/// A line on stderr for people (progress, warnings), in both modes. Escaped like text.
pub fn note(s: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{}", terminal_safe(s));
}

/// The line protocol between `pulse start --detach` and the daemon it starts
/// (`note …`, `ready <port>`): not output for people or for scripts.
pub fn protocol_line(s: &str) {
    let _ = writeln!(std::io::stdout().lock(), "{}", terminal_safe(s));
}
