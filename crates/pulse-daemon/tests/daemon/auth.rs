//! The listener's access rules: token, Host, Origin, agent tokens, login codes.

use pulse_contracts::rpc::{Empty, method};
use pulse_daemon::client::Client;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::support::*;

/// Sends a raw HTTP/1.1 request and returns the status code and body.
async fn http(port: u16, head: &str, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let req =
        format!("{head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    s.write_all(req.as_bytes()).await.unwrap();
    // Read the head, and the body unless the connection was upgraded (then it stays open).
    let mut out = String::new();
    within("an HTTP response", async {
        let mut buf = [0u8; 4096];
        loop {
            let n = s.read(&mut buf).await.unwrap();
            out.push_str(&String::from_utf8_lossy(&buf[..n]));
            if n == 0 || (out.starts_with("HTTP/1.1 101") && out.contains("\r\n\r\n")) {
                break;
            }
        }
    })
    .await;
    let status = out.split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let body = out.split("\r\n\r\n").nth(1).unwrap_or("").to_owned();
    (status, body)
}

/// Opens a WebSocket the way the web UI does and returns the close code the daemon sends first.
async fn browser_close_code(port: u16, protocols: &str) -> Option<u16> {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let mut req = format!("ws://127.0.0.1:{port}/ws").into_client_request().unwrap();
    let h = req.headers_mut();
    h.insert("origin", format!("http://127.0.0.1:{port}").parse().unwrap());
    h.insert("sec-websocket-protocol", protocols.parse().unwrap());
    let (mut ws, resp) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(
        resp.headers().get("sec-websocket-protocol").and_then(|v| v.to_str().ok()),
        Some(pulse_contracts::auth::WS_PROTOCOL),
        "the daemon selects the app protocol and never echoes the token"
    );
    within("a close frame", async {
        while let Some(msg) = ws.next().await {
            match msg {
                Ok(Message::Close(frame)) => return frame.map(|f| u16::from(f.code)),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
        None
    })
    .await
}

const UPGRADE: &str = "Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tokens_hosts_and_origins() {
    let mut t = TestDaemon::start(COMMAND_AGENT).await;
    let port = t.port();
    let token = t.token();
    let host = format!("Host: 127.0.0.1:{port}");
    let agent_token = {
        let c = t.client().await;
        c.call::<method::AgentsMcpCredentials>(pulse_contracts::rpc::NameParams {
            name: "sh".into(),
        })
        .await
        .unwrap()
        .token
    };
    assert!(agent_token.starts_with("pa.sh."));

    let cases: Vec<(String, u16)> = vec![
        (format!("GET /ws HTTP/1.1\r\n{host}\r\n{UPGRADE}"), 401),
        (format!("GET /ws HTTP/1.1\r\n{host}\r\nAuthorization: Bearer nope\r\n{UPGRADE}"), 401),
        (
            format!(
                "GET /ws HTTP/1.1\r\n{host}\r\nAuthorization: Bearer {agent_token}\r\n{UPGRADE}"
            ),
            401,
        ),
        (
            format!(
                "GET /ws HTTP/1.1\r\nHost: evil.example:{port}\r\nAuthorization: Bearer {token}\r\n{UPGRADE}"
            ),
            403,
        ),
        (
            format!(
                "GET /ws HTTP/1.1\r\nHost: 127.0.0.1:1\r\nAuthorization: Bearer {token}\r\n{UPGRADE}"
            ),
            403,
        ),
        (
            format!(
                "GET /ws HTTP/1.1\r\n{host}\r\nOrigin: http://evil.example\r\nAuthorization: Bearer {token}\r\n{UPGRADE}"
            ),
            403,
        ),
        (format!("GET /ws HTTP/1.1\r\n{host}\r\nAuthorization: Bearer {token}\r\n{UPGRADE}"), 101),
        (
            format!(
                "GET /ws HTTP/1.1\r\n{host}\r\nOrigin: http://localhost:{port}\r\nSec-WebSocket-Protocol: pulse, pulse.token.{token}\r\n{UPGRADE}"
            ),
            101,
        ),
        (
            format!("POST /ingest?source=ci HTTP/1.1\r\n{host}\r\nContent-Type: application/json"),
            401,
        ),
        (
            format!(
                "POST /ingest?source=ci HTTP/1.1\r\n{host}\r\nAuthorization: Bearer {agent_token}\r\nContent-Type: application/json"
            ),
            401,
        ),
        // `/mcp` is the real MCP router: the token is checked before rmcp sees the request, and
        // only an agent token of a configured agent passes. The daemon token is not one.
        (format!("GET /mcp HTTP/1.1\r\n{host}"), 401),
        (
            format!(
                "POST /mcp HTTP/1.1\r\n{host}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream"
            ),
            401,
        ),
        (format!("GET /mcp HTTP/1.1\r\n{host}\r\nAuthorization: Bearer {token}"), 401),
        (format!("GET /mcp/x HTTP/1.1\r\n{host}\r\nAuthorization: Bearer {token}"), 401),
        (format!("GET /mcp HTTP/1.1\r\n{host}\r\nAuthorization: Bearer pa.ghost.00"), 401),
        (format!("GET /mcp HTTP/1.1\r\n{host}\r\nAuthorization: Bearer {agent_token}x"), 401),
        // A forged tag for an agent that exists.
        (
            format!(
                "GET /mcp HTTP/1.1\r\n{host}\r\nAuthorization: Bearer pa.sh.{}",
                "0".repeat(64)
            ),
            401,
        ),
        // No browser may call /mcp, not even a page served by this daemon.
        (
            format!(
                "GET /mcp HTTP/1.1\r\n{host}\r\nOrigin: http://evil.example\r\nAuthorization: Bearer {agent_token}"
            ),
            403,
        ),
        (
            format!(
                "GET /mcp HTTP/1.1\r\n{host}\r\nOrigin: http://127.0.0.1:{port}\r\nAuthorization: Bearer {agent_token}"
            ),
            403,
        ),
        (
            format!(
                "GET /mcp HTTP/1.1\r\nHost: evil.example:{port}\r\nAuthorization: Bearer {agent_token}"
            ),
            403,
        ),
        (format!("GET / HTTP/1.1\r\n{host}"), 200),
        (format!("GET /rules/ci HTTP/1.1\r\n{host}"), 200),
        ("GET / HTTP/1.1\r\nHost: attacker.example".to_owned(), 403),
    ];
    for (head, want) in cases {
        let (status, _) = http(port, &head, "").await;
        assert_eq!(status, want, "{head}");
    }
    // The agent token gets past auth on /mcp (rmcp then answers the bare GET as it likes).
    let (status, _) = http(
        port,
        &format!("GET /mcp HTTP/1.1\r\n{host}\r\nAuthorization: Bearer {agent_token}"),
        "",
    )
    .await;
    assert!(![401, 403, 501].contains(&status), "{status}");
    // Ingest with the token gets past auth (what it does next is the parser's business).
    let (status, _) = http(port, &format!("POST /ingest?source=ci HTTP/1.1\r\n{host}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json"), "{\"type\":\"x\"}").await;
    assert!(status != 401 && status != 403, "{status}");

    // A login code from `pulse open` works once, for the daemon token.
    let code = pulse_daemon::auth::login_code(
        &token,
        pulse_contracts::Timestamp::from_system_time(std::time::SystemTime::now()),
    );
    let login = format!(
        "POST /auth/login HTTP/1.1\r\n{host}\r\nOrigin: http://127.0.0.1:{port}\r\nContent-Type: application/json"
    );
    let body = format!("{{\"code\":\"{code}\"}}");
    let (status, got) = http(port, &login, &body).await;
    assert_eq!(status, 200);
    assert!(got.contains(&token));
    assert_eq!(http(port, &login, &body).await.0, 401, "single use");

    // A browser cannot read the status of a refused upgrade (it sees 1006, like a daemon that
    // is down), so a browser-style upgrade with a wrong token is accepted and closed with 4401.
    for offered in ["pulse, pulse.token.nope", "pulse"] {
        let (status, _) = http(
            port,
            &format!("GET /ws HTTP/1.1\r\n{host}\r\nOrigin: http://127.0.0.1:{port}\r\nSec-WebSocket-Protocol: {offered}\r\n{UPGRADE}"),
            "",
        )
        .await;
        assert_eq!(status, 101, "{offered}");
        assert_eq!(browser_close_code(port, offered).await, Some(4401), "{offered}");
    }

    // The typed client authenticates with the token file.
    let client = Client::connect(&t.home).await.unwrap();
    client.call::<method::SystemStatusGet>(Empty {}).await.unwrap();
    t.stop().await;
}
