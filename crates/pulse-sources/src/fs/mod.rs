//! Filesystem watching: `fs.file.created`, `.modified`, `.deleted`, `.moved` and `fs.burst`.
//!
//! Three layers:
//! - [`platform`]: `notify` events to observations, the only place inotify and FSEvents differ.
//! - [`coalesce`]: a pure state machine from observations to settled changes (quiet periods,
//!   atomic saves, rename pairing, storm folding), in bounded memory.
//! - this module: the I/O around them. It owns the watcher, the per-directory watches (Linux),
//!   directory scans, the root's presence, health, and the bounded hand-off to the engine.
//!
//! Paths in events are clean absolute paths that start with the configured root as written
//! (after `~` expansion). Symlinks are not followed: a symlink inside the tree is reported as a
//! file (its own creation, removal or replacement), and a symlinked directory is not descended.
//! A root that is itself a symlink is watched through it, and events still use the configured
//! path, so they match the scopes the user wrote. See `docs/internals/0005-sources.md`.

mod coalesce;
mod ignore;
mod platform;

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use notify::{RecursiveMode, Watcher};
use pulse_contracts::catalog::{BuiltinEvent, FsBurstData, FsFileData, FsMovedData};
use pulse_contracts::config::FsSourceSettings;
use pulse_contracts::{Duration, NewEvent, SourceKind, SourceRef, Timestamp};
use tokio::sync::mpsc;

use self::coalesce::{Action, Change, Coalescer, Footprint, Settings, Settled, Signal};
use self::ignore::Ignore;
use self::platform::{Backend, FileKind, Normalizer, Probe, Raw};
use crate::clock::{Clock, SystemClock};
use crate::{BoxFuture, Source, SourceContext, SourceEnv, SourceError};

const DEFAULT_QUIET: Duration = Duration::from_secs(2);
const DEFAULT_BURST_THRESHOLD: u32 = 200;
/// Raw `notify` events buffered between the watcher thread and the source task. The watcher
/// thread never blocks on us: when this is full we note an overflow and rescan.
const RAW_CAPACITY: usize = 8192;
/// Settled events waiting for room in the engine's channel. While full, settled changes stay in
/// the coalescer, where repeated changes keep merging.
const OUTBOX_CAPACITY: usize = 256;
/// Directories queued for scanning; beyond this a scan is incomplete (and says so).
const MAX_SCAN_QUEUE: usize = 1_000_000;

#[derive(Debug)]
pub struct FsSource {
    source: SourceRef,
    root: String,
    ignore: Vec<String>,
    recursive: bool,
    home: PathBuf,
    settings: Settings,
    clock: Arc<dyn Clock>,
    probe: Option<Arc<std::sync::Mutex<Footprint>>>,
}

impl FsSource {
    pub fn new(
        name: &str,
        settings: &FsSourceSettings,
        env: &SourceEnv,
    ) -> Result<Self, SourceError> {
        let root = pulse_contracts::pattern::expand_path(&settings.path, &env.home, None)
            .map_err(|e| SourceError::Config(format!("path: {e}")))?;
        Ignore::new(&root, &settings.ignore, &env.home).map_err(SourceError::Config)?;
        let quiet = settings.quiet.unwrap_or(DEFAULT_QUIET);
        if quiet.is_zero() {
            return Err(SourceError::Config("quiet must be positive".into()));
        }
        let threshold = settings.burst_threshold.unwrap_or(DEFAULT_BURST_THRESHOLD);
        Ok(FsSource {
            source: SourceRef::new(SourceKind::Fs, name),
            root,
            ignore: settings.ignore.clone(),
            recursive: settings.recursive,
            home: env.home.clone(),
            settings: Settings::new(quiet, threshold as usize),
            clock: Arc::new(SystemClock),
            probe: None,
        })
    }

    /// Records the coalescer's high-water marks into `probe` (tests and the manual check).
    #[doc(hidden)]
    pub fn with_footprint_probe(mut self, probe: Arc<std::sync::Mutex<FsFootprint>>) -> Self {
        self.probe = Some(probe);
        self
    }
}

/// The coalescer's peak sizes while a source ran.
pub use self::coalesce::Footprint as FsFootprint;

impl Source for FsSource {
    fn source_ref(&self) -> &SourceRef {
        &self.source
    }

    fn run(self: Box<Self>, cx: SourceContext) -> BoxFuture<'static, Result<(), SourceError>> {
        Box::pin(async move {
            let result = run(*self, &cx).await;
            match &result {
                Ok(()) => cx.health.stopped(),
                Err(e) => cx.health.failed(e.to_string()),
            }
            result
        })
    }
}

#[derive(Clone, Debug)]
enum ScanMode {
    /// Startup: files only teach the coalescer what exists.
    Initial,
    /// A new or moved-in directory: its files are new (unless known).
    Appeared,
    /// A directory that moved within the tree; `from` is this directory's old path.
    Moved { from: String },
    /// After lost events: unknown files are new, known ones changed if written since `since`.
    Resync { since: std::time::SystemTime },
}

#[derive(Debug)]
struct ScanJob {
    dir: String,
    mode: ScanMode,
}

enum Presence {
    Watching,
    /// The root is missing (or not a directory); `anchor` is the nearest existing ancestor we
    /// watch to notice it coming back.
    Waiting {
        anchor: Option<String>,
    },
}

struct Fs<'a, W> {
    cx: &'a SourceContext,
    source: SourceRef,
    root: String,
    backend: Backend,
    watcher: W,
    normalizer: Normalizer,
    coalescer: Coalescer,
    clock: Arc<dyn Clock>,
    presence: Presence,
    /// Directories with a watch (Linux), bounded by the kernel's watch limit.
    watched: BTreeSet<String>,
    scan_queue: VecDeque<ScanJob>,
    /// What to report once the current scan finishes, if it is the initial scan or a resync.
    scanning: bool,
    watch_limit_hit: usize,
    outbox: VecDeque<NewEvent>,
    next_poll: Option<Timestamp>,
    last_raw: std::time::SystemTime,
    overflow: Arc<AtomicBool>,
    probe: Option<Arc<std::sync::Mutex<Footprint>>>,
}

type RawRx = mpsc::Receiver<notify::Result<notify::Event>>;

async fn run(src: FsSource, cx: &SourceContext) -> Result<(), SourceError> {
    let (tx, mut rx) = mpsc::channel::<notify::Result<notify::Event>>(RAW_CAPACITY);
    let overflow = Arc::new(AtomicBool::new(false));
    let flag = overflow.clone();
    let watcher = notify::recommended_watcher(move |res| {
        if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send(res) {
            flag.store(true, Ordering::Relaxed);
        }
    })
    .map_err(|e| SourceError::Platform(watcher_error(&e)))?;
    drive(src, cx, watcher, Backend::current(), &mut rx, overflow).await
}

/// The source's loop, over any watcher and raw-event channel (tests inject both).
async fn drive<W: Watcher>(
    src: FsSource,
    cx: &SourceContext,
    watcher: W,
    backend: Backend,
    rx: &mut RawRx,
    overflow: Arc<AtomicBool>,
) -> Result<(), SourceError> {
    let ignore = Ignore::new(&src.root, &src.ignore, &src.home)
        .map_err(SourceError::Config)?
        .recursive(src.recursive);
    let mut fs = Fs {
        cx,
        source: src.source.clone(),
        root: src.root.clone(),
        backend,
        watcher,
        normalizer: Normalizer::new(backend, &src.root, None),
        coalescer: Coalescer::new(&src.root, ignore, src.settings),
        clock: src.clock,
        presence: Presence::Waiting { anchor: None },
        watched: BTreeSet::new(),
        scan_queue: VecDeque::new(),
        scanning: false,
        watch_limit_hit: 0,
        outbox: VecDeque::new(),
        next_poll: None,
        last_raw: std::time::SystemTime::now(),
        overflow,
        probe: src.probe,
    };
    fs.check_root(ScanMode::Initial)?;
    loop {
        fs.record_footprint();
        let sleep = fs.next_poll.map(|at| {
            let wait = at.since(fs.clock.now()).to_std();
            tokio::time::sleep(wait)
        });
        tokio::select! {
            biased;
            () = cx.shutdown.cancelled() => return Ok(()),
            msg = rx.recv() => {
                let Some(msg) = msg else {
                    return Err(SourceError::Platform("the file watcher stopped".into()));
                };
                fs.handle(msg)?;
                // Drain what is already queued before doing anything slower.
                while let Ok(msg) = rx.try_recv() {
                    fs.handle(msg)?;
                }
                if fs.overflow.swap(false, Ordering::Relaxed) {
                    fs.resync("events arrived faster than they could be read");
                }
                fs.after_signals();
            }
            permit = cx.events.reserve(), if !fs.outbox.is_empty() => {
                let permit = permit?;
                if let Some(ev) = fs.outbox.pop_front() {
                    permit.send(ev);
                }
                if fs.outbox.len() < OUTBOX_CAPACITY / 2 {
                    fs.poll();
                }
            }
            () = async { if let Some(s) = sleep { s.await } }, if fs.next_poll.is_some() => fs.poll(),
            () = std::future::ready(()), if !fs.scan_queue.is_empty() => fs.scan_step(),
        }
    }
}

fn watcher_error(e: &notify::Error) -> String {
    match &e.kind {
        notify::ErrorKind::Io(io) if io.raw_os_error() == Some(24) => {
            "cannot create a file watcher: too many inotify instances. Raise fs.inotify.max_user_instances (for example `sudo sysctl fs.inotify.max_user_instances=512`)".into()
        }
        _ => format!("cannot create a file watcher: {e}"),
    }
}

impl Probe for BTreeSet<String> {
    fn kind(&self, path: &str) -> Option<FileKind> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        Some(if meta.is_dir() { FileKind::Dir } else { FileKind::File })
    }

    fn was_watched_dir(&self, path: &str) -> bool {
        self.contains(path)
    }
}

impl<W: Watcher> Fs<'_, W> {
    fn record_footprint(&self) {
        if let Some(p) = &self.probe {
            if let Ok(mut peak) = p.lock() {
                *peak = peak.max(self.coalescer.footprint());
            }
        }
    }

    /// Checks the root and starts watching it or waiting for it.
    fn check_root(&mut self, mode: ScanMode) -> Result<(), SourceError> {
        match std::fs::metadata(&self.root) {
            Ok(m) if m.is_dir() => {
                if let Err(e) = std::fs::read_dir(&self.root) {
                    return Err(SourceError::Io {
                        context: format!("cannot read {}", self.root),
                        source: e,
                    });
                }
                self.unwatch_anchor();
                self.start_watching(mode)
            }
            Ok(_) => {
                self.wait(format!(
                    "{} is not a directory; waiting for a directory there",
                    self.root
                ));
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.wait(format!("{} does not exist; waiting for it to appear", self.root));
                Ok(())
            }
            Err(e) => {
                Err(SourceError::Io { context: format!("cannot read {}", self.root), source: e })
            }
        }
    }

    fn start_watching(&mut self, mode: ScanMode) -> Result<(), SourceError> {
        self.presence = Presence::Watching;
        let canonical =
            std::fs::canonicalize(&self.root).ok().and_then(|p| p.to_str().map(str::to_owned));
        self.normalizer = Normalizer::new(self.backend, &self.root, canonical);
        if !self.backend.watches_per_directory() {
            self.watcher
                .watch(Path::new(&self.root), RecursiveMode::Recursive)
                .map_err(|e| SourceError::Platform(format!("cannot watch {}: {e}", self.root)))?;
        }
        self.scanning = true;
        self.scan_queue.push_back(ScanJob { dir: self.root.clone(), mode });
        Ok(())
    }

    fn wait(&mut self, message: String) {
        self.cx.health.degraded(message);
        self.stop_watching();
        let mut anchor = Path::new(&self.root).parent();
        while let Some(a) = anchor {
            if a.is_dir() && self.watcher.watch(a, RecursiveMode::NonRecursive).is_ok() {
                break;
            }
            anchor = a.parent();
        }
        let anchor = anchor.and_then(|a| a.to_str()).map(str::to_owned);
        self.presence = Presence::Waiting { anchor };
    }

    fn unwatch_anchor(&mut self) {
        if let Presence::Waiting { anchor: Some(a) } = &self.presence {
            let _ = self.watcher.unwatch(Path::new(a));
        }
        self.presence = Presence::Waiting { anchor: None };
    }

    fn stop_watching(&mut self) {
        if self.backend.watches_per_directory() {
            for d in std::mem::take(&mut self.watched) {
                let _ = self.watcher.unwatch(Path::new(&d));
            }
        } else if matches!(self.presence, Presence::Watching) {
            let _ = self.watcher.unwatch(Path::new(&self.root));
        }
        self.scan_queue.clear();
        self.scanning = false;
    }

    fn handle(&mut self, msg: notify::Result<notify::Event>) -> Result<(), SourceError> {
        let ev = match msg {
            Ok(ev) => ev,
            Err(e) => {
                self.cx.health.error(format!("file watcher error under {}: {e}", self.root));
                return Ok(());
            }
        };
        if let Presence::Waiting { .. } = self.presence {
            // Anything near the root may be the root coming back; check rather than interpret.
            let anchor_changed = match &self.presence {
                Presence::Waiting { anchor: Some(a) } => !Path::new(a).is_dir(),
                _ => true,
            };
            if Path::new(&self.root).is_dir() {
                return self.check_root(ScanMode::Appeared);
            }
            if anchor_changed {
                self.unwatch_anchor();
                self.wait(format!("{} does not exist; waiting for it to appear", self.root));
            }
            return Ok(());
        }
        self.last_raw = std::time::SystemTime::now();
        let now = self.clock.now();
        let raws = self.normalizer.normalize(&ev, &self.watched);
        for raw in raws {
            match raw {
                Raw::Signal(s) => self.coalescer.push(s, now),
                Raw::DirCreated(d) => {
                    if self.coalescer.descends() && !self.coalescer.is_ignored(&d) {
                        self.enqueue(ScanJob { dir: d, mode: ScanMode::Appeared });
                    }
                }
                Raw::DirGone(d) => self.forget_watches(&d),
                Raw::Rescan => self.resync("the operating system dropped file events"),
                Raw::RootChanged => {
                    if !Path::new(&self.root).is_dir() {
                        self.wait(format!(
                            "{} was removed or moved away; waiting for it to come back",
                            self.root
                        ));
                        return Ok(());
                    }
                }
            }
        }
        self.apply_actions();
        Ok(())
    }

    fn apply_actions(&mut self) {
        for action in self.coalescer.take_actions() {
            if !self.coalescer.descends() {
                continue;
            }
            match action {
                Action::ScanAppeared(dir) => {
                    self.enqueue(ScanJob { dir, mode: ScanMode::Appeared })
                }
                Action::ScanMoved { from, to } => {
                    self.forget_watches(&from);
                    self.enqueue(ScanJob { dir: to, mode: ScanMode::Moved { from } });
                }
            }
        }
    }

    fn after_signals(&mut self) {
        // A held rename half is the earliest a new deadline can be.
        let soon = self.clock.now().saturating_add(Duration::from_millis(100));
        self.next_poll = Some(self.next_poll.map_or(soon, |n| n.min(soon)));
    }

    fn forget_watches(&mut self, dir: &str) {
        let prefix = format!("{dir}/");
        let gone: Vec<String> = self
            .watched
            .range(dir.to_owned()..)
            .take_while(|d| *d == dir || d.starts_with(&prefix))
            .cloned()
            .collect();
        for d in gone {
            // notify drops watches for removed and moved-away directories itself; this only
            // keeps our record (and the watch, if notify kept it) in step.
            let _ = self.watcher.unwatch(Path::new(&d));
            self.watched.remove(&d);
        }
    }

    fn enqueue(&mut self, job: ScanJob) {
        if self.scan_queue.len() < MAX_SCAN_QUEUE {
            self.scan_queue.push_back(job);
        } else {
            self.cx
                .health
                .error(format!("too many directories under {}; some were not scanned", self.root));
        }
    }

    fn resync(&mut self, why: &str) {
        self.cx.health.degraded(format!(
            "{why}; rescanning {}. Changes made while events were lost are reported as one batch, and deletions in that time are not seen",
            self.root
        ));
        let since = self
            .last_raw
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap_or(std::time::UNIX_EPOCH);
        self.scanning = true;
        self.enqueue(ScanJob { dir: self.root.clone(), mode: ScanMode::Resync { since } });
    }

    /// Scans one directory: watches it (Linux), then lists it, so nothing created in between is
    /// missed (it shows up in the listing, the watch, or both).
    fn scan_step(&mut self) {
        let Some(job) = self.scan_queue.pop_front() else { return };
        if self.backend.watches_per_directory() && !self.watched.contains(&job.dir) {
            self.watch_dir(&job.dir);
        }
        let entries = match std::fs::read_dir(&job.dir) {
            Ok(e) => e,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    self.cx.health.error(format!("cannot read {}: {e}", job.dir));
                }
                return self.scan_done_check();
            }
        };
        let now = self.clock.now();
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                self.normalizer.unusable_paths += 1;
                continue;
            };
            let path =
                if job.dir == "/" { format!("/{name}") } else { format!("{}/{name}", job.dir) };
            if self.coalescer.is_ignored(&path) {
                continue;
            }
            // `file_type` does not follow symlinks: a symlinked directory is a file here.
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                if !self.coalescer.descends() {
                    continue;
                }
                let mode = match &job.mode {
                    ScanMode::Moved { from } => ScanMode::Moved { from: format!("{from}/{name}") },
                    m => m.clone(),
                };
                self.enqueue(ScanJob { dir: path, mode });
                continue;
            }
            match &job.mode {
                ScanMode::Initial => self.coalescer.note_existing(&path),
                ScanMode::Appeared => {
                    self.coalescer.push(Signal::Scanned { path, recent: false }, now)
                }
                ScanMode::Moved { from } => {
                    let from = format!("{from}/{name}");
                    self.coalescer.push(Signal::Moved { from, to: path }, now);
                }
                ScanMode::Resync { since } => {
                    let recent =
                        entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| t >= *since);
                    self.coalescer.push(Signal::Scanned { path, recent }, now);
                }
            }
        }
        self.apply_actions();
        self.after_signals();
        self.scan_done_check();
    }

    fn scan_done_check(&mut self) {
        if !self.scan_queue.is_empty() || !self.scanning {
            return;
        }
        self.scanning = false;
        if self.watch_limit_hit > 0 {
            let limit = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
                .map(|s| format!(" (currently {})", s.trim()))
                .unwrap_or_default();
            self.cx.health.degraded(format!(
                "the inotify watch limit{limit} is reached: {} directories under {} are not watched, so changes there are missed. Raise fs.inotify.max_user_watches (for example `sudo sysctl fs.inotify.max_user_watches=524288`) or ignore large directories, then restart this source",
                self.watch_limit_hit, self.root
            ));
        } else {
            self.cx.health.healthy();
        }
    }

    fn watch_dir(&mut self, dir: &str) {
        if self.watch_limit_hit > 0 {
            self.watch_limit_hit += 1;
            return;
        }
        match self.watcher.watch(Path::new(dir), RecursiveMode::NonRecursive) {
            Ok(()) => {
                self.watched.insert(dir.to_owned());
            }
            Err(e) if matches!(e.kind, notify::ErrorKind::MaxFilesWatch) => {
                self.watch_limit_hit = 1;
            }
            Err(e) if matches!(e.kind, notify::ErrorKind::PathNotFound) => {}
            Err(e) => self.cx.health.error(format!("cannot watch {dir}: {e}")),
        }
    }

    fn poll(&mut self) {
        let room = OUTBOX_CAPACITY.saturating_sub(self.outbox.len());
        let now = self.clock.now();
        let (settled, next) = self.coalescer.poll(now, room);
        self.next_poll = next;
        for s in settled {
            if let Some(ev) = to_event(&self.source, s) {
                self.outbox.push_back(ev);
            }
        }
        if room == 0 {
            // Wait for the sink rather than spin: sending re-polls once there is room.
            self.next_poll = None;
        }
    }
}

fn file_parts(path: &str) -> (String, String, Option<String>) {
    let (dir, name) = match path.rfind('/') {
        Some(0) => ("/", &path[1..]),
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("/", path),
    };
    let ext = name
        .rfind('.')
        .filter(|&i| i > 0 && i + 1 < name.len())
        .map(|i| name[i + 1..].to_lowercase());
    (dir.to_owned(), name.to_owned(), ext)
}

fn size_of(path: &str) -> Option<Option<u64>> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => None,
        Ok(m) if m.is_file() => Some(Some(m.len())),
        _ => Some(None),
    }
}

fn file_data(path: String, size: Option<u64>) -> FsFileData {
    let (dir, name, ext) = file_parts(&path);
    FsFileData { path, dir, name, ext, size }
}

fn to_event(source: &SourceRef, s: Settled) -> Option<NewEvent> {
    let ev = match s.change {
        Change::Created(p) => BuiltinEvent::FsFileCreated(file_data(p.clone(), size_of(&p)?)),
        Change::Modified(p) => BuiltinEvent::FsFileModified(file_data(p.clone(), size_of(&p)?)),
        Change::Deleted(p) => BuiltinEvent::FsFileDeleted(file_data(p, None)),
        Change::Moved { from, to } => {
            let size = size_of(&to)?;
            let (dir, name, ext) = file_parts(&to);
            BuiltinEvent::FsFileMoved(FsMovedData { from, path: to, dir, name, ext, size })
        }
        Change::Burst(b) => BuiltinEvent::FsBurst(FsBurstData {
            dir: b.dir,
            count: b.created + b.modified + b.deleted,
            created: b.created,
            modified: b.modified,
            deleted: b.deleted,
            sample: b.sample,
        }),
    };
    let mut ev = ev.into_new_event(source.clone(), s.at);
    ev.coalesced = Some(s.coalesced);
    Some(ev)
}

#[cfg(test)]
mod tests;
