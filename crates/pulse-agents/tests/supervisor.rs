//! The supervisor against the fake agents in `tests/fixtures`. Time is tokio's paused clock: it
//! does not move while a child is alive (the supervisor waits for it on a blocking task), so
//! timeouts and grace periods happen exactly when a test advances the clock, or on their own
//! once nothing but a timer is left.

// `allow-unwrap-in-tests` covers test functions, not the helpers they share.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::time::Duration;

use pulse_agents::command::LineParser;
use pulse_agents::{
    CancellationToken, Invocation, RunEnd, SuperviseError, SuperviseOptions, SupervisorEvent,
    supervise,
};
use pulse_contracts::{OutputKind, RunOutput};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// The tests in this file run one at a time. While one test spawns a process, the forked child
/// briefly holds copies of every pipe the process has open (until it execs), so another test's
/// pipe can reach EOF a moment after its agent exited. In real time that moment is nothing; on
/// the paused clock an idle runtime jumps straight past the grace period, and the run would end
/// with a spurious "pipe still open" marker.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const OPTIONS: SuperviseOptions = SuperviseOptions {
    timeout: Duration::from_secs(60),
    grace: Duration::from_secs(5),
    max_line_bytes: 64 * 1024,
};

fn fake(args: &[&str]) -> Invocation {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    Invocation {
        program: fixtures.join("fake-agent").display().to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        cwd: std::env::temp_dir(),
        env: vec![],
        stdin: None,
    }
}

struct Run {
    events: mpsc::Receiver<SupervisorEvent>,
    cancel: CancellationToken,
    handle: JoinHandle<Result<RunEnd, SuperviseError>>,
    pid: u32,
    seen: Vec<RunOutput>,
}

async fn start(invocation: Invocation, capacity: usize) -> Run {
    reap_orphans_here();
    let (tx, mut events) = mpsc::channel(capacity);
    let cancel = CancellationToken::new();
    let handle =
        tokio::spawn(supervise(invocation, Box::new(LineParser), tx, OPTIONS, cancel.clone()));
    let pid = match events.recv().await {
        Some(SupervisorEvent::Started { pid }) => pid,
        other => panic!("expected Started first, got {other:?}"),
    };
    Run { events, cancel, handle, pid, seen: Vec::new() }
}

impl Run {
    /// Receives output until a line with this text arrives.
    async fn until(&mut self, text: &str) {
        loop {
            match self.events.recv().await {
                Some(SupervisorEvent::Output { output, .. }) => {
                    let done = output.text == text;
                    self.seen.push(output);
                    if done {
                        return;
                    }
                }
                other => panic!("expected {text:?}, got {other:?} after {:?}", self.seen),
            }
        }
    }

    /// Receives everything until the supervisor is done; returns all output and the result.
    async fn finish(mut self) -> (Vec<RunOutput>, RunEnd) {
        while let Some(event) = self.events.recv().await {
            match event {
                SupervisorEvent::Output { output, .. } => self.seen.push(output),
                started => panic!("unexpected {started:?}"),
            }
        }
        let end = self.handle.await.unwrap().unwrap();
        (self.seen, end)
    }
}

fn texts(outputs: &[RunOutput], kind: OutputKind) -> Vec<&str> {
    outputs.iter().filter(|o| o.kind == kind).map(|o| o.text.as_str()).collect()
}

/// Makes this test process the reaper of orphaned descendants (Linux), so a test can wait for
/// a killed grandchild deterministically instead of racing init.
fn reap_orphans_here() {
    #[cfg(target_os = "linux")]
    rustix::process::set_child_subreaper(Some(rustix::process::getpid())).unwrap();
}

/// Waits for every process in the group to exit, then checks that the group and `pid` are gone.
/// Only meaningful with `reap_orphans_here`: orphans are then our children.
///
/// When the supervisor works, everything left in the group already has SIGKILL pending and this
/// returns at once. The real-time ceiling only bounds a failing test, which then kills the group
/// it started (its ID is still held by the survivors) instead of leaving them behind.
#[cfg(target_os = "linux")]
fn assert_group_gone(pgid: u32, pid: u32) {
    use rustix::io::Errno;
    use rustix::process::{
        Pid, Signal, WaitId, WaitIdOptions, kill_process_group, test_kill_process,
        test_kill_process_group, waitid,
    };
    let pgid = Pid::from_raw(pgid as i32).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        loop {
            match waitid(WaitId::Pgid(Some(pgid)), WaitIdOptions::EXITED) {
                Ok(_) | Err(Errno::INTR) => {}
                Err(Errno::CHILD) => break,
                Err(e) => panic!("waitid: {e}"),
            }
        }
        let _ = tx.send(());
    });
    if rx.recv_timeout(Duration::from_secs(30)).is_err() {
        let _ = kill_process_group(pgid, Signal::KILL);
        panic!("processes in group {pgid:?} outlived the supervisor");
    }
    assert_eq!(test_kill_process_group(pgid), Err(Errno::SRCH), "group {pgid:?} still exists");
    let pid = Pid::from_raw(pid as i32).unwrap();
    assert_eq!(test_kill_process(pid), Err(Errno::SRCH), "process {pid:?} still exists");
}

#[cfg(not(target_os = "linux"))]
fn assert_group_gone(_pgid: u32, _pid: u32) {}

fn grandchild_pid(outputs: &[RunOutput]) -> u32 {
    outputs
        .iter()
        .find_map(|o| o.text.strip_prefix("grandchild "))
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("no grandchild pid in {outputs:?}"))
}

#[tokio::test(start_paused = true)]
async fn exits() {
    let _serial = SERIAL.lock().await;
    let cases = [("0", Some(0)), ("3", Some(3))];
    for (arg, code) in cases {
        let (out, end) = start(fake(&["exit", arg]), 4).await.finish().await;
        assert_eq!(end, RunEnd::Exited { code, signal: None });
        assert_eq!(texts(&out, OutputKind::Stdout), ["stdout line"]);
        assert_eq!(texts(&out, OutputKind::Stderr), ["stderr line"]);
    }
}

#[tokio::test(start_paused = true)]
async fn stdin_and_environment() {
    let _serial = SERIAL.lock().await;
    let mut inv = fake(&["stdin"]);
    inv.stdin = Some("line 1\nline 2".into());
    let (out, _) = start(inv, 4).await.finish().await;
    assert_eq!(texts(&out, OutputKind::Stdout), ["line 1", "line 2"]);

    let mut inv = fake(&["env"]);
    inv.env = vec![("PULSE_RUN_ID".into(), "r1".into()), ("PULSE_RUN_ID".into(), "r2".into())];
    let (out, _) = start(inv, 4).await.finish().await;
    assert_eq!(
        texts(&out, OutputKind::Stdout),
        ["PULSE_RUN_ID=r2"],
        "only the run's own PULSE_ variables"
    );

    // A child that never reads a large stdin must not stall the run.
    let mut inv = fake(&["exit", "0"]);
    inv.stdin = Some("x".repeat(1 << 20));
    let (_, end) = start(inv, 4).await.finish().await;
    assert_eq!(end, RunEnd::Exited { code: Some(0), signal: None });
}

#[tokio::test(start_paused = true)]
async fn spawn_failure() {
    let _serial = SERIAL.lock().await;
    let mut inv = fake(&[]);
    inv.program = "/nonexistent/pulse-agent".into();
    let (tx, _rx) = mpsc::channel(1);
    let err = supervise(inv, Box::new(LineParser), tx, OPTIONS, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, SuperviseError::Spawn { .. }), "{err}");
}

#[tokio::test(start_paused = true)]
async fn timeout_terminates_the_group() {
    let _serial = SERIAL.lock().await;
    let mut run = start(fake(&["hang"]), 4).await;
    run.until("ready").await;
    tokio::time::advance(OPTIONS.timeout).await;
    let pid = run.pid;
    let (_, end) = run.finish().await;
    assert_eq!(end, RunEnd::TimedOut);
    assert_group_gone(pid, pid);
}

#[tokio::test(start_paused = true)]
async fn cancel_terminates_the_group() {
    let _serial = SERIAL.lock().await;
    let mut run = start(fake(&["hang"]), 4).await;
    run.until("ready").await;
    run.cancel.cancel();
    let pid = run.pid;
    assert_eq!(run.finish().await.1, RunEnd::Cancelled);
    assert_group_gone(pid, pid);
}

#[tokio::test(start_paused = true)]
async fn sigterm_ignored_then_sigkill_after_grace() {
    let _serial = SERIAL.lock().await;
    let mut run = start(fake(&["ignore-term"]), 4).await;
    run.until("ready").await;
    run.cancel.cancel();
    // The agent saw SIGTERM and kept going; only the end of the grace period stops it.
    run.until("term-ignored").await;
    tokio::time::advance(OPTIONS.grace).await;
    let pid = run.pid;
    assert_eq!(run.finish().await.1, RunEnd::Cancelled);
    assert_group_gone(pid, pid);
}

#[tokio::test(start_paused = true)]
async fn grandchildren_left_behind_are_killed() {
    let _serial = SERIAL.lock().await;
    let cases: [&[&str]; 3] = [
        // Holds the output pipe and dies on SIGTERM.
        &["grandchild"],
        // Holds the output pipe and ignores SIGTERM: killed when the grace period ends.
        &["grandchild", "--ignore-term"],
        // Closed its pipes and ignores SIGTERM: killed as soon as the pipes are at EOF.
        &["grandchild", "--ignore-term", "--detached"],
    ];
    for args in cases {
        let run = start(fake(args), 4).await;
        let pgid = run.pid;
        let (out, end) = run.finish().await;
        assert_eq!(end, RunEnd::Exited { code: Some(0), signal: None }, "{args:?}");
        assert_group_gone(pgid, grandchild_pid(&out));
    }
}

#[tokio::test(start_paused = true)]
async fn dropping_the_future_kills_the_group() {
    let _serial = SERIAL.lock().await;
    let mut run = start(fake(&["grandchild", "--ignore-term", "--hang"]), 4).await;
    let event = run.events.recv().await;
    let Some(SupervisorEvent::Output { output, .. }) = event else { panic!("{event:?}") };
    let grandchild = grandchild_pid(&[output]);
    run.handle.abort();
    assert!(run.handle.await.unwrap_err().is_cancelled());
    assert_group_gone(run.pid, grandchild);
}

#[tokio::test(start_paused = true)]
async fn a_slow_consumer_slows_the_agent_and_loses_nothing() {
    let _serial = SERIAL.lock().await;
    const LINES: usize = 40_000; // 4 MB: far more than a pipe holds
    let done = std::env::temp_dir().join(format!("pulse-flood-{}", std::process::id()));
    let _ = std::fs::remove_file(&done);
    let mut run = start(fake(&["flood", &LINES.to_string(), &done.display().to_string()]), 1).await;
    run.until(&format!("line {:08} {:086}", 1, 0)).await;
    // Everything the agent wrote must still be in the pipe, our one-line buffer or the
    // one-slot channel: it cannot have finished.
    assert!(!done.exists(), "the agent finished writing while nobody was reading");
    let (out, end) = run.finish().await;
    assert_eq!(end, RunEnd::Exited { code: Some(0), signal: None });
    let lines = texts(&out, OutputKind::Stdout);
    assert_eq!(lines.len(), LINES);
    assert!(lines.iter().enumerate().all(|(i, l)| l.starts_with(&format!("line {:08} ", i + 1))));
    assert!(done.exists());
    let _ = std::fs::remove_file(&done);
}

#[tokio::test(start_paused = true)]
async fn long_lines_are_cut() {
    let _serial = SERIAL.lock().await;
    let (out, _) = start(fake(&["long-line", "3000000"]), 4).await.finish().await;
    assert_eq!(
        out.len(),
        3,
        "{:?}",
        out.iter().map(|o| (o.kind, o.text.len())).collect::<Vec<_>>()
    );
    assert_eq!(out[0].kind, OutputKind::Stdout);
    assert_eq!(out[0].text.len(), OPTIONS.max_line_bytes);
    assert_eq!(out[1].kind, OutputKind::Marker);
    let data = out[1].data.as_ref().unwrap();
    assert_eq!(data["dropped_bytes"], 3_000_000 - OPTIONS.max_line_bytes);
    assert_eq!(data["stream"], "stdout");
    assert_eq!(out[2].text, "after");
}

#[tokio::test(start_paused = true)]
async fn invalid_utf8_and_missing_newline() {
    let _serial = SERIAL.lock().await;
    let (out, _) = start(fake(&["bad-utf8"]), 4).await.finish().await;
    assert_eq!(
        texts(&out, OutputKind::Stdout),
        ["caf\u{FFFD}", "\u{FFFD}\u{FFFD} binary", "no newline at end"]
    );
}

/// The documented limit: a process that leaves the group is not killed. The run still ends
/// (after the grace period without output), says why, and keeps the output it had read.
#[cfg(target_os = "linux")]
#[tokio::test(start_paused = true)]
async fn an_escaped_grandchild_survives_but_the_run_ends() {
    use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process, waitid};
    let _serial = SERIAL.lock().await;
    let (out, end) = start(fake(&["escape"]), 4).await.finish().await;
    let escaped = out
        .iter()
        .find_map(|o| o.text.strip_prefix("escaped "))
        .and_then(|p| Pid::from_raw(p.parse().unwrap()))
        .unwrap();
    // Alive and, re-parented to this test process, our child (so the PID is certainly it):
    // clean it up before asserting anything else.
    let alive = matches!(
        waitid(WaitId::Pid(escaped), WaitIdOptions::EXITED | WaitIdOptions::NOHANG),
        Ok(None)
    );
    if alive {
        kill_process(escaped, Signal::KILL).unwrap();
        waitid(WaitId::Pid(escaped), WaitIdOptions::EXITED).unwrap();
    }
    assert!(alive, "the escapee should have survived");
    assert_eq!(end, RunEnd::Exited { code: Some(0), signal: None });
    assert_eq!(texts(&out, OutputKind::Stdout)[1..], ["partial"]);
    assert_eq!(out.last().unwrap().data.as_ref().unwrap()["reason"], "pipe_held_open");
}
