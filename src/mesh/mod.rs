//! Optional mesh memory surface (SRR6 umbrella).
//!
//! `ee` is local-first; the mesh layer is an opt-in compose of SRR6 primitives
//! (transport, peer trust, workspace scope, peer enrollment) that lets multiple
//! machines on a trusted Tailscale network share, cache, and learn from each
//! other's memories.
//!
//! With `EE_MESH_ENABLED=0` (the default) none of the code in this module is
//! reachable from ordinary commands. ADR 0037 / ADR 0038 own the invariants.
//!
//! Sub-surfaces live in their own files and are documented in their bead
//! descriptions (SRR6.46.* under bd-36bbk).

pub mod auto_enrollment_safety;
pub mod discovery_policy;
pub mod hello;
pub mod identity_change_guard;
pub mod lane_grant_preview;
pub mod repair_action_graph;
