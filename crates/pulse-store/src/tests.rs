use pulse_contracts::config::{AdapterSettings, AgentConfig, McpSettings, Retention};
use pulse_contracts::rpc::{DeliveryKind, EventFilter, EventQuery, Order, RunFilter};
use pulse_contracts::run::{InvocationSummary, OutputKind, RunSnapshot};
use pulse_contracts::{
    BlockReason, Causation, Delivery, DeliveryId, DeliveryOutcome, Duration, Event, EventId,
    EventType, RunId, RunOutput, RunStatus, Timestamp, Ulid,
};
use serde_json::{Value, json};

use crate::schema::{MIGRATIONS, migrate_to};
use crate::*;

fn t(ms: i64) -> Timestamp {
    Timestamp::from_millis(ms)
}

fn eid(n: u64) -> EventId {
    EventId::from_ulid(Ulid::from_parts(n, 0))
}

fn rid(n: u64) -> RunId {
    RunId::from_ulid(Ulid::from_parts(n, 1))
}

fn event(n: u64, type_: &str, source: &str, subject: Option<&str>, data: Value) -> Event {
    Event {
        specversion: Default::default(),
        id: eid(n),
        type_: EventType::new(type_).unwrap(),
        source: source.into(),
        subject: subject.map(str::to_owned),
        time: t(n as i64 * 1000),
        datacontenttype: Some("application/json".into()),
        data,
        dedupe_key: None,
        coalesced: None,
        causation_run: None,
        causation: None,
        chain_depth: 0,
        replay_of: None,
    }
}

fn fs(n: u64, path: &str) -> Event {
    event(n, "fs.file.created", "pulse://fs/university", Some(path), json!({ "path": path }))
}

fn agent() -> AgentConfig {
    AgentConfig {
        name: "study".into(),
        adapter: AdapterSettings::Mcp(McpSettings {}),
        allow: vec![],
        cwd: None,
        timeout: Duration::from_mins(1),
        max_concurrent: 1,
        max_queued: 1,
        env: Default::default(),
    }
}

fn new_run(n: u64, event: EventId, rule: &str, status: RunStatus) -> NewRun {
    NewRun {
        id: rid(n),
        rule: Some(rule.into()),
        agent: "study".into(),
        event,
        status,
        created_at: t(n as i64),
        prompt: Some("p".into()),
        chain_depth: 1,
        rerun_of: None,
        snapshot: RunSnapshot { rule: None, agent: agent() },
    }
}

fn delivery(
    n: u64,
    event: EventId,
    rule: &str,
    at: i64,
    outcome: DeliveryOutcome,
    run: Option<RunId>,
) -> Delivery {
    Delivery {
        id: DeliveryId::from_ulid(Ulid::from_parts(n, 2)),
        event,
        rule: rule.into(),
        agent: "study".into(),
        at: t(at),
        outcome,
        run,
    }
}

#[test]
fn migrations_apply_fresh_and_from_every_earlier_version() {
    let dir = tempfile::tempdir().unwrap();
    for start in 0..=MIGRATIONS.len() {
        let path = dir.path().join(format!("v{start}.db"));
        {
            let mut conn = rusqlite::Connection::open(&path).unwrap();
            assert_eq!(migrate_to(&mut conn, start).unwrap() as usize, start);
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.schema_version().unwrap() as usize,
            MIGRATIONS.len(),
            "upgrade from {start}"
        );
        drop(store);
        // Reopening is a no-op.
        assert_eq!(
            Store::open(&path).unwrap().schema_version().unwrap() as usize,
            MIGRATIONS.len()
        );
    }
    let path = dir.path().join("future.db");
    rusqlite::Connection::open(&path).unwrap().pragma_update(None, "user_version", 999).unwrap();
    assert!(matches!(Store::open(&path), Err(StoreError::TooNew { found: 999, .. })));
}

#[test]
fn event_queries() {
    let mut s = Store::open_in_memory().unwrap();
    let mut caused = fs(5, "/home/me/University/b.pdf");
    caused.causation_run = Some(rid(1));
    caused.causation = Some(Causation::Inferred);
    let events = vec![
        fs(1, "/home/me/University/a.pdf"),
        fs(2, "/home/me/University/Arch/l7.pdf"),
        fs(3, "/home/me/Universityx/c.pdf"),
        event(
            4,
            "git.commit.created",
            "pulse://git/pulse",
            Some("/home/me/code/pulse"),
            json!({"repo": "/home/me/code/pulse", "message": "Fix 100% of the bugs"}),
        ),
        caused,
        event(6, "ext.ci.build.failed", "pulse://ext/ci", Some("main"), json!({})),
        event(
            7,
            "fs.burst",
            "pulse://fs/university",
            Some("/home/me/University/data"),
            json!({"sample": ["/home/me/University/data/1"]}),
        ),
    ];
    s.append_events(&events).unwrap();
    assert_eq!(s.query().event(eid(2)).unwrap().as_ref(), Some(&events[1]));
    assert_eq!(s.query().event(eid(99)).unwrap(), None);

    let ids = |f: EventFilter| -> Vec<u64> {
        s.query()
            .query_events(&EventQuery { filter: f, ..Default::default() })
            .unwrap()
            .events
            .iter()
            .map(|e| e.id.timestamp_ms())
            .collect()
    };
    let types = |ts: &[&str]| EventFilter {
        types: ts.iter().map(|t| t.parse().unwrap()).collect(),
        ..Default::default()
    };
    // (filter, expected ids newest first)
    let cases: Vec<(EventFilter, Vec<u64>)> = vec![
        (EventFilter::default(), vec![7, 6, 5, 4, 3, 2, 1]),
        (types(&["fs.*"]), vec![7, 5, 3, 2, 1]),
        (types(&["fs.file.*"]), vec![5, 3, 2, 1]),
        (types(&["{git,ext}.*"]), vec![6, 4]),
        (types(&["git.commit.created", "fs.burst"]), vec![7, 4]),
        (
            EventFilter { sources: vec!["university".into()], ..Default::default() },
            vec![7, 5, 3, 2, 1],
        ),
        (
            EventFilter {
                sources: vec!["pulse://ext/ci".into(), "pulse".into()],
                ..Default::default()
            },
            vec![6, 4],
        ),
        (
            EventFilter { subject: Some("/home/me/University/*".into()), ..Default::default() },
            vec![7, 5, 2, 1],
        ),
        (EventFilter { subject: Some("ma?n".into()), ..Default::default() }, vec![6]),
        (
            EventFilter { path: Some("/home/me/University".into()), ..Default::default() },
            vec![7, 5, 2, 1],
        ),
        (
            EventFilter { path: Some("/home/me/University/".into()), ..Default::default() },
            vec![7, 5, 2, 1],
        ),
        (
            EventFilter { path: Some("/home/me/University/a.pdf".into()), ..Default::default() },
            vec![1],
        ),
        (EventFilter { path: Some("/".into()), ..Default::default() }, vec![7, 5, 4, 3, 2, 1]),
        (EventFilter { text: Some("100%".into()), ..Default::default() }, vec![4]),
        (EventFilter { text: Some("ARCH".into()), ..Default::default() }, vec![2]),
        (
            EventFilter { since: Some(t(3000)), until: Some(t(5000)), ..Default::default() },
            vec![5, 4, 3],
        ),
        (EventFilter { caused_by: Some(rid(1)), ..Default::default() }, vec![5]),
    ];
    for (filter, want) in cases {
        assert_eq!(ids(filter.clone()), want, "{filter:?}");
    }

    // Cursor pagination in both directions.
    let page = |cursor, order| {
        s.query()
            .query_events(&EventQuery { filter: Default::default(), cursor, limit: Some(3), order })
            .unwrap()
    };
    let p1 = page(None, Order::NewestFirst);
    assert_eq!(p1.events.iter().map(|e| e.id).collect::<Vec<_>>(), vec![eid(7), eid(6), eid(5)]);
    let p2 = page(p1.next_cursor, Order::NewestFirst);
    let p3 = page(p2.next_cursor, Order::NewestFirst);
    assert_eq!(p3.events.iter().map(|e| e.id).collect::<Vec<_>>(), vec![eid(1)]);
    assert_eq!(p3.next_cursor, None);
    let o1 = page(None, Order::OldestFirst);
    assert_eq!(o1.events[0].id, eid(1));
    assert_eq!(page(o1.next_cursor, Order::OldestFirst).events[0].id, eid(4));
}

#[test]
fn commit_is_atomic() {
    let mut s = Store::open_in_memory().unwrap();
    let e = fs(1, "/a");
    let batch = Commit {
        now: t(10),
        events: vec![e.clone()],
        deliveries: vec![delivery(1, e.id, "r", 10, DeliveryOutcome::Fired, Some(rid(1)))],
        runs: vec![new_run(1, e.id, "r", RunStatus::Queued)],
        outbox: vec![Effect::StartRun { run: rid(1) }],
        rule_states: vec![RuleStateRow {
            rule: "r".into(),
            last_fired_at: Some(t(10)),
            ..Default::default()
        }],
    };
    s.commit(&batch).unwrap();
    // The same batch again fails (its first event is not after the stored one), and leaves
    // nothing behind.
    let mut again = batch.clone();
    again.events.push(fs(2, "/b"));
    again.events.swap(0, 1);
    assert!(s.commit(&again).is_err());
    let q = s.query();
    assert_eq!(q.event(eid(2)).unwrap(), None);
    assert_eq!(q.deliveries_for_event(e.id).unwrap().len(), 1);
    assert_eq!(q.list_runs(&RunFilter::default(), None, None).unwrap().runs.len(), 1);
    assert_eq!(q.rule_states().unwrap()[0].last_fired_at, Some(t(10)));
    assert_eq!(s.claim_outbox(t(10), 10).unwrap().len(), 1);

    // Events are appended in id order, within a batch and after what is stored: a cursor
    // reader that has passed an id never looks below it again.
    s.append_events(&[fs(3, "/c"), fs(5, "/e")]).unwrap();
    assert_eq!(s.query().last_event_id().unwrap(), Some(eid(5)));
    for bad in [vec![fs(4, "/d")], vec![fs(7, "/g"), fs(6, "/f")], vec![fs(5, "/e")]] {
        let err = s.append_events(&bad).unwrap_err();
        assert!(matches!(err, StoreError::OutOfOrder { .. }), "{bad:?}: {err}");
    }
    assert_eq!(s.query().last_event_id().unwrap(), Some(eid(5)), "refused batches left nothing");
}

#[test]
fn run_transitions() {
    use RunStatus::*;
    let inv = InvocationSummary { program: "claude".into(), args: vec![], cwd: "/w".into() };
    let start = RunTransition::Start { pid: Some(42), invocation: inv };
    let finish =
        |status| RunTransition::Finish { status, exit_code: Some(0), signal: None, error: None };
    // (initial status, transition, Ok(new status) or Err)
    let cases: Vec<(RunStatus, RunTransition, Result<RunStatus, ()>)> = vec![
        (PendingApproval, RunTransition::Approve, Ok(Queued)),
        (PendingApproval, RunTransition::Reject { reason: Some("no".into()) }, Ok(Rejected)),
        (PendingApproval, RunTransition::Cancel, Ok(Cancelled)),
        (PendingApproval, start.clone(), Err(())),
        (Queued, RunTransition::Approve, Err(())),
        (Queued, start.clone(), Ok(Running)),
        (Queued, RunTransition::Cancel, Ok(Cancelled)),
        (Queued, RunTransition::FailToStart { error: "ENOENT".into() }, Ok(Failed)),
        (Queued, finish(Succeeded), Err(())),
        // A graceful shutdown terminates running runs and records them as interrupted.
        (Running, finish(Interrupted), Ok(Interrupted)),
        (Queued, finish(Interrupted), Err(())),
    ];
    for (i, (initial, transition, want)) in cases.into_iter().enumerate() {
        let mut s = Store::open_in_memory().unwrap();
        let e = fs(1, "/a");
        s.commit(&Commit {
            events: vec![e.clone()],
            runs: vec![new_run(i as u64, e.id, "r", initial)],
            ..Default::default()
        })
        .unwrap();
        let got = s
            .transition_run(rid(i as u64), transition.clone(), t(100))
            .map(|r| r.status)
            .map_err(|_| ());
        assert_eq!(got, want, "{initial:?} + {transition:?}");
    }
    // A full lifecycle, with the outbox row from approval and the recorded fields.
    let mut s = Store::open_in_memory().unwrap();
    let e = fs(1, "/a");
    s.commit(&Commit {
        events: vec![e.clone()],
        runs: vec![new_run(1, e.id, "r", PendingApproval)],
        ..Default::default()
    })
    .unwrap();
    assert!(s.claim_outbox(t(0), 10).unwrap().is_empty());
    s.transition_run(rid(1), RunTransition::Approve, t(5)).unwrap();
    assert_eq!(s.claim_outbox(t(5), 10).unwrap()[0].effect, Effect::StartRun { run: rid(1) });
    let run = s.transition_run(rid(1), start, t(6)).unwrap();
    assert_eq!((run.pid, run.started_at), (Some(42), Some(t(6))));
    assert!(matches!(
        s.transition_run(rid(1), RunTransition::Cancel, t(7)),
        Err(StoreError::IllegalTransition { .. })
    ));
    let run = s.transition_run(rid(1), finish(Succeeded), t(8)).unwrap();
    assert_eq!((run.status, run.exit_code, run.ended_at), (Succeeded, Some(0), Some(t(8))));
    assert!(s.transition_run(rid(1), finish(Failed), t(9)).is_err());
    assert!(matches!(
        s.transition_run(rid(77), RunTransition::Cancel, t(9)),
        Err(StoreError::NotFound { .. })
    ));
}

#[test]
fn restart_recovery_and_listing() {
    use RunStatus::*;
    let mut s = Store::open_in_memory().unwrap();
    let e = fs(1, "/a");
    let mut runs = vec![
        new_run(1, e.id, "a", Queued),
        new_run(2, e.id, "a", Queued),
        new_run(3, e.id, "b", PendingApproval),
        new_run(4, e.id, "b", Queued),
    ];
    runs[3].agent = "other".into();
    s.commit(&Commit {
        events: vec![e.clone()],
        runs,
        outbox: vec![Effect::StartRun { run: rid(1) }],
        ..Default::default()
    })
    .unwrap();
    let inv = InvocationSummary { program: "x".into(), args: vec![], cwd: "/".into() };
    s.transition_run(rid(1), RunTransition::Start { pid: None, invocation: inv }, t(1)).unwrap();
    assert_eq!(s.claim_outbox(t(1), 10).unwrap().len(), 1);
    let counts = s.query().run_counts().unwrap();
    assert_eq!((counts.running, counts.queued, counts.pending_approval), (1, 2, 1));
    assert_eq!(s.query().agent_backlog().unwrap().get("study"), Some(&2));

    assert_eq!(s.recover_after_restart(t(50)).unwrap(), vec![rid(1)]);
    let r1 = s.query().run(rid(1)).unwrap().unwrap();
    assert_eq!((r1.status, r1.ended_at), (Interrupted, Some(t(50))));
    assert_eq!(s.claim_outbox(t(50), 10).unwrap().len(), 1, "claimed rows are released");

    let q = s.query();
    let list = |f: RunFilter| {
        q.list_runs(&f, None, None).unwrap().runs.iter().map(|r| r.id).collect::<Vec<_>>()
    };
    assert_eq!(list(RunFilter::default()), vec![rid(4), rid(3), rid(2), rid(1)]);
    assert_eq!(
        list(RunFilter { rule: Some("a".into()), ..Default::default() }),
        vec![rid(2), rid(1)]
    );
    assert_eq!(list(RunFilter { agent: Some("other".into()), ..Default::default() }), vec![rid(4)]);
    assert_eq!(
        list(RunFilter { status: vec![PendingApproval, Interrupted], ..Default::default() }),
        vec![rid(3), rid(1)]
    );
    let page = q.list_runs(&RunFilter::default(), None, Some(3)).unwrap();
    assert_eq!(page.next_cursor, Some(rid(2)));
    assert_eq!(
        q.list_runs(&RunFilter::default(), page.next_cursor, Some(3)).unwrap().runs.len(),
        1
    );
    assert_eq!(q.runs_for_event(e.id).unwrap().len(), 4);
}

#[test]
fn output_keeps_head_and_tail() {
    let mut s = Store::open_in_memory().unwrap();
    let e = fs(1, "/a");
    s.commit(&Commit {
        events: vec![e.clone()],
        runs: vec![new_run(1, e.id, "r", RunStatus::Queued)],
        ..Default::default()
    })
    .unwrap();
    let line = |i: usize| (t(i as i64), RunOutput::new(OutputKind::Stdout, format!("line {i:02}")));
    // Each chunk is 7 bytes; cap 50 keeps a head of 7 chunks (<= 25 bytes is 3) and a tail.
    let chunks: Vec<_> = (0..20).map(line).collect();
    let stats = s.append_output(rid(1), &chunks[..10], 50).unwrap();
    assert_eq!(stats.bytes, 70);
    s.append_output(rid(1), &chunks[10..], 50).unwrap();
    let page = s.query().run_output(rid(1), None, None).unwrap();
    let texts: Vec<&str> = page.chunks.iter().map(|c| c.output.text.as_str()).collect();
    assert_eq!(&texts[..3], ["line 00", "line 01", "line 02"]);
    assert_eq!(page.chunks[3].output.kind, OutputKind::Marker);
    assert!(texts[3].contains("bytes of output dropped"));
    assert_eq!(texts.last(), Some(&"line 19"));
    let kept: usize = page
        .chunks
        .iter()
        .filter(|c| c.output.kind != OutputKind::Marker)
        .map(|c| c.output.text.len())
        .sum();
    assert!(kept <= 50, "kept {kept}");
    assert_eq!(page.stats.bytes, 140);
    assert_eq!(page.stats.dropped_bytes as usize, 140 - kept);
    assert_eq!(
        page.chunks[3].output.data.as_ref().unwrap()["dropped_bytes"],
        page.stats.dropped_bytes
    );
    assert!(!page.done);
    // Sequence numbers increase; reading after the last one gives nothing new.
    let seqs: Vec<u64> = page.chunks.iter().map(|c| c.seq).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]));
    let next = s.query().run_output(rid(1), page.next_after, Some(5)).unwrap();
    assert!(next.chunks.is_empty() && next.next_after == page.next_after);
    // Pagination.
    let first = s.query().run_output(rid(1), None, Some(2)).unwrap();
    assert_eq!(first.chunks.len(), 2);
    assert_eq!(
        s.query().run_output(rid(1), first.next_after, Some(1)).unwrap().chunks[0].output.text,
        "line 02"
    );

    // A single huge chunk is cut to half the cap.
    s.commit(&Commit {
        runs: vec![new_run(2, e.id, "r", RunStatus::Queued)],
        ..Default::default()
    })
    .unwrap();
    s.append_output(
        rid(2),
        &[(t(0), RunOutput::new(OutputKind::Assistant, "é".repeat(1000)))],
        100,
    )
    .unwrap();
    let page = s.query().run_output(rid(2), None, None).unwrap();
    assert!(page.chunks[0].output.text.len() <= 50 + '…'.len_utf8());
    assert_eq!(page.stats.bytes, 2000);
}

#[test]
fn outbox_retries_and_parks() {
    let mut s = Store::open_in_memory().unwrap();
    let e = fs(1, "/a");
    s.commit(&Commit {
        now: t(0),
        events: vec![e.clone()],
        runs: vec![
            new_run(1, e.id, "r", RunStatus::Queued),
            new_run(2, e.id, "r", RunStatus::Queued),
        ],
        outbox: vec![Effect::StartRun { run: rid(1) }, Effect::StartRun { run: rid(2) }],
        ..Default::default()
    })
    .unwrap();
    assert_eq!(s.query().next_outbox_at().unwrap(), Some(t(0)));
    let items = s.claim_outbox(t(0), 1).unwrap();
    assert_eq!((items.len(), items[0].attempts), (1, 1));
    assert_eq!(s.claim_outbox(t(0), 10).unwrap().len(), 1, "the first stays claimed");
    s.fail_outbox(items[0].id, "spawn failed", Some(t(1000))).unwrap();
    assert!(s.claim_outbox(t(999), 10).unwrap().is_empty());
    assert_eq!(s.query().next_outbox_at().unwrap(), Some(t(1000)));
    let retry = s.claim_outbox(t(1000), 10).unwrap();
    assert_eq!(retry[0].attempts, 2);
    s.fail_outbox(retry[0].id, "again", None).unwrap();
    assert_eq!(s.query().next_outbox_at().unwrap(), None);
    assert!(s.complete_outbox(retry[0].id).is_err(), "parked rows are not claimed");
}

#[test]
fn deliveries_history_and_fires() {
    let mut s = Store::open_in_memory().unwrap();
    let e = fs(1, "/a");
    let blocked = DeliveryOutcome::Blocked { reason: BlockReason::Cooldown { until: t(99) } };
    s.commit(&Commit {
        events: vec![e.clone()],
        deliveries: vec![
            delivery(1, e.id, "r", 10, DeliveryOutcome::Fired, None),
            delivery(2, e.id, "r", 20, blocked.clone(), None),
            delivery(3, e.id, "r", 30, DeliveryOutcome::AwaitingApproval, None),
            delivery(4, e.id, "other", 40, DeliveryOutcome::Fired, None),
        ],
        ..Default::default()
    })
    .unwrap();
    let q = s.query();
    assert_eq!(q.recent_fires("r", t(0), 10).unwrap(), vec![t(30), t(10)]);
    assert_eq!(q.recent_fires("r", t(10), 10).unwrap(), vec![t(30)]);
    let h = q.rule_history("r", &[], None, Some(2)).unwrap();
    assert_eq!(h.deliveries.iter().map(|d| d.at).collect::<Vec<_>>(), vec![t(30), t(20)]);
    assert_eq!(h.deliveries[1].outcome, blocked);
    assert_eq!(q.rule_history("r", &[], h.next_cursor, Some(2)).unwrap().deliveries.len(), 1);
    assert_eq!(
        q.rule_history("r", &[DeliveryKind::Blocked], None, None).unwrap().deliveries.len(),
        1
    );
    assert_eq!(q.deliveries_for_event(e.id).unwrap().len(), 4);
}

#[test]
fn rule_state_and_flags() {
    let mut s = Store::open_in_memory().unwrap();
    let row = RuleStateRow {
        rule: "r".into(),
        snoozed_until: Some(t(5)),
        circuit_open_until: None,
        last_fired_at: None,
    };
    s.set_rule_state(&row).unwrap();
    s.set_rule_state(&RuleStateRow { circuit_open_until: Some(t(9)), ..row.clone() }).unwrap();
    assert_eq!(s.query().rule_states().unwrap()[0].circuit_open_until, Some(t(9)));
    s.delete_rule_state("r").unwrap();
    assert!(s.query().rule_states().unwrap().is_empty());
    assert_eq!(s.query().flag(flags::PAUSED).unwrap(), None);
    s.set_flag(flags::PAUSED, "1").unwrap();
    assert_eq!(s.query().flag(flags::PAUSED).unwrap().as_deref(), Some("1"));
    s.delete_flag(flags::PAUSED).unwrap();
    assert_eq!(s.query().flag(flags::PAUSED).unwrap(), None);
}

#[test]
fn retention_keeps_events_of_retained_runs() {
    let mut s = Store::open_in_memory().unwrap();
    let events: Vec<Event> = (1..=10).map(|n| fs(n, &format!("/a/{n}"))).collect();
    let mut runs = vec![
        new_run(1, eid(2), "r", RunStatus::Queued),
        new_run(2, eid(3), "r", RunStatus::Queued),
    ];
    runs[1].created_at = t(0);
    s.commit(&Commit {
        events,
        runs,
        deliveries: vec![delivery(1, eid(1), "r", 1, DeliveryOutcome::Fired, None)],
        ..Default::default()
    })
    .unwrap();
    // Finish run 2 long ago so it ages out; run 1 stays queued.
    s.transition_run(rid(2), RunTransition::Cancel, t(500)).unwrap();
    let day = 86_400_000;
    let policy = Retention {
        events_max_age: Duration::from_days(1),
        events_max_count: 1_000,
        runs_max_age: Duration::from_days(1),
    };
    // Events 1..=5 are older than a day at `now`; run 1 keeps event 2.
    let now = t(day + 5_500);
    let r = s.prune(&policy, now, 2).unwrap();
    assert_eq!((r.runs, r.events, r.more), (1, 2, true));
    let r = s.prune(&policy, now, 2).unwrap();
    assert_eq!(r.events, 2);
    let r = s.prune(&policy, now, 2).unwrap();
    assert_eq!((r.events, r.more), (0, false));
    let q = s.query();
    let left: Vec<u64> = q
        .query_events(&EventQuery::default())
        .unwrap()
        .events
        .iter()
        .map(|e| e.id.timestamp_ms())
        .collect();
    assert_eq!(left, vec![10, 9, 8, 7, 6, 2]);
    assert!(q.deliveries_for_event(eid(1)).unwrap().is_empty());
    assert!(
        q.query_events(&EventQuery {
            filter: EventFilter { path: Some("/a/1".into()), ..Default::default() },
            ..Default::default()
        })
        .unwrap()
        .events
        .is_empty()
    );

    // By count: at most 3 events. The one run 1 refers to is kept and counts toward the limit.
    let policy =
        Retention { events_max_count: 3, events_max_age: Duration::from_days(365), ..policy };
    while s.prune(&policy, now, 100).unwrap().more {}
    let left: Vec<u64> = s
        .query()
        .query_events(&EventQuery::default())
        .unwrap()
        .events
        .iter()
        .map(|e| e.id.timestamp_ms())
        .collect();
    assert_eq!(left, vec![10, 9, 2]);
}

#[tokio::test]
async fn handle_serializes_writes_and_reads_see_them() {
    let dir = tempfile::tempdir().unwrap();
    let handle = StoreHandle::open(&dir.path().join("pulse.db")).unwrap();
    let mut tasks = Vec::new();
    for n in 1..=20u64 {
        let h = handle.clone();
        tasks.push(tokio::spawn(
            async move { h.write(move |s| s.append_events(&[fs(n, "/x")])).await },
        ));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    let count = handle
        .read(|q| {
            Ok(q.query()
                .query_events(&EventQuery { limit: Some(500), ..Default::default() })?
                .events
                .len())
        })
        .await
        .unwrap();
    assert_eq!(count, 20);
    let err =
        handle.write(|s| s.transition_run(rid(1), RunTransition::Cancel, t(0))).await.unwrap_err();
    assert!(matches!(err, StoreError::NotFound { .. }));

    let mem = StoreHandle::spawn(Store::open_in_memory().unwrap(), None).unwrap();
    mem.write(|s| s.append_events(&[fs(1, "/m")])).await.unwrap();
    assert!(mem.read(|q| q.query().event(eid(1))).await.unwrap().is_some());
}
