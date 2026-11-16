//! Watches the directories holding config files and signals when a `pulse.toml` in one of them
//! changes. Directories, not files, are watched: editors save by writing a new file and renaming
//! it over the old one, which a file watch would lose track of.

use std::collections::BTreeSet;
use std::path::PathBuf;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

pub(crate) struct ConfigWatcher {
    watcher: RecommendedWatcher,
    dirs: BTreeSet<PathBuf>,
}

impl std::fmt::Debug for ConfigWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigWatcher").field("dirs", &self.dirs).finish()
    }
}

impl ConfigWatcher {
    /// `changed` receives a signal (coalesced: the channel holds at most one) per change.
    pub(crate) fn new(changed: mpsc::Sender<()>) -> notify::Result<ConfigWatcher> {
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let relevant = match res {
                Ok(ev) => ev
                    .paths
                    .iter()
                    .any(|p| p.file_name().is_some_and(|n| n == "pulse.toml")),
                // On an error (for example a queue overflow) reload to be safe.
                Err(_) => true,
            };
            if relevant {
                let _ = changed.try_send(());
            }
        })?;
        Ok(ConfigWatcher { watcher, dirs: BTreeSet::new() })
    }

    /// Watches exactly `dirs`. Directories that cannot be watched (missing) are skipped; they
    /// are retried on the next call.
    pub(crate) fn set_dirs(&mut self, dirs: Vec<PathBuf>) {
        let wanted: BTreeSet<PathBuf> = dirs.into_iter().collect();
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
                    Err(e) => tracing::warn!("cannot watch {} for config changes: {e}", dir.display()),
                }
            }
        }
    }
}
