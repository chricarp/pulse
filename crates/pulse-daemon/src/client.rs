//! A typed JSON-RPC client for the daemon, used by the CLI and by tests:
//! `client.call::<method::RunsApprove>(RunIdParams { id }).await`.
//!
//! Before it sends the token, the client asks whatever listens on the port to prove that it
//! knows the token (`GET /auth/proof`, see `pulse_contracts::auth`), on the same HTTP/1.1
//! connection that then carries the WebSocket upgrade, so the answer and the token go to the
//! same peer. A wrong answer is [`ClientError::NotPulse`] and the token is not sent.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Empty, Limited};
use hyper::body::Bytes;
use hyper::client::conn::http1::SendRequest;
use hyper::{Request, StatusCode, header};
use hyper_util::rt::TokioIo;
use pulse_contracts::auth::{DaemonProof, PROOF_PATH, WS_PATH};
use pulse_contracts::rpc::{
    JsonRpcNotification, JsonRpcResponse, Notification, RequestId, RpcError, RpcMethod,
    decode_response, request,
};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_util::sync::CancellationToken;

use crate::home::PulseHome;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// No daemon is running for this Pulse home (or `daemon.json` is stale).
    #[error("Pulse is not running ({0}). Start it with `pulse start`.")]
    NotRunning(String),
    /// Something answers on the port but cannot prove that it knows the token, so the token
    /// was not sent.
    #[error(
        "something else is listening on Pulse's port {port}: it could not prove that it knows this Pulse home's token ({why}), so the token was not sent to it. If the Pulse daemon is stopped, find and stop the program on that port, or set another `[daemon] port` in pulse.toml. A Pulse daemon older than this client cannot prove itself either: stop it with the binary that started it."
    )]
    NotPulse { port: u16, why: String },
    #[error("the daemon refused the connection: {0}")]
    Refused(String),
    #[error("{}", .0.message)]
    Rpc(RpcError),
    #[error("the connection to the daemon closed")]
    Closed,
    #[error("unexpected reply from the daemon: {0}")]
    Protocol(String),
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<JsonRpcResponse>>>>;

/// One WebSocket connection to the daemon.
#[derive(Debug)]
pub struct Client {
    out: mpsc::Sender<Message>,
    pending: Pending,
    next_id: AtomicI64,
    notifications: Mutex<Option<mpsc::Receiver<Notification>>>,
    wants_notifications: Arc<AtomicBool>,
    closed: CancellationToken,
    port: u16,
}

/// Asks the peer behind `sender` for a proof of `token` and checks it. Nothing secret is sent.
async fn require_proof(
    sender: &mut SendRequest<Empty<Bytes>>,
    port: u16,
    token: &str,
) -> Result<(), ClientError> {
    let not_pulse = |why: &str| ClientError::NotPulse { port, why: why.to_owned() };
    let nonce = crate::auth::random_hex::<32>()
        .map_err(|e| ClientError::Protocol(format!("cannot draw a nonce: {e}")))?;
    let req = Request::get(format!("{PROOF_PATH}?nonce={nonce}"))
        .header(header::HOST, format!("127.0.0.1:{port}"))
        .body(Empty::new())
        .map_err(|e| ClientError::Protocol(e.to_string()))?;
    let resp = sender
        .send_request(req)
        .await
        .map_err(|e| ClientError::NotRunning(format!("port {port}: {e}")))?;
    if resp.status() != StatusCode::OK {
        return Err(not_pulse(&format!("it answered HTTP {}", resp.status())));
    }
    let body = Limited::new(resp.into_body(), 4096)
        .collect()
        .await
        .map_err(|_| not_pulse("its answer was unreadable or too long"))?
        .to_bytes();
    let answer: DaemonProof =
        serde_json::from_slice(&body).map_err(|_| not_pulse("its answer is not a proof"))?;
    if crate::auth::proof_matches(token, port, &nonce, &answer) {
        Ok(())
    } else {
        Err(not_pulse("its proof is wrong"))
    }
}

impl Client {
    /// Connects to the daemon of `home` using its `daemon.json` and token.
    pub async fn connect(home: &Path) -> Result<Client, ClientError> {
        let home = PulseHome::new(home);
        let info = home
            .read_daemon_info()
            .map_err(|e| ClientError::NotRunning(format!("cannot read daemon.json: {e}")))?
            .ok_or_else(|| {
                ClientError::NotRunning(format!("no daemon.json in {}", home.root().display()))
            })?;
        let token = home
            .read_token()
            .map_err(|e| ClientError::NotRunning(format!("cannot read the token: {e}")))?;
        Client::connect_to(info.port, &token).await
    }

    /// Checks that the program on `port` is the daemon that holds `token`, without sending the
    /// token (`pulse open` does this before it opens a browser there).
    pub async fn verify(port: u16, token: &str) -> Result<(), ClientError> {
        let mut sender = Self::http(port).await?.0;
        require_proof(&mut sender, port, token).await
    }

    async fn http(
        port: u16,
    ) -> Result<(SendRequest<Empty<Bytes>>, tokio::task::JoinHandle<()>), ClientError> {
        let stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.map_err(|e| {
            ClientError::NotRunning(format!("nothing is listening on port {port}: {e}"))
        })?;
        let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| ClientError::NotRunning(format!("port {port}: {e}")))?;
        // Ends when the connection does, or hands the socket over on upgrade.
        let driver = tokio::spawn(async move {
            let _ = conn.with_upgrades().await;
        });
        Ok((sender, driver))
    }

    pub async fn connect_to(port: u16, token: &str) -> Result<Client, ClientError> {
        let (mut sender, _driver) = Self::http(port).await?;
        require_proof(&mut sender, port, token).await?;
        // The connection takes the next request only once it has finished the last one.
        sender.ready().await.map_err(|e| {
            ClientError::NotRunning(format!("port {port} closed the connection: {e}"))
        })?;
        let key = generate_key();
        let req = Request::get(WS_PATH)
            .header(header::HOST, format!("127.0.0.1:{port}"))
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "websocket")
            .header(header::SEC_WEBSOCKET_VERSION, "13")
            .header(header::SEC_WEBSOCKET_KEY, &key)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Empty::new())
            .map_err(|_| ClientError::Refused("the token is not a valid header value".into()))?;
        let resp = sender
            .send_request(req)
            .await
            .map_err(|e| ClientError::Protocol(format!("the upgrade failed: {e}")))?;
        if resp.status() != StatusCode::SWITCHING_PROTOCOLS {
            return Err(ClientError::Refused(format!("HTTP {}", resp.status())));
        }
        let accept = resp.headers().get(header::SEC_WEBSOCKET_ACCEPT).map(|v| v.as_bytes());
        if accept != Some(derive_accept_key(key.as_bytes()).as_bytes()) {
            return Err(ClientError::Protocol("a bad WebSocket accept key".into()));
        }
        let upgraded = hyper::upgrade::on(resp)
            .await
            .map_err(|e| ClientError::Protocol(format!("the upgrade failed: {e}")))?;
        let ws = WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Client, None).await;
        let (mut sink, mut stream) = ws.split();
        let (out, mut outgoing) = mpsc::channel::<Message>(64);
        let (notify_tx, notify_rx) = mpsc::channel(256);
        let pending: Pending = Arc::default();
        let wants = Arc::new(AtomicBool::new(false));
        let closed = CancellationToken::new();

        let done = closed.clone();
        tokio::spawn(async move {
            while let Some(msg) = outgoing.recv().await {
                if sink.send(msg).await.is_err() {
                    break;
                }
            }
            done.cancel();
        });
        let (p, w, done) = (pending.clone(), wants.clone(), closed.clone());
        tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                let Message::Text(text) = msg else {
                    if matches!(msg, Message::Close(_)) {
                        break;
                    }
                    continue;
                };
                let value: serde_json::Value = match serde_json::from_str(text.as_str()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if value.get("method").is_some() {
                    if w.load(Ordering::SeqCst) {
                        if let Ok(n) = serde_json::from_value::<JsonRpcNotification>(value) {
                            if notify_tx.send(n.notification).await.is_err() {
                                break;
                            }
                        }
                    }
                    continue;
                }
                if let Ok(resp) = serde_json::from_value::<JsonRpcResponse>(value) {
                    if let Some(RequestId::Number(id)) = resp.id {
                        let waiter = p.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                        if let Some(waiter) = waiter {
                            let _ = waiter.send(resp);
                        }
                    }
                }
            }
            // Fail every call still waiting.
            p.lock().unwrap_or_else(|e| e.into_inner()).clear();
            done.cancel();
        });
        Ok(Client {
            out,
            pending,
            next_id: AtomicI64::new(1),
            notifications: Mutex::new(Some(notify_rx)),
            wants_notifications: wants,
            closed,
            port,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Calls one method.
    pub async fn call<M: RpcMethod>(&self, params: M::Params) -> Result<M::Result, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let frame = request::<M>(RequestId::Number(id), &params)
            .and_then(|r| serde_json::to_string(&r))
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
        if self.out.send(Message::Text(frame.into())).await.is_err() {
            return Err(ClientError::Closed);
        }
        let resp = rx.await.map_err(|_| ClientError::Closed)?;
        decode_response::<M>(resp).map_err(ClientError::Rpc)
    }

    /// Server notifications (subscriptions and `state.changed`). Until this is called they are
    /// discarded, so a client that never reads them cannot stall its own calls.
    pub fn notifications(&self) -> Option<mpsc::Receiver<Notification>> {
        self.wants_notifications.store(true, Ordering::SeqCst);
        self.notifications.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// Resolves when the connection has closed.
    pub async fn closed(&self) {
        self.closed.cancelled().await;
    }
}
