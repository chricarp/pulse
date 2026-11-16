//! Command-line syntax.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "pulse", version, about = "The local event layer for AI agents.", long_about = None)]
pub struct Cli {
    /// The Pulse home. Default: $PULSE_HOME, else ~/.pulse.
    #[arg(long, global = true, value_name = "DIR")]
    pub home: Option<PathBuf>,
    /// Print JSON instead of text, for scripts: one document, or an error object (see the CLI
    /// reference).
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the daemon (in the foreground unless --detach).
    Start(StartArgs),
    /// Stop the daemon and wait until it has exited.
    Stop,
    /// Show what the daemon is doing.
    Status,
    /// Pause everything: rules are evaluated and recorded but nothing fires.
    Pause,
    /// Resume after `pulse pause`.
    Resume,
    /// Follow events (and run changes) live.
    Tail(TailArgs),
    /// Query the event log.
    Events(EventsArgs),
    /// Rules (lists them without a subcommand): show, add, remove, enable, snooze, archive,
    /// history.
    Rules(RulesArgs),
    /// Test a rule (saved, or a draft from a file) against a past event. No side effects.
    TestRule(TestRuleArgs),
    /// Re-emit a past event as a new one and route it.
    Replay(ReplayArgs),
    /// Run a command and report how it exited as `process.exited`.
    Run(RunArgs),
    /// Runs (lists them without a subcommand): show, logs, approve, reject, cancel, rerun.
    Runs(RunsArgs),
    /// Agents (lists them without a subcommand): show, add, remove, and their MCP credentials.
    Agents(AgentsArgs),
    /// MCP over stdio, for MCP clients that launch a server: a bridge to the daemon's /mcp as
    /// one agent. Stdout carries MCP messages only.
    Mcp(McpArgs),
    /// Sources and their health (lists them without a subcommand): add, remove, enable, disable.
    Sources(SourcesArgs),
    /// Projects: trust or untrust a repository's pulse.toml (lists them without a subcommand).
    Project(ProjectArgs),
    /// Send external events (JSON lines on stdin) as `ext.<source>.<type>`.
    Ingest(IngestArgs),
    /// Open the web UI (or print its login URL).
    Open(OpenArgs),
    /// Write a starter config, or add a template to it.
    Init(InitArgs),
}

#[derive(Debug, Args)]
pub struct StartArgs {
    /// Start in the background and return once it is ready.
    #[arg(long, short)]
    pub detach: bool,
    /// Listen on this port instead of `[daemon] port`.
    #[arg(long)]
    pub port: Option<u16>,
    /// Print `ready <port>` on stdout once listening (used by --detach).
    #[arg(long, hide = true)]
    pub ready_line: bool,
}

#[derive(Debug, Args, Default)]
pub struct FilterArgs {
    /// Event type glob (repeatable), e.g. `fs.*`.
    #[arg(long = "type", short = 't', value_name = "GLOB")]
    pub types: Vec<String>,
    /// Source name or pulse:// URI (repeatable).
    #[arg(long = "source", short = 's')]
    pub sources: Vec<String>,
    /// Glob over the subject.
    #[arg(long)]
    pub subject: Option<String>,
    /// Events carrying a path under this directory.
    #[arg(long)]
    pub path: Option<PathBuf>,
    /// Case-insensitive text in the subject or data.
    #[arg(long)]
    pub text: Option<String>,
}

#[derive(Debug, Args)]
pub struct TailArgs {
    #[command(flatten)]
    pub filter: FilterArgs,
    /// Do not show run state changes.
    #[arg(long)]
    pub no_runs: bool,
}

#[derive(Debug, Args)]
pub struct EventsArgs {
    #[command(subcommand)]
    pub show: Option<EventsCmd>,
    #[command(flatten)]
    pub filter: FilterArgs,
    /// At most this many.
    #[arg(long, short = 'n', default_value_t = 50)]
    pub limit: u32,
    /// Continue after this cursor (printed at the end of a page).
    #[arg(long)]
    pub cursor: Option<String>,
    /// Oldest first instead of newest first.
    #[arg(long)]
    pub oldest_first: bool,
    /// Only events at or after this time (RFC 3339) or this long ago (e.g. 2h).
    #[arg(long)]
    pub since: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum EventsCmd {
    /// One event: its data, its cause, every rule it reached and why, the runs it caused.
    Show {
        /// The event's id, or its last characters (at least 3).
        id: String,
    },
}

#[derive(Debug, Args)]
pub struct RulesArgs {
    #[command(subcommand)]
    pub cmd: Option<RulesCmd>,
}

#[derive(Debug, Subcommand)]
pub enum RulesCmd {
    /// Active rules with their state (with --all, archived ones too).
    List {
        /// Include archived rules.
        #[arg(long)]
        all: bool,
    },
    /// A rule's definition as TOML, the file it is in, its state and its last fire.
    Show {
        /// The rule's name.
        name: String,
    },
    /// Let a disabled rule fire again (sets `enabled = true` in its file).
    Enable {
        /// The rule's name.
        name: String,
    },
    /// Stop a rule from firing, keeping it and its history (sets `enabled = false`).
    Disable {
        /// The rule's name.
        name: String,
    },
    /// Hold a rule for a while (`1h`, `30m`) or until a time (RFC 3339).
    Snooze {
        /// The rule's name.
        name: String,
        /// How long (`1h`, `30m`) or until when (`2026-11-16T09:00:00Z`).
        r#for: String,
    },
    /// End a snooze (and close a tripped circuit breaker).
    Unsnooze {
        /// The rule's name.
        name: String,
    },
    /// Hide a rule and stop routing to it; history is kept.
    Archive {
        /// The rule's name.
        name: String,
    },
    /// Undo `archive`.
    Reopen {
        /// The rule's name.
        name: String,
    },
    /// Add a rule from a TOML file (`name = …`, `on = …`, `[run]` …; what `rules show` prints).
    /// `-` reads stdin. Written to pulse.toml, comments kept. Refused if a rule of that name
    /// exists, unless --replace.
    Add {
        /// The TOML file with the rule, or `-` for stdin.
        #[arg(long, short, value_name = "FILE")]
        file: PathBuf,
        /// Add it to this trusted project's pulse.toml instead.
        #[arg(long)]
        project: Option<String>,
        /// Replace the rule of that name, whatever it is now.
        #[arg(long)]
        replace: bool,
    },
    /// Delete a rule from its config file (`archive` keeps it and its history visible).
    Remove {
        /// The rule's name.
        name: String,
    },
    /// The rule's decisions, newest first, and how each loop-protection block knew its cause.
    History {
        /// The rule's name.
        name: String,
        /// Only these outcomes (repeatable).
        #[arg(long, value_enum)]
        outcome: Vec<OutcomeArg>,
        /// At most this many.
        #[arg(long, short = 'n', default_value_t = 50)]
        limit: u32,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OutcomeArg {
    Fired,
    AwaitingApproval,
    Debounced,
    Blocked,
}

#[derive(Debug, Args)]
pub struct TestRuleArgs {
    /// The saved rule's name (omit with --file).
    pub rule: Option<String>,
    /// The past event's id, or its last characters (at least 3).
    #[arg(long, short)]
    pub event: String,
    /// A TOML file with one unsaved rule (`name = …`, `on = …`, `[run]` …).
    #[arg(long, short)]
    pub file: Option<PathBuf>,
    /// The project a draft belongs to.
    #[arg(long)]
    pub project: Option<String>,
    /// Ignore the rule's history (snooze, cooldown, rate limit).
    #[arg(long)]
    pub fresh: bool,
}

#[derive(Debug, Args)]
pub struct ReplayArgs {
    /// The event's id, or its last characters (at least 3).
    pub event: String,
    /// Only route to this rule (repeatable).
    #[arg(long)]
    pub rule: Vec<String>,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Bytes of output kept for the event.
    #[arg(long, default_value_t = 8192)]
    pub tail_bytes: usize,
    /// The command and its arguments (after `--`).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
pub struct RunsArgs {
    #[command(subcommand)]
    pub cmd: Option<RunsCmd>,
}

/// What `pulse runs` does without a subcommand: `list` with its defaults.
impl Default for RunsCmd {
    fn default() -> Self {
        RunsCmd::List { rule: None, agent: None, status: Vec::new(), limit: 50 }
    }
}

#[derive(Debug, Subcommand)]
pub enum RunsCmd {
    /// Runs, newest first.
    List {
        /// Only runs of this rule.
        #[arg(long)]
        rule: Option<String>,
        /// Only runs of this agent.
        #[arg(long)]
        agent: Option<String>,
        /// Only these states (repeatable), e.g. `running`, `pending_approval`.
        #[arg(long)]
        status: Vec<String>,
        /// At most this many.
        #[arg(long, short = 'n', default_value_t = 50)]
        limit: u32,
    },
    /// The event, timing, what was run (or will run) and where, the prompt, any error.
    Show {
        /// The run's id, or its last characters (at least 3).
        id: String,
    },
    /// A run's output (with -f, follow it until it ends).
    Logs {
        /// The run's id, or its last characters (at least 3).
        id: String,
        /// Keep printing output until the run ends.
        #[arg(long, short)]
        follow: bool,
    },
    /// Start a run that waits for approval (refused if its agent or rule changed since).
    Approve {
        /// The run's id, or its last characters (at least 3).
        id: String,
    },
    /// Refuse a run that waits for approval.
    Reject {
        /// The run's id, or its last characters (at least 3).
        id: String,
        /// Why, recorded on the run.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Stop a queued or running run (its whole process group).
    Cancel {
        /// The run's id, or its last characters (at least 3).
        id: String,
    },
    /// A new run with the same event and prompt, now, with the agent as it is configured now.
    Rerun {
        /// The run's id, or its last characters (at least 3).
        id: String,
    },
}

#[derive(Debug, Args)]
pub struct AgentsArgs {
    #[command(subcommand)]
    pub cmd: Option<AgentsCmd>,
}

#[derive(Debug, Subcommand)]
pub enum AgentsCmd {
    /// Agents, their running and queued counts, their scopes.
    List,
    /// The agent's definition as TOML.
    Show {
        /// The agent's name.
        name: String,
    },
    /// Add an agent from a TOML file (`name = …`, `adapter = …`, `allow = […]` …; what
    /// `agents show` prints). `-` reads stdin. Refused if an agent of that name exists, unless
    /// --replace.
    Add {
        /// The TOML file with the agent, or `-` for stdin.
        #[arg(long, short, value_name = "FILE")]
        file: PathBuf,
        /// Add it to this trusted project's pulse.toml instead.
        #[arg(long)]
        project: Option<String>,
        /// Replace the agent of that name, whatever it is now.
        #[arg(long)]
        replace: bool,
    },
    /// Delete an agent from its config file. Refused while rules use it.
    Remove {
        /// The agent's name.
        name: String,
    },
    /// The /mcp URL, the agent's token (valid only for MCP, only as this agent), and
    /// configuration to paste into MCP clients.
    McpCredentials {
        /// The agent's name.
        name: String,
    },
}

#[derive(Debug, Args)]
pub struct SourcesArgs {
    #[command(subcommand)]
    pub cmd: Option<SourcesCmd>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SourceKindArg {
    Fs,
    Git,
    Schedule,
    Calendar,
}

#[derive(Debug, Subcommand)]
pub enum SourcesCmd {
    /// Sources with their health and event counts.
    List,
    /// Add a source: `fs NAME DIR`, `git NAME REPO`, `schedule NAME --cron "0 9 * * 1-5"` or
    /// `--every 1h`, `calendar NAME FILE.ics|URL`. Refused if a source of that name exists,
    /// unless --replace.
    Add {
        /// What it watches.
        kind: SourceKindArg,
        /// Its name, which events carry as their source.
        name: String,
        /// The directory, repository, `.ics` file or calendar URL.
        target: Option<String>,
        /// Five-field cron (schedule).
        #[arg(long)]
        cron: Option<String>,
        /// A fixed interval like `30m` (schedule).
        #[arg(long)]
        every: Option<String>,
        /// IANA time zone for --cron.
        #[arg(long)]
        timezone: Option<String>,
        /// Extra ignore glob (fs; repeatable).
        #[arg(long)]
        ignore: Vec<String>,
        /// How long before an event `calendar.event.starting` fires (calendar).
        #[arg(long)]
        lead: Option<String>,
        /// Add it to this trusted project's pulse.toml instead.
        #[arg(long)]
        project: Option<String>,
        /// Replace the source of that name, whatever it is now.
        #[arg(long)]
        replace: bool,
    },
    /// Delete a source from its config file.
    Remove {
        /// The source's name.
        name: String,
    },
    /// Start watching again (sets `enabled` in its config file, nothing else).
    Enable {
        /// The source's name.
        name: String,
    },
    /// Stop watching, keeping the definition (`enable` reverses it).
    Disable {
        /// The source's name.
        name: String,
    },
}

#[derive(Debug, Args)]
pub struct McpArgs {
    /// The agent to act as: its `allow` list scopes everything the client sees.
    #[arg(long)]
    pub agent: String,
}

#[derive(Debug, Args)]
pub struct ProjectArgs {
    #[command(subcommand)]
    pub cmd: Option<ProjectCmd>,
}

#[derive(Debug, Subcommand)]
pub enum ProjectCmd {
    /// Trust DIR/pulse.toml. Without --hash, shows what it would load and its content hash,
    /// and trusts nothing; with --hash, trusts the file only if it still has that content.
    /// Any later change makes it inactive again.
    Trust {
        /// The repository: the directory with the pulse.toml.
        dir: PathBuf,
        /// Prefix for its definitions (default: the directory name).
        #[arg(long)]
        name: Option<String>,
        /// The content hash a review printed: trust exactly that content.
        #[arg(long)]
        hash: Option<String>,
    },
    /// Make a trusted project inactive again; its definitions stop loading.
    Untrust {
        /// The repository: the directory with the pulse.toml.
        dir: PathBuf,
    },
    /// Projects and their trust state.
    List,
}

#[derive(Debug, Args)]
pub struct IngestArgs {
    /// The external source name; types become `ext.<source>.<type>`.
    #[arg(long, short)]
    pub source: String,
}

#[derive(Debug, Args)]
pub struct OpenArgs {
    /// Print the URL instead of opening a browser.
    #[arg(long)]
    pub print: bool,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Add a template (repeatable): see --list.
    #[arg(long, short)]
    pub template: Vec<String>,
    /// List the templates.
    #[arg(long)]
    pub list: bool,
}
