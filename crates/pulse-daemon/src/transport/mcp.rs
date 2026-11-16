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
//!
//! **What one agent can hold** is bounded in `pulse-mcp` (`pulse_mcp::Limits`): requests in
//! flight per agent and in total, sessions, waits, streams, subscriptions. The total keeps
//! `/mcp` to a few hundred sockets; [`raise_open_file_limit`] gives the process room above
//! that, so `/ws` (the CLI and the UI) stays reachable while one agent holds all it may.

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
        ServiceError::NotFound { .. }
        | ServiceError::Ambiguous { .. }
        | ServiceError::ConfigInvalid(_) => BackendError::Invalid(e.to_string()),
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

/// The soft limit on open descriptors the daemon asks for at start, if the hard limit allows.
/// The usual default soft limit is 1024 (Linux) or 256 (macOS), which `/mcp`'s total request
/// budget (256) plus the daemon's own descriptors (the store, watchers, run output pipes) and
/// idle keep-alive connections can approach. 8192 leaves room for all of those many times
/// over, and is below macOS's `OPEN_MAX` (10240), above which `setrlimit` fails there.
const OPEN_FILES_WANTED: u64 = 8192;

/// The soft limit to set, or `None` to leave it: raised to [`OPEN_FILES_WANTED`], never above
/// the hard limit, never lowered. `None` in an `Rlimit` is "unlimited".
fn raised_soft_limit(soft: Option<u64>, hard: Option<u64>) -> Option<u64> {
    let want = hard.map_or(OPEN_FILES_WANTED, |h| h.min(OPEN_FILES_WANTED));
    soft.filter(|&s| s < want).map(|_| want)
}

/// Raises this process's soft descriptor limit (see [`OPEN_FILES_WANTED`]). Called once, when
/// `/mcp` is built at start. Failing is not fatal: the per-agent and total budgets still hold.
fn raise_open_file_limit() {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
    let limit = getrlimit(Resource::Nofile);
    let Some(soft) = raised_soft_limit(limit.current, limit.maximum) else { return };
    match setrlimit(Resource::Nofile, Rlimit { current: Some(soft), maximum: limit.maximum }) {
        Ok(()) => tracing::debug!("raised the open file limit to {soft}"),
        Err(e) => tracing::warn!("cannot raise the open file limit to {soft}: {e}"),
    }
}

/// The MCP router for `/mcp`, built once: it owns the subscription registry. `allowed_hosts`
/// is the list the daemon's guard accepts.
pub(crate) fn router(services: Services, allowed_hosts: Vec<String>) -> Router {
    raise_open_file_limit();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_soft_descriptor_limit_is_raised_within_the_hard_limit() {
        let cases = [
            // (soft, hard) -> new soft
            ((Some(1024), Some(524_288)), Some(OPEN_FILES_WANTED)),
            ((Some(256), None), Some(OPEN_FILES_WANTED)),
            ((Some(1024), Some(4096)), Some(4096)),
            ((Some(512), Some(512)), None),
            ((Some(65_536), Some(1_048_576)), None),
            ((None, None), None),
        ];
        for ((soft, hard), want) in cases {
            assert_eq!(raised_soft_limit(soft, hard), want, "{soft:?} {hard:?}");
        }
    }
}
