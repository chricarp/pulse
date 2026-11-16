//! Approvals, cancel, shutdown without orphans, and recovery after a crash.

use pulse_contracts::config::AgentConfig;
use pulse_contracts::rpc::*;
use pulse_contracts::run::RunSnapshot;
use pulse_contracts::{RunStatus, Timestamp, Ulid};
use pulse_store::{Commit, Effect, NewRun, RunTransition, Store};

use crate::support::*;

const LONG_AGENT: &str = r#"
[[agent]]
name = "long"
adapter = "command"
# exec, so the pid we record is the process itself.
command = ["sh", "-c", "echo started; exec sleep 600"]
allow = ["ext.*"]
max_concurrent = 1
"#;

fn ext(type_: &str) -> IngestParams {
    IngestParams {
        source: "t".into(),
        events: vec![IngestEvent {
            type_: type_.into(),
            subject: None,
            time: None,
            data: serde_json::json!({}),
            dedupe_key: None,
            causation_run: None,
        }],
    }
}

fn alive(pid: u32) -> bool {
    rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid as i32).unwrap()).is_ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervised_runs_wait_for_approval() {
    let mut t = TestDaemon::start(&format!(
        r#"{COMMAND_AGENT}
[[rule]]
name = "careful"
on = "ext.*"
supervised = true
run = {{ agent = "sh" }}
"#
    ))
    .await;
    let mut watch = RunWatch::new(t.client().await).await;
    let client = t.client().await;
    let mut notes = client.notifications().unwrap();
    client.call::<method::EventsIngest>(ext("one")).await.unwrap();
    client.call::<method::EventsIngest>(ext("two")).await.unwrap();
    let a = watch.until(None, RunStatus::PendingApproval).await;
    let b = loop {
        let r = watch.until(None, RunStatus::PendingApproval).await;
        if r.id != a.id {
            break r;
        }
        watch.seen.retain(|s| s.id != a.id);
    };
    let status = client.call::<method::SystemStatusGet>(Empty {}).await.unwrap();
    assert_eq!(status.runs.pending_approval, 2);
    // Every connection is told when run counts change, so a client that shows "2 waiting for
    // approval" (the desktop shell's badge) needs no run subscription.
    within("state.changed with the new counts", async {
        loop {
            if let Notification::StateChanged(c) = notes.recv().await.unwrap() {
                if c.status.runs.pending_approval == 2 {
                    return;
                }
            }
        }
    })
    .await;
    assert_eq!(status.runs.running, 0, "nothing runs before approval");

    let approved = client.call::<method::RunsApprove>(RunIdParams { id: a.id }).await.unwrap();
    assert_eq!(approved.status, RunStatus::Queued);
    let done = watch.until(Some(a.id), RunStatus::Succeeded).await;
    assert_eq!(done.exit_code, Some(0));

    let rejected = client
        .call::<method::RunsReject>(RejectParams { id: b.id, reason: Some("not now".into()) })
        .await
        .unwrap();
    assert_eq!(rejected.status, RunStatus::Rejected);
    let full = client.call::<method::RunsGet>(RunIdParams { id: b.id }).await.unwrap();
    assert_eq!(full.error.as_deref(), Some("not now"));
    let again = client.call::<method::RunsApprove>(RunIdParams { id: b.id }).await.unwrap_err();
    assert!(
        matches!(&again, pulse_daemon::client::ClientError::Rpc(e) if e.kind() == Some(ErrorCode::Conflict)),
        "{again}"
    );

    // A rerun of the approved run starts at once: the user asked for it.
    let rerun = client.call::<method::RunsRerun>(RunIdParams { id: a.id }).await.unwrap();
    let rerun_done = watch.until(Some(rerun.id), RunStatus::Succeeded).await;
    let full = client.call::<method::RunsGet>(RunIdParams { id: rerun_done.id }).await.unwrap();
    assert_eq!(full.rerun_of, Some(a.id));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_terminates_and_queued_runs_wait_their_turn() {
    let mut t = TestDaemon::start(&format!(
        r#"{LONG_AGENT}
[[rule]]
name = "slow"
on = "ext.*"
run = {{ agent = "long" }}
"#
    ))
    .await;
    let mut watch = RunWatch::new(t.client().await).await;
    let client = t.client().await;
    client.call::<method::EventsIngest>(ext("one")).await.unwrap();
    let running = watch.until(None, RunStatus::Running).await;
    client.call::<method::EventsIngest>(ext("two")).await.unwrap();
    let queued = loop {
        let r = watch.until(None, RunStatus::Queued).await;
        if r.id != running.id {
            break r;
        }
        watch.seen.retain(|s| s.id != r.id || s.status != RunStatus::Queued);
    };
    let agents =
        client.call::<method::AgentsGet>(NameParams { name: "long".into() }).await.unwrap();
    assert_eq!((agents.running, agents.queued), (1, 1), "max_concurrent = 1");

    let pid =
        client.call::<method::RunsGet>(RunIdParams { id: running.id }).await.unwrap().pid.unwrap();
    assert!(alive(pid));
    client.call::<method::RunsCancel>(RunIdParams { id: running.id }).await.unwrap();
    watch.until(Some(running.id), RunStatus::Cancelled).await;
    assert!(!alive(pid), "the process was terminated and reaped");

    // The slot is free: the queued run starts. Cancel it while it runs too.
    watch.until(Some(queued.id), RunStatus::Running).await;
    client.call::<method::RunsCancel>(RunIdParams { id: queued.id }).await.unwrap();
    watch.until(Some(queued.id), RunStatus::Cancelled).await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_terminates_runs_and_restart_recovers() {
    let config = format!(
        r#"{LONG_AGENT}
[[rule]]
name = "slow"
on = "ext.*"
run = {{ agent = "long" }}
"#
    );
    let mut t = TestDaemon::start(&config).await;
    let mut watch = RunWatch::new(t.client().await).await;
    let client = t.client().await;
    client.call::<method::EventsIngest>(ext("one")).await.unwrap();
    let running = watch.until(None, RunStatus::Running).await;
    let pid =
        client.call::<method::RunsGet>(RunIdParams { id: running.id }).await.unwrap().pid.unwrap();
    drop(client);
    t.stop().await;
    assert!(!alive(pid), "no process outlives the daemon");
    assert!(!t.home.join("daemon.json").exists());

    // Simulate a crash on top: a run left `running` and a queued one with its outbox row. The
    // queued run was created with the agent as it is configured now (a run never starts with
    // another definition).
    let resumed = config.replace("echo started; exec sleep 600", "echo resumed");
    t.write_config(&resumed);
    let mut store = Store::open(&t.home.join("pulse.db")).unwrap();
    let event = store.query().run(running.id).unwrap().unwrap().event;
    let file: pulse_contracts::config::ConfigFile = toml::from_str(&resumed).unwrap();
    let snapshot = |_: &str| RunSnapshot { rule: None, agent: file.agents[0].clone() };
    let rid = |n| pulse_contracts::RunId::from_ulid(Ulid::from_parts(n, 1));
    let new_run = |n, status| NewRun {
        id: rid(n),
        rule: Some("slow".into()),
        agent: "long".into(),
        event,
        status,
        created_at: Timestamp::from_millis(1),
        prompt: None,
        chain_depth: 1,
        rerun_of: None,
        snapshot: snapshot("echo resumed"),
        invocation: None,
    };
    store
        .commit(&Commit {
            runs: vec![new_run(1, RunStatus::Queued), new_run(2, RunStatus::Queued)],
            outbox: vec![Effect::StartRun { run: rid(2) }],
            ..Commit::default()
        })
        .unwrap();
    let inv = pulse_contracts::run::InvocationSummary {
        program: "x".into(),
        args: vec![],
        cwd: "/".into(),
    };
    store
        .transition_run(
            rid(1),
            RunTransition::Start { pid: Some(1), invocation: inv },
            Timestamp::from_millis(2),
        )
        .unwrap();
    drop(store);

    t.boot(Setup::default()).await;
    let mut watch = RunWatch::new(t.client().await).await;
    let client = t.client().await;
    let get = |id| client.call::<method::RunsGet>(RunIdParams { id });
    let stopped = get(running.id).await.unwrap();
    assert_eq!(stopped.status, RunStatus::Interrupted);
    assert_eq!(stopped.error.as_deref(), Some("the daemon shut down"));
    let crashed = get(rid(1)).await.unwrap();
    assert_eq!(
        crashed.status,
        RunStatus::Interrupted,
        "running at a crash: interrupted, never re-run"
    );
    // The queued run from before the restart is started by the new daemon, possibly before the
    // watch subscribed: then the store already says so.
    if get(rid(2)).await.unwrap().status != RunStatus::Succeeded {
        watch.until(Some(rid(2)), RunStatus::Succeeded).await;
    }
    t.stop().await;
}

fn withdrawn(run: &pulse_contracts::Run, what: &str) {
    assert_eq!(run.status, RunStatus::Cancelled, "{run:?}");
    assert!(run.started_at.is_none() && run.pid.is_none(), "nothing ran: {run:?}");
    let error = run.error.as_deref().unwrap_or_default();
    assert!(
        error.starts_with(&format!("the {what} was removed or changed"))
            || error.starts_with(&format!("the {what} was removed, archived")),
        "{error}"
    );
    assert!(error.contains("replay the event"), "{error}");
}

/// A run never starts with an agent definition that is not the current one, at each point
/// where it is released: `queued → running`, approval, and rerun. And what an approval shows
/// (the invocation rendered when the run was created) is what runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runs_start_only_with_the_current_agent() {
    let config = format!(
        r#"{LONG_AGENT}
[[agent]]
name = "echo"
adapter = "command"
command = ["echo", "--", "{{{{subject}}}}"]
allow = ["ext.*"]

[[rule]]
name = "slow"
on = "ext.t.slow"
run = {{ agent = "long" }}

[[rule]]
name = "careful"
on = "ext.t.careful"
supervised = true
run = {{ agent = "echo" }}
"#
    );
    let mut t = TestDaemon::start(&config).await;
    let mut watch = RunWatch::new(t.client().await).await;
    let client = t.client().await;
    let get = |id| client.call::<method::RunsGet>(RunIdParams { id });
    let agent = |name: &str| client.call::<method::AgentsGet>(NameParams { name: name.into() });
    let replace = |agent: AgentConfig| {
        client.call::<method::AgentsUpsert>(AgentUpsertParams {
            agent,
            project: None,
            expected: Expected::Any,
        })
    };
    let ingest = |type_: &str, subject: &str| {
        let mut p = ext(type_);
        p.events[0].subject = Some(subject.into());
        client.call::<method::EventsIngest>(p)
    };

    // Queued → running. One run holds the only slot; two wait. The agent is edited meanwhile.
    ingest("slow", "a").await.unwrap();
    let running = watch.until(None, RunStatus::Running).await;
    ingest("slow", "b").await.unwrap();
    ingest("slow", "c").await.unwrap();
    let waiting = client
        .call::<method::RunsList>(RunsListParams {
            filter: RunFilter { status: vec![RunStatus::Queued], ..RunFilter::default() },
            ..RunsListParams::default()
        })
        .await
        .unwrap()
        .runs;
    assert_eq!(waiting.len(), 2);
    let mut long = agent("long").await.unwrap().config;
    long.adapter = toml::from_str::<AgentConfig>(
        "name = \"long\"\nadapter = \"command\"\ncommand = [\"sh\", \"-c\", \"echo changed\"]",
    )
    .unwrap()
    .adapter;
    replace(long).await.unwrap();
    client.call::<method::RunsCancel>(RunIdParams { id: running.id }).await.unwrap();
    for r in &waiting {
        watch.until(Some(r.id), RunStatus::Cancelled).await;
        withdrawn(&get(r.id).await.unwrap(), "agent");
    }

    // Approval. The pending run shows the argv it will run, with the event's values in it.
    ingest("careful", "--x y").await.unwrap();
    let pending = watch.until(None, RunStatus::PendingApproval).await;
    let shown = get(pending.id).await.unwrap().invocation.expect("rendered at creation");
    assert_eq!(
        (shown.program.as_str(), shown.args.clone()),
        ("echo", vec!["--".into(), "--x y".into()])
    );
    // Approved unchanged: what was shown is what ran.
    client.call::<method::RunsApprove>(RunIdParams { id: pending.id }).await.unwrap();
    watch.until(Some(pending.id), RunStatus::Succeeded).await;
    assert_eq!(get(pending.id).await.unwrap().invocation, Some(shown));
    // Changed before approval: approving is refused and the run ends without starting.
    ingest("careful", "z").await.unwrap();
    let pending = loop {
        let r = watch.until(None, RunStatus::PendingApproval).await;
        if r.id != pending.id {
            break r;
        }
        watch.seen.retain(|s| s.id != r.id);
    };
    let mut echo = agent("echo").await.unwrap().config;
    echo.allow.push("fs.*".parse().unwrap());
    replace(echo.clone()).await.unwrap();
    let err = client.call::<method::RunsApprove>(RunIdParams { id: pending.id }).await.unwrap_err();
    assert!(
        matches!(&err, pulse_daemon::client::ClientError::Rpc(e) if e.kind() == Some(ErrorCode::Conflict) && e.message.contains("not approved")),
        "{err}"
    );
    withdrawn(&get(pending.id).await.unwrap(), "agent");

    // Rerun: the stored prompt with the agent as it is now, never the old snapshot; refused
    // once the agent is gone.
    let first = waiting[0].id;
    let rerun = client.call::<method::RunsRerun>(RunIdParams { id: first }).await.unwrap();
    let rerun = get(rerun.id).await.unwrap();
    assert_eq!(rerun.snapshot.agent, agent("long").await.unwrap().config);
    assert_eq!(rerun.invocation.unwrap().args, vec!["-c".to_owned(), "echo changed".into()]);
    watch.until(Some(rerun.id), RunStatus::Succeeded).await;
    let config = t.read_config();
    let without = config[..config.find("[[rule]]\nname = \"careful\"").unwrap()]
        .replace("[[agent]]\nname = \"echo\"", "[[agent]]\nname = \"unused\"");
    let mut notes = client.notifications().unwrap();
    t.write_config(&without);
    within("the echo agent to be gone", async {
        loop {
            if let Notification::StateChanged(c) = notes.recv().await.unwrap() {
                if c.topics.contains(&StateTopic::Agents) && agent("echo").await.is_err() {
                    return;
                }
            }
        }
    })
    .await;
    let err = client.call::<method::RunsRerun>(RunIdParams { id: pending.id }).await.unwrap_err();
    assert!(err.to_string().contains("no longer exists"), "{err}");
    t.stop().await;
}

/// The run is recorded as running before its process is spawned, so a crash in between leaves
/// a `running` run (recovered as `interrupted`) and never a `queued` one whose start would be
/// retried: that would run the agent twice, the first copy orphaned in its own process group.
/// The engine knows of it from then too, so a file the agent writes in its first moment is
/// already attributed to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_is_running_before_its_process_is_spawned() {
    let (tx, mut spawned) = tokio::sync::mpsc::unbounded_channel();
    let setup = Setup { supervisor: std::sync::Arc::new(SpawnGate(tx)), ..Setup::default() };
    let mut t = TestDaemon::start_with(
        &format!(
            "[[source]]\nname = \"fake\"\nkind = \"fs\"\npath = \"/\"\n{COMMAND_AGENT}\n[[rule]]\nname = \"r\"\non = \"ext.*\"\nrun = {{ agent = \"sh\" }}\n"
        ),
        setup,
    )
    .await;
    let client = t.client().await;
    let mut notes = client.notifications().unwrap();
    client.call::<method::EventsSubscribe>(EventsSubscribeParams::default()).await.unwrap();
    client.call::<method::EventsIngest>(ext("one")).await.unwrap();
    let id = within("the spawn", spawned.recv()).await.unwrap();
    let run = client.call::<method::RunsGet>(RunIdParams { id }).await.unwrap();
    assert_eq!(run.status, RunStatus::Running);
    assert!(run.started_at.is_some() && run.pid.is_none(), "{run:?}");

    // The process has not even been spawned; a write in its folder is already its own.
    let cwd = run.invocation.unwrap().cwd;
    let path = format!("{cwd}/out.txt");
    t.sources.push("fake", file_created("fake", &path)).await;
    let written = within("the file event", async {
        loop {
            if let Notification::EventsBatch(b) = notes.recv().await.unwrap() {
                if let Some(e) =
                    b.batch.events.into_iter().find(|e| e.subject.as_deref() == Some(&path))
                {
                    return e;
                }
            }
        }
    })
    .await;
    assert_eq!(written.causation_run, Some(id));
    assert_eq!(written.causation, Some(pulse_contracts::Causation::Inferred));
    t.stop().await;
}

/// The agent's `cwd` reaches the watched folder through a symbolic link, and the fs source
/// reports the real path. Inference compares the event with every spelling of the run's working
/// directory the daemon knows, as reported writes do.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inference_sees_a_working_directory_through_a_symlink() {
    let dirs = tempfile::tempdir().unwrap();
    let real = dirs.path().canonicalize().unwrap().join("real");
    let link = dirs.path().canonicalize().unwrap().join("link");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let (tx, mut spawned) = tokio::sync::mpsc::unbounded_channel();
    let setup = Setup { supervisor: std::sync::Arc::new(SpawnGate(tx)), ..Setup::default() };
    let agent = format!(
        "[[agent]]\nname = \"sh\"\nadapter = \"command\"\ncommand = [\"true\"]\nallow = [\"ext.*\"]\ncwd = \"{}\"\n",
        link.display()
    );
    let mut t = TestDaemon::start_with(
        &format!(
            "[[source]]\nname = \"fake\"\nkind = \"fs\"\npath = \"/\"\n{agent}\n[[rule]]\nname = \"r\"\non = \"ext.*\"\nrun = {{ agent = \"sh\" }}\n"
        ),
        setup,
    )
    .await;
    let client = t.client().await;
    let mut notes = client.notifications().unwrap();
    client.call::<method::EventsSubscribe>(EventsSubscribeParams::default()).await.unwrap();
    client.call::<method::EventsIngest>(ext("one")).await.unwrap();
    let id = within("the spawn", spawned.recv()).await.unwrap();

    let path = real.join("out.txt").display().to_string();
    t.sources.push("fake", file_created("fake", &path)).await;
    let written = within("the file event", async {
        loop {
            if let Notification::EventsBatch(b) = notes.recv().await.unwrap() {
                if let Some(e) =
                    b.batch.events.into_iter().find(|e| e.subject.as_deref() == Some(&path))
                {
                    return e;
                }
            }
        }
    })
    .await;
    assert_eq!(written.causation_run, Some(id));
    assert_eq!(written.causation, Some(pulse_contracts::Causation::Inferred));
    t.stop().await;
}
