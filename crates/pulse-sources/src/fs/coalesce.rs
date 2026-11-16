//! The filesystem coalescer: a pure state machine from raw signals to settled changes.
//!
//! Signals and timestamps go in ([`Coalescer::push`]); settled changes and the next deadline
//! come out ([`Coalescer::poll`]). It does no I/O. The async wrapper drives it with one timer.
//! The rules, in short (the reasoning is in `docs/internals/0005-sources.md`):
//!
//! - **Net effect per path.** A path's pending entry remembers whether the path existed when the
//!   window opened and whether it exists now. When the path has been quiet for `quiet` it settles
//!   into created, modified, deleted, moved, or nothing (created then deleted).
//! - **What existed before** comes from a bounded index of hashes of the files known to exist
//!   (filled by the initial scan, kept current by signals). It is what lets an atomic save
//!   (write a temp file, rename it over the target) read as a modification of the target, and
//!   what turns coarse "something happened here" reports (FSEvents) into a kind.
//! - **Renames** pair by the platform's cookie (inotify) or, without one (FSEvents), by
//!   adjacency within `pair_window`. A half that never pairs is a move out of (removal) or into
//!   (appearance) the tree. A rename from a file created in the same window, or from an ignored
//!   temp name, means "new content at the target", not a move.
//! - **Storms.** When pending entries pass `burst_threshold`, the busiest subtree (see
//!   [`Coalescer::plan`]) stops tracking paths and keeps counts and a small sample; it settles as
//!   one `fs.burst` once the subtree is quiet for `storm_quiet`. Entries outside it are untouched.
//! - **Bounds.** Pending entries, storms, per-storm exact counting, the index, held rename halves
//!   and held downloads all have caps. Past a cap the coalescer degrades (approximate counts, a
//!   storm at the root, unknown prior existence), never grows.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::hash::{BuildHasher, RandomState};

use pulse_contracts::{Duration, Timestamp};

use super::ignore::{Ignore, download_target};

#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub quiet: Duration,
    pub storm_quiet: Duration,
    pub burst_threshold: usize,
    /// A path or storm that never goes quiet is reported after this long anyway.
    pub max_hold: Duration,
    pub pair_window: Duration,
    pub index_capacity: usize,
    pub pending_capacity: usize,
    pub max_storms: usize,
    /// Paths counted exactly across all storms; past this, storms count signals instead.
    pub storm_exact_capacity: usize,
    pub sample_size: usize,
}

impl Settings {
    pub(crate) fn new(quiet: Duration, burst_threshold: usize) -> Self {
        let burst_threshold = burst_threshold.max(1);
        Settings {
            quiet,
            storm_quiet: Duration::from_millis(quiet.as_millis().saturating_mul(2)),
            burst_threshold,
            max_hold: Duration::from_mins(5),
            pair_window: Duration::from_millis(quiet.as_millis().min(500)),
            index_capacity: 200_000,
            pending_capacity: burst_threshold.saturating_mul(4).max(1024),
            max_storms: 16,
            storm_exact_capacity: 100_000,
            sample_size: 8,
        }
    }
}

/// A normalized raw observation. Paths are clean absolute paths under the root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Signal {
    /// The platform reported a file creation.
    Created(String),
    /// The platform reported a write.
    Modified(String),
    Removed(String),
    /// A coarse report (FSEvents): something happened here; `exists` is what a stat saw.
    Changed {
        path: String,
        exists: bool,
    },
    MovedFrom {
        path: String,
        cookie: Option<u64>,
        dir: bool,
    },
    MovedTo {
        path: String,
        cookie: Option<u64>,
        dir: bool,
    },
    /// A rename whose halves are already known (from scanning a moved directory).
    Moved {
        from: String,
        to: String,
    },
    /// A scan found this file. `recent` means its mtime is inside the window being resynced.
    Scanned {
        path: String,
        recent: bool,
    },
}

/// Directory work for the I/O layer, decided while pairing renames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// A directory appeared (moved in from outside): watch and scan it.
    ScanAppeared(String),
    /// A directory moved within the tree: watch and scan `to`, reporting files as moved.
    ScanMoved { from: String, to: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    Created(String),
    Modified(String),
    Deleted(String),
    Moved { from: String, to: String },
    Burst(Burst),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Burst {
    pub dir: String,
    pub created: u64,
    pub modified: u64,
    pub deleted: u64,
    pub sample: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settled {
    pub change: Change,
    /// When the last signal folded into it arrived.
    pub at: Timestamp,
    pub coalesced: u32,
}

/// What the coalescer holds: entries per path, storms, paths counted exactly inside storms,
/// the index of known files, held rename halves and held downloads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Footprint {
    pub pending: usize,
    pub storms: usize,
    pub storm_paths: usize,
    pub index: usize,
    pub halves: usize,
    pub downloads: usize,
}

impl Footprint {
    pub fn max(self, o: Footprint) -> Footprint {
        Footprint {
            pending: self.pending.max(o.pending),
            storms: self.storms.max(o.storms),
            storm_paths: self.storm_paths.max(o.storm_paths),
            index: self.index.max(o.index),
            halves: self.halves.max(o.halves),
            downloads: self.downloads.max(o.downloads),
        }
    }
}

/// The net effect on one path within the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Net {
    existed_before: bool,
    exists: bool,
    written: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Created,
    Modified,
    Deleted,
}

impl Net {
    fn new(existed_before: bool) -> Self {
        Net { existed_before, exists: existed_before, written: false }
    }

    fn present(mut self) -> Self {
        self.exists = true;
        self.written = true;
        self
    }

    fn gone(mut self) -> Self {
        self.exists = false;
        self
    }

    fn kind(self) -> Option<Kind> {
        match (self.existed_before, self.exists) {
            (false, true) => Some(Kind::Created),
            (true, true) if self.written => Some(Kind::Modified),
            (true, false) => Some(Kind::Deleted),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct Entry {
    net: Net,
    /// The content came from this path, which existed when the window opened.
    moved_from: Option<String>,
    first_at: Timestamp,
    last_at: Timestamp,
    signals: u32,
}

#[derive(Clone, Copy, Debug, Default)]
struct Counts {
    created: u64,
    modified: u64,
    deleted: u64,
}

impl Counts {
    fn add(&mut self, kind: Option<Kind>) {
        match kind {
            Some(Kind::Created) => self.created += 1,
            Some(Kind::Modified) => self.modified += 1,
            Some(Kind::Deleted) => self.deleted += 1,
            None => {}
        }
    }
}

#[derive(Debug)]
struct Storm {
    dir: String,
    started_at: Timestamp,
    last_at: Timestamp,
    signals: u32,
    /// Exact net effect per path, keyed by path hash, while the global budget allows.
    nets: HashMap<u64, Net>,
    /// Signals beyond the exact budget, counted by kind (an over-count for modifications).
    approx: Counts,
    /// Candidate sample paths, first seen first.
    sample: Vec<String>,
}

#[derive(Debug)]
struct Half {
    path: String,
    cookie: Option<u64>,
    dir: bool,
    to: bool,
    at: Timestamp,
}

/// A bounded set of hashes of files known to exist.
#[derive(Debug)]
struct Index {
    set: HashSet<u64>,
    capacity: usize,
    /// Set once an insert was refused: absence then means "unknown", not "did not exist".
    saturated: bool,
}

impl Index {
    fn known(&self, h: u64) -> Option<bool> {
        if self.set.contains(&h) {
            Some(true)
        } else if self.saturated {
            None
        } else {
            Some(false)
        }
    }

    fn insert(&mut self, h: u64) {
        if self.set.len() < self.capacity {
            self.set.insert(h);
        } else if !self.set.contains(&h) {
            self.saturated = true;
        }
    }

    fn remove(&mut self, h: u64) {
        self.set.remove(&h);
    }
}

#[derive(Debug)]
pub(crate) struct Coalescer {
    s: Settings,
    root: String,
    ignore: Ignore,
    hasher: RandomState,
    index: Index,
    pending: HashMap<String, Entry>,
    storms: Vec<Storm>,
    storm_paths: usize,
    halves: VecDeque<Half>,
    /// Targets of download temp files that currently exist (`a.pdf` for `a.pdf.part`).
    downloads: HashSet<String>,
    actions: Vec<Action>,
    /// Re-plan storms only when pending grows past this, so a scope that hovers just above the
    /// threshold does not re-plan on every signal.
    next_plan_at: usize,
}

const MAX_HALVES: usize = 64;
const MAX_DOWNLOADS: usize = 256;

impl Coalescer {
    pub(crate) fn new(root: &str, ignore: Ignore, s: Settings) -> Self {
        let index = Index { set: HashSet::new(), capacity: s.index_capacity, saturated: false };
        let next_plan_at = s.burst_threshold;
        Coalescer {
            s,
            root: root.to_owned(),
            ignore,
            hasher: RandomState::new(),
            index,
            pending: HashMap::new(),
            storms: Vec::new(),
            storm_paths: 0,
            halves: VecDeque::new(),
            downloads: HashSet::new(),
            actions: Vec::new(),
            next_plan_at,
        }
    }

    pub(crate) fn is_ignored(&self, path: &str) -> bool {
        self.ignore.is_ignored(path)
    }

    pub(crate) fn descends(&self) -> bool {
        self.ignore.descends()
    }

    /// The initial scan found an existing file. Not a change; it only teaches the index.
    pub(crate) fn note_existing(&mut self, path: &str) {
        let h = self.hash(path);
        self.index.insert(h);
    }

    pub(crate) fn take_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.actions)
    }

    pub(crate) fn footprint(&self) -> Footprint {
        Footprint {
            pending: self.pending.len(),
            storms: self.storms.len(),
            storm_paths: self.storm_paths,
            index: self.index.set.len(),
            halves: self.halves.len(),
            downloads: self.downloads.len(),
        }
    }

    fn hash(&self, path: &str) -> u64 {
        self.hasher.hash_one(path)
    }

    pub(crate) fn push(&mut self, signal: Signal, now: Timestamp) {
        match signal {
            Signal::Created(p) => self.observe(p, now, Observed::Created),
            Signal::Modified(p) => self.observe(p, now, Observed::Written),
            Signal::Removed(p) => self.observe(p, now, Observed::Removed),
            Signal::Changed { path, exists: true } => self.observe(path, now, Observed::Changed),
            Signal::Changed { path, exists: false } => self.observe(path, now, Observed::Removed),
            Signal::Scanned { path, recent } => {
                self.observe(path, now, Observed::Scanned { recent })
            }
            Signal::Moved { from, to } => self.moved(from, to, now),
            Signal::MovedFrom { path, cookie, dir } => self.half(path, cookie, dir, false, now),
            Signal::MovedTo { path, cookie, dir } => self.half(path, cookie, dir, true, now),
        }
    }

    // ---- Renames ----

    fn half(&mut self, path: String, cookie: Option<u64>, dir: bool, to: bool, now: Timestamp) {
        self.flush_halves_for(&path, now);
        let partner = match cookie {
            Some(c) => self.halves.iter().position(|h| h.to != to && h.cookie == Some(c)),
            // Without cookies (FSEvents) the two halves of a rename are reported together, so
            // only the most recent held half can be the partner.
            None => self
                .halves
                .back()
                .filter(|h| h.to != to && h.cookie.is_none())
                .map(|_| self.halves.len() - 1),
        };
        if let Some(other) = partner.and_then(|i| self.halves.remove(i)) {
            let is_dir = dir || other.dir;
            let (from, to_path) = if to { (other.path, path) } else { (path, other.path) };
            if is_dir {
                self.dir_moved(from, to_path);
            } else {
                self.moved(from, to_path, now);
            }
            return;
        }
        if self.halves.len() >= MAX_HALVES {
            if let Some(oldest) = self.halves.pop_front() {
                self.expire_half(oldest, now);
            }
        }
        self.halves.push_back(Half { path, cookie, dir, to, at: now });
    }

    /// A new signal for a path with a held half: the half cannot pair any more, resolve it first
    /// so ordering is kept.
    fn flush_halves_for(&mut self, path: &str, now: Timestamp) {
        while let Some(i) = self.halves.iter().position(|h| h.path == path) {
            if let Some(h) = self.halves.remove(i) {
                self.expire_half(h, now);
            }
        }
    }

    fn expire_half(&mut self, h: Half, now: Timestamp) {
        match (h.to, h.dir) {
            (false, true) => {}
            (false, false) => self.observe(h.path, now, Observed::Removed),
            (true, true) => {
                if !self.ignore.is_ignored(&h.path) {
                    self.actions.push(Action::ScanAppeared(h.path));
                }
            }
            (true, false) => self.observe(h.path, now, Observed::Appeared),
        }
    }

    fn dir_moved(&mut self, from: String, to: String) {
        match (self.ignore.is_ignored(&from), self.ignore.is_ignored(&to)) {
            (_, true) => {}
            (true, false) => self.actions.push(Action::ScanAppeared(to)),
            (false, false) => self.actions.push(Action::ScanMoved { from, to }),
        }
    }

    fn moved(&mut self, from: String, to: String, now: Timestamp) {
        self.flush_halves_for(&from, now);
        self.flush_halves_for(&to, now);
        match (self.ignore.is_ignored(&from), self.ignore.is_ignored(&to)) {
            (true, true) => {
                if let Some(t) = download_target(&from) {
                    self.downloads.remove(t);
                }
                return;
            }
            // Moved to an ignored name (an editor's backup copy, into an ignored directory):
            // the visible file is gone.
            (false, true) => return self.observe(from, now, Observed::Removed),
            // From an ignored name (a download finishing, an editor's temp file): new content
            // at the target.
            (true, false) => {
                if let Some(t) = download_target(&from) {
                    self.downloads.remove(t);
                }
                return self.observe(to, now, Observed::Appeared);
            }
            (false, false) => {}
        }
        if self.storm_of(&from).is_some() || self.storm_of(&to).is_some() {
            self.observe(from, now, Observed::Removed);
            return self.observe(to, now, Observed::Appeared);
        }
        let hf = self.hash(&from);
        let known_from = self.index.known(hf);
        let prior = self.pending.remove(&from);
        let (mut moved_from, written, mut first_at, signals) = match prior {
            Some(e) => match e.moved_from {
                Some(x) => (Some(x), e.net.written, e.first_at, e.signals),
                None if e.net.existed_before => (Some(from), e.net.written, e.first_at, e.signals),
                // Created in this window: a temp file renamed into place.
                None => (None, true, e.first_at, e.signals),
            },
            None if known_from == Some(false) => (None, true, now, 0),
            None => (Some(from), false, now, 0),
        };
        self.index.remove(hf);
        let ht = self.hash(&to);
        let known_to = self.index.known(ht);
        self.index.insert(ht);
        let existing = self.pending.remove(&to);
        if let Some(e) = &existing {
            first_at = first_at.min(e.first_at);
        }
        let mut existed_before = true;
        if moved_from.as_deref() == Some(to.as_str()) {
            // Moved back where it started.
            moved_from = None;
        } else if moved_from.is_none() {
            existed_before = match &existing {
                Some(e) => e.net.existed_before,
                None => known_to.unwrap_or(false),
            };
        }
        // `written` is already true when the content is new (no `moved_from`).
        let net = Net { existed_before, exists: true, written };
        let signals = signals + existing.map_or(0, |e| e.signals) + 1;
        let entry = Entry { net, moved_from, first_at, last_at: now, signals };
        self.insert_pending(to, entry, now);
    }

    // ---- Single-path observations ----

    fn observe(&mut self, path: String, now: Timestamp, what: Observed) {
        if self.ignore.is_ignored(&path) {
            if let Some(t) = download_target(&path) {
                if what.is_removal() {
                    self.downloads.remove(t);
                } else if self.downloads.len() < MAX_DOWNLOADS {
                    self.downloads.insert(t.to_owned());
                }
            }
            return;
        }
        self.flush_halves_for(&path, now);
        let h = self.hash(&path);
        let known = self.index.known(h);
        match what {
            Observed::Removed => self.index.remove(h),
            _ => self.index.insert(h),
        }
        if let Some(i) = self.storm_of(&path) {
            return self.storm_observe(i, h, &path, known, what, now);
        }
        if let Some(e) = self.pending.get_mut(&path) {
            e.net = match what {
                Observed::Removed => e.net.gone(),
                Observed::Scanned { recent: false } if e.net.exists => e.net,
                _ => e.net.present(),
            };
            e.last_at = now;
            e.signals = e.signals.saturating_add(1);
            return;
        }
        let Some(net) = what.fresh(known) else { return };
        let entry = Entry { net, moved_from: None, first_at: now, last_at: now, signals: 1 };
        self.insert_pending(path, entry, now);
    }

    fn insert_pending(&mut self, path: String, entry: Entry, now: Timestamp) {
        self.pending.insert(path, entry);
        if self.pending.len() >= self.s.pending_capacity {
            // The hard bound: whatever the shape of the activity, fold the whole scope.
            self.fold(String::new(), now);
        } else if self.pending.len() >= self.next_plan_at {
            for root in self.plan() {
                self.fold(root, now);
            }
            self.next_plan_at = (self.pending.len() + self.s.burst_threshold.div_ceil(4))
                .max(self.s.burst_threshold);
        }
    }

    // ---- Storms ----

    fn storm_of(&self, path: &str) -> Option<usize> {
        self.storms.iter().position(|s| within(path, &s.dir))
    }

    fn storm_observe(
        &mut self,
        i: usize,
        h: u64,
        path: &str,
        known: Option<bool>,
        what: Observed,
        now: Timestamp,
    ) {
        let budget = self.storm_paths < self.s.storm_exact_capacity;
        let sample_cap = self.s.sample_size * 2;
        let storm = &mut self.storms[i];
        storm.last_at = now;
        storm.signals = storm.signals.saturating_add(1);
        if storm.sample.len() < sample_cap
            && !what.is_removal()
            && !storm.sample.iter().any(|p| p == path)
        {
            storm.sample.push(path.to_owned());
        }
        if let Some(net) = storm.nets.get_mut(&h) {
            *net = match what {
                Observed::Removed => net.gone(),
                Observed::Scanned { recent: false } if net.exists => *net,
                _ => net.present(),
            };
        } else if budget {
            if let Some(net) = what.fresh(known) {
                storm.nets.insert(h, net);
                self.storm_paths += 1;
            }
        } else {
            storm.approx.add(what.fresh(known).and_then(Net::kind));
        }
    }

    /// Chooses where storms are, given the pending entries and existing storms (which weigh as
    /// much as the threshold).
    ///
    /// Walking down from the root, at each directory with at least `threshold` changes under
    /// it: if one child holds at least as much as everything else and everything else is below
    /// the threshold, that child dominates and the walk descends into it, leaving the rest as
    /// individual changes. Otherwise the activity is spread here, and this directory is the storm
    /// root. So an archive unpacked into `dataset/` (spreading over its subdirectories) folds at
    /// `dataset`, a storm in `project/build` folds there, and an edit to `notes.md` next to them
    /// is still reported on its own. A dominant child that is itself below the threshold folds
    /// nothing yet; planning runs again as pending grows.
    fn plan(&self) -> Vec<String> {
        let mut items: Vec<(&str, bool)> = self
            .pending
            .keys()
            .filter_map(|p| relative(&self.root, p).map(|r| (r, false)))
            .chain(
                self.storms.iter().filter_map(|s| relative(&self.root, &s.dir).map(|r| (r, true))),
            )
            .collect();
        items.sort_unstable();
        let mut out = Vec::new();
        plan_dir("", &items, self.s.burst_threshold, &mut out);
        out
    }

    /// Folds everything under `rel` (relative to the root; empty for the root) into one storm.
    fn fold(&mut self, rel: String, now: Timestamp) {
        let dir = if rel.is_empty() { self.root.clone() } else { join(&self.root, &rel) };
        let mut storm = Storm {
            dir: dir.clone(),
            started_at: now,
            last_at: now,
            signals: 0,
            nets: HashMap::new(),
            approx: Counts::default(),
            sample: Vec::new(),
        };
        let mut kept = Vec::with_capacity(self.storms.len());
        for s in std::mem::take(&mut self.storms) {
            if within(&s.dir, &dir) {
                storm.started_at = storm.started_at.min(s.started_at);
                storm.signals = storm.signals.saturating_add(s.signals);
                storm.nets.extend(s.nets);
                storm.approx.created += s.approx.created;
                storm.approx.modified += s.approx.modified;
                storm.approx.deleted += s.approx.deleted;
                for p in s.sample {
                    if storm.sample.len() < self.s.sample_size * 2 {
                        storm.sample.push(p);
                    }
                }
            } else {
                kept.push(s);
            }
        }
        let absorbed: Vec<String> =
            self.pending.keys().filter(|p| within(p, &dir)).cloned().collect();
        let mut absorbed_entries = Vec::with_capacity(absorbed.len());
        for p in absorbed {
            if let Some(e) = self.pending.remove(&p) {
                absorbed_entries.push((p, e));
            }
        }
        absorbed_entries.sort_by_key(|(_, e)| e.first_at);
        for (p, e) in absorbed_entries {
            storm.started_at = storm.started_at.min(e.first_at);
            storm.signals = storm.signals.saturating_add(e.signals);
            let mut nets = vec![(self.hash(&p), e.net)];
            if let Some(x) = &e.moved_from {
                nets[0].1 = Net { existed_before: false, ..e.net };
                nets.push((
                    self.hash(x),
                    Net { existed_before: true, exists: false, written: false },
                ));
            }
            for (h, net) in nets {
                if self.storm_paths < self.s.storm_exact_capacity {
                    if storm.nets.insert(h, net).is_none() {
                        self.storm_paths += 1;
                    }
                } else {
                    storm.approx.add(net.kind());
                }
            }
            if storm.sample.len() < self.s.sample_size * 2 && e.net.exists {
                storm.sample.push(p);
            }
        }
        kept.push(storm);
        self.storms = kept;
        if self.storms.len() > self.s.max_storms {
            self.fold(String::new(), now);
        }
    }

    // ---- Settling ----

    /// Settles what is due at `now`, at most `limit` changes, and returns the next deadline.
    /// Changes still due beyond `limit` make the next deadline `now`.
    pub(crate) fn poll(
        &mut self,
        now: Timestamp,
        limit: usize,
    ) -> (Vec<Settled>, Option<Timestamp>) {
        while let Some(h) = self.halves.front() {
            if h.at.saturating_add(self.s.pair_window) > now {
                break;
            }
            if let Some(h) = self.halves.pop_front() {
                self.expire_half(h, now);
            }
        }
        let mut out = Vec::new();
        let mut next: Option<Timestamp> =
            self.halves.front().map(|h| h.at.saturating_add(self.s.pair_window));
        let mut due: Vec<(Timestamp, String)> = Vec::new();
        for (p, e) in &self.pending {
            let at = self.entry_due(p, e);
            if at <= now {
                due.push((at, p.clone()));
            } else {
                next = earliest(next, at);
            }
        }
        due.sort();
        let mut over_limit = false;
        for (_, p) in due {
            if out.len() >= limit {
                over_limit = true;
                break;
            }
            if let Some(e) = self.pending.remove(&p) {
                if let Some(change) = settle_entry(p, &e) {
                    out.push(Settled { change, at: e.last_at, coalesced: e.signals });
                }
            }
        }
        let mut i = 0;
        while i < self.storms.len() {
            let s = &self.storms[i];
            let at = self.storm_due(s);
            if at > now {
                next = earliest(next, at);
                i += 1;
                continue;
            }
            if out.len() >= limit {
                over_limit = true;
                i += 1;
                continue;
            }
            let s = self.storms.remove(i);
            self.storm_paths = self.storm_paths.saturating_sub(s.nets.len());
            if let Some(settled) = self.settle_storm(s, now) {
                out.push(settled);
            }
        }
        if over_limit {
            next = Some(now);
        }
        if self.pending.len() < self.s.burst_threshold {
            self.next_plan_at = self.s.burst_threshold;
        }
        (out, next)
    }

    fn entry_due(&self, path: &str, e: &Entry) -> Timestamp {
        let hold = e.first_at.saturating_add(self.s.max_hold);
        if self.downloads.contains(path) {
            return hold;
        }
        e.last_at.saturating_add(self.s.quiet).min(hold)
    }

    fn storm_due(&self, s: &Storm) -> Timestamp {
        s.last_at
            .saturating_add(self.s.storm_quiet)
            .min(s.started_at.saturating_add(self.s.max_hold))
    }

    fn settle_storm(&mut self, s: Storm, now: Timestamp) -> Option<Settled> {
        let still_active = s.last_at.saturating_add(self.s.storm_quiet) > now;
        let mut counts = s.approx;
        for net in s.nets.values() {
            counts.add(net.kind());
        }
        let sample: Vec<String> = s
            .sample
            .iter()
            .filter(|p| s.nets.get(&self.hash(p)).is_none_or(|n| n.kind().is_some()))
            .take(self.s.sample_size)
            .cloned()
            .collect();
        if still_active {
            // Held for `max_hold` and still busy: report what we have and keep folding the
            // subtree, so activity there does not fall back to per-path tracking mid-storm.
            self.storms.push(Storm {
                dir: s.dir.clone(),
                started_at: now,
                last_at: s.last_at,
                signals: 0,
                nets: HashMap::new(),
                approx: Counts::default(),
                sample: Vec::new(),
            });
        }
        let total = counts.created + counts.modified + counts.deleted;
        (total > 0).then_some(Settled {
            change: Change::Burst(Burst {
                dir: s.dir,
                created: counts.created,
                modified: counts.modified,
                deleted: counts.deleted,
                sample,
            }),
            at: s.last_at,
            coalesced: s.signals,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Observed {
    Created,
    Written,
    Removed,
    /// Coarse report that the path exists now.
    Changed,
    /// New content arrived by rename or move-in.
    Appeared,
    Scanned {
        recent: bool,
    },
}

impl Observed {
    fn is_removal(self) -> bool {
        self == Observed::Removed
    }

    /// The entry for a path with no pending state, given what the index knew; `None` when the
    /// observation carries no news.
    fn fresh(self, known: Option<bool>) -> Option<Net> {
        match (self, known) {
            // A create report for a file we already know exists (a scan racing a create event)
            // says nothing new.
            (Observed::Created, Some(true)) => None,
            (Observed::Created, _) => Some(Net::new(false).present()),
            // A write to a file we never saw: it appeared after we last looked.
            (Observed::Written, Some(false)) => Some(Net::new(false).present()),
            (Observed::Written, _) => Some(Net::new(true).present()),
            (Observed::Removed, Some(false)) => None,
            (Observed::Removed, _) => Some(Net::new(true).gone()),
            (Observed::Changed | Observed::Appeared, k) => {
                Some(Net::new(k.unwrap_or(false)).present())
            }
            (Observed::Scanned { recent }, Some(true)) => recent.then(|| Net::new(true).present()),
            (Observed::Scanned { .. }, _) => Some(Net::new(false).present()),
        }
    }
}

fn settle_entry(path: String, e: &Entry) -> Option<Change> {
    match (&e.moved_from, e.net.exists) {
        (Some(from), true) => Some(Change::Moved { from: from.clone(), to: path }),
        (Some(from), false) => Some(Change::Deleted(from.clone())),
        (None, _) => match e.net.kind()? {
            Kind::Created => Some(Change::Created(path)),
            Kind::Modified => Some(Change::Modified(path)),
            Kind::Deleted => Some(Change::Deleted(path)),
        },
    }
}

fn plan_dir(dir: &str, items: &[(&str, bool)], t: usize, out: &mut Vec<String>) {
    // Per child directory: its weight (a storm counts as storm-sized) and its items.
    let mut children: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
    let mut direct = 0usize;
    for (idx, &(path, is_storm)) in items.iter().enumerate() {
        let rest = if dir.is_empty() || path == dir {
            if dir.is_empty() { path } else { "" }
        } else {
            match path.strip_prefix(dir).and_then(|r| r.strip_prefix('/')) {
                Some(r) => r,
                // Outside `dir`: the caller's slice is a superset of the child's items.
                None => continue,
            }
        };
        if rest.is_empty() {
            // A storm exactly here.
            out.push(dir.to_owned());
            return;
        }
        match (rest.find('/'), is_storm) {
            (None, false) => direct += 1,
            (slash, _) => {
                let child = &rest[..slash.unwrap_or(rest.len())];
                let w = if is_storm { t } else { 1 };
                let c = children.entry(child).or_insert((0, idx, idx));
                c.0 = c.0.saturating_add(w);
                c.2 = idx;
            }
        }
    }
    let total = direct + children.values().map(|c| c.0).sum::<usize>();
    if total < t {
        return;
    }
    let heaviest = children.iter().max_by_key(|(_, c)| c.0).map(|(k, c)| (*k, *c));
    let dominant = heaviest.filter(|(_, (w, _, _))| {
        let rest = total - w;
        rest < t && *w >= rest
    });
    match dominant {
        Some((child, (_, first, last))) => {
            let child_dir =
                if dir.is_empty() { child.to_owned() } else { format!("{dir}/{child}") };
            // Items are sorted, so a child's items are contiguous (with some neighbours, which
            // the callee skips).
            plan_dir(&child_dir, &items[first..=last], t, out);
        }
        None => out.push(dir.to_owned()),
    }
}

fn earliest(a: Option<Timestamp>, b: Timestamp) -> Option<Timestamp> {
    Some(a.map_or(b, |a| a.min(b)))
}

/// Whether `path` is `dir` or under it (both clean absolute).
pub(crate) fn within(path: &str, dir: &str) -> bool {
    dir == "/" || path == dir || path.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

fn relative<'a>(root: &str, path: &'a str) -> Option<&'a str> {
    if path == root {
        return Some("");
    }
    if root == "/" {
        return path.strip_prefix('/');
    }
    path.strip_prefix(root)?.strip_prefix('/')
}

fn join(root: &str, rel: &str) -> String {
    if root == "/" { format!("/{rel}") } else { format!("{root}/{rel}") }
}

#[cfg(test)]
mod tests;
