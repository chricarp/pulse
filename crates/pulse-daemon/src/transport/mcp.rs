//! `/mcp`: the MCP server from `pulse-mcp`, mounted once, over the daemon's
//! [`pulse_mcp::Backend`]. Every answer is the `mcp_*` service of the same name, scoped to the
//! calling agent with the configuration in effect at the time of the call. There is
//! deliberately no way to approve runs or change rules through it.
//!
//! **Who is calling** is decided in one place: the router's bearer layer calls
//! [`Backend::resolve_agent`], which is `Services::mcp_agent`, on every request before rmcp
//! sees it, and tools act only as the identity it stored. The daemon's own guard checks `Host`
//! and refuses any `Origin` on `/mcp` (MCP clients are not browsers), and leaves the token to
//! that layer, so the daemon token, a forged token or a removed agent's token is a 401 there.

use std::sync::Arc;

use axum::Router;
use pulse_contracts::catalog::EventTypeInfo;
use pulse_contracts::rpc::{EventFilter, EventPage, EventQuery};
use pulse_mcp::{AgentIdentity, AgentSubscription, Backend, BackendError, McpConfig};

use crate::error::ServiceError;
use crate::services::Services;

#[derive(Clone, Debug)]
pub struct McpBackend {
    services: Services,
}

fn backend_error(e: ServiceError) -> BackendError {
    match e {
        // The agent was removed (or never existed) since its token was accepted.
        ServiceError::NotFound { what: "agent", .. } => BackendError::Unauthorized,
        ServiceError::Invalid(m) => BackendError::Invalid(m),
        ServiceError::NotFound { .. } | ServiceError::ConfigInvalid(_) => {
            BackendError::Invalid(e.to_string())
        }
        other => BackendError::Unavailable(other.to_string()),
    }
}

impl Backend for McpBackend {
    async fn resolve_agent(&self, token: &str) -> Option<AgentIdentity> {
        self.services.mcp_agent(token).map(|name| AgentIdentity { name })
    }

    fn event_types(&self, agent: &AgentIdentity) -> Vec<EventTypeInfo> {
        self.services.mcp_event_types(&agent.name)
    }

    async fn query_events(
        &self,
        agent: &AgentIdentity,
        query: EventQuery,
    ) -> Result<EventPage, BackendError> {
        self.services.mcp_query_events(&agent.name, query).await.map_err(backend_error)
    }

    async fn subscribe(
        &self,
        agent: &AgentIdentity,
        filter: EventFilter,
    ) -> Result<AgentSubscription, BackendError> {
        let (id, wake) = self.services.mcp_wakeups(&agent.name, filter).map_err(backend_error)?;
        Ok(AgentSubscription { id, wake })
    }
}

/// The MCP router for `/mcp`, built once: it owns the subscription registry. `allowed_hosts`
/// is the list the daemon's guard accepts.
pub(crate) fn router(services: Services, allowed_hosts: Vec<String>) -> Router {
    let shared = &services.shared;
    let clock = shared.clock;
    let config = McpConfig {
        events_extension: shared.snapshot().config.daemon.mcp_events_extension,
        allowed_hosts,
        allowed_origins: Vec::new(),
        shutdown: shared.shutdown.clone(),
        clock: pulse_mcp::Clock::new(move || clock.now()),
        ..McpConfig::default()
    };
    pulse_mcp::router(Arc::new(McpBackend { services }), config)
}
