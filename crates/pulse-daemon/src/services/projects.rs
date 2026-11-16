//! `projects.*`: trusting a repository's `pulse.toml`, bound to its content hash.

use std::path::PathBuf;

use pulse_contracts::rpc::ProjectReview;
use pulse_contracts::status::Project;

use super::Services;
use crate::config::TRUST_KEY;
use crate::error::{ServiceError, ServiceResult};

impl Services {
    pub fn projects_list(&self) -> Vec<Project> {
        self.shared.projects()
    }

    /// What trusting `dir/pulse.toml` would load, as it is now. `dir` may start with `~`.
    pub async fn projects_review(
        &self,
        dir: &str,
        name: Option<&str>,
    ) -> ServiceResult<ProjectReview> {
        let dir = PathBuf::from(super::expand_tilde(dir, &self.shared.user_home));
        self.shared.loader.lock().await.review(&dir, name)
    }

    /// Trusts `dir/pulse.toml` if its content still has `hash` (what the user reviewed). Any
    /// later change makes it inert again. `dir` may start with `~`, like paths in config.
    pub async fn projects_trust(
        &self,
        dir: &str,
        name: Option<&str>,
        hash: &str,
    ) -> ServiceResult<Project> {
        let dir = PathBuf::from(super::expand_tilde(dir, &self.shared.user_home));
        self.change_trust(|loader, now| loader.trust(&dir, name, hash, now)).await
    }

    pub async fn projects_untrust(&self, dir: &str) -> ServiceResult<Project> {
        let dir = PathBuf::from(super::expand_tilde(dir, &self.shared.user_home));
        self.change_trust(|loader, _| loader.untrust(&dir)).await
    }

    async fn change_trust(
        &self,
        change: impl FnOnce(
            &mut crate::config::Loader,
            pulse_contracts::Timestamp,
        ) -> ServiceResult<Project>,
    ) -> ServiceResult<Project> {
        let mut loader = self.shared.loader.lock().await;
        let project = change(&mut loader, self.shared.clock.now())?;
        let records = serde_json::to_string(&loader.trust)
            .map_err(|e| ServiceError::Internal(e.to_string()))?;
        self.shared.store.write(move |s| s.set_flag(TRUST_KEY, &records)).await?;
        let loaded = loader.load();
        let watched = loader.watched_dirs();
        self.shared.apply(loaded, watched).await;
        Ok(project)
    }
}
