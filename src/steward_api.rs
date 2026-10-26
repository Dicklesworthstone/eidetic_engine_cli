//! Public steward API and the journaled RPC-daemon scheduler.
//!
//! Explicit exports take precedence over historical scheduler implementations.
//! Manual and bounded foreground callers keep their existing APIs and reports.
//! The shadowed scheduler in `steward/mod.rs` is reached only by that file's
//! tests and carries `expect(dead_code)` until it is removed.

#[path = "steward/mod.rs"]
mod implementation;
pub use implementation::*;

#[path = "steward/durable_scheduler.rs"]
mod durable_scheduler;
pub use durable_scheduler::run_daemon_background_scheduler;
