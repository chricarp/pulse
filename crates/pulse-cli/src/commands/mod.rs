//! One function per command. Each decodes its arguments, calls the daemon, and prints.

pub mod daemon;
mod events;
mod mcp;
mod misc;
mod rules;
mod runs;

use std::process::ExitCode;
use std::str::FromStr;
use std::time::SystemTime;

use pulse_contracts::rpc::{ResolveIdParams, method};
use pulse_contracts::{Duration, EventId, RunId, Timestamp};
use pulse_daemon::client::Client;

use crate::cli::Command;
use crate::{Ctx, Failure};

pub type Res = Result<ExitCode, Failure>;

pub async fn run(ctx: &Ctx, command: Command) -> Res {
    match command {
        Command::Start(_) => Ok(ExitCode::SUCCESS),
        Command::Stop => daemon::stop(ctx).await,
        Command::Status => daemon::status(ctx).await,
        Command::Pause => daemon::pause(ctx, true).await,
        Command::Resume => daemon::pause(ctx, false).await,
        Command::Open(args) => daemon::open(ctx, args).await,
        Command::Init(args) => daemon::init(ctx, args),
        Command::Tail(args) => events::tail(ctx, args).await,
        Command::Events(args) => events::events(ctx, args).await,
        Command::Replay(args) => events::replay(ctx, args).await,
        Command::Ingest(args) => events::ingest(ctx, args).await,
        Command::Run(args) => events::run(ctx, args).await,
        Command::TestRule(args) => rules::test_rule(ctx, args).await,
        Command::Rules(cmd) => rules::rules(ctx, cmd).await,
        Command::Runs(cmd) => runs::runs(ctx, cmd).await,
        Command::Agents(args) => misc::agents(ctx, args).await,
        Command::Mcp(args) => mcp::bridge(ctx, args).await,
        Command::Sources(args) => misc::sources(ctx, args).await,
        Command::Project(cmd) => misc::project(ctx, cmd).await,
    }
}

/// Reads one definition (a bare TOML table) from `file`, or stdin for `-`.
pub fn read_definition<T: serde::de::DeserializeOwned>(
    file: &std::path::Path,
) -> Result<T, Failure> {
    let text = if file.as_os_str() == "-" {
        std::io::read_to_string(std::io::stdin())?
    } else {
        std::fs::read_to_string(file)
            .map_err(|e| Failure::invalid(format!("cannot read {}: {e}", file.display())))?
    };
    toml::from_str(&text).map_err(|e| Failure::invalid(format!("{}: {e}", file.display())))
}

/// What `add` expects: nothing of that name, unless the user asked to replace it.
pub fn add_expected(replace: bool) -> pulse_contracts::rpc::Expected {
    use pulse_contracts::rpc::Expected;
    if replace { Expected::Any } else { Expected::Absent }
}

/// `add` refused because the name exists: say how to replace it.
pub fn exists_hint(e: pulse_daemon::client::ClientError, what: &str) -> Failure {
    match e {
        pulse_daemon::client::ClientError::Rpc(err)
            if err.kind() == Some(pulse_contracts::rpc::ErrorCode::Stale) =>
        {
            Failure::from(err)
                .with_message(format!("{what} already exists; pass --replace to replace it"))
        }
        other => other.into(),
    }
}

/// After a write: prints the config warnings about `item` (`rule "x"`, `agent "y"`), so a
/// definition that cannot work is pointed out when it is added.
pub async fn warn_about(
    ctx: &Ctx,
    client: &pulse_daemon::client::Client,
    item: &str,
) -> Result<(), Failure> {
    use pulse_contracts::rpc::{Empty, method};
    if ctx.json {
        return Ok(());
    }
    let report = client.call::<method::SystemConfigErrors>(Empty {}).await?;
    for w in report.warnings.iter().filter(|w| w.item.as_deref() == Some(item)) {
        crate::output::note(&format!("warning: {}", w.message));
    }
    Ok(())
}

pub fn now() -> Timestamp {
    Timestamp::from_system_time(SystemTime::now())
}

/// `2026-11-16T09:00:00Z`, or a duration measured from now (`ago`: back, else forward).
pub fn time_arg(s: &str, ago: bool) -> Result<Timestamp, Failure> {
    if let Ok(t) = Timestamp::from_str(s) {
        return Ok(t);
    }
    let d = Duration::from_str(s).map_err(|_| {
        Failure::invalid(format!(
            "expected a time like 2026-11-16T09:00:00Z or a duration like 2h, got {s:?}"
        ))
    })?;
    Ok(if ago { now().saturating_sub(d) } else { now().saturating_add(d) })
}

/// An event id as typed: a full id as is, a shorter one (its last characters, as lists show
/// them) resolved by the daemon.
pub async fn event_id(client: &Client, text: &str) -> Result<EventId, Failure> {
    if let Ok(id) = text.parse() {
        return Ok(id);
    }
    let params = ResolveIdParams { id: text.to_owned() };
    Ok(client.call::<method::EventsResolveId>(params).await?.id)
}

/// A run id as typed; see [`event_id`].
pub async fn run_id(client: &Client, text: &str) -> Result<RunId, Failure> {
    if let Ok(id) = text.parse() {
        return Ok(id);
    }
    let params = ResolveIdParams { id: text.to_owned() };
    Ok(client.call::<method::RunsResolveId>(params).await?.id)
}

pub fn parse_id<T: FromStr>(s: &str) -> Result<T, Failure>
where
    T::Err: std::fmt::Display,
{
    s.parse().map_err(|e: T::Err| Failure::invalid(e.to_string()))
}
