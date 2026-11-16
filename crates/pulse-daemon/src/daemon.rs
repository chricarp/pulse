//! Starting and stopping the daemon.
//!
//! Start: lock the Pulse home (one daemon per home), load or create the secret, open and
//! recover the store (`running` runs become `interrupted`), load config, rebuild routing state,
//! start the engine, the worker, the sources and the listener, then write `daemon.json`.
//!
//! Stop (SIGTERM/SIGINT, `system.shutdown`, or [`Daemon::shutdown`]): stop the listener and
//! close connections (those still open after a few seconds are dropped: see `server`), stop sources, stop the engine, cancel every run and wait until the
//! supervisor has reaped each one, checkpoint the database, remove `daemon.json`, release the
//! lock.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pulse_contracts::status::DaemonInfo;
use pulse_store::{StoreHandle, flags};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::clock::Clock;
use crate::config::watch::ConfigWatcher;
use crate::config::{Loader, Snapshot, TRUST_KEY, TrustRecord};
use crate::engine::{self, Engine};
use crate::home::{InstanceLock, PulseHome};
use crate::services::Services;
use crate::shared::{Shared, SharedParts};
use crate::wiring::Wiring;
use crate::worker::{Worker, WorkerHandle};

/// How the daemon is started.
#[derive(Clone, Debug)]
pub struct DaemonOptions {
    /// `$PULSE_HOME`.
    pub home: PathBuf,
    /// The user's home directory, for `~` in config.
    pub user_home: PathBuf,
    /// Overrides `[daemon] port` (tests use 0).
    pub port: Option<u16>,
    pub clock: Clock,
    /// Watch config files and reload on change.
    pub watch_config: bool,
    /// How often retention runs. `None` disables it.
    pub retention_every: Option<Duration>,
}

impl DaemonOptions {
    pub fn new(home: impl Into<PathBuf>, user_home: impl Into<PathBuf>) -> Self {
        DaemonOptions {
            home: home.into(),
            user_home: user_home.into(),
            port: None,
            clock: Clock::System,
            watch_config: true,
            retention_every: Some(Duration::from_secs(3600)),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("another Pulse daemon is already running for {home}{detail}")]
    AlreadyRunning { home: String, detail: String },
    #[error("{context}: {source}")]
    Io { context: String, source: std::io::Error },
    #[error("cannot open the database: {0}")]
    Store(#[from] pulse_store::StoreError),
    #[error("{0}")]
    Other(String),
}

fn io(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> StartError {
    let context = context.into();
    move |source| StartError::Io { context, source }
}

/// A running daemon.
#[derive(Debug)]
pub struct Daemon {
    shared: Arc<Shared>,
    services: Services,
    tasks: Vec<JoinHandle<()>>,
    engine: JoinHandle<()>,
    worker: JoinHandle<()>,
    server: JoinHandle<()>,
    _lock: InstanceLock,
}

impl Daemon {
    pub async fn start(opts: DaemonOptions, wiring: Wiring) -> Result<Daemon, StartError> {
        let home = PulseHome::new(&opts.home);
        home.create_dirs().map_err(io(format!("cannot create {}", opts.home.display())))?;
        let lock = InstanceLock::try_acquire(&home)
            .map_err(io("cannot lock the Pulse home"))?
            .ok_or_else(|| StartError::AlreadyRunning {
                home: opts.home.display().to_string(),
                detail: match home.read_daemon_info() {
                    Ok(Some(i)) => format!(" (pid {}, port {})", i.pid, i.port),
                    _ => String::new(),
                },
            })?;
        let secret = home.load_or_create_token().map_err(io("cannot read or create the token"))?;
        let instance = crate::auth::random_hex::<16>().map_err(io("cannot draw an instance id"))?;
        let store = StoreHandle::open(&home.database())?;
        let now = opts.clock.now();
        let interrupted = store.write(move |s| s.recover_after_restart(now)).await?;
        if !interrupted.is_empty() {
            tracing::info!(
                "{} runs were active when the daemon stopped; marked interrupted",
                interrupted.len()
            );
        }
        let paused = store.read(|q| q.query().flag(flags::PAUSED)).await?.is_some();
        let last_event = store.read(|q| q.query().last_event_id()).await?;
        let trust: Vec<TrustRecord> = match store.read(|q| q.query().flag(TRUST_KEY)).await? {
            Some(json) => serde_json::from_str(&json)
                .map_err(|e| StartError::Other(format!("trusted projects are unreadable: {e}")))?,
            None => Vec::new(),
        };
        let mut loader = Loader::new(home.clone(), opts.user_home.clone(), trust);
        let loaded = loader.load();
        let initial = Snapshot::empty(&home, &opts.user_home)
            .map_err(|e| StartError::Other(e.to_string()))?;

        let (engine_handle, control_rx, input_rx) = engine::channels();
        let (worker_handle, worker_rx) = WorkerHandle::channels();
        let worker_notify = worker_handle.clone();
        let shared = Arc::new(Shared::new(SharedParts {
            home: home.clone(),
            user_home: opts.user_home.clone(),
            clock: opts.clock,
            store,
            last_event,
            secret,
            instance,
            snapshot: initial,
            loader,
            engine: engine_handle,
            worker: worker_handle,
            paused,
            wiring,
        }));
        let services = Services::new(shared.clone());

        // The config goes into effect before the engine starts, so routing state is rebuilt
        // against the real rules.
        let watched = shared.loader.lock().await.watched_dirs();
        shared.apply(loaded, watched.clone()).await;
        let engine = Engine::restore(shared.clone(), shared.snapshot(), &interrupted)
            .await
            .map_err(|e| StartError::Other(format!("cannot rebuild routing state: {e}")))?;
        // `apply` above queued a Config message for the engine; replaying it is harmless.
        let engine = tokio::spawn(engine.run(control_rx, input_rx));
        let worker = Worker::new(shared.clone());
        let worker = tokio::spawn(worker.run(worker_rx, worker_notify.notify()));
        shared.worker.wake();

        let port = opts.port.unwrap_or(shared.snapshot().config.daemon.port);
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(io(format!("cannot listen on 127.0.0.1:{port}")))?;
        let port = listener.local_addr().map_err(io("cannot read the listening port"))?.port();
        let _ = shared.port.set(port);
        let app = crate::transport::router(services.clone());
        let stop = shared.shutdown.clone();
        let server = tokio::spawn(crate::server::serve(listener, app, stop, crate::server::LIMITS));

        shared.clear_pending_state();
        let mut tasks = vec![tokio::spawn(shared.clone().run_state_notifier(services.clone()))];
        if opts.watch_config {
            tasks.push(spawn_config_watch(shared.clone(), watched));
        }
        if let Some(every) = opts.retention_every {
            tasks.push(tokio::spawn(retention(shared.clone(), every)));
        }

        home.write_daemon_info(&DaemonInfo {
            pid: std::process::id(),
            port,
            version: crate::VERSION.to_owned(),
            started_at: shared.started_at,
        })
        .map_err(io("cannot write daemon.json"))?;
        tracing::info!("Pulse {} listening on 127.0.0.1:{port}", crate::VERSION);
        Ok(Daemon { shared, services, tasks, engine, worker, server, _lock: lock })
    }

    pub fn port(&self) -> u16 {
        self.shared.port()
    }

    pub fn services(&self) -> Services {
        self.services.clone()
    }

    /// Cancel it to shut down (what SIGTERM and `system.shutdown` do).
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shared.shutdown.clone()
    }

    /// Waits for a shutdown request, then shuts down.
    pub async fn wait(self) {
        self.shared.shutdown.cancelled().await;
        self.finish().await;
    }

    /// Shuts down now.
    pub async fn shutdown(self) {
        self.shared.shutdown.cancel();
        self.finish().await;
    }

    async fn finish(self) {
        tracing::info!("shutting down");
        let _ = self.server.await;
        self.shared.sources.stop_all().await;
        let _ = self.engine.await;
        // Waits until every run's process group is terminated and the run is recorded.
        let _ = self.worker.await;
        for t in self.tasks {
            t.abort();
            let _ = t.await;
        }
        if let Some(w) = self.shared.watcher.lock().unwrap_or_else(|p| p.into_inner()).take() {
            drop(w);
        }
        if let Err(e) = self.shared.store.write(|s| s.checkpoint()).await {
            tracing::warn!("could not checkpoint the database: {e}");
        }
        self.shared.home.remove_daemon_info();
        tracing::info!("stopped");
    }
}

fn spawn_config_watch(shared: Arc<Shared>, dirs: Vec<PathBuf>) -> JoinHandle<()> {
    let (tx, mut rx) = mpsc::channel(1);
    match ConfigWatcher::new(tx) {
        Ok(mut w) => {
            w.set_dirs(dirs);
            *shared.watcher.lock().unwrap_or_else(|p| p.into_inner()) = Some(w);
        }
        Err(e) => tracing::warn!("config files will not be watched: {e}"),
    }
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shared.shutdown.cancelled() => return,
                got = rx.recv() => if got.is_none() { return },
            }
            // Editors write in several steps; let them finish.
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(150)) => break,
                    got = rx.recv() => if got.is_none() { return },
                }
            }
            tracing::debug!("a config file changed; reloading");
            shared.reload().await;
        }
    })
}

/// Prunes in small batches, yielding between them so routing commits interleave.
async fn retention(shared: Arc<Shared>, every: Duration) {
    loop {
        let policy = shared.snapshot().config.retention.clone();
        // Totals over all batches: the last batch alone is usually empty.
        let (mut events, mut runs) = (0, 0);
        loop {
            let now = shared.clock.now();
            let policy = policy.clone();
            match shared.store.write(move |s| s.prune(&policy, now, 500)).await {
                Ok(report) => {
                    events += report.events;
                    runs += report.runs;
                    if report.more {
                        tokio::task::yield_now().await;
                        continue;
                    }
                    if runs + events > 0 {
                        tracing::info!("retention pruned {events} events and {runs} runs");
                    }
                    break;
                }
                Err(e) => {
                    tracing::warn!("retention: {e}");
                    break;
                }
            }
        }
        prune_run_dirs(&shared).await;
        tokio::select! {
            _ = shared.shutdown.cancelled() => return,
            _ = tokio::time::sleep(every) => {}
        }
    }
}

/// Removes per-run directories (event file, default working directory) of runs that retention
/// has deleted.
async fn prune_run_dirs(shared: &Arc<Shared>) {
    let dir = shared.home.runs_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let ids: Vec<(pulse_contracts::RunId, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| Some((e.file_name().to_str()?.parse().ok()?, e.path())))
        .collect();
    for (id, path) in ids {
        let exists = shared.store.read(move |q| q.query().run(id)).await;
        if matches!(exists, Ok(None)) {
            let _ = tokio::fs::remove_dir_all(path).await;
        }
    }
}
