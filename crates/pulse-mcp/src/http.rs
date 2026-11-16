//! Streamable HTTP at `/mcp`: bearer authentication and a per-agent request budget in front of
//! rmcp's service.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::{HeaderValue, StatusCode, header};
use http_body_util::BodyExt;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

use crate::server::{Authenticated, Caller, PulseMcp};
use crate::sessions::{CALLER, Sessions};
use crate::{AgentIdentity, Backend, Limits, McpConfig, Shared};

/// The largest request body accepted (rmcp's default is 4 MiB). MCP requests from an agent are
/// tool arguments and handshakes, a few KiB at most; a filter is stored with its subscription
/// and matched against every event, so its size is bounded here, once. Larger: HTTP 413.
const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024;

/// The MCP endpoint, for the daemon to mount with `Router::nest("/mcp", router(..))`.
///
/// Every request needs `Authorization: Bearer <agent token>`; a missing or unknown token is
/// answered `401` before rmcp sees the request. The identity resolved from the token is the
/// only one tools act as, per request: there is no session-level identity to switch.
///
/// An authenticated request then needs a slot in the agent's request budget
/// ([`Limits::requests_per_agent`], [`Limits::requests_total`]); it holds it until its response
/// body ends or is dropped, so an open SSE stream counts. Without one it is answered `429` (or
/// `503` when every agent together is at the total) at once, with a fixed body and
/// `Connection: close`, so a refused client does not keep the socket.
///
/// rmcp then checks `Host` against [`McpConfig::allowed_hosts`] (403 otherwise) and rejects any
/// `Origin` not in [`McpConfig::allowed_origins`]. If the daemon applies its own Host/Origin
/// layer to the whole listener, pass it the same lists; the checks then agree.
///
/// SSE comment keepalives are off: SEP-3415 streams carry their own heartbeat, and a long
/// `wait_for_events` sends progress notifications instead.
pub fn router<B: Backend>(backend: Arc<B>, config: McpConfig) -> axum::Router {
    let rmcp_config = StreamableHttpServerConfig::default()
        .with_sse_keep_alive(None)
        .with_allowed_hosts(config.allowed_hosts.clone())
        .with_allowed_origins(config.allowed_origins.clone())
        .enforce_origin_validation()
        .with_max_request_body_bytes(MAX_REQUEST_BODY_BYTES)
        .with_cancellation_token(config.shutdown.child_token());
    let sessions = Sessions::new(config.limits.sessions_per_agent);
    let shared = Shared::new(backend, config);
    let for_factory = shared.clone();
    let service = StreamableHttpService::new(
        move || Ok(PulseMcp::new(for_factory.clone(), Caller::Http)),
        sessions.into(),
        rmcp_config,
    );
    let gate = Arc::new(Gate { shared, requests: Requests::default() });
    axum::Router::new()
        .fallback_service(service)
        .layer(axum::middleware::from_fn_with_state(gate, authenticate::<B>))
}

struct Gate<B> {
    shared: Arc<Shared<B>>,
    requests: Requests,
}

async fn authenticate<B: Backend>(
    State(gate): State<Arc<Gate<B>>>,
    mut request: Request,
    next: Next,
) -> Response {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let Some(token) = token else {
        return unauthorized("missing bearer token");
    };
    let Some(agent) = gate.shared.backend.resolve_agent(token).await else {
        return unauthorized("unknown token");
    };
    let slot = match gate.requests.take(&agent, &gate.shared.config.limits) {
        Ok(slot) => slot,
        Err(refusal) => return refusal.into_response(),
    };
    request.extensions_mut().insert(Authenticated(agent.clone()));
    let response = CALLER.scope(agent, next.run(request)).await;
    // The slot is released when the body is, so a stream holds it for as long as it is open.
    response.map(|body| {
        Body::new(body.map_frame(move |frame| {
            let _held = &slot;
            frame
        }))
    })
}

fn unauthorized(message: &'static str) -> Response {
    let mut response = (StatusCode::UNAUTHORIZED, message).into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer realm=\"pulse\""));
    response
}

/// HTTP requests in flight on `/mcp`, per agent and in total.
#[derive(Default)]
struct Requests(Arc<Mutex<InFlight>>);

#[derive(Default)]
struct InFlight {
    total: usize,
    /// Only agents with a request in flight; an entry is removed when it reaches zero.
    per_agent: HashMap<AgentIdentity, usize>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Why a request was refused. Each answer is a fixed response: refusing costs the same however
/// often a client tries.
#[derive(Debug)]
enum Refusal {
    Agent,
    Total,
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        // JSON-RPC errors with a null id (the body was not read), so a client that shows the
        // error says why. -32000 is the JSON-RPC range for implementation-defined server errors.
        let (status, body) = match self {
            Refusal::Agent => (
                StatusCode::TOO_MANY_REQUESTS,
                r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32000,"message":"Too many requests in flight for this agent: finish or close some (long waits and open streams count) and retry"}}"#,
            ),
            Refusal::Total => (
                StatusCode::SERVICE_UNAVAILABLE,
                r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32000,"message":"Pulse has too many MCP requests in flight: retry shortly"}}"#,
            ),
        };
        let mut response = (status, body).into_response();
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
        headers.insert(header::CONNECTION, HeaderValue::from_static("close"));
        response
    }
}

impl Requests {
    fn take(&self, agent: &AgentIdentity, limits: &Limits) -> Result<Slot, Refusal> {
        let mut state = lock(&self.0);
        let mine = state.per_agent.get(agent).copied().unwrap_or(0);
        if mine >= limits.requests_per_agent {
            return Err(Refusal::Agent);
        }
        if state.total >= limits.requests_total {
            return Err(Refusal::Total);
        }
        state.total += 1;
        *state.per_agent.entry(agent.clone()).or_default() += 1;
        Ok(Slot { requests: self.0.clone(), agent: agent.clone() })
    }
}

/// One request's place in the budget, released on drop.
struct Slot {
    requests: Arc<Mutex<InFlight>>,
    agent: AgentIdentity,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut state = lock(&self.requests);
        state.total = state.total.saturating_sub(1);
        if let Some(n) = state.per_agent.get_mut(&self.agent) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                state.per_agent.remove(&self.agent);
            }
        }
    }
}
