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

use pulse_contracts::rpc::Expected;

use crate::config::{DefKind, Loader, edit};
use crate::error::{ServiceError, ServiceResult};
use crate::home::write_atomic;
use crate::shared::Shared;

/// The last characters of an event or run id, as typed: uppercase Crockford base32, shorter
/// than a full id. `what` is `event` or `run`.
pub(crate) fn id_suffix(text: &str, what: &str) -> ServiceResult<String> {
    const MIN: usize = 3;
    let suffix = text.trim().to_ascii_uppercase();
    let crockford = |c: char| c.is_ascii_digit() || (c.is_ascii_uppercase() && !"ILOU".contains(c));
    if suffix.len() < MIN || suffix.len() >= 26 || !suffix.chars().all(crockford) {
        return Err(ServiceError::Invalid(format!(
            "expected a {what} id: all 26 characters, or at least the last {MIN} (as lists show them), got {text:?}"
        )));
    }
    Ok(suffix)
}

/// The one id `found` holds (newest first, up to one more than is shown), or why not. `what`
/// is `event` or `run`.
pub(crate) fn one_id<T: std::fmt::Display>(
    found: Vec<T>,
    suffix: &str,
    what: &str,
) -> ServiceResult<T> {
    let count = found.len();
    let mut found = found.into_iter();
    match (found.next(), count) {
        // "recent": among the newest `SHORT_ID_WINDOW`, as the CLI reference says.
        (None, _) => Err(ServiceError::NotFound {
            what: if what == "run" { "recent run ending in" } else { "recent event ending in" },
            id: suffix.to_owned(),
        }),
        (Some(id), 1) => Ok(id),
        (Some(first), _) => {
            let candidates: Vec<String> =
                std::iter::once(first).chain(found).map(|id| id.to_string()).collect();
            let more = if candidates.len() > SHORT_ID_CANDIDATES { " and more" } else { "" };
            let candidates: Vec<String> =
                candidates.into_iter().take(SHORT_ID_CANDIDATES).collect();
            Err(ServiceError::Ambiguous {
                message: format!(
                    "{suffix} is the end of more than one {what} id: {}{more}; type more of it",
                    candidates.join(", ")
                ),
                candidates,
            })
        }
    }
}

/// Candidates an ambiguous short id lists; the lookup asks for one more to know there are more.
pub(crate) const SHORT_ID_CANDIDATES: usize = 10;

/// Cloneable handle to every service.
#[derive(Clone, Debug)]
pub struct Services {
    pub(crate) shared: Arc<Shared>,
}

/// A change to one definition in a config file.
pub(crate) enum Edit<'a, T> {
    /// Create it, or replace it whole (an editor's save, `add --file`).
    Put(&'a T),
    /// Change some fields of the definition as it is in the file (enable, archive): nothing
    /// else about it is touched, whatever the client last saw.
    Modify(&'a (dyn Fn(&mut T) + Sync)),
    Delete,
}

impl Services {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Services { shared }
    }

    /// Applies `change` to the definition `bare` (as written, without project prefix) in the
    /// user file or project `project`'s file. The file is read **once**; that text is checked
    /// against the project's trusted hash and against `expected`, edited, checked as a reload
    /// would check it, and written atomically. Serialized with reloads by the loader lock.
    pub(crate) async fn write_definition<T>(
        &self,
        kind: DefKind,
        project: Option<&str>,
        bare: &str,
        expected: &Expected,
        change: Edit<'_, T>,
    ) -> ServiceResult<()>
    where
        T: Serialize + DeserializeOwned + PartialEq,
    {
        let mut loader = self.shared.loader.lock().await;
        let (origin, path, trusted) = loader.target(project)?;
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                return Err(ServiceError::Internal(format!("cannot read {}: {e}", path.display())));
            }
        };
        Loader::check_trusted(&origin, trusted.as_deref(), &text)?;
        let edit_error = |e: edit::EditError| match e {
            edit::EditError::NotFound => ServiceError::not_found(kind.array(), bare),
            other => ServiceError::config(format!("{}: {other}", path.display())),
        };
        let current = edit::find::<T>(&text, kind, bare).map_err(edit_error)?;
        check_expected(kind, bare, expected, current.as_ref().map(|c| c.revision.as_str()))?;
        let new_text = match change {
            Edit::Put(def) => edit::upsert(&text, kind, bare, def),
            Edit::Modify(f) => {
                let found = current.ok_or_else(|| ServiceError::not_found(kind.array(), bare))?;
                let mut def = found.def.ok_or_else(|| {
                    ServiceError::config(format!(
                        "{}: the {} {bare:?} is not valid as written; fix it in the file first",
                        path.display(),
                        kind.array()
                    ))
                })?;
                f(&mut def);
                edit::upsert(&text, kind, bare, &def)
            }
            Edit::Delete => edit::remove(&text, kind, bare),
        }
        .map_err(edit_error)?;
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
        // Binds trust to exactly the text written, which was derived from text that had the
        // trusted hash.
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

/// Refuses an edit whose base is not the definition as it is now (`current`: its revision,
/// `None` when absent).
fn check_expected(
    kind: DefKind,
    name: &str,
    expected: &Expected,
    current: Option<&str>,
) -> ServiceResult<()> {
    let what = kind.array();
    let message = match (expected, current) {
        (Expected::Any, _) | (Expected::Absent, None) => return Ok(()),
        (Expected::Revision { revision }, Some(c)) if revision == c => return Ok(()),
        (Expected::Absent, Some(_)) => format!("a {what} named {name:?} already exists"),
        (Expected::Revision { .. }, None) => {
            format!("the {what} {name:?} was removed since it was read")
        }
        (Expected::Revision { .. }, Some(_)) => {
            format!("the {what} {name:?} changed since it was read")
        }
    };
    Err(ServiceError::Stale { message, current: current.map(str::to_owned) })
}

/// The revision of a definition as written in the files a snapshot was built from.
pub(crate) fn written_revision<T: Clone + Serialize>(
    files: &[Sourced<pulse_contracts::config::ConfigFile>],
    origin: &ConfigOrigin,
    pick: impl Fn(&pulse_contracts::config::ConfigFile) -> Option<&T>,
) -> String {
    as_written(files, origin, pick).map(|d| crate::config::revision(&d)).unwrap_or_default()
}

/// The as-written definition and its file, for a definition known by its full name.
pub(crate) fn as_written<T: Clone>(
    files: &[Sourced<pulse_contracts::config::ConfigFile>],
    origin: &ConfigOrigin,
    pick: impl Fn(&pulse_contracts::config::ConfigFile) -> Option<&T>,
) -> Option<T> {
    files.iter().find(|f| f.origin == *origin).and_then(|f| pick(&f.config).cloned())
}

/// A leading `~` (alone or before `/`) as the user's home directory, the way config paths are
/// read. Clients send what the user typed.
pub(crate) fn expand_tilde(s: &str, user_home: &std::path::Path) -> String {
    let home = user_home.display().to_string();
    if s == "~" {
        home
    } else if let Some(rest) = s.strip_prefix("~/") {
        format!("{}/{rest}", home.trim_end_matches('/'))
    } else {
        s.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_ids_are_suffixes_and_resolve_to_exactly_one() {
        assert_eq!(id_suffix(" 8d1 ", "run").unwrap(), "8D1");
        for bad in ["8D", "8DI", "has space", "", &"0".repeat(26)] {
            let err = id_suffix(bad, "run").unwrap_err().to_string();
            assert!(err.contains("at least the last 3"), "{bad:?}: {err}");
        }

        assert_eq!(one_id(vec!["01AAA8D1"], "8D1", "run").unwrap(), "01AAA8D1");
        let none = one_id(Vec::<String>::new(), "8D1", "run").unwrap_err();
        assert!(matches!(none, ServiceError::NotFound { .. }), "{none:?}");
        assert_eq!(none.to_string(), "recent run ending in \"8D1\" not found");

        let Err(ServiceError::Ambiguous { message, candidates }) =
            one_id(vec!["02B8D1", "01A8D1"], "8D1", "run")
        else {
            panic!("two matches are ambiguous");
        };
        assert_eq!(candidates, ["02B8D1", "01A8D1"]);
        assert!(message.contains("02B8D1, 01A8D1; type more"), "{message}");
        let many: Vec<String> = (0..=SHORT_ID_CANDIDATES).map(|n| format!("{n:02}8D1")).collect();
        let Err(ServiceError::Ambiguous { message, candidates }) = one_id(many, "8D1", "run")
        else {
            panic!("ambiguous");
        };
        assert_eq!(candidates.len(), SHORT_ID_CANDIDATES);
        assert!(message.contains("and more"), "{message}");
    }
}
