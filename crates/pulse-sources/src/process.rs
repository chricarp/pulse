//! The process wrapper behind `pulse run -- <command>`: runs the command with inherited stdio,
//! keeps a bounded tail of its output, and describes how it exited. The CLI then reports it with
//! `events.report` (and stays silent if the daemon is down).
//!
//! Stub: returns `NotImplemented`.

use std::path::Path;

use pulse_contracts::catalog::ProcessExitedData;

use crate::SourceError;

/// Runs `command` (argv; never a shell string) in `cwd`, passing output through to the
/// terminal while keeping at most `tail_bytes` of combined stdout and stderr.
pub async fn run_wrapped(
    command: &[String],
    cwd: &Path,
    tail_bytes: usize,
) -> Result<ProcessExitedData, SourceError> {
    let _ = (command, cwd, tail_bytes);
    Err(SourceError::NotImplemented("process wrapper"))
}
