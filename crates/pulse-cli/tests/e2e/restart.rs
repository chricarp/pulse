//! Restart safety: stop the daemon (gracefully, or by SIGKILL) with runs running and queued,
//! start it again, and check what survived.

use pulse_contracts::rpc::EventFilter;
use pulse_contracts::status::SourceState;
use pulse_contracts::{BlockReason, DeliveryOutcome, Event, RunStatus};

use crate::support::*;

fn config(env: &Env) -> String {
    format!(
        r#"
[limits]
causation_grace = "30s"

[[source]]
name = "watched"
kind = "fs"
path = "~/watched"
quiet = "100ms"

# Makes a fifo named after its run, says "ready", blocks until the test writes a line to it,
# then writes a file where its rule watches. Its working directory is a subfolder, so the test's
# own files (outside it) are not taken for the agent's.
[[agent]]
name = "writer"
adapter = "command"
command = ["sh", "-c", "f=\"$FIFOS/$PULSE_RUN_ID\"; mkfifo \"$f\"; echo ready; read go < \"$f\"; echo again > \"$LOOP_DIR/out-$PULSE_RUN_ID.txt\""]
allow = ["fs.*:~/watched/**"]
cwd = "~/watched/out"
max_concurrent = 1
env = {{ LOOP_DIR = {dir:?}, FIFOS = {fifos:?} }}

[[agent]]
name = "quick"
adapter = "command"
command = ["true"]
allow = ["ext.*"]
max_concurrent = 2

[[rule]]
name = "on-new-file"
on = "fs.file.created"
where = {{ path = "~/watched/**" }}
run = {{ agent = "writer" }}

[[rule]]
name = "cool"
on = "ext.e2e.ping"
cooldown = "1h"
run = {{ agent = "quick" }}

[[rule]]
name = "rated"
on = "ext.e2e.ping"
rate_limit = "1/hour"
run = {{ agent = "quick" }}
"#,
        dir = env.user_path("watched/out").display().to_string(),
        fifos = env.dir.path().display().to_string(),
    )
}

fn outcome(e: &pulse_contracts::rpc::EventDetail, rule: &str) -> Option<DeliveryOutcome> {
    e.deliveries.iter().find(|d| d.rule == rule).map(|d| d.outcome.clone())
}

/// Lets run `id`'s agent continue (it said "ready" before).
async fn release(env: &Env, id: pulse_contracts::RunId) {
    let mut fifo = Fifo::existing(env.dir.path().join(id.to_string()));
    fifo.open().await;
    fifo.write("go\n");
    fifo.close();
}

async fn ping(env: &Env, key: &str) -> (i32, String) {
    let line = format!("{{\"type\":\"ping\",\"subject\":\"p\",\"dedupe_key\":{key:?}}}\n");
    let (code, out, err) = env.run_stdin(&["--json", "ingest", "-s", "e2e"], &line).await;
    assert_eq!(code, 0, "{err}");
    (code, out)
}

async fn created(watch: &mut Watch, name: &str) -> Event {
    let name = format!("/{name}");
    watch
        .event(&format!("fs.file.created for {name}"), |e| {
            e.type_.as_str() == "fs.file.created"
                && e.subject.as_deref().is_some_and(|s| s.ends_with(&name))
        })
        .await
}

/// A crash of the daemon process (SIGKILL, not of the OS: see 0004 on power loss) with one run
/// running and one queued, an acknowledged ingest, and limits in force.
/// After the restart: the running run is `interrupted`, the queued one runs, nothing that was
/// acknowledged is lost or fires twice, a dedupe key still dedupes, cooldown and rate limit
/// still hold, and the orphaned agent's late write is still recognised as its own.
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_loses_nothing_committed_and_fires_nothing_twice() {
    let env = Env::new();
    env.mkdir("watched/out");
    env.write_config(&config(&env));
    let daemon = env.start().await;
    let mut watch = daemon.watch().await;
    watch.sources(SourceState::Healthy).await;

    let (_, out) = ping(&env, "ping-1").await;
    assert!(out.contains("\"accepted\": 1"), "{out}");
    for rule in ["cool", "rated"] {
        let r = watch.run_in(rule, |r| r.rule.as_deref() == Some(rule)).await;
        watch.run_status(r.id, RunStatus::Succeeded).await;
    }
    std::fs::write(env.user_path("watched/seed-1.txt"), "1").unwrap();
    let first =
        watch.run_in("the first writer run", |r| r.rule.as_deref() == Some("on-new-file")).await;
    watch.output_line(first.id, "ready").await;
    std::fs::write(env.user_path("watched/seed-2.txt"), "2").unwrap();
    let second = watch
        .run_in("the second writer run, queued", |r| {
            r.rule.as_deref() == Some("on-new-file")
                && r.id != first.id
                && r.status == RunStatus::Queued
        })
        .await;
    drop(watch);
    daemon.crash().await;

    let daemon = env.start().await;
    let mut watch = daemon.watch().await;
    let interrupted = watch.get_run(first.id).await;
    assert_eq!(interrupted.status, RunStatus::Interrupted);
    assert!(interrupted.error.as_deref().is_some_and(|e| e.contains("stopped")), "{interrupted:?}");
    watch.output_line(second.id, "ready").await;
    watch.sources(SourceState::Healthy).await;
    release(&env, second.id).await;
    let out = created(&mut watch, &format!("out-{}.txt", second.id)).await;
    let detail = watch.detail(out.id).await;
    assert_eq!(
        outcome(&detail, "on-new-file"),
        Some(DeliveryOutcome::Blocked { reason: BlockReason::LoopSelfTrigger { run: second.id } }),
    );
    watch.run_status(second.id, RunStatus::Succeeded).await;
    // The first run's agent survived the crash (it is in its own process group); let it end.
    release(&env, first.id).await;

    // An external source retrying after the outage does not fire twice.
    let (_, out) = ping(&env, "ping-1").await;
    assert!(out.contains("\"duplicates\": 1"), "{out}");
    ping(&env, "ping-2").await;
    let fresh = watch.event("the second ping", |e| e.dedupe_key.as_deref() == Some("ping-2")).await;
    let detail = watch.detail(fresh.id).await;
    assert!(
        matches!(
            outcome(&detail, "cool"),
            Some(DeliveryOutcome::Blocked { reason: BlockReason::Cooldown { .. } })
        ),
        "{:?}",
        detail.deliveries
    );
    assert!(
        matches!(
            outcome(&detail, "rated"),
            Some(DeliveryOutcome::Blocked { reason: BlockReason::RateLimited { .. } })
        ),
        "{:?}",
        detail.deliveries
    );

    // Each seed fired exactly once; four runs in all (the orphan's late write is the subject
    // of the next test).
    created(&mut watch, &format!("out-{}.txt", first.id)).await;
    let runs = watch.all_runs().await;
    assert!(runs.len() >= 4, "{runs:#?}");
    for seed in ["seed-1.txt", "seed-2.txt"] {
        let filter = EventFilter { subject: Some(format!("**/{seed}")), ..Default::default() };
        let e = watch.query(filter).await.pop().unwrap();
        let fired = watch
            .detail(e.id)
            .await
            .deliveries
            .iter()
            .filter(|d| d.outcome == DeliveryOutcome::Fired)
            .count();
        assert_eq!(fired, 1, "{seed}");
    }
    daemon.stop().await;
}

/// `pulse stop` with a run running and one queued: the running one is terminated and recorded
/// as interrupted before the daemon exits; the queued one runs after the next start.
#[tokio::test(flavor = "multi_thread")]
async fn a_graceful_stop_interrupts_running_runs_and_keeps_queued_ones() {
    let env = Env::new();
    env.mkdir("watched/out");
    env.write_config(&config(&env));
    let daemon = env.start().await;
    let mut watch = daemon.watch().await;
    watch.sources(SourceState::Healthy).await;
    std::fs::write(env.user_path("watched/seed-1.txt"), "1").unwrap();
    let first = watch.run_in("the first run", |r| r.rule.as_deref() == Some("on-new-file")).await;
    watch.output_line(first.id, "ready").await;
    std::fs::write(env.user_path("watched/seed-2.txt"), "2").unwrap();
    let second = watch
        .run_in("the second run, queued", |r| r.id != first.id && r.status == RunStatus::Queued)
        .await;
    let pid = watch.get_run(first.id).await.pid.unwrap();
    drop(watch);

    let (code, out, _) = env.run(&["stop"]).await;
    assert_eq!((code, out.trim()), (0, "Pulse stopped."));
    // The agent's process is gone once `pulse stop` returns (it was killed, and reaped by the
    // daemon before it exited). Asked with signal 0, which works where there is no `/proc`.
    let gone =
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid as i32).unwrap());
    assert_eq!(gone, Err(rustix::io::Errno::SRCH), "agent {pid} survived");
    assert!(!env.home.join("daemon.json").exists());
    drop(daemon);

    let daemon = env.start().await;
    let mut watch = daemon.watch().await;
    let run = watch.get_run(first.id).await;
    assert_eq!(run.status, RunStatus::Interrupted);
    assert_eq!(run.error.as_deref(), Some("the daemon shut down"));
    watch.output_line(second.id, "ready").await;
    daemon.stop().await;
}

/// A crash orphans a running agent, which keeps working and writes after the restart. The
/// restarted daemon no longer has the run in memory, but its write must still count as the
/// run's own, or the restart is a way around loop protection.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_orphaned_by_a_crash_cannot_retrigger_its_rule_after_the_restart() {
    let env = Env::new();
    env.mkdir("watched/out");
    env.write_config(&config(&env));
    let daemon = env.start().await;
    let mut watch = daemon.watch().await;
    watch.sources(SourceState::Healthy).await;
    std::fs::write(env.user_path("watched/seed.txt"), "1").unwrap();
    let run = watch.run_in("the run", |r| r.rule.as_deref() == Some("on-new-file")).await;
    watch.output_line(run.id, "ready").await;
    drop(watch);
    daemon.crash().await;

    let daemon = env.start().await;
    let mut watch = daemon.watch().await;
    watch.sources(SourceState::Healthy).await;
    assert_eq!(watch.get_run(run.id).await.status, RunStatus::Interrupted);
    release(&env, run.id).await;
    let out = created(&mut watch, &format!("out-{}.txt", run.id)).await;
    assert_eq!(out.causation_run, Some(run.id), "{out:#?}");
    let detail = watch.detail(out.id).await;
    assert_eq!(
        outcome(&detail, "on-new-file"),
        Some(DeliveryOutcome::Blocked { reason: BlockReason::LoopSelfTrigger { run: run.id } }),
    );
    assert_eq!(watch.all_runs().await.len(), 1);
    daemon.stop().await;
}
