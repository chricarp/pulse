//! Approvals, cancel, shutdown without orphans, and recovery after a crash.

use pulse_contracts::config::{AdapterSettings, AgentConfig, CommandSettings, StdinMode};
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
    assert!(matches!(&again, pulse_daemon::client::ClientError::Rpc(e) if e.kind() == Some(ErrorCode::Conflict)), "{again}");

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
    let agents = client.call::<method::AgentsGet>(NameParams { name: "long".into() }).await.unwrap();
    assert_eq!((agents.running, agents.queued), (1, 1), "max_concurrent = 1");

    let pid = client.call::<method::RunsGet>(RunIdParams { id: running.id }).await.unwrap().pid.unwrap();
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
    let pid = client.call::<method::RunsGet>(RunIdParams { id: running.id }).await.unwrap().pid.unwrap();
    drop(client);
    t.stop().await;
    assert!(!alive(pid), "no process outlives the daemon");
    assert!(!t.home.join("daemon.json").exists());

    // Simulate a crash on top: a run left `running` and a queued one with its outbox row.
    let mut store = Store::open(&t.home.join("pulse.db")).unwrap();
    let event = store.query().run(running.id).unwrap().unwrap().event;
    let snapshot = |cmd: &str| RunSnapshot {
        rule: None,
        agent: AgentConfig {
            name: "long".into(),
            adapter: AdapterSettings::Command(CommandSettings {
                command: vec!["sh".into(), "-c".into(), cmd.into()],
                stdin: StdinMode::None,
            }),
            allow: vec![],
            cwd: None,
            timeout: pulse_contracts::Duration::from_mins(1),
            max_concurrent: 1,
            max_queued: 8,
            env: Default::default(),
        },
    };
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
    };
    store
        .commit(&Commit {
            runs: vec![new_run(1, RunStatus::Queued), new_run(2, RunStatus::Queued)],
            outbox: vec![Effect::StartRun { run: rid(2) }],
            ..Commit::default()
        })
        .unwrap();
    let inv = pulse_contracts::run::InvocationSummary { program: "x".into(), args: vec![], cwd: "/".into() };
    store
        .transition_run(rid(1), RunTransition::Start { pid: Some(1), invocation: inv }, Timestamp::from_millis(2))
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
    assert_eq!(crashed.status, RunStatus::Interrupted, "running at a crash: interrupted, never re-run");
    // The queued run from before the restart is started by the new daemon.
    watch.until(Some(rid(2)), RunStatus::Succeeded).await;
    t.stop().await;
}
