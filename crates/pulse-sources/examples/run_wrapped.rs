//! Runs a command through the `pulse run` wrapper and prints the resulting `process.exited`
//! data, for checking terminal behaviour by hand:
//!
//! ```sh
//! cargo run -p pulse-sources --example run_wrapped -- <command> [args...]
//! ```
#![allow(clippy::print_stderr, clippy::unwrap_used, clippy::expect_used)]

#[tokio::main]
async fn main() {
    let command: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().expect("a working directory");
    let data = pulse_sources::process::run_wrapped(&command, &cwd, 4096).await.unwrap();
    eprintln!("\n--- process.exited ---\n{}", serde_json::to_string_pretty(&data).unwrap());
    let code = data.exit_code.unwrap_or(128 + data.signal.unwrap_or(0));
    std::process::exit(code);
}
