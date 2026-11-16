//! SEP-3415 poll and push, through a real MCP client (stdio framing, in-process) and through the
//! HTTP router.
#![cfg(feature = "sep3415")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::time::Duration;

use axum::body::Body;
use http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use pulse_mcp::{Limits, McpConfig};
use rmcp::model::{ClientRequest, CustomRequest};
use rmcp::service::PeerRequestOptions;
use serde_json::{Value, json};
use support::*;
use tokio::time::Instant;
use tower::ServiceExt;

const SUB: &str = "io.modelcontextprotocol/subscriptionId";

fn http_request(token: &str, body: &Value, name_header: Option<&str>) -> Request<Body> {
    let mut b = Request::post("/")
        .header(header::HOST, "127.0.0.1:7477")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", body["method"].as_str().unwrap());
    if let Some(n) = name_header {
        b = b.header("mcp-name", n);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

fn rpc(id: i64, method: &str, mut params: Value) -> Value {
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": { "extensions": { "io.modelcontextprotocol/events": {} } },
    });
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

async fn json_body(response: http::Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let data = text.lines().find_map(|l| l.strip_prefix("data:")).unwrap_or(&text);
    serde_json::from_str(data.trim()).unwrap()
}

#[tokio::test]
async fn advertised_only_when_enabled() {
    let fake = Fake::new();
    for enabled in [true, false] {
        let config = McpConfig { events_extension: enabled, ..fake.config() };
        let router = pulse_mcp::router(fake.clone(), config);
        let discover = rpc(1, "server/discover", json!({}));
        let r = router.clone().oneshot(http_request(ALICE_TOKEN, &discover, None)).await.unwrap();
        let v = json_body(r).await;
        let ext = v.pointer("/result/capabilities/extensions/io.modelcontextprotocol~1events");
        assert_eq!(ext, enabled.then(|| json!({})).as_ref(), "{v}");
        let list = rpc(2, "events/list", json!({}));
        let r = router.clone().oneshot(http_request(ALICE_TOKEN, &list, None)).await.unwrap();
        let v = json_body(r).await;
        if enabled {
            assert_eq!(v.pointer("/result/resultType"), Some(&json!("complete")));
            continue;
        }
        assert_eq!(v.pointer("/error/code"), Some(&json!(-32601)));
        // Off means off: every extension method, including the ones that read events.
        for method in ["events/poll", "events/stream", "events/subscribe", "events/unsubscribe"] {
            let body = rpc(3, method, json!({ "name": "fs.file.created" }));
            let r = router
                .clone()
                .oneshot(http_request(ALICE_TOKEN, &body, Some("fs.file.created")))
                .await
                .unwrap();
            let v = json_body(r).await;
            assert_eq!(v.pointer("/error/code"), Some(&json!(-32601)), "{method}: {v}");
        }
    }
    assert_eq!(*fake.queries.borrow(), 0, "nothing was read with the extension off");
}

#[tokio::test(start_paused = true)]
async fn list_shows_the_agents_types_with_poll_and_push() {
    let fake = Fake::new();
    let (client, _) = connect(&fake, fake.config(), alice()).await;
    let v = custom(&client, "events/list", json!({})).await.unwrap();
    let events = v["events"].as_array().unwrap();
    let names: Vec<&str> = events.iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"fs.file.created") && names.contains(&"calendar.event.starting"));
    assert!(!names.iter().any(|n| n.starts_with("git.") || n.contains('*')), "{names:?}");
    for e in events {
        assert_eq!(e["delivery"], json!(["poll", "push"]));
        assert_eq!(e["inputSchema"]["type"], "object");
    }
}

#[tokio::test(start_paused = true)]
async fn poll_round_trip() {
    let fake = Fake::new();
    let old = fake.file("/home/me/University/old.pdf");
    let (client, _) = connect(&fake, fake.config(), alice()).await;
    let poll = |cursor: Value, extra: Value| {
        let mut p = json!({ "name": "fs.file.created", "cursor": cursor });
        p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        p
    };

    // Bootstrap: no events, a fresh cursor.
    let v = custom(&client, "events/poll", poll(Value::Null, json!({}))).await.unwrap();
    assert_eq!(v["events"], json!([]));
    assert_eq!((v["resultType"].clone(), v["hasMore"].clone()), (json!("complete"), json!(false)));
    let c0 = v["cursor"].clone();

    fake.tick();
    let a = fake.file("/home/me/University/a.pdf");
    fake.file("/home/me/Private/secret.pdf");
    fake.emit(
        "fs.file.deleted",
        Some("/home/me/University/x"),
        json!({ "path": "/home/me/University/x" }),
    );
    let b = fake.file("/home/me/University/b.pdf");

    let v =
        custom(&client, "events/poll", poll(c0.clone(), json!({ "maxEvents": 1 }))).await.unwrap();
    let got: Vec<&str> =
        v["events"].as_array().unwrap().iter().map(|e| e["eventId"].as_str().unwrap()).collect();
    assert_eq!(got, [a.id.to_string()]);
    assert_eq!(v["hasMore"], true);
    assert_eq!(v["events"][0]["name"], "fs.file.created");
    let v = custom(&client, "events/poll", poll(v["cursor"].clone(), json!({}))).await.unwrap();
    let got: Vec<&str> =
        v["events"].as_array().unwrap().iter().map(|e| e["eventId"].as_str().unwrap()).collect();
    assert_eq!(got, [b.id.to_string()]);
    assert_eq!(v["cursor"], b.id.to_string());
    let v = custom(&client, "events/poll", poll(v["cursor"].clone(), json!({}))).await.unwrap();
    assert_eq!(v["events"], json!([]));

    // Arguments filter; maxAgeMs floors a stale cursor and says so.
    let v = custom(
        &client,
        "events/poll",
        poll(c0.clone(), json!({ "arguments": { "subject": "/home/me/University/b*" } })),
    )
    .await
    .unwrap();
    assert_eq!(v["events"].as_array().unwrap().len(), 1);
    let from_start = json!(old.id.to_string());
    let v = custom(&client, "events/poll", poll(from_start.clone(), json!({}))).await.unwrap();
    assert_eq!((v["truncated"].clone(), v["events"].as_array().unwrap().len()), (json!(false), 2));
    let v =
        custom(&client, "events/poll", poll(from_start, json!({ "maxAgeMs": 0 }))).await.unwrap();
    assert_eq!(v["truncated"], true);
}

#[tokio::test(start_paused = true)]
async fn errors_use_the_provisional_codes() {
    let fake = Fake::new();
    let config =
        McpConfig { limits: Limits { streams_per_agent: 1, ..Limits::default() }, ..fake.config() };
    let (client, _) = connect(&fake, config, alice()).await;
    let cases = [
        (
            "events/poll",
            json!({ "name": "nope.thing" }),
            -32023,
            json!({ "kind": "event", "name": "nope.thing" }),
        ),
        ("events/poll", json!({ "name": "git.commit.created" }), -32024, Value::Null),
        (
            "events/poll",
            json!({ "name": "fs.file.created", "arguments": { "bogus": 1 } }),
            -32602,
            Value::Null,
        ),
        (
            "events/poll",
            json!({ "name": "fs.file.created", "cursor": "garbage" }),
            -32602,
            Value::Null,
        ),
        ("events/stream", json!({ "name": "fs.*" }), -32023, Value::Null),
        // Scopes apply to push as to poll and the tools: alice may not see git events.
        ("events/stream", json!({ "name": "git.commit.created" }), -32024, Value::Null),
        (
            "events/subscribe",
            json!({ "name": "fs.file.created" }),
            -32026,
            json!({ "feature": "deliveryMode", "value": "webhook" }),
        ),
        ("events/unknown", json!({}), -32601, Value::Null),
    ];
    for (method, params, code, data) in cases {
        let e = custom(&client, method, params.clone()).await.unwrap_err();
        assert_eq!(e.code.0, code, "{method} {params}");
        if !data.is_null() {
            assert_eq!(e.data, Some(data), "{method} {params}");
        }
    }

    // One stream per agent here: the second is refused before it opens.
    let first = client
        .peer()
        .send_cancellable_request(
            stream_request(json!({ "name": "fs.file.created" })),
            PeerRequestOptions::no_options(),
        )
        .await
        .unwrap();
    let e =
        custom(&client, "events/stream", json!({ "name": "fs.file.deleted" })).await.unwrap_err();
    assert_eq!(e.code.0, -32025);
    assert_eq!(e.data, Some(json!({ "limit": "streams", "max": 1 })));
    first.cancel(None).await.unwrap();
}

fn stream_request(params: Value) -> ClientRequest {
    ClientRequest::CustomRequest(CustomRequest::new("events/stream", Some(params)))
}

#[tokio::test(start_paused = true)]
async fn push_round_trip_with_two_streams_kept_apart() {
    let fake = Fake::new();
    let (client, mut notes) = connect(&fake, fake.config(), alice()).await;
    let open = |name: &str| {
        let peer = client.peer().clone();
        let params = json!({ "name": name, "cursor": null, "heartbeatMs": 5000 });
        async move {
            peer.send_cancellable_request(stream_request(params), PeerRequestOptions::no_options())
                .await
                .unwrap()
        }
    };
    let files = open("fs.file.created").await;
    let active = next_custom(&mut notes).await;
    assert_eq!(active.method, "notifications/events/active");
    let p = active.params.unwrap();
    let files_id = serde_json::to_value(&files.id).unwrap();
    assert_eq!(p["_meta"][SUB], files_id);
    assert_eq!((p["truncated"].clone(), p["heartbeatMs"].clone()), (json!(false), json!(5000)));

    let meetings = open("calendar.event.starting").await;
    let meetings_id = serde_json::to_value(&meetings.id).unwrap();
    let n = next_custom(&mut notes).await;
    assert_eq!(n.params.as_ref().unwrap()["_meta"][SUB], meetings_id, "{n:?}");

    let start = Instant::now();
    fake.tick();
    fake.file("/home/me/Private/no.pdf");
    let file = fake.file("/home/me/University/a.pdf");
    let event = next_custom(&mut notes).await;
    assert_eq!(event.method, "notifications/events/event");
    let p = event.params.unwrap();
    assert_eq!(p["_meta"][SUB], files_id);
    assert_eq!(
        (p["eventId"].clone(), p["cursor"].clone()),
        (json!(file.id.to_string()), json!(file.id.to_string()))
    );
    assert_eq!(p["data"]["path"], "/home/me/University/a.pdf");
    assert_eq!(Instant::now(), start, "pushed on arrival");

    let meeting =
        fake.emit("calendar.event.starting", Some("Standup"), json!({ "title": "Standup" }));
    let p = next_custom(&mut notes).await.params.unwrap();
    assert_eq!(
        (p["_meta"][SUB].clone(), p["eventId"].clone()),
        (meetings_id.clone(), json!(meeting.id.to_string()))
    );

    // Quiet: each stream heartbeats with its cursor once 5 s pass without a message.
    let mut beats = Vec::new();
    for _ in 0..2 {
        let n = next_custom(&mut notes).await;
        assert_eq!(n.method, "notifications/events/heartbeat");
        let p = n.params.unwrap();
        beats.push((p["_meta"][SUB].clone(), p["cursor"].clone()));
    }
    beats.sort_by_key(|(id, _)| id.to_string());
    let mut want = vec![
        (files_id.clone(), json!(file.id.to_string())),
        (meetings_id.clone(), json!(meeting.id.to_string())),
    ];
    want.sort_by_key(|(id, _)| id.to_string());
    assert_eq!(beats, want);
    assert_eq!(Instant::now() - start, Duration::from_secs(5));

    // Cancelling one stream releases exactly its backend subscription.
    files.cancel(None).await.unwrap();
    fake.closed_where(|f| f.types[0].as_str() == "fs.file.created").await;
    assert_eq!(fake.live(), 1);
    fake.file("/home/me/University/after-cancel.pdf");
    let later = fake.emit("calendar.event.starting", Some("Review"), json!({ "title": "Review" }));
    let n = next_custom(&mut notes).await;
    let p = n.params.unwrap();
    assert_eq!(
        (p["_meta"][SUB].clone(), p["eventId"].clone()),
        (meetings_id, json!(later.id.to_string()))
    );
    meetings.cancel(None).await.unwrap();
    fake.all_closed(&alice()).await;
}

#[tokio::test(start_paused = true)]
async fn stream_replays_from_a_cursor_and_ends_on_revocation() {
    let fake = Fake::new();
    let first = fake.file("/home/me/University/1.pdf");
    let second = fake.file("/home/me/University/2.pdf");
    let (client, mut notes) = connect(&fake, fake.config(), alice()).await;
    let handle = client
        .peer()
        .send_cancellable_request(
            stream_request(json!({ "name": "fs.file.created", "cursor": first.id.to_string() })),
            PeerRequestOptions::no_options(),
        )
        .await
        .unwrap();
    let id = handle.id.clone();
    let active = next_custom(&mut notes).await.params.unwrap();
    assert_eq!(active["cursor"], first.id.to_string());
    let replayed = next_custom(&mut notes).await.params.unwrap();
    assert_eq!(replayed["eventId"], second.id.to_string());

    fake.revoke(&alice());
    let n = next_custom(&mut notes).await;
    assert_eq!(n.method, "notifications/events/terminated");
    assert_eq!(n.params.unwrap()["error"]["code"], -32024);
    // On stdio the server sends the final StreamEventsResult after terminating.
    // (rmcp's client parses it as the look-alike SubscriptionsListenResult.)
    let done = serde_json::to_value(handle.await_response().await.unwrap()).unwrap();
    assert_eq!(done["resultType"], "complete");
    assert_eq!(done["_meta"][SUB], serde_json::to_value(&id).unwrap());
}

#[tokio::test(start_paused = true)]
async fn http_disconnect_ends_the_stream() {
    let fake = Fake::new();
    let router = pulse_mcp::router(fake.clone(), fake.config());
    let body = rpc(9, "events/stream", json!({ "name": "fs.file.created" }));
    let response =
        router.oneshot(http_request(ALICE_TOKEN, &body, Some("fs.file.created"))).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let mut seen = String::new();
    while !seen.contains("notifications/events/active") {
        let frame = body.frame().await.unwrap().unwrap();
        if let Ok(data) = frame.into_data() {
            seen.push_str(std::str::from_utf8(&data).unwrap());
        }
    }
    assert!(seen.contains(&format!("\"{SUB}\":9")), "{seen}");
    assert_eq!(fake.live(), 1);
    let start = Instant::now();
    drop(body);
    fake.all_closed(&alice()).await;
    assert_eq!(Instant::now(), start, "released on disconnect, not at the next heartbeat");
}

#[tokio::test]
async fn mcp_name_is_required_and_must_match() {
    let fake = Fake::new();
    let router = pulse_mcp::router(fake.clone(), fake.config());
    let body = rpc(1, "events/poll", json!({ "name": "fs.file.created" }));
    let cases = [
        (Some("fs.file.created"), StatusCode::OK),
        (Some("=?base64?ZnMuZmlsZS5jcmVhdGVk?="), StatusCode::OK),
        (Some("fs.file.deleted"), StatusCode::BAD_REQUEST),
        (Some("=?base64?not base64?="), StatusCode::BAD_REQUEST),
        (None, StatusCode::BAD_REQUEST),
    ];
    for (header, want) in cases {
        let r = router.clone().oneshot(http_request(ALICE_TOKEN, &body, header)).await.unwrap();
        assert_eq!(r.status(), want, "{header:?}");
        let v = json_body(r).await;
        if want == StatusCode::BAD_REQUEST {
            assert_eq!(v.pointer("/error/code"), Some(&json!(-32020)), "{v}");
        } else {
            assert_eq!(v.pointer("/result/resultType"), Some(&json!("complete")), "{v}");
        }
    }
}

/// Events of about 7 KiB each: three fill most of a 24 KiB page.
fn emit_big(fake: &Fake, n: usize) -> Vec<String> {
    (0..n)
        .map(|i| {
            let path = format!("/home/me/University/{i}.pdf");
            let mut data = json!({ "path": path });
            for f in 0..7 {
                data[format!("f{f}")] = json!("x".repeat(1000));
            }
            fake.emit("fs.file.created", Some(&path), data).id.to_string()
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn poll_pages_are_bounded_in_bytes_and_skip_nothing() {
    let fake = Fake::new();
    let start = fake.file("/home/me/University/start.pdf").id.to_string();
    let emitted = emit_big(&fake, 12);
    let (client, _) = connect(&fake, fake.config(), alice()).await;
    let budget = Limits::default().result_max_bytes;
    let mut cursor = json!(start);
    let mut got = Vec::new();
    let mut pages = 0;
    loop {
        let params = json!({ "name": "fs.file.created", "cursor": cursor, "maxEvents": 200 });
        let v = custom(&client, "events/poll", params).await.unwrap();
        pages += 1;
        let events = v["events"].as_array().unwrap();
        assert!(!events.is_empty());
        assert!(v["events"].to_string().len() <= budget, "page {pages}");
        got.extend(events.iter().map(|e| e["eventId"].as_str().unwrap().to_owned()));
        // The cursor is the last event delivered, so the next page starts right after it.
        assert_eq!(v["cursor"], *got.last().unwrap());
        cursor = v["cursor"].clone();
        if v["hasMore"] != true {
            break;
        }
    }
    assert_eq!(got, emitted);
    assert!(pages >= 4, "{pages}");
}

#[tokio::test(start_paused = true)]
async fn push_pages_are_bounded_in_bytes_and_skip_nothing() {
    let fake = Fake::new();
    let start = fake.file("/home/me/University/start.pdf").id.to_string();
    let emitted = emit_big(&fake, 12);
    let (client, mut notes) = connect(&fake, fake.config(), alice()).await;
    let reads = *fake.queries.borrow();
    let handle = client
        .peer()
        .send_cancellable_request(
            stream_request(json!({ "name": "fs.file.created", "cursor": start })),
            PeerRequestOptions::no_options(),
        )
        .await
        .unwrap();
    assert_eq!(next_custom(&mut notes).await.method, "notifications/events/active");
    let mut got = Vec::new();
    while got.len() < emitted.len() {
        let p = next_custom(&mut notes).await.params.unwrap();
        assert_eq!(p["cursor"], p["eventId"]);
        got.push(p["eventId"].as_str().unwrap().to_owned());
    }
    assert_eq!(got, emitted);
    // About 84 KiB of events: read and sent a budget's worth at a time, not in one page.
    assert!(*fake.queries.borrow() - reads >= 4, "{}", *fake.queries.borrow() - reads);
    // Then quiet: the heartbeat carries the last cursor sent.
    let n = next_custom(&mut notes).await;
    assert_eq!(n.method, "notifications/events/heartbeat");
    assert_eq!(n.params.unwrap()["cursor"], *emitted.last().unwrap());
    handle.cancel(None).await.unwrap();
}
