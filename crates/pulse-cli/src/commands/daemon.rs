//! `start`, `stop`, `status`, `pause`, `resume`, `open`, `init`.

use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::process::{ExitCode, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

use pulse_contracts::config::{ConfigOrigin, LogLevel, parse_config_file};
use pulse_contracts::rpc::{ConfigErrorList, Empty, ErrorCode, method};
use pulse_daemon::home::InstanceLock;
use pulse_daemon::{Daemon, DaemonOptions, PulseHome, StartError, Wiring};

use super::{Res, now};
use crate::cli::{InitArgs, OpenArgs, StartArgs};
use crate::output::{note, protocol_line};
use crate::{Ctx, Failure, templates};

/// `[daemon] log_level`, read before the daemon loads the file properly.
fn log_level(home: &PulseHome) -> LogLevel {
    std::fs::read_to_string(home.config_file())
        .ok()
        .and_then(|t| toml::from_str::<pulse_contracts::config::ConfigFile>(&t).ok())
        .and_then(|f| f.daemon)
        .map(|d| d.log_level)
        .unwrap_or_default()
}

/// Why the daemon could not start, as a failure for the user or, under `--ready-line`, for the
/// `--detach` parent to report.
fn start_failure(e: &StartError) -> Failure {
    let kind = match e {
        StartError::AlreadyRunning { .. } => "conflict",
        StartError::Io { .. } => "io",
        StartError::Store(_) | StartError::Other(_) => "internal",
    };
    Failure::new(kind, e.to_string())
}

pub fn start(ctx: &Ctx, args: StartArgs) -> Res {
    if args.detach {
        return detach(ctx, &args);
    }
    let failed = |f: Failure| {
        // The `--detach` parent reads stdout only; tell it why.
        if args.ready_line {
            if let Ok(json) = serde_json::to_string(&f) {
                protocol_line(&format!("failed {json}"));
            }
        }
        Err(f)
    };
    let home = PulseHome::new(&ctx.home);
    if let Err(e) = home.create_dirs() {
        return failed(Failure::new("io", format!("cannot create {}: {e}", ctx.home.display())));
    }
    // Detached, stderr goes away once the parent has read `ready`; log to the file only.
    if let Err(e) = pulse_daemon::logging::init(&home, log_level(&home), !args.ready_line) {
        note(&format!("pulse: cannot open the log: {e}"));
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => return failed(Failure::internal(e.to_string())),
    };
    runtime.block_on(async {
        let mut opts = DaemonOptions::new(&ctx.home, &ctx.user_home);
        opts.port = args.port;
        let daemon = match Daemon::start(opts, Wiring::real()).await {
            Ok(d) => d,
            Err(e) => return failed(start_failure(&e)),
        };
        let port = daemon.port();
        // Say what is wrong with the config now, while the user is looking.
        let problems = problem_lines(&daemon.services().config_report());
        if args.ready_line {
            // `--detach` passes these on before it returns.
            for p in &problems {
                protocol_line(&format!("note {p}"));
            }
            protocol_line(&format!("ready {port}"));
        } else {
            for p in &problems {
                note(p);
            }
            note(&format!(
                "Pulse {} on http://127.0.0.1:{port} (home {}). `pulse open` for the UI; Ctrl-C stops.",
                pulse_daemon::VERSION,
                ctx.home.display()
            ));
            // The same answer as `--detach`, once listening.
            ctx.answer(&serde_json::json!({ "pid": std::process::id(), "port": port }));
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
                note("pulse: cannot install signal handlers");
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
        Ok(ExitCode::SUCCESS)
    })
}

/// Starts `pulse start --ready-line` in its own process group and returns when it is ready.
fn detach(ctx: &Ctx, args: &StartArgs) -> Res {
    let exe = std::env::current_exe()
        .map_err(|e| Failure::new("io", format!("cannot find my own executable: {e}")))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--home").arg(&ctx.home).arg("start").arg("--ready-line");
    if let Some(p) = args.port {
        cmd.arg("--port").arg(p.to_string());
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
    let mut child =
        cmd.spawn().map_err(|e| Failure::new("io", format!("cannot start the daemon: {e}")))?;
    let (tx, rx) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                let n = stdout.read_line(&mut line).unwrap_or(0);
                let done = n == 0 || line.starts_with("ready ");
                if tx.send(line).is_err() || done {
                    break;
                }
            }
        });
    }
    let line = loop {
        let line = rx.recv_timeout(Duration::from_secs(30)).unwrap_or_default();
        match line.strip_prefix("note ") {
            Some(n) => note(n.trim_end()),
            None => break line,
        }
    };
    if let Some(port) = line.trim().strip_prefix("ready ") {
        let started = serde_json::json!({ "pid": child.id(), "port": port.parse::<u16>().ok() });
        if let Some(out) = ctx.answer(&started) {
            out.line(&format!(
                "Pulse started in the background (pid {}) on http://127.0.0.1:{port}",
                child.id()
            ));
        }
        // The daemon keeps running in its own process group after we exit.
        return Ok(ExitCode::SUCCESS);
    }
    let mut err = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut err);
    }
    let _ = child.wait();
    let reported = line.strip_prefix("failed ").and_then(|j| serde_json::from_str::<Value>(j).ok());
    Err(match reported {
        Some(f) => Failure::new(
            match f["kind"].as_str() {
                Some("conflict") => "conflict",
                Some("io") => "io",
                _ => "internal",
            },
            f["message"].as_str().unwrap_or("the daemon did not start"),
        ),
        None => Failure::internal(format!("the daemon did not start: {}", err.trim_end())),
    })
}

/// Config errors and warnings as lines for people.
pub fn problem_lines(report: &ConfigErrorList) -> Vec<String> {
    let mut lines = Vec::new();
    if !report.errors.is_empty() {
        lines.push(format!(
            "config: {} error(s); the last good config is in effect (`pulse status` shows them again):",
            report.errors.len()
        ));
        lines.extend(report.errors.iter().map(|e| format!("  error: {e}")));
    }
    if !report.warnings.is_empty() {
        lines.push(format!("config: {} warning(s):", report.warnings.len()));
        lines.extend(report.warnings.iter().map(|e| format!("  warning: {e}")));
    }
    lines
}

pub async fn stop(ctx: &Ctx) -> Res {
    let client = match ctx.client().await {
        Ok(c) => c,
        Err(f) if f.kind == "not_running" => {
            if let Some(out) = ctx.answer(&serde_json::json!({ "was_running": false })) {
                out.line("Pulse is not running.");
            }
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
            if let Some(out) = ctx.answer(&serde_json::json!({ "was_running": true })) {
                out.line("Pulse stopped.");
            }
            Ok(ExitCode::SUCCESS)
        }
        _ => Err(Failure::internal("the daemon has not exited after 2 minutes")),
    }
}

pub async fn status(ctx: &Ctx) -> Res {
    let client = ctx.client().await?;
    let s = client.call::<method::SystemStatusGet>(Empty {}).await?;
    let report = if s.config_errors + s.config_warnings > 0 {
        client.call::<method::SystemConfigErrors>(Empty {}).await?
    } else {
        ConfigErrorList { errors: Vec::new(), warnings: Vec::new() }
    };
    let answer = serde_json::json!({
        "status": s,
        "config_errors": report.errors,
        "config_warnings": report.warnings,
    });
    let Some(out) = ctx.answer(&answer) else { return Ok(ExitCode::SUCCESS) };
    out.line(&format!(
        "Pulse {} on http://127.0.0.1:{}, since {}",
        s.version, s.port, s.started_at
    ));
    out.line(&format!("home     {}", s.pulse_home));
    out.line(&format!("paused   {}", if s.paused { "yes (pulse resume)" } else { "no" }));
    out.line(&format!("rules    {}    agents {}", s.rules, s.agents));
    let src = s.sources;
    out.line(&format!(
        "sources  {} healthy, {} degraded, {} failed, {} other",
        src.healthy, src.degraded, src.failed, src.other
    ));
    if src.degraded + src.failed > 0 {
        out.line("         `pulse sources` says what is wrong with them.");
    }
    out.line(&format!(
        "runs     {} running, {} queued, {} waiting for approval",
        s.runs.running, s.runs.queued, s.runs.pending_approval
    ));
    if report.errors.is_empty() && report.warnings.is_empty() {
        out.line("config   ok");
    }
    if !report.errors.is_empty() {
        out.line(&format!(
            "config   {} error(s); the last good config is in effect:",
            report.errors.len()
        ));
        for e in &report.errors {
            out.line(&format!("  {e}"));
        }
    }
    if !report.warnings.is_empty() {
        out.line(&format!("config   {} warning(s):", report.warnings.len()));
        for e in &report.warnings {
            out.line(&format!("  {e}"));
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
    let Some(out) = ctx.answer(&s) else { return Ok(ExitCode::SUCCESS) };
    if s.paused {
        out.line(
            "Paused: rules are still evaluated and recorded, but nothing fires. `pulse resume` undoes it.",
        );
    } else {
        out.line("Resumed.");
    }
    Ok(ExitCode::SUCCESS)
}

pub async fn open(ctx: &Ctx, args: OpenArgs) -> Res {
    let home = PulseHome::new(&ctx.home);
    let info = home.read_daemon_info()?.ok_or_else(|| {
        Failure::not_running("Pulse is not running. Start it with `pulse start`.")
    })?;
    let token = home.read_token()?;
    // The browser will trust whatever answers on the port with a login code; make sure it is
    // this home's daemon first.
    pulse_daemon::client::Client::verify(info.port, &token).await?;
    // A single-use code that expires in two minutes, not the token.
    let code = pulse_daemon::auth::login_code(&token, now());
    let url = format!("http://127.0.0.1:{}/#login={code}", info.port);
    // Printed to the user's own terminal (or the desktop shell's pipe): no other user can
    // read that, unlike a process's arguments.
    let Some(out) = ctx.answer(&serde_json::json!({ "url": url })) else {
        return Ok(ExitCode::SUCCESS);
    };
    if args.print {
        out.line(&url);
        return Ok(ExitCode::SUCCESS);
    }
    let (mut cmd, page) = browser_command(&home, &url)?;
    match cmd.stdout(Stdio::null()).stderr(Stdio::null()).status() {
        Ok(s) if s.success() => {
            out.line(&format!("Opened the Pulse UI (port {}).", info.port));
            let hidden =
                page.components().any(|c| c.as_os_str().to_string_lossy().starts_with('.'));
            if hidden && cfg!(target_os = "linux") {
                out.line(&format!(
                    "If the browser says it cannot read {} (snap and flatpak browsers cannot read hidden directories), run `pulse open --print` and paste the link into it.",
                    page.display()
                ));
            }
        }
        _ => out.line(&format!("Open this URL within two minutes: {url}")),
    }
    Ok(ExitCode::SUCCESS)
}

/// The command that opens `url` in the browser: through a mode-0600 page in the Pulse home that
/// redirects to it, so the login code in `url` is never in the arguments of the opener or the
/// browser (other users can read every process's arguments in `/proc/<pid>/cmdline`).
fn browser_command(
    home: &PulseHome,
    url: &str,
) -> std::io::Result<(std::process::Command, std::path::PathBuf)> {
    let page = home.write_login_page(url)?;
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let mut cmd = std::process::Command::new(opener);
    cmd.arg(&page);
    Ok((cmd, page))
}

pub fn init(ctx: &Ctx, args: InitArgs) -> Res {
    if args.list {
        let list: Vec<_> = templates::TEMPLATES
            .iter()
            .map(|t| serde_json::json!({ "name": t.name, "summary": t.summary }))
            .collect();
        if let Some(out) = ctx.answer(&serde_json::json!({ "templates": list })) {
            for t in templates::TEMPLATES {
                out.line(&format!("{:<16} {}", t.name, t.summary));
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    let mut chosen = Vec::new();
    for name in &args.template {
        chosen.push(templates::find(name).ok_or_else(|| {
            Failure::invalid(format!("no template named {name:?}; `pulse init --list` shows them"))
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
    let names: Vec<&str> = chosen.iter().map(|t| t.name).collect();
    let answer = serde_json::json!({
        "path": path.display().to_string(),
        "created": existing.is_none(),
        "added": names,
    });
    if existing.is_some() && chosen.is_empty() {
        if let Some(out) = ctx.answer(&answer) {
            out.line(&format!(
                "{} already exists; nothing changed. Add a template with --template.",
                path.display()
            ));
        }
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
        return Err(Failure::new(
            ErrorCode::ConfigInvalid.name(),
            format!(
                "adding the template would make {} invalid, so nothing was written:\n{errors}",
                path.display()
            ),
        )
        .with_data(serde_json::json!({ "errors": errors.0 })));
    }
    // Adding a template rewrites the user's file: atomically, so a crash cannot truncate it.
    let mode = std::fs::metadata(&path).map_or(0o600, |m| {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o777
    });
    pulse_daemon::home::write_atomic(&path, text.as_bytes(), mode)?;
    let Some(out) = ctx.answer(&answer) else { return Ok(ExitCode::SUCCESS) };
    if existing.is_none() {
        out.line(&format!("Wrote {}.", path.display()));
    }
    for t in chosen {
        out.line(&format!(
            "Added the {} template to {}. Review its paths before relying on it.",
            t.name,
            path.display()
        ));
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn the_login_code_is_in_a_private_file_not_in_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let home = PulseHome::new(dir.path());
        let url = "http://127.0.0.1:7477/#login=1700000000000.c0ffee";
        let (cmd, page) = browser_command(&home, url).unwrap();
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, [page.display().to_string()]);
        assert!(!args.iter().any(|a| a.contains("c0ffee")), "{args:?}");
        let text = std::fs::read_to_string(&page).unwrap();
        assert!(text.contains(&format!("content=\"0;url={url}\"")), "{text}");
        assert!(text.contains(&format!("location.replace(\"{url}\")")), "{text}");
        let mode = std::fs::metadata(&page).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        // The next `pulse open` replaces it: the pages do not pile up.
        browser_command(&home, "http://127.0.0.1:7477/#login=2.ab").unwrap();
        assert!(!std::fs::read_to_string(&page).unwrap().contains("c0ffee"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
