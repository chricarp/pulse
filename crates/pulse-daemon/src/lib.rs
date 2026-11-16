//! The daemon: the engine that ties sources, routing, the store and agents together, the
//! services that are its API, and the transports in front of them.
//!
//! ```text
//! sources ─▶ engine ─(one transaction)─▶ store ◀─ effect worker ─▶ adapter ─▶ supervisor
//!   ingest/report/replay ─┘   │                     │
//!                            hub ◀──────────────────┘   (live events, run changes, output)
//!                             │
//!   services ◀── transports: /ws (JSON-RPC), /ingest, /auth/login, /mcp, / (UI)
//! ```
//!
//! - [`daemon`]: start and graceful shutdown; [`Daemon`] is what `pulse start` runs.
//! - `engine`: dedupe, attribution, batching, `route()`, the atomic commit, the one debounce
//!   timer. `worker`: outbox effects, concurrency, supervision, output, approvals, cancel.
//! - [`Services`]: `events`, `rules`, `agents`, `runs`, `sources`, `projects`, `system` as plain
//!   Rust methods. Transports call exactly one of them per request.
//! - `config`: loading, last-good, hot reload, project trust, format-preserving write-back.
//! - `hub`: bounded, rate-limited fan-out to subscribers.
//! - [`wiring`]: the injected edges (source builder, adapters, supervisor).
//! - [`client`]: the typed RPC client the CLI uses.
//! - [`auth`]: daemon token, per-agent MCP tokens, UI login codes.
//!
//! Decisions and their reasons: `docs/internals/0004-daemon-and-config.md`.

pub mod auth;
pub mod client;
pub mod clock;
mod config;
pub mod daemon;
mod engine;
mod error;
pub mod home;
mod hub;
pub mod logging;
mod release;
mod server;
mod services;
mod shared;
mod sources;
mod transport;
pub mod wiring;
mod worker;

pub use clock::Clock;
pub use daemon::{Daemon, DaemonOptions, StartError};
pub use error::{ServiceError, ServiceResult};
pub use home::PulseHome;
pub use services::Services;
pub use transport::{WS_PROTOCOL, WS_TOKEN_PROTOCOL_PREFIX};
pub use wiring::{RealSupervisor, Supervisor, Wiring};

/// The daemon's version, reported by `system.status`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
