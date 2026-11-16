//! The MCP server Pulse exposes to agents. Design and reasoning:
//! `docs/internals/0007-mcp-and-sep-3415.md`.
//!
//! ## Contract
//!
//! The server is generic over a [`Backend`], which the daemon implements and tests fake. The
//! backend does the scoping: every call takes the calling [`AgentIdentity`], and results only
//! include events that agent's `allow` list covers (`pulse_core::scope::ScopeSet::permits`).
//! This crate always passes the authenticated identity and offers no way to name another one.
//! MCP cannot approve runs or change rules; there is deliberately no backend method for it.
//!
//! ## Surfaces
//!
//! - [`router`]: streamable HTTP for the daemon to mount at `/mcp`. Bearer token →
//!   [`Backend::resolve_agent`] before any MCP handling.
//! - [`stdio::proxy`]: `pulse mcp --agent <name>`, a stdio-to-HTTP bridge to the daemon's `/mcp`.
//! - [`serve_io`]: the same server over any byte stream, bound to an already-authenticated
//!   identity (tests, embedding).
//!
//! Tools: `list_event_types`, `query_events`, `subscribe`, `unsubscribe`, `wait_for_events`.
//! Subscriptions are read from the event log by cursor (the `subscriptions` module).
//!
//! The MCP Events extension (SEP-3415) lives only in the `sep3415` module behind the cargo
//! feature of the same name and [`McpConfig::events_extension`]. Spec types do not leak out of it.

use std::fmt;
use std::future::Future;
use std::sync::Arc;

use pulse_contracts::catalog::EventTypeInfo;
use pulse_contracts::rpc::{EventFilter, EventPage, EventQuery};
use pulse_contracts::{EventId, SubscriptionId, Timestamp, Ulid};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

mod http;
mod present;
mod server;
mod sessions;
pub mod stdio;
mod subscriptions;
mod tools;

#[cfg(feature = "sep3415")]
mod sep3415;

pub use http::router;
pub use server::{ServeError, serve_io};

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

/// A live, scoped subscription. It carries no events: a message on `wake` says that something
/// matching the filter, that the agent may see, was committed, and the server reads it from the
/// log by cursor. One pending wakeup is enough (the backend may coalesce them). Dropping it
/// unsubscribes. The backend closes the channel when the subscription can no longer be served
/// (the agent was removed or lost access, the daemon is stopping).
#[derive(Debug)]
pub struct AgentSubscription {
    pub id: SubscriptionId,
    pub wake: mpsc::Receiver<()>,
}

/// What the MCP server needs from the daemon.
///
/// Ordering requirement: events must become visible to [`Backend::query_events`] in event-id
/// order (ids assigned and committed by the single engine task). Subscriptions read the log
/// after a cursor; an event that became visible after a later id would be skipped.
pub trait Backend: Send + Sync + 'static {
    /// The agent a token belongs to, or `None` if the token is not valid for `/mcp`.
    fn resolve_agent(&self, token: &str) -> impl Future<Output = Option<AgentIdentity>> + Send;

    /// The catalog entries `agent` could ever receive: those whose type its scopes can match
    /// (`ext.*` when any scope matches an `ext.` type). A model should not be shown types it
    /// can never see.
    fn event_types(&self, agent: &AgentIdentity) -> Vec<EventTypeInfo>;

    /// Queries history as `agent`: only events its scopes cover are returned. Pages may be
    /// shorter than `limit` (even empty) because of scoping; follow `next_cursor`, which must
    /// advance past every row the backend examined so that a reader is never stuck.
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

/// Wall-clock time, as a parameter so tests can fix it. Used for SEP-3415 `maxAgeMs` and as the
/// "from now" cursor of an empty log; timers use tokio's clock.
#[derive(Clone)]
pub struct Clock(Arc<dyn Fn() -> Timestamp + Send + Sync>);

impl Clock {
    pub fn system() -> Self {
        Clock(Arc::new(|| Timestamp::from_system_time(std::time::SystemTime::now())))
    }

    pub fn new(f: impl Fn() -> Timestamp + Send + Sync + 'static) -> Self {
        Clock(Arc::new(f))
    }

    pub fn now(&self) -> Timestamp {
        (self.0)()
    }

    /// A cursor positioned at "now": every event created from this millisecond on sorts after
    /// it (events from earlier in the same millisecond may too).
    pub(crate) fn now_cursor(&self) -> EventId {
        let ms = u64::try_from(self.now().as_millis()).unwrap_or(0);
        EventId::from_ulid(Ulid::from_parts(ms, 0))
    }
}

impl fmt::Debug for Clock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Clock")
    }
}

impl Default for Clock {
    fn default() -> Self {
        Clock::system()
    }
}

/// Server settings. The daemon builds one from `[daemon]` config.
#[derive(Clone, Debug)]
pub struct McpConfig {
    /// `[daemon] mcp_events_extension`. Without the `sep3415` cargo feature it has no effect.
    pub events_extension: bool,
    /// `Host` values accepted on `/mcp` (DNS rebinding). Entries are `host` or `host:port`.
    /// Pass the same list the daemon's own Host check uses.
    pub allowed_hosts: Vec<String>,
    /// Browser origins accepted on `/mcp`. Empty (the default) rejects every request that
    /// carries an `Origin` header: MCP clients are not browsers, and the web UI never calls
    /// `/mcp`.
    pub allowed_origins: Vec<String>,
    /// Cancelled when the daemon shuts down; ends open waits and streams.
    pub shutdown: CancellationToken,
    pub limits: Limits,
    pub clock: Clock,
}

impl Default for McpConfig {
    fn default() -> Self {
        McpConfig {
            events_extension: false,
            allowed_hosts: vec!["localhost".into(), "127.0.0.1".into(), "::1".into()],
            allowed_origins: Vec::new(),
            shutdown: CancellationToken::new(),
            limits: Limits::default(),
            clock: Clock::system(),
        }
    }
}

/// Bounds on what one agent can hold open and how much one result carries.
///
/// These are not user configuration: they are sized so that one agent token, however it is
/// used, cannot starve the daemon's other clients, and so that no honest client comes near
/// them. Each default says how it was chosen. Tests lower them to reach the edges cheaply.
#[derive(Clone, Debug)]
pub struct Limits {
    /// HTTP requests on `/mcp` in flight per agent, open SSE streams included (a long
    /// `wait_for_events`, a SEP-3415 push stream, a pre-2026 client's standalone `GET`). Past
    /// it: HTTP 429. Default 32: an honest client holds one or two (a wait and a `GET` stream),
    /// so this is room for about a dozen clients sharing one agent, and it keeps
    /// `waits_per_agent` plus `streams_per_agent` below it, so an agent at both of those can
    /// still call `unsubscribe` or `query_events`.
    pub requests_per_agent: usize,
    /// HTTP requests on `/mcp` in flight across all agents. Past it: HTTP 503. Default 256: each
    /// holds a socket, and this leaves most of even a 1024 descriptor limit to the daemon, its
    /// sources and `/ws`, with room for eight agents at their full share.
    pub requests_total: usize,
    /// Sessions per agent for clients on protocol revisions before 2026-07-28 (2026-07-28 has
    /// none). A new `initialize` past it closes that agent's least recently used session that
    /// has no request or stream open (the client of a closed session gets HTTP 404 and, per the
    /// spec, starts a new one). Default 48: above `requests_per_agent`, so at least one session
    /// is always idle and an `initialize` is never refused; a session costs about 56 KB, so
    /// this is under 3 MB per agent.
    pub sessions_per_agent: usize,
    /// Concurrent `wait_for_events` calls per subscription. Default 4: one per client, plus a
    /// few left behind by clients that gave up without closing the connection (a pre-2026
    /// session does not cancel a request when its connection drops; it ends at its timeout).
    pub waits_per_subscription: usize,
    /// Concurrent `wait_for_events` calls per agent, over all its subscriptions. Default 16:
    /// one per subscription at the subscription limit.
    pub waits_per_agent: usize,
    /// Subscriptions per agent. Subscribing again with the same filter reuses one.
    pub subscriptions_per_agent: usize,
    /// Subscriptions across all agents.
    pub subscriptions_total: usize,
    /// A subscription nobody has waited on for this long is removed.
    pub subscription_idle_ttl: std::time::Duration,
    /// Concurrent SEP-3415 push streams per agent. Default 8: a client opens one per event
    /// name it follows, and with `waits_per_agent` it stays below `requests_per_agent`.
    pub streams_per_agent: usize,
    /// Events per tool result when the caller does not ask for fewer.
    pub default_events: u32,
    /// Events per tool result, at most.
    pub max_events: u32,
    /// Bytes of event JSON per tool result, SEP-3415 poll result and push page. Events past it
    /// are left for the next call or page.
    pub result_max_bytes: usize,
    /// Bytes per string inside an event before it is shortened.
    pub field_max_bytes: u32,
    /// `wait_for_events` waits this long when the caller gives no timeout. Below Codex CLI's
    /// default 60 s tool timeout.
    pub default_wait: std::time::Duration,
    /// The longest wait a caller can ask for.
    pub max_wait: std::time::Duration,
    /// Progress notifications during a wait, for clients that sent a progress token.
    pub progress_interval: std::time::Duration,
}

impl Default for Limits {
    fn default() -> Self {
        use std::time::Duration;
        Limits {
            requests_per_agent: 32,
            requests_total: 256,
            sessions_per_agent: 48,
            waits_per_subscription: 4,
            waits_per_agent: 16,
            subscriptions_per_agent: 16,
            subscriptions_total: 256,
            subscription_idle_ttl: Duration::from_secs(30 * 60),
            streams_per_agent: 8,
            default_events: 20,
            max_events: 50,
            result_max_bytes: 24 * 1024,
            field_max_bytes: 1024,
            default_wait: Duration::from_secs(45),
            max_wait: Duration::from_secs(10 * 60),
            progress_interval: Duration::from_secs(15),
        }
    }
}

/// State shared by every handler instance. On protocol 2026-07-28 rmcp builds a fresh handler
/// per HTTP request, so nothing that must outlive a request may live in the handler.
pub(crate) struct Shared<B> {
    pub backend: Arc<B>,
    pub config: McpConfig,
    pub subscriptions: subscriptions::Registry,
    #[cfg(feature = "sep3415")]
    pub streams: sep3415::StreamCounts,
}

impl<B: Backend> Shared<B> {
    pub fn new(backend: Arc<B>, config: McpConfig) -> Arc<Self> {
        Arc::new(Shared {
            subscriptions: subscriptions::Registry::new(config.limits.clone()),
            #[cfg(feature = "sep3415")]
            streams: sep3415::StreamCounts::default(),
            backend,
            config,
        })
    }

    #[cfg(feature = "sep3415")]
    pub fn events_extension(&self) -> bool {
        self.config.events_extension
    }
}
