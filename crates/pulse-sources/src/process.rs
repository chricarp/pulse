//! The process wrapper behind `pulse run -- <command>`: runs the command so it behaves as if run
//! directly, keeps a bounded tail of its combined output, and describes how it exited. The CLI
//! then reports it with `events.report` (and stays silent if the daemon is down).
//!
//! **Terminal.** When stdout and stderr are terminals, the command runs on a pseudo-terminal:
//! it sees a TTY (colours, line buffering, progress bars), stdout and stderr arrive in one stream
//! in their real order, and the window size follows ours. If stdin is a terminal too, it is put
//! in raw mode and forwarded, so keystrokes (including Ctrl-C, Ctrl-Z and password prompts that
//! read `/dev/tty`) reach the command through its terminal exactly as they would directly.
//! Otherwise (CI, redirected output) the command gets pipes, and each chunk is passed through
//! the moment it arrives.
//!
//! **Signals.** SIGINT, SIGTERM, SIGHUP and SIGQUIT sent to the wrapper are forwarded to the
//! command (its process group when it has its own). When the wrapper shares the command's
//! foreground process group on a terminal (pipe mode with an interactive stdin), the terminal
//! already delivered SIGINT and SIGQUIT to both, so those are not sent a second time.
//!
//! **Exit.** `exit_code` is `None` when the command was killed by a signal (`signal` says which);
//! a shell would report that as `128 + signal`, and the CLI should exit the same way.
//!
//! The tail is cleaned for humans and prompts: terminal escape sequences are removed, carriage
//! return overwrites (progress bars) keep their final text, and it is cut to `tail_bytes` at a
//! character boundary, dropping a partial first line.

use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Instant;

use pulse_contracts::catalog::ProcessExitedData;
use rustix::process::{Pid, Signal};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::signal::unix::{SignalKind, signal};

use crate::SourceError;

/// After the command exits, output still in flight is read for at most this long; a background
/// process that inherited the output must not keep the wrapper alive.
const DRAIN: std::time::Duration = std::time::Duration::from_millis(200);

/// Runs `command` (argv; never a shell string) in `cwd`, passing output through to the
/// terminal while keeping at most `tail_bytes` of combined stdout and stderr.
pub async fn run_wrapped(
    command: &[String],
    cwd: &Path,
    tail_bytes: usize,
) -> Result<ProcessExitedData, SourceError> {
    let tty = |fd: rustix::fd::BorrowedFd<'_>| rustix::termios::isatty(fd);
    let (stdin, stdout, stderr) =
        (tty(rustix::stdio::stdin()), tty(rustix::stdio::stdout()), tty(rustix::stdio::stderr()));
    let mode = if stdout && stderr {
        Mode::Pty { raw_stdin: stdin }
    } else {
        // In the terminal's foreground group with the command, terminal signals reach it
        // directly; otherwise give it its own group and forward everything.
        let foreground = stdin
            && rustix::termios::tcgetpgrp(rustix::stdio::stdin()).ok()
                == Some(rustix::process::getpgrp());
        Mode::Pipes { own_group: !foreground }
    };
    run(command, cwd, tail_bytes, mode, &mut StdOutput).await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Pty { raw_stdin: bool },
    Pipes { own_group: bool },
}

/// Where passed-through output goes.
pub(crate) trait Output {
    fn stdout(&mut self, bytes: &[u8]);
    fn stderr(&mut self, bytes: &[u8]);
}

struct StdOutput;

impl Output for StdOutput {
    fn stdout(&mut self, bytes: &[u8]) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(bytes);
        let _ = out.flush();
    }

    fn stderr(&mut self, bytes: &[u8]) {
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(bytes);
        let _ = err.flush();
    }
}

/// The last `cap` bytes of output.
struct Tail {
    buf: VecDeque<u8>,
    cap: usize,
    truncated: bool,
}

impl Tail {
    fn push(&mut self, bytes: &[u8]) {
        let bytes = &bytes[bytes.len().saturating_sub(self.cap)..];
        let overflow = (self.buf.len() + bytes.len()).saturating_sub(self.cap);
        if overflow > 0 {
            self.truncated = true;
            self.buf.drain(..overflow.min(self.buf.len()));
        }
        self.buf.extend(bytes);
    }

    fn text(&self) -> String {
        let (a, b) = self.buf.as_slices();
        clean_tail(&[a, b].concat(), self.cap, self.truncated)
    }
}

/// Signals the wrapper catches and forwards. Created before spawning so a signal that arrives
/// during startup is not lost (or fatal to the wrapper).
struct Signals {
    streams: Vec<(Signal, tokio::signal::unix::Signal)>,
}

impl Signals {
    fn install() -> std::io::Result<Signals> {
        let kinds = [
            (Signal::INT, SignalKind::interrupt()),
            (Signal::TERM, SignalKind::terminate()),
            (Signal::HUP, SignalKind::hangup()),
            (Signal::QUIT, SignalKind::quit()),
        ];
        let mut streams = Vec::new();
        for (sig, kind) in kinds {
            streams.push((sig, signal(kind)?));
        }
        Ok(Signals { streams })
    }

    async fn recv(&mut self) -> Signal {
        let futures = self.streams.iter_mut().map(|(sig, s)| {
            let sig = *sig;
            Box::pin(async move {
                s.recv().await;
                sig
            })
        });
        futures_select_all(futures).await
    }
}

/// The first of several futures to finish (a tiny `select_all`, to avoid a dependency).
async fn futures_select_all<F: std::future::Future<Output = Signal> + Unpin>(
    futures: impl Iterator<Item = F>,
) -> Signal {
    let mut futures: Vec<F> = futures.collect();
    std::future::poll_fn(move |cx| {
        for f in &mut futures {
            if let std::task::Poll::Ready(v) = std::pin::Pin::new(f).poll(cx) {
                return std::task::Poll::Ready(v);
            }
        }
        std::task::Poll::Pending
    })
    .await
}

fn forward(pid: Option<u32>, sig: Signal, group: bool) {
    let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()).and_then(Pid::from_raw) else { return };
    // Only the process (group) we spawned and have not reaped: its id cannot have been reused.
    let _ = if group {
        rustix::process::kill_process_group(pid, sig)
    } else {
        rustix::process::kill_process(pid, sig)
    };
}

/// Restores the terminal's mode when dropped.
struct RawMode {
    saved: rustix::termios::Termios,
}

impl RawMode {
    fn enter() -> Option<RawMode> {
        let fd = rustix::stdio::stdin();
        let saved = rustix::termios::tcgetattr(fd).ok()?;
        let mut raw = saved.clone();
        raw.make_raw();
        rustix::termios::tcsetattr(fd, rustix::termios::OptionalActions::Now, &raw).ok()?;
        Some(RawMode { saved })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = rustix::termios::tcsetattr(
            rustix::stdio::stdin(),
            rustix::termios::OptionalActions::Now,
            &self.saved,
        );
    }
}

fn spawn_error(command: &[String], e: impl std::fmt::Display) -> SourceError {
    SourceError::Io {
        context: format!("cannot run {:?}", command.first().map(String::as_str).unwrap_or("")),
        source: std::io::Error::other(e.to_string()),
    }
}

pub(crate) async fn run(
    command: &[String],
    cwd: &Path,
    tail_bytes: usize,
    mode: Mode,
    output: &mut dyn Output,
) -> Result<ProcessExitedData, SourceError> {
    let Some(program) = command.first() else {
        return Err(SourceError::Config("no command given".into()));
    };
    let mut signals = Signals::install().map_err(|e| spawn_error(command, e))?;
    let mut tail = Tail { buf: VecDeque::new(), cap: tail_bytes, truncated: false };
    let started = Instant::now();
    let status = match mode {
        Mode::Pipes { own_group } => {
            let mut cmd = tokio::process::Command::new(program);
            cmd.args(&command[1..])
                .current_dir(cwd)
                .stdin(Stdio::inherit())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if own_group {
                cmd.process_group(0);
            }
            let mut child = cmd.spawn().map_err(|e| spawn_error(command, e))?;
            let pid = child.id();
            let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
            let (mut ob, mut eb) = (vec![0u8; 16 * 1024], vec![0u8; 16 * 1024]);
            let mut status: Option<ExitStatus> = None;
            let mut drain_until: Option<tokio::time::Instant> = None;
            loop {
                if out.is_none() && err.is_none() && status.is_some() {
                    break;
                }
                tokio::select! {
                    n = read_some(&mut out, &mut ob) => match n {
                        Some(n) => { output.stdout(&ob[..n]); tail.push(&ob[..n]); }
                        None => out = None,
                    },
                    n = read_some(&mut err, &mut eb) => match n {
                        Some(n) => { output.stderr(&eb[..n]); tail.push(&eb[..n]); }
                        None => err = None,
                    },
                    sig = signals.recv() => {
                        let from_terminal = !own_group && (sig == Signal::INT || sig == Signal::QUIT);
                        if !from_terminal {
                            forward(pid, sig, own_group);
                        }
                    }
                    s = child.wait(), if status.is_none() => {
                        status = Some(s.map_err(|e| spawn_error(command, e))?);
                        drain_until = Some(tokio::time::Instant::now() + DRAIN);
                    }
                    () = sleep_until(drain_until), if drain_until.is_some() => break,
                }
            }
            status
        }
        Mode::Pty { raw_stdin } => {
            let (pty, pts) = pty_process::open().map_err(|e| spawn_error(command, e))?;
            resize(&pty);
            let mut cmd = pty_process::Command::new(program).args(&command[1..]).current_dir(cwd);
            if !raw_stdin {
                cmd = cmd.stdin(Stdio::inherit());
            }
            let mut child = cmd.spawn(pts).map_err(|e| spawn_error(command, e))?;
            let pid = child.id();
            let (mut reader, mut writer) = pty.into_split();
            let _raw = if raw_stdin { RawMode::enter() } else { None };
            let mut keys = raw_stdin.then(stdin_reader);
            let mut winch = signal(SignalKind::window_change()).ok();
            let mut buf = vec![0u8; 16 * 1024];
            let mut status: Option<ExitStatus> = None;
            let mut drain_until: Option<tokio::time::Instant> = None;
            let mut open = true;
            loop {
                if !open && status.is_some() {
                    break;
                }
                tokio::select! {
                    n = reader.read(&mut buf), if open => match n {
                        // The pty reports EIO once the command's side is closed.
                        Ok(0) | Err(_) => open = false,
                        Ok(n) => { output.stdout(&buf[..n]); tail.push(&buf[..n]); }
                    },
                    k = recv_keys(&mut keys) => match k {
                        Some(bytes) => { let _ = tokio::io::AsyncWriteExt::write_all(&mut writer, &bytes).await; }
                        None => keys = None,
                    },
                    () = recv_winch(&mut winch) => {
                        if let Ok(ws) = rustix::termios::tcgetwinsize(rustix::stdio::stdout()) {
                            let _ = writer.resize(pty_process::Size::new(ws.ws_row, ws.ws_col));
                        }
                    }
                    sig = signals.recv() => forward(pid, sig, true),
                    s = child.wait(), if status.is_none() => {
                        status = Some(s.map_err(|e| spawn_error(command, e))?);
                        drain_until = Some(tokio::time::Instant::now() + DRAIN);
                    }
                    () = sleep_until(drain_until), if drain_until.is_some() => break,
                }
            }
            status
        }
    };
    let Some(status) = status else { return Err(spawn_error(command, "no exit status")) };
    use std::os::unix::process::ExitStatusExt;
    let cwd =
        cwd.to_str().and_then(pulse_contracts::pattern::clean_absolute).unwrap_or_else(|| {
            std::fs::canonicalize(cwd)
                .ok()
                .and_then(|p| p.to_str().map(str::to_owned))
                .unwrap_or_default()
        });
    Ok(ProcessExitedData {
        command: command.to_vec(),
        cwd,
        exit_code: status.code(),
        success: status.success(),
        signal: status.signal(),
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        output_tail: tail.text(),
    })
}

fn resize(pty: &pty_process::Pty) {
    if let Ok(ws) = rustix::termios::tcgetwinsize(rustix::stdio::stdout()) {
        let _ = pty.resize(pty_process::Size::new(ws.ws_row, ws.ws_col));
    }
}

async fn read_some<R: AsyncRead + Unpin>(r: &mut Option<R>, buf: &mut [u8]) -> Option<usize> {
    match r {
        Some(r) => match r.read(buf).await {
            Ok(0) | Err(_) => None,
            Ok(n) => Some(n),
        },
        None => std::future::pending().await,
    }
}

async fn sleep_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

async fn recv_keys(keys: &mut Option<tokio::sync::mpsc::Receiver<Vec<u8>>>) -> Option<Vec<u8>> {
    match keys {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

async fn recv_winch(s: &mut Option<tokio::signal::unix::Signal>) {
    match s {
        Some(s) => {
            s.recv().await;
        }
        None => std::future::pending().await,
    }
}

/// Reads our stdin on a thread. A blocked terminal read cannot be cancelled, so the thread is
/// left to end with the process (the CLI exits right after the command does).
fn stdin_reader() -> tokio::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 1024];
        let mut stdin = std::io::stdin();
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 || tx.blocking_send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    rx
}

/// Terminal output as plain text: no escape sequences, carriage-return overwrites resolved,
/// at most `max` bytes, ending at the end of the output.
fn clean_tail(raw: &[u8], max: usize, truncated: bool) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            plain.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters, then a final byte in @..~.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC (titles, hyperlinks): until BEL or ESC \.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    let mut lines: Vec<String> = Vec::new();
    for line in plain.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let last = line.rsplit('\r').next().unwrap_or(line);
        lines.push(last.chars().filter(|c| !c.is_control() || *c == '\t').collect());
    }
    if truncated && lines.len() > 1 {
        // The first line starts mid-way (perhaps mid-escape); drop it.
        lines.remove(0);
    }
    let mut out = lines.join("\n");
    if out.len() > max {
        let mut start = out.len() - max;
        while !out.is_char_boundary(start) {
            start += 1;
        }
        out = out[start..].to_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Captured {
        out: Vec<u8>,
        err: Vec<u8>,
    }

    impl Output for Captured {
        fn stdout(&mut self, bytes: &[u8]) {
            self.out.extend_from_slice(bytes);
        }
        fn stderr(&mut self, bytes: &[u8]) {
            self.err.extend_from_slice(bytes);
        }
    }

    fn sh(script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    async fn go(script: &str, tail: usize, mode: Mode) -> (ProcessExitedData, Captured) {
        let mut cap = Captured::default();
        let dir = std::env::temp_dir();
        let data = run(&sh(script), &dir, tail, mode, &mut cap).await.unwrap();
        (data, cap)
    }

    const PIPES: Mode = Mode::Pipes { own_group: true };

    #[tokio::test]
    async fn exit_codes_signals_and_passthrough() {
        let (d, cap) = go("echo out; echo err >&2; echo out2; exit 3", 4096, PIPES).await;
        assert_eq!((d.exit_code, d.success, d.signal), (Some(3), false, None));
        assert_eq!(cap.out, b"out\nout2\n");
        assert_eq!(cap.err, b"err\n");
        for line in ["out", "err", "out2"] {
            assert!(d.output_tail.lines().any(|l| l == line), "{:?}", d.output_tail);
        }
        assert_eq!(d.command, sh("echo out; echo err >&2; echo out2; exit 3"));
        assert!(d.cwd.starts_with('/'));

        let (d, _) = go("true", 4096, PIPES).await;
        assert_eq!((d.exit_code, d.success), (Some(0), true));

        let (d, _) = go("kill -TERM $$", 4096, PIPES).await;
        assert_eq!((d.exit_code, d.success, d.signal), (None, false, Some(15)));
    }

    #[tokio::test]
    async fn the_tail_is_bounded_and_ends_with_the_last_output() {
        let (d, cap) =
            go("i=0; while [ $i -lt 5000 ]; do echo \"line $i\"; i=$((i+1)); done", 1000, PIPES)
                .await;
        assert!(cap.out.len() > 40_000);
        assert!(d.output_tail.len() <= 1000);
        assert!(
            d.output_tail.ends_with("line 4999\n"),
            "{:?}",
            &d.output_tail[d.output_tail.len() - 40..]
        );
        assert!(d.output_tail.starts_with("line "), "the partial first line is dropped");
    }

    #[tokio::test]
    async fn a_pty_looks_like_a_terminal_and_escapes_are_cleaned() {
        let (d, cap) = go(
            "test -t 1 && echo tty-out; test -t 2 && echo tty-err >&2; printf '\\033[31mred\\033[0m\\n'; printf 'progress 10%%\\rprogress 100%%\\n'",
            4096,
            Mode::Pty { raw_stdin: false },
        )
        .await;
        assert_eq!(d.exit_code, Some(0));
        let raw = String::from_utf8_lossy(&cap.out);
        assert!(
            raw.contains("tty-out") && raw.contains("tty-err") && raw.contains("\u{1b}[31m"),
            "{raw:?}"
        );
        assert_eq!(d.output_tail, "tty-out\ntty-err\nred\nprogress 100%\n");
        // Without a pty, the command sees pipes, as it would if its output were redirected.
        let (d, _) = go("test -t 1 && echo tty || echo pipe", 4096, PIPES).await;
        assert_eq!(d.output_tail, "pipe\n");
    }

    #[tokio::test]
    async fn signals_are_forwarded_to_the_commands_group() {
        let mut child =
            tokio::process::Command::new("sleep").arg("30").process_group(0).spawn().unwrap();
        forward(child.id(), Signal::TERM, true);
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(child.wait().await.unwrap().signal(), Some(15));
    }

    #[tokio::test]
    async fn a_background_process_holding_the_output_does_not_hang_the_wrapper() {
        let (d, _) = go("sleep 3 & echo started", 4096, PIPES).await;
        assert_eq!(d.exit_code, Some(0));
        assert!(d.duration_ms < 10_000);
    }

    #[tokio::test]
    async fn a_missing_program_is_an_error() {
        let mut cap = Captured::default();
        let err =
            run(&["definitely-not-a-program-xyz".into()], Path::new("/"), 10, PIPES, &mut cap)
                .await
                .unwrap_err();
        assert!(err.to_string().contains("definitely-not-a-program-xyz"), "{err}");
        assert!(run(&[], Path::new("/"), 10, PIPES, &mut cap).await.is_err());
    }

    #[test]
    fn tail_cleaning() {
        assert_eq!(clean_tail(b"\x1b]0;title\x07a\x1b[1;32mb\x1b[0m\r\n", 100, false), "ab\n");
        assert_eq!(clean_tail("é".repeat(10).as_bytes(), 5, false), "éé");
        assert_eq!(clean_tail(b"partial\nwhole\n", 100, true), "whole\n");
    }
}
