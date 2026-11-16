//! `projects.*`: trusting a repository's `pulse.toml`, bound to its content hash.

use std::path::Path;

use pulse_contracts::status::Project;

use super::Services;
use crate::config::TRUST_KEY;
use crate::error::{ServiceError, ServiceResult};

impl Services {
    pub fn projects_list(&self) -> Vec<Project> {
        self.shared.projects()
    }

    /// Trusts `dir/pulse.toml` exactly as it is now. Any later change makes it inert again.
    pub async fn projects_trust(&self, dir: &Path, name: Option<&str>) -> ServiceResult<Project> {
        self.change_trust(|loader, now| loader.trust(dir, name, now)).await
    }

    pub async fn projects_untrust(&self, dir: &Path) -> ServiceResult<Project> {
        self.change_trust(|loader, _| loader.untrust(dir)).await
    }

    async fn change_trust(
        &self,
        change: impl FnOnce(&mut crate::config::Loader, pulse_contracts::Timestamp) -> ServiceResult<Project>,
    ) -> ServiceResult<Project> {
        let mut loader = self.shared.loader.lock().await;
        let project = change(&mut loader, self.shared.clock.now())?;
        let records =
            serde_json::to_string(&loader.trust).map_err(|e| ServiceError::Internal(e.to_string()))?;
        self.shared.store.write(move |s| s.set_flag(TRUST_KEY, &records)).await?;
        let loaded = loader.load();
        let watched = loader.watched_dirs();
        self.shared.apply(loaded, watched).await;
        Ok(project)
    }
}
