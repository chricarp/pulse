//! The process supervisor: the only place agents' processes are spawned and signalled.
//!
//! Stub: [`supervise`] returns `NotImplemented`. The signature is the contract.
//!
//! Requirements for the implementation:
//! - spawn `Invocation` with `process_group(0)` (a new group), stdin from `Invocation::stdin`,
//!   stdout and stderr piped and split into lines (cap line length at `max_line_bytes`);
//! - send `SupervisorEvent::Started` with the pid before any output;
//! - feed lines to the parser and forward its output in order; the channel is bounded, and a
//!   slow receiver slows reading (the pipe fills and the process blocks), never buffering;
//! - on `cancel` or timeout: SIGTERM to the group, wait `grace`, then SIGKILL to the group;
//!   only ever the group this call created (the workspace forbids `unsafe`; use a safe wrapper
//!   such as `rustix` or `nix`);
//! - return how the run ended once the process has been reaped and output drained.

use std::time::Duration;

use pulse_contracts::RunOutput;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{Invocation, OutputParser};

#[derive(Clone, Copy, Debug)]
pub struct SuperviseOptions {
    pub timeout: Duration,
    /// Between SIGTERM and SIGKILL.
    pub grace: Duration,
    pub max_line_bytes: usize,
}

impl Default for SuperviseOptions {
    fn default() -> Self {
        SuperviseOptions {
            timeout: Duration::from_secs(15 * 60),
            grace: Duration::from_secs(5),
            max_line_bytes: 64 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SupervisorEvent {
    Started { pid: u32 },
    Output(RunOutput),
}

/// How a supervised process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunEnd {
    Exited { code: Option<i32>, signal: Option<i32> },
    TimedOut,
    Cancelled,
}

#[derive(Debug, thiserror::Error)]
pub enum SuperviseError {
    /// The process could not be started; the run becomes `failed` without having run.
    #[error("could not start {program:?}: {source}")]
    Spawn { program: String, source: std::io::Error },
    #[error("supervising the process failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("the supervisor is not implemented yet")]
    NotImplemented,
}

pub async fn supervise(
    invocation: Invocation,
    parser: Box<dyn OutputParser>,
    events: mpsc::Sender<SupervisorEvent>,
    options: SuperviseOptions,
    cancel: CancellationToken,
) -> Result<RunEnd, SuperviseError> {
    let _ = (invocation, parser, events, options, cancel);
    Err(SuperviseError::NotImplemented)
}
