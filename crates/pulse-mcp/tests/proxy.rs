//! `pulse mcp` end to end: an MCP client on stdio framing, the proxy, real TCP on a loopback
//! port, and the `/mcp` router. Real time (sockets do not mix with a paused clock); every wait
//! is for a specific message.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::sync::Arc;

use pulse_mcp::stdio::{CheckError, Target, check, proxy};
use rmcp::serve_client_with_lifecycle;
use rmcp::service::ClientLifecycleMode;
use serde_json::{Value, json};
use support::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// Serves the router on 127.0.0.1:0 and returns its MCP URL.
async fn daemon(fake: &Arc<Fake>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new().nest("/mcp", pulse_mcp::router(fake.clone(), fake.config()));
    tokio::spawn(async move { axum::serve(listener, app).await });
    format!("http://{addr}/mcp")
}

/// A proxy for `token`, with an MCP client on its stdio side.
async fn client_via_proxy(
    url: &str,
    token: &str,
    lifecycle: ClientLifecycleMode,
) -> (Client, mpsc::UnboundedReceiver<Note>) {
    let (client_io, proxy_io) = tokio::io::duplex(1 << 20);
    let (pr, pw) = tokio::io::split(proxy_io);
    tokio::spawn(proxy(Target::new(url, token).unwrap(), pr, pw));
    let (cr, cw) = tokio::io::split(client_io);
    let (tx, rx) = mpsc::unbounded_channel();
    (serve_client_with_lifecycle(Recorder(tx), (cr, cw), lifecycle).await.unwrap(), rx)
}

#[tokio::test]
async fn tools_work_through_the_proxy_as_the_tokens_agent() {
    let fake = Fake::new();
    let lecture = fake.file("/home/me/University/a.pdf");
    fake.emit("git.commit.created", Some("/r"), json!({ "repo": "/r", "message": "m" }));
    let url = daemon(&fake).await;
    for lifecycle in [discover(), ClientLifecycleMode::Initialize] {
        let (client, _) = client_via_proxy(&url, ALICE_TOKEN, lifecycle).await;
        assert_eq!(client.list_all_tools().await.unwrap().len(), 5);
        let (err, text) = tool(&client, "query_events", json!({})).await;
        assert!(!err, "{text}");
        assert_eq!(ids(&split(&text).1), [lecture.id.to_string()]);
        // Long results arrive as SSE and are relayed one message per line.
        let (_, text) = tool(&client, "subscribe", json!({})).await;
        let sub: Value = serde_json::from_str(&text).unwrap();
        let (err, _) = tool(
            &client,
            "wait_for_events",
            json!({ "subscription": sub["subscription"], "cursor": sub["cursor"], "timeout_seconds": 0 }),
        )
        .await;
        assert!(!err);
        client.cancel().await.unwrap();
    }
}

/// Raw lines in, raw lines out.
async fn exchange(url: &str, token: &str, line: Value) -> Value {
    let (mut ours, theirs) = tokio::io::duplex(1 << 16);
    let (pr, pw) = tokio::io::split(theirs);
    let running = tokio::spawn(proxy(Target::new(url, token).unwrap(), pr, pw));
    ours.write_all(format!("{line}\n").as_bytes()).await.unwrap();
    let (r, mut w) = tokio::io::split(ours);
    let mut lines = BufReader::new(r).lines();
    let reply = lines.next_line().await.unwrap().unwrap();
    // Closing stdin ends the proxy cleanly.
    w.shutdown().await.unwrap();
    drop(w);
    drop(lines);
    running.await.unwrap().ok();
    serde_json::from_str(&reply).unwrap()
}

fn tools_list() -> Value {
    json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/list", "params": { "_meta": {
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
    } } })
}

#[tokio::test]
async fn failures_come_back_as_errors_for_the_request() {
    let fake = Fake::new();
    let url = daemon(&fake).await;
    let reply = exchange(&url, "tok-wrong", tools_list()).await;
    assert_eq!(reply["id"], 5);
    assert!(reply["error"]["message"].as_str().unwrap().contains("rejected this agent's token"));

    // Nothing listening: the request fails, the proxy keeps running.
    let free = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = format!("http://{}/mcp", free.local_addr().unwrap());
    drop(free);
    let reply = exchange(&dead, ALICE_TOKEN, tools_list()).await;
    assert_eq!(reply["id"], 5);
    assert!(reply["error"]["message"].as_str().unwrap().contains("cannot reach the Pulse daemon"));

    let reply = exchange(&url, ALICE_TOKEN, json!("not a message")).await;
    assert!(reply["error"].is_object(), "{reply}");

    // The launcher's probe tells the three cases apart before serving anything.
    check(&Target::new(&url, ALICE_TOKEN).unwrap()).await.unwrap();
    let wrong = check(&Target::new(&url, "tok-wrong").unwrap()).await.unwrap_err();
    assert!(matches!(wrong, CheckError::Rejected), "{wrong}");
    let down = check(&Target::new(&dead, ALICE_TOKEN).unwrap()).await.unwrap_err();
    assert!(matches!(down, CheckError::Unreachable { .. }), "{down}");
}

#[cfg(feature = "sep3415")]
#[tokio::test]
async fn cancelling_a_stream_through_the_proxy_releases_it() {
    use rmcp::model::{ClientRequest, CustomRequest};
    use rmcp::service::PeerRequestOptions;

    let fake = Fake::new();
    let url = daemon(&fake).await;
    let (client, mut notes) = client_via_proxy(&url, ALICE_TOKEN, discover()).await;
    let request = ClientRequest::CustomRequest(CustomRequest::new(
        "events/stream",
        Some(json!({ "name": "fs.file.created" })),
    ));
    let stream = client
        .peer()
        .send_cancellable_request(request, PeerRequestOptions::no_options())
        .await
        .unwrap();
    assert_eq!(next_custom(&mut notes).await.method, "notifications/events/active");
    let file = fake.file("/home/me/University/a.pdf");
    let event = next_custom(&mut notes).await;
    assert_eq!(event.params.unwrap()["eventId"], file.id.to_string());
    // stdio cancellation becomes an HTTP disconnect, which ends the stream on the daemon.
    stream.cancel(None).await.unwrap();
    fake.all_closed(&alice()).await;
    let _ = client.cancel().await;
}
