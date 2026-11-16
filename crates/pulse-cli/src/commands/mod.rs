//! One function per command. Each decodes its arguments, calls the daemon, and prints.

pub mod daemon;
mod events;
mod misc;
mod rules;
mod runs;

use std::process::ExitCode;
use std::str::FromStr;
use std::time::SystemTime;

use pulse_contracts::{Duration, Timestamp};

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
        Command::Sources => misc::sources(ctx).await,
        Command::Project(cmd) => misc::project(ctx, cmd).await,
    }
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
        Failure::Message(format!(
            "expected a time like 2026-11-16T09:00:00Z or a duration like 2h, got {s:?}"
        ))
    })?;
    Ok(if ago { now().saturating_sub(d) } else { now().saturating_add(d) })
}

pub fn parse_id<T: FromStr>(s: &str) -> Result<T, Failure>
where
    T::Err: std::fmt::Display,
{
    s.parse().map_err(|e: T::Err| Failure::Message(e.to_string()))
}
