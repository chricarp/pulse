//! The config schema: `pulse.toml` and trusted project files.
//!
//! Types here are **as written**: paths keep their `~`, project names are not yet prefixed.
//! [`parse_config_file`] turns TOML text into a [`ConfigFile`] and validates it on its own;
//! [`resolve`] combines the user file with trusted project files into one [`Config`] (prefixing
//! project names and resolving rule → agent references). The same types are the wire form for
//! `rules.upsert`, `agents.upsert` and `sources.upsert`.
//!
//! Unknown keys are errors everywhere. Every error says which file, item and field, and what
//! was expected; see [`ConfigError`].

mod parse;
mod values;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::time::Duration;

pub use parse::{ConfigError, ConfigErrors, parse_config_file, resolve, validate_name};
pub use values::{
    Rate, RateError, Scope, ScopeError, TypeGlob, TypeGlobs, WhereClause, WhereCondition,
    WhereField, WhereOps, WhereScalar,
};

/// Where a definition came from.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigOrigin {
    /// `$PULSE_HOME/pulse.toml`.
    User {
        #[ts(type = "string")]
        path: PathBuf,
    },
    /// A trusted project file. Its names are prefixed `<name>/`; relative paths resolve against
    /// `dir`.
    Project {
        name: String,
        #[ts(type = "string")]
        dir: PathBuf,
        #[ts(type = "string")]
        path: PathBuf,
    },
}

impl ConfigOrigin {
    pub fn path(&self) -> &std::path::Path {
        match self {
            ConfigOrigin::User { path } | ConfigOrigin::Project { path, .. } => path,
        }
    }

    /// The directory relative paths resolve against; `None` means relative paths are errors.
    pub fn base_dir(&self) -> Option<&std::path::Path> {
        match self {
            ConfigOrigin::User { .. } => None,
            ConfigOrigin::Project { dir, .. } => Some(dir),
        }
    }

    pub fn project(&self) -> Option<&str> {
        match self {
            ConfigOrigin::User { .. } => None,
            ConfigOrigin::Project { name, .. } => Some(name),
        }
    }
}

/// One config file as written.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    /// Not allowed in project files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon: Option<DaemonSettings>,
    /// Not allowed in project files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<Limits>,
    /// Not allowed in project files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<Retention>,
    #[serde(default, rename = "source", skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceConfig>,
    #[serde(default, rename = "agent", skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<AgentConfig>,
    #[serde(default, rename = "rule", skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<RuleConfig>,
}

/// A definition together with the file it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct Sourced<T> {
    pub origin: ConfigOrigin,
    pub config: T,
}

/// The effective configuration: the user file plus every trusted project file. Names are
/// unique and fully qualified (`project/name` for project definitions); every rule's
/// `run.agent` names an existing, runnable agent.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub daemon: DaemonSettings,
    pub limits: Limits,
    pub retention: Retention,
    pub sources: Vec<Sourced<SourceConfig>>,
    pub agents: Vec<Sourced<AgentConfig>>,
    pub rules: Vec<Sourced<RuleConfig>>,
}

impl Config {
    pub fn rule(&self, name: &str) -> Option<&Sourced<RuleConfig>> {
        self.rules.iter().find(|r| r.config.name == name)
    }
    pub fn agent(&self, name: &str) -> Option<&Sourced<AgentConfig>> {
        self.agents.iter().find(|a| a.config.name == name)
    }
    pub fn source(&self, name: &str) -> Option<&Sourced<SourceConfig>> {
        self.sources.iter().find(|s| s.config.name == name)
    }
}

// ---- [daemon], [limits], [retention] ----

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields, default)]
pub struct DaemonSettings {
    /// TCP port on 127.0.0.1. `0` picks a free port (written to `daemon.json`).
    pub port: u16,
    pub log_level: LogLevel,
    /// Serve the draft MCP Events extension (SEP-3415) when the binary was built with it.
    pub mcp_events_extension: bool,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        DaemonSettings { port: 7477, log_level: LogLevel::Info, mcp_events_extension: false }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields, default)]
pub struct Limits {
    /// At most this many runs in one causal chain (an event caused by a run caused by a run…).
    pub max_chain_depth: u32,
    /// How long after a run ends its working directory still attributes events to it.
    pub causation_grace: Duration,
    /// How long a `pulsededupekey` suppresses repeats.
    pub dedupe_ttl: Duration,
    /// Dedupe keys remembered at once; the oldest are forgotten first.
    pub dedupe_capacity: u32,
    /// Pending debounce keys at once. Past this, a rule's events skip debouncing (but still pass
    /// cooldown and rate limits).
    pub debounce_capacity: u32,
    pub circuit_breaker: CircuitBreaker,
    pub prompt: PromptLimits,
    /// Per-run output cap. Past it, the head and tail are kept and the middle is dropped.
    pub run_output_max_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_chain_depth: 3,
            causation_grace: Duration::from_secs(30),
            dedupe_ttl: Duration::from_mins(10),
            dedupe_capacity: 10_000,
            debounce_capacity: 10_000,
            circuit_breaker: CircuitBreaker::default(),
            prompt: PromptLimits::default(),
            run_output_max_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Snoozes a rule that fires more than `max_fires` times within `window`, for `open_for`.
/// This backstop does not depend on causation being attributed correctly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields, default)]
pub struct CircuitBreaker {
    pub max_fires: u32,
    pub window: Duration,
    pub open_for: Duration,
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        CircuitBreaker {
            max_fires: 20,
            window: Duration::from_mins(10),
            open_for: Duration::from_hours(1),
        }
    }
}

/// Size limits for rendered prompts (see the threat model).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields, default)]
pub struct PromptLimits {
    /// Inline `{{…}}` substitutions in the instruction are cut to this many characters.
    pub inline_max_chars: u32,
    /// Each string in the untrusted data block is cut to this many bytes.
    pub field_max_bytes: u32,
    /// The whole untrusted data block is at most this many bytes.
    pub data_max_bytes: u32,
}

impl Default for PromptLimits {
    fn default() -> Self {
        PromptLimits { inline_max_chars: 200, field_max_bytes: 4096, data_max_bytes: 32 * 1024 }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields, default)]
pub struct Retention {
    pub events_max_age: Duration,
    pub events_max_count: u64,
    /// Finished runs older than this are pruned with their output. Events they reference are
    /// kept until then.
    pub runs_max_age: Duration,
}

impl Default for Retention {
    fn default() -> Self {
        Retention {
            events_max_age: Duration::from_days(30),
            events_max_count: 1_000_000,
            runs_max_age: Duration::from_days(90),
        }
    }
}

// ---- [[source]] ----

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct SourceConfig {
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(flatten)]
    pub settings: SourceSettings,
}

impl SourceConfig {
    pub fn kind(&self) -> crate::event::SourceKind {
        use crate::event::SourceKind;
        match self.settings {
            SourceSettings::Fs(_) => SourceKind::Fs,
            SourceSettings::Git(_) => SourceKind::Git,
            SourceSettings::Schedule(_) => SourceKind::Schedule,
            SourceSettings::Calendar(_) => SourceKind::Calendar,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceSettings {
    Fs(FsSourceSettings),
    Git(GitSourceSettings),
    Schedule(ScheduleSourceSettings),
    Calendar(CalendarSourceSettings),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct FsSourceSettings {
    /// The directory to watch (`~` allowed; relative to the project in project files).
    pub path: String,
    /// Globs relative to `path` (or absolute) added to the built-in ignores.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore: Vec<String>,
    #[serde(default = "yes")]
    pub recursive: bool,
    /// Per-path quiet period before a change is emitted. Default 2s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet: Option<Duration>,
    /// Pending changes under one directory beyond which they fold into one `fs.burst`.
    /// Default 200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burst_threshold: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct GitSourceSettings {
    /// The repository's working tree.
    pub path: String,
}

/// Exactly one of `cron` and `every`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct ScheduleSourceSettings {
    /// Five fields: minute hour day-of-month month day-of-week.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<Duration>,
    /// IANA name for `cron`; default is the system zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

/// Exactly one of `url` and `path`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct CalendarSourceSettings {
    /// An `http(s)://` ICS feed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// A local `.ics` file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// How often to re-read the calendar. Default 15m.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<Duration>,
    /// How long before an event `calendar.event.starting` fires. Default 10m.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead: Option<Duration>,
}

// ---- [[agent]] ----

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(optional_fields)]
pub struct AgentConfig {
    pub name: String,
    #[serde(flatten)]
    pub adapter: AdapterSettings,
    /// Permission scopes. Default deny: an agent with no scopes receives nothing.
    #[serde(default)]
    pub allow: Vec<Scope>,
    /// Working directory for runs (`~` allowed). Default: the Pulse home's `runs/` directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout: Duration,
    /// Runs executing at once.
    #[serde(default = "one")]
    pub max_concurrent: u32,
    /// Runs waiting for a slot. Past this, new fires are blocked as `agent_busy`.
    #[serde(default = "default_max_queued")]
    pub max_queued: u32,
    /// Extra environment for runs. Trusted (it is config). Keys starting with `PULSE_` are
    /// reserved.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

impl AgentConfig {
    pub fn kind(&self) -> AgentKind {
        match self.adapter {
            AdapterSettings::ClaudeCode(_) => AgentKind::ClaudeCode,
            AdapterSettings::Codex(_) => AgentKind::Codex,
            AdapterSettings::Command(_) => AgentKind::Command,
            AdapterSettings::Mcp(_) => AgentKind::Mcp,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum AgentKind {
    ClaudeCode,
    Codex,
    Command,
    /// Not spawned by Pulse. Exists to give an MCP client a scoped identity; rules cannot target
    /// it.
    Mcp,
}

impl AgentKind {
    pub fn is_runnable(self) -> bool {
        !matches!(self, AgentKind::Mcp)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "adapter", rename_all = "kebab-case")]
pub enum AdapterSettings {
    ClaudeCode(ClaudeCodeSettings),
    Codex(CodexSettings),
    Command(CommandSettings),
    Mcp(McpSettings),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct ClaudeCodeSettings {
    /// The `claude` executable. Default: found on `PATH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Passed through as `--permission-mode`. Pulse never widens it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct CodexSettings {
    /// The `codex` executable. Default: found on `PATH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Passed through as `--sandbox`. Pulse never widens it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CommandSettings {
    /// argv template. Never run through a shell; each element is rendered on its own, so event
    /// content cannot add arguments. The program (first element) may not contain placeholders.
    pub command: Vec<String>,
    /// What the process gets on stdin.
    #[serde(default)]
    pub stdin: StdinMode,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum StdinMode {
    /// The rendered prompt (if the rule has one).
    #[default]
    Prompt,
    /// The event as JSON.
    Event,
    None,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct McpSettings {}

// ---- [[rule]] ----

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct RuleConfig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Event type glob(s).
    pub on: TypeGlobs,
    #[serde(default, rename = "where", skip_serializing_if = "BTreeMap::is_empty")]
    pub where_: WhereClause,
    /// Trailing debounce: wait until matching events stop for this long, then fire once with
    /// the latest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debounce: Option<Duration>,
    #[serde(default, skip_serializing_if = "DebounceBy::is_default")]
    pub debounce_by: DebounceBy,
    /// Minimum time between fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown: Option<Duration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<Rate>,
    /// Each run waits for a human to approve it.
    #[serde(default)]
    pub supervised: bool,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Hidden from active lists and never routed; history is kept. `rules.reopen` reverses it.
    #[serde(default)]
    pub archived: bool,
    pub run: RunSpec,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct RunSpec {
    pub agent: String,
    /// Required except for `command` agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

/// What one debounce timer covers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DebounceBy {
    /// One timer for the rule: a burst of matching events fires once.
    #[default]
    Rule,
    /// One timer per subject: each file (repo, title…) fires once when it settles.
    Subject,
}

impl DebounceBy {
    fn is_default(&self) -> bool {
        *self == DebounceBy::Rule
    }
}

fn yes() -> bool {
    true
}
fn one() -> u32 {
    1
}
fn default_timeout() -> Duration {
    Duration::from_mins(15)
}
fn default_max_queued() -> u32 {
    8
}
