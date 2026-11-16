//! The `pulse` binary. `pulse start` will run the daemon; every other subcommand is a client
//! of it over the RPC socket (`pulse_contracts::rpc::method::*` for typed calls).
//!
//! Not built yet: it prints its version.

use std::io::Write;

fn main() -> std::process::ExitCode {
    let mut out = std::io::stdout().lock();
    match writeln!(out, "pulse {}", pulse_daemon::VERSION) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => std::process::ExitCode::FAILURE,
    }
}
