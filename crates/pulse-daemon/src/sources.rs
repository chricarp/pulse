//! Running the configured sources: start, restart on config change, stop, and health.
//!
//! Each source gets its own bounded channel (from `pulse_sources::context`) and a forwarder task
//! that moves its events into the engine's input channel with `send().await`, so a busy engine
//! slows the source down instead of buffering. Health is a `watch` channel per source; state
//! changes (not counters) notify clients.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use pulse_contracts::config::{Config, SourceConfig, SourceSettings, Sourced};
use pulse_contracts::pattern::expand_path;
use pulse_contracts::rpc::StateTopic;
use pulse_contracts::status::{SourceHealth, SourceState};
use pulse_sources::{SourceEnv, context};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::engine::Input;
use crate::shared::Shared;

/// Events a source may have in flight to the engine.
const SOURCE_CHANNEL: usize = 256;

enum Health {
    Fixed(SourceHealth),
    Live(watch::Receiver<SourceHealth>),
}

struct Running {
    config: Sourced<SourceConfig>,
    health: Health,
    cancel: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
}

#[derive(Default)]
pub(crate) struct SourceManager {
    running: Mutex<BTreeMap<String, Running>>,
}

impl std::fmt::Debug for SourceManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceManager").finish_non_exhaustive()
    }
}

/// Resolves a project source's relative paths against the project directory, so the source
/// itself only ever sees `~/…` or absolute paths.
fn with_resolved_paths(s: &Sourced<SourceConfig>, home: &Path) -> SourceConfig {
    let mut config = s.config.clone();
    let Some(base) = s.origin.base_dir() else { return config };
    let fix = |p: &mut String| {
        if let Ok(abs) = expand_path(p, home, Some(base)) {
            *p = abs;
        }
    };
    match &mut config.settings {
        SourceSettings::Fs(fs) => fix(&mut fs.path),
        SourceSettings::Git(g) => fix(&mut g.path),
        SourceSettings::Calendar(c) => {
            if let Some(p) = &mut c.path {
                fix(p);
            }
        }
        SourceSettings::Schedule(_) => {}
    }
    config
}

impl SourceManager {
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Running>> {
        self.running.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Brings running sources in line with `config`: stops removed ones, restarts changed ones,
    /// starts new ones.
    pub(crate) fn reconcile(&self, shared: &Arc<Shared>, config: &Config) {
        let mut running = self.lock();
        let wanted: BTreeMap<&str, &Sourced<SourceConfig>> =
            config.sources.iter().map(|s| (s.config.name.as_str(), s)).collect();
        let stale: Vec<String> = running
            .iter()
            .filter(|(name, r)| wanted.get(name.as_str()).is_none_or(|w| **w != r.config))
            .map(|(name, _)| name.clone())
            .collect();
        for name in stale {
            if let Some(r) = running.remove(&name) {
                r.cancel.cancel();
            }
        }
        for (name, s) in wanted {
            if !running.contains_key(name) {
                running.insert(name.to_owned(), start(shared, s));
            }
        }
    }

    pub(crate) fn health(&self, name: &str) -> SourceHealth {
        let now = pulse_contracts::Timestamp::EPOCH;
        match self.lock().get(name).map(|r| &r.health) {
            Some(Health::Fixed(h)) => h.clone(),
            Some(Health::Live(rx)) => rx.borrow().clone(),
            None => SourceHealth { state: SourceState::Stopped, ..SourceHealth::starting(now) },
        }
    }

    pub(crate) fn states(&self) -> Vec<SourceState> {
        self.lock()
            .values()
            .map(|r| match &r.health {
                Health::Fixed(h) => h.state,
                Health::Live(rx) => rx.borrow().state,
            })
            .collect()
    }

    /// Stops every source and waits for their tasks.
    pub(crate) async fn stop_all(&self) {
        let running: Vec<Running> = std::mem::take(&mut *self.lock()).into_values().collect();
        for r in &running {
            r.cancel.cancel();
        }
        for r in running {
            for t in r.tasks {
                let _ = t.await;
            }
        }
    }
}

fn fixed(config: &Sourced<SourceConfig>, state: SourceState, message: Option<String>, shared: &Shared) -> Running {
    let now = shared.clock.now();
    let mut health = SourceHealth::starting(now);
    health.state = state;
    if let Some(m) = &message {
        health.last_error =
            Some(pulse_contracts::status::SourceErrorInfo { at: now, message: m.clone() });
    }
    health.message = message;
    Running {
        config: config.clone(),
        health: Health::Fixed(health),
        cancel: CancellationToken::new(),
        tasks: Vec::new(),
    }
}

fn start(shared: &Arc<Shared>, s: &Sourced<SourceConfig>) -> Running {
    if !s.config.enabled {
        return fixed(s, SourceState::Disabled, None, shared);
    }
    let env = SourceEnv {
        home: shared.user_home.clone(),
        pulse_home: shared.home.root().to_path_buf(),
    };
    let config = with_resolved_paths(s, &shared.user_home);
    let source = match (shared.source_builder)(&config, &env) {
        Ok(source) => source,
        Err(e) => {
            tracing::warn!("source {:?} cannot start: {e}", s.config.name);
            return fixed(s, SourceState::Failed, Some(e.to_string()), shared);
        }
    };
    let cancel = shared.shutdown.child_token();
    let (cx, mut events, health) =
        context(source.source_ref().clone(), SOURCE_CHANNEL, cancel.clone(), shared.clock.now());
    let reporter = cx.health.clone();
    let name = s.config.name.clone();
    let runner = tokio::spawn(async move {
        match source.run(cx).await {
            Ok(()) => reporter.stopped(),
            Err(e) => {
                tracing::warn!("source {name:?} stopped: {e}");
                reporter.failed(e.to_string());
            }
        }
    });
    let engine = shared.engine.clone();
    let forwarder = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if engine.input(Input::Source(event)).await.is_err() {
                break;
            }
        }
    });
    let mut watch_health = health.clone();
    let notify = shared.clone();
    let stop = cancel.clone();
    let watcher = tokio::spawn(async move {
        let mut last = watch_health.borrow().state;
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                changed = watch_health.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let state = watch_health.borrow().state;
                    if state != last {
                        last = state;
                        notify.notify_state(&[StateTopic::Sources]);
                    }
                }
            }
        }
    });
    Running {
        config: s.clone(),
        health: Health::Live(health),
        cancel,
        tasks: vec![runner, forwarder, watcher],
    }
}
