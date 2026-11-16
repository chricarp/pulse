//! Platform differences, confined to translating `notify` events into [`Raw`] observations.
//!
//! **inotify (Linux)** reports precise operations per watched directory, with a cookie pairing
//! the two halves of a rename. We watch each directory ourselves (non-recursively) so ignored
//! directories cost no watches; directory creation, removal and moves are therefore our job and
//! are surfaced as separate observations. `notify` drops its watches for a directory that is
//! removed or moved away; we drop our record of them at the same time.
//!
//! **FSEvents (macOS)** watches a tree by path, batches reports, and merges flags: a file created,
//! written and deleted within the batch latency can arrive as one report with all three flags,
//! and history flags can linger. We do not trust the kind. Every report becomes "something
//! happened here" plus what a stat says now ([`Signal::Changed`]); the coalescer's index of known
//! files decides created versus modified. Rename reports carry no cookie; whether the path exists
//! says which half it is. Paths are canonical (`/private/var/…` for `/var/…`), so they are mapped
//! back to the configured root.
//!
//! The translation is pure given a [`Probe`] (stat and "was this a watched directory"), so it is
//! tested on Linux with synthetic events for both backends.

use notify::event::{CreateKind, EventKind, ModifyKind, RemoveKind, RenameMode};

use super::coalesce::Signal;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Backend {
    /// One watch per directory; precise operations.
    Inotify,
    /// One recursive watch on the root; coarse, merged reports.
    FsEvents,
}

impl Backend {
    pub(crate) fn current() -> Self {
        if cfg!(target_os = "macos") { Backend::FsEvents } else { Backend::Inotify }
    }

    pub(crate) fn watches_per_directory(self) -> bool {
        self == Backend::Inotify
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileKind {
    /// Anything that is not a directory, including symlinks (never followed).
    File,
    Dir,
}

/// What normalization may ask about the filesystem.
pub(crate) trait Probe {
    fn kind(&self, path: &str) -> Option<FileKind>;
    /// Whether `path` was a directory we watched (it may be gone already).
    fn was_watched_dir(&self, path: &str) -> bool;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Raw {
    Signal(Signal),
    /// Watch and scan it.
    DirCreated(String),
    /// Removed or moved away: forget the watches under it.
    DirGone(String),
    /// Events were lost; rescan.
    Rescan,
    /// The root itself was removed, moved or replaced.
    RootChanged,
}

#[derive(Debug)]
pub(crate) struct Normalizer {
    backend: Backend,
    root: String,
    canonical_root: Option<String>,
    /// FSEvents: the path of the last rename report. Other reports for it from the same batch
    /// are merged flags of the rename and would break the pairing.
    last_renamed: Option<String>,
    /// Paths dropped because they are not UTF-8 or not clean.
    pub(crate) unusable_paths: u64,
}

impl Normalizer {
    pub(crate) fn new(backend: Backend, root: &str, canonical_root: Option<String>) -> Self {
        let canonical_root = canonical_root.filter(|c| c != root);
        Normalizer {
            backend,
            root: root.to_owned(),
            canonical_root,
            last_renamed: None,
            unusable_paths: 0,
        }
    }

    pub(crate) fn normalize(&mut self, ev: &notify::Event, probe: &dyn Probe) -> Vec<Raw> {
        let mut out = Vec::new();
        if ev.need_rescan() {
            out.push(Raw::Rescan);
        }
        for p in &ev.paths {
            let Some(path) = self.path(p) else { continue };
            match self.backend {
                Backend::Inotify => self.inotify(ev, path, probe, &mut out),
                Backend::FsEvents => self.fsevents(ev, path, probe, &mut out),
            }
        }
        out
    }

    fn path(&mut self, p: &std::path::Path) -> Option<String> {
        let Some(s) = p.to_str() else {
            self.unusable_paths += 1;
            return None;
        };
        let mapped;
        let s = match &self.canonical_root {
            Some(c) => match s.strip_prefix(c.as_str()) {
                Some(rest) if rest.is_empty() || rest.starts_with('/') => {
                    mapped = format!("{}{rest}", self.root);
                    mapped.as_str()
                }
                _ => s,
            },
            None => s,
        };
        let Some(clean) = pulse_contracts::pattern::clean_absolute(s) else {
            self.unusable_paths += 1;
            return None;
        };
        let inside = clean == self.root
            || self.root == "/"
            || clean.strip_prefix(self.root.as_str()).is_some_and(|r| r.starts_with('/'));
        inside.then_some(clean)
    }

    fn inotify(&mut self, ev: &notify::Event, path: String, probe: &dyn Probe, out: &mut Vec<Raw>) {
        let is_root = path == self.root;
        let cookie = ev.tracker().map(|t| t as u64);
        match ev.kind {
            EventKind::Create(kind) if !is_root => {
                if kind == CreateKind::Folder || probe.kind(&path) == Some(FileKind::Dir) {
                    out.push(Raw::DirCreated(path));
                } else {
                    out.push(Raw::Signal(Signal::Created(path)));
                }
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                if is_root {
                    out.push(Raw::RootChanged);
                } else if cookie.is_some() {
                    // Without a cookie this is a watched directory's own MOVE_SELF; its parent
                    // reports the same rename with a cookie.
                    let dir = probe.was_watched_dir(&path);
                    if dir {
                        out.push(Raw::DirGone(path.clone()));
                    }
                    out.push(Raw::Signal(Signal::MovedFrom { path, cookie, dir }));
                }
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::To)) if !is_root => {
                let dir = probe.kind(&path) == Some(FileKind::Dir);
                out.push(Raw::Signal(Signal::MovedTo { path, cookie, dir }));
            }
            // `Both` repeats a pair we already got as halves; the coalescer pairs them.
            EventKind::Modify(ModifyKind::Name(_)) => {}
            EventKind::Modify(ModifyKind::Metadata(_)) => {}
            EventKind::Modify(_) if !is_root => out.push(Raw::Signal(Signal::Modified(path))),
            EventKind::Remove(kind) => {
                if is_root {
                    out.push(Raw::RootChanged);
                } else if kind == RemoveKind::Folder || probe.was_watched_dir(&path) {
                    out.push(Raw::DirGone(path));
                } else {
                    out.push(Raw::Signal(Signal::Removed(path)));
                }
            }
            _ => {}
        }
    }

    fn fsevents(
        &mut self,
        ev: &notify::Event,
        path: String,
        probe: &dyn Probe,
        out: &mut Vec<Raw>,
    ) {
        let kind = probe.kind(&path);
        if path == self.root {
            if kind != Some(FileKind::Dir) {
                out.push(Raw::RootChanged);
            }
            return;
        }
        match ev.kind {
            EventKind::Access(_)
            | EventKind::Modify(ModifyKind::Metadata(_))
            | EventKind::Other => {}
            EventKind::Modify(ModifyKind::Name(_)) => {
                self.last_renamed = Some(path.clone());
                out.push(Raw::Signal(match kind {
                    Some(k) => Signal::MovedTo { path, cookie: None, dir: k == FileKind::Dir },
                    None => Signal::MovedFrom { path, cookie: None, dir: false },
                }));
            }
            _ if self.last_renamed.as_deref() == Some(path.as_str()) => {}
            ref k => {
                self.last_renamed = None;
                match kind {
                    Some(FileKind::Dir) => {
                        if matches!(k, EventKind::Create(_)) {
                            out.push(Raw::DirCreated(path));
                        }
                    }
                    Some(FileKind::File) => {
                        out.push(Raw::Signal(Signal::Changed { path, exists: true }))
                    }
                    None if *k == EventKind::Remove(RemoveKind::Folder) => {}
                    None => out.push(Raw::Signal(Signal::Changed { path, exists: false })),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;

    use notify::event::{AccessKind, AccessMode, DataChange, Flag, MetadataKind};

    use super::*;

    #[derive(Default)]
    struct Fake {
        kinds: HashMap<String, FileKind>,
        watched: HashSet<String>,
    }

    impl Probe for Fake {
        fn kind(&self, path: &str) -> Option<FileKind> {
            self.kinds.get(path).copied()
        }
        fn was_watched_dir(&self, path: &str) -> bool {
            self.watched.contains(path)
        }
    }

    fn ev(kind: EventKind, path: &str) -> notify::Event {
        notify::Event::new(kind).add_path(PathBuf::from(path))
    }

    fn sig(s: Signal) -> Raw {
        Raw::Signal(s)
    }

    #[test]
    fn inotify_events() {
        let mut fake = Fake::default();
        fake.kinds.insert("/w/d".into(), FileKind::Dir);
        fake.kinds.insert("/w/b".into(), FileKind::File);
        fake.kinds.insert("/w/newdir".into(), FileKind::Dir);
        fake.watched.insert("/w/sub".into());
        let name = |m| EventKind::Modify(ModifyKind::Name(m));
        let cases: Vec<(notify::Event, Vec<Raw>)> = vec![
            (
                ev(EventKind::Create(CreateKind::File), "/w/a"),
                vec![sig(Signal::Created("/w/a".into()))],
            ),
            (
                ev(EventKind::Create(CreateKind::Folder), "/w/d"),
                vec![Raw::DirCreated("/w/d".into())],
            ),
            (
                ev(EventKind::Modify(ModifyKind::Data(DataChange::Any)), "/w/a"),
                vec![sig(Signal::Modified("/w/a".into()))],
            ),
            (ev(EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)), "/w/a"), vec![]),
            (ev(EventKind::Access(AccessKind::Close(AccessMode::Write)), "/w/a"), vec![]),
            (ev(EventKind::Access(AccessKind::Open(AccessMode::Any)), "/w/a"), vec![]),
            (
                ev(EventKind::Remove(RemoveKind::File), "/w/a"),
                vec![sig(Signal::Removed("/w/a".into()))],
            ),
            (
                ev(EventKind::Remove(RemoveKind::Folder), "/w/sub"),
                vec![Raw::DirGone("/w/sub".into())],
            ),
            (ev(EventKind::Remove(RemoveKind::Folder), "/w"), vec![Raw::RootChanged]),
            (ev(name(RenameMode::From), "/w").set_tracker(0), vec![Raw::RootChanged]),
            (
                ev(name(RenameMode::From), "/w/a").set_tracker(9),
                vec![sig(Signal::MovedFrom { path: "/w/a".into(), cookie: Some(9), dir: false })],
            ),
            (
                ev(name(RenameMode::From), "/w/sub").set_tracker(3),
                vec![
                    Raw::DirGone("/w/sub".into()),
                    sig(Signal::MovedFrom { path: "/w/sub".into(), cookie: Some(3), dir: true }),
                ],
            ),
            // A watched directory's MOVE_SELF has no cookie; the parent's report counts.
            (ev(name(RenameMode::From), "/w/sub"), vec![]),
            (
                ev(name(RenameMode::To), "/w/b").set_tracker(9),
                vec![sig(Signal::MovedTo { path: "/w/b".into(), cookie: Some(9), dir: false })],
            ),
            (
                ev(name(RenameMode::To), "/w/newdir").set_tracker(4),
                vec![sig(Signal::MovedTo { path: "/w/newdir".into(), cookie: Some(4), dir: true })],
            ),
            (
                notify::Event::new(name(RenameMode::Both))
                    .add_path("/w/a".into())
                    .add_path("/w/b".into())
                    .set_tracker(9),
                vec![],
            ),
            (notify::Event::new(EventKind::Other).set_flag(Flag::Rescan), vec![Raw::Rescan]),
            (ev(EventKind::Create(CreateKind::File), "/elsewhere/x"), vec![]),
            (ev(EventKind::Create(CreateKind::File), "/w/a/../../etc"), vec![]),
        ];
        let mut n = Normalizer::new(Backend::Inotify, "/w", None);
        for (e, want) in cases {
            assert_eq!(n.normalize(&e, &fake), want, "{e:?}");
        }
        assert_eq!(n.unusable_paths, 1);
    }

    #[test]
    fn fsevents_reports_are_coarse_and_canonical_paths_map_back() {
        let mut fake = Fake::default();
        fake.kinds.insert("/w".into(), FileKind::Dir);
        fake.kinds.insert("/w/exists".into(), FileKind::File);
        fake.kinds.insert("/w/new-name".into(), FileKind::File);
        fake.kinds.insert("/w/d".into(), FileKind::Dir);
        let mut n = Normalizer::new(Backend::FsEvents, "/w", Some("/private/w".into()));
        let changed = |p: &str, exists| sig(Signal::Changed { path: p.into(), exists });
        let cases: Vec<(notify::Event, Vec<Raw>)> = vec![
            // Merged flags: created+modified for a file that exists now, removed for one that
            // does not. The kind in the report does not matter, only the stat.
            (
                ev(EventKind::Create(CreateKind::File), "/private/w/exists"),
                vec![changed("/w/exists", true)],
            ),
            (
                ev(EventKind::Remove(RemoveKind::File), "/private/w/exists"),
                vec![changed("/w/exists", true)],
            ),
            (
                ev(EventKind::Create(CreateKind::File), "/private/w/gone"),
                vec![changed("/w/gone", false)],
            ),
            (
                ev(
                    EventKind::Modify(ModifyKind::Metadata(MetadataKind::Extended)),
                    "/private/w/exists",
                ),
                vec![],
            ),
            (
                ev(EventKind::Create(CreateKind::Folder), "/private/w/d"),
                vec![Raw::DirCreated("/w/d".into())],
            ),
            (ev(EventKind::Remove(RemoveKind::Folder), "/private/w/old-dir"), vec![]),
            // Rename halves: existence decides which is which; merged flags for the same path
            // right after a rename report are dropped.
            (
                ev(EventKind::Modify(ModifyKind::Name(RenameMode::Any)), "/private/w/old-name"),
                vec![sig(Signal::MovedFrom {
                    path: "/w/old-name".into(),
                    cookie: None,
                    dir: false,
                })],
            ),
            (ev(EventKind::Remove(RemoveKind::File), "/private/w/old-name"), vec![]),
            (
                ev(EventKind::Modify(ModifyKind::Name(RenameMode::Any)), "/private/w/new-name"),
                vec![sig(Signal::MovedTo { path: "/w/new-name".into(), cookie: None, dir: false })],
            ),
            // Paths outside the root as given also work (FSEvents reports either form).
            (
                ev(EventKind::Modify(ModifyKind::Data(DataChange::Content)), "/w/exists"),
                vec![changed("/w/exists", true)],
            ),
        ];
        for (e, want) in cases {
            assert_eq!(n.normalize(&e, &fake), want, "{e:?}");
        }
        fake.kinds.remove("/w");
        assert_eq!(
            n.normalize(
                &ev(EventKind::Modify(ModifyKind::Name(RenameMode::Any)), "/private/w"),
                &fake
            ),
            vec![Raw::RootChanged]
        );
    }
}
