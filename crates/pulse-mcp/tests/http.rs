//! The `/mcp` router, driven in-process with raw HTTP requests as a 2026-07-28 client sends
//! them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use pulse_mcp::{Limits, McpConfig};
use serde_json::{Value, json};
use support::*;
use tower::ServiceExt;

pub fn rpc(id: i64, method: &str, mut params: Value) -> Value {
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
    });
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

pub fn request(token: Option<&str>, body: &Value) -> http::request::Builder {
    let method = body["method"].as_str().unwrap();
    let mut b = Request::post("/")
        .header(header::HOST, "127.0.0.1:7477")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", method);
    if let Some(name) = body.pointer("/params/name").and_then(Value::as_str) {
        b = b.header("mcp-name", name);
    }
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    b
}

/// Every JSON-RPC message in a response, whether it came as JSON or as SSE.
pub async fn messages(response: http::Response<Body>) -> Vec<Value> {
    let sse = response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|c| c.to_str().unwrap().starts_with("text/event-stream"));
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    if !sse {
        return vec![serde_json::from_str(&text).unwrap()];
    }
    text.lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(|d| serde_json::from_str(d).unwrap())
        .collect()
}

async fn tool_over_http(router: &axum::Router, token: &str, name: &str, args: Value) -> String {
    let body = rpc(1, "tools/call", json!({ "name": name, "arguments": args }));
    let response = router
        .clone()
        .oneshot(request(Some(token), &body).body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // One request per connection: no idle keep-alive sockets outside the request budget.
    assert_eq!(response.headers()[header::CONNECTION], "close");
    let msgs = messages(response).await;
    let result = msgs.iter().find(|m| m.get("id") == Some(&json!(1))).unwrap();
    result.pointer("/result/content/0/text").and_then(Value::as_str).unwrap().to_owned()
}

#[tokio::test]
async fn missing_or_unknown_tokens_are_rejected_before_mcp() {
    let fake = Fake::new();
    let router = pulse_mcp::router(fake.clone(), fake.config());
    let body = rpc(1, "tools/call", json!({ "name": "query_events", "arguments": {} }));
    let cases = [
        (None, "missing bearer token", 0),
        (Some("Basic dG9rLWFsaWNl"), "missing bearer token", 0),
        (Some("Bearer "), "missing bearer token", 0),
        (Some("Bearer tok-mallory"), "unknown token", 1),
        // Another agent's name is not a token.
        (Some("Bearer alice"), "unknown token", 2),
    ];
    for (auth, want, resolves) in cases {
        let mut b = request(None, &body);
        if let Some(a) = auth {
            b = b.header(header::AUTHORIZATION, a);
        }
        let response =
            router.clone().oneshot(b.body(Body::from(body.to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{auth:?}");
        assert_eq!(response.headers()[header::CONNECTION], "close");
        assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer realm=\"pulse\"");
        let text = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(text, want.as_bytes());
        assert_eq!(*fake.resolves.borrow(), resolves, "{auth:?}");
    }
    assert_eq!(*fake.queries.borrow(), 0, "no request reached a tool");
}

#[tokio::test]
async fn host_and_origin_are_checked() {
    let fake = Fake::new();
    let router = pulse_mcp::router(fake.clone(), fake.config());
    let body = rpc(1, "tools/call", json!({ "name": "list_event_types", "arguments": {} }));
    let cases = [
        (vec![(header::HOST, "evil.example:7477")], StatusCode::FORBIDDEN),
        (vec![(header::ORIGIN, "https://evil.example")], StatusCode::FORBIDDEN),
        (vec![(header::ORIGIN, "http://127.0.0.1:7477")], StatusCode::FORBIDDEN),
        (vec![], StatusCode::OK),
    ];
    for (headers, want) in cases {
        let mut b = request(Some(ALICE_TOKEN), &body);
        for (k, v) in &headers {
            b.headers_mut().unwrap().insert(k, v.parse().unwrap());
        }
        let response =
            router.clone().oneshot(b.body(Body::from(body.to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), want, "{headers:?}");
    }
}

#[tokio::test]
async fn each_request_acts_as_its_own_token() {
    let fake = Fake::new();
    let lecture = fake.file("/home/me/University/a.pdf");
    let commit =
        fake.emit("git.commit.created", Some("/r"), json!({ "repo": "/r", "message": "m" }));
    let router = pulse_mcp::router(fake.clone(), fake.config());

    let alice_sees = tool_over_http(&router, ALICE_TOKEN, "query_events", json!({})).await;
    assert_eq!(ids(&split(&alice_sees).1), [lecture.id.to_string()]);
    let bob_sees = tool_over_http(&router, BOB_TOKEN, "query_events", json!({})).await;
    assert_eq!(ids(&split(&bob_sees).1), [commit.id.to_string()]);

    // Subscriptions outlive a request (handlers are per request) but not their owner.
    fake.tick();
    let sub: Value =
        serde_json::from_str(&tool_over_http(&router, BOB_TOKEN, "subscribe", json!({})).await)
            .unwrap();
    let args = json!({ "subscription": sub["subscription"], "cursor": sub["cursor"], "timeout_seconds": 0 });
    let as_alice = tool_over_http(&router, ALICE_TOKEN, "wait_for_events", args.clone()).await;
    assert!(as_alice.contains("unknown or expired"), "{as_alice}");
    let as_bob = tool_over_http(&router, BOB_TOKEN, "wait_for_events", args).await;
    assert_eq!(split(&as_bob).0["timed_out"], true);
}

/// A request as a client on protocol 2025-06-18 sends it: sessions, no per-request metadata.
fn legacy(
    token: &str,
    method: http::Method,
    session: Option<&str>,
    body: Option<&Value>,
) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri("/")
        .header(header::HOST, "127.0.0.1:7477")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if let Some(s) = session {
        b = b.header("mcp-session-id", s).header("mcp-protocol-version", "2025-06-18");
    }
    b.body(body.map_or_else(Body::empty, |v| Body::from(v.to_string()))).unwrap()
}

/// `initialize` and `notifications/initialized`; returns the session id.
async fn open_session(router: &axum::Router, token: &str) -> String {
    let init = json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": { "name": "test", "version": "0" },
    }});
    let r =
        router.clone().oneshot(legacy(token, http::Method::POST, None, Some(&init))).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let id = r.headers()["mcp-session-id"].to_str().unwrap().to_owned();
    messages(r).await;
    let done = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    let r = router
        .clone()
        .oneshot(legacy(token, http::Method::POST, Some(&id), Some(&done)))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::ACCEPTED);
    id
}

async fn list_tools_in(router: &axum::Router, token: &str, session: &str) -> StatusCode {
    let list = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
    let r = router
        .clone()
        .oneshot(legacy(token, http::Method::POST, Some(session), Some(&list)))
        .await
        .unwrap();
    let status = r.status();
    drop(r.into_body().collect().await);
    status
}

#[tokio::test]
async fn sessions_are_bounded_per_agent_and_the_least_recently_used_idle_one_goes() {
    let fake = Fake::new();
    let config = McpConfig {
        limits: Limits { sessions_per_agent: 2, ..Limits::default() },
        ..fake.config()
    };
    let router = pulse_mcp::router(fake.clone(), config);
    let first = open_session(&router, ALICE_TOKEN).await;
    let second = open_session(&router, ALICE_TOKEN).await;
    let bobs = open_session(&router, BOB_TOKEN).await;
    let third = open_session(&router, ALICE_TOKEN).await;
    // The first was the least recently used: closed, and a client that still holds it is told
    // to start over (404 is the spec's "session terminated").
    assert_eq!(list_tools_in(&router, ALICE_TOKEN, &first).await, StatusCode::NOT_FOUND);
    for (token, s) in [(ALICE_TOKEN, &second), (ALICE_TOKEN, &third), (BOB_TOKEN, &bobs)] {
        assert_eq!(list_tools_in(&router, token, s).await, StatusCode::OK);
    }

    // A session with an open stream is not idle: `second` is now the least recently used, but
    // its client is listening, so `third` goes instead.
    let listening = router
        .clone()
        .oneshot(legacy(ALICE_TOKEN, http::Method::GET, Some(&second), None))
        .await
        .unwrap();
    assert_eq!(listening.status(), StatusCode::OK);
    assert_eq!(list_tools_in(&router, ALICE_TOKEN, &third).await, StatusCode::OK);
    let fourth = open_session(&router, ALICE_TOKEN).await;
    assert_eq!(list_tools_in(&router, ALICE_TOKEN, &third).await, StatusCode::NOT_FOUND);
    for s in [&second, &fourth] {
        assert_eq!(list_tools_in(&router, ALICE_TOKEN, s).await, StatusCode::OK);
    }
    assert_eq!(list_tools_in(&router, BOB_TOKEN, &bobs).await, StatusCode::OK);
    drop(listening);
}

/// A `wait_for_events` call that stays open; its first progress notification opens the
/// response.
fn long_wait(token: &str, id: i64, subscription: &Value, cursor: &Value) -> Request<Body> {
    let mut body = rpc(
        id,
        "tools/call",
        json!({ "name": "wait_for_events", "arguments": {
            "subscription": subscription, "cursor": cursor, "timeout_seconds": 600,
        }}),
    );
    body["params"]["_meta"]["progressToken"] = json!(id);
    request(Some(token), &body).body(Body::from(body.to_string())).unwrap()
}

async fn subscribe_over_http(router: &axum::Router, token: &str) -> (Value, Value) {
    let v: Value =
        serde_json::from_str(&tool_over_http(router, token, "subscribe", json!({})).await).unwrap();
    (v["subscription"].clone(), v["cursor"].clone())
}

#[tokio::test]
async fn requests_in_flight_are_bounded_per_agent_and_in_total() {
    let fake = Fake::new();
    let config = McpConfig {
        limits: Limits { requests_per_agent: 2, requests_total: 3, ..Limits::default() },
        ..fake.config()
    };
    let router = pulse_mcp::router(fake.clone(), config);
    let (alice_sub, alice_cursor) = subscribe_over_http(&router, ALICE_TOKEN).await;
    let (bob_sub, bob_cursor) = subscribe_over_http(&router, BOB_TOKEN).await;
    let mut held = Vec::new();
    for id in [1, 2] {
        let r = router.clone().oneshot(long_wait(ALICE_TOKEN, id, &alice_sub, &alice_cursor)).await;
        let r = r.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        held.push(r);
    }
    let query = rpc(3, "tools/call", json!({ "name": "query_events", "arguments": {} }));
    let send_query = |token| {
        router
            .clone()
            .oneshot(request(Some(token), &query).body(Body::from(query.to_string())).unwrap())
    };
    let reads = *fake.queries.borrow();
    let refused = send_query(ALICE_TOKEN).await.unwrap();
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(refused.headers()[header::CONNECTION], "close");
    assert!(refused.headers().contains_key(header::RETRY_AFTER));
    let error = messages(refused).await.remove(0);
    assert_eq!(error["error"]["code"], -32000, "{error}");
    assert!(error["error"]["message"].as_str().unwrap().contains("requests in flight"), "{error}");
    assert_eq!(*fake.queries.borrow(), reads, "refused before any work");

    // Another agent is not affected by alice's share, until the total is reached.
    let r = router.clone().oneshot(long_wait(BOB_TOKEN, 4, &bob_sub, &bob_cursor)).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    held.push(r);
    let refused = send_query(BOB_TOKEN).await.unwrap();
    assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);

    // A request ends when its response is dropped (the client disconnected).
    drop(held.remove(0));
    assert_eq!(send_query(ALICE_TOKEN).await.unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn oversized_requests_are_refused() {
    let fake = Fake::new();
    let router = pulse_mcp::router(fake.clone(), fake.config());
    let body = rpc(
        1,
        "tools/call",
        json!({ "name": "query_events", "arguments": { "text": "x".repeat(100 * 1024) } }),
    );
    let r = router
        .oneshot(request(Some(ALICE_TOKEN), &body).body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(*fake.queries.borrow(), 0);
}
