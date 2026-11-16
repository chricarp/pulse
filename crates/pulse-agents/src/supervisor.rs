//! The process supervisor: the only place agents' processes are spawned and signalled.
//!
//! ## Lifecycle
//!
//! 1. Spawn the invocation directly (no shell) as the leader of a **new process group**, with
//!    stdin from `Invocation::stdin` (or null) and stdout and stderr piped. Reserved `PULSE_*`
//!    variables inherited from the daemon are removed first, so a run never sees another run's
//!    values (`PULSE_EVENT_SUBJECT` is absent for an event without a subject, not stale).
//! 2. Send [`SupervisorEvent::Started`], then stream both pipes line by line through the
//!    adapter's parser. Order is kept within a stream; the two streams are read concurrently and
//!    interleave in the order lines complete. Each output carries the time its line was read.
//! 3. The run ends when the leader exits, the timeout elapses, or `cancel` fires (the daemon
//!    cancels every run on shutdown). In all three cases the group gets SIGTERM, then SIGKILL
//!    after `grace` unless every process holding the pipes has exited by then. After a natural
//!    exit this sweeps up whatever the agent left running in its group.
//! 4. The leader is reaped, the pipes are drained to EOF, the parser is flushed, and the result
//!    is returned.
//!
//! ## Only our group, only while we own it
//!
//! The leader is detected as exited with `waitid(WNOWAIT)`, which leaves it a zombie. A zombie
//! keeps its PID, so the group ID (equal to the leader's PID) cannot be reused by another
//! process while the zombie exists. Every signal is sent before the leader is reaped, and the
//! reap happens strictly after the last signal. That is what makes `killpg` safe: we never
//! signal a group ID that could belong to someone else. If something else in the daemon reaps
//! our child (a `waitpid(-1)` elsewhere in the process; tokio and std never do this), the
//! supervisor notices and stops signalling.
//!
//! ## What a process group catches
//!
//! Everything the agent starts stays in its group unless it moves itself out: a descendant that
//! calls `setsid()` or `setpgid()` (daemons, `nohup`-style double forks with `setsid`, some
//! terminal multiplexers) escapes, and Pulse will not kill it. Such a process may also keep the
//! output pipes open: draining then stops after `grace` without new output (or after
//! [`DRAIN_MAX_BYTES`]) and a `marker` output says so. See
//! `docs/internals/0006-agent-adapters.md` for what would close that gap on Linux and macOS.
//!
//! When `supervise` returns, every process in the group has been sent SIGKILL and the leader
//! has been reaped. If the future is dropped mid-run, the group is sent SIGKILL at once (no
//! grace) and the leader is reaped on a short-lived thread.
//!
//! ## Backpressure
//!
//! `events` is bounded. While it is full the supervisor stops reading the pipes, the pipes
//! fill, and the agent blocks on its next write: a slow consumer slows the agent and nothing is
//! dropped or buffered without bound. Timeout and cancellation keep working while blocked (a
//! stalled consumer can therefore make a run time out). Memory per run is bounded by two line
//! buffers of `max_line_bytes` and the outputs of one line. If the receiver is dropped, output
//! is discarded and the process is still supervised to the end.

use std::collections::VecDeque;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::pin::pin;
use std::process::{Child, ExitStatus, Stdio};
use std::time::{Duration, SystemTime};

use pulse_contracts::{OutputKind, RunOutput, Timestamp};
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::pipe;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep};
use tokio_util::sync::CancellationToken;

use crate::{Invocation, OutputParser, Stream};

/// After the group is gone, stop draining a pipe that keeps producing more than this: whoever
/// writes it has left the group.
pub const DRAIN_MAX_BYTES: u64 = 1 << 20;

const READ_CHUNK: usize = 8 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct SuperviseOptions {
    pub timeout: Duration,
    /// Between SIGTERM and SIGKILL, and how long to wait for pipes held open by processes that
    /// left the group.
    pub grace: Duration,
    /// Longer lines are cut: the parser sees the first `max_line_bytes`, then a `marker` output
    /// says how much was dropped.
    pub max_line_bytes: usize,
}

impl Default for SuperviseOptions {
    fn default() -> Self {
        SuperviseOptions {
            timeout: Duration::from_secs(15 * 60),
            grace: Duration::from_secs(5),
            // Claude Code puts a whole tool result (a file it read) on one stream-json line.
            max_line_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SupervisorEvent {
    /// Sent before any output. `pid` is also the process group ID.
    Started { pid: u32 },
    /// `at` is when the line it came from was read (for Pulse's markers, when it was made).
    Output { at: Timestamp, output: RunOutput },
}

/// How a supervised process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunEnd {
    /// The leader exited on its own: `code` if it exited, `signal` if a signal killed it.
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    TimedOut,
    Cancelled,
}

#[derive(Debug, thiserror::Error)]
pub enum SuperviseError {
    /// The process could not be started; the run becomes `failed` without having run.
    #[error("could not start {program:?}: {source}")]
    Spawn { program: String, source: std::io::Error },
    /// Supervision failed after the process started. The group has been sent SIGKILL.
    #[error("supervising the process failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Why the run is ending, before the exit status is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    Exited,
    TimedOut,
    Cancelled,
}

pub async fn supervise(
    invocation: Invocation,
    mut parser: Box<dyn OutputParser>,
    events: mpsc::Sender<SupervisorEvent>,
    options: SuperviseOptions,
    cancel: CancellationToken,
) -> Result<RunEnd, SuperviseError> {
    let mut command = std::process::Command::new(&invocation.program);
    command
        .args(&invocation.args)
        .current_dir(&invocation.cwd)
        .stdin(if invocation.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    for (key, _) in std::env::vars_os() {
        if key.as_encoded_bytes().starts_with(b"PULSE_") {
            command.env_remove(key);
        }
    }
    command.envs(invocation.env.iter().map(|(k, v)| (k, v)));
    let mut child = command
        .spawn()
        .map_err(|source| SuperviseError::Spawn { program: invocation.program.clone(), source })?;
    let pid = child.id();
    let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    // From here on, an early return drops `group`, which kills the group and reaps the leader.
    let (mut group, mut exited) = Group::watch(child);

    let missing = || io::Error::other("child pipe missing although it was requested");
    let mut out = Lines::new(Stream::Stdout, receiver(stdout.ok_or_else(missing)?)?, &options);
    let mut err = Lines::new(Stream::Stderr, receiver(stderr.ok_or_else(missing)?)?, &options);
    let stdin = match stdin {
        Some(pipe) => Some(pipe::Sender::from_owned_fd(OwnedFd::from(pipe))?),
        None => None,
    };
    let mut stdin_task = pin!(write_stdin(stdin, invocation.stdin));
    let mut stdin_done = false;

    let mut sink = Sink { events, open: true, pending: VecDeque::new() };
    sink.pending.push_back(SupervisorEvent::Started { pid });
    let mut parsed = Vec::new();

    let deadline = sleep(options.timeout);
    let mut deadline = pin!(deadline);
    let kill_timer = sleep(options.timeout);
    let mut kill_timer = pin!(kill_timer);
    let drain_timer = sleep(options.grace);
    let mut drain_timer = pin!(drain_timer);

    let mut ending: Option<Ending> = None;
    let mut leader_exited = false;
    let mut killed = false;
    let mut status: Option<ExitStatus> = None;
    let mut drained_bytes = 0u64;
    let mut gave_up = false;

    loop {
        let eof = out.eof && err.eof;
        if status.is_none() && leader_exited && ending.is_some() && (killed || eof) {
            if !killed {
                // Anything left in the group closed its pipes; it had SIGTERM already.
                group.signal(Signal::KILL);
                killed = true;
            }
            status = Some(group.reap()?);
            drain_timer.as_mut().reset(Instant::now() + options.grace);
        }
        if status.is_some() && (eof || gave_up) && sink.pending.is_empty() {
            break;
        }
        let reading = sink.pending.is_empty() && !gave_up;

        tokio::select! {
            biased;
            lost = &mut exited, if !leader_exited => {
                leader_exited = true;
                if lost.unwrap_or(true) {
                    group.lost();
                }
                if ending.is_none() {
                    ending = Some(Ending::Exited);
                    group.signal(Signal::TERM);
                    kill_timer.as_mut().reset(Instant::now() + options.grace);
                }
            }
            () = cancel.cancelled(), if ending.is_none() => {
                ending = Some(Ending::Cancelled);
                group.signal(Signal::TERM);
                kill_timer.as_mut().reset(Instant::now() + options.grace);
            }
            () = &mut deadline, if ending.is_none() => {
                ending = Some(Ending::TimedOut);
                group.signal(Signal::TERM);
                kill_timer.as_mut().reset(Instant::now() + options.grace);
            }
            () = &mut kill_timer, if ending.is_some() && !killed => {
                group.signal(Signal::KILL);
                killed = true;
            }
            () = &mut stdin_task, if !stdin_done => stdin_done = true,
            sent = sink.send_one(), if !sink.pending.is_empty() => {
                if sent && status.is_some() {
                    drain_timer.as_mut().reset(Instant::now() + options.grace);
                }
            }
            chunk = out.next(), if reading && !out.eof => {
                let n = handle(chunk?, &mut out, &mut *parser, &mut parsed, &mut sink);
                if status.is_some() {
                    drained_bytes += n;
                    drain_timer.as_mut().reset(Instant::now() + options.grace);
                }
            }
            chunk = err.next(), if reading && !err.eof => {
                let n = handle(chunk?, &mut err, &mut *parser, &mut parsed, &mut sink);
                if status.is_some() {
                    drained_bytes += n;
                    drain_timer.as_mut().reset(Instant::now() + options.grace);
                }
            }
            () = &mut drain_timer, if status.is_some() && reading => gave_up = true,
            // Unreachable by the conditions above; ending here reports an error, never spins.
            else => break,
        }
        if drained_bytes > DRAIN_MAX_BYTES {
            gave_up = true;
        }
        if gave_up && !(out.eof && err.eof) && status.is_some() && sink.pending.is_empty() {
            // Stop reading, but deliver what was already read (a last line without newline).
            for lines in [&mut out, &mut err] {
                for chunk in lines.close() {
                    handle(Some(chunk), lines, &mut *parser, &mut parsed, &mut sink);
                }
            }
            sink.push(marker(
                "output pipe still open after the run's process group ended; a process that left the group holds it",
                json!({ "reason": "pipe_held_open" }),
            ));
        }
    }

    parser.finish(&mut parsed);
    let at = now();
    for output in parsed.drain(..) {
        sink.push(SupervisorEvent::Output { at, output });
    }
    while !sink.pending.is_empty() {
        sink.send_one().await;
    }

    // `status` is set before the loop can exit, and `ending` before `status`.
    let status = status.ok_or_else(|| io::Error::other("supervisor ended without reaping"))?;
    Ok(match ending {
        Some(Ending::TimedOut) => RunEnd::TimedOut,
        Some(Ending::Cancelled) => RunEnd::Cancelled,
        Some(Ending::Exited) | None => {
            RunEnd::Exited { code: status.code(), signal: status.signal() }
        }
    })
}

fn receiver(pipe: impl Into<OwnedFd>) -> io::Result<pipe::Receiver> {
    pipe::Receiver::from_owned_fd(pipe.into())
}

fn now() -> Timestamp {
    Timestamp::from_system_time(SystemTime::now())
}

fn marker(text: &str, data: serde_json::Value) -> SupervisorEvent {
    SupervisorEvent::Output {
        at: now(),
        output: RunOutput { kind: OutputKind::Marker, text: text.to_owned(), data: Some(data) },
    }
}

/// Feeds one chunk to the parser; returns the bytes it consumed.
fn handle(
    chunk: Option<Chunk>,
    lines: &mut Lines,
    parser: &mut dyn OutputParser,
    parsed: &mut Vec<RunOutput>,
    sink: &mut Sink,
) -> u64 {
    let Some(chunk) = chunk else { return 0 };
    let at = now();
    match chunk {
        Chunk::Line(bytes) => {
            parser.line(lines.stream, &String::from_utf8_lossy(&bytes), parsed);
            for output in parsed.drain(..) {
                sink.push(SupervisorEvent::Output { at, output });
            }
            bytes.len() as u64
        }
        Chunk::Dropped { kept, dropped } => {
            sink.push(marker(
                &format!(
                    "{} line too long: kept the first {kept} bytes, dropped {dropped}",
                    lines.stream.as_str()
                ),
                json!({ "reason": "line_too_long", "stream": lines.stream.as_str(), "kept_bytes": kept, "dropped_bytes": dropped }),
            ));
            dropped
        }
    }
}

async fn write_stdin(pipe: Option<pipe::Sender>, data: Option<String>) {
    if let (Some(mut pipe), Some(data)) = (pipe, data) {
        // A child that exits without reading its input is not an error of ours (EPIPE).
        let _ = pipe.write_all(data.as_bytes()).await;
    }
}

/// The bounded output queue: holds the outputs of at most one line while waiting for the
/// receiver.
struct Sink {
    events: mpsc::Sender<SupervisorEvent>,
    open: bool,
    pending: VecDeque<SupervisorEvent>,
}

impl Sink {
    fn push(&mut self, event: SupervisorEvent) {
        if self.open {
            self.pending.push_back(event);
        }
    }

    /// Sends the oldest pending event. Cancel safe: `reserve` takes no item.
    async fn send_one(&mut self) -> bool {
        match self.events.reserve().await {
            Ok(permit) => match self.pending.pop_front() {
                Some(event) => {
                    permit.send(event);
                    true
                }
                None => false,
            },
            Err(_) => {
                self.open = false;
                self.pending.clear();
                false
            }
        }
    }
}

/// The leader of the run's process group.
///
/// Invariant: `child` is `Some` until the leader is reaped. Signals are only sent while it is
/// `Some`, and it is only reaped after the leader has exited and the last signal was sent, so
/// every signal reaches our group and never a reused ID.
struct Group {
    pgid: Pid,
    child: Option<Child>,
    /// The leader was reaped behind our back, so its ID may be reused: never signal.
    lost: bool,
}

impl Group {
    /// Starts watching the leader. The receiver resolves once it has exited (it is then a
    /// zombie, still holding the group ID); `Ok(true)` means it could not be watched and the
    /// group must not be signalled.
    fn watch(child: Child) -> (Group, oneshot::Receiver<bool>) {
        let pgid = Pid::from_child(&child);
        let (exited_tx, exited) = oneshot::channel();
        // A blocking task that ends when the leader exits. Under tokio's paused test clock,
        // time does not auto-advance while it runs, so tests drive timeouts deterministically
        // against a live child, and grace periods elapse on their own once it has exited.
        tokio::task::spawn_blocking(move || {
            let lost = loop {
                match waitid(WaitId::Pid(pgid), WaitIdOptions::EXITED | WaitIdOptions::NOWAIT) {
                    Ok(_) => break false,
                    Err(rustix::io::Errno::INTR) => {}
                    Err(_) => break true,
                }
            };
            let _ = exited_tx.send(lost);
        });
        (Group { pgid, child: Some(child), lost: false }, exited)
    }

    fn signal(&self, signal: Signal) {
        if self.child.is_some() && !self.lost {
            // ESRCH is impossible while the zombie leader holds the group; other errors leave
            // nothing to do but go on to the next step.
            let _ = kill_process_group(self.pgid, signal);
        }
    }

    fn lost(&mut self) {
        self.lost = true;
    }

    /// Reaps the leader. Only called once it has exited, so this does not block.
    fn reap(&mut self) -> io::Result<ExitStatus> {
        match self.child.take() {
            Some(mut child) => child.wait(),
            None => Err(io::Error::other("the leader was already reaped")),
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if !self.lost {
                let _ = kill_process_group(self.pgid, Signal::KILL);
            }
            // The leader dies as soon as SIGKILL is delivered; reap it off the async thread.
            // A plain thread, because a dropped future may outlive its runtime.
            std::thread::spawn(move || child.wait());
        }
    }
}

enum Chunk {
    /// A complete line without its terminator (`\n` or `\r\n`), the unterminated last line, or
    /// the first `max_line_bytes` of a longer line.
    Line(Vec<u8>),
    /// The rest of an over-long line ended; `dropped` bytes of it were discarded.
    Dropped { kept: usize, dropped: u64 },
}

/// A cancel-safe line splitter over a pipe with a bounded buffer.
struct Lines {
    stream: Stream,
    pipe: pipe::Receiver,
    buf: Vec<u8>,
    /// `buf[..scanned]` has no newline.
    scanned: usize,
    scratch: Box<[u8]>,
    max: usize,
    /// Inside an over-long line: bytes dropped so far.
    dropping: Option<u64>,
    /// An over-long line ended with this many bytes dropped; reported by the next `take`.
    ended_drop: Option<u64>,
    eof: bool,
}

impl Lines {
    fn new(stream: Stream, pipe: pipe::Receiver, options: &SuperviseOptions) -> Self {
        Lines {
            stream,
            pipe,
            buf: Vec::new(),
            scanned: 0,
            scratch: vec![0; READ_CHUNK].into_boxed_slice(),
            max: options.max_line_bytes.max(1),
            dropping: None,
            ended_drop: None,
            eof: false,
        }
    }

    /// The next chunk, or `None` at end of stream. Cancel safe: state changes only after a read
    /// has completed, and `read` itself is cancel safe.
    async fn next(&mut self) -> io::Result<Option<Chunk>> {
        loop {
            if let Some(chunk) = self.take() {
                return Ok(Some(chunk));
            }
            if self.eof {
                return Ok(None);
            }
            let n = self.pipe.read(&mut self.scratch).await?;
            if n == 0 {
                self.eof = true;
            } else {
                self.buf.extend_from_slice(&self.scratch[..n]);
            }
        }
    }

    /// Stops reading: marks the stream ended and returns what is still buffered.
    fn close(&mut self) -> Vec<Chunk> {
        let mut rest = Vec::new();
        if !self.eof {
            self.eof = true;
            while let Some(chunk) = self.take() {
                rest.push(chunk);
            }
        }
        rest
    }

    fn take(&mut self) -> Option<Chunk> {
        if let Some(dropped) = self.ended_drop.take() {
            return Some(Chunk::Dropped { kept: self.max, dropped });
        }
        if let Some(i) = self.buf[self.scanned..].iter().position(|b| *b == b'\n') {
            let end = self.scanned + i;
            self.scanned = 0;
            if let Some(dropped) = self.dropping.take() {
                self.buf.drain(..=end);
                self.buf.shrink_to(READ_CHUNK * 2);
                return Some(Chunk::Dropped { kept: self.max, dropped: dropped + end as u64 });
            }
            let mut line: Vec<u8> = self.buf.drain(..=end).collect();
            line.pop();
            if line.len() > self.max {
                // One read completed the line past the limit.
                self.ended_drop = Some((line.len() - self.max) as u64);
                line.truncate(self.max);
            } else if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Some(Chunk::Line(line));
        }
        if let Some(dropped) = &mut self.dropping {
            *dropped += self.buf.len() as u64;
            self.buf.clear();
            self.scanned = 0;
            if self.eof {
                let dropped = *dropped;
                self.dropping = None;
                return Some(Chunk::Dropped { kept: self.max, dropped });
            }
            return None;
        }
        if self.buf.len() > self.max {
            let rest = self.buf.split_off(self.max);
            self.dropping = Some(rest.len() as u64);
            self.scanned = 0;
            return Some(Chunk::Line(std::mem::take(&mut self.buf)));
        }
        self.scanned = self.buf.len();
        if self.eof && !self.buf.is_empty() {
            self.scanned = 0;
            return Some(Chunk::Line(std::mem::take(&mut self.buf)));
        }
        None
    }
}
