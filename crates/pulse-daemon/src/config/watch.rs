//! Watches the directories holding config files and signals when a `pulse.toml` in one of them
//! changes. Directories, not files, are watched: editors save by writing a new file and renaming
//! it over the old one, which a file watch would lose track of.
//!
//! A `pulse.toml` that is a symbolic link (into a dotfiles repository, say) is followed: the
//! directory of the file it points to is watched too, and changes to that file count. Links are
//! re-resolved on every [`ConfigWatcher::set_dirs`], which follows every reload, so pointing the
//! link elsewhere is picked up after the reload the change to the link itself causes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

pub(crate) struct ConfigWatcher {
    watcher: RecommendedWatcher,
    dirs: BTreeSet<PathBuf>,
    /// The files `pulse.toml` links resolve to, as the watcher reports their paths.
    targets: Arc<RwLock<BTreeSet<PathBuf>>>,
}

impl std::fmt::Debug for ConfigWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigWatcher").field("dirs", &self.dirs).finish()
    }
}

/// Whether an event can change a config file's content. Reading the file (which every reload
/// does) produces access events, and those must not trigger another reload.
fn is_relevant(ev: &notify::Event, targets: &BTreeSet<PathBuf>) -> bool {
    use notify::EventKind::{Any, Create, Modify, Other, Remove};
    use notify::event::ModifyKind;
    let kind = match ev.kind {
        Modify(ModifyKind::Metadata(_)) => false,
        Create(_) | Modify(_) | Remove(_) | Any | Other => true,
        notify::EventKind::Access(_) => false,
    };
    kind && ev
        .paths
        .iter()
        .any(|p| p.file_name().is_some_and(|n| n == "pulse.toml") || targets.contains(p))
}

impl ConfigWatcher {
    /// `changed` receives a signal (coalesced: the channel holds at most one) per change.
    pub(crate) fn new(changed: mpsc::Sender<()>) -> notify::Result<ConfigWatcher> {
        let targets = Arc::new(RwLock::new(BTreeSet::new()));
        let seen = targets.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let relevant = match res {
                Ok(ev) => is_relevant(&ev, &seen.read().unwrap_or_else(|p| p.into_inner())),
                // On an error (for example a queue overflow) reload to be safe.
                Err(_) => true,
            };
            if relevant {
                let _ = changed.try_send(());
            }
        })?;
        Ok(ConfigWatcher { watcher, dirs: BTreeSet::new(), targets })
    }

    /// Watches exactly `dirs`. Directories that cannot be watched (missing) are skipped; they
    /// are retried on the next call.
    pub(crate) fn set_dirs(&mut self, dirs: Vec<PathBuf>) {
        let mut targets: BTreeSet<PathBuf> = dirs.iter().filter_map(|d| link_target(d)).collect();
        let mut wanted: BTreeSet<PathBuf> = dirs.iter().cloned().collect();
        wanted.extend(targets.iter().filter_map(|t| t.parent().map(Path::to_path_buf)));
        // A target's directory may also be watched under another spelling (a link to a sibling
        // file, a home reached through a linked directory); one watch reports one spelling.
        let spellings: Vec<PathBuf> = dirs
            .iter()
            .filter_map(|d| {
                let real = std::fs::canonicalize(d).ok()?;
                let same = targets.iter().filter(|t| t.parent() == Some(real.as_path()));
                Some(same.filter_map(|t| Some(d.join(t.file_name()?))).collect::<Vec<_>>())
            })
            .flatten()
            .collect();
        targets.extend(spellings);
        *self.targets.write().unwrap_or_else(|p| p.into_inner()) = targets;
        for gone in self.dirs.difference(&wanted).cloned().collect::<Vec<_>>() {
            let _ = self.watcher.unwatch(&gone);
            self.dirs.remove(&gone);
        }
        for dir in wanted {
            if !self.dirs.contains(&dir) {
                match self.watcher.watch(&dir, RecursiveMode::NonRecursive) {
                    Ok(()) => {
                        self.dirs.insert(dir);
                    }
                    Err(e) => {
                        tracing::warn!("cannot watch {} for config changes: {e}", dir.display())
                    }
                }
            }
        }
    }
}

/// Where `dir/pulse.toml` points, when it is a symbolic link, in the form the watcher reports
/// for a watch on the target's directory.
fn link_target(dir: &Path) -> Option<PathBuf> {
    let file = dir.join("pulse.toml");
    if !std::fs::symlink_metadata(&file).ok()?.file_type().is_symlink() {
        return None;
    }
    let target = crate::home::resolve_links(&file).ok()?;
    // The watch is set on the canonical directory, so events name the canonical path. The
    // target file itself may not exist yet (a dangling link); its directory must.
    let parent = std::fs::canonicalize(target.parent()?).ok()?;
    Some(parent.join(target.file_name()?))
}

#[cfg(test)]
mod tests {
    use notify::event::{AccessKind, CreateKind, DataChange, MetadataKind, ModifyKind, RenameMode};
    use notify::{Event, EventKind};

    use super::*;

    #[test]
    fn only_content_changes_to_pulse_toml_count() {
        let cases = [
            (EventKind::Access(AccessKind::Any), "pulse.toml", false),
            (EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)), "pulse.toml", false),
            (EventKind::Modify(ModifyKind::Data(DataChange::Any)), "pulse.toml", true),
            (EventKind::Modify(ModifyKind::Name(RenameMode::To)), "pulse.toml", true),
            (EventKind::Create(CreateKind::File), "pulse.toml", true),
            (EventKind::Remove(notify::event::RemoveKind::File), "pulse.toml", true),
            (EventKind::Modify(ModifyKind::Data(DataChange::Any)), "pulse.db-wal", false),
        ];
        for (kind, name, want) in cases {
            let ev = Event::new(kind).add_path(PathBuf::from("/p").join(name));
            assert_eq!(is_relevant(&ev, &BTreeSet::new()), want, "{kind:?} {name}");
        }
    }

    /// A `pulse.toml` that links into a dotfiles repository: editing the file there reloads.
    #[tokio::test]
    async fn edits_to_a_linked_config_are_noticed() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let dotfiles = dir.path().join("dotfiles");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&dotfiles).unwrap();
        std::fs::write(dotfiles.join("pulse-config.toml"), "# old").unwrap();
        std::os::unix::fs::symlink("../dotfiles/pulse-config.toml", home.join("pulse.toml"))
            .unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        let mut watcher = ConfigWatcher::new(tx).unwrap();
        watcher.set_dirs(vec![home]);
        std::fs::write(dotfiles.join("pulse-config.toml"), "# new").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv())
            .await
            .expect("the edit is noticed")
            .unwrap();
    }
}
