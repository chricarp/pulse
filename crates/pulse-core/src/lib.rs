//! Pure decision logic. No tokio, no filesystem, no clock: time, state and randomness are
//! arguments, so everything here is tested with explicit timestamps.
//!
//! - [`routing::route`]: the routing function, and the documented order of its checks.
//! - [`matcher`]: the `where` language. [`glob`]: glob matching. [`path`]: lexical path rules.
//! - [`scope`]: agent permission scopes (default deny, every path must be in scope).
//! - [`dedupe`], [`limits`] (cooldown, rate limit, circuit breaker), [`debounce`].
//! - [`causation`]: attributing an event to the run that caused it.
//! - [`filter`]: evaluating an `EventFilter` against one event (live subscriptions).
//! - [`prompt`]: prompt rendering with a fenced untrusted-data block, `command` argument
//!   rendering, and the event environment.
//!
//! Inputs the core requires: paths in events are clean absolute paths (see [`path`]); anything
//! else fails closed.

pub mod causation;
pub mod debounce;
pub mod dedupe;
pub mod filter;
pub mod glob;
pub mod limits;
pub mod lint;
pub mod matcher;
pub mod path;
pub mod prompt;
pub mod routing;
pub mod scope;

pub use causation::{RecentRun, RunWrites, attribute, cause_of_change};
pub use prompt::{NonceSource, RenderInput, event_env, render_args, render_prompt};
pub use routing::{
    Cause, DebounceKey, DueDebounce, RouteContext, RouteMode, RouterState, Routing, RuleRuntime,
    route,
};
