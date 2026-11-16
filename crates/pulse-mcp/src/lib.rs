//! The MCP server Pulse exposes to agents.
//!
//! ## Contract
//!
//! The server is generic over a [`Backend`], which the daemon implements and tests fake. The
//! backend does the scoping: every call takes the calling [`AgentIdentity`], and results only
//! include events that agent's `allow` list covers (`pulse_core::Routing::agent_permits`).
//! MCP cannot approve runs or change rules; there is deliberately no backend method for it.
//!
//! Tools (to build on the backend): `list_event_types`, `query_events`, `subscribe`,
//! `unsubscribe`, and a pull tool returning buffered events for clients that cannot take
//! server push. Buffers are bounded; overflow is counted, not stored.
//!
//! Authentication: `/mcp` requests carry an agent token, derived from the daemon secret and the
//! agent name; [`Backend::resolve_agent`] maps it to an identity or rejects it.
//!
//! The draft Events extension (SEP-3415) lives only in the `sep3415` module behind the cargo
//! feature of the same name. Spec types must not leak out of it.

use std::future::Future;

use pulse_contracts::SubscriptionId;
use pulse_contracts::catalog::EventTypeInfo;
use pulse_contracts::rpc::{EventFilter, EventPage, EventQuery, EventsBatch};
use tokio::sync::mpsc;

#[cfg(feature = "sep3415")]
pub mod sep3415;

/// The agent an MCP session acts as.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AgentIdentity {
    pub name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("not authorized")]
    Unauthorized,
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("unavailable: {0}")]
    Unavailable(String),
}

/// A live, scoped subscription. Batches arrive on `events`; dropping it unsubscribes.
#[derive(Debug)]
pub struct AgentSubscription {
    pub id: SubscriptionId,
    pub events: mpsc::Receiver<EventsBatch>,
}

/// What the MCP server needs from the daemon.
pub trait Backend: Send + Sync + 'static {
    /// The agent a token belongs to, or `None` if the token is not valid for `/mcp`.
    fn resolve_agent(&self, token: &str) -> impl Future<Output = Option<AgentIdentity>> + Send;

    /// The catalog (not scoped: knowing a type exists reveals nothing).
    fn event_types(&self) -> Vec<EventTypeInfo>;

    /// Queries history as `agent`: only events its scopes cover are returned. Pages may be
    /// shorter than `limit` because of scoping; follow `next_cursor`.
    fn query_events(
        &self,
        agent: &AgentIdentity,
        query: EventQuery,
    ) -> impl Future<Output = Result<EventPage, BackendError>> + Send;

    /// Subscribes as `agent` to new events matching `filter` and its scopes.
    fn subscribe(
        &self,
        agent: &AgentIdentity,
        filter: EventFilter,
    ) -> impl Future<Output = Result<AgentSubscription, BackendError>> + Send;
}
