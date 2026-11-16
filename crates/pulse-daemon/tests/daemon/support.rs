//! Test harness: a daemon on port 0 in a temp `PULSE_HOME`, fake sources, and supervisors.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pulse_agents::{
    Invocation, OutputParser, RunEnd, Stream, SuperviseError, SuperviseOptions, SupervisorEvent,
};
use pulse_contracts::catalog::{BuiltinEvent, FsFileData};
use pulse_contracts::config::SourceConfig;
use pulse_contracts::rpc::{Notification, RunFilter, RunsSubscribeParams, method};
use pulse_contracts::{
    NewEvent, OutputKind, RunId, RunOutput, RunStatus, RunSummary, SourceKind, SourceRef, Timestamp,
};
use pulse_daemon::client::Client;
use pulse_daemon::wiring::{SourceBuilder, Supervisor, Wiring};
use pulse_daemon::{Clock, Daemon, DaemonOptions};
use pulse_sources::{BoxFuture, Source, SourceContext, SourceError};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// The ceiling for anything a test awaits. Reaching it means the test failed; nothing waits
/// for it on success.
pub const CEILING: Duration = Duration::from_secs(30);

/// Paused tests run on the current-thread runtime (tokio requires it); all others here use the
/// multi-thread runtime. Under `tokio::time::pause()` a timeout is useless as a ceiling: time
/// jumps to the next timer whenever the runtime idles, for instance while the store's threads
/// work, so the ceiling would fire first. Paused tests are bounded by the suite's timeout.
fn paused() -> bool {
    tokio::runtime::Handle::current().runtime_flavor()
        == tokio::runtime::RuntimeFlavor::CurrentThread
}

pub async fn within<T>(what: &str, f: impl std::future::Future<Output = T>) -> T {
    if paused() {
        return f.await;
    }
    match tokio::time::timeout(CEILING, f).await {
        Ok(v) => v,
        Err(_) => panic!("timed out waiting for {what}"),
    }
}

/// Sources whose events the test pushes. Every configured source becomes one of these.
#[derive(Clone, Default)]
pub struct FakeSources {
    senders: Arc<Mutex<HashMap<String, mpsc::Sender<NewEvent>>>>,
    built: Arc<Mutex<Vec<String>>>,
}

struct FakeSource {
    source: SourceRef,
    events: mpsc::Receiver<NewEvent>,
}

impl Source for FakeSource {
    fn source_ref(&self) -> &SourceRef {
        &self.source
    }
    fn run(mut self: Box<Self>, cx: SourceContext) -> BoxFuture<'static, Result<(), SourceError>> {
        Box::pin(async move {
            cx.health.healthy();
            loop {
                tokio::select! {
                    _ = cx.shutdown.cancelled() => return Ok(()),
                    ev = self.events.recv() => match ev {
                        Some(ev) => cx.events.send(ev).await?,
                        None => return Ok(()),
                    },
                }
            }
        })
    }
}

impl FakeSources {
    pub fn builder(&self) -> SourceBuilder {
        let this = self.clone();
        Arc::new(move |config: &SourceConfig, _env| {
            let (tx, rx) = mpsc::channel(1024);
            this.senders.lock().unwrap().insert(config.name.clone(), tx);
            this.built.lock().unwrap().push(config.name.clone());
            Ok(Box::new(FakeSource {
                source: SourceRef::new(config.kind(), &config.name),
                events: rx,
            }) as Box<dyn Source>)
        })
    }

    pub async fn push(&self, source: &str, event: NewEvent) {
        let tx = self.senders.lock().unwrap().get(source).cloned().expect("source is running");
        tx.send(event).await.unwrap();
    }

    /// Names of sources built so far, in order (restarts appear again).
    pub fn built(&self) -> Vec<String> {
        self.built.lock().unwrap().clone()
    }
}

pub fn file_created(source: &str, path: &str) -> NewEvent {
    let p = Path::new(path);
    BuiltinEvent::FsFileCreated(FsFileData {
        path: path.to_owned(),
        dir: p.parent().unwrap().display().to_string(),
        name: p.file_name().unwrap().to_string_lossy().into_owned(),
        ext: p.extension().map(|e| e.to_string_lossy().into_owned()),
        size: Some(1),
    })
    .into_new_event(
        SourceRef::new(SourceKind::Fs, source),
        Timestamp::from_system_time(std::time::SystemTime::now()),
    )
}

/// Runs invocations as real child processes in their own process group, streaming stdout and
/// stderr lines, and terminates the group on cancel or timeout. A small stand-in for
/// `pulse_agents::supervise` until that lands.
#[derive(Clone, Copy, Debug, Default)]
pub struct ChildSupervisor;

fn kill_group(pid: u32, signal: rustix::process::Signal) {
    if let Some(pid) = rustix::process::Pid::from_raw(pid as i32) {
        let _ = rustix::process::kill_process_group(pid, signal);
    }
}

impl Supervisor for ChildSupervisor {
    fn supervise(
        &self,
        inv: Invocation,
        mut parser: Box<dyn OutputParser>,
        events: mpsc::Sender<SupervisorEvent>,
        options: SuperviseOptions,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<RunEnd, SuperviseError>> {
        Box::pin(async move {
            let mut cmd = tokio::process::Command::new(&inv.program);
            cmd.args(&inv.args)
                .current_dir(&inv.cwd)
                .envs(inv.env.iter().cloned())
                .process_group(0)
                .kill_on_drop(true)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .stdin(if inv.stdin.is_some() {
                    std::process::Stdio::piped()
                } else {
                    std::process::Stdio::null()
                });
            let mut child = cmd
                .spawn()
                .map_err(|source| SuperviseError::Spawn { program: inv.program.clone(), source })?;
            let pid = child.id().unwrap_or(0);
            let _ = events.send(SupervisorEvent::Started { pid }).await;
            if let (Some(text), Some(mut stdin)) = (inv.stdin, child.stdin.take()) {
                let _ = stdin.write_all(text.as_bytes()).await;
            }
            let mut out = BufReader::new(child.stdout.take().unwrap()).lines();
            let mut err = BufReader::new(child.stderr.take().unwrap()).lines();
            let (mut out_open, mut err_open) = (true, true);
            let deadline = tokio::time::sleep(options.timeout);
            tokio::pin!(deadline);
            let mut ended = None;
            let mut parsed = Vec::new();
            while out_open || err_open {
                tokio::select! {
                    line = out.next_line(), if out_open => match line? {
                        Some(l) => parser.line(Stream::Stdout, &l, &mut parsed),
                        None => out_open = false,
                    },
                    line = err.next_line(), if err_open => match line? {
                        Some(l) => parser.line(Stream::Stderr, &l, &mut parsed),
                        None => err_open = false,
                    },
                    _ = cancel.cancelled(), if ended.is_none() => {
                        ended = Some(RunEnd::Cancelled);
                        kill_group(pid, rustix::process::Signal::TERM);
                    }
                    _ = &mut deadline, if ended.is_none() => {
                        ended = Some(RunEnd::TimedOut);
                        kill_group(pid, rustix::process::Signal::TERM);
                    }
                }
                for o in parsed.drain(..) {
                    let _ = events.send(SupervisorEvent::Output(o)).await;
                }
            }
            parser.finish(&mut parsed);
            for o in parsed.drain(..) {
                let _ = events.send(SupervisorEvent::Output(o)).await;
            }
            let status = tokio::select! {
                s = child.wait() => s?,
                _ = tokio::time::sleep(options.grace) => {
                    kill_group(pid, rustix::process::Signal::KILL);
                    child.wait().await?
                }
            };
            use std::os::unix::process::ExitStatusExt;
            Ok(ended.unwrap_or(RunEnd::Exited { code: status.code(), signal: status.signal() }))
        })
    }
}

/// Ends every run at once: `Started`, one output line per argument, exit 0. No process, no
/// timer, so it is safe under `tokio::time::pause()`.
#[derive(Clone, Copy, Debug, Default)]
pub struct InstantSupervisor;

impl Supervisor for InstantSupervisor {
    fn supervise(
        &self,
        inv: Invocation,
        _: Box<dyn OutputParser>,
        events: mpsc::Sender<SupervisorEvent>,
        _: SuperviseOptions,
        _: CancellationToken,
    ) -> BoxFuture<'static, Result<RunEnd, SuperviseError>> {
        Box::pin(async move {
            let _ = events.send(SupervisorEvent::Started { pid: 1 }).await;
            for a in inv.args {
                let _ = events
                    .send(SupervisorEvent::Output(RunOutput::new(OutputKind::Stdout, a)))
                    .await;
            }
            Ok(RunEnd::Exited { code: Some(0), signal: None })
        })
    }
}

pub struct TestDaemon {
    pub daemon: Option<Daemon>,
    pub dir: tempfile::TempDir,
    pub home: PathBuf,
    pub user_home: PathBuf,
    pub sources: FakeSources,
}

pub struct Setup {
    pub supervisor: Arc<dyn Supervisor>,
    pub clock: Clock,
    pub watch_config: bool,
    pub mcp: Option<pulse_daemon::wiring::McpMount>,
}

impl Default for Setup {
    fn default() -> Self {
        Setup {
            supervisor: Arc::new(ChildSupervisor),
            clock: Clock::System,
            watch_config: true,
            mcp: None,
        }
    }
}

impl TestDaemon {
    pub async fn start(config: &str) -> TestDaemon {
        TestDaemon::start_with(config, Setup::default()).await
    }

    pub async fn start_with(config: &str, setup: Setup) -> TestDaemon {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("pulse-home");
        let user_home = dir.path().join("user");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&user_home).unwrap();
        std::fs::write(home.join("pulse.toml"), config).unwrap();
        let mut t =
            TestDaemon { daemon: None, dir, home, user_home, sources: FakeSources::default() };
        t.boot(setup).await;
        t
    }

    pub async fn boot(&mut self, setup: Setup) {
        if std::env::var_os("PULSE_TEST_LOG").is_some() {
            let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        }
        let wiring = Wiring {
            sources: self.sources.builder(),
            supervisor: setup.supervisor,
            mcp: setup.mcp,
            ..Wiring::real()
        };
        let opts = DaemonOptions {
            port: Some(0),
            clock: setup.clock,
            watch_config: setup.watch_config,
            retention_every: None,
            ..DaemonOptions::new(&self.home, &self.user_home)
        };
        self.daemon = Some(Daemon::start(opts, wiring).await.expect("daemon starts"));
    }

    pub fn port(&self) -> u16 {
        self.daemon.as_ref().unwrap().port()
    }

    pub fn token(&self) -> String {
        std::fs::read_to_string(self.home.join("token")).unwrap()
    }

    pub async fn client(&self) -> Client {
        Client::connect(&self.home).await.expect("client connects")
    }

    pub fn write_config(&self, text: &str) {
        std::fs::write(self.home.join("pulse.toml"), text).unwrap();
    }

    pub fn read_config(&self) -> String {
        std::fs::read_to_string(self.home.join("pulse.toml")).unwrap()
    }

    pub async fn stop(&mut self) {
        if let Some(d) = self.daemon.take() {
            d.shutdown().await;
        }
    }
}

/// Subscribes to run changes and waits for runs to reach states.
pub struct RunWatch {
    pub client: Client,
    rx: mpsc::Receiver<Notification>,
    pub seen: Vec<RunSummary>,
}

impl RunWatch {
    pub async fn new(client: Client) -> RunWatch {
        let rx = client.notifications().unwrap();
        client
            .call::<method::RunsSubscribe>(RunsSubscribeParams { filter: RunFilter::default() })
            .await
            .unwrap();
        RunWatch { client, rx, seen: Vec::new() }
    }

    /// The next run (any id, or `id`) reaching `status`.
    pub async fn until(&mut self, id: Option<RunId>, status: RunStatus) -> RunSummary {
        if let Some(r) =
            self.seen.iter().find(|r| id.is_none_or(|i| r.id == i) && r.status == status)
        {
            return r.clone();
        }
        let what = format!("a run to be {status:?}");
        let seen = &mut self.seen;
        let rx = &mut self.rx;
        let wait = async {
            loop {
                if let Notification::RunsChanged(c) = rx.recv().await.expect("connection open") {
                    // Keep the whole batch: later waits may be for other runs in it.
                    seen.extend(c.runs.iter().cloned());
                    if let Some(r) = c
                        .runs
                        .into_iter()
                        .find(|r| id.is_none_or(|i| r.id == i) && r.status == status)
                    {
                        return r;
                    }
                }
            }
        };
        if paused() {
            return wait.await;
        }
        match tokio::time::timeout(CEILING, wait).await {
            Ok(r) => r,
            Err(_) => panic!("timed out waiting for {what}; saw {:#?}", self.seen),
        }
    }

    /// The next notification of another kind (for tests that interleave).
    pub async fn next(&mut self) -> Notification {
        within("a notification", self.rx.recv()).await.expect("connection open")
    }
}

pub const COMMAND_AGENT: &str = r#"
[[agent]]
name = "sh"
adapter = "command"
command = ["sh", "-c", "echo \"got $PULSE_EVENT_SUBJECT\"; echo oops >&2"]
allow = ["fs.*", "ext.*", "process.*"]
max_concurrent = 1
"#;

pub const FAKE_SOURCE: &str = r#"
[[source]]
name = "fake"
kind = "schedule"
every = "1h"
"#;
