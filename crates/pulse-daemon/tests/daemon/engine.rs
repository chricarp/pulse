//! The vertical slice, atomicity, loop protection, debounce timing, backpressure, replay and
//! dry runs.

use std::sync::Arc;

use pulse_contracts::rpc::*;
use pulse_contracts::{BlockReason, Causation, DeliveryOutcome, OutputKind, RunStatus};
use pulse_daemon::Clock;

use crate::support::*;

fn config(rules: &str) -> String {
    format!("{FAKE_SOURCE}{COMMAND_AGENT}{rules}")
}

const ON_PDF: &str = r#"
[[rule]]
name = "pdf"
on = "fs.file.created"
where = { path = "/u/**/*.pdf" }
run = { agent = "sh" }
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_to_run_to_output_end_to_end() {
    let mut t = TestDaemon::start(&config(ON_PDF)).await;
    let client = t.client().await;
    let mut notes = client.notifications().unwrap();
    client
        .call::<method::EventsSubscribe>(EventsSubscribeParams {
            filter: EventFilter { types: vec!["fs.*".parse().unwrap()], ..Default::default() },
        })
        .await
        .unwrap();
    client.call::<method::RunsSubscribe>(RunsSubscribeParams::default()).await.unwrap();

    t.sources.push("fake", file_created("fake", "/u/a/notes.txt")).await;
    t.sources.push("fake", file_created("fake", "/u/a/lecture.pdf")).await;

    // Both events arrive live; only the PDF starts a run, which succeeds.
    let mut events = Vec::new();
    let mut succeeded = None;
    within("the run to succeed", async {
        while succeeded.is_none() || events.len() < 2 {
            match notes.recv().await.unwrap() {
                Notification::EventsBatch(b) => events.extend(b.batch.events),
                Notification::RunsChanged(c) => {
                    succeeded = c.runs.into_iter().find(|r| r.status == RunStatus::Succeeded);
                }
                _ => {}
            }
        }
    })
    .await;
    let run = succeeded.unwrap();
    assert_eq!(
        events.iter().map(|e| e.subject.as_deref().unwrap()).collect::<Vec<_>>(),
        ["/u/a/notes.txt", "/u/a/lecture.pdf"]
    );
    assert_eq!(run.event, events[1].id);
    assert_eq!(
        (run.rule.as_deref(), run.agent.as_str(), run.exit_code),
        (Some("pdf"), "sh", Some(0))
    );

    // Output was stored, in order, with stream kinds.
    let out = client
        .call::<method::RunsOutputGet>(RunOutputParams { id: run.id, after: None, limit: None })
        .await
        .unwrap();
    let lines: Vec<(OutputKind, &str)> =
        out.chunks.iter().map(|c| (c.output.kind, c.output.text.as_str())).collect();
    assert!(lines.contains(&(OutputKind::Stdout, "got /u/a/lecture.pdf")), "{lines:?}");
    assert!(lines.contains(&(OutputKind::Stderr, "oops")), "{lines:?}");
    assert!(out.done);

    // The decision is recorded: one delivery for the PDF, none for the text file.
    let detail =
        client.call::<method::EventsGet>(EventIdParams { id: events[1].id }).await.unwrap();
    assert_eq!(detail.deliveries.len(), 1);
    assert_eq!(detail.deliveries[0].outcome, DeliveryOutcome::Fired);
    assert_eq!(detail.deliveries[0].run, Some(run.id));
    let other = client.call::<method::EventsGet>(EventIdParams { id: events[0].id }).await.unwrap();
    assert!(other.deliveries.is_empty());

    // A late output subscriber gets everything stored, then `done`.
    let sub = client
        .call::<method::RunsSubscribeOutput>(RunOutputSubscribeParams { id: run.id, after: None })
        .await
        .unwrap();
    let streamed = within("streamed output", async {
        loop {
            if let Notification::RunsOutput(o) = notes.recv().await.unwrap() {
                if o.subscription == sub.subscription {
                    return o;
                }
            }
        }
    })
    .await;
    assert!(streamed.done);
    assert_eq!(streamed.chunks.len(), out.chunks.len());

    // A subscription is answered before its first notification, even when stored output is
    // ready at once.
    let mut raw = raw_socket(t.port(), &t.token()).await;
    let first = raw_call(
        &mut raw,
        &format!(
            r#"{{"jsonrpc":"2.0","id":7,"method":"runs.subscribe_output","params":{{"id":"{}"}}}}"#,
            run.id
        ),
    )
    .await;
    assert_eq!(first["id"], 7, "the response comes first: {first}");

    // The run saw the event file and its own id.
    let full = client.call::<method::RunsGet>(RunIdParams { id: run.id }).await.unwrap();
    let inv = full.invocation.unwrap();
    assert_eq!(inv.program, "sh");
    assert!(inv.cwd.ends_with(&run.id.to_string()), "default cwd is the per-run directory");
    assert!(t.home.join("runs").join(run.id.to_string()).join("event.json").exists());
    t.stop().await;
}

const FAILED_COMMIT_RULES: &str = r#"
[[rule]]
name = "every-ext"
on = "ext.ci.first"
cooldown = "1h"
run = { agent = "sh" }

[[rule]]
name = "settle"
on = "ext.ci.settle"
debounce = "1h"
debounce_by = "subject"
run = { agent = "sh" }
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_commit_leaves_nothing_behind() {
    // One debounce slot: whether it is still taken shows whether a pending debounce survived.
    let limits = "[limits]\ndebounce_capacity = 1\n";
    let mut t = TestDaemon::start(&format!("{limits}{}", config(FAILED_COMMIT_RULES))).await;
    let client = t.client().await;
    let ingest = |kind: &str, subject: &str, key: Option<&str>| IngestParams {
        source: "ci".into(),
        events: vec![IngestEvent {
            type_: kind.into(),
            subject: Some(subject.into()),
            time: None,
            data: serde_json::json!({}),
            dedupe_key: key.map(Into::into),
            causation_run: None,
        }],
    };
    let outcome = |id| {
        let client = &client;
        async move {
            let detail = client.call::<method::EventsGet>(EventIdParams { id }).await.unwrap();
            detail.deliveries[0].outcome.clone()
        }
    };
    // A debounce pending from an earlier, committed event.
    let ok = client.call::<method::EventsIngest>(ingest("settle", "a", None)).await.unwrap();
    assert!(matches!(outcome(ok.accepted[0]).await, DeliveryOutcome::Debounced { .. }));

    // Make every insert into `runs` fail, from a second connection: the commit that would
    // create a run must roll back its event and delivery too.
    let db = rusqlite::Connection::open(t.home.join("pulse.db")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER no_runs BEFORE INSERT ON runs BEGIN SELECT RAISE(ABORT, 'injected'); END;",
    )
    .unwrap();
    let first = ingest("first", "x", Some("k1"));
    let err = client.call::<method::EventsIngest>(first.clone()).await.unwrap_err();
    assert!(err.to_string().contains("injected"), "{err}");
    let page = client
        .call::<method::EventsQuery>(EventQuery {
            filter: EventFilter {
                types: vec!["ext.ci.first".parse().unwrap()],
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(page.page.events.is_empty(), "the event was rolled back with the run");
    let history = client
        .call::<method::RulesHistory>(RuleHistoryParams {
            name: "every-ext".into(),
            outcomes: vec![],
            cursor: None,
            limit: None,
        })
        .await
        .unwrap();
    assert!(history.deliveries.is_empty());

    // The sender's retry is not a duplicate of the event that was never stored, and the fire
    // that was not stored does not hold the rule in its cooldown.
    db.execute_batch("DROP TRIGGER no_runs;").unwrap();
    let ok = client.call::<method::EventsIngest>(first.clone()).await.unwrap();
    assert_eq!((ok.accepted.len(), ok.duplicates), (1, 0), "the retry is accepted");
    assert_eq!(outcome(ok.accepted[0]).await, DeliveryOutcome::Fired);
    let detail =
        client.call::<method::EventsGet>(EventIdParams { id: ok.accepted[0] }).await.unwrap();
    assert_eq!(detail.runs.len(), 1);

    // The debounce pending before the failure is still pending: the one slot is taken, so a
    // new subject is not debounced.
    let ok = client.call::<method::EventsIngest>(ingest("settle", "b", None)).await.unwrap();
    assert_eq!(outcome(ok.accepted[0]).await, DeliveryOutcome::Fired);

    // A config change that resizes the dedupe table keeps the keys already accepted.
    let mut notes = client.notifications().unwrap();
    t.write_config(&format!(
        "[limits]\ndebounce_capacity = 1\ndedupe_ttl = \"20m\"\n{}",
        config(FAILED_COMMIT_RULES)
    ));
    within("the new limits", async {
        loop {
            if let Notification::StateChanged(s) = notes.recv().await.unwrap() {
                if s.topics.contains(&StateTopic::Config) {
                    return;
                }
            }
        }
    })
    .await;
    let again = client.call::<method::EventsIngest>(first).await.unwrap();
    assert_eq!((again.accepted.len(), again.duplicates), (0, 1), "still a duplicate");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_cannot_trigger_its_own_rule() {
    let mut t = TestDaemon::start(&config(
        r#"
[[rule]]
name = "echo"
on = "ext.*"
run = { agent = "sh" }

[[rule]]
name = "other"
on = "ext.loop.again"
run = { agent = "sh" }
"#,
    ))
    .await;
    let mut watch = RunWatch::new(t.client().await).await;
    let client = t.client().await;
    let live = t.client().await;
    let mut notes = live.notifications().unwrap();
    live.call::<method::EventsSubscribe>(EventsSubscribeParams::default()).await.unwrap();
    let event = |type_: &str, cause| IngestParams {
        source: "loop".into(),
        events: vec![IngestEvent {
            type_: type_.into(),
            subject: Some("x".into()),
            time: None,
            data: serde_json::json!({}),
            dedupe_key: None,
            causation_run: cause,
        }],
    };
    client.call::<method::EventsIngest>(event("start", None)).await.unwrap();
    let first = watch.until(None, RunStatus::Succeeded).await;

    // What `pulse ingest` inside that run would send: exact causation through PULSE_RUN_ID.
    let res = client.call::<method::EventsIngest>(event("again", Some(first.id))).await.unwrap();
    let detail =
        client.call::<method::EventsGet>(EventIdParams { id: res.accepted[0] }).await.unwrap();
    assert_eq!(detail.event.causation, Some(Causation::Exact));
    assert_eq!(detail.event.causation_run, Some(first.id));
    assert_eq!(detail.event.chain_depth, 1);
    let outcome = |rule: &str| {
        detail.deliveries.iter().find(|d| d.rule == rule).map(|d| d.outcome.clone()).unwrap()
    };
    assert_eq!(
        outcome("echo"),
        DeliveryOutcome::Blocked { reason: BlockReason::LoopSelfTrigger { run: first.id } },
        "the blocked delivery is recorded"
    );
    // Another rule may react, one level deeper in the chain.
    assert_eq!(outcome("other"), DeliveryOutcome::Fired);
    assert_eq!(detail.runs.len(), 1);
    assert_eq!(detail.runs[0].chain_depth, 2);
    // A live view sees the block at a glance: the batch carries what each event caused.
    let again = res.accepted[0];
    let outcomes = within("the event's outcomes, live", async {
        loop {
            if let Notification::EventsBatch(b) = notes.recv().await.unwrap() {
                if b.batch.events.iter().any(|e| e.id == again) {
                    return b.outcomes.into_iter().filter(|o| o.event == again).collect::<Vec<_>>();
                }
            }
        }
    })
    .await;
    let summary = |list: &[EventOutcome]| {
        let mut v: Vec<(String, DeliveryOutcome, bool)> =
            list.iter().map(|o| (o.rule.clone(), o.outcome.clone(), o.run.is_some())).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    };
    let want = vec![
        (
            "echo".to_owned(),
            DeliveryOutcome::Blocked { reason: BlockReason::LoopSelfTrigger { run: first.id } },
            false,
        ),
        ("other".to_owned(), DeliveryOutcome::Fired, true),
    ];
    assert_eq!(summary(&outcomes), want);
    // So does a page from the log.
    let page = client.call::<method::EventsQuery>(EventQuery::default()).await.unwrap();
    assert_eq!(page.page.events[0].id, again);
    let from_log: Vec<EventOutcome> =
        page.outcomes.into_iter().filter(|o| o.event == again).collect();
    assert_eq!(summary(&from_log), want);

    // And the history of the rule shows it, for "why didn't it fire?".
    let blocked = client
        .call::<method::RulesHistory>(RuleHistoryParams {
            name: "echo".into(),
            outcomes: vec![DeliveryKind::Blocked],
            cursor: None,
            limit: None,
        })
        .await
        .unwrap();
    assert_eq!(blocked.deliveries.len(), 1);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tripped_circuit_breaker_is_announced() {
    // Clients (the desktop tray) learn that a rule was stopped from `state.changed`, not by
    // polling `rules.list`.
    let mut t = TestDaemon::start(&config(
        r#"
[limits]
circuit_breaker = { max_fires = 1, window = "1h", open_for = "1h" }

[[rule]]
name = "hot"
on = "ext.*"
run = { agent = "sh" }
"#,
    ))
    .await;
    let client = t.client().await;
    let mut notes = client.notifications().unwrap();
    let ingest = |subject: &str| IngestParams {
        source: "ci".into(),
        events: vec![IngestEvent {
            type_: "build".into(),
            subject: Some(subject.into()),
            time: None,
            data: serde_json::json!({}),
            dedupe_key: None,
            causation_run: None,
        }],
    };
    client.call::<method::EventsIngest>(ingest("first")).await.unwrap();
    let res = client.call::<method::EventsIngest>(ingest("second")).await.unwrap();
    let detail =
        client.call::<method::EventsGet>(EventIdParams { id: res.accepted[0] }).await.unwrap();
    assert!(matches!(
        detail.deliveries[0].outcome,
        DeliveryOutcome::Blocked { reason: BlockReason::CircuitOpen { .. } }
    ));
    within("state.changed with the rules topic", async {
        loop {
            if let Notification::StateChanged(s) = notes.recv().await.unwrap() {
                if s.topics.contains(&StateTopic::Rules) {
                    break;
                }
            }
        }
    })
    .await;
    let rule = client.call::<method::RulesGet>(NameParams { name: "hot".into() }).await.unwrap();
    assert!(rule.state.circuit_open_until.is_some());

    // Unsnoozing closes it for good, across a restart: the fire that tripped it is rebuilt from
    // the log at startup and is still inside the window, but no longer counts.
    let rule =
        client.call::<method::RulesUnsnooze>(NameParams { name: "hot".into() }).await.unwrap();
    assert_eq!(rule.state.circuit_open_until, None);
    drop((client, notes));
    t.stop().await;
    t.boot(Setup::default()).await;
    let client = t.client().await;
    let res = client.call::<method::EventsIngest>(ingest("third")).await.unwrap();
    let detail =
        client.call::<method::EventsGet>(EventIdParams { id: res.accepted[0] }).await.unwrap();
    assert_eq!(detail.deliveries[0].outcome, DeliveryOutcome::Fired);
    t.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_debounce_fires_once_after_the_quiet_period() {
    let rules = r#"
[[rule]]
name = "settle"
on = "fs.file.created"
debounce = "10m"
run = { agent = "sh" }
"#;
    let mut t = TestDaemon::start_with(
        &config(rules),
        Setup {
            supervisor: Arc::new(InstantSupervisor),
            clock: Clock::tokio(1_700_000_000_000),
            watch_config: false,
        },
    )
    .await;
    let mut watch = RunWatch::new(t.client().await).await;
    let live = t.client().await;
    let mut notes = live.notifications().unwrap();
    live.call::<method::EventsSubscribe>(EventsSubscribeParams::default()).await.unwrap();
    for name in ["a", "b", "c"] {
        t.sources.push("fake", file_created("fake", &format!("/w/{name}"))).await;
    }
    // Virtual time runs ahead to the one armed deadline; the run uses the latest event.
    let run = watch.until(None, RunStatus::Succeeded).await;
    let client = t.client().await;
    let detail = client.call::<method::EventsGet>(EventIdParams { id: run.event }).await.unwrap();
    assert_eq!(detail.event.subject.as_deref(), Some("/w/c"));
    let page = client.call::<method::RunsList>(RunsListParams::default()).await.unwrap();
    assert_eq!(page.runs.len(), 1, "three events, one run");
    let history = client
        .call::<method::RulesHistory>(RuleHistoryParams {
            name: "settle".into(),
            outcomes: vec![DeliveryKind::Debounced],
            cursor: None,
            limit: None,
        })
        .await
        .unwrap();
    assert_eq!(history.deliveries.len(), 3);
    // The fire happened long after the event's batch went out; the live view still learns it.
    let late = within("the late outcome", async {
        loop {
            if let Notification::EventsBatch(b) = notes.recv().await.unwrap() {
                if let Some(o) =
                    b.outcomes.into_iter().find(|o| o.outcome == DeliveryOutcome::Fired)
                {
                    return o;
                }
            }
        }
    })
    .await;
    assert_eq!((late.event, late.run), (run.event, Some(run.id)));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_burst_of_events_reaches_a_slow_client_summarized() {
    let mut t = TestDaemon::start(&config("")).await;
    let subscriber = t.client().await;
    let mut notes = subscriber.notifications().unwrap();
    subscriber.call::<method::EventsSubscribe>(EventsSubscribeParams::default()).await.unwrap();
    let client = t.client().await;
    const TOTAL: u64 = 5000;
    for chunk in 0..(TOTAL / 1000) {
        let events = (0..1000)
            .map(|i| IngestEvent {
                type_: "burst".into(),
                subject: Some(format!("{chunk}-{i}")),
                time: None,
                data: serde_json::json!({ "pad": "x".repeat(200) }),
                dedupe_key: None,
                causation_run: None,
            })
            .collect();
        client
            .call::<method::EventsIngest>(IngestParams { source: "load".into(), events })
            .await
            .unwrap();
    }
    // The daemon answers while the subscriber has not read anything yet.
    let status = client.call::<method::SystemStatusGet>(Empty {}).await.unwrap();
    assert_eq!(status.version, pulse_daemon::VERSION);

    let (mut seen, mut dropped, mut batches) = (0u64, 0u64, 0u32);
    let mut last = String::new();
    within("the burst to be accounted for", async {
        while seen + dropped < TOTAL {
            if let Notification::EventsBatch(b) = notes.recv().await.unwrap() {
                let b = b.batch;
                assert!(b.events.len() <= 200, "batches are bounded");
                batches += 1;
                seen += b.events.len() as u64;
                dropped += b.dropped;
                if let Some(e) = b.events.last() {
                    last = e.subject.clone().unwrap();
                }
            }
        }
    })
    .await;
    assert_eq!(seen + dropped, TOTAL, "every event is delivered or counted");
    assert!(dropped > 0, "the live feed summarized");
    assert!(batches < 50, "{batches} notifications for {TOTAL} events");
    assert_eq!(last, "4-999", "the newest events are the ones kept");
    let all = client
        .call::<method::EventsQuery>(EventQuery { limit: Some(1), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(all.page.events[0].subject.as_deref(), Some("4-999"), "the log has everything");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_and_dry_runs() {
    let rules = r#"
[[rule]]
name = "pdf"
on = "fs.file.created"
where = { path = "/u/**/*.pdf" }
cooldown = "1h"
run = { agent = "sh" }

[[rule]]
name = "any"
on = "fs.*"
supervised = true
run = { agent = "sh" }
"#;
    let mut t = TestDaemon::start(&config(rules)).await;
    let mut watch = RunWatch::new(t.client().await).await;
    let client = t.client().await;
    t.sources.push("fake", file_created("fake", "/u/x.pdf")).await;
    let first = watch.until(None, RunStatus::Succeeded).await;

    // Dry run of the saved rule: in cooldown now, unless runtime state is ignored.
    let test = |ignore| RuleTestParams {
        rule: RuleRef::Saved { name: "pdf".into() },
        event: first.event,
        ignore_runtime_state: ignore,
    };
    let r = client.call::<method::RulesTest>(test(false)).await.unwrap();
    assert!(r.matched);
    assert!(matches!(
        r.decision.unwrap().outcome,
        pulse_contracts::Outcome::Blocked { reason: BlockReason::Cooldown { .. } }
    ));
    let r = client.call::<method::RulesTest>(test(true)).await.unwrap();
    assert!(matches!(r.decision.unwrap().outcome, pulse_contracts::Outcome::Fire { .. }));

    // An unsaved draft with a prompt and a debounce shows the prompt it would send.
    let draft: pulse_contracts::config::RuleConfig = toml::from_str(
        r#"
name = "draft"
on = "fs.*"
debounce = "5s"
run = { agent = "sh", prompt = "Look at {{subject}}" }
"#,
    )
    .unwrap();
    let r = client
        .call::<method::RulesTest>(RuleTestParams {
            rule: RuleRef::Draft { rule: Box::new(draft), project: None },
            event: first.event,
            ignore_runtime_state: false,
        })
        .await
        .unwrap();
    assert!(matches!(r.decision.unwrap().outcome, pulse_contracts::Outcome::Debounced { .. }));
    match r.after_debounce.unwrap().outcome {
        pulse_contracts::Outcome::Fire { prompt: Some(p) } => {
            assert!(p.text.starts_with("Look at /u/x.pdf\n"), "{}", p.text);
            assert!(p.text.contains(&p.delimiter));
        }
        other => panic!("{other:?}"),
    }
    // No side effects: still exactly the deliveries of the original event.
    let detail = client.call::<method::EventsGet>(EventIdParams { id: first.event }).await.unwrap();
    assert_eq!(detail.deliveries.len(), 2);
    assert!(client.call::<method::RulesGet>(NameParams { name: "draft".into() }).await.is_err());

    // Replay to one rule: a new event marked as a replay, routed only there.
    let replay = client
        .call::<method::EventsReplay>(ReplayParams {
            id: first.event,
            rules: Some(vec!["any".into()]),
        })
        .await
        .unwrap();
    assert_eq!(replay.event.replay_of, Some(first.event));
    assert_ne!(replay.event.id, first.event);
    assert_eq!(replay.deliveries.len(), 1);
    assert_eq!(replay.deliveries[0].rule, "any");
    assert_eq!(replay.deliveries[0].outcome, DeliveryOutcome::AwaitingApproval);
    let err = client
        .call::<method::EventsReplay>(ReplayParams {
            id: first.event,
            rules: Some(vec!["nope".into()]),
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("nope"));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filters_expand_the_home_directory() {
    let mut t = TestDaemon::start(&config("")).await;
    let client = t.client().await;
    let status = client.call::<method::SystemStatusGet>(Empty {}).await.unwrap();
    assert_eq!(status.user_home, t.user_home.display().to_string());
    let home = t.user_home.display().to_string();
    t.sources.push("fake", file_created("fake", &format!("{home}/University/a.pdf"))).await;
    t.sources.push("fake", file_created("fake", "/elsewhere/b.pdf")).await;
    let all = within("both events", async {
        loop {
            let p = client.call::<method::EventsQuery>(EventQuery::default()).await.unwrap();
            if p.page.events.len() == 2 {
                return p;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert_eq!(all.page.events.len(), 2);
    for filter in [
        EventFilter { path: Some("~/University".into()), ..Default::default() },
        EventFilter { subject: Some("~/University/*.pdf".into()), ..Default::default() },
    ] {
        let page = client
            .call::<method::EventsQuery>(EventQuery {
                filter: filter.clone(),
                ..Default::default()
            })
            .await
            .unwrap();
        let subjects: Vec<_> =
            page.page.events.iter().map(|e| e.subject.clone().unwrap()).collect();
        assert_eq!(subjects, [format!("{home}/University/a.pdf")], "{filter:?}");
    }
    t.stop().await;
}
