//! Against real git in temp directories. Scenario tests call `Tracker::scan` after each command
//! (what the source does once the files settle); the last tests run the source itself.

use std::path::Path;
use std::process::Command;

use pulse_contracts::status::SourceState;
use tokio_util::sync::CancellationToken;

use super::*;

struct Git {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Git {
    fn new() -> Git {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        Git { _tmp: tmp, root }
    }

    /// Runs git in `dir` (relative to the temp root) with no user or system configuration.
    fn command(&self, dir: &str, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args(["-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(self.root.join(dir))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", &self.root)
            .env("GIT_AUTHOR_NAME", "Ada Lovelace")
            .env("GIT_AUTHOR_EMAIL", "ada@example.com")
            .env("GIT_COMMITTER_NAME", "Charles Babbage")
            .env("GIT_COMMITTER_EMAIL", "charles@example.com")
            .output()
            .unwrap()
    }

    /// Runs git with extra environment (a committer date), which must succeed.
    fn ok_env(&self, dir: &str, env: &[(&str, &str)], args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(self.root.join(dir))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", &self.root)
            .env("GIT_AUTHOR_NAME", "Ada Lovelace")
            .env("GIT_AUTHOR_EMAIL", "ada@example.com")
            .env("GIT_COMMITTER_NAME", "Charles Babbage")
            .env("GIT_COMMITTER_EMAIL", "charles@example.com")
            .envs(env.iter().copied())
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// Runs git, which may fail (a conflicting merge); returns stdout.
    fn run(&self, dir: &str, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.command(dir, args).stdout).trim().to_owned()
    }

    fn ok(&self, dir: &str, args: &[&str]) {
        let out = self.command(dir, args);
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn write(&self, rel: &str, content: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    fn sha(&self, dir: &str, rev: &str) -> String {
        self.run(dir, &["rev-parse", rev]).lines().next().unwrap().to_owned()
    }

    fn path(&self, rel: &str) -> String {
        self.root.join(rel).to_str().unwrap().to_owned()
    }

    fn tracker(&self, dir: &str) -> Tracker {
        let work = self.path(dir);
        Tracker::new(&work, repo::discover(Path::new(&work)).unwrap(), false, 0)
    }

    /// A repository with one commit on `main`.
    fn repo(&self, dir: &str) {
        std::fs::create_dir_all(self.root.join(dir)).unwrap();
        self.ok(dir, &["init", "-q"]);
        self.write(&format!("{dir}/a"), "a\n");
        self.ok(dir, &["add", "a"]);
        self.ok(dir, &["commit", "-qm", "first"]);
    }
}

/// A compact view of events for assertions.
fn show(events: &[BuiltinEvent]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            BuiltinEvent::GitCommitCreated(d) => {
                format!("commit {} {:?}", d.branch.as_deref().unwrap_or("(detached)"), d.message)
            }
            BuiltinEvent::GitBranchSwitched(d) => {
                format!("switch {} -> {}", d.from.as_deref().unwrap_or("?"), d.to)
            }
            BuiltinEvent::GitMergeConflicted(d) => format!("conflict {:?}", d.files),
            BuiltinEvent::GitPushCompleted(d) => {
                format!("push {} {} {}", d.remote.as_deref().unwrap_or("?"), d.branch, &d.sha[..7])
            }
            other => format!("{other:?}"),
        })
        .collect()
}

#[test]
fn commits_amends_switches_and_detached_heads() {
    let g = Git::new();
    g.repo("r");
    let mut t = g.tracker("r");
    assert_eq!(show(&t.scan()), Vec::<String>::new(), "history is not replayed");

    g.write("r/a", "a\nb\n");
    g.ok("r", &["commit", "-qam", "Second line\n\nWith a body."]);
    let evs = t.scan();
    assert_eq!(show(&evs), vec!["commit main \"Second line\\n\\nWith a body.\""]);
    let BuiltinEvent::GitCommitCreated(c) = &evs[0] else { unreachable!() };
    assert_eq!(c.author, "Ada Lovelace <ada@example.com>");
    assert_eq!(c.sha, g.sha("r", "HEAD"));
    assert_eq!(c.repo, g.path("r"));

    g.ok("r", &["commit", "-q", "--amend", "-m", "Second, amended"]);
    assert_eq!(show(&t.scan()), vec!["commit main \"Second, amended\""]);

    g.ok("r", &["switch", "-q", "-c", "feat"]);
    g.write("r/c", "c");
    g.ok("r", &["add", "c"]);
    g.ok("r", &["commit", "-qm", "feat c"]);
    assert_eq!(show(&t.scan()), vec!["switch main -> feat", "commit feat \"feat c\""]);

    // Switching to the branch you are on is not a switch.
    g.ok("r", &["checkout", "-q", "feat"]);
    assert_eq!(show(&t.scan()), Vec::<String>::new());

    g.ok("r", &["checkout", "-q", "--detach", "HEAD~1"]);
    g.write("r/d", "d");
    g.ok("r", &["add", "d"]);
    g.ok("r", &["commit", "-qm", "on a detached head"]);
    let detached = g.sha("r", "HEAD~1");
    assert_eq!(
        show(&t.scan()),
        vec![
            format!("switch feat -> {detached}"),
            "commit (detached) \"on a detached head\"".into()
        ]
    );
    g.ok("r", &["checkout", "-q", "main"]);
    assert_eq!(show(&t.scan()), vec![format!("switch {} -> main", g.sha("r", "HEAD@{1}"))]);
}

#[test]
fn merges_conflicts_and_rebases() {
    let g = Git::new();
    g.repo("r");
    g.ok("r", &["switch", "-q", "-c", "feat"]);
    g.write("r/a", "feat\n");
    g.write("r/dir/sub/b", "feat\n");
    g.ok("r", &["add", "-A"]);
    g.ok("r", &["commit", "-qm", "feat"]);
    g.ok("r", &["switch", "-q", "main"]);
    g.write("r/a", "main\n");
    g.write("r/dir/sub/b", "main\n");
    g.ok("r", &["add", "-A"]);
    g.ok("r", &["commit", "-qm", "main"]);
    let mut t = g.tracker("r");

    // Conflicting merge: one event per episode, however often we look.
    g.run("r", &["merge", "feat"]);
    let files = vec![g.path("r/a"), g.path("r/dir/sub/b")];
    assert_eq!(show(&t.scan()), vec![format!("conflict {files:?}")]);
    assert_eq!(show(&t.scan()), Vec::<String>::new());
    g.ok("r", &["merge", "--abort"]);
    assert_eq!(show(&t.scan()), Vec::<String>::new());

    // Again, then resolved and committed: a new episode, then the merge commit.
    g.run("r", &["merge", "feat"]);
    assert_eq!(show(&t.scan()).len(), 1);
    g.write("r/a", "resolved\n");
    g.write("r/dir/sub/b", "resolved\n");
    g.ok("r", &["add", "-A"]);
    g.ok("r", &["commit", "-q", "--no-edit"]);
    // `--no-edit` keeps git's `# Conflicts:` notes in the message; that is the real message.
    let evs = show(&t.scan());
    assert!(
        evs.len() == 1 && evs[0].starts_with("commit main \"Merge branch 'feat'\\n\\n# Conflicts:"),
        "{evs:?}"
    );

    // A fast-forward creates no commit.
    g.ok("r", &["switch", "-q", "-c", "ff"]);
    g.write("r/e", "e");
    g.ok("r", &["add", "e"]);
    g.ok("r", &["commit", "-qm", "ff work"]);
    g.ok("r", &["switch", "-q", "main"]);
    let _ = t.scan();
    g.ok("r", &["merge", "-q", "ff"]);
    assert_eq!(show(&t.scan()), Vec::<String>::new());

    // A merge that makes a merge commit is a commit.
    g.ok("r", &["switch", "-q", "-c", "side"]);
    g.write("r/f", "f");
    g.ok("r", &["add", "f"]);
    g.ok("r", &["commit", "-qm", "side work"]);
    g.ok("r", &["switch", "-q", "main"]);
    g.write("r/g", "g");
    g.ok("r", &["add", "g"]);
    g.ok("r", &["commit", "-qm", "main work"]);
    let _ = t.scan();
    g.ok("r", &["merge", "-q", "--no-edit", "side"]);
    let evs = show(&t.scan());
    assert_eq!(evs.len(), 1);
    assert!(evs[0].starts_with("commit main \"Merge branch 'side'"), "{evs:?}");

    // A clean rebase rewrites commits: nothing to report, not even the detour through a
    // detached HEAD.
    g.ok("r", &["switch", "-q", "-c", "topic", "HEAD~1"]);
    g.write("r/h", "h");
    g.ok("r", &["add", "h"]);
    g.ok("r", &["commit", "-qm", "topic work"]);
    let _ = t.scan();
    g.ok("r", &["rebase", "-q", "main"]);
    assert_eq!(show(&t.scan()), Vec::<String>::new());
    // A commit after it is on the right branch.
    g.write("r/i", "i");
    g.ok("r", &["add", "i"]);
    g.ok("r", &["commit", "-qm", "after rebase"]);
    assert_eq!(show(&t.scan()), vec!["commit topic \"after rebase\""]);

    // A rebase that stops on a conflict.
    g.write("r/a", "topic\n");
    g.ok("r", &["commit", "-qam", "topic edits a"]);
    let _ = t.scan();
    g.run("r", &["rebase", "feat"]);
    assert_eq!(show(&t.scan()), vec![format!("conflict {files:?}")]);
    g.ok("r", &["rebase", "--abort"]);
    assert_eq!(show(&t.scan()), Vec::<String>::new());
}

#[test]
fn pushes_to_a_local_bare_remote() {
    let g = Git::new();
    g.repo("r");
    g.ok("", &["init", "-q", "--bare", "remote.git"]);
    g.ok("r", &["remote", "add", "origin", "../remote.git"]);
    let mut t = g.tracker("r");
    g.ok("r", &["push", "-q", "origin", "main"]);
    let sha = g.sha("r", "HEAD");
    assert_eq!(show(&t.scan()), vec![format!("push origin main {}", &sha[..7])]);
    // Fetching is not pushing.
    g.ok("r", &["fetch", "-q", "origin"]);
    assert_eq!(show(&t.scan()), Vec::<String>::new());
    g.ok("r", &["switch", "-q", "-c", "feature/x"]);
    g.ok("r", &["commit", "-q", "--allow-empty", "-m", "x"]);
    g.ok("r", &["push", "-q", "origin", "feature/x"]);
    let evs = show(&t.scan());
    assert_eq!(evs[2], format!("push origin feature/x {}", &g.sha("r", "HEAD")[..7]), "{evs:?}");
    // Deleting the remote branch is not a completed push of a commit.
    g.ok("r", &["push", "-q", "origin", "--delete", "feature/x"]);
    assert_eq!(show(&t.scan()), Vec::<String>::new());

    // Remote names may contain a slash. The ref belongs to the longest configured remote that
    // prefixes it: `team/fork/main` is branch `main` of `team/fork`, not `fork/main` of `team`.
    g.ok("", &["init", "-q", "--bare", "fork.git"]);
    g.ok("r", &["remote", "add", "team", "../remote.git"]);
    // Written as config: newer git refuses `remote add` for a name that nests under another
    // remote, but a repository configured by an older git, or by hand, still has such remotes.
    g.ok("r", &["config", "remote.team/fork.url", "../fork.git"]);
    g.ok("r", &["config", "remote.team/fork.fetch", "+refs/heads/*:refs/remotes/team/fork/*"]);
    g.ok("r", &["switch", "-q", "main"]);
    g.ok("r", &["push", "-q", "team/fork", "main"]);
    g.ok("r", &["push", "-q", "team", "feature/x"]);
    let sha = &g.sha("r", "main")[..7];
    let evs = show(&t.scan());
    assert!(evs.contains(&format!("push team/fork main {sha}")), "{evs:?}");
    assert!(
        evs.contains(&format!("push team feature/x {}", &g.sha("r", "feature/x")[..7])),
        "{evs:?}"
    );
    // A remote-tracking ref no configured remote prefixes (its remote is defined somewhere the
    // source does not read, or was removed by hand): the remote is unknown, not guessed, and the
    // branch is the whole name under `refs/remotes/`.
    g.ok("r", &["update-ref", "-m", "update by push", "refs/remotes/ghost/x", "main"]);
    assert_eq!(show(&t.scan()), vec![format!("push ? ghost/x {sha}")]);
}

#[test]
fn linked_worktrees_packed_refs_and_packed_objects() {
    let g = Git::new();
    g.repo("r");
    g.ok("", &["init", "-q", "--bare", "remote.git"]);
    g.ok("r", &["remote", "add", "origin", "../remote.git"]);
    g.ok("r", &["worktree", "add", "-q", "-b", "wt-branch", "../wt"]);
    assert!(g.root.join("wt/.git").is_file(), "a linked worktree's .git is a file");
    let mut main = g.tracker("r");
    let mut wt = g.tracker("wt");
    assert_ne!(wt.layout().git_dir, wt.layout().common_dir);

    g.ok("wt", &["commit", "-q", "--allow-empty", "-m", "in the worktree"]);
    g.ok("r", &["commit", "-q", "--allow-empty", "-m", "in the main tree"]);
    assert_eq!(show(&wt.scan()), vec!["commit wt-branch \"in the worktree\""]);
    assert_eq!(show(&main.scan()), vec!["commit main \"in the main tree\""]);
    // Remote-tracking reflogs live in the common directory: a push from the worktree is seen
    // there (and by the main tree's source too, which shares them).
    g.ok("wt", &["push", "-q", "origin", "wt-branch"]);
    assert_eq!(show(&wt.scan()).len(), 1);

    // Packed refs: a switch to a branch that only exists in packed-refs is still a branch.
    g.ok("r", &["branch", "packed"]);
    g.ok("r", &["pack-refs", "--all"]);
    assert!(!g.root.join("r/.git/refs/heads/packed").exists());
    let _ = main.scan();
    g.ok("r", &["switch", "-q", "packed"]);
    assert_eq!(show(&main.scan()), vec!["switch main -> packed"]);

    // Once objects are packed, the message comes from the reflog's subject line.
    g.ok("r", &["commit", "-q", "--allow-empty", "-m", "subject\n\nbody that only the object has"]);
    g.ok("r", &["gc", "-q"]);
    assert_eq!(show(&main.scan()), vec!["commit packed \"subject\""]);
}

#[test]
fn index_v4_conflicts_and_bounded_messages() {
    let g = Git::new();
    g.repo("r");
    g.ok("r", &["switch", "-q", "-c", "feat"]);
    for f in ["deep/one/x.rs", "deep/one/y.rs", "deep/two/z.rs"] {
        g.write(&format!("r/{f}"), "feat\n");
    }
    g.ok("r", &["add", "-A"]);
    g.ok("r", &["commit", "-qm", "feat"]);
    g.ok("r", &["switch", "-q", "main"]);
    for f in ["deep/one/x.rs", "deep/one/y.rs", "deep/two/z.rs"] {
        g.write(&format!("r/{f}"), "main\n");
    }
    g.ok("r", &["add", "-A"]);
    g.ok("r", &["commit", "-qm", "main"]);
    g.ok("r", &["update-index", "--index-version", "4"]);
    let mut t = g.tracker("r");
    g.run("r", &["merge", "feat"]);
    let index = std::fs::read(g.root.join("r/.git/index")).unwrap();
    assert_eq!(&index[4..8], &[0, 0, 0, 4], "the index is version 4");
    let want: Vec<String> = ["deep/one/x.rs", "deep/one/y.rs", "deep/two/z.rs"]
        .iter()
        .map(|f| g.path(&format!("r/{f}")))
        .collect();
    assert_eq!(show(&t.scan()), vec![format!("conflict {want:?}")]);
    g.ok("r", &["merge", "--abort"]);
    let _ = t.scan();

    let long = "x".repeat(20_000);
    g.ok("r", &["commit", "-q", "--allow-empty", "-m", &long]);
    let evs = t.scan();
    let BuiltinEvent::GitCommitCreated(c) = &evs[0] else { panic!("{evs:?}") };
    assert_eq!(c.message.len(), MAX_MESSAGE);
}

#[test]
fn a_checkout_of_thousands_of_files_is_one_switch() {
    let g = Git::new();
    g.repo("r");
    g.ok("r", &["switch", "-q", "-c", "big"]);
    for i in 0..3000 {
        g.write(&format!("r/files/{}/{i}.txt", i % 30), "x");
    }
    g.ok("r", &["add", "-A"]);
    g.ok("r", &["commit", "-qm", "3000 files"]);
    let mut t = g.tracker("r");
    g.ok("r", &["checkout", "-q", "main"]);
    assert_eq!(show(&t.scan()), vec!["switch big -> main"]);
}

#[test]
fn reflog_times_are_not_trusted_after_the_first_scan() {
    let g = Git::new();
    g.repo("r");
    g.ok("", &["init", "-q", "--bare", "remote.git"]);
    g.ok("r", &["remote", "add", "origin", "../remote.git"]);
    // A tracker started now; the committer's clock (or GIT_COMMITTER_DATE) says 2020.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    let work = g.path("r");
    let mut t =
        Tracker::new(&work, repo::discover(Path::new(&work)).unwrap(), false, now.as_secs() as i64);
    let old = [("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")];
    g.ok("r", &["commit", "-q", "--allow-empty", "-m", "one"]);
    g.ok_env("r", &old, &["commit", "-q", "--allow-empty", "-m", "two, dated 2020"]);
    g.ok("r", &["commit", "-q", "--allow-empty", "-m", "three"]);
    g.ok_env("r", &old, &["push", "-q", "origin", "main"]);
    let sha = g.sha("r", "HEAD");
    assert_eq!(
        show(&t.scan()),
        vec![
            "commit main \"one\"".to_owned(),
            "commit main \"two, dated 2020\"".into(),
            "commit main \"three\"".into(),
            format!("push origin main {}", &sha[..7]),
        ]
    );
}

#[test]
fn a_rewritten_or_replaced_reflog_is_not_replayed() {
    let g = Git::new();
    g.repo("r");
    for i in 0..5 {
        g.ok("r", &["commit", "-q", "--allow-empty", "-m", &format!("history {i}")]);
    }
    let mut t = g.tracker("r");
    // gc rewrites the reflog (lock and rename) and keeps its lines; a commit lands before we
    // look. Only the commit is news.
    g.ok("r", &["reflog", "expire", "--expire=never", "--all"]);
    g.ok("r", &["commit", "-q", "--allow-empty", "-m", "after gc"]);
    g.ok("r", &["reflog", "expire", "--expire=never", "--all"]);
    assert_eq!(show(&t.scan()), vec!["commit main \"after gc\""]);
    // Expired to nothing, then a commit: the commit.
    g.ok("r", &["reflog", "expire", "--expire=now", "--expire-unreachable=now", "--all"]);
    let _ = t.scan();
    g.ok("r", &["commit", "-q", "--allow-empty", "-m", "after expiry"]);
    assert_eq!(show(&t.scan()), vec!["commit main \"after expiry\""]);

    // The repository is replaced at the same path between two looks (deleted and cloned, or
    // another .git copied in) by one with more history: none of it is news.
    g.repo("other");
    for i in 0..20 {
        g.ok("other", &["commit", "-q", "--allow-empty", "-m", &format!("other {i}")]);
    }
    std::fs::remove_dir_all(g.root.join("r/.git")).unwrap();
    let copied = Command::new("cp")
        .arg("-a")
        .arg(g.root.join("other/.git"))
        .arg(g.root.join("r/.git"))
        .status()
        .unwrap();
    assert!(copied.success());
    assert_eq!(show(&t.scan()), Vec::<String>::new());
    g.ok("r", &["commit", "-q", "--allow-empty", "-m", "in the new one"]);
    assert_eq!(show(&t.scan()), vec!["commit main \"in the new one\""]);
}

#[test]
fn a_commit_subject_cannot_set_the_branch() {
    let g = Git::new();
    g.repo("r");
    g.ok("r", &["switch", "-q", "-c", "topic"]);
    g.ok("r", &["commit", "-q", "--allow-empty", "-m", "fix): returning to refs/heads/evil"]);
    g.ok("r", &["switch", "-q", "main"]);
    g.ok("r", &["commit", "-q", "--allow-empty", "-m", "main moves"]);
    g.ok("r", &["switch", "-q", "topic"]);
    let mut t = g.tracker("r");
    // The pick's reflog line carries the subject; a commit made during the rebase (by --exec)
    // follows it in the same batch.
    g.ok("r", &["rebase", "-q", "--exec", "git commit -q --allow-empty -m during", "main"]);
    let evs = show(&t.scan());
    assert!(evs.iter().all(|e| !e.contains("evil")), "{evs:?}");
    assert_eq!(evs.iter().filter(|e| e.contains("during")).count(), 1, "{evs:?}");
}

// ---- The source itself ----

async fn start(
    work: &Path,
) -> (
    mpsc::Receiver<NewEvent>,
    tokio::sync::watch::Receiver<pulse_contracts::status::SourceHealth>,
    CancellationToken,
) {
    let env = SourceEnv { home: work.into(), pulse_home: work.join("ph") };
    let src = GitSource::new("g", &GitSourceSettings { path: work.to_str().unwrap().into() }, &env)
        .unwrap();
    let stop = CancellationToken::new();
    let (cx, rx, health) =
        crate::context(src.source.clone(), 16, stop.clone(), pulse_contracts::Timestamp::EPOCH);
    tokio::spawn(Box::new(src).run(cx));
    (rx, health, stop)
}

#[tokio::test]
async fn the_source_waits_for_a_repository_and_reports_from_its_first_commit() {
    let g = Git::new();
    let work = g.root.join("later");
    let (mut rx, mut health, stop) = start(&work).await;
    health.wait_for(|h| h.state == SourceState::Degraded).await.unwrap();
    std::fs::create_dir(&work).unwrap();
    g.ok("later", &["init", "-q"]);
    health.wait_for(|h| h.state == SourceState::Healthy).await.unwrap();
    g.ok("later", &["commit", "-q", "--allow-empty", "-m", "the very first"]);
    let ev = rx.recv().await.unwrap();
    assert_eq!(ev.type_.as_str(), "git.commit.created");
    assert_eq!(ev.data["message"], "the very first");
    assert_eq!(ev.data["branch"], "main");
    assert_eq!(ev.subject.as_deref(), Some(work.to_str().unwrap()));

    // Thousands of files in one checkout: one switch, through the real watcher.
    g.ok("later", &["switch", "-q", "-c", "big"]);
    for i in 0..2000 {
        g.write(&format!("later/f/{}/{i}", i % 20), "x");
    }
    g.ok("later", &["add", "-A"]);
    g.ok("later", &["commit", "-qm", "many"]);
    g.ok("later", &["checkout", "-q", "main"]);
    g.ok("later", &["commit", "-q", "--allow-empty", "-m", "sentinel"]);
    let mut types = Vec::new();
    loop {
        let ev = rx.recv().await.unwrap();
        let done = ev.data["message"] == "sentinel";
        types.push(ev.type_.to_string());
        if done {
            break;
        }
    }
    assert_eq!(
        types,
        ["git.branch.switched", "git.commit.created", "git.branch.switched", "git.commit.created"]
    );
    stop.cancel();
}
