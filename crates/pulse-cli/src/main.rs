//! The `pulse` binary. `pulse start` runs the daemon; every other command is a client of it
//! over the RPC socket, using the typed calls from `pulse_contracts::rpc::method`.
//!
//! Output is for people by default and JSON with `--json` (the contract, including the error
//! object, is in `docs/user/cli.md`). Exit codes: 0 success, 1 error, 2 usage error, 3 the
//! daemon is not running. `pulse run` exits with the wrapped command's code.

mod cli;
mod commands;
mod failure;
mod format;
mod output;
mod tail;
mod templates;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use pulse_daemon::client::Client;

use crate::cli::{Cli, Command};
pub use crate::failure::Failure;
use crate::output::{Out, Text, note};

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

    /// The one printer for this invocation: text, or JSON under `--json`.
    pub fn out(&self) -> Out {
        Out::new(self.json)
    }

    /// For a command that answers once: under `--json`, prints `value` as the answer and
    /// returns `None`; otherwise returns the text printer for the command to describe it.
    pub fn answer<T: serde::Serialize>(&self, value: &T) -> Option<Text> {
        match self.out() {
            Out::Json(json) => {
                json.value(value);
                None
            }
            Out::Text(text) => Some(text),
        }
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

/// `--json` before any `--` (after it, words belong to `pulse run`'s command). For reporting a
/// usage error, which happens before the command line is understood.
fn json_requested() -> bool {
    std::env::args_os().skip(1).take_while(|a| a != "--").any(|a| a == "--json")
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            use clap::error::ErrorKind;
            let usage = !matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion);
            if usage && json_requested() {
                return report(true, Failure::usage(e.render().to_string().trim_end()));
            }
            e.exit();
        }
    };
    let (home, user_home) = match resolve_home(cli.home.clone()) {
        Ok(h) => h,
        Err(e) => return report(cli.json, Failure::invalid(e)),
    };
    let ctx = Ctx { home, user_home, json: cli.json };
    // `pulse mcp` keeps stdout for MCP, and `pulse run` belongs to the command it wraps: their
    // failures are text on stderr, `--json` or not.
    let json_errors = ctx.json && !matches!(cli.command, Command::Mcp(_) | Command::Run(_));
    // `pulse start` builds its own runtime (it may detach first).
    let result = if let Command::Start(args) = cli.command {
        commands::daemon::start(&ctx, args)
    } else {
        match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
            Ok(runtime) => {
                let result = runtime.block_on(commands::run(&ctx, cli.command));
                // Do not wait for blocking reads that are still pending (stdin in `pulse mcp`).
                runtime.shutdown_background();
                result
            }
            Err(e) => Err(Failure::internal(format!("cannot start: {e}"))),
        }
    };
    match result {
        Ok(code) => code,
        Err(failure) => report(json_errors, failure),
    }
}

/// Prints a failure: under `--json`, `{"error": …}` on stdout and nothing on stderr; otherwise
/// its message on stderr.
fn report(json: bool, failure: Failure) -> ExitCode {
    let code = ExitCode::from(failure.exit_code());
    if failure.reported.is_some() {
        return code;
    }
    match Out::new(json) {
        Out::Json(out) => out.value(&serde_json::json!({ "error": failure })),
        Out::Text(_) if failure.kind == "not_running" || failure.kind == "usage" => {
            note(&failure.message)
        }
        Out::Text(_) => note(&format!("error: {}", failure.message)),
    }
    code
}
