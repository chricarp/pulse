//! The fs source end to end. The first tests inject raw events under paused time; the rest run
//! the real watcher on temp directories and wait for the specific events they expect (the suite
//! ceiling is the only timeout).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use notify::event::{CreateKind, DataChange, ModifyKind};
use notify::{Event, EventKind};
use pulse_contracts::config::FsSourceSettings;
use pulse_contracts::status::{SourceHealth, SourceState};
use serde_json::Value;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::clock::TokioClock;

fn settings(root: &Path) -> FsSourceSettings {
    FsSourceSettings {
        path: root.to_str().unwrap().to_owned(),
        ignore: vec![],
        recursive: true,
        quiet: Some(Duration::from_millis(300)),
        burst_threshold: None,
    }
}

fn env(home: &Path) -> SourceEnv {
    SourceEnv { home: home.into(), pulse_home: home.join("pulse-home") }
}

fn ev(kind: EventKind, path: &Path) -> notify::Result<Event> {
    Ok(Event::new(kind).add_path(path.into()))
}

fn path_of(e: &NewEvent) -> &str {
    e.data.get("path").or_else(|| e.data.get("dir")).and_then(Value::as_str).unwrap_or("")
}

#[tokio::test(start_paused = true)]
async fn keeps_coalescing_while_the_engine_is_behind() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::write(root.join("a.txt"), "existing").unwrap();
    let mut src = FsSource::new("t", &settings(&root), &env(&root)).unwrap();
    src.clock = Arc::new(TokioClock::new(Timestamp::from_millis(1_000_000)));
    let source = src.source.clone();
    // The engine's channel holds one event and nobody reads it for a while.
    let (cx, mut rx, mut health) =
        crate::context(source, 1, CancellationToken::new(), Timestamp::EPOCH);
    // A small raw channel: if the loop stopped reading while the sink is full, the sends below
    // would never complete.
    let (tx, mut raw_rx) = mpsc::channel(16);
    let overflow = Arc::new(AtomicBool::new(false));
    let task = tokio::spawn(async move {
        drive(src, &cx, notify::NullWatcher, Backend::Inotify, &mut raw_rx, overflow).await
    });
    health.wait_for(|h| h.state == SourceState::Healthy).await.unwrap();

    tx.send(ev(EventKind::Create(CreateKind::File), &root.join("b.txt"))).await.unwrap();
    health.wait_for(|h| h.events_emitted == 1).await.unwrap();
    // The sink is full now. 100,000 raw events for two files go through anyway.
    tx.send(ev(EventKind::Create(CreateKind::File), &root.join("c.txt"))).await.unwrap();
    for i in 0..100_000 {
        let p = if i % 2 == 0 { "a.txt" } else { "c.txt" };
        tx.send(ev(EventKind::Modify(ModifyKind::Data(DataChange::Any)), &root.join(p)))
            .await
            .unwrap();
    }
    let mut got = Vec::new();
    for _ in 0..3 {
        let e = rx.recv().await.unwrap();
        got.push((
            e.type_.to_string(),
            path_of(&e).rsplit('/').next().unwrap().to_owned(),
            e.coalesced,
        ));
    }
    got[1..].sort();
    assert_eq!(
        got,
        vec![
            ("fs.file.created".into(), "b.txt".into(), Some(1)),
            ("fs.file.created".into(), "c.txt".into(), Some(50_001)),
            ("fs.file.modified".into(), "a.txt".into(), Some(50_000)),
        ]
    );
    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    assert!(rx.try_recv().is_err(), "nothing else");
    drop(tx);
    assert!(task.await.unwrap().is_err(), "a closed watcher channel is an error");
}

#[tokio::test(start_paused = true)]
async fn lost_events_trigger_a_rescan_that_finds_what_changed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::write(root.join("old.txt"), "x").unwrap();
    let mut src = FsSource::new("t", &settings(&root), &env(&root)).unwrap();
    src.clock = Arc::new(TokioClock::new(Timestamp::from_millis(1_000_000)));
    let (cx, mut rx, mut health) =
        crate::context(src.source.clone(), 16, CancellationToken::new(), Timestamp::EPOCH);
    let (tx, mut raw_rx) = mpsc::channel(16);
    let overflow = Arc::new(AtomicBool::new(false));
    let flag = overflow.clone();
    tokio::spawn(async move {
        drive(src, &cx, notify::NullWatcher, Backend::Inotify, &mut raw_rx, flag).await
    });
    health.wait_for(|h| h.state == SourceState::Healthy).await.unwrap();
    // A file appears but its events are lost.
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/new.txt"), "y").unwrap();
    overflow.store(true, Ordering::Relaxed);
    tx.send(ev(EventKind::Modify(ModifyKind::Data(DataChange::Any)), &root.join("old.txt")))
        .await
        .unwrap();
    let mut got = BTreeMap::new();
    while got.len() < 2 {
        let e = rx.recv().await.unwrap();
        got.insert(path_of(&e).rsplit('/').next().unwrap().to_owned(), e.type_.to_string());
    }
    assert_eq!(got["new.txt"], "fs.file.created");
    assert_eq!(got["old.txt"], "fs.file.modified");
    health.wait_for(|h| h.state == SourceState::Healthy).await.unwrap();
    assert!(health.borrow().last_error.as_ref().unwrap().message.contains("rescanning"));
}

// ---- The real watcher on temp directories ----

struct Running {
    rx: mpsc::Receiver<NewEvent>,
    health: watch::Receiver<SourceHealth>,
    stop: CancellationToken,
    root: String,
}

impl Running {
    async fn start(root: &Path, configure: impl FnOnce(&mut FsSourceSettings)) -> Running {
        Self::start_with(root, configure, None).await
    }

    async fn start_with(
        root: &Path,
        configure: impl FnOnce(&mut FsSourceSettings),
        probe: Option<Arc<Mutex<FsFootprint>>>,
    ) -> Running {
        let mut s = settings(root);
        configure(&mut s);
        let mut src = FsSource::new("t", &s, &env(root.parent().unwrap())).unwrap();
        src.probe = probe;
        let stop = CancellationToken::new();
        let (cx, rx, health) =
            crate::context(src.source.clone(), 64, stop.clone(), Timestamp::EPOCH);
        tokio::spawn(Box::new(src).run(cx));
        Running { rx, health, stop, root: root.to_str().unwrap().to_owned() }
    }

    async fn healthy(&mut self) {
        self.health.wait_for(|h| h.state == SourceState::Healthy).await.unwrap();
    }

    /// Collects events until one matches.
    async fn until(&mut self, mut pred: impl FnMut(&NewEvent) -> bool) -> Vec<NewEvent> {
        let mut out = Vec::new();
        loop {
            let e = self.rx.recv().await.expect("source stopped");
            let done = pred(&e);
            out.push(e);
            if done {
                return out;
            }
        }
    }

    async fn until_path(&mut self, type_: &str, rel: &str) -> Vec<NewEvent> {
        let want = format!("{}/{rel}", self.root);
        let type_ = type_.to_owned();
        self.until(move |e| e.type_.as_str() == type_ && path_of(e) == want).await
    }

    fn summary(&self, events: &[NewEvent]) -> Vec<(String, String)> {
        events
            .iter()
            .map(|e| {
                let p = path_of(e);
                let rel = p.strip_prefix(&self.root).unwrap_or(p).trim_start_matches('/');
                (e.type_.to_string(), rel.to_owned())
            })
            .collect()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

fn pair(t: &str, p: &str) -> (String, String) {
    (t.to_owned(), p.to_owned())
}

#[tokio::test]
async fn file_lifecycle_on_a_real_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("w");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("notes.md"), "v0").unwrap();
    let mut s = Running::start(&root, |_| {}).await;
    s.healthy().await;

    // Created and written in chunks: one event, with the final size.
    {
        use std::io::Write;
        let mut f = std::fs::File::create(root.join("lecture.pdf")).unwrap();
        for _ in 0..64 {
            f.write_all(&[7u8; 1024]).unwrap();
            f.flush().unwrap();
        }
    }
    let got = s.until_path("fs.file.created", "lecture.pdf").await;
    assert_eq!(s.summary(&got), vec![pair("fs.file.created", "lecture.pdf")]);
    assert_eq!(got[0].data["size"], 65_536);
    assert_eq!(got[0].data["ext"], "pdf");
    assert_eq!(got[0].subject.as_deref(), Some(format!("{}/lecture.pdf", s.root).as_str()));

    // Forty saves: one modification.
    for i in 0..40 {
        std::fs::write(root.join("notes.md"), format!("v{i}")).unwrap();
    }
    let got = s.until_path("fs.file.modified", "notes.md").await;
    assert_eq!(s.summary(&got), vec![pair("fs.file.modified", "notes.md")]);

    // An atomic save (temp file renamed over the target) is a modification of the target.
    std::fs::write(root.join(".notes.md.tmp"), "atomic").unwrap();
    std::fs::write(root.join("sedX1y2"), "atomic").unwrap();
    std::fs::rename(root.join("sedX1y2"), root.join("notes.md")).unwrap();
    let got = s.until_path("fs.file.modified", "notes.md").await;
    assert_eq!(s.summary(&got), vec![pair("fs.file.modified", "notes.md")]);

    // Created then deleted inside the window: nothing. A rename: one move. Then a sentinel.
    std::fs::write(root.join("flash"), "x").unwrap();
    std::fs::remove_file(root.join("flash")).unwrap();
    std::fs::rename(root.join("lecture.pdf"), root.join("lecture-07.pdf")).unwrap();
    std::fs::remove_file(root.join(".notes.md.tmp")).unwrap();
    let got = s.until_path("fs.file.moved", "lecture-07.pdf").await;
    assert_eq!(s.summary(&got), vec![pair("fs.file.moved", "lecture-07.pdf")]);
    assert_eq!(got[0].data["from"], format!("{}/lecture.pdf", s.root));
    std::fs::remove_file(root.join("lecture-07.pdf")).unwrap();
    let got = s.until_path("fs.file.deleted", "lecture-07.pdf").await;
    assert_eq!(s.summary(&got), vec![pair("fs.file.deleted", "lecture-07.pdf")]);
}

#[tokio::test]
async fn new_directories_are_watched_without_missing_their_first_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("w");
    std::fs::create_dir(&root).unwrap();
    let mut s = Running::start(&root, |_| {}).await;
    s.healthy().await;
    std::fs::create_dir_all(root.join("a/b/c")).unwrap();
    for p in ["a/1.txt", "a/b/2.txt", "a/b/c/3.txt"] {
        std::fs::write(root.join(p), p).unwrap();
    }
    let mut seen = std::collections::BTreeSet::new();
    let got = s
        .until(|e| {
            seen.insert(path_of(e).to_owned());
            seen.len() == 3
        })
        .await;
    let mut sum = s.summary(&got);
    sum.sort();
    assert_eq!(
        sum,
        vec![
            pair("fs.file.created", "a/1.txt"),
            pair("fs.file.created", "a/b/2.txt"),
            pair("fs.file.created", "a/b/c/3.txt")
        ]
    );
    // Then the directory moves: its files move with it, and the new location is watched.
    std::fs::rename(root.join("a"), root.join("z")).unwrap();
    let mut moved = std::collections::BTreeSet::new();
    let got = s
        .until(|e| {
            moved.insert(path_of(e).to_owned());
            moved.len() == 3
        })
        .await;
    assert!(got.iter().all(|e| e.type_.as_str() == "fs.file.moved"), "{:?}", s.summary(&got));
    std::fs::write(root.join("z/b/c/4.txt"), "x").unwrap();
    s.until_path("fs.file.created", "z/b/c/4.txt").await;
}

#[tokio::test]
async fn ignored_paths_produce_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("w");
    std::fs::create_dir(&root).unwrap();
    let mut s = Running::start(&root, |c| c.ignore = vec!["*.log".into(), "out/**".into()]).await;
    s.healthy().await;
    std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
    std::fs::write(root.join("node_modules/pkg/index.js"), "x").unwrap();
    std::fs::create_dir_all(root.join(".git/refs")).unwrap();
    std::fs::write(root.join(".git/HEAD"), "x").unwrap();
    std::fs::write(root.join(".notes.md.swp"), "x").unwrap();
    std::fs::write(root.join("server.log"), "x").unwrap();
    std::fs::create_dir_all(root.join("out/deep")).unwrap();
    std::fs::write(root.join("out/deep/a.o"), "x").unwrap();
    std::fs::write(root.join("sentinel"), "x").unwrap();
    let got = s.until_path("fs.file.created", "sentinel").await;
    assert_eq!(s.summary(&got), vec![pair("fs.file.created", "sentinel")]);
}

#[tokio::test]
async fn ten_thousand_files_are_one_burst_and_an_edit_elsewhere_still_counts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("w");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("notes.md"), "v0").unwrap();
    let probe = Arc::new(Mutex::new(FsFootprint::default()));
    let mut s = Running::start_with(
        &root,
        |c| c.quiet = Some(Duration::from_millis(500)),
        Some(probe.clone()),
    )
    .await;
    s.healthy().await;
    let started = std::time::Instant::now();
    for d in 0..100 {
        let sub = root.join(format!("dataset/part{d:02}"));
        std::fs::create_dir_all(&sub).unwrap();
        for f in 0..100 {
            std::fs::write(sub.join(format!("row{f:03}.csv")), "1,2,3\n").unwrap();
        }
        if d == 50 {
            std::fs::write(root.join("notes.md"), "edited during the storm").unwrap();
        }
    }
    let wrote = started.elapsed();
    let mut created = 0u64;
    let mut notes = false;
    let got = s
        .until(|e| {
            match e.type_.as_str() {
                "fs.burst" => created += e.data["created"].as_u64().unwrap(),
                "fs.file.created" => created += 1,
                "fs.file.modified" if path_of(e).ends_with("/notes.md") => notes = true,
                _ => {}
            }
            created >= 10_000 && notes
        })
        .await;
    let peak = *probe.lock().unwrap();
    let bursts: Vec<&NewEvent> = got.iter().filter(|e| e.type_.as_str() == "fs.burst").collect();
    eprintln!(
        "10k files written in {wrote:?}; {} events ({} bursts): {:?}; peak footprint {peak:?}",
        got.len(),
        bursts.len(),
        bursts.iter().map(|b| (&b.data["dir"], &b.data["count"], b.coalesced)).collect::<Vec<_>>()
    );
    assert_eq!(created, 10_000);
    assert!(got.len() <= 6, "{:?}", s.summary(&got));
    assert!(bursts.iter().all(|b| b.data["dir"] == format!("{}/dataset", s.root)));
    assert!(bursts.iter().all(|b| b.data["sample"].as_array().unwrap().len() <= 8));
    // Memory stayed bounded: per-path state never exceeded the pending cap, and every path in a
    // storm cost a hash entry at most.
    assert!(peak.pending <= 1024, "{peak:?}");
    assert!(peak.storms <= 16 && peak.storm_paths <= 10_100 && peak.halves <= 64, "{peak:?}");
}

#[tokio::test]
async fn a_missing_root_is_waited_for_and_a_removed_root_comes_back() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("later/w");
    let mut s = Running::start(&root, |_| {}).await;
    s.health.wait_for(|h| h.state == SourceState::Degraded).await.unwrap();
    assert!(s.health.borrow().message.as_ref().unwrap().contains("does not exist"));
    std::fs::create_dir_all(&root).unwrap();
    s.healthy().await;
    std::fs::write(root.join("a"), "x").unwrap();
    s.until_path("fs.file.created", "a").await;

    std::fs::rename(&root, dir.path().join("later/elsewhere")).unwrap();
    s.health.wait_for(|h| h.state == SourceState::Degraded).await.unwrap();
    assert!(s.health.borrow().message.as_ref().unwrap().contains("removed or moved away"));
    std::fs::rename(dir.path().join("later/elsewhere"), &root).unwrap();
    s.healthy().await;
    // The files it had are known: coming back is not a flood of creations.
    std::fs::write(root.join("b"), "x").unwrap();
    let got = s.until_path("fs.file.created", "b").await;
    assert_eq!(s.summary(&got), vec![pair("fs.file.created", "b")]);
}

#[tokio::test]
async fn non_recursive_sources_ignore_subdirectories() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("w");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    let mut s = Running::start(&root, |c| c.recursive = false).await;
    s.healthy().await;
    std::fs::write(root.join("sub/deep.txt"), "x").unwrap();
    std::fs::create_dir(root.join("new")).unwrap();
    std::fs::write(root.join("new/deeper.txt"), "x").unwrap();
    std::fs::write(root.join("top.txt"), "x").unwrap();
    let got = s.until_path("fs.file.created", "top.txt").await;
    assert_eq!(s.summary(&got), vec![pair("fs.file.created", "top.txt")]);
}

#[test]
fn config_errors_are_reported_at_build() {
    let home = Path::new("/home/me");
    let mut s = settings(Path::new("/w"));
    s.path = "relative".into();
    assert!(matches!(FsSource::new("t", &s, &env(home)), Err(SourceError::Config(_))));
    let mut s = settings(Path::new("/w"));
    s.ignore = vec!["a/**b".into()];
    assert!(matches!(FsSource::new("t", &s, &env(home)), Err(SourceError::Config(_))));
}
