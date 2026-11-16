//! Every tool through a real MCP client, against the fake backend. Time is paused: deadlines
//! and expiry happen when the runtime is otherwise idle, never by sleeping.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::time::Duration;

use pulse_mcp::{Limits, McpConfig};
use rmcp::model::{NumberOrString, ProgressToken, RequestMetaObject};
use serde_json::{Value, json};
use support::*;
use tokio::time::Instant;

#[tokio::test(start_paused = true)]
async fn list_and_query_show_only_what_the_agent_may_see() {
    let fake = Fake::new();
    let lecture = fake.file("/home/me/University/a.pdf");
    fake.file("/home/me/Private/diary.txt");
    fake.emit(
        "git.commit.created",
        Some("/home/me/code"),
        json!({ "repo": "/home/me/code", "message": "x" }),
    );
    let meeting =
        fake.emit("calendar.event.starting", Some("Standup"), json!({ "title": "Standup" }));
    let (client, _) = connect(&fake, fake.config(), alice()).await;

    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(
        names,
        ["list_event_types", "query_events", "subscribe", "unsubscribe", "wait_for_events"]
    );
    for t in &tools {
        let d = t.description.as_deref().unwrap();
        assert!(
            d.len() > 150 && (t.name == "list_event_types" || d.contains("Example")),
            "{}",
            t.name
        );
    }

    let (err, text) = tool(&client, "list_event_types", json!({})).await;
    assert!(!err);
    let catalog: Value = serde_json::from_str(&text).unwrap();
    let types: Vec<&str> = catalog["event_types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["type"].as_str().unwrap())
        .collect();
    assert!(types.contains(&"fs.file.created") && types.contains(&"calendar.event.ended"));
    assert!(!types.iter().any(|t| t.starts_with("git.") || t.starts_with("process.")), "{types:?}");

    let (err, text) = tool(&client, "query_events", json!({})).await;
    assert!(!err, "{text}");
    let (summary, events) = split(&text);
    assert_eq!(ids(&events), [meeting.id.to_string(), lecture.id.to_string()]);
    assert_eq!(summary["next_cursor"], Value::Null);
    assert!(text.contains("untrusted data"));

    let (_, text) =
        tool(&client, "query_events", json!({ "types": ["fs.*"], "order": "oldest_first" })).await;
    assert_eq!(ids(&split(&text).1), [lecture.id.to_string()]);

    // Pagination by cursor.
    let (_, text) = tool(&client, "query_events", json!({ "limit": 1 })).await;
    let (summary, page1) = split(&text);
    assert_eq!(ids(&page1), [meeting.id.to_string()]);
    let cursor = summary["next_cursor"].as_str().unwrap();
    let (_, text) = tool(&client, "query_events", json!({ "limit": 1, "cursor": cursor })).await;
    let (_, page2) = split(&text);
    // The hidden rows between the two visible events make some pages short or empty; walking
    // the cursor reaches the lecture without ever showing a hidden event.
    let mut seen = ids(&page2);
    let mut next = split(&text).0["next_cursor"].clone();
    while let Some(c) = next.as_str() {
        let (_, text) = tool(&client, "query_events", json!({ "limit": 1, "cursor": c })).await;
        let (s, evs) = split(&text);
        seen.extend(ids(&evs));
        next = s["next_cursor"].clone();
    }
    assert_eq!(seen, [lecture.id.to_string()]);
}

#[tokio::test(start_paused = true)]
async fn arguments_cannot_name_an_identity_and_bad_input_is_explained() {
    let fake = Fake::new();
    let (client, _) = connect(&fake, fake.config(), alice()).await;
    let cases = [
        ("query_events", json!({ "agent": "bob" }), "unknown field `agent`"),
        ("subscribe", json!({ "agent": "bob" }), "unknown field `agent`"),
        ("list_event_types", json!({ "as": "bob" }), "unknown field"),
        ("query_events", json!({ "types": ["Git"] }), "types:"),
        ("query_events", json!({ "path": "~/University" }), "absolute"),
        ("query_events", json!({ "since": "yesterday" }), "since:"),
        ("query_events", json!({ "cursor": "nope" }), "cursor"),
        ("wait_for_events", json!({ "subscription": "x", "cursor": "y" }), "subscribe"),
        ("unsubscribe", json!({ "subscription": "01JD8X3T9Q2V7M4K6N0P5R1S8W" }), "unknown"),
    ];
    for (name, args, want) in cases {
        let (err, text) = tool(&client, name, args.clone()).await;
        assert!(err && text.contains(want), "{name} {args}: {text}");
    }
}

#[tokio::test(start_paused = true)]
async fn subscriptions_belong_to_one_agent() {
    let fake = Fake::new();
    let (alice_client, _) = connect(&fake, fake.config(), alice()).await;
    let (bob_client, _) = connect(&fake, fake.config(), bob()).await;
    let (_, text) = tool(&alice_client, "subscribe", json!({ "types": ["fs.*"] })).await;
    let sub: Value = serde_json::from_str(&text).unwrap();
    let args = json!({ "subscription": sub["subscription"], "cursor": sub["cursor"], "timeout_seconds": 0 });
    let (err, text) = tool(&bob_client, "wait_for_events", args.clone()).await;
    assert!(err && text.contains("unknown or expired"), "{text}");
    let (err, _) =
        tool(&bob_client, "unsubscribe", json!({ "subscription": sub["subscription"] })).await;
    assert!(err);
    let (err, _) = tool(&alice_client, "wait_for_events", args).await;
    assert!(!err);
}

async fn subscribe(client: &Client, filter: Value) -> (Value, Value) {
    let (err, text) = tool(client, "subscribe", filter).await;
    assert!(!err, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    (v["subscription"].clone(), v["cursor"].clone())
}

#[tokio::test(start_paused = true)]
async fn wait_returns_at_once_on_arrival_or_empty_at_the_deadline() {
    let fake = Fake::new();
    let (client, _) = connect(&fake, fake.config(), alice()).await;
    let (sub, cursor) = subscribe(&client, json!({ "types": ["fs.*"] })).await;

    // Already queued: no time passes.
    fake.tick();
    let first = fake.file("/home/me/University/1.pdf");
    let start = Instant::now();
    let (_, text) =
        tool(&client, "wait_for_events", json!({ "subscription": sub, "cursor": cursor })).await;
    let (summary, events) = split(&text);
    assert_eq!(ids(&events), [first.id.to_string()]);
    assert_eq!(Instant::now(), start);
    let cursor = summary["cursor"].clone();
    assert_eq!(cursor, first.id.to_string());

    // Arrives while waiting: the wait reads once, finds nothing, and wakes on the event.
    let mut reads = fake.queries.subscribe();
    let peer = client.peer().clone();
    let args = json!({ "subscription": sub, "cursor": cursor });
    let waiting = tokio::spawn(async move { peer.call_tool(call("wait_for_events", args)).await });
    let before = *reads.borrow_and_update();
    reads.wait_for(|n| *n > before).await.unwrap();
    // Out of scope: does not end the wait.
    fake.file("/home/me/Private/x");
    let second = fake.file("/home/me/University/2.pdf");
    let result = waiting.await.unwrap().unwrap();
    let text = result.content[0].as_text().unwrap().text.clone();
    let (summary, events) = split(&text);
    assert_eq!(ids(&events), [second.id.to_string()]);
    assert_eq!(Instant::now(), start, "woken by the event, not a timer");

    // Nothing arrives: empty at exactly the deadline, cursor unchanged.
    let cursor = summary["cursor"].clone();
    let (err, text) = tool(
        &client,
        "wait_for_events",
        json!({ "subscription": sub, "cursor": cursor, "timeout_seconds": 30 }),
    )
    .await;
    assert!(!err);
    let (summary, events) = split(&text);
    assert!(events.is_empty());
    assert_eq!(summary["timed_out"], true);
    assert_eq!(summary["cursor"], cursor);
    assert_eq!(Instant::now() - start, Duration::from_secs(30));
}

#[tokio::test(start_paused = true)]
async fn long_waits_report_progress_when_asked() {
    let fake = Fake::new();
    let (client, mut notes) = connect(&fake, fake.config(), alice()).await;
    let (sub, cursor) = subscribe(&client, json!({})).await;
    let mut p = call("wait_for_events", json!({ "subscription": sub, "cursor": cursor }));
    p.meta = Some(RequestMetaObject::with_progress_token(ProgressToken(NumberOrString::Number(7))));
    let r = client.call_tool(p).await.unwrap();
    assert!(!r.is_error.unwrap_or(false));
    let mut ticks = Vec::new();
    while let Ok(n) = notes.try_recv() {
        if let Note::Progress(p) = n {
            ticks.push(p.progress);
        }
    }
    // At the start, then every 15 s of the default 45 s wait.
    assert_eq!(ticks, [0.0, 15.0, 30.0]);
}

#[tokio::test(start_paused = true)]
async fn a_backlog_is_delivered_in_order_and_an_old_cursor_redelivers() {
    let fake = Fake::new();
    let (client, _) = connect(&fake, fake.config(), alice()).await;
    let (sub, start) = subscribe(&client, json!({ "types": ["fs.file.created"] })).await;
    fake.tick();
    // Far more than the live channel holds (it drops batches); the log has them all.
    let emitted: Vec<String> = (0..120)
        .map(|i| fake.file(&format!("/home/me/University/{i}.pdf")).id.to_string())
        .collect();
    let mut got = Vec::new();
    let mut cursor = start.clone();
    let mut calls = 0;
    loop {
        calls += 1;
        let (_, text) = tool(
            &client,
            "wait_for_events",
            json!({ "subscription": sub, "cursor": cursor, "max_events": 50 }),
        )
        .await;
        let (summary, events) = split(&text);
        got.extend(ids(&events));
        cursor = summary["cursor"].clone();
        if summary["has_more"] != true {
            break;
        }
    }
    assert_eq!(got, emitted);
    assert_eq!(calls, 3);

    // A result the client never saw (its tool call timed out) comes back with the old cursor.
    let (_, text) = tool(
        &client,
        "wait_for_events",
        json!({ "subscription": sub, "cursor": start, "max_events": 2 }),
    )
    .await;
    assert_eq!(ids(&split(&text).1), emitted[..2]);
}

#[tokio::test(start_paused = true)]
async fn results_are_bounded_and_say_so() {
    let fake = Fake::new();
    let config = McpConfig {
        limits: Limits { result_max_bytes: 4000, ..Limits::default() },
        ..fake.config()
    };
    let (client, _) = connect(&fake, config, alice()).await;
    for i in 0..10 {
        fake.emit(
            "fs.file.created",
            Some(&format!("/home/me/University/{i}")),
            json!({ "path": format!("/home/me/University/{i}"), "note": "z".repeat(3000) }),
        );
    }
    let (_, text) = tool(&client, "query_events", json!({ "limit": 50 })).await;
    let (summary, events) = split(&text);
    assert!(events.len() < 10 && !events.is_empty());
    assert_eq!(summary["count"], events.len());
    assert_eq!(summary["shortened_values"], true);
    assert!(events[0]["data"]["note"].as_str().unwrap().contains("more bytes]"));
    // The page stops where the budget did, and the cursor continues from there.
    let cursor = summary["next_cursor"].as_str().unwrap();
    assert_eq!(cursor, events.last().unwrap()["id"]);
    assert!(text.len() < 6000);
}

#[tokio::test(start_paused = true)]
async fn subscriptions_expire_when_unused_and_resume_from_a_cursor() {
    let fake = Fake::new();
    let config = McpConfig {
        limits: Limits { subscription_idle_ttl: Duration::from_secs(60), ..Limits::default() },
        ..fake.config()
    };
    let (client, _) = connect(&fake, config, alice()).await;
    let (sub, cursor) = subscribe(&client, json!({ "types": ["fs.*"] })).await;

    // A wait longer than the ttl keeps it alive.
    let (err, _) = tool(
        &client,
        "wait_for_events",
        json!({ "subscription": sub, "cursor": cursor, "timeout_seconds": 120 }),
    )
    .await;
    assert!(!err);
    assert_eq!(fake.live(), 1);

    // Then nobody waits: it is removed and the backend subscription released.
    let start = Instant::now();
    fake.all_closed(&alice()).await;
    assert_eq!(Instant::now() - start, Duration::from_secs(60));
    fake.tick();
    let missed = fake.file("/home/me/University/during-gap.pdf");
    let (err, text) =
        tool(&client, "wait_for_events", json!({ "subscription": sub, "cursor": cursor })).await;
    assert!(err && text.contains("Subscribe again"), "{text}");

    // Resubscribing and keeping the old cursor delivers what happened in between.
    let (sub2, _) = subscribe(&client, json!({ "types": ["fs.*"] })).await;
    let (_, text) =
        tool(&client, "wait_for_events", json!({ "subscription": sub2, "cursor": cursor })).await;
    assert_eq!(ids(&split(&text).1), [missed.id.to_string()]);
}

#[tokio::test(start_paused = true)]
async fn subscribing_is_idempotent_and_bounded() {
    let fake = Fake::new();
    let config = McpConfig {
        limits: Limits { subscriptions_per_agent: 2, ..Limits::default() },
        ..fake.config()
    };
    let (client, _) = connect(&fake, config, alice()).await;
    let (a, _) = subscribe(&client, json!({ "types": ["fs.*"] })).await;
    let (again, _) = subscribe(&client, json!({ "types": ["fs.*"] })).await;
    assert_eq!(a, again);
    let (b, _) = subscribe(&client, json!({ "types": ["calendar.*"] })).await;
    let (err, text) = tool(&client, "subscribe", json!({ "text": "x" })).await;
    assert!(
        err && text.contains(a.as_str().unwrap()) && text.contains(b.as_str().unwrap()),
        "{text}"
    );
    assert_eq!(fake.live(), 2);
    for s in [a, b] {
        let (err, _) = tool(&client, "unsubscribe", json!({ "subscription": s })).await;
        assert!(!err);
    }
    fake.all_closed(&alice()).await;
    let (err, _) = tool(&client, "subscribe", json!({ "text": "x" })).await;
    assert!(!err);
}

#[tokio::test(start_paused = true)]
async fn revocation_ends_a_wait() {
    let fake = Fake::new();
    let (client, _) = connect(&fake, fake.config(), alice()).await;
    let (sub, cursor) = subscribe(&client, json!({})).await;
    let mut reads = fake.queries.subscribe();
    let peer = client.peer().clone();
    let args = json!({ "subscription": sub, "cursor": cursor, "timeout_seconds": 600 });
    let waiting = tokio::spawn(async move { peer.call_tool(call("wait_for_events", args)).await });
    let before = *reads.borrow_and_update();
    reads.wait_for(|n| *n > before).await.unwrap();
    let start = Instant::now();
    fake.revoke(&alice());
    let r = waiting.await.unwrap().unwrap();
    assert!(r.is_error.unwrap_or(false));
    assert!(r.content[0].as_text().unwrap().text.contains("ended by Pulse"));
    assert_eq!(Instant::now(), start);
}

#[tokio::test(start_paused = true)]
async fn clients_on_the_initialize_handshake_work_too() {
    let fake = Fake::new();
    fake.file("/home/me/University/a.pdf");
    let lifecycle = rmcp::service::ClientLifecycleMode::Initialize;
    let (client, _) = connect_with(&fake, fake.config(), alice(), lifecycle).await;
    let (err, text) = tool(&client, "query_events", json!({})).await;
    assert!(!err);
    assert_eq!(split(&text).1.len(), 1);
}
