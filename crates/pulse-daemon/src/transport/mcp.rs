//! The daemon's [`pulse_mcp::Backend`]: what the MCP server (mounted at `/mcp`) may ask. Every
//! answer is scoped to the calling agent's `allow` list through `Routing::agent_permits`, with
//! the configuration in effect at the time of the call. There is deliberately no way to approve
//! runs or change rules through it.

use pulse_contracts::catalog::EventTypeInfo;
use pulse_contracts::rpc::{EventFilter, EventPage, EventQuery};
use pulse_mcp::{AgentIdentity, AgentSubscription, Backend, BackendError};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::auth::verify_agent_token;
use crate::error::ServiceError;
use crate::hub::Sent;
use crate::services::Services;

/// Buffered batches per MCP subscription before delivery reports "full" (and batches are
/// summarized into a dropped count).
const SUBSCRIPTION_BUFFER: usize = 16;

#[derive(Clone, Debug)]
pub struct McpBackend {
    services: Services,
}

impl McpBackend {
    pub(crate) fn new(services: Services) -> Self {
        McpBackend { services }
    }
}

fn backend_error(e: ServiceError) -> BackendError {
    match e {
        ServiceError::Invalid(m) => BackendError::Invalid(m),
        ServiceError::NotFound { .. } | ServiceError::ConfigInvalid(_) => {
            BackendError::Invalid(e.to_string())
        }
        other => BackendError::Unavailable(other.to_string()),
    }
}

impl Backend for McpBackend {
    async fn resolve_agent(&self, token: &str) -> Option<AgentIdentity> {
        let shared = &self.services.shared;
        let agent = verify_agent_token(&shared.secret, token)?;
        shared.snapshot().config.agent(agent)?;
        Some(AgentIdentity { name: agent.to_owned() })
    }

    fn event_types(&self, _agent: &AgentIdentity) -> Vec<EventTypeInfo> {
        self.services.event_types()
    }

    async fn query_events(
        &self,
        agent: &AgentIdentity,
        query: EventQuery,
    ) -> Result<EventPage, BackendError> {
        let snapshot = self.services.shared.snapshot();
        if snapshot.config.agent(&agent.name).is_none() {
            return Err(BackendError::Unauthorized);
        }
        let mut page = self.services.events_query(query).await.map_err(backend_error)?;
        // Filtered after paging, so a page may be short; `next_cursor` still continues it.
        page.events.retain(|e| snapshot.routing.agent_permits(&agent.name, e));
        Ok(page)
    }

    async fn subscribe(
        &self,
        agent: &AgentIdentity,
        filter: EventFilter,
    ) -> Result<AgentSubscription, BackendError> {
        if self.services.shared.snapshot().config.agent(&agent.name).is_none() {
            return Err(BackendError::Unauthorized);
        }
        let (tx, rx) = mpsc::channel(SUBSCRIPTION_BUFFER);
        let stop = CancellationToken::new();
        // Dropping the receiver ends the subscription even if no event ever arrives.
        let closed = tx.clone();
        let stop_on_drop = stop.clone();
        tokio::spawn(async move {
            closed.closed().await;
            stop_on_drop.cancel();
        });
        let deliver =
            move |batch: &pulse_contracts::rpc::EventsBatch| match tx.try_send(batch.clone()) {
                Ok(()) => Sent::Ok,
                Err(mpsc::error::TrySendError::Full(_)) => Sent::Full,
                Err(mpsc::error::TrySendError::Closed(_)) => Sent::Closed,
            };
        let id = self
            .services
            .events_subscribe(
                filter,
                Some(agent.name.clone()),
                deliver,
                stop,
                self.services.shared.shutdown.child_token(),
            )
            .map_err(backend_error)?;
        Ok(AgentSubscription { id, events: rx })
    }
}
