//! The `pulse` binary. `pulse start` runs the daemon; every other command is a client of it
//! over the RPC socket, using the typed calls from `pulse_contracts::rpc::method`.
//!
//! Output is for people by default and JSON with `--json`. Exit codes: 0 success, 1 error,
//! 2 usage error, 3 the daemon is not running. `pulse run` exits with the wrapped command's
//! code.

mod cli;
mod commands;
mod format;
mod templates;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use pulse_daemon::client::{Client, ClientError};

use crate::cli::{Cli, Command};

/// Where the CLI runs: the Pulse home and how to print.
#[derive(Debug)]
pub struct Ctx {
    pub home: PathBuf,
    pub user_home: PathBuf,
    pub json: bool,
}

impl Ctx {
    pub async fn client(&self) -> Result<Client, Failure> {
        Client::connect(&self.home).await.map_err(Failure::from)
    }
}

/// Why a command failed, and how to exit.
#[derive(Debug)]
pub enum Failure {
    NotRunning(String),
    Message(String),
    /// Already reported; exit with this code.
    Exit(u8),
}

impl From<ClientError> for Failure {
    fn from(e: ClientError) -> Self {
        match e {
            ClientError::NotRunning(_) => Failure::NotRunning(e.to_string()),
            ClientError::Rpc(err) => {
                let mut msg = err.message.clone();
                if let Some(errors) = err.data.as_ref().and_then(|d| d.get("errors")) {
                    if let Ok(list) = serde_json::from_value::<
                        Vec<pulse_contracts::config::ConfigError>,
                    >(errors.clone())
                    {
                        msg = list.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
                    }
                }
                Failure::Message(msg)
            }
            other => Failure::Message(other.to_string()),
        }
    }
}

impl From<std::io::Error> for Failure {
    fn from(e: std::io::Error) -> Self {
        Failure::Message(e.to_string())
    }
}

fn resolve_home(flag: Option<PathBuf>) -> Result<(PathBuf, PathBuf), String> {
    let user_home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set; Pulse needs it to expand `~` in config")?;
    let home = flag
        .or_else(|| std::env::var_os("PULSE_HOME").map(PathBuf::from))
        .unwrap_or_else(|| user_home.join(".pulse"));
    let home = if home.is_absolute() {
        home
    } else {
        std::env::current_dir().map_err(|e| e.to_string())?.join(home)
    };
    Ok((home, user_home))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (home, user_home) = match resolve_home(cli.home.clone()) {
        Ok(h) => h,
        Err(e) => {
            eprint_line(&format!("pulse: {e}"));
            return ExitCode::from(1);
        }
    };
    let ctx = Ctx { home, user_home, json: cli.json };
    // `pulse start` builds its own runtime (it may detach first).
    if let Command::Start(args) = cli.command {
        return commands::daemon::start(&ctx, args);
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprint_line(&format!("pulse: cannot start: {e}"));
            return ExitCode::from(1);
        }
    };
    match runtime.block_on(commands::run(&ctx, cli.command)) {
        Ok(code) => code,
        Err(Failure::NotRunning(msg)) => {
            eprint_line(&msg);
            ExitCode::from(3)
        }
        Err(Failure::Message(msg)) => {
            eprint_line(&format!("error: {msg}"));
            ExitCode::from(1)
        }
        Err(Failure::Exit(code)) => ExitCode::from(code),
    }
}

/// Writes a line to stdout, ignoring a closed pipe (`pulse events | head`).
pub fn print_line(s: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stdout().lock(), "{s}");
}

pub fn eprint_line(s: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr().lock(), "{s}");
}

/// Prints a value as pretty JSON (for `--json`).
pub fn print_json<T: serde::Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => print_line(&s),
        Err(e) => eprint_line(&format!("error: {e}")),
    }
}
