//! The end-to-end harness: the real `pulse` binary as the daemon, in a temp `PULSE_HOME` with a
//! temp `HOME` (so `~/University` in a shipped template is a temp directory), real sources, the
//! fake agents from `pulse-agents/tests/fixtures`, and an RPC client.
//!
//! Nothing here sleeps. [`Watch::until`] waits for a predicate over what the daemon has pushed
//! (events, run states, run output, state changes), re-checking after every notification, with
//! [`CEILING`] as the failure-only bound. Every process the harness starts is killed by the PID
//! captured at spawn when its handle drops.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use pulse_contracts::rpc::{
    Empty, EventFilter, EventQuery, EventsSubscribeParams, Notification, Order,
    RunOutputSubscribeParams, RunsSubscribeParams, method,
};
use pulse_contracts::run::RunOutputChunk;
use pulse_contracts::status::SourceState;
use pulse_contracts::{Delivery, Event, EventId, Run, RunId, RunStatus, RunSummary};
use pulse_daemon::client::Client;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

/// Reaching this means the test failed; nothing waits for it on success.
pub const CEILING: Duration = Duration::from_secs(60);

pub fn pulse_bin() -> &'static str {
    env!("CARGO_BIN_EXE_pulse")
}

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../pulse-agents/tests/fixtures")
}

pub fn fixture(name: &str) -> String {
    fixtures().join(name).canonicalize().unwrap().display().to_string()
}

pub fn template(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../templates/{name}.toml")),
    )
    .unwrap()
}

/// Replaces `from` with `to` in `text`, failing if `from` is not there (so a template edit
/// that would silently stop applying breaks the test instead).
pub fn replace(text: &str, from: &str, to: &str) -> String {
    assert!(text.contains(from), "expected {from:?} in:\n{text}");
    text.replacen(from, to, 1)
}

pub async fn within<T>(what: &str, f: impl std::future::Future<Output = T>) -> T {
    match tokio::time::timeout(CEILING, f).await {
        Ok(v) => v,
        Err(_) => panic!("timed out waiting for {what}"),
    }
}

/// A temp Pulse home and user home. Directories under `user` are what `~/…` means.
pub struct Env {
    pub dir: tempfile::TempDir,
    pub home: PathBuf,
    pub user: PathBuf,
}

impl Env {
    pub fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        // Canonical, so paths in events (built from the configured root) and the paths the
        // test computes agree even when the temp dir is behind a symlink.
        let root = dir.path().canonicalize().unwrap();
        let home = root.join("pulse-home");
        let user = root.join("user");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&user).unwrap();
        Env { dir, home, user }
    }

    pub fn user_path(&self, rel: &str) -> PathBuf {
        self.user.join(rel)
    }

    pub fn mkdir(&self, rel: &str) -> PathBuf {
        let p = self.user.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    pub fn write_config(&self, text: &str) {
        std::fs::write(self.home.join("pulse.toml"), text).unwrap();
    }

    pub fn read_config(&self) -> String {
        std::fs::read_to_string(self.home.join("pulse.toml")).unwrap()
    }

    /// The CLI, pointed at this home, with the inherited `PULSE_*` variables removed (the
    /// suite may itself run inside a Pulse run).
    pub fn pulse(&self) -> Command {
        let mut cmd = Command::new(pulse_bin());
        scrub(&mut cmd);
        cmd.env("PULSE_HOME", &self.home).env("HOME", &self.user).kill_on_drop(true);
        cmd
    }

    /// Runs the CLI to completion: (exit code, stdout, stderr).
    pub async fn run(&self, args: &[&str]) -> (i32, String, String) {
        self.run_in(args, &[], None).await
    }

    pub async fn run_in(
        &self,
        args: &[&str],
        env: &[(&str, &str)],
        cwd: Option<&Path>,
    ) -> (i32, String, String) {
        let mut cmd = self.pulse();
        cmd.args(args).stdin(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        let out = within(&format!("pulse {args:?}"), cmd.output()).await.unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// Runs the CLI with `input` on stdin: (exit code, stdout, stderr).
    pub async fn run_stdin(&self, args: &[&str], input: &str) -> (i32, String, String) {
        use tokio::io::AsyncWriteExt;
        let mut cmd = self.pulse();
        cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(input.as_bytes()).await.unwrap();
        drop(stdin);
        let out = within(&format!("pulse {args:?}"), child.wait_with_output()).await.unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// `pulse start --ready-line --port 0`, returned once it is listening.
    pub async fn start(&self) -> Daemon {
        let mut cmd = self.pulse();
        cmd.args(["start", "--ready-line", "--port", "0"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        let pid = child.id().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut lines = BufReader::new(stdout).lines();
        let line = within("the daemon to be ready", lines.next_line()).await.unwrap();
        let Some(port) = line.as_deref().and_then(|l| l.strip_prefix("ready ")) else {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                use tokio::io::AsyncReadExt;
                let _ = e.read_to_string(&mut err).await;
            }
            panic!("the daemon did not start: {line:?}\n{err}");
        };
        let port = port.parse().unwrap();
        // Keep reading stdout so the daemon never blocks on a full pipe.
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        Daemon { child: Some(child), pid, port, home: self.home.clone() }
    }
}

pub fn scrub(cmd: &mut Command) {
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("PULSE_") {
            cmd.env_remove(&k);
        }
    }
}

/// A daemon process this test started.
pub struct Daemon {
    child: Option<Child>,
    pub pid: u32,
    pub port: u16,
    home: PathBuf,
}

impl Daemon {
    pub async fn client(&self) -> Client {
        Client::connect(&self.home).await.expect("client connects")
    }

    pub async fn watch(&self) -> Watch {
        Watch::new(self.client().await).await
    }

    /// `system.shutdown`, then waits for the process to exit. Returns its exit status code.
    pub async fn stop(mut self) -> Option<i32> {
        let client = self.client().await;
        client.call::<method::SystemShutdown>(Empty {}).await.unwrap();
        let mut child = self.child.take().unwrap();
        within("the daemon to exit", child.wait()).await.unwrap().code()
    }

    /// SIGTERM, then waits for the process to exit.
    pub async fn terminate(mut self) -> Option<i32> {
        let mut child = self.child.take().unwrap();
        signal(self.pid, rustix::process::Signal::TERM);
        within("the daemon to exit", child.wait()).await.unwrap().code()
    }

    /// SIGKILL: a crash. Its agents (in their own process groups) are not killed.
    pub async fn crash(mut self) {
        let mut child = self.child.take().unwrap();
        child.start_kill().unwrap();
        within("the daemon to die", child.wait()).await.unwrap();
    }

    /// CPU time (user + system, clock ticks) and resident set (KiB) from /proc.
    pub fn proc_stats(&self) -> ProcStats {
        proc_stats(self.pid)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            // The PID captured at spawn; the child is unreaped, so it cannot have been reused.
            let _ = child.start_kill();
        }
    }
}

pub fn signal(pid: u32, sig: rustix::process::Signal) {
    let pid = rustix::process::Pid::from_raw(pid as i32).unwrap();
    rustix::process::kill_process(pid, sig).unwrap();
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcStats {
    pub cpu_ticks: u64,
    pub rss_kib: u64,
    pub fds: usize,
}

pub fn proc_stats(pid: u32) -> ProcStats {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    // Fields after the parenthesized command name; utime and stime are fields 14 and 15.
    let rest = stat.rsplit_once(')').map_or("", |(_, r)| r);
    let f: Vec<&str> = rest.split_whitespace().collect();
    let num = |i: usize| f.get(i).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let rss = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
        .unwrap_or(0);
    let fds = std::fs::read_dir(format!("/proc/{pid}/fd")).map_or(0, |d| d.count());
    ProcStats { cpu_ticks: num(11) + num(12), rss_kib: rss, fds }
}

/// What a connection has been told, accumulated. Wait for anything with [`Watch::until`].
pub struct Watch {
    pub client: Client,
    rx: mpsc::Receiver<Notification>,
    /// Live events in arrival order (live batches may summarize; see `dropped`).
    pub events: Vec<Event>,
    pub dropped: u64,
    /// Number of `events.batch` notifications and the largest batch.
    pub batches: usize,
    pub max_batch: usize,
    /// Latest state per run, and every state seen per run in order.
    pub runs: HashMap<RunId, RunSummary>,
    pub run_states: HashMap<RunId, Vec<RunStatus>>,
    /// Output per run, and whether the subscription said it is done.
    pub output: HashMap<RunId, Vec<RunOutputChunk>>,
    pub output_done: HashMap<RunId, bool>,
    /// For each output chunk: the run's latest known state when the chunk arrived.
    pub output_seen_while: HashMap<RunId, Vec<RunStatus>>,
    pub state_changes: usize,
}

impl Watch {
    /// Subscribes to every event and every run.
    pub async fn new(client: Client) -> Watch {
        let rx = client.notifications().unwrap();
        client
            .call::<method::EventsSubscribe>(EventsSubscribeParams {
                filter: EventFilter::default(),
            })
            .await
            .unwrap();
        client.call::<method::RunsSubscribe>(RunsSubscribeParams::default()).await.unwrap();
        Watch {
            client,
            rx,
            events: Vec::new(),
            dropped: 0,
            batches: 0,
            max_batch: 0,
            runs: HashMap::new(),
            run_states: HashMap::new(),
            output: HashMap::new(),
            output_done: HashMap::new(),
            output_seen_while: HashMap::new(),
            state_changes: 0,
        }
    }

    pub async fn follow_output(&mut self, run: RunId) {
        self.client
            .call::<method::RunsSubscribeOutput>(RunOutputSubscribeParams { id: run, after: None })
            .await
            .unwrap();
    }

    fn apply(&mut self, n: Notification) {
        match n {
            Notification::EventsBatch(b) => {
                self.batches += 1;
                self.max_batch = self.max_batch.max(b.events.len());
                self.dropped += b.dropped;
                self.events.extend(b.events);
            }
            Notification::RunsChanged(c) => {
                for r in c.runs {
                    let states = self.run_states.entry(r.id).or_default();
                    if states.last() != Some(&r.status) {
                        states.push(r.status);
                    }
                    self.runs.insert(r.id, r);
                }
            }
            Notification::RunsOutput(o) => {
                let state = self.runs.get(&o.run).map(|r| r.status);
                let seen = self.output_seen_while.entry(o.run).or_default();
                for _ in &o.chunks {
                    seen.push(state.unwrap_or(RunStatus::Queued));
                }
                self.output.entry(o.run).or_default().extend(o.chunks);
                if o.done {
                    self.output_done.insert(o.run, true);
                }
            }
            Notification::StateChanged(_) => self.state_changes += 1,
        }
    }

    /// Waits until `f` returns `Some`, checking after every notification.
    pub async fn until<T>(&mut self, what: &str, mut f: impl FnMut(&Watch) -> Option<T>) -> T {
        let wait = async {
            loop {
                if let Some(v) = f(self) {
                    return v;
                }
                let n = self.rx.recv().await.expect("the daemon closed the connection");
                self.apply(n);
            }
        };
        match tokio::time::timeout(CEILING, wait).await {
            Ok(v) => v,
            Err(_) => panic!(
                "timed out waiting for {what}\nevents: {:#?}\nruns: {:#?}",
                self.events.iter().map(|e| (&e.type_, &e.subject)).collect::<Vec<_>>(),
                self.runs.values().collect::<Vec<_>>()
            ),
        }
    }

    /// Drains notifications that already arrived, without waiting.
    pub fn drain(&mut self) {
        while let Ok(n) = self.rx.try_recv() {
            self.apply(n);
        }
    }

    pub async fn event(&mut self, what: &str, f: impl Fn(&Event) -> bool) -> Event {
        self.until(what, |w| w.events.iter().find(|e| f(e)).cloned()).await
    }

    pub async fn run_in(&mut self, what: &str, f: impl Fn(&RunSummary) -> bool) -> RunSummary {
        self.until(what, |w| w.runs.values().find(|r| f(r)).cloned()).await
    }

    /// Waits for run `id` to be in `status`. The subscription predates this call, so reading
    /// the run first misses nothing: any later change is notified.
    pub async fn run_status(&mut self, id: RunId, status: RunStatus) -> RunSummary {
        let now = self.get_run(id).await.summary();
        if now.status == status {
            return now;
        }
        self.run_in(&format!("run {id} to be {status:?}"), |r| r.id == id && r.status == status)
            .await
    }

    pub async fn run_terminal(&mut self, id: RunId) -> RunSummary {
        let now = self.get_run(id).await.summary();
        if now.status.is_terminal() {
            return now;
        }
        self.run_in(&format!("run {id} to end"), |r| r.id == id && r.status.is_terminal()).await
    }

    /// Waits until run `id` has printed a line containing `text`.
    pub async fn output_line(&mut self, id: RunId, text: &str) {
        if !self.output.contains_key(&id) {
            self.follow_output(id).await;
            self.output.insert(id, Vec::new());
        }
        self.until(&format!("run {id} to print {text:?}"), |w| {
            w.output[&id].iter().any(|c| c.output.text.contains(text)).then_some(())
        })
        .await;
    }

    /// Waits until every configured source reports `state`.
    pub async fn sources(&mut self, state: SourceState) {
        self.source_state(None, state).await;
    }

    /// Waits until source `name` (every source, with `None`) reports `state`.
    pub async fn source_state(&mut self, name: Option<&str>, state: SourceState) {
        loop {
            let list = self.client.call::<method::SourcesList>(Empty {}).await.unwrap();
            let mut wanted =
                list.sources.iter().filter(|s| name.is_none_or(|n| s.config.name == n));
            let mut any = false;
            if wanted.all(|s| {
                any = true;
                s.health.state == state
            }) && any
            {
                return;
            }
            let seen = self.state_changes;
            self.until(&format!("sources to be {state:?}: {list:#?}"), |w| {
                (w.state_changes > seen).then_some(())
            })
            .await;
        }
    }

    /// Waits for an event with a delivery matching `f`. Deliveries are committed with their
    /// event, and later ones (an elapsed debounce) come with a run change, so the log is
    /// re-checked after every notification. For tests with few events.
    pub async fn delivery(
        &mut self,
        what: &str,
        f: impl Fn(&Event, &Delivery) -> bool,
    ) -> (Event, Delivery) {
        let wait = async {
            loop {
                for e in self.events.clone() {
                    let detail = self.detail(e.id).await;
                    if let Some(d) = detail.deliveries.into_iter().find(|d| f(&detail.event, d)) {
                        return (detail.event, d);
                    }
                }
                let n = self.rx.recv().await.expect("the daemon closed the connection");
                self.apply(n);
            }
        };
        match tokio::time::timeout(CEILING, wait).await {
            Ok(v) => v,
            Err(_) => panic!("timed out waiting for {what}"),
        }
    }

    pub async fn get_run(&self, id: RunId) -> Run {
        self.client.call::<method::RunsGet>(pulse_contracts::rpc::RunIdParams { id }).await.unwrap()
    }

    pub async fn detail(&self, id: EventId) -> pulse_contracts::rpc::EventDetail {
        self.client
            .call::<method::EventsGet>(pulse_contracts::rpc::EventIdParams { id })
            .await
            .unwrap()
    }

    /// Every event in the log matching `filter`, oldest first.
    pub async fn query(&self, filter: EventFilter) -> Vec<Event> {
        let mut out = Vec::new();
        let mut cursor = None;
        loop {
            let page = self
                .client
                .call::<method::EventsQuery>(EventQuery {
                    filter: filter.clone(),
                    cursor,
                    limit: Some(500),
                    order: Order::OldestFirst,
                })
                .await
                .unwrap();
            out.extend(page.events);
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => return out,
            }
        }
    }

    pub async fn all_runs(&self) -> Vec<RunSummary> {
        self.client
            .call::<method::RunsList>(pulse_contracts::rpc::RunsListParams {
                limit: Some(500),
                ..Default::default()
            })
            .await
            .unwrap()
            .runs
    }
}

/// A named pipe the test writes and an agent reads, so the test decides when the agent
/// proceeds (no sleeps, in the agent or here).
pub struct Fifo {
    pub path: PathBuf,
    writer: Option<std::fs::File>,
}

impl Fifo {
    pub fn new(dir: &Path, name: &str) -> Fifo {
        let path = dir.join(name);
        let ok = std::process::Command::new("mkfifo").arg(&path).status().unwrap().success();
        assert!(ok, "mkfifo {}", path.display());
        Fifo { path, writer: None }
    }

    /// A fifo someone else (an agent) created.
    pub fn existing(path: PathBuf) -> Fifo {
        Fifo { path, writer: None }
    }

    /// Opens the write end (waits until the agent opens the read end). The blocking open runs
    /// on its own thread, not tokio's blocking pool: if the agent never opens the fifo the test
    /// fails at the ceiling, and a pool thread stuck in `open` would keep the runtime (and the
    /// test binary) from ever shutting down.
    pub async fn open(&mut self) {
        let path = self.path.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let _ = tx.send(std::fs::OpenOptions::new().write(true).open(path));
        });
        let f = within("the agent to open the fifo", rx).await.unwrap().unwrap();
        self.writer = Some(f);
    }

    pub fn write(&mut self, text: &str) {
        use std::io::Write;
        let w = self.writer.as_mut().expect("open() first");
        w.write_all(text.as_bytes()).unwrap();
        w.flush().unwrap();
    }

    /// Closes the write end: the agent reads EOF.
    pub fn close(&mut self) {
        self.writer = None;
    }
}
