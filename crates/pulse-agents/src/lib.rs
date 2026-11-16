//! Agent adapters and the process supervisor.
//!
//! ## Contract
//!
//! An adapter is two pure pieces ([`AgentAdapter`]): it builds an [`Invocation`] (argv, env,
//! cwd, stdin) from the agent's config and a [`RunRequest`], and it provides an
//! [`OutputParser`] that turns the process's native output lines into [`RunOutput`]s, keeping
//! provider detail in `RunOutput::data`. Adapters do no I/O.
//!
//! One shared [`supervise`] does the I/O for every adapter: spawn in a new process group,
//! stream lines through the parser, enforce the timeout, and terminate the group (SIGTERM, a
//! grace period, SIGKILL) on cancel or timeout. It only signals the group it created.
//!
//! ### Run environment
//!
//! A run's environment is the daemon's environment minus every `PULSE_*` variable, then the
//! agent's configured `env`, then [`run_env`] (which wins):
//!
//! | Variable | Value |
//! |---|---|
//! | `PULSE_RUN_ID` | the run id; `pulse run` and `pulse ingest` read it for exact causation |
//! | `PULSE_RULE`, `PULSE_AGENT` | rule and agent names (`PULSE_RULE` absent for reruns of deleted rules) |
//! | `PULSE_CHAIN_DEPTH` | the run's chain depth |
//! | `PULSE_HOME` | the Pulse home, so `pulse` subcommands reach this daemon |
//! | `PULSE_EVENT_FILE` | path of the triggering event as JSON, written by the daemon |
//! | `PULSE_EVENT_ID`, `_TYPE`, `_SOURCE`, `_SUBJECT`, `_TIME`, `_DATA` | from `pulse_core::event_env` |
//!
//! Adapters never pass event content through a shell. Agents' own permission modes and
//! sandboxes are passed through from config and never widened.

pub mod command;
pub mod supervisor;

use std::path::PathBuf;

use pulse_contracts::config::{AgentConfig, AgentKind, PromptLimits};
use pulse_contracts::run::InvocationSummary;
use pulse_contracts::{Event, RenderedPrompt, RunId, RunOutput};

pub use supervisor::{RunEnd, SuperviseError, SuperviseOptions, SupervisorEvent, supervise};
pub use tokio_util::sync::CancellationToken;

/// Everything an adapter needs to build an invocation. Paths are resolved by the daemon.
#[derive(Clone, Debug)]
pub struct RunRequest {
    pub run_id: RunId,
    pub rule: Option<String>,
    pub agent: String,
    pub event: Event,
    /// `None` for `command` agents whose rule has no prompt.
    pub prompt: Option<RenderedPrompt>,
    pub chain_depth: u32,
    /// The working directory: the agent's `cwd` expanded, or a per-run directory.
    pub cwd: PathBuf,
    /// Where the daemon wrote the event JSON.
    pub event_file: PathBuf,
    pub pulse_home: PathBuf,
    pub limits: PromptLimits,
}

/// A process to start. Never a shell string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Added to the daemon's environment, in order (later entries win).
    pub env: Vec<(String, String)>,
    /// Written to stdin, which is then closed. `None`: stdin is null.
    pub stdin: Option<String>,
}

impl Invocation {
    pub fn summary(&self) -> InvocationSummary {
        InvocationSummary {
            program: self.program.clone(),
            args: self.args.clone(),
            cwd: self.cwd.display().to_string(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("agent {agent:?} is configured for {configured:?}, not this adapter")]
    WrongAdapter { agent: String, configured: AgentKind },
    #[error("{0}")]
    Invalid(String),
    #[error("the {0} adapter is not implemented yet")]
    NotImplemented(&'static str),
}

/// Which stream a line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    pub const fn as_str(self) -> &'static str {
        match self {
            Stream::Stdout => "stdout",
            Stream::Stderr => "stderr",
        }
    }
}

/// Turns native output into normalized output. One parser per run; it may keep state across
/// lines (for example, a partially streamed message).
pub trait OutputParser: Send {
    /// One complete line, without its terminator.
    fn line(&mut self, stream: Stream, line: &str, out: &mut Vec<RunOutput>);
    /// The process ended; flush anything pending.
    fn finish(&mut self, out: &mut Vec<RunOutput>);
}

/// How Pulse invokes one kind of agent. Pure.
pub trait AgentAdapter: Send + Sync {
    fn kind(&self) -> AgentKind;
    fn invocation(
        &self,
        agent: &AgentConfig,
        request: &RunRequest,
    ) -> Result<Invocation, AdapterError>;
    fn parser(&self) -> Box<dyn OutputParser>;
}

/// The adapter for a kind. `None` for `mcp` agents, which Pulse does not run.
pub fn adapter_for(kind: AgentKind) -> Option<&'static dyn AgentAdapter> {
    match kind {
        AgentKind::Command => Some(&command::CommandAdapter),
        AgentKind::ClaudeCode => Some(&Unimplemented(AgentKind::ClaudeCode)),
        AgentKind::Codex => Some(&Unimplemented(AgentKind::Codex)),
        AgentKind::Mcp => None,
    }
}

/// The variables every run gets; see the crate docs.
pub fn run_env(request: &RunRequest) -> Vec<(String, String)> {
    let mut env = vec![
        ("PULSE_RUN_ID".to_owned(), request.run_id.to_string()),
        ("PULSE_AGENT".to_owned(), request.agent.clone()),
        ("PULSE_CHAIN_DEPTH".to_owned(), request.chain_depth.to_string()),
        ("PULSE_HOME".to_owned(), request.pulse_home.display().to_string()),
        ("PULSE_EVENT_FILE".to_owned(), request.event_file.display().to_string()),
    ];
    if let Some(rule) = &request.rule {
        env.push(("PULSE_RULE".to_owned(), rule.clone()));
    }
    env.extend(pulse_core::event_env(&request.event, &request.limits));
    env
}

/// Placeholder for the claude-code and codex adapters.
struct Unimplemented(AgentKind);

impl AgentAdapter for Unimplemented {
    fn kind(&self) -> AgentKind {
        self.0
    }
    fn invocation(&self, _: &AgentConfig, _: &RunRequest) -> Result<Invocation, AdapterError> {
        Err(AdapterError::NotImplemented(match self.0 {
            AgentKind::ClaudeCode => "claude-code",
            AgentKind::Codex => "codex",
            AgentKind::Command => "command",
            AgentKind::Mcp => "mcp",
        }))
    }
    fn parser(&self) -> Box<dyn OutputParser> {
        Box::new(command::LineParser)
    }
}
