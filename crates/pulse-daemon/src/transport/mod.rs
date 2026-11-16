//! The one local HTTP listener (bound to 127.0.0.1): `/ws` (JSON-RPC over WebSocket),
//! `POST /ingest`, `POST /auth/login`, `/mcp`, and the web UI at `/`.
//!
//! Every request passes [`guard`] first:
//! - `Host` must be `127.0.0.1:<port>` or `localhost:<port>` (DNS rebinding: a page on
//!   `evil.example` that resolves to 127.0.0.1 sends its own name);
//! - an `Origin`, when present (browsers), must be this daemon's own origin;
//! - `/ws` and `/ingest` need the daemon token (`Authorization: Bearer`, or for a browser
//!   WebSocket the subprotocol `pulse.token.<token>`); `/mcp` needs an agent token, never the
//!   daemon token; the static UI and the login exchange need none. A browser-style upgrade
//!   (offering the `pulse` subprotocol) with a missing or wrong token is not refused with a 401,
//!   which a browser cannot read, but upgraded and closed at once with 4401 (see
//!   [`pulse_contracts::auth`]).
//!
//! Handlers decode, call one service method, and map the error. Nothing here logs a request's
//! headers or query, so tokens never reach the log.

mod mcp;
mod rpc;
mod ui;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use pulse_contracts::rpc::{IngestParams, RpcError};
use pulse_contracts::status::{UiLoginRequest, UiLoginResponse};
use pulse_mcp::AgentIdentity;
use pulse_sources::ingest::{IngestError, IngestLimits};
use serde::Deserialize;

pub use mcp::McpBackend;

use crate::auth::{constant_time_eq, verify_agent_token};
use crate::error::ServiceError;
use crate::services::Services;
use crate::wiring::McpMount;

pub use pulse_contracts::auth::{WS_PROTOCOL, WS_TOKEN_PROTOCOL_PREFIX};

#[derive(Clone, Debug)]
pub(crate) struct App {
    pub services: Services,
}

pub(crate) fn router(services: Services, mcp: Option<McpMount>) -> Router {
    let app = App { services: services.clone() };
    let limits = IngestLimits::default();
    let mcp_routes: Router = match mcp {
        Some(mount) => Router::new().nest("/mcp", mount(McpBackend::new(services))),
        None => Router::new()
            .route("/mcp", any(mcp_unavailable))
            .route("/mcp/{*rest}", any(mcp_unavailable)),
    };
    Router::new()
        .route("/ws", get(rpc::upgrade))
        .route("/ingest", post(ingest).layer(DefaultBodyLimit::max(limits.max_body_bytes + 1)))
        .route("/auth/login", post(login))
        .fallback(ui::serve)
        .with_state(app.clone())
        .merge(mcp_routes)
        .layer(middleware::from_fn_with_state(app, guard))
}

fn allowed_host(host: &str, port: u16) -> bool {
    host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
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
    if let Some(origin) = req.headers().get(header::ORIGIN) {
        if !origin.to_str().is_ok_and(|o| allowed_origin(o, port)) {
            return deny(StatusCode::FORBIDDEN, "cross-origin requests are not allowed");
        }
    }
    let path = req.uri().path();
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
    } else if path == "/mcp" || path.starts_with("/mcp/") {
        let agent = bearer(req.headers())
            .and_then(|t| verify_agent_token(&shared.secret, t))
            .filter(|a| shared.snapshot().config.agent(a).is_some())
            .map(|name| AgentIdentity { name: name.to_owned() });
        match agent {
            Some(identity) => {
                req.extensions_mut().insert(identity);
            }
            None => return deny(StatusCode::UNAUTHORIZED, "missing or wrong agent token"),
        }
    }
    next.run(req).await
}

fn status_of(e: &ServiceError) -> StatusCode {
    match e {
        ServiceError::NotFound { .. } => StatusCode::NOT_FOUND,
        ServiceError::Invalid(_) | ServiceError::ConfigInvalid(_) => StatusCode::BAD_REQUEST,
        ServiceError::Conflict(_) => StatusCode::CONFLICT,
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

/// `POST /auth/login`: a login code from `pulse open` for the daemon token.
async fn login(State(app): State<App>, Json(req): Json<UiLoginRequest>) -> Response {
    let shared = &app.services.shared;
    if shared.login_codes.redeem(&shared.secret, &req.code, shared.clock.now()) {
        let mut res = Json(UiLoginResponse { token: shared.secret.clone() }).into_response();
        res.headers_mut()
            .insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
        res
    } else {
        deny(
            StatusCode::UNAUTHORIZED,
            "the login code is invalid, expired or already used; run `pulse open` again",
        )
    }
}

async fn mcp_unavailable() -> Response {
    deny(StatusCode::NOT_IMPLEMENTED, "the MCP server is not available in this build")
}
