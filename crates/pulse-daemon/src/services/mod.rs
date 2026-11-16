//! The daemon's features as plain Rust APIs: `events`, `rules`, `agents`, `runs`, `sources`,
//! `projects`, `system`, and what MCP clients may ask (`mcp`). Every transport (the RPC server,
//! `POST /ingest`, the MCP backend) calls
//! exactly one method here per request and only maps errors; logic belongs here, not there.

mod agents; // and sources
mod events;
mod mcp;
mod projects;
mod rules;
mod runs;

mod system;

use std::sync::Arc;

use pulse_contracts::config::{ConfigOrigin, Sourced};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::config::{DefKind, edit};
use crate::error::{ServiceError, ServiceResult};
use crate::home::write_atomic;
use crate::shared::Shared;

/// Cloneable handle to every service.
#[derive(Clone, Debug)]
pub struct Services {
    pub(crate) shared: Arc<Shared>,
}

/// A change to one definition in a config file.
pub(crate) enum Edit<'a, T> {
    Upsert(&'a T),
    Delete,
}

impl Services {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Services { shared }
    }

    /// Applies `change` to the definition `bare` (as written, without project prefix) in the
    /// user file or project `project`'s file: edit the text, check the result as a reload
    /// would, replace the file atomically, reload. Serialized with reloads by the loader lock.
    pub(crate) async fn write_definition<T>(
        &self,
        kind: DefKind,
        project: Option<&str>,
        bare: &str,
        change: Edit<'_, T>,
    ) -> ServiceResult<()>
    where
        T: Serialize + DeserializeOwned + PartialEq,
    {
        let mut loader = self.shared.loader.lock().await;
        let (origin, path) = loader.target(project)?;
        if matches!(origin, ConfigOrigin::User { .. }) && loader.user_file_changed() {
            // Pick up hand edits first, so the check below compares against the file as it is.
            let loaded = loader.load();
            let watched = loader.watched_dirs();
            self.shared.apply(loaded, watched).await;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                return Err(ServiceError::Internal(format!("cannot read {}: {e}", path.display())));
            }
        };
        let new_text = match change {
            Edit::Upsert(def) => edit::upsert(&text, kind, bare, def),
            Edit::Delete => edit::remove(&text, kind, bare),
        }
        .map_err(|e| match e {
            edit::EditError::NotFound => ServiceError::not_found(kind.array(), bare),
            other => ServiceError::config(format!("{}: {other}", path.display())),
        })?;
        if new_text == text {
            return Ok(());
        }
        loader.check(&new_text, &origin, &self.shared.snapshot())?;
        let mode = std::fs::metadata(&path).map_or(0o600, |m| {
            use std::os::unix::fs::PermissionsExt;
            m.permissions().mode() & 0o777
        });
        write_atomic(&path, new_text.as_bytes(), mode)
            .map_err(|e| ServiceError::Internal(format!("cannot write {}: {e}", path.display())))?;
        if loader.retrust_written(&origin, &new_text) {
            let records = serde_json::to_string(&loader.trust)
                .map_err(|e| ServiceError::Internal(e.to_string()))?;
            self.shared
                .store
                .write(move |s| s.set_flag(crate::config::TRUST_KEY, &records))
                .await?;
        }
        let loaded = loader.load();
        let watched = loader.watched_dirs();
        self.shared.apply(loaded, watched).await;
        Ok(())
    }
}

/// The as-written definition and its file, for a definition known by its full name.
pub(crate) fn as_written<T: Clone>(
    files: &[Sourced<pulse_contracts::config::ConfigFile>],
    origin: &ConfigOrigin,
    pick: impl Fn(&pulse_contracts::config::ConfigFile) -> Option<&T>,
) -> Option<T> {
    files.iter().find(|f| f.origin == *origin).and_then(|f| pick(&f.config).cloned())
}
