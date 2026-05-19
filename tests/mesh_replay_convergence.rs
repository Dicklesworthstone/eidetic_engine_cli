//! Executable wrapper for the SRR6.18 replay convergence harness.
//!
//! The harness implementation lives under `tests/mesh/` per the SRR6 fixture
//! layout, while this top-level file makes it discoverable as a Cargo
//! integration test target.

#[path = "mesh/replay_convergence.rs"]
mod replay_convergence;
