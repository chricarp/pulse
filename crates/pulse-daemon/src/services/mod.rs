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

use std::path::PathBuf;
use std::sync::Arc;

use pulse_contracts::config::{ConfigOrigin, Sourced};
use serde::Serialize;
use serde::de::DeserializeOwned;

use pulse_contracts::rpc::Expected;

use crate::config::{DefKind, Loader, edit};
use crate::error::{ServiceError, ServiceResult};
use crate::home::{NoFollowError, NoFollowFile, write_atomic};
use crate::shared::Shared;

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
        let file = Target::open(&origin, path.clone())?;
        let (text, mode) = file.read()?;
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
        file.write(&new_text, mode)?;
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

/// The file write-back reads once and then replaces. The user's own `pulse.toml` follows
/// symbolic links (one kept in a dotfiles repository stays a link, see [`write_atomic`]). A
/// project's never does: it is read and replaced relative to its directory without following
/// any link ([`NoFollowFile`]), and one that is a link is refused, since someone who can write
/// the repository chose where it points.
enum Target {
    User(PathBuf),
    Project(NoFollowFile),
}

/// Permission bits a new file gets.
const NEW_FILE_MODE: u32 = 0o600;

impl Target {
    fn open(origin: &ConfigOrigin, path: PathBuf) -> ServiceResult<Target> {
        let Some(dir) = origin.base_dir() else { return Ok(Target::User(path)) };
        NoFollowFile::open(dir, "pulse.toml").map(Target::Project).map_err(no_follow_error)
    }

    /// The text (empty if there is no file) and its permission bits.
    fn read(&self) -> ServiceResult<(String, u32)> {
        match self {
            Target::User(path) => match std::fs::read_to_string(path) {
                Ok(t) => {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = std::fs::metadata(path)
                        .map_or(NEW_FILE_MODE, |m| m.permissions().mode() & 0o777);
                    Ok((t, mode))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    Ok((String::new(), NEW_FILE_MODE))
                }
                Err(e) => {
                    Err(ServiceError::Internal(format!("cannot read {}: {e}", path.display())))
                }
            },
            Target::Project(file) => match file.read(crate::config::MAX_PROJECT_FILE as u64) {
                Ok(Some(read)) => Ok(read),
                Ok(None) => Ok((String::new(), NEW_FILE_MODE)),
                Err(e) => Err(no_follow_error(e)),
            },
        }
    }

    fn write(&self, text: &str, mode: u32) -> ServiceResult<()> {
        match self {
            Target::User(path) => write_atomic(path, text.as_bytes(), mode).map_err(|e| {
                ServiceError::Internal(format!("cannot write {}: {e}", path.display()))
            }),
            Target::Project(file) => file.replace(text.as_bytes(), mode).map_err(no_follow_error),
        }
    }
}

fn no_follow_error(e: NoFollowError) -> ServiceError {
    match e {
        NoFollowError::Link(path) => {
            let dir = path.parent().unwrap_or(&path).display().to_string();
            ServiceError::Conflict(format!(
                "{} is a symbolic link (or a directory on its path is). Pulse reads and trusts a \
                 project file through a link, but never writes through one, because whoever can \
                 write the repository chooses where it points. Edit the file by hand, then trust \
                 it again (`pulse project trust {dir}`).",
                path.display()
            ))
        }
        NoFollowError::NotAFile(_) | NoFollowError::TooLarge(..) => {
            ServiceError::Conflict(e.to_string())
        }
        NoFollowError::Io(..) => ServiceError::Internal(e.to_string()),
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
