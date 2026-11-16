//! Storms: 10,000 files unzipped into a watched folder, and a real `git clone` and `git
//! checkout` inside one. The daemon must stay small in what it emits and runs, and responsive.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use pulse_contracts::rpc::{Empty, EventFilter, method};
use pulse_contracts::status::SourceState;

use crate::support::*;

/// `quiet` is the fs source's quiet period: storms fold when enough changes are pending at once,
/// so a short one splits a slow storm (a clone on a busy machine) into single files.
fn config(quiet: &str, extra_sources: &str, extra_rules: &str) -> String {
    format!(
        r#"
[[source]]
name = "watched"
kind = "fs"
path = "~/watched"
quiet = "{quiet}"
{extra_sources}

[[agent]]
name = "quick"
adapter = "command"
command = ["true"]
allow = ["fs.*:~/watched/**", "git.*:~/watched/**"]

[[rule]]
name = "new-file"
on = "fs.file.created"
where = {{ path = "~/watched/**" }}
run = {{ agent = "quick" }}
{extra_rules}
"#
    )
}

/// `system.status` round trips, as fast as they complete, until stopped: (count, slowest).
fn probe(
    client: pulse_daemon::client::Client,
) -> (Arc<AtomicBool>, tokio::task::JoinHandle<(u32, Duration)>) {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let task = tokio::spawn(async move {
        let (mut n, mut worst) = (0u32, Duration::ZERO);
        while !flag.load(Ordering::SeqCst) {
            let t = Instant::now();
            client.call::<method::SystemStatusGet>(Empty {}).await.unwrap();
            worst = worst.max(t.elapsed());
            n += 1;
        }
        (n, worst)
    });
    (stop, task)
}

fn python(script: &str, args: &[&Path]) {
    let ok = std::process::Command::new("python3")
        .arg("-I")
        .arg("-c")
        .arg(script)
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "python {script}");
}

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// Definition of done: a 10,000-file archive unzipped into a watched folder that has a rule on
/// `fs.file.created` gives a few events and runs, a live subscriber gets bounded batches, and
/// the daemon keeps answering RPC promptly while it happens.
#[tokio::test(flavor = "multi_thread")]
async fn unzipping_ten_thousand_files_is_a_few_events_and_the_daemon_stays_responsive() {
    let env = Env::new();
    let watched = env.mkdir("watched");
    let zip = env.dir.path().join("dataset.zip");
    python(
        "import sys, zipfile\n\
         with zipfile.ZipFile(sys.argv[1], 'w') as z:\n\
         \x20   for d in range(100):\n\
         \x20       for f in range(100):\n\
         \x20           z.writestr(f'dataset/part{d:02}/{f:04}.csv', 'a,b\\n1,2\\n')\n",
        &[&zip],
    );
    env.write_config(&config("200ms", "", ""));
    let daemon = env.start().await;
    let mut watch = daemon.watch().await;
    watch.sources(SourceState::Healthy).await;
    let (stop, probe) = probe(daemon.client().await);

    let started = Instant::now();
    python(
        "import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])",
        &[&zip, &watched],
    );
    std::fs::write(watched.join("notes.md"), "an unrelated edit").unwrap();
    watch
        .until("10,000 creations and the unrelated edit", |w| {
            let created: u64 = w
                .events
                .iter()
                .filter(|e| e.type_.as_str() == "fs.burst")
                .filter_map(|e| e.data["created"].as_u64())
                .sum();
            let notes = w
                .events
                .iter()
                .any(|e| e.subject.as_deref().is_some_and(|s| s.ends_with("/notes.md")));
            (created >= 10_000 && notes).then_some(())
        })
        .await;
    let settled = started.elapsed();
    stop.store(true, Ordering::SeqCst);
    let (calls, worst) = probe.await.unwrap();

    let events = watch.query(EventFilter::default()).await;
    let runs = watch.all_runs().await;
    println!(
        "10k unzip: settled in {settled:?}; {} events, {} runs; {} live batches (largest {}, dropped {}); \
         system.status: {calls} calls, slowest {worst:?}",
        events.len(),
        runs.len(),
        watch.batches,
        watch.max_batch,
        watch.dropped,
    );
    assert!(
        events.len() <= 4,
        "{:#?}",
        events.iter().map(|e| (&e.type_, &e.subject)).collect::<Vec<_>>()
    );
    // Only the unrelated file is a creation a rule sees; the storm is one `fs.burst`.
    assert_eq!(runs.len(), 1, "{runs:#?}");
    assert!(watch.max_batch <= 200);
    assert!(calls > 10, "the probe made progress during the storm");
    assert!(worst < Duration::from_secs(1), "system.status took {worst:?} during the storm");
    daemon.stop().await;
}

/// A real `git clone` into a watched folder that also has a git source for the clone, then a
/// `git checkout` that rewrites most of the tree.
#[tokio::test(flavor = "multi_thread")]
async fn a_clone_and_a_checkout_in_a_watched_folder_stay_bounded() {
    let env = Env::new();
    let watched = env.mkdir("watched");
    // The default quiet period, as a user has it.
    env.write_config(&config(
        "2s",
        "\n[[source]]\nname = \"repo\"\nkind = \"git\"\npath = \"~/watched/repo\"\n",
        "\n[[rule]]\nname = \"switched\"\non = \"git.branch.switched\"\nrun = { agent = \"quick\" }\n",
    ));
    let daemon = env.start().await;
    let mut watch = daemon.watch().await;
    watch.source_state(Some("watched"), SourceState::Healthy).await;
    let (stop, probe) = probe(daemon.client().await);
    let this_repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let repo = watched.join("repo");
    let out = std::process::Command::new("git")
        .args(["clone", "--quiet", "--no-hardlinks"])
        .arg(&this_repo)
        .arg(&repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    watch.sources(SourceState::Healthy).await;
    let first = String::from_utf8(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["rev-list", "--max-parents=0", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    git(&repo, &["checkout", "--quiet", first.trim()]);
    let switched =
        watch.event("git.branch.switched", |e| e.type_.as_str() == "git.branch.switched").await;
    assert_eq!(switched.data["to"], first.trim());
    // Let the checkout's file changes settle: a marker written after it is seen last.
    std::fs::write(watched.join("marker.txt"), "after the checkout").unwrap();
    watch
        .event("the marker", |e| e.subject.as_deref().is_some_and(|s| s.ends_with("/marker.txt")))
        .await;
    stop.store(true, Ordering::SeqCst);
    let (calls, worst) = probe.await.unwrap();

    let events = watch.query(EventFilter::default()).await;
    let fs = events.iter().filter(|e| e.type_.as_str().starts_with("fs.")).count();
    let git_events: Vec<_> =
        events.iter().filter(|e| e.type_.as_str().starts_with("git.")).collect();
    let runs = watch.all_runs().await;
    println!(
        "clone + checkout: {} events ({fs} fs, {} git), {} runs; system.status: {calls} calls, slowest {worst:?}",
        events.len(),
        git_events.len(),
        runs.len()
    );
    for r in &runs {
        let e = watch.detail(r.event).await.event;
        println!("  run {:?} for {} {:?}", r.rule, e.type_, e.subject);
    }
    assert_eq!(git_events.len(), 1, "{git_events:#?}");
    // The clone (about 300 files) and the checkout each settle as one `fs.burst`, plus the
    // marker. Runs: the git rule's, the marker's, and whatever top-level files settled on
    // their own.
    assert!(
        fs <= 10,
        "{fs} fs events: {:#?}",
        events.iter().map(|e| (&e.type_, &e.subject)).collect::<Vec<_>>()
    );
    assert!(runs.len() <= 10, "{} runs", runs.len());
    assert!(worst < Duration::from_secs(1));
    daemon.stop().await;
}
