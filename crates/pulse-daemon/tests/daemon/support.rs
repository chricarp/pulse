//! Test harness: a daemon on port 0 in a temp `PULSE_HOME`, fake sources, the real supervisor
//! with the fake agents from `pulse-agents/tests/fixtures`, and an MCP client.
//!
//! Real processes only run on real time. Under `tokio::time::pause()` an idle runtime jumps past
//! the supervisor's grace period while a sibling test's fork still holds a copy of a pipe, and a
//! run would end with a spurious "pipe still open" marker (see the fixtures README). So the one
//! paused test uses [`InstantSupervisor`], which spawns nothing, and every test that runs an
//! agent uses the multi-thread runtime and real time.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pulse_agents::{
    Invocation, OutputParser, RunEnd, SuperviseError, SuperviseOptions, SupervisorEvent,
};
use pulse_contracts::catalog::{BuiltinEvent, FsFileData};
use pulse_contracts::config::SourceConfig;
use pulse_contracts::rpc::{Notification, RunFilter, RunsSubscribeParams, method};
use pulse_contracts::{
    NewEvent, OutputKind, RunId, RunOutput, RunStatus, RunSummary, SourceKind, SourceRef, Timestamp,
};
use pulse_daemon::client::Client;
use pulse_daemon::wiring::{RealSupervisor, SourceBuilder, Supervisor, Wiring};
use pulse_daemon::{Clock, Daemon, DaemonOptions};
use pulse_sources::{BoxFuture, Source, SourceContext, SourceError};
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

/// Ends every run at once: `Started`, one output line per argument, exit 0. No process, no
/// timer, so it is safe under `tokio::time::pause()`.
/// Spawns nothing and never reports a start: it tells the test when a spawn was asked for, then
/// holds the run until it is cancelled. The window between "spawn" and "started" stays open.
pub struct SpawnGate(pub mpsc::UnboundedSender<RunId>);

impl Supervisor for SpawnGate {
    fn supervise(
        &self,
        inv: Invocation,
        _: Box<dyn OutputParser>,
        _events: mpsc::Sender<SupervisorEvent>,
        _: SuperviseOptions,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<RunEnd, SuperviseError>> {
        let id = inv.env.iter().find(|(k, _)| k == "PULSE_RUN_ID").unwrap().1.parse().unwrap();
        let _ = self.0.send(id);
        Box::pin(async move {
            cancel.cancelled().await;
            Ok(RunEnd::Cancelled)
        })
    }
}

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
                    .send(SupervisorEvent::Output {
                        at: Timestamp::EPOCH,
                        output: RunOutput::new(OutputKind::Stdout, a),
                    })
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
}

impl Default for Setup {
    fn default() -> Self {
        Setup { supervisor: Arc::new(RealSupervisor), clock: Clock::System, watch_config: true }
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

pub type RawSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A WebSocket without the typed client, to observe frame order.
pub async fn raw_socket(port: u16, token: &str) -> RawSocket {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://127.0.0.1:{port}/ws").into_client_request().unwrap();
    req.headers_mut().insert("authorization", format!("Bearer {token}").parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

/// Sends one frame and returns the first text frame that comes back.
pub async fn raw_call(ws: &mut RawSocket, frame: &str) -> serde_json::Value {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    ws.send(Message::Text(frame.to_owned().into())).await.unwrap();
    within("a frame", async {
        loop {
            if let Message::Text(t) = ws.next().await.unwrap().unwrap() {
                return serde_json::from_str(t.as_str()).unwrap();
            }
        }
    })
    .await
}

/// The fake agents (`fake-agent`, `fake-claude`, `fake-codex`) and recorded streams.
pub fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../pulse-agents/tests/fixtures")
}

pub fn fixture(name: &str) -> String {
    fixtures().join(name).display().to_string()
}
