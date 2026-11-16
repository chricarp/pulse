//! Daemon-level tests: only what cannot be tested in core, store, sources or agents. One test
//! binary (modules below) to keep link time down.
//!
//! No sleeps: every test awaits a specific notification, run state or response, with
//! `support::CEILING` as the failure-only bound.

// Test helpers outside `#[test]` functions panic on failure too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

mod auth;
mod config;
mod engine;
mod mcp;
mod runs;
