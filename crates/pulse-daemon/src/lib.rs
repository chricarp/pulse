//! The daemon: engine wiring, services, the effect worker, config load/watch/write-back,
//! project trust, and transports (RPC over WebSocket, `POST /ingest`, `/mcp`, the static UI).
//!
//! Not built yet. The pieces it wires together are ready:
//! - `pulse_contracts::config::{parse_config_file, resolve}` and `pulse_core::Routing::compile`
//!   for config; `pulse_contracts::rpc::{Request, RpcHandler, dispatch}` for the RPC server;
//! - `pulse_core::{attribute, route, RouterState}` and `pulse_core::dedupe::Dedupe` for the
//!   engine loop; `pulse_store::StoreHandle` and `Store::commit` for persistence;
//! - `pulse_sources::{build, context}`, `pulse_agents::{adapter_for, supervise}` and
//!   `pulse_mcp::Backend` for the edges.
//!
//! Transports decode, call one service method, and map the error. Logic goes in services.

/// The daemon's version, reported by `system.status`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
