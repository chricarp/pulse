//! Streamable HTTP at `/mcp`: bearer authentication in front of rmcp's service.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::{HeaderValue, StatusCode, header};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

use crate::server::{Authenticated, Caller, PulseMcp};
use crate::{Backend, McpConfig, Shared};

/// The MCP endpoint, for the daemon to mount with `Router::nest("/mcp", router(..))`.
///
/// Every request needs `Authorization: Bearer <agent token>`; a missing or unknown token is
/// answered `401` before rmcp sees the request. The identity resolved from the token is the
/// only one tools act as, per request: there is no session-level identity to switch.
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
        .with_cancellation_token(config.shutdown.child_token());
    let shared = Shared::new(backend, config);
    let for_factory = shared.clone();
    let service = StreamableHttpService::new(
        move || Ok(PulseMcp::new(for_factory.clone(), Caller::Http)),
        LocalSessionManager::default().into(),
        rmcp_config,
    );
    axum::Router::new()
        .fallback_service(service)
        .layer(axum::middleware::from_fn_with_state(shared, authenticate::<B>))
}

async fn authenticate<B: Backend>(
    State(shared): State<Arc<Shared<B>>>,
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
    match shared.backend.resolve_agent(token).await {
        Some(agent) => {
            request.extensions_mut().insert(Authenticated(agent));
            next.run(request).await
        }
        None => unauthorized("unknown token"),
    }
}

fn unauthorized(message: &'static str) -> Response {
    let mut response = (StatusCode::UNAUTHORIZED, message).into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer realm=\"pulse\""));
    response
}
