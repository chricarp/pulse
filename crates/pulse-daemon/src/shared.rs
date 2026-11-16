//! What the services, the engine, the worker and the transports share.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use pulse_contracts::Timestamp;
use pulse_contracts::config::ConfigError;
use pulse_contracts::rpc::{StateChanged, StateTopic};
use pulse_contracts::status::Project;
use pulse_store::StoreHandle;
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

use crate::auth::LoginCodes;
use crate::clock::{Clock, Ids};
use crate::config::watch::ConfigWatcher;
use crate::config::{Loaded, Loader, Snapshot, changed_topics};
use crate::engine::{Control, EngineHandle};
use crate::home::PulseHome;
use crate::hub::Hub;
use crate::sources::SourceManager;
use crate::wiring::{AdapterLookup, SourceBuilder, Supervisor};
use crate::worker::WorkerHandle;

/// Clients are told about state changes at most this often.
const STATE_COALESCE: Duration = Duration::from_millis(100);

pub(crate) struct Shared {
    pub home: PulseHome,
    /// Expands `~` in config.
    pub user_home: PathBuf,
    pub clock: Clock,
    pub ids: Ids,
    pub store: StoreHandle,
    pub hub: Hub,
    pub secret: String,
    pub login_codes: LoginCodes,
    pub started_at: Timestamp,
    pub port: OnceLock<u16>,
    pub shutdown: CancellationToken,

    snapshot: RwLock<Arc<Snapshot>>,
    config_errors: RwLock<Vec<ConfigError>>,
    projects: RwLock<Vec<Project>>,
    pub loader: tokio::sync::Mutex<Loader>,
    pub watcher: Mutex<Option<ConfigWatcher>>,

    pub engine: EngineHandle,
    pub worker: WorkerHandle,
    pub sources: SourceManager,
    paused: AtomicBool,
    running: Mutex<HashMap<String, u32>>,

    pending_topics: Mutex<BTreeSet<StateTopicKey>>,
    state_notify: Notify,
    /// Bumped whenever a new configuration takes effect.
    config_version: watch::Sender<u64>,

    pub source_builder: SourceBuilder,
    pub adapters: AdapterLookup,
    pub supervisor: Arc<dyn Supervisor>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared").field("home", &self.home).finish_non_exhaustive()
    }
}

/// `StateTopic` is not `Ord`; this orders it for the pending set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct StateTopicKey(u8);

const TOPICS: [StateTopic; 6] = [
    StateTopic::Pause,
    StateTopic::Config,
    StateTopic::Rules,
    StateTopic::Agents,
    StateTopic::Sources,
    StateTopic::Projects,
];

fn key(t: StateTopic) -> StateTopicKey {
    StateTopicKey(TOPICS.iter().position(|x| *x == t).unwrap_or(0) as u8)
}

pub(crate) struct SharedParts {
    pub home: PulseHome,
    pub user_home: PathBuf,
    pub clock: Clock,
    pub store: StoreHandle,
    /// The newest event in the log: new ids continue after it.
    pub last_event: Option<pulse_contracts::EventId>,
    pub secret: String,
    pub snapshot: Snapshot,
    pub loader: Loader,
    pub engine: EngineHandle,
    pub worker: WorkerHandle,
    pub paused: bool,
    pub wiring: crate::wiring::Wiring,
}

impl Shared {
    pub(crate) fn new(p: SharedParts) -> Shared {
        let started_at = p.clock.now();
        Shared {
            ids: Ids::new(p.clock, p.last_event.map(|e| e.ulid())),
            home: p.home,
            user_home: p.user_home,
            clock: p.clock,
            store: p.store,
            hub: Hub::default(),
            secret: p.secret,
            login_codes: LoginCodes::default(),
            started_at,
            port: OnceLock::new(),
            shutdown: CancellationToken::new(),
            snapshot: RwLock::new(Arc::new(p.snapshot)),
            config_errors: RwLock::new(Vec::new()),
            projects: RwLock::new(Vec::new()),
            loader: tokio::sync::Mutex::new(p.loader),
            watcher: Mutex::new(None),
            engine: p.engine,
            worker: p.worker,
            sources: SourceManager::default(),
            paused: AtomicBool::new(p.paused),
            running: Mutex::new(HashMap::new()),
            pending_topics: Mutex::new(BTreeSet::new()),
            state_notify: Notify::new(),
            config_version: watch::channel(0).0,
            source_builder: p.wiring.sources,
            adapters: p.wiring.adapters,
            supervisor: p.wiring.supervisor,
        }
    }

    pub(crate) fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub(crate) fn config_errors(&self) -> Vec<ConfigError> {
        self.config_errors.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub(crate) fn projects(&self) -> Vec<Project> {
        self.projects.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Changes whenever a new configuration takes effect.
    pub(crate) fn config_changes(&self) -> watch::Receiver<u64> {
        self.config_version.subscribe()
    }

    pub(crate) fn port(&self) -> u16 {
        self.port.get().copied().unwrap_or(0)
    }

    pub(crate) fn paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub(crate) fn set_paused_flag(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    pub(crate) fn set_running(&self, running: HashMap<String, u32>) {
        *self.running.lock().unwrap_or_else(|p| p.into_inner()) = running;
    }

    pub(crate) fn running(&self, agent: &str) -> u32 {
        self.running.lock().unwrap_or_else(|p| p.into_inner()).get(agent).copied().unwrap_or(0)
    }

    /// Queues a `state.changed` for every connection. Bursts are coalesced.
    pub(crate) fn notify_state(&self, topics: &[StateTopic]) {
        let mut pending = self.pending_topics.lock().unwrap_or_else(|p| p.into_inner());
        pending.extend(topics.iter().map(|t| key(*t)));
        drop(pending);
        self.state_notify.notify_one();
    }

    /// Forgets queued notifications (the initial load at startup has no one to tell).
    pub(crate) fn clear_pending_state(&self) {
        self.pending_topics.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    /// Sends coalesced `state.changed` notifications until shutdown.
    pub(crate) async fn run_state_notifier(self: Arc<Self>, services: crate::Services) {
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = self.state_notify.notified() => {}
            }
            tokio::time::sleep(STATE_COALESCE).await;
            let keys =
                std::mem::take(&mut *self.pending_topics.lock().unwrap_or_else(|p| p.into_inner()));
            if keys.is_empty() {
                continue;
            }
            let topics = keys.iter().map(|k| TOPICS[k.0 as usize]).collect();
            match services.status().await {
                Ok(status) => self.hub.publish_state(StateChanged { status, topics }),
                Err(e) => tracing::warn!("could not compute status: {e}"),
            }
        }
    }

    /// Puts a load result into effect: the new config (if it resolved), its errors and project
    /// states. Restarts sources and wakes the engine and worker when the config changed.
    pub(crate) async fn apply(self: &Arc<Self>, loaded: Loaded, watched: Vec<PathBuf>) {
        let mut topics = Vec::new();
        {
            let mut errors = self.config_errors.write().unwrap_or_else(|p| p.into_inner());
            if *errors != loaded.errors {
                // A daemon runs headless: the log is where a broken file is noticed.
                for e in &loaded.errors {
                    tracing::warn!("config error (the last good config stays in effect): {e}");
                }
                *errors = loaded.errors;
                topics.push(StateTopic::Config);
            }
        }
        {
            let mut projects = self.projects.write().unwrap_or_else(|p| p.into_inner());
            if *projects != loaded.projects {
                *projects = loaded.projects;
                topics.push(StateTopic::Projects);
            }
        }
        if let Some(snapshot) = loaded.snapshot {
            let old = self.snapshot();
            if old.warnings != snapshot.warnings {
                for w in &snapshot.warnings {
                    tracing::warn!("config: {w}");
                }
                topics.push(StateTopic::Config);
            }
            if old.config != snapshot.config {
                topics.extend(changed_topics(&old.config, &snapshot.config));
                // `port` is set once listening; the first load is what the listener uses.
                if self.port.get().is_some()
                    && old.config.daemon.port != snapshot.config.daemon.port
                {
                    tracing::info!("the new [daemon] port takes effect when the daemon restarts");
                }
                if old.config.daemon.mcp_events_extension
                    != snapshot.config.daemon.mcp_events_extension
                {
                    tracing::info!(
                        "the new [daemon] mcp_events_extension takes effect when the daemon restarts"
                    );
                }
                let snapshot = Arc::new(snapshot);
                *self.snapshot.write().unwrap_or_else(|p| p.into_inner()) = snapshot.clone();
                let _ = self.engine.control(Control::Config(snapshot.clone())).await;
                self.sources.reconcile(self, &snapshot.config);
                self.worker.wake();
                self.config_version.send_modify(|v| *v = v.wrapping_add(1));
            } else {
                // Same meaning; keep the files (they may differ in formatting only).
                *self.snapshot.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(snapshot);
            }
        }
        if let Some(w) = self.watcher.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
            w.set_dirs(watched);
        }
        if !topics.is_empty() {
            tracing::info!("config applied ({topics:?} changed)");
            self.notify_state(&topics);
        }
    }

    /// Re-reads every config file.
    pub(crate) async fn reload(self: &Arc<Self>) {
        let mut loader = self.loader.lock().await;
        let loaded = loader.load();
        let watched = loader.watched_dirs();
        self.apply(loaded, watched).await;
    }
}
