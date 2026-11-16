//! `pulse mcp --agent <name>`: MCP over stdio, for clients that launch a server process.
//!
//! The process is a thin proxy to the daemon's `/mcp`, not a second server. Each JSON-RPC
//! message read from stdin is POSTed to the daemon with the agent's token; each response
//! (JSON, or an SSE stream for long requests) is written back to stdout, one message per line.
//! So there is one MCP implementation, one place that resolves identity (the daemon, from the
//! token), and the stdio process holds only the agent token, never the daemon token.
//!
//! What the proxy itself does:
//! - Sets the per-request headers streamable HTTP requires (`MCP-Protocol-Version`,
//!   `Mcp-Method`, `Mcp-Name`) from the message, and carries `Mcp-Session-Id` for clients on
//!   pre-2026 protocol revisions.
//! - Maps `notifications/cancelled` to aborting that request's HTTP connection, which is how
//!   streamable HTTP cancels (rmcp cancels the handler when the client disconnects).
//! - Answers a request with a JSON-RPC error when the daemon cannot be reached, and keeps
//!   running: the daemon may be started later.
//! - Ends when stdin closes, aborting whatever is still in flight.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine;
use bytes::{Buf, Bytes};
use futures_util::StreamExt;
use http::{HeaderMap, HeaderValue, Method, Request, StatusCode, Uri, header};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};
use tokio_util::codec::{FramedRead, LinesCodec, LinesCodecError};

/// One JSON-RPC message, at most. Longer lines are rejected, not buffered.
const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const PROTOCOL_VERSION_META: &str = "io.modelcontextprotocol/protocolVersion";

/// Where the daemon's MCP endpoint is and which agent token to present.
#[derive(Clone)]
pub struct Target {
    authority: String,
    path: String,
    token: String,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("authority", &self.authority)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("invalid MCP URL {0:?}: expected http://127.0.0.1:<port>/mcp")]
    Url(String),
    #[error("reading stdin: {0}")]
    Stdin(std::io::Error),
    #[error("writing stdout: {0}")]
    Stdout(std::io::Error),
}

impl Target {
    /// `url` is the daemon's MCP URL, as `agents.mcp_credentials` returns it
    /// (`http://127.0.0.1:7477/mcp`). Only plain `http` is accepted: the daemon listens on
    /// loopback and the proxy carries no TLS.
    pub fn new(url: &str, token: impl Into<String>) -> Result<Self, ProxyError> {
        let bad = || ProxyError::Url(url.to_owned());
        let uri: Uri = url.parse().map_err(|_| bad())?;
        if uri.scheme_str() != Some("http") {
            return Err(bad());
        }
        let authority = uri.authority().ok_or_else(bad)?;
        if authority.port_u16().is_none() {
            return Err(bad());
        }
        Ok(Target {
            authority: authority.to_string(),
            path: uri.path_and_query().map_or("/", |p| p.as_str()).to_owned(),
            token: token.into(),
        })
    }
}

/// What the proxy learned from responses: the session id and protocol version a pre-2026
/// client negotiated with `initialize`. 2026-07-28 clients carry the version in every request.
#[derive(Default)]
struct Negotiated {
    session: Option<HeaderValue>,
    version: Option<String>,
}

struct Proxy {
    target: Target,
    negotiated: Mutex<Negotiated>,
    out: mpsc::Sender<String>,
}

/// Runs the proxy until `input` closes. Messages are newline-delimited JSON, as MCP's stdio
/// transport defines.
pub async fn proxy<R, W>(target: Target, input: R, output: W) -> Result<(), ProxyError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (out, out_rx) = mpsc::channel(64);
    let writer = tokio::spawn(write_lines(output, out_rx));
    let proxy = Arc::new(Proxy { target, negotiated: Mutex::default(), out });
    let mut lines = FramedRead::new(input, LinesCodec::new_with_max_length(MAX_MESSAGE_BYTES));
    let mut in_flight = JoinSet::new();
    // Request id (as JSON text) → its task, so a cancellation can abort the right one.
    let mut by_id: HashMap<String, AbortHandle> = HashMap::new();
    let mut ids: HashMap<tokio::task::Id, String> = HashMap::new();
    let result = loop {
        tokio::select! {
            line = lines.next() => match line {
                None => break Ok(()),
                Some(Err(LinesCodecError::Io(e))) => break Err(ProxyError::Stdin(e)),
                Some(Err(LinesCodecError::MaxLineLengthExceeded)) => {
                    proxy.reply(&error(Value::Null, -32700, "message too long")).await;
                }
                Some(Ok(line)) if line.trim().is_empty() => {}
                Some(Ok(line)) => {
                    let Ok(message) = serde_json::from_str::<Value>(&line) else {
                        proxy.reply(&error(Value::Null, -32700, "parse error")).await;
                        continue;
                    };
                    if message.get("method").and_then(Value::as_str) == Some("notifications/cancelled") {
                        let target = message.pointer("/params/requestId").map(Value::to_string);
                        if let Some(handle) = target.and_then(|k| by_id.remove(&k)) {
                            handle.abort();
                        }
                        continue;
                    }
                    let id = message.get("method").and(message.get("id")).map(Value::to_string);
                    let handle = in_flight.spawn(proxy.clone().forward(message, line));
                    if let Some(id) = id {
                        ids.insert(handle.id(), id.clone());
                        by_id.insert(id, handle);
                    }
                }
            },
            Some(done) = in_flight.join_next_with_id() => {
                let task = match &done { Ok((task, ())) => *task, Err(e) => e.id() };
                if let Some(id) = ids.remove(&task) {
                    by_id.remove(&id);
                }
            }
        }
    };
    in_flight.abort_all();
    drop(proxy);
    match writer.await {
        Ok(Ok(())) => result,
        Ok(Err(e)) => Err(ProxyError::Stdout(e)),
        Err(e) => Err(ProxyError::Stdout(std::io::Error::other(e))),
    }
}

async fn write_lines<W: AsyncWrite + Unpin>(
    mut output: W,
    mut lines: mpsc::Receiver<String>,
) -> std::io::Result<()> {
    while let Some(mut line) = lines.recv().await {
        line.push('\n');
        output.write_all(line.as_bytes()).await?;
        output.flush().await?;
    }
    Ok(())
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

impl Proxy {
    async fn reply(&self, message: &Value) {
        // The writer only stops when stdout fails, and then nothing can be told anyway.
        let _ = self.out.send(message.to_string()).await;
    }

    async fn forward(self: Arc<Self>, message: Value, raw: String) {
        let id = message.get("method").and(message.get("id")).cloned();
        let failure = match self.exchange(&message, raw).await {
            Ok(()) => return,
            Err(e) => e,
        };
        if let Some(id) = id {
            self.reply(&error(id, -32603, failure)).await;
        }
    }

    /// POSTs one message and relays what comes back. `Err` is a message for the client.
    async fn exchange(&self, message: &Value, raw: String) -> Result<(), String> {
        let request = self.request(message, raw)?;
        let unreachable = |e: &dyn std::fmt::Display| {
            format!(
                "cannot reach the Pulse daemon at http://{}{}: {e}. Is `pulse start` running?",
                self.target.authority, self.target.path
            )
        };
        let stream = tokio::net::TcpStream::connect(&self.target.authority)
            .await
            .map_err(|e| unreachable(&e))?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| unreachable(&e))?;
        // Driving the connection ends when the response is done or this task is aborted, which
        // closes the socket: the daemon sees the disconnect and cancels the request.
        let _connection = AbortOnDrop(tokio::spawn(connection).abort_handle());
        let response = sender.send_request(request).await.map_err(|e| unreachable(&e))?;
        let (parts, mut body) = response.into_parts();
        self.learn(&parts.headers, None);
        if parts.status == StatusCode::UNAUTHORIZED {
            return Err("the Pulse daemon rejected this agent's token (401). Check the agent name, or fetch a fresh token with `pulse agents mcp-credentials <agent>`.".to_owned());
        }
        if parts.status == StatusCode::ACCEPTED {
            return Ok(());
        }
        let content_type = parts.headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok());
        if content_type.is_some_and(|c| c.starts_with("text/event-stream")) {
            let mut sse = SseParser::default();
            while let Some(frame) = body.frame().await {
                let frame =
                    frame.map_err(|e| format!("the Pulse daemon closed the stream: {e}"))?;
                let Ok(mut data) = frame.into_data() else { continue };
                while data.has_remaining() {
                    let chunk = data.chunk().to_vec();
                    data.advance(chunk.len());
                    for event in sse.push(&chunk)? {
                        self.relay(&event).await;
                    }
                }
            }
            return Ok(());
        }
        let bytes = body
            .collect()
            .await
            .map_err(|e| format!("reading the Pulse daemon's response: {e}"))?
            .to_bytes();
        if bytes.is_empty() {
            return if parts.status.is_success() {
                Ok(())
            } else {
                Err(format!("the Pulse daemon answered HTTP {}", parts.status))
            };
        }
        match std::str::from_utf8(&bytes) {
            Ok(text) if serde_json::from_str::<Value>(text).is_ok() => {
                self.relay(text).await;
                Ok(())
            }
            _ => Err(format!(
                "the Pulse daemon answered HTTP {}: {}",
                parts.status,
                String::from_utf8_lossy(&bytes[..bytes.len().min(200)])
            )),
        }
    }

    async fn relay(&self, text: &str) {
        let Ok(message) = serde_json::from_str::<Value>(text) else { return };
        self.learn(&HeaderMap::new(), Some(&message));
        // Re-serialized so the message is guaranteed to be on one line.
        self.reply(&message).await;
    }

    fn learn(&self, headers: &HeaderMap, message: Option<&Value>) {
        let mut n = self.negotiated.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(session) = headers.get("mcp-session-id") {
            n.session = Some(session.clone());
        }
        if let Some(v) = message.and_then(|m| m.pointer("/result/protocolVersion")) {
            n.version = v.as_str().map(str::to_owned);
        }
    }

    fn request(&self, message: &Value, raw: String) -> Result<Request<Full<Bytes>>, String> {
        let method = message.get("method").and_then(Value::as_str);
        let version = message
            .pointer("/params/_meta")
            .and_then(|m| m.get(PROTOCOL_VERSION_META))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let negotiated = self.negotiated.lock().unwrap_or_else(PoisonError::into_inner);
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(&self.target.path)
            .header(header::HOST, &self.target.authority)
            .header(header::AUTHORIZATION, format!("Bearer {}", self.target.token))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream");
        if method != Some("initialize")
            && let Some(v) = version.or_else(|| negotiated.version.clone())
        {
            builder = builder.header("mcp-protocol-version", v);
        }
        if let Some(session) = &negotiated.session {
            builder = builder.header("mcp-session-id", session);
        }
        if let Some(method) = method {
            builder = builder.header("mcp-method", header_value(method));
            if let Some(name) = routing_name(method, message.get("params")) {
                builder = builder.header("mcp-name", header_value(name));
            }
        }
        builder.body(Full::new(Bytes::from(raw))).map_err(|e| format!("building the request: {e}"))
    }
}

struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The `Mcp-Name` routing header's source field per method (SEP-2243, and SEP-3415 for
/// `events/*`).
fn routing_name<'a>(method: &str, params: Option<&'a Value>) -> Option<&'a str> {
    let key = match method {
        "tools/call" | "prompts/get" => "name",
        "resources/read" | "resources/subscribe" | "resources/unsubscribe" => "uri",
        "tasks/get" | "tasks/update" | "tasks/cancel" => "taskId",
        m if m.starts_with("events/") && m != "events/list" => "name",
        _ => return None,
    };
    params?.get(key)?.as_str()
}

/// A header value, in SEP-2243's Base64 sentinel form when it is not plain printable ASCII.
pub(crate) fn header_value(s: &str) -> String {
    let plain = !s.is_empty()
        && s.bytes().all(|b| (0x20..0x7f).contains(&b))
        && !s.starts_with([' ', '\t'])
        && !s.ends_with([' ', '\t'])
        && !(s.starts_with("=?base64?") && s.ends_with("?="));
    if plain || s.is_empty() {
        s.to_owned()
    } else {
        format!("=?base64?{}?=", base64::engine::general_purpose::STANDARD.encode(s))
    }
}

/// Server-sent events, reduced to the `data` of each event.
#[derive(Default)]
struct SseParser {
    line: Vec<u8>,
    data: Option<String>,
}

impl SseParser {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, String> {
        let mut events = Vec::new();
        for &b in bytes {
            if b != b'\n' {
                self.line.push(b);
                if self.line.len() > MAX_MESSAGE_BYTES {
                    return Err("the Pulse daemon sent an oversized event".to_owned());
                }
                continue;
            }
            if self.line.last() == Some(&b'\r') {
                self.line.pop();
            }
            let line = String::from_utf8_lossy(&std::mem::take(&mut self.line)).into_owned();
            if line.is_empty() {
                events.extend(self.data.take());
            } else if let Some(value) = line.strip_prefix("data:") {
                let value = value.strip_prefix(' ').unwrap_or(value);
                match &mut self.data {
                    Some(d) => {
                        d.push('\n');
                        d.push_str(value);
                    }
                    None => self.data = Some(value.to_owned()),
                }
            }
            // `id:`, `event:`, `retry:` and `:` comments carry nothing the client needs.
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_frames_reassemble_across_chunks() {
        let mut p = SseParser::default();
        let mut got = p.push(b"id: 1\r\nretry: 3000\r\n\r\ndata: {\"a\"").unwrap();
        got.extend(p.push(b":1}\n\n: comment\n\ndata: x\ndata: y\n\n").unwrap());
        assert_eq!(got, vec!["{\"a\":1}".to_owned(), "x\ny".to_owned()]);
    }

    #[test]
    fn routing_headers_follow_the_message() {
        let p = json!({ "name": "query_events", "uri": "file:///x", "taskId": "t1" });
        let cases = [
            ("tools/call", Some("query_events")),
            ("events/poll", Some("query_events")),
            ("events/list", None),
            ("resources/read", Some("file:///x")),
            ("tasks/get", Some("t1")),
            ("tools/list", None),
        ];
        for (method, want) in cases {
            assert_eq!(routing_name(method, Some(&p)), want, "{method}");
        }
        assert_eq!(header_value("fs.file.created"), "fs.file.created");
        assert_eq!(header_value("é"), "=?base64?w6k=?=");
    }

    #[test]
    fn only_plain_http_targets() {
        assert!(Target::new("http://127.0.0.1:7477/mcp", "t").is_ok());
        for bad in ["https://127.0.0.1:7477/mcp", "127.0.0.1:7477", "http://127.0.0.1/mcp"] {
            assert!(Target::new(bad, "t").is_err(), "{bad}");
        }
    }
}
