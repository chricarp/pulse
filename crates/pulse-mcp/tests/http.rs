//! The `/mcp` router, driven in-process with raw HTTP requests as a 2026-07-28 client sends
//! them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use http::{Request, StatusCode, header};
use http_body_util::BodyExt;
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
