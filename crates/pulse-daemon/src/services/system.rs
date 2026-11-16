//! `system.*`: status, global pause, shutdown, the catalog, config errors.

use pulse_contracts::catalog::{EventTypeInfo, event_types};
use pulse_contracts::config::ConfigError;
use pulse_contracts::rpc::{ConfigErrorList, StateTopic};
use pulse_contracts::status::{SourceCounts, SourceState, SystemStatus};
use pulse_store::flags;

use super::Services;
use crate::engine::Control;
use crate::error::ServiceResult;

impl Services {
    pub async fn status(&self) -> ServiceResult<SystemStatus> {
        let runs = self.shared.store.read(|q| q.query().run_counts()).await?;
        let snapshot = self.shared.snapshot();
        let mut sources = SourceCounts::default();
        for state in self.shared.sources.states() {
            match state {
                SourceState::Healthy => sources.healthy += 1,
                SourceState::Degraded => sources.degraded += 1,
                SourceState::Failed => sources.failed += 1,
                _ => sources.other += 1,
            }
        }
        let n = |len: usize| u32::try_from(len).unwrap_or(u32::MAX);
        Ok(SystemStatus {
            version: crate::VERSION.to_owned(),
            started_at: self.shared.started_at,
            paused: self.shared.paused(),
            pulse_home: self.shared.home.root().display().to_string(),
            user_home: self.shared.user_home.display().to_string(),
            port: self.shared.port(),
            sources,
            runs,
            rules: n(snapshot.config.rules.iter().filter(|r| !r.config.archived).count()),
            agents: n(snapshot.config.agents.len()),
            config_errors: n(self.shared.config_errors().len()),
            config_warnings: n(self.config_warnings().len()),
        })
    }

    /// Global pause: rules are still evaluated and every decision recorded (as
    /// `global_pause`), but nothing fires, and queued runs wait. Running runs continue.
    pub async fn pause(&self) -> ServiceResult<SystemStatus> {
        self.set_paused(true).await
    }

    pub async fn resume(&self) -> ServiceResult<SystemStatus> {
        self.set_paused(false).await
    }

    async fn set_paused(&self, paused: bool) -> ServiceResult<SystemStatus> {
        self.shared
            .store
            .write(move |s| {
                if paused { s.set_flag(flags::PAUSED, "1") } else { s.delete_flag(flags::PAUSED) }
            })
            .await?;
        self.shared.set_paused_flag(paused);
        self.shared.engine.control(Control::Paused(paused)).await?;
        if !paused {
            self.shared.worker.wake();
        }
        self.shared.notify_state(&[StateTopic::Pause]);
        self.status().await
    }

    /// Starts a graceful shutdown: stop sources and transports, terminate runs, flush, exit.
    pub fn shutdown(&self) {
        tracing::info!("shutdown requested");
        self.shared.shutdown.cancel();
    }

    pub fn event_types(&self) -> Vec<EventTypeInfo> {
        event_types()
    }

    pub fn config_errors(&self) -> Vec<ConfigError> {
        self.shared.config_errors()
    }

    /// Errors (what did not load) and warnings (what loaded but cannot work as written).
    pub fn config_report(&self) -> ConfigErrorList {
        ConfigErrorList { errors: self.shared.config_errors(), warnings: self.config_warnings() }
    }

    /// Checked now, not at load: creating a missing directory or installing the agent's CLI
    /// clears its warning without touching the config.
    fn config_warnings(&self) -> Vec<ConfigError> {
        crate::config::warnings(&self.shared.snapshot().config, &self.shared.user_home)
    }

    /// Re-reads every config file now (also done on file changes and SIGHUP).
    pub async fn reload_config(&self) {
        self.shared.reload().await;
    }
}
