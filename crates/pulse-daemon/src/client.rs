//! A typed JSON-RPC client for the daemon, used by the CLI and by tests:
//! `client.call::<method::RunsApprove>(RunIdParams { id }).await`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use pulse_contracts::rpc::{
    JsonRpcNotification, JsonRpcResponse, Notification, RequestId, RpcError, RpcMethod,
    decode_response, request,
};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_util::sync::CancellationToken;

use crate::home::PulseHome;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// No daemon is running for this Pulse home (or `daemon.json` is stale).
    #[error("Pulse is not running ({0}). Start it with `pulse start`.")]
    NotRunning(String),
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

impl Client {
    /// Connects to the daemon of `home` using its `daemon.json` and token.
    pub async fn connect(home: &Path) -> Result<Client, ClientError> {
        let home = PulseHome::new(home);
        let info = home
            .read_daemon_info()
            .map_err(|e| ClientError::NotRunning(format!("cannot read daemon.json: {e}")))?
            .ok_or_else(|| ClientError::NotRunning(format!("no daemon.json in {}", home.root().display())))?;
        let token = home
            .read_token()
            .map_err(|e| ClientError::NotRunning(format!("cannot read the token: {e}")))?;
        Client::connect_to(info.port, &token).await
    }

    pub async fn connect_to(port: u16, token: &str) -> Result<Client, ClientError> {
        let url = format!("ws://127.0.0.1:{port}/ws");
        let mut req = url
            .as_str()
            .into_client_request()
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        let auth = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| ClientError::Refused("the token is not a valid header value".into()))?;
        req.headers_mut().insert("authorization", auth);
        let (ws, _) = tokio_tungstenite::connect_async(req).await.map_err(|e| match e {
            tokio_tungstenite::tungstenite::Error::Io(io) => {
                ClientError::NotRunning(format!("nothing is listening on port {port}: {io}"))
            }
            tokio_tungstenite::tungstenite::Error::Http(resp) => {
                ClientError::Refused(format!("HTTP {}", resp.status()))
            }
            other => ClientError::Protocol(other.to_string()),
        })?;
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
