//! `pulse mcp --agent <name>` end to end: the real binary as a child process, a real daemon on
//! port 0 in a temp `PULSE_HOME`, and an MCP client (rmcp) on the child's stdin and stdout. If
//! anything but MCP messages reached stdout, the client would fail to parse it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use pulse_contracts::rpc::{IngestEvent, IngestParams};
use pulse_daemon::wiring::Wiring;
use pulse_daemon::{Daemon, DaemonOptions};
use rmcp::model::{CallToolRequestParams, ProtocolVersion};
use rmcp::serve_client_with_lifecycle;
use rmcp::service::ClientLifecycleMode;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

/// Only reached when a test fails.
const CEILING: Duration = Duration::from_secs(30);

async fn within<T>(what: &str, f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(CEILING, f).await.unwrap_or_else(|_| panic!("timed out: {what}"))
}

const CONFIG: &str = r#"
[[agent]]
name = "x"
adapter = "mcp"
allow = ["ext.ci.*"]
"#;

fn bridge(home: &Path, agent: &str, token: Option<&str>) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pulse"));
    cmd.arg("--home")
        .arg(home)
        .args(["mcp", "--agent", agent])
        .env_remove("PULSE_HOME")
        .env_remove("PULSE_MCP_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(t) = token {
        cmd.env("PULSE_MCP_TOKEN", t);
    }
    cmd.spawn().unwrap()
}

/// Waits for a bridge that should refuse to start: its exit code, stdout and stderr.
async fn refused(mut child: Child) -> (Option<i32>, String, String) {
    drop(child.stdin.take());
    let (mut out, mut err) = (String::new(), String::new());
    child.stdout.take().unwrap().read_to_string(&mut out).await.unwrap();
    child.stderr.take().unwrap().read_to_string(&mut err).await.unwrap();
    let status = within("the bridge to exit", child.wait()).await.unwrap();
    (status.code(), out, err)
}

async fn tool(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &'static str,
) -> Result<String, rmcp::ServiceError> {
    let p = CallToolRequestParams::new(name);
    let r = within(name, client.call_tool(p)).await?;
    Ok(r.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pulse_mcp_bridges_stdio_to_the_daemon_as_one_agent() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("pulse-home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("pulse.toml"), CONFIG).unwrap();

    // No daemon: exit 3, nothing on stdout.
    let (code, out, err) = refused(bridge(&home, "x", None)).await;
    assert_eq!((code, out.as_str()), (Some(3), ""), "{err}");
    assert!(err.contains("not running"), "{err}");

    let opts = DaemonOptions {
        port: Some(0),
        watch_config: false,
        retention_every: None,
        ..DaemonOptions::new(&home, dir.path())
    };
    let daemon = Daemon::start(opts, Wiring::real()).await.unwrap();
    let services = daemon.services();
    let ingest = |type_: &str| IngestParams {
        source: "ci".into(),
        events: vec![IngestEvent {
            type_: type_.into(),
            subject: Some(format!("{type_} happened")),
            time: None,
            data: json!({}),
            dedupe_key: None,
            causation_run: None,
        }],
    };
    services.events_ingest(ingest("build")).await.unwrap();

    // An agent the daemon does not know: exit 1. A token for another agent: usage, exit 2.
    let (code, out, err) = refused(bridge(&home, "ghost", None)).await;
    assert_eq!((code, out.as_str()), (Some(1), ""), "{err}");
    assert!(err.contains("ghost"), "{err}");
    let secret = std::fs::read_to_string(home.join("token")).unwrap();
    let token = pulse_daemon::auth::agent_token(secret.trim(), "x");
    let (code, out, err) = refused(bridge(&home, "other", Some(&token))).await;
    assert_eq!((code, out.as_str()), (Some(2), ""), "{err}");

    // Through the daemon token file, and through PULSE_MCP_TOKEN alone.
    for token in [None, Some(token.as_str())] {
        let mut child = bridge(&home, "x", token);
        let io = (child.stdout.take().unwrap(), child.stdin.take().unwrap());
        let lifecycle = ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        };
        let client =
            within("connect", serve_client_with_lifecycle((), io, lifecycle)).await.unwrap();
        let tools = within("tools", client.list_all_tools()).await.unwrap();
        assert_eq!(tools.len(), 5);
        let text = tool(&client, "query_events").await.unwrap();
        assert!(text.contains("ext.ci.build"), "{text}");
        let catalog: Value =
            serde_json::from_str(&tool(&client, "list_event_types").await.unwrap()).unwrap();
        let types: Vec<&str> = catalog["event_types"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["type"].as_str().unwrap())
            .collect();
        assert_eq!(types, ["ext.*"]);
        client.cancel().await.unwrap();
        let status = within("the bridge to exit", child.wait()).await.unwrap();
        assert_eq!(status.code(), Some(0), "stdin closed: a clean exit");
    }

    // The daemon stops while the bridge is connected: each request gets an error naming the
    // problem, and the bridge stays up until its client goes.
    let mut child = bridge(&home, "x", None);
    let io = (child.stdout.take().unwrap(), child.stdin.take().unwrap());
    let lifecycle =
        ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] };
    let client = within("connect", serve_client_with_lifecycle((), io, lifecycle)).await.unwrap();
    tool(&client, "query_events").await.unwrap();
    daemon.shutdown().await;
    let err = tool(&client, "query_events").await.unwrap_err();
    assert!(err.to_string().contains("cannot reach the Pulse daemon"), "{err}");
    assert!(child.try_wait().unwrap().is_none(), "the bridge keeps running");
    client.cancel().await.unwrap();
    let status = within("the bridge to exit", child.wait()).await.unwrap();
    assert_eq!(status.code(), Some(0));
    let mut err = String::new();
    child.stderr.take().unwrap().read_to_string(&mut err).await.unwrap();
    assert!(err.is_empty(), "nothing to report while serving: {err}");
}
