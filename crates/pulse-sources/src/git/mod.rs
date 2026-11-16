//! Git activity in one working tree: `git.commit.created`, `git.branch.switched`,
//! `git.merge.conflicted`, `git.push.completed`.
//!
//! Detection reads files, never runs `git`:
//! - `logs/HEAD` (the worktree's HEAD reflog) for commits and branch switches. Each new line is
//!   classified by its message ([`reflog::classify_head`]). The working tree is not watched, so a
//!   checkout that rewrites thousands of files is one reflog line and one event.
//! - `logs/refs/remotes/<remote>/<branch>` for pushes: git appends `update by push` when a push
//!   updates the remote-tracking ref (fetches write `fetch: …`).
//! - `MERGE_HEAD`, `CHERRY_PICK_HEAD`, `REVERT_HEAD`, `rebase-merge/`, `rebase-apply/` plus the
//!   index's conflict stages for conflicts, reported once per conflict episode.
//!
//! Decisions (see `docs/internals/0005-sources.md`): an amend is a new commit and is reported;
//! a rebase reports nothing for the commits it rewrites and no branch switch (it starts and ends
//! on the same branch), only conflicts if it stops; fast-forward merges and pulls create no
//! commit and report nothing; a merge that creates a merge commit is a commit. Commits made in
//! another worktree of the same repository belong to that worktree's source.
//!
//! The source watches only the git directories (`.git`, or the `gitdir:` target and its common
//! directory for linked worktrees and submodules). History present at startup is not replayed:
//! reflogs are read from their end. A repository that appears while the source runs (`git
//! init`, `git clone`) is read from the start, skipping lines older than its appearance; that
//! first scan is the only place a reflog line's time is used, because the time is the
//! committer's clock (`GIT_COMMITTER_DATE`, a clock set back), not ours.
//!
//! After that, what is history is decided by position: each reflog's [`Cursor`] remembers how
//! far it was read and the last line read. A reflog that git rewrote (`reflog expire`, `gc`,
//! which keep lines and swap in a new file) is recognised by finding that line again, and read
//! on from there; one that no longer has it (expired to nothing, or a different repository
//! copied or cloned in at the same path) is read from its end.

mod config;
mod reflog;
mod repo;

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use notify::{RecursiveMode, Watcher};
use pulse_contracts::catalog::{
    BuiltinEvent, GitBranchSwitchedData, GitCommitData, GitMergeConflictedData,
    GitPushCompletedData,
};
use pulse_contracts::config::GitSourceSettings;
use pulse_contracts::{NewEvent, SourceKind, SourceRef};
use tokio::sync::mpsc;

use self::reflog::HeadMove;
use self::repo::{Layout, LayoutError, MAX_IDENT, MAX_MESSAGE, MAX_NAME, bounded};
use crate::clock::{Clock, SystemClock};
use crate::{BoxFuture, Source, SourceContext, SourceEnv, SourceError};

/// Git writes several files per operation; wait this long after the last change before reading.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(250);
/// But never longer than this after the first change (a long rebase keeps writing).
const MAX_SETTLE: std::time::Duration = std::time::Duration::from_secs(2);
/// New reflog bytes read per file per scan; more than this (a script making thousands of
/// commits between two scans) skips ahead and says so.
const MAX_READ: u64 = 1 << 20;
const MAX_REMOTE_LOGS: usize = 10_000;
const MAX_CONFLICT_FILES: usize = 200;
/// How much of the last line read is kept to recognise a reflog by. Lines start with two object
/// ids and end with a time and message, so this is unique in practice.
const MARK: usize = 1024;
const OUTBOX: usize = 64;

#[derive(Debug)]
pub struct GitSource {
    source: SourceRef,
    work: String,
    clock: Arc<dyn Clock>,
}

impl GitSource {
    pub fn new(
        name: &str,
        settings: &GitSourceSettings,
        env: &SourceEnv,
    ) -> Result<Self, SourceError> {
        let work = pulse_contracts::pattern::expand_path(&settings.path, &env.home, None)
            .map_err(|e| SourceError::Config(format!("path: {e}")))?;
        Ok(GitSource {
            source: SourceRef::new(SourceKind::Git, name),
            work,
            clock: Arc::new(SystemClock),
        })
    }
}

impl Source for GitSource {
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

/// The state that turns repository files into events. Synchronous; the source calls
/// [`Tracker::scan`] after the files change, and tests call it after each git command.
#[derive(Debug)]
pub(crate) struct Tracker {
    work: String,
    layout: Layout,
    hash_bytes: usize,
    /// How far each reflog has been read.
    cursors: BTreeMap<PathBuf, Cursor>,
    /// For the first scan of a repository that appeared while we ran: reflog lines older than
    /// this (unix seconds) are history. Cleared after that scan.
    baseline: Option<i64>,
    branch: Option<String>,
    /// The conflicted files of the current conflict episode, if one is in progress.
    conflict: Option<Vec<String>>,
    index_stamp: Option<(std::time::SystemTime, u64)>,
    pub(crate) warnings: Vec<String>,
}

impl Tracker {
    /// `appeared`: the repository showed up while we were running, so its reflogs are read from
    /// the start (in that first scan, lines older than `now - 5s` are skipped); otherwise from
    /// their current end.
    pub(crate) fn new(work: &str, layout: Layout, appeared: bool, now_secs: i64) -> Self {
        let hash_bytes = layout.hash_bytes();
        let mut t = Tracker {
            work: work.to_owned(),
            branch: layout.head_branch(),
            layout,
            hash_bytes,
            cursors: BTreeMap::new(),
            baseline: appeared.then(|| now_secs.saturating_sub(5)),
            conflict: None,
            index_stamp: None,
            warnings: Vec::new(),
        };
        if !appeared {
            for f in t.reflog_files() {
                let c = Cursor::at_end(&f);
                t.cursors.insert(f, c);
            }
            // A conflict in progress at startup is not news.
            t.conflict = t.read_conflicts().filter(|c| !c.is_empty());
        }
        t
    }

    pub(crate) fn layout(&self) -> &Layout {
        &self.layout
    }

    fn head_log(&self) -> PathBuf {
        self.layout.git_dir.join("logs/HEAD")
    }

    fn remotes_dir(&self) -> PathBuf {
        self.layout.common_dir.join("logs/refs/remotes")
    }

    fn reflog_files(&self) -> Vec<PathBuf> {
        let mut out = vec![self.head_log()];
        let mut stack = vec![self.remotes_dir()];
        while let Some(d) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else { continue };
            for e in entries.flatten() {
                if out.len() > MAX_REMOTE_LOGS {
                    return out;
                }
                match e.file_type() {
                    Ok(t) if t.is_dir() => stack.push(e.path()),
                    Ok(t) if t.is_file() => out.push(e.path()),
                    _ => {}
                }
            }
        }
        out
    }

    /// New complete lines of a reflog since the last read. A reflog seen for the first time
    /// (a new remote-tracking branch, the first commit) is read from its start.
    fn new_lines(&mut self, path: &Path) -> Vec<String> {
        let Ok(meta) = std::fs::metadata(path) else {
            self.cursors.remove(path);
            return Vec::new();
        };
        let len = meta.len();
        let mut cursor = self.cursors.remove(path).unwrap_or_default();
        // Unchanged in length: nothing to read (and nothing read to find that out). A rewrite to
        // the same length is noticed with the next line appended.
        if len == cursor.offset {
            self.cursors.insert(path.to_owned(), cursor);
            return Vec::new();
        }
        if !cursor.still_matches(path, len) {
            // Rewritten or replaced: on from the last line we read, if it is still there.
            match cursor.find_mark(path, len) {
                Some(c) => cursor = c,
                None => {
                    self.cursors.insert(path.to_owned(), Cursor::at_end(path));
                    return Vec::new();
                }
            }
        }
        let mut start = cursor.offset;
        if len - start > MAX_READ {
            self.warnings.push(format!(
                "{} grew by {} bytes at once; skipped to its end",
                path.display(),
                len - start
            ));
            self.cursors.insert(path.to_owned(), Cursor::at_end(path));
            return Vec::new();
        }
        let Ok(bytes) = read_range(path, start, len) else {
            self.cursors.insert(path.to_owned(), cursor);
            return Vec::new();
        };
        // Only complete lines; a line being appended is read next time.
        let complete = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        if complete > 0 {
            cursor.mark = Some(last_line_mark(&bytes[..complete]));
        }
        start += complete as u64;
        cursor.offset = start;
        self.cursors.insert(path.to_owned(), cursor);
        String::from_utf8_lossy(&bytes[..complete]).lines().map(str::to_owned).collect()
    }

    pub(crate) fn scan(&mut self) -> Vec<BuiltinEvent> {
        let mut out = Vec::new();
        let head = self.head_log();
        for line in self.new_lines(&head) {
            self.head_line(&line, &mut out);
        }
        // HEAD is the truth for the branch going into the next batch.
        self.branch = self.layout.head_branch();
        let remotes_dir = self.remotes_dir();
        // Read only when there is a push to name, at most once per scan.
        let mut configured: Option<Vec<String>> = None;
        for file in self.reflog_files().into_iter().skip(1) {
            let Ok(rel) = file.strip_prefix(&remotes_dir) else { continue };
            let Some(rel) = rel.to_str().map(str::to_owned) else { continue };
            let pushes: Vec<String> = self
                .new_lines(&file)
                .into_iter()
                .filter_map(|line| {
                    let l = reflog::parse(&line)?;
                    (self.news(l.time) && reflog::is_push(l.message) && !reflog::is_zero(l.new))
                        .then(|| l.new.to_owned())
                })
                .collect();
            if pushes.is_empty() {
                continue;
            }
            let configured = configured.get_or_insert_with(|| {
                config::read_remotes(&self.layout.common_dir.join("config")).unwrap_or_else(|e| {
                    self.warnings.push(format!("remote names unknown: {e}"));
                    Vec::new()
                })
            });
            // A remote's name may contain `/`; a ref no configured remote prefixes is reported
            // with no remote rather than split at a guess.
            let (remote, branch) = match config::split_remote(configured, &rel) {
                Some((remote, branch)) => (Some(bounded(remote, MAX_NAME)), branch),
                None => (None, rel.as_str()),
            };
            // `refs/remotes/<remote>/HEAD` names the remote's default branch; it is not pushed.
            let default_branch = match remote {
                Some(_) => branch == "HEAD",
                None => branch.rsplit('/').next() == Some("HEAD"),
            };
            if default_branch {
                continue;
            }
            for sha in pushes {
                out.push(BuiltinEvent::GitPushCompleted(GitPushCompletedData {
                    repo: self.work.clone(),
                    remote: remote.clone(),
                    branch: bounded(branch, MAX_NAME),
                    sha,
                }));
            }
        }
        if let Some(files) = self.read_conflicts() {
            match (&self.conflict, files.is_empty()) {
                (None, false) => {
                    out.push(BuiltinEvent::GitMergeConflicted(GitMergeConflictedData {
                        repo: self.work.clone(),
                        files: files.clone(),
                    }));
                    self.conflict = Some(files);
                }
                (Some(_), true) => self.conflict = None,
                _ => {}
            }
        } else {
            self.conflict = None;
        }
        self.baseline = None;
        out
    }

    fn news(&self, time: i64) -> bool {
        self.baseline.is_none_or(|b| time >= b)
    }

    fn head_line(&mut self, line: &str, out: &mut Vec<BuiltinEvent>) {
        let Some(l) = reflog::parse(line) else { return };
        let news = self.news(l.time);
        match reflog::classify_head(l.message) {
            HeadMove::Commit { subject } if news => {
                let (author, message) = self.layout.commit_info(l.new).unwrap_or_else(|| {
                    (bounded(l.ident, MAX_IDENT), bounded(subject, MAX_MESSAGE))
                });
                out.push(BuiltinEvent::GitCommitCreated(GitCommitData {
                    repo: self.work.clone(),
                    branch: self.branch.clone(),
                    sha: l.new.to_owned(),
                    message,
                    author,
                }));
            }
            HeadMove::Checkout { from, to } => {
                let to_branch = self.layout.is_branch(to);
                self.branch = to_branch.then(|| bounded(to, MAX_NAME));
                if news {
                    out.push(BuiltinEvent::GitBranchSwitched(GitBranchSwitchedData {
                        repo: self.work.clone(),
                        from: Some(bounded(from, MAX_NAME)),
                        // Detached: the commit, not whatever expression was checked out.
                        to: if to_branch { bounded(to, MAX_NAME) } else { l.new.to_owned() },
                    }));
                }
            }
            HeadMove::RebaseStart => self.branch = None,
            HeadMove::RebaseEnd { branch } => self.branch = Some(bounded(branch, MAX_NAME)),
            HeadMove::Commit { .. } | HeadMove::Other => {}
        }
    }

    /// `Some(conflicted files)` while a merge-like operation is in progress, `None` otherwise.
    fn read_conflicts(&mut self) -> Option<Vec<String>> {
        if !self.layout.in_merge_state() {
            self.index_stamp = None;
            return None;
        }
        let index = self.layout.index_path();
        let meta = std::fs::metadata(&index).ok()?;
        let stamp = (meta.modified().ok()?, meta.len());
        if self.index_stamp == Some(stamp) {
            return Some(self.conflict.clone().unwrap_or_default());
        }
        self.index_stamp = Some(stamp);
        if meta.len() > repo::MAX_INDEX {
            self.warnings.push(format!("{} is too large to check for conflicts", index.display()));
            return Some(Vec::new());
        }
        let bytes = std::fs::read(&index).ok()?;
        let paths = repo::conflicted_paths(&bytes, self.hash_bytes, MAX_CONFLICT_FILES)?;
        Some(
            paths
                .iter()
                .filter_map(|p| {
                    pulse_contracts::pattern::clean_absolute(&format!("{}/{p}", self.work))
                })
                .collect(),
        )
    }

    /// Directories whose changes can mean news.
    fn watch_dirs(&self) -> Vec<(PathBuf, RecursiveMode)> {
        let mut dirs = vec![
            (self.layout.git_dir.clone(), RecursiveMode::NonRecursive),
            (self.layout.git_dir.join("logs"), RecursiveMode::Recursive),
        ];
        if self.layout.common_dir != self.layout.git_dir {
            dirs.push((self.layout.common_dir.join("logs"), RecursiveMode::Recursive));
        }
        dirs
    }
}

/// Where reading a reflog goes on from.
#[derive(Debug, Default)]
struct Cursor {
    /// Bytes consumed (always at a line boundary).
    offset: u64,
    /// The end of the last line consumed, newline included, at most [`MARK`] bytes; `None`
    /// before any line was.
    mark: Option<Vec<u8>>,
}

fn last_line_mark(complete: &[u8]) -> Vec<u8> {
    let body = &complete[..complete.len() - 1];
    let start = body.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let line = &complete[start..];
    line[line.len().saturating_sub(MARK)..].to_vec()
}

impl Cursor {
    /// After the last complete line the file has now.
    fn at_end(path: &Path) -> Cursor {
        let len = std::fs::metadata(path).map_or(0, |m| m.len());
        let from = len.saturating_sub(MAX_READ);
        let Ok(tail) = read_range(path, from, len) else { return Cursor::default() };
        match tail.iter().rposition(|&b| b == b'\n') {
            Some(i) => {
                Cursor { offset: from + i as u64 + 1, mark: Some(last_line_mark(&tail[..=i])) }
            }
            None => Cursor { offset: len, mark: None },
        }
    }

    /// Whether the file still ends its first `offset` bytes with the line we read last.
    fn still_matches(&self, path: &Path, len: u64) -> bool {
        if len < self.offset {
            return false;
        }
        let Some(mark) = &self.mark else { return true };
        let start = self.offset.saturating_sub(mark.len() as u64);
        read_range(path, start, self.offset).is_ok_and(|b| b == *mark)
    }

    /// The position after the last occurrence of our last line in the file's tail.
    fn find_mark(&self, path: &Path, len: u64) -> Option<Cursor> {
        let mark = self.mark.as_ref()?;
        let from = len.saturating_sub(MAX_READ);
        let tail = read_range(path, from, len).ok()?;
        let end = tail.windows(mark.len()).rposition(|w| w == mark.as_slice())? + mark.len();
        Some(Cursor { offset: from + end as u64, mark: Some(mark.clone()) })
    }
}

fn read_range(path: &Path, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity(usize::try_from(end - start).unwrap_or(0));
    f.take(end - start).read_to_end(&mut buf)?;
    Ok(buf)
}

enum State {
    Tracking { tracker: Box<Tracker>, watched: Vec<PathBuf> },
    Waiting { watched: Option<PathBuf> },
}

async fn run(src: GitSource, cx: &SourceContext) -> Result<(), SourceError> {
    let (tx, mut rx) = mpsc::channel::<notify::Result<notify::Event>>(1024);
    let mut watcher = notify::recommended_watcher(move |res| {
        // Only "something changed" matters; a full channel already says that.
        let _ = tx.try_send(res);
    })
    .map_err(|e| SourceError::Platform(format!("cannot create a file watcher: {e}")))?;
    let mut state = State::Waiting { watched: None };
    let mut outbox: VecDeque<NewEvent> = VecDeque::new();
    let mut first_change: Option<tokio::time::Instant> = None;
    let mut deadline: Option<tokio::time::Instant> = None;
    let secs = |c: &dyn Clock| c.now().as_millis() / 1000;
    refresh(&src, cx, &mut watcher, &mut state, false, secs(&*src.clock));
    loop {
        tokio::select! {
            biased;
            () = cx.shutdown.cancelled() => return Ok(()),
            msg = rx.recv() => {
                if msg.is_none() {
                    return Err(SourceError::Platform("the file watcher stopped".into()));
                }
                while rx.try_recv().is_ok() {}
                let now = tokio::time::Instant::now();
                let first = *first_change.get_or_insert(now);
                deadline = Some((now + SETTLE).min(first + MAX_SETTLE));
            }
            permit = cx.events.reserve(), if !outbox.is_empty() => {
                if let Some(ev) = outbox.pop_front() {
                    permit?.send(ev);
                }
            }
            () = async {
                if let Some(d) = deadline { tokio::time::sleep_until(d).await }
            }, if deadline.is_some() && outbox.len() < OUTBOX => {
                deadline = None;
                first_change = None;
                let now = secs(&*src.clock);
                refresh(&src, cx, &mut watcher, &mut state, true, now);
                if let State::Tracking { tracker, .. } = &mut state {
                    let at = src.clock.now();
                    for ev in tracker.scan() {
                        outbox.push_back(ev.into_new_event(src.source.clone(), at));
                    }
                    for w in tracker.warnings.drain(..) {
                        cx.health.error(w);
                    }
                }
            }
        }
    }
}

/// Re-checks where the repository is and adjusts watches: starts tracking when `.git` appears,
/// goes back to waiting when it disappears, and watches directories created since (`logs/`
/// appears with the first commit).
fn refresh<W: Watcher>(
    src: &GitSource,
    cx: &SourceContext,
    watcher: &mut W,
    state: &mut State,
    running: bool,
    now_secs: i64,
) {
    let work = Path::new(&src.work);
    let layout = repo::discover(work);
    match (layout, &mut *state) {
        (Ok(layout), State::Tracking { tracker, watched }) if *tracker.layout() == layout => {
            for (dir, mode) in tracker.watch_dirs() {
                if !watched.contains(&dir) && dir.is_dir() && watcher.watch(&dir, mode).is_ok() {
                    watched.push(dir);
                }
            }
        }
        (Ok(layout), _) => {
            unwatch_all(watcher, state);
            let tracker = Tracker::new(&src.work, layout, running, now_secs);
            let mut watched = Vec::new();
            for (dir, mode) in tracker.watch_dirs() {
                if dir.is_dir() {
                    match watcher.watch(&dir, mode) {
                        Ok(()) => watched.push(dir),
                        Err(e) => cx.health.error(format!("cannot watch {}: {e}", dir.display())),
                    }
                }
            }
            *state = State::Tracking { tracker: Box::new(tracker), watched };
            cx.health.healthy();
        }
        // Still waiting, and already watching the right directory: the working tree, or an
        // ancestor while the working tree does not exist.
        (Err(e), State::Waiting { watched: Some(a) })
            if !matches!(e, LayoutError::Invalid { .. }) && (a == work || !work.is_dir()) => {}
        (Err(e), _) => {
            unwatch_all(watcher, state);
            let message = match &e {
                LayoutError::Missing(_) if !work.exists() => {
                    format!("{} does not exist; waiting for it to appear", src.work)
                }
                LayoutError::Missing(_) => {
                    format!("{} is not a git repository (no .git); waiting for one", src.work)
                }
                LayoutError::Invalid { .. } => format!("{e}; waiting for a valid repository"),
            };
            cx.health.degraded(message);
            // Watch the working tree (or its nearest existing ancestor) for `.git` appearing.
            let mut anchor = Some(work);
            while let Some(a) = anchor {
                if a.is_dir() && watcher.watch(a, RecursiveMode::NonRecursive).is_ok() {
                    break;
                }
                anchor = a.parent();
            }
            *state = State::Waiting { watched: anchor.map(Path::to_owned) };
        }
    }
}

fn unwatch_all<W: Watcher>(watcher: &mut W, state: &mut State) {
    match state {
        State::Tracking { watched, .. } => {
            for d in watched.drain(..) {
                let _ = watcher.unwatch(&d);
            }
        }
        State::Waiting { watched } => {
            if let Some(d) = watched.take() {
                let _ = watcher.unwatch(&d);
            }
        }
    }
}

#[cfg(test)]
mod tests;
