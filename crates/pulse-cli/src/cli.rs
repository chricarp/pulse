//! Command-line syntax.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "pulse", version, about = "The local event layer for AI agents.", long_about = None)]
pub struct Cli {
    /// The Pulse home. Default: $PULSE_HOME, else ~/.pulse.
    #[arg(long, global = true, value_name = "DIR")]
    pub home: Option<PathBuf>,
    /// Print JSON instead of text.
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
    /// Rules: list, inspect, add, remove, enable, snooze, archive, history.
    #[command(subcommand)]
    Rules(RulesCmd),
    /// Test a rule (saved, or a draft from a file) against a past event. No side effects.
    TestRule(TestRuleArgs),
    /// Re-emit a past event as a new one and route it.
    Replay(ReplayArgs),
    /// Run a command and report how it exited as `process.exited`.
    Run(RunArgs),
    /// Runs: list, show, logs, approve, reject, cancel, rerun.
    #[command(subcommand)]
    Runs(RunsCmd),
    /// Agents: list, show, add, remove, and their MCP credentials.
    Agents(AgentsArgs),
    /// Sources and their health: list, add, remove.
    Sources(SourcesArgs),
    /// Trust or untrust a repository's pulse.toml.
    #[command(subcommand)]
    Project(ProjectCmd),
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
    /// At most this many (default 50).
    #[arg(long, short = 'n', default_value_t = 50)]
    pub limit: u32,
    /// Continue after this cursor (printed at the end of a page).
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long)]
    pub oldest_first: bool,
    /// Only events at or after this time (RFC 3339) or this long ago (e.g. 2h).
    #[arg(long)]
    pub since: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum EventsCmd {
    /// One event with its deliveries and runs.
    Show { id: String },
}

#[derive(Debug, Subcommand)]
pub enum RulesCmd {
    /// Active rules (with --all, archived ones too).
    List {
        #[arg(long)]
        all: bool,
    },
    /// A rule's definition, origin and state.
    Show {
        name: String,
    },
    Enable {
        name: String,
    },
    Disable {
        name: String,
    },
    /// Hold a rule for a while (`1h`, `30m`) or until a time (RFC 3339).
    Snooze {
        name: String,
        r#for: String,
    },
    /// End a snooze (and close a tripped circuit breaker).
    Unsnooze {
        name: String,
    },
    /// Hide a rule and stop routing to it; history is kept.
    Archive {
        name: String,
    },
    /// Undo `archive`.
    Reopen {
        name: String,
    },
    /// Add a rule from a TOML file (`name = …`, `on = …`, `[run]` …; what `rules show` prints),
    /// or replace the rule of that name. `-` reads stdin. Written to pulse.toml, comments kept.
    Add {
        #[arg(long, short, value_name = "FILE")]
        file: PathBuf,
        /// Add it to this trusted project's pulse.toml instead.
        #[arg(long)]
        project: Option<String>,
    },
    /// Delete a rule from its config file (`archive` keeps it and its history visible).
    Remove {
        name: String,
    },
    /// The rule's decisions, newest first.
    History {
        name: String,
        /// Only these outcomes (repeatable).
        #[arg(long, value_enum)]
        outcome: Vec<OutcomeArg>,
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
    /// The past event's id.
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

#[derive(Debug, Subcommand)]
pub enum RunsCmd {
    /// Runs, newest first.
    List {
        #[arg(long)]
        rule: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        /// Only these states (repeatable), e.g. `running`, `pending_approval`.
        #[arg(long)]
        status: Vec<String>,
        #[arg(long, short = 'n', default_value_t = 50)]
        limit: u32,
    },
    Show {
        id: String,
    },
    /// A run's output (with -f, follow it until it ends).
    Logs {
        id: String,
        #[arg(long, short)]
        follow: bool,
    },
    Approve {
        id: String,
    },
    Reject {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    Cancel {
        id: String,
    },
    /// A new run with the same event, prompt and configuration snapshot.
    Rerun {
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
    List,
    Show {
        name: String,
    },
    /// Add an agent from a TOML file (`name = …`, `adapter = …`, `allow = […]` …; what
    /// `agents show` prints), or replace the agent of that name. `-` reads stdin.
    Add {
        #[arg(long, short, value_name = "FILE")]
        file: PathBuf,
        #[arg(long)]
        project: Option<String>,
    },
    /// Delete an agent from its config file. Refused while rules use it.
    Remove {
        name: String,
    },
    /// The /mcp URL and the agent's token (valid only for MCP, only as this agent).
    Mcp {
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
    List,
    /// Add a source (or replace the one of that name):
    /// `fs NAME DIR`, `git NAME REPO`, `schedule NAME --cron "0 9 * * 1-5"` or `--every 1h`,
    /// `calendar NAME FILE.ics|URL`.
    Add {
        kind: SourceKindArg,
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
        #[arg(long)]
        project: Option<String>,
    },
    /// Delete a source from its config file.
    Remove {
        name: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProjectCmd {
    /// Trust DIR/pulse.toml as it is now. Any later change makes it inactive again.
    Trust {
        dir: PathBuf,
        /// Prefix for its definitions (default: the directory name).
        #[arg(long)]
        name: Option<String>,
    },
    Untrust {
        dir: PathBuf,
    },
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
