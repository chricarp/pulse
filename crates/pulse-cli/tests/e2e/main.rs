//! End-to-end tests: the real `pulse` binary as the daemon, real sources on temp directories
//! and repositories, real adapters and supervisor, and the fake agents instead of models.
//! One binary (modules below) to keep link time down. How to add one:
//! `docs/internals/0009-end-to-end.md`.
//!
//! No sleeps: every test waits for a specific event, delivery, run state or output (see
//! `support::Watch::until`), with `support::CEILING` as the failure-only bound. Durations that
//! must pass (quiet periods, debounces) are configured short.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout, clippy::print_stderr)]

mod support;

mod loops;
mod restart;
mod templates;
