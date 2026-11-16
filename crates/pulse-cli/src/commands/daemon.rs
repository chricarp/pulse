//! `start`, `stop`, `status`, `pause`, `resume`, `open`, `init`.

use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::process::{ExitCode, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use pulse_contracts::config::{ConfigOrigin, LogLevel, parse_config_file};
use pulse_contracts::rpc::{Empty, method};
use pulse_daemon::home::InstanceLock;
use pulse_daemon::{Daemon, DaemonOptions, PulseHome, StartError, Wiring};

use super::{Res, now};
use crate::cli::{InitArgs, OpenArgs, StartArgs};
use crate::{Ctx, Failure, eprint_line, print_json, print_line, templates};

/// `[daemon] log_level`, read before the daemon loads the file properly.
fn log_level(home: &PulseHome) -> LogLevel {
    std::fs::read_to_string(home.config_file())
        .ok()
        .and_then(|t| toml::from_str::<pulse_contracts::config::ConfigFile>(&t).ok())
        .and_then(|f| f.daemon)
        .map(|d| d.log_level)
        .unwrap_or_default()
}

pub fn start(ctx: &Ctx, args: StartArgs) -> ExitCode {
    if args.detach {
        return detach(ctx, &args);
    }
    let home = PulseHome::new(&ctx.home);
    if let Err(e) = home.create_dirs() {
        eprint_line(&format!("pulse: cannot create {}: {e}", ctx.home.display()));
        return ExitCode::from(1);
    }
    // Detached, stderr goes away once the parent has read `ready`; log to the file only.
    if let Err(e) = pulse_daemon::logging::init(&home, log_level(&home), !args.ready_line) {
        eprint_line(&format!("pulse: cannot open the log: {e}"));
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprint_line(&format!("pulse: {e}"));
            return ExitCode::from(1);
        }
    };
    runtime.block_on(async {
        let mut opts = DaemonOptions::new(&ctx.home, &ctx.user_home);
        opts.port = args.port;
        let daemon = match Daemon::start(opts, Wiring::real()).await {
            Ok(d) => d,
            Err(e) => {
                eprint_line(&format!("pulse: {e}"));
                return ExitCode::from(if matches!(e, StartError::AlreadyRunning { .. }) { 1 } else { 2 });
            }
        };
        let port = daemon.port();
        if args.ready_line {
            print_line(&format!("ready {port}"));
        } else {
            eprint_line(&format!(
                "Pulse {} on http://127.0.0.1:{port} (home {}). `pulse open` for the UI; Ctrl-C stops.",
                pulse_daemon::VERSION,
                ctx.home.display()
            ));
        }
        let stop = daemon.shutdown_token();
        let services = daemon.services();
        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let (Ok(mut term), Ok(mut int), Ok(mut hup)) = (
                signal(SignalKind::terminate()),
                signal(SignalKind::interrupt()),
                signal(SignalKind::hangup()),
            ) else {
                eprint_line("pulse: cannot install signal handlers");
                stop.cancel();
                return;
            };
            loop {
                tokio::select! {
                    _ = term.recv() => break,
                    _ = int.recv() => break,
                    // SIGHUP reloads the config, as daemons traditionally do.
                    _ = hup.recv() => services.reload_config().await,
                    _ = stop.cancelled() => return,
                }
            }
            stop.cancel();
        });
        daemon.wait().await;
        ExitCode::SUCCESS
    })
}

/// Starts `pulse start --ready-line` in its own process group and returns when it is ready.
fn detach(ctx: &Ctx, args: &StartArgs) -> ExitCode {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprint_line(&format!("pulse: cannot find my own executable: {e}"));
            return ExitCode::from(1);
        }
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--home").arg(&ctx.home).arg("start").arg("--ready-line");
    if let Some(p) = args.port {
        cmd.arg("--port").arg(p.to_string());
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprint_line(&format!("pulse: cannot start the daemon: {e}"));
            return ExitCode::from(1);
        }
    };
    let (tx, rx) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(stdout).read_line(&mut line);
            let _ = tx.send(line);
        });
    }
    let line = rx.recv_timeout(Duration::from_secs(30)).unwrap_or_default();
    if let Some(port) = line.trim().strip_prefix("ready ") {
        if ctx.json {
            print_json(&serde_json::json!({ "pid": child.id(), "port": port.parse::<u16>().ok() }));
        } else {
            print_line(&format!(
                "Pulse started in the background (pid {}) on http://127.0.0.1:{port}",
                child.id()
            ));
        }
        // The daemon keeps running in its own process group after we exit.
        return ExitCode::SUCCESS;
    }
    let mut err = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut err);
    }
    let _ = child.wait();
    eprint_line(err.trim_end());
    eprint_line("pulse: the daemon did not start");
    ExitCode::from(1)
}

pub async fn stop(ctx: &Ctx) -> Res {
    let client = match ctx.client().await {
        Ok(c) => c,
        Err(Failure::NotRunning(_)) => {
            print_line("Pulse is not running.");
            return Ok(ExitCode::SUCCESS);
        }
        Err(e) => return Err(e),
    };
    client.call::<method::SystemShutdown>(Empty {}).await?;
    drop(client);
    // The lock is released when the daemon process has exited, after it terminated its runs.
    let home = PulseHome::new(&ctx.home);
    let waited = tokio::time::timeout(
        Duration::from_secs(120),
        tokio::task::spawn_blocking(move || InstanceLock::wait_released(&home)),
    )
    .await;
    match waited {
        Ok(Ok(Ok(()))) => {
            print_line("Pulse stopped.");
            Ok(ExitCode::SUCCESS)
        }
        _ => Err(Failure::Message("the daemon has not exited after 2 minutes".into())),
    }
}

pub async fn status(ctx: &Ctx) -> Res {
    let client = ctx.client().await?;
    let s = client.call::<method::SystemStatusGet>(Empty {}).await?;
    let errors = if s.config_errors > 0 {
        client.call::<method::SystemConfigErrors>(Empty {}).await?.errors
    } else {
        Vec::new()
    };
    if ctx.json {
        print_json(&serde_json::json!({ "status": s, "config_errors": errors }));
        return Ok(ExitCode::SUCCESS);
    }
    print_line(&format!(
        "Pulse {} on http://127.0.0.1:{}, since {}",
        s.version, s.port, s.started_at
    ));
    print_line(&format!("home     {}", s.pulse_home));
    print_line(&format!("paused   {}", if s.paused { "yes (pulse resume)" } else { "no" }));
    print_line(&format!("rules    {}    agents {}", s.rules, s.agents));
    let src = s.sources;
    print_line(&format!(
        "sources  {} healthy, {} degraded, {} failed, {} other",
        src.healthy, src.degraded, src.failed, src.other
    ));
    print_line(&format!(
        "runs     {} running, {} queued, {} waiting for approval",
        s.runs.running, s.runs.queued, s.runs.pending_approval
    ));
    if errors.is_empty() {
        print_line("config   ok");
    } else {
        print_line(&format!(
            "config   {} error(s); the last good config is in effect:",
            errors.len()
        ));
        for e in errors {
            print_line(&format!("  {e}"));
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub async fn pause(ctx: &Ctx, paused: bool) -> Res {
    let client = ctx.client().await?;
    let s = if paused {
        client.call::<method::SystemPause>(Empty {}).await?
    } else {
        client.call::<method::SystemResume>(Empty {}).await?
    };
    if ctx.json {
        print_json(&s);
    } else if s.paused {
        print_line(
            "Paused: rules are still evaluated and recorded, but nothing fires. `pulse resume` undoes it.",
        );
    } else {
        print_line("Resumed.");
    }
    Ok(ExitCode::SUCCESS)
}

pub async fn open(ctx: &Ctx, args: OpenArgs) -> Res {
    let home = PulseHome::new(&ctx.home);
    let info = home.read_daemon_info()?.ok_or_else(|| {
        Failure::NotRunning("Pulse is not running. Start it with `pulse start`.".into())
    })?;
    let token = home.read_token()?;
    // A single-use code that expires in two minutes, not the token: URLs end up in process
    // lists and browser history.
    let code = pulse_daemon::auth::login_code(&token, now());
    let url = format!("http://127.0.0.1:{}/#login={code}", info.port);
    if args.print || ctx.json {
        if ctx.json {
            print_json(&serde_json::json!({ "url": url }));
        } else {
            print_line(&url);
        }
        return Ok(ExitCode::SUCCESS);
    }
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    match std::process::Command::new(opener)
        .arg(&url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(s) if s.success() => print_line(&format!("Opened the Pulse UI (port {}).", info.port)),
        _ => print_line(&format!("Open this URL within two minutes: {url}")),
    }
    Ok(ExitCode::SUCCESS)
}

pub fn init(ctx: &Ctx, args: InitArgs) -> Res {
    if args.list {
        for t in templates::TEMPLATES {
            print_line(&format!("{:<16} {}", t.name, t.summary));
        }
        return Ok(ExitCode::SUCCESS);
    }
    let mut chosen = Vec::new();
    for name in &args.template {
        chosen.push(templates::find(name).ok_or_else(|| {
            Failure::Message(format!("no template named {name:?}; `pulse init --list` shows them"))
        })?);
    }
    let home = PulseHome::new(&ctx.home);
    home.create_dirs()?;
    let path = home.config_file();
    let existing = match std::fs::read_to_string(&path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    if existing.is_some() && chosen.is_empty() {
        print_line(&format!(
            "{} already exists; nothing changed. Add a template with --template.",
            path.display()
        ));
        return Ok(ExitCode::SUCCESS);
    }
    let mut text = existing.clone().unwrap_or_else(|| templates::STARTER.to_owned());
    for t in &chosen {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push('\n');
        text.push_str(t.text);
    }
    let origin = ConfigOrigin::User { path: path.clone() };
    if let Err(errors) = parse_config_file(&text, &origin, &ctx.user_home) {
        return Err(Failure::Message(format!(
            "adding the template would make {} invalid, so nothing was written:\n{errors}",
            path.display()
        )));
    }
    std::fs::write(&path, &text)?;
    if existing.is_none() {
        print_line(&format!("Wrote {}.", path.display()));
    }
    for t in chosen {
        print_line(&format!(
            "Added the {} template to {}. Review its paths before relying on it.",
            t.name,
            path.display()
        ));
    }
    Ok(ExitCode::SUCCESS)
}
