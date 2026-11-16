//! `/mcp` on the real daemon, through a real MCP client: rmcp in process, the stdio proxy
//! `pulse mcp` uses, the daemon's listener and guard, and the mounted `pulse_mcp` router over
//! the daemon's services. What the MCP crate tests with a fake backend is not repeated; these
//! are the things that only hold (or break) when the two are joined: scoping, the catalog,
//! cursors, wakeups, revocation and id order.

use pulse_contracts::rpc::*;
use pulse_contracts::{Event, NewEvent};
use pulse_mcp::stdio::{Target, proxy};
use rmcp::model::{
    CallToolRequestParams, ClientRequest, CustomRequest, ProtocolVersion, ServerResult,
};
use rmcp::service::{ClientLifecycleMode, RunningService};
use rmcp::{RoleClient, serve_client_with_lifecycle};
use serde_json::{Value, json};

use crate::support::*;

type Mcp = RunningService<RoleClient, ()>;

/// An MCP client (protocol 2026-07-28) connected as `agent`, through the stdio proxy.
async fn connect(t: &TestDaemon, agent: &str) -> Mcp {
    let creds = t
        .client()
        .await
        .call::<method::AgentsMcpCredentials>(NameParams { name: agent.into() })
        .await
        .unwrap();
    let (client_io, proxy_io) = tokio::io::duplex(1 << 20);
    let (pr, pw) = tokio::io::split(proxy_io);
    tokio::spawn(proxy(Target::new(&creds.url, creds.token).unwrap(), pr, pw));
    let (cr, cw) = tokio::io::split(client_io);
    let lifecycle =
        ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] };
    within("the MCP client to connect", serve_client_with_lifecycle((), (cr, cw), lifecycle))
        .await
        .unwrap()
}

/// Calls a tool: `(is_error, text)`.
async fn tool(mcp: &Mcp, name: &'static str, args: Value) -> (bool, String) {
    let mut p = CallToolRequestParams::new(name);
    p.arguments = args.as_object().cloned();
    let r = within(name, mcp.call_tool(p)).await.unwrap();
    let text = r.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect();
    (r.is_error.unwrap_or(false), text)
}

/// The summary line and the fenced events of a tool result.
fn split(text: &str) -> (Value, Vec<Value>) {
    let (summary, rest) = text.split_once('\n').unwrap_or((text, ""));
    let summary: Value = serde_json::from_str(summary).unwrap();
    let Some(start) = rest.find("\nPULSE-UNTRUSTED-DATA-") else {
        return (summary, Vec::new());
    };
    let block = &rest[start + 1..];
    let (fence, body) = block.split_once('\n').unwrap();
    let end = body.find(fence).unwrap();
    (summary, serde_json::from_str(&body[..end]).unwrap())
}

fn subjects(events: &[Value]) -> Vec<&str> {
    events.iter().map(|e| e["subject"].as_str().unwrap_or("")).collect()
}

/// A SEP-3415 request: its result, or its error code.
async fn custom(mcp: &Mcp, method: &'static str, params: Value) -> Result<Value, i32> {
    let request = ClientRequest::CustomRequest(CustomRequest::new(method, Some(params)));
    match within(method, mcp.peer().send_request(request)).await {
        Ok(ServerResult::CustomResult(r)) => Ok(r.0),
        Ok(other) => panic!("unexpected result {other:?}"),
        Err(rmcp::ServiceError::McpError(e)) => Err(e.code.0),
        Err(e) => panic!("{e}"),
    }
}

/// Pushes events from the fake source and waits until all of them are committed.
async fn push_all(t: &TestDaemon, events: Vec<NewEvent>) -> Vec<Event> {
    let client = t.client().await;
    let mut notes = client.notifications().unwrap();
    client.call::<method::EventsSubscribe>(EventsSubscribeParams::default()).await.unwrap();
    let n = events.len();
    for e in events {
        t.sources.push("fake", e).await;
    }
    let mut got = Vec::new();
    within("the events to be committed", async {
        while got.len() < n {
            if let Notification::EventsBatch(b) = notes.recv().await.unwrap() {
                got.extend(b.batch.events);
            }
        }
    })
    .await;
    got
}

fn ext(type_: &str, data: Value) -> IngestEvent {
    IngestEvent {
        type_: type_.into(),
        subject: None,
        time: None,
        data,
        dedupe_key: None,
        causation_run: None,
    }
}

const AGENTS: &str = r#"
[daemon]
mcp_events_extension = true

[[agent]]
name = "x"
adapter = "mcp"
allow = ["fs.*:/tmp/x/**"]

[[agent]]
name = "all"
adapter = "mcp"
allow = ["*"]
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scoped_agent_sees_nothing_outside_its_scope() {
    let mut t = TestDaemon::start(&format!("{FAKE_SOURCE}{AGENTS}")).await;
    let hidden_secret = file_created("fake", "/tmp/y/secret-plans.txt");
    let committed = push_all(
        &t,
        vec![
            file_created("fake", "/tmp/x/a.txt"),
            hidden_secret,
            file_created("fake", "/tmp/x/../y/escape.txt"),
            file_created("fake", "/tmp/xy/near.txt"),
            file_created("fake", "/tmp/x/b-secret.txt"),
        ],
    )
    .await;
    let client = t.client().await;
    client
        .call::<method::EventsIngest>(IngestParams {
            source: "ci".into(),
            events: vec![ext("secret", json!({ "path": "/tmp/x/c.txt", "note": "secret" }))],
        })
        .await
        .unwrap();
    let id_of = |subject: &str| {
        committed.iter().find(|e| e.subject.as_deref() == Some(subject)).unwrap().id.to_string()
    };

    let x = connect(&t, "x").await;
    // The catalog is scoped: path-carrying fs types only.
    let (err, text) = tool(&x, "list_event_types", json!({})).await;
    assert!(!err, "{text}");
    let catalog: Value = serde_json::from_str(&text).unwrap();
    let mut types: Vec<&str> = catalog["event_types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["type"].as_str().unwrap())
        .collect();
    types.sort_unstable();
    assert_eq!(
        types,
        ["fs.burst", "fs.file.created", "fs.file.deleted", "fs.file.modified", "fs.file.moved"]
    );

    // History, every way of asking: only the two events under /tmp/x, and no cursor that
    // points at anything else.
    let visible = ["/tmp/x/a.txt", "/tmp/x/b-secret.txt"];
    let probes = [
        json!({}),
        json!({ "order": "oldest_first" }),
        json!({ "text": "secret" }),
        json!({ "text": "plans" }),
        json!({ "subject": "/tmp/y/**" }),
        json!({ "path": "/tmp/y" }),
        json!({ "path": "/tmp" }),
        json!({ "types": ["ext.*"] }),
        json!({ "types": ["fs.*"], "limit": 1 }),
        // A guessed cursor (the hidden event's id) only positions the scan.
        json!({ "cursor": id_of("/tmp/y/secret-plans.txt"), "order": "oldest_first" }),
    ];
    for probe in probes {
        let mut seen = Vec::new();
        let mut args = probe.clone();
        loop {
            let (err, text) = tool(&x, "query_events", args.clone()).await;
            assert!(!err, "{probe}: {text}");
            let (summary, events) = split(&text);
            assert!(
                !text.contains("secret-plans")
                    && !text.contains("escape")
                    && !text.contains("near"),
                "{probe}: {text}"
            );
            seen.extend(subjects(&events).into_iter().map(str::to_owned));
            match summary["next_cursor"].as_str() {
                Some(c) => {
                    assert!(
                        visible.iter().any(|s| id_of(s) == c),
                        "{probe}: a cursor names an event the agent cannot see"
                    );
                    args["cursor"] = json!(c);
                }
                None => break,
            }
        }
        for s in &seen {
            assert!(visible.contains(&s.as_str()), "{probe}: saw {s}");
        }
    }

    // SEP-3415: the same scope.
    let list = custom(&x, "events/list", json!({})).await.unwrap();
    let mut names: Vec<&str> =
        list["events"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    assert_eq!(names, types);
    let forbidden = custom(&x, "events/poll", json!({ "name": "ext.ci.secret", "cursor": null }))
        .await
        .unwrap_err();
    assert_eq!(forbidden, -32024, "a type outside the catalog is Forbidden");
    let first = committed[0].id.to_string();
    for args in [json!({}), json!({ "text": "secret" }), json!({ "path": "/tmp/y" })] {
        let polled = custom(
            &x,
            "events/poll",
            json!({ "name": "fs.file.created", "cursor": first, "arguments": args }),
        )
        .await
        .unwrap();
        let text = polled.to_string();
        assert!(!text.contains("secret-plans") && !text.contains("escape"), "{args}: {text}");
        let cursor = polled["cursor"].as_str().unwrap();
        assert!(cursor == first || visible.iter().any(|s| id_of(s) == cursor), "{text}");
    }

    // An agent allowed everything sees all of it, through the same server.
    let all = connect(&t, "all").await;
    let (_, text) = tool(&all, "query_events", json!({ "text": "secret" })).await;
    let (_, events) = split(&text);
    assert_eq!(events.len(), 3, "{text}");
    let _ = x.cancel().await;
    let _ = all.cancel().await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscriptions_wake_on_visible_events_and_end_when_the_agent_goes() {
    let config = format!("{FAKE_SOURCE}{AGENTS}");
    let mut t = TestDaemon::start(&config).await;
    let x = connect(&t, "x").await;
    let (err, text) = tool(&x, "subscribe", json!({})).await;
    assert!(!err, "{text}");
    let sub: Value = serde_json::from_str(&text).unwrap();
    let wait = |cursor: Value| json!({ "subscription": sub["subscription"], "cursor": cursor, "timeout_seconds": 30 });

    // A hidden event followed by a visible one: the wait returns the visible one only.
    let waiting = {
        let args = wait(sub["cursor"].clone());
        let x = x.peer().clone();
        tokio::spawn(async move {
            let mut p = CallToolRequestParams::new("wait_for_events");
            p.arguments = args.as_object().cloned();
            x.call_tool(p).await
        })
    };
    push_all(
        &t,
        vec![file_created("fake", "/tmp/y/hidden.txt"), file_created("fake", "/tmp/x/shown.txt")],
    )
    .await;
    let result = within("the wait to return", waiting).await.unwrap().unwrap();
    let text: String =
        result.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect();
    let (summary, events) = split(&text);
    assert_eq!(subjects(&events), ["/tmp/x/shown.txt"], "{text}");

    // The agent is removed from the config: a waiting call is told its subscription ended, and
    // its token stops working.
    let waiting = {
        let args = wait(summary["cursor"].clone());
        let x = x.peer().clone();
        tokio::spawn(async move {
            let mut p = CallToolRequestParams::new("wait_for_events");
            p.arguments = args.as_object().cloned();
            x.call_tool(p).await
        })
    };
    let client = t.client().await;
    client.call::<method::AgentsDelete>(NameParams { name: "x".into() }).await.unwrap();
    let ended = within("the wait to end", waiting).await.unwrap().unwrap();
    assert_eq!(ended.is_error, Some(true));
    let text: String =
        ended.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect();
    assert!(text.contains("ended by Pulse"), "{text}");
    let gone = within("a call after removal", x.list_all_tools()).await;
    assert!(gone.is_err(), "the removed agent's token is refused: {gone:?}");
    let _ = x.cancel().await;
    t.stop().await;
}

/// Readers follow the log by cursor and never look below one again, so every event must be
/// committed above every event before it: across batches and paths (sources, ingest, replay),
/// and across a restart whose clock reads earlier than the log. Live batches arrive in commit
/// order, so their ids must increase; and the store refuses an out-of-order commit, so a
/// violation would also fail one of the calls.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_ids_increase_in_commit_order_across_paths_and_restarts() {
    let mut t = TestDaemon::start(&format!("{FAKE_SOURCE}{AGENTS}")).await;
    let client = t.client().await;
    let first = client
        .call::<method::EventsIngest>(IngestParams {
            source: "ci".into(),
            events: vec![ext("first", json!({}))],
        })
        .await
        .unwrap()
        .accepted[0];
    let mut notes = client.notifications().unwrap();
    client.call::<method::EventsSubscribe>(EventsSubscribeParams::default()).await.unwrap();
    let ingest = |i: usize| {
        let client = &client;
        async move {
            let events = (0..20).map(|j| ext("tick", json!({ "i": i, "j": j }))).collect();
            client
                .call::<method::EventsIngest>(IngestParams { source: "ci".into(), events })
                .await
                .unwrap()
                .accepted
                .len()
        }
    };
    let replay = async {
        client.call::<method::EventsReplay>(ReplayParams { id: first, rules: None }).await.unwrap();
        1
    };
    let pushes = async {
        for i in 0..20 {
            t.sources.push("fake", file_created("fake", &format!("/tmp/x/{i}"))).await;
        }
        20
    };
    let (ingested, replayed, pushed) =
        tokio::join!(futures_util::future::join_all((0..6).map(ingest)), replay, pushes);
    let total = ingested.iter().sum::<usize>() + replayed + pushed;
    let mut live = Vec::new();
    within("every event live", async {
        while live.len() < total {
            if let Notification::EventsBatch(b) = notes.recv().await.unwrap() {
                assert_eq!(b.batch.dropped, 0, "the live view summarized; lower the counts");
                live.extend(b.batch.events.into_iter().map(|e| e.id));
            }
        }
    })
    .await;
    assert!(live.windows(2).all(|w| w[0] < w[1]), "live batches out of id order: {live:?}");
    drop(client);
    t.stop().await;

    // The clock now reads a day before the log.
    let clock =
        pulse_daemon::Clock::tokio(i64::try_from(first.timestamp_ms()).unwrap() - 24 * 3600 * 1000);
    t.boot(Setup { clock, ..Setup::default() }).await;
    let client = t.client().await;
    let after = client
        .call::<method::EventsIngest>(IngestParams {
            source: "ci".into(),
            events: vec![ext("late", json!({})), ext("later", json!({}))],
        })
        .await
        .unwrap()
        .accepted;
    let newest_before = *live.last().unwrap();
    assert!(after[0] > newest_before && after[1] > after[0], "{after:?} after {newest_before}");
    t.stop().await;
}
