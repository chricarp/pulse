//! The one local HTTP listener (bound to 127.0.0.1): `/ws` (JSON-RPC over WebSocket),
//! `POST /ingest`, `GET /auth/proof`, `POST /auth/login`, `/mcp`, and the web UI at `/`.
//!
//! Every request passes [`guard`] first:
//! - `Host` must be `127.0.0.1:<port>` or `localhost:<port>` (DNS rebinding: a page on
//!   `evil.example` that resolves to 127.0.0.1 sends its own name);
//! - an `Origin`, when present (browsers), must be this daemon's own origin, and on `/mcp` no
//!   `Origin` is accepted at all (MCP clients are not browsers; the UI never calls `/mcp`);
//! - `/ws` and `/ingest` need the daemon token (`Authorization: Bearer`, or for a browser
//!   WebSocket the subprotocol `pulse.token.<token>`); the static UI, the proof and the login
//!   exchange need none. An agent token is not the daemon token, so it is refused on all of them. A
//!   browser-style upgrade (offering the `pulse` subprotocol) with a missing or wrong token is
//!   not refused with a 401, which a browser cannot read, but upgraded and closed at once with
//!   4401 (see [`pulse_contracts::auth`]).
//! - `/mcp` needs an agent token, checked by the MCP router's own bearer layer through
//!   `Services::mcp_agent` before any MCP handling (see [`mcp`]); the daemon token is not an
//!   agent token and is refused there.
//!
//! Handlers decode, call one service method, and map the error. Nothing here logs a request's
//! headers or query, so tokens never reach the log.

mod mcp;
mod rpc;
mod ui;

pub(crate) use mcp::agent_open_files;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use pulse_contracts::auth::{
    DaemonProof, ProofQuery, RESUME_CODE_PREFIX, UiLoginRequest, UiLoginResponse, valid_proof_nonce,
};
use pulse_contracts::rpc::{IngestParams, RpcError};
use pulse_sources::ingest::{IngestError, IngestLimits};
use serde::Deserialize;

use crate::auth::constant_time_eq;
use crate::error::ServiceError;
use crate::services::Services;

pub use pulse_contracts::auth::{WS_PROTOCOL, WS_TOKEN_PROTOCOL_PREFIX};

#[derive(Clone, Debug)]
pub(crate) struct App {
    pub services: Services,
}

/// The whole listener. Call once the port is known: the Host checks need it.
pub(crate) fn router(services: Services) -> Router {
    let app = App { services: services.clone() };
    let limits = IngestLimits::default();
    let port = services.shared.port();
    let mcp_routes = Router::new().nest("/mcp", mcp::router(services, allowed_hosts(port)));
    Router::new()
        .route("/ws", get(rpc::upgrade))
        .route("/ingest", post(ingest).layer(DefaultBodyLimit::max(limits.max_body_bytes + 1)))
        .route("/auth/login", post(login))
        .route("/auth/proof", get(proof))
        .fallback(ui::serve)
        .with_state(app.clone())
        .merge(mcp_routes)
        .layer(middleware::from_fn_with_state(app, guard))
}

/// The `Host` values the listener accepts (the guard, and rmcp's own check on `/mcp`).
fn allowed_hosts(port: u16) -> Vec<String> {
    vec![format!("127.0.0.1:{port}"), format!("localhost:{port}")]
}

fn allowed_host(host: &str, port: u16) -> bool {
    allowed_hosts(port).iter().any(|h| h == host)
}

fn allowed_origin(origin: &str, port: u16) -> bool {
    origin == format!("http://127.0.0.1:{port}") || origin == format!("http://localhost:{port}")
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ").map(str::trim)
}

fn ws_protocols(headers: &HeaderMap) -> impl Iterator<Item = &str> {
    headers
        .get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
}

fn ws_protocol_token(headers: &HeaderMap) -> Option<&str> {
    ws_protocols(headers).find_map(|p| p.strip_prefix(WS_TOKEN_PROTOCOL_PREFIX))
}

fn deny(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": { "message": message } }))).into_response()
}

async fn guard(State(app): State<App>, mut req: Request, next: Next) -> Response {
    let shared = &app.services.shared;
    let port = shared.port();
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok());
    if !host.is_some_and(|h| allowed_host(h, port)) {
        return deny(StatusCode::FORBIDDEN, "unexpected Host header");
    }
    let path = req.uri().path();
    let mcp = path == "/mcp" || path.starts_with("/mcp/");
    if let Some(origin) = req.headers().get(header::ORIGIN) {
        if mcp {
            return deny(StatusCode::FORBIDDEN, "/mcp does not accept browser requests");
        }
        if !origin.to_str().is_ok_and(|o| allowed_origin(o, port)) {
            return deny(StatusCode::FORBIDDEN, "cross-origin requests are not allowed");
        }
    }
    if path == "/ws" || path == "/ingest" {
        let token = bearer(req.headers()).or_else(|| ws_protocol_token(req.headers()));
        if !token.is_some_and(|t| constant_time_eq(t.as_bytes(), shared.secret.as_bytes())) {
            let browser = path == "/ws" && ws_protocols(req.headers()).any(|p| p == WS_PROTOCOL);
            if !browser {
                return deny(StatusCode::UNAUTHORIZED, "missing or wrong daemon token");
            }
            // The upgrade handler closes it with 4401 before reading anything.
            req.extensions_mut().insert(rpc::Unauthenticated);
        }
    }
    next.run(req).await
}

fn status_of(e: &ServiceError) -> StatusCode {
    match e {
        ServiceError::NotFound { .. } => StatusCode::NOT_FOUND,
        ServiceError::Invalid(_)
        | ServiceError::Ambiguous { .. }
        | ServiceError::ConfigInvalid(_) => StatusCode::BAD_REQUEST,
        ServiceError::Conflict(_) | ServiceError::Stale { .. } => StatusCode::CONFLICT,
        ServiceError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        ServiceError::NotImplemented(_) => StatusCode::NOT_IMPLEMENTED,
        ServiceError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

impl IntoResponse for ServiceError {
    fn into_response(self) -> Response {
        let status = status_of(&self);
        (status, Json(serde_json::json!({ "error": RpcError::from(self) }))).into_response()
    }
}

#[derive(Debug, Deserialize)]
struct IngestQuery {
    /// The external source name: events become `ext.<source>.<type>`.
    source: String,
}

/// `POST /ingest?source=<name>`: a JSON object, a JSON array, or JSON lines.
async fn ingest(
    State(app): State<App>,
    Query(q): Query<IngestQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let content_type = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok());
    let events = match pulse_sources::ingest::parse(content_type, &body, &IngestLimits::default()) {
        Ok(events) => events,
        Err(e) => {
            let err = match e {
                IngestError::ContentType(_) => {
                    return deny(StatusCode::UNSUPPORTED_MEDIA_TYPE, &e.to_string());
                }
                IngestError::TooLarge { .. } | IngestError::TooMany { .. } => {
                    return deny(StatusCode::PAYLOAD_TOO_LARGE, &e.to_string());
                }
                IngestError::Parse { .. } | IngestError::Empty | IngestError::Invalid(_) => {
                    ServiceError::Invalid(e.to_string())
                }
            };
            return err.into_response();
        }
    };
    match app.services.events_ingest(IngestParams { source: q.source, events }).await {
        Ok(result) => Json(result).into_response(),
        Err(e) => e.into_response(),
    }
}

fn no_store(mut res: Response) -> Response {
    res.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    res
}

/// `GET /auth/proof?nonce=<hex>[&agent=<name>|&resume=true]`: shows that this daemon knows the
/// credential the client is about to send (see `pulse_contracts::auth`). It needs no credential,
/// and its answer is a MAC under a key that is used for nothing else.
async fn proof(State(app): State<App>, Query(q): Query<ProofQuery>) -> Response {
    let shared = &app.services.shared;
    if !valid_proof_nonce(&q.nonce) {
        return deny(StatusCode::BAD_REQUEST, "the nonce must be 32 to 128 lowercase hex digits");
    }
    let credential = match (&q.agent, q.resume) {
        (Some(_), true) => return deny(StatusCode::BAD_REQUEST, "ask for one credential"),
        (Some(agent), false) => crate::auth::agent_token(&shared.secret, agent),
        (None, true) => crate::auth::resume_code(&shared.secret, &shared.instance),
        (None, false) => shared.secret.clone(),
    };
    let proof = crate::auth::proof(&credential, shared.port(), &shared.instance, &q.nonce);
    no_store(Json(DaemonProof { instance: shared.instance.clone(), proof }).into_response())
}

/// `POST /auth/login`: a login code from `pulse open`, or a resume code from a tab that was
/// connected to this daemon instance, for the daemon token.
async fn login(State(app): State<App>, Json(req): Json<UiLoginRequest>) -> Response {
    let shared = &app.services.shared;
    let accepted = if req.code.starts_with(RESUME_CODE_PREFIX) {
        crate::auth::verify_resume_code(&shared.secret, &shared.instance, &req.code)
    } else {
        shared.login_codes.redeem(&shared.secret, &req.code, shared.clock.now())
    };
    if accepted {
        no_store(Json(UiLoginResponse { token: shared.secret.clone() }).into_response())
    } else {
        deny(
            StatusCode::UNAUTHORIZED,
            "the login code is invalid, expired or already used; run `pulse open` again",
        )
    }
}
