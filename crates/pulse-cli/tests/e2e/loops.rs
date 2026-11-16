//! Loop protection through the whole pipeline: agents that really write into the folder their
//! own rule watches, or report through `pulse run` / `pulse ingest` from inside their run.

use pulse_contracts::rpc::{NameParams, method};
use pulse_contracts::status::SourceState;
use pulse_contracts::{BlockReason, Causation, DeliveryOutcome, Event, RunStatus};

use crate::support::*;

fn blocked(detail: &pulse_contracts::rpc::EventDetail, rule: &str) -> Option<BlockReason> {
    detail.deliveries.iter().find(|d| d.rule == rule).and_then(|d| match &d.outcome {
        DeliveryOutcome::Blocked { reason } => Some(reason.clone()),
        _ => None,
    })
}

/// A watched folder, a `command` agent running `script` (with `$LOOP_DIR`, `$E2E_PULSE_BIN` and
/// `$FIFO` in its environment), and one rule on files created there.
struct LoopSetup<'a> {
    agent_cwd: Option<&'a str>,
    script: &'a str,
    limits: &'a str,
    rule: &'a str,
}

fn fs_loop_config(env: &Env, s: LoopSetup<'_>) -> String {
    let dir = env.user_path("watched");
    let cwd = s.agent_cwd.map(|c| format!("cwd = {c:?}\n")).unwrap_or_default();
    format!(
        r#"
[limits]
causation_grace = "30s"
{limits}

[[source]]
name = "watched"
kind = "fs"
path = "~/watched"
quiet = "100ms"

[[agent]]
name = "writer"
adapter = "command"
command = ["sh", "-c", {script:?}]
allow = ["fs.*:~/watched/**"]
{cwd}max_concurrent = 4
env = {{ LOOP_DIR = {dir:?}, E2E_PULSE_BIN = {bin:?}, FIFO = {fifo:?} }}

[[rule]]
name = "on-new-file"
on = "fs.file.created"
where = {{ path = "~/watched/**" }}
{rule}
run = {{ agent = "writer" }}
"#,
        script = s.script,
        limits = s.limits,
        rule = s.rule,
        dir = dir.display().to_string(),
        bin = pulse_bin(),
        fifo = env.dir.path().join("fifo").display().to_string(),
    )
}

async fn created(watch: &mut Watch, name: &str) -> Event {
    let name = name.to_owned();
    watch
        .event(&format!("fs.file.created for {name}"), |e| {
            e.type_.as_str() == "fs.file.created"
                && e.subject.as_deref().is_some_and(|s| s.ends_with(&format!("/{name}")))
        })
        .await
}

/// Inferred causation: the run's working directory is the watched folder and the agent writes
/// a file there. Each case is a way the write could slip past: written at once, through a
/// debounced rule, and by a process that left the run's group and writes after the run ended
/// (inside `causation_grace`).
#[tokio::test(flavor = "multi_thread")]
async fn a_run_writing_into_its_own_watched_folder_is_blocked_as_a_self_trigger() {
    let at_once = "echo again > \"$LOOP_DIR/out-$PULSE_RUN_ID.txt\"";
    // The writer leaves the run's process group, then waits for the test, so it provably
    // writes after the run ended. The run waits until it has left (a fifo handshake): otherwise
    // the group's SIGTERM at the run's end could reach it before `setsid` does.
    let escaped = "r=\"$FIFO.left\"; mkfifo \"$r\"; \
                   setsid sh -c 'echo > \"$1\"; read go < \"$2\"; echo late > \"$3\"' sh \
                   \"$r\" \"$FIFO\" \"$LOOP_DIR/out-$PULSE_RUN_ID.txt\" </dev/null >/dev/null 2>&1 & \
                   read _ < \"$r\"; rm -f \"$r\"";
    let cases = [
        ("at once", at_once, ""),
        ("debounced", at_once, "debounce = \"300ms\""),
        ("after the run", escaped, ""),
    ];
    for (case, script, rule) in cases {
        let env = Env::new();
        env.mkdir("watched");
        let mut fifo = Fifo::new(env.dir.path(), "fifo");
        env.write_config(&fs_loop_config(
            &env,
            LoopSetup { agent_cwd: Some("~/watched"), script, limits: "", rule },
        ));
        let daemon = env.start().await;
        let mut watch = daemon.watch().await;
        watch.sources(SourceState::Healthy).await;

        std::fs::write(env.user_path("watched/seed.txt"), "seed").unwrap();
        let run = watch.run_in("the first run", |_| true).await;
        let ended = watch.run_terminal(run.id).await;
        assert_eq!(ended.status, RunStatus::Succeeded, "{case}");
        if script == escaped {
            fifo.open().await;
            fifo.write("go\n");
            fifo.close();
        }

        let out = created(&mut watch, &format!("out-{}.txt", run.id)).await;
        let full = watch.get_run(run.id).await;
        assert_eq!(
            out.causation_run,
            Some(run.id),
            "{case}: event time {} run started {:?} ended {:?}",
            out.time,
            full.started_at,
            full.ended_at
        );
        assert_eq!(out.causation, Some(Causation::Inferred), "{case}");
        assert_eq!(out.chain_depth, 1, "{case}");
        let detail = watch.detail(out.id).await;
        assert_eq!(
            blocked(&detail, "on-new-file"),
            Some(BlockReason::LoopSelfTrigger { run: run.id }),
            "{case}: {:?}",
            detail.deliveries
        );
        assert_eq!(watch.all_runs().await.len(), 1, "{case}");

        if case == "at once" {
            // Visible from the CLI, so "why didn't it fire?" has an answer.
            let (code, text, _) = env.run(&["rules", "history", "on-new-file"]).await;
            assert_eq!(code, 0);
            assert!(text.contains("loop protection"), "{text}");
            // A replay is the user's explicit request: it fires once, without the old
            // causation, and the new run's own write is blocked in turn. One run, not a loop.
            let (code, text, _) = env.run(&["replay", &out.id.to_string()]).await;
            assert_eq!(code, 0, "{text}");
            let again = watch.run_in("the replay's run", |r| r.id != run.id).await;
            watch.run_terminal(again.id).await;
            let out2 = created(&mut watch, &format!("out-{}.txt", again.id)).await;
            let detail = watch.detail(out2.id).await;
            assert_eq!(
                blocked(&detail, "on-new-file"),
                Some(BlockReason::LoopSelfTrigger { run: again.id })
            );
            assert_eq!(watch.all_runs().await.len(), 2);
        }
        daemon.stop().await;
    }
}

/// Exact causation: the agent wraps a failing command with `pulse run` (and reports through
/// `pulse ingest`) from inside its run. `PULSE_RUN_ID` attributes both to the run.
#[tokio::test(flavor = "multi_thread")]
async fn reports_from_inside_a_run_carry_exact_causation_and_cannot_retrigger_their_rule() {
    let env = Env::new();
    let work = env.mkdir("work");
    let script = r#"
        "$E2E_PULSE_BIN" run -- sh -c 'exit 7'
        echo '{"type":"done","subject":"x"}' | "$E2E_PULSE_BIN" ingest -s agent
        true
    "#;
    env.write_config(&format!(
        r#"
[[agent]]
name = "fixer"
adapter = "command"
command = ["sh", "-c", {script:?}]
allow = ["process.*", "ext.*"]
env = {{ E2E_PULSE_BIN = {bin:?} }}

[[rule]]
name = "on-failure"
on = "process.exited"
where = {{ success = false }}
run = {{ agent = "fixer" }}

[[rule]]
name = "on-done"
on = "ext.agent.done"
run = {{ agent = "fixer" }}
"#,
        bin = pulse_bin()
    ));
    let daemon = env.start().await;
    let mut watch = daemon.watch().await;

    let (code, _, _) = env.run_in(&["run", "--", "false"], &[], Some(&work)).await;
    assert_eq!(code, 1);
    let first = watch.run_in("the first run", |r| r.rule.as_deref() == Some("on-failure")).await;
    watch.run_terminal(first.id).await;

    let exited = watch
        .event("the run's own process.exited", |e| {
            e.type_.as_str() == "process.exited" && e.causation_run.is_some()
        })
        .await;
    assert_eq!(exited.causation_run, Some(first.id));
    assert_eq!(exited.causation, Some(Causation::Exact));
    assert_eq!(exited.data["exit_code"], 7);
    let detail = watch.detail(exited.id).await;
    assert_eq!(
        blocked(&detail, "on-failure"),
        Some(BlockReason::LoopSelfTrigger { run: first.id })
    );

    // `ext.agent.done` from the first run fires the *other* rule (not a self-trigger), whose run
    // reports again; those reports are blocked for on-done and fire on-failure at depth 3, …
    let done = watch.event("ext.agent.done", |e| e.type_.as_str() == "ext.agent.done").await;
    assert_eq!((done.causation_run, done.causation), (Some(first.id), Some(Causation::Exact)));
    assert_eq!(done.chain_depth, 1);
    // … until the chain reaches max_chain_depth (3): the fourth run is refused.
    let deep = watch
        .event("ext.agent.done at chain depth 3", |e| {
            e.type_.as_str() == "ext.agent.done" && e.chain_depth == 3
        })
        .await;
    let detail = watch.detail(deep.id).await;
    let reasons: Vec<_> = detail.deliveries.iter().map(|d| (&d.rule, &d.outcome)).collect();
    assert!(
        detail.deliveries.iter().any(|d| matches!(
            &d.outcome,
            DeliveryOutcome::Blocked { reason: BlockReason::LoopChainDepth { depth: 4, max: 3 } }
        )),
        "{reasons:#?}"
    );
    // Wait for every run to end, then count: one per depth.
    watch
        .until("all runs to end", |w| {
            (w.runs.len() == 3 && w.runs.values().all(|r| r.status.is_terminal())).then_some(())
        })
        .await;
    let runs = watch.all_runs().await;
    let mut depths: Vec<u32> = runs.iter().map(|r| r.chain_depth).collect();
    depths.sort();
    assert_eq!(depths, [1, 2, 3], "{runs:#?}");
    daemon.stop().await;
}

/// The backstop when attribution is wrong: the agent's working directory is elsewhere (its
/// per-run directory), and it writes into the watched folder by absolute path, so nothing ties
/// the new file to the run. The rule loops until the circuit breaker opens, and the trip is
/// recorded on the rule and in the delivery.
#[tokio::test(flavor = "multi_thread")]
async fn an_unattributable_loop_is_stopped_by_the_circuit_breaker() {
    let env = Env::new();
    env.mkdir("watched");
    env.write_config(&fs_loop_config(
        &env,
        LoopSetup {
            agent_cwd: None,
            script: "echo again > \"$LOOP_DIR/gen-$PULSE_RUN_ID.txt\"",
            limits: "circuit_breaker = { max_fires = 3, window = \"10m\", open_for = \"1h\" }",
            rule: "",
        },
    ));
    let daemon = env.start().await;
    let mut watch = daemon.watch().await;
    watch.sources(SourceState::Healthy).await;

    std::fs::write(env.user_path("watched/seed.txt"), "seed").unwrap();
    // Each fire's file is a new event; the fourth fire is refused.
    let (_, d) = watch
        .delivery("a delivery blocked by the breaker", |_, d| {
            matches!(
                d.outcome,
                DeliveryOutcome::Blocked { reason: BlockReason::CircuitOpen { .. } }
            )
        })
        .await;
    let DeliveryOutcome::Blocked { reason: BlockReason::CircuitOpen { until } } = d.outcome else {
        unreachable!()
    };
    let runs = watch.all_runs().await;
    assert_eq!(runs.len(), 3, "{runs:#?}");
    for r in &runs {
        assert_eq!(r.chain_depth, 1, "unattributed: every run looks like a fresh start");
    }
    let rule = watch
        .client
        .call::<method::RulesGet>(NameParams { name: "on-new-file".into() })
        .await
        .unwrap();
    assert_eq!(rule.state.circuit_open_until, Some(until));
    let (_, out, _) = env.run(&["rules", "show", "on-new-file"]).await;
    assert!(out.contains("breaker open until"), "{out}");

    // The way out: unsnooze closes the breaker.
    let (code, _, _) = env.run(&["rules", "unsnooze", "on-new-file"]).await;
    assert_eq!(code, 0);
    let rule = watch
        .client
        .call::<method::RulesGet>(NameParams { name: "on-new-file".into() })
        .await
        .unwrap();
    assert_eq!(rule.state.circuit_open_until, None);
    daemon.stop().await;
}
