//! The edges the daemon is built with. [`Wiring::real`] uses the real sources, adapters and
//! supervisor; tests replace any of them with fakes. The MCP server is not an edge: `/mcp` is
//! always `pulse_mcp::router` over the daemon's own services.

use std::fmt;
use std::sync::Arc;

use pulse_agents::{
    AgentAdapter, Invocation, OutputParser, RunEnd, SuperviseError, SuperviseOptions,
    SupervisorEvent,
};
use pulse_contracts::config::{AgentKind, SourceConfig};
use pulse_sources::{BoxFuture, Source, SourceEnv, SourceError};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Runs one agent process. The real one is [`pulse_agents::supervise`]; see its contract
/// (process group, `Started` before output, bounded output channel, terminate the group on
/// cancel or timeout, return after reaping).
pub trait Supervisor: Send + Sync + 'static {
    fn supervise(
        &self,
        invocation: Invocation,
        parser: Box<dyn OutputParser>,
        events: mpsc::Sender<SupervisorEvent>,
        options: SuperviseOptions,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<RunEnd, SuperviseError>>;
}

/// [`pulse_agents::supervise`].
#[derive(Clone, Copy, Debug, Default)]
pub struct RealSupervisor;

impl Supervisor for RealSupervisor {
    fn supervise(
        &self,
        invocation: Invocation,
        parser: Box<dyn OutputParser>,
        events: mpsc::Sender<SupervisorEvent>,
        options: SuperviseOptions,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<RunEnd, SuperviseError>> {
        Box::pin(pulse_agents::supervise(invocation, parser, events, options, cancel))
    }
}

/// Builds a source from its config ([`pulse_sources::build`] by default).
pub type SourceBuilder =
    Arc<dyn Fn(&SourceConfig, &SourceEnv) -> Result<Box<dyn Source>, SourceError> + Send + Sync>;

/// Finds the adapter for an agent kind ([`pulse_agents::adapter_for`] by default).
pub type AdapterLookup = fn(AgentKind) -> Option<&'static dyn AgentAdapter>;

#[derive(Clone)]
pub struct Wiring {
    pub sources: SourceBuilder,
    pub adapters: AdapterLookup,
    pub supervisor: Arc<dyn Supervisor>,
}

impl Wiring {
    /// The real edges.
    pub fn real() -> Wiring {
        Wiring {
            sources: Arc::new(pulse_sources::build),
            adapters: pulse_agents::adapter_for,
            supervisor: Arc::new(RealSupervisor),
        }
    }
}

impl fmt::Debug for Wiring {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Wiring").finish_non_exhaustive()
    }
}
