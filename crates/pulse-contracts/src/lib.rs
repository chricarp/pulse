//! Everything that crosses a process boundary or is written in a config file.
//!
//! - [`event`]: the CloudEvents envelope ([`Event`]) and what sources hand the engine
//!   ([`NewEvent`]).
//! - [`catalog`]: the v1 event types as data, typed payloads, and [`catalog::event_paths`].
//! - [`config`]: the `pulse.toml` schema, parsing with located errors, and resolution of the user
//!   file with trusted project files.
//! - [`pattern`] and [`template`]: glob and template *syntax* (matching and rendering live in
//!   `pulse-core`).
//! - [`run`]: runs, output, routing decisions ([`Outcome`], [`BlockReason`]) and deliveries.
//! - [`auth`]: how clients authenticate (login codes, WebSocket subprotocols, close codes).
//! - [`rpc`]: JSON-RPC framing, the v1 method table, notifications.
//! - [`typescript`]: generation of `apps/web/src/contracts/index.ts`.
//!
//! Contract: serde types and validation only. No I/O, no clock, no environment: the home
//! directory and the current time are always parameters.

pub mod auth;
pub mod catalog;
pub mod config;
pub mod event;
pub mod ids;
pub mod pattern;
pub mod rpc;
pub mod run;
pub mod status;
pub mod template;
pub mod time;
pub mod typescript;

pub use event::{
    Attribution, Causation, Event, EventType, Inference, NewEvent, SourceKind, SourceRef,
};
pub use ids::{DeliveryId, EventId, RunId, SubscriptionId, Ulid};
pub use run::{
    BlockReason, Decision, Delivery, DeliveryOutcome, Outcome, OutputKind, RenderedPrompt, Run,
    RunOutput, RunStatus, RunSummary,
};
pub use time::{Duration, Timestamp};

/// The version of the Pulse wire protocol and config schema.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
