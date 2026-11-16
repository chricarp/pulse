//! JSON-RPC 2.0 over WebSocket at `/ws`.
//!
//! Per connection: a reader that runs up to [`MAX_IN_FLIGHT`] requests at once (responses may
//! arrive out of order, as JSON-RPC allows), and a writer that owns the socket and drains one
//! bounded queue of frames. Request handlers wait for room in that queue, which only slows the
//! client's own requests; subscriptions never wait (see [`crate::hub`]). On shutdown the writer
//! sends what is queued, then closes.
//!
//! Batch requests (JSON arrays) are not supported and get an `invalid_request` error.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::Extension;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use pulse_contracts::auth::WS_CLOSE_UNAUTHORIZED;
use pulse_contracts::rpc::*;
use pulse_contracts::status::{Project, SystemStatus};
use pulse_contracts::{Run, RunSummary, SubscriptionId};
use serde::Serialize;
use tokio::sync::{Semaphore, broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use super::{App, WS_PROTOCOL};
use crate::hub::Sent;
use crate::services::Services;

const OUT_QUEUE: usize = 64;
const MAX_IN_FLIGHT: usize = 16;
const MAX_SUBSCRIPTIONS: usize = 64;
const MAX_MESSAGE: usize = 4 * 1024 * 1024;

/// Set by the guard on a browser upgrade without a valid token.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Unauthenticated;

pub(crate) async fn upgrade(
    State(app): State<App>,
    unauthenticated: Option<Extension<Unauthenticated>>,
    ws: WebSocketUpgrade,
) -> Response {
    let ws = ws.protocols([WS_PROTOCOL]).max_message_size(MAX_MESSAGE);
    if unauthenticated.is_some() {
        return ws.on_upgrade(|mut socket| async move {
            let frame = CloseFrame {
                code: WS_CLOSE_UNAUTHORIZED,
                reason: "missing or wrong daemon token; open Pulse with `pulse open`".into(),
            };
            let _ = socket.send(Message::Close(Some(frame))).await;
        });
    }
    ws.on_upgrade(move |socket| serve(app.services, socket))
}

/// One client connection: its outgoing queue and its subscriptions.
#[derive(Debug)]
pub(crate) struct Connection {
    out: mpsc::Sender<String>,
    subs: Mutex<HashMap<SubscriptionId, CancellationToken>>,
    /// Subscriptions whose subscribe call has not been answered yet: they deliver nothing until
    /// the response is queued, so the client knows the id before the first notification.
    gates: Mutex<HashMap<SubscriptionId, CancellationToken>>,
    /// Cancelled when the connection ends (or the daemon shuts down).
    closed: CancellationToken,
    shutdown_after_reply: AtomicBool,
}

#[derive(Serialize)]
struct Frame<'a, T> {
    jsonrpc: &'static str,
    method: &'static str,
    params: &'a T,
}

fn frame<T: Serialize>(method: &'static str, params: &T) -> String {
    serde_json::to_string(&Frame { jsonrpc: "2.0", method, params }).unwrap_or_default()
}

impl Connection {
    fn try_send(&self, text: String) -> Sent {
        match self.out.try_send(text) {
            Ok(()) => Sent::Ok,
            Err(mpsc::error::TrySendError::Full(_)) => Sent::Full,
            Err(mpsc::error::TrySendError::Closed(_)) => Sent::Closed,
        }
    }

    fn subs(&self) -> std::sync::MutexGuard<'_, HashMap<SubscriptionId, CancellationToken>> {
        self.subs.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn reserve_subscription(&self) -> Result<CancellationToken, RpcError> {
        if self.subs().len() >= MAX_SUBSCRIPTIONS {
            return Err(RpcError::new(
                ErrorCode::Conflict,
                format!("at most {MAX_SUBSCRIPTIONS} subscriptions per connection"),
            ));
        }
        Ok(self.closed.child_token())
    }

    fn register(&self, id: SubscriptionId, stop: CancellationToken, ready: CancellationToken) {
        self.subs().insert(id, stop);
        self.gates.lock().unwrap_or_else(|p| p.into_inner()).insert(id, ready);
    }

    /// The subscribe call's answer is queued; the subscription may deliver.
    fn open_gate(&self, result: &serde_json::Value) {
        let Some(id) = result.get("subscription") else { return };
        let Ok(id) = serde_json::from_value::<SubscriptionId>(id.clone()) else { return };
        if let Some(ready) = self.gates.lock().unwrap_or_else(|p| p.into_inner()).remove(&id) {
            ready.cancel();
        }
    }

    fn unsubscribe(&self, id: SubscriptionId) -> Result<Empty, RpcError> {
        match self.subs().remove(&id) {
            Some(stop) => {
                stop.cancel();
                Ok(Empty {})
            }
            None => Err(RpcError::not_found("subscription", id)),
        }
    }

    /// A delivery function that enqueues a notification without waiting.
    fn notifier<T: Serialize>(
        self: &Arc<Self>,
        method: &'static str,
    ) -> impl FnMut(&T) -> Sent + Send + 'static {
        let cx = Arc::downgrade(self);
        move |params: &T| match cx.upgrade() {
            Some(cx) => cx.try_send(frame(method, params)),
            None => Sent::Closed,
        }
    }
}

async fn serve(services: Services, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (out, mut queue) = mpsc::channel::<String>(OUT_QUEUE);
    let closed = services.shared.shutdown.child_token();
    let cx = Arc::new(Connection {
        out,
        subs: Mutex::new(HashMap::new()),
        gates: Mutex::new(HashMap::new()),
        closed: closed.clone(),
        shutdown_after_reply: AtomicBool::new(false),
    });

    let writer_closed = closed.clone();
    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                next = queue.recv() => match next {
                    Some(text) => {
                        if sink.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
                _ = writer_closed.cancelled() => {
                    while let Ok(text) = queue.try_recv() {
                        if sink.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
            }
        }
    });

    tokio::spawn(forward_state(services.shared.hub.subscribe_state(), cx.clone()));

    let server = Arc::new(RpcServer { services });
    let in_flight = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    loop {
        let frame = tokio::select! {
            _ = closed.cancelled() => break,
            frame = stream.next() => frame,
        };
        match frame {
            Some(Ok(Message::Text(text))) => {
                let Ok(permit) = in_flight.clone().acquire_owned().await else { break };
                let (server, cx) = (server.clone(), cx.clone());
                tokio::spawn(async move {
                    handle(&server, &cx, text.as_str()).await;
                    drop(permit);
                });
            }
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
            Some(Ok(_)) => {}
        }
    }
    closed.cancel();
    let _ = writer.await;
}

async fn forward_state(mut rx: broadcast::Receiver<Arc<StateChanged>>, cx: Arc<Connection>) {
    loop {
        let state = tokio::select! {
            _ = cx.closed.cancelled() => return,
            r = rx.recv() => r,
        };
        match state {
            Ok(state) => {
                if cx.out.send(frame("state.changed", &*state)).await.is_err() {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

async fn handle(server: &RpcServer, cx: &Arc<Connection>, text: &str) {
    let req: JsonRpcRequest = match serde_json::from_str(text) {
        Ok(r) => r,
        Err(e) => {
            let error = if text.trim_start().starts_with('[') {
                RpcError::new(ErrorCode::InvalidRequest, "batch requests are not supported")
            } else {
                RpcError::new(ErrorCode::ParseError, format!("not a JSON-RPC request: {e}"))
            };
            respond(cx, JsonRpcResponse::err(None, error)).await;
            return;
        }
    };
    let result = match Request::parse(&req.method, req.params) {
        Ok(request) => dispatch(server, cx, request).await,
        Err(e) => Err(e),
    };
    let opened = result.as_ref().ok().cloned();
    if let Some(id) = req.id {
        let response = match result {
            Ok(v) => JsonRpcResponse::ok(Some(id), v),
            Err(e) => JsonRpcResponse::err(Some(id), e),
        };
        respond(cx, response).await;
    }
    if let Some(v) = opened {
        cx.open_gate(&v);
    }
    if cx.shutdown_after_reply.load(Ordering::SeqCst) {
        server.services.shutdown();
    }
}

async fn respond(cx: &Connection, response: JsonRpcResponse) {
    let _ = cx.out.send(serde_json::to_string(&response).unwrap_or_default()).await;
}

struct RpcServer {
    services: Services,
}

type R<T> = Result<T, RpcError>;

fn map<T>(r: crate::error::ServiceResult<T>) -> R<T> {
    r.map_err(RpcError::from)
}

impl RpcHandler for RpcServer {
    type Context = Arc<Connection>;

    async fn events_query(&self, _: &Self::Context, p: EventQuery) -> R<EventPageWithOutcomes> {
        map(self.services.events_query(p).await)
    }
    async fn events_get(&self, _: &Self::Context, p: EventIdParams) -> R<EventDetail> {
        map(self.services.events_get(p.id).await)
    }
    async fn events_subscribe(
        &self,
        cx: &Self::Context,
        p: EventsSubscribeParams,
    ) -> R<Subscribed> {
        let stop = cx.reserve_subscription()?;
        let ready = CancellationToken::new();
        let deliver = cx.notifier::<EventsBatchWithOutcomes>("events.batch");
        let id = map(self.services.events_subscribe(
            p.filter,
            deliver,
            ready.clone(),
            stop.clone(),
            cx.closed.clone(),
        ))?;
        cx.register(id, stop, ready);
        Ok(Subscribed { subscription: id })
    }
    async fn events_unsubscribe(&self, cx: &Self::Context, p: Unsubscribe) -> R<Empty> {
        cx.unsubscribe(p.subscription)
    }
    async fn events_replay(&self, _: &Self::Context, p: ReplayParams) -> R<ReplayResult> {
        map(self.services.events_replay(p).await)
    }
    async fn events_ingest(&self, _: &Self::Context, p: IngestParams) -> R<IngestResult> {
        map(self.services.events_ingest(p).await)
    }
    async fn events_report(&self, _: &Self::Context, p: ReportParams) -> R<ReportResult> {
        map(self.services.events_report(p).await)
    }

    async fn rules_list(&self, _: &Self::Context, p: RulesListParams) -> R<RuleList> {
        map(self.services.rules_list(p.include_archived).await)
    }
    async fn rules_get(&self, _: &Self::Context, p: NameParams) -> R<RuleInfo> {
        map(self.services.rules_get(&p.name).await)
    }
    async fn rules_upsert(&self, _: &Self::Context, p: RuleUpsertParams) -> R<RuleInfo> {
        map(self.services.rules_upsert(p).await)
    }
    async fn rules_delete(&self, _: &Self::Context, p: DeleteParams) -> R<Empty> {
        map(self.services.rules_delete(&p.name, &p.expected).await).map(|()| Empty {})
    }
    async fn rules_set_enabled(&self, _: &Self::Context, p: SetEnabledParams) -> R<RuleInfo> {
        map(self.services.rules_set_enabled(&p.name, p.enabled).await)
    }
    async fn rules_snooze(&self, _: &Self::Context, p: SnoozeParams) -> R<RuleInfo> {
        map(self.services.rules_snooze(&p.name, p.until).await)
    }
    async fn rules_unsnooze(&self, _: &Self::Context, p: NameParams) -> R<RuleInfo> {
        map(self.services.rules_unsnooze(&p.name).await)
    }
    async fn rules_archive(&self, _: &Self::Context, p: NameParams) -> R<RuleInfo> {
        map(self.services.rules_archive(&p.name).await)
    }
    async fn rules_reopen(&self, _: &Self::Context, p: NameParams) -> R<RuleInfo> {
        map(self.services.rules_reopen(&p.name).await)
    }
    async fn rules_test(&self, _: &Self::Context, p: RuleTestParams) -> R<RuleTestResult> {
        map(self.services.rules_test(p).await)
    }
    async fn rules_history(&self, _: &Self::Context, p: RuleHistoryParams) -> R<DeliveryPage> {
        map(self.services.rules_history(p).await)
    }

    async fn agents_list(&self, _: &Self::Context, _: Empty) -> R<AgentList> {
        map(self.services.agents_list().await)
    }
    async fn agents_get(&self, _: &Self::Context, p: NameParams) -> R<AgentInfo> {
        map(self.services.agents_get(&p.name).await)
    }
    async fn agents_upsert(&self, _: &Self::Context, p: AgentUpsertParams) -> R<AgentInfo> {
        map(self.services.agents_upsert(p).await)
    }
    async fn agents_delete(&self, _: &Self::Context, p: DeleteParams) -> R<Empty> {
        map(self.services.agents_delete(&p.name, &p.expected).await).map(|()| Empty {})
    }
    async fn agents_mcp_credentials(&self, _: &Self::Context, p: NameParams) -> R<McpCredentials> {
        map(self.services.agents_mcp_credentials(&p.name).await)
    }

    async fn sources_list(&self, _: &Self::Context, _: Empty) -> R<SourceList> {
        map(self.services.sources_list().await)
    }
    async fn sources_upsert(&self, _: &Self::Context, p: SourceUpsertParams) -> R<SourceInfo> {
        map(self.services.sources_upsert(p).await)
    }
    async fn sources_delete(&self, _: &Self::Context, p: DeleteParams) -> R<Empty> {
        map(self.services.sources_delete(&p.name, &p.expected).await).map(|()| Empty {})
    }
    async fn sources_set_enabled(&self, _: &Self::Context, p: SetEnabledParams) -> R<SourceInfo> {
        map(self.services.sources_set_enabled(&p.name, p.enabled).await)
    }

    async fn runs_list(&self, _: &Self::Context, p: RunsListParams) -> R<RunPage> {
        map(self.services.runs_list(p.filter, p.cursor, p.limit).await)
    }
    async fn runs_get(&self, _: &Self::Context, p: RunIdParams) -> R<Run> {
        map(self.services.runs_get(p.id).await)
    }
    async fn runs_output(&self, _: &Self::Context, p: RunOutputParams) -> R<RunOutputPage> {
        map(self.services.runs_output(p.id, p.after, p.limit).await)
    }
    async fn runs_subscribe(&self, cx: &Self::Context, p: RunsSubscribeParams) -> R<Subscribed> {
        let stop = cx.reserve_subscription()?;
        let ready = CancellationToken::new();
        let deliver = cx.notifier::<RunsChanged>("runs.changed");
        let id = self.services.runs_subscribe(
            p.filter,
            deliver,
            ready.clone(),
            stop.clone(),
            cx.closed.clone(),
        );
        cx.register(id, stop, ready);
        Ok(Subscribed { subscription: id })
    }
    async fn runs_subscribe_output(
        &self,
        cx: &Self::Context,
        p: RunOutputSubscribeParams,
    ) -> R<Subscribed> {
        let stop = cx.reserve_subscription()?;
        let ready = CancellationToken::new();
        let deliver = cx.notifier::<RunsOutput>("runs.output");
        let id = map(self
            .services
            .runs_subscribe_output(
                p.id,
                p.after,
                deliver,
                ready.clone(),
                stop.clone(),
                cx.closed.clone(),
            )
            .await)?;
        cx.register(id, stop, ready);
        Ok(Subscribed { subscription: id })
    }
    async fn runs_unsubscribe(&self, cx: &Self::Context, p: Unsubscribe) -> R<Empty> {
        cx.unsubscribe(p.subscription)
    }
    async fn runs_cancel(&self, _: &Self::Context, p: RunIdParams) -> R<RunSummary> {
        map(self.services.runs_cancel(p.id).await)
    }
    async fn runs_approve(&self, _: &Self::Context, p: RunIdParams) -> R<RunSummary> {
        map(self.services.runs_approve(p.id).await)
    }
    async fn runs_reject(&self, _: &Self::Context, p: RejectParams) -> R<RunSummary> {
        map(self.services.runs_reject(p.id, p.reason).await)
    }
    async fn runs_rerun(&self, _: &Self::Context, p: RunIdParams) -> R<RunSummary> {
        map(self.services.runs_rerun(p.id).await)
    }

    async fn projects_list(&self, _: &Self::Context, _: Empty) -> R<ProjectList> {
        Ok(ProjectList { projects: self.services.projects_list() })
    }
    async fn projects_review(&self, _: &Self::Context, p: ProjectReviewParams) -> R<ProjectReview> {
        map(self.services.projects_review(&p.dir, p.name.as_deref()).await)
    }
    async fn projects_trust(&self, _: &Self::Context, p: ProjectTrustParams) -> R<Project> {
        map(self.services.projects_trust(&p.dir, p.name.as_deref(), &p.hash).await)
    }
    async fn projects_untrust(&self, _: &Self::Context, p: ProjectDirParams) -> R<Project> {
        map(self.services.projects_untrust(&p.dir).await)
    }

    async fn system_status(&self, _: &Self::Context, _: Empty) -> R<SystemStatus> {
        map(self.services.status().await)
    }
    async fn system_pause(&self, _: &Self::Context, _: Empty) -> R<SystemStatus> {
        map(self.services.pause().await)
    }
    async fn system_resume(&self, _: &Self::Context, _: Empty) -> R<SystemStatus> {
        map(self.services.resume().await)
    }
    async fn system_shutdown(&self, cx: &Self::Context, _: Empty) -> R<Empty> {
        // Shut down once the reply is queued, so the caller learns that it worked.
        cx.shutdown_after_reply.store(true, Ordering::SeqCst);
        Ok(Empty {})
    }
    async fn system_event_types(&self, _: &Self::Context, _: Empty) -> R<EventTypeList> {
        Ok(EventTypeList { types: self.services.event_types() })
    }
    async fn system_config_errors(&self, _: &Self::Context, _: Empty) -> R<ConfigErrorList> {
        Ok(self.services.config_report())
    }
}
