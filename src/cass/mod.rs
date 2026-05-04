//! CASS (`coding_agent_session_search`) adapter (EE-100, EE-101).
//!
//! `ee` consumes CASS as a versioned CLI dependency rather than
//! linking to its internals. This module establishes the surfaces
//! `ee-core` will use to talk to it:
//!
//! * [`discover`] / [`discover_with_override`] — find the `cass` binary
//!   via `$PATH`, env var, or config override (EE-101);
//! * [`DiscoveredBinary`] / [`DiscoverySource`] — provenance for how
//!   the binary was located;
//! * [`CassClient`] — owns the binary path and stable env overrides;
//! * [`CassInvocation`] / [`CassOutcome`] — captured intent and result
//!   for one subprocess call (with provenance metadata baked in);
//! * [`CassExitClass`] — three-way classification that preserves the
//!   "degraded but data" state CASS exposes;
//! * [`CassContract`] — typed view of `cass api-version` /
//!   `cass capabilities` output, with [`CassContract::ensure_compatible`]
//!   guarding against contract drift;
//! * [`CassError`] — stable taxonomy keyed off the CASS stderr
//!   `error.kind` string (see EE-283 spike).
//!
//! What this slice does *not* yet ship:
//!
//! * a production JSON parser for the CASS contract (fixture tests use
//!   `serde_json`; preflight parsing lands in its own bead);
//! * an actual preflight invocation (`CassClient::preflight_invocations`
//!   returns the *intent*; running and parsing them is its own bead);
//! * caching, retry, or backoff (each has a dedicated bead).
//!
//! See `docs/spikes/cass-json-contract-stability.md` for the contract
//! basis and the surfaces `ee` is allowed to consume.

pub mod client;
pub mod contract;
pub mod error;
pub mod health;
pub mod import;
pub mod process;
pub mod session;

pub use client::{
    CassClient, DEFAULT_BINARY, DiscoveredBinary, DiscoverySource, STABLE_ENV_OVERRIDES, discover,
    discover_with_override,
};
pub use contract::{
    CassContract, REQUIRED_API_VERSION, REQUIRED_CAPABILITIES, REQUIRED_CONTRACT_VERSION,
};
pub use error::CassError;
pub use health::{CassDbHealth, CassHealth, CassIndexHealth};
pub use import::{
    CassImportError, CassImportOptions, CassImportReport, ImportSessionStatus, ImportedCassSession,
    import_cass_sessions,
};
pub use process::{CASS_EXIT_DEGRADED, CASS_EXIT_OK, CassExitClass, CassInvocation, CassOutcome};
pub use session::{
    CassAgent, CassAggregationBucket, CassIndexFreshness, CassRole, CassSearchCacheStats,
    CassSearchHit, CassSearchMeta, CassSearchResponse, CassSearchTiming, CassSessionInfo,
    CassSpanKind, CassTimestamp, CassViewSpan, ImportCursor, ImportSessionResult,
};

/// Stable subsystem name surfaced through `ee status` and audit logs.
pub const SUBSYSTEM: &str = "cass";

/// Return the stable subsystem identifier. Used by status/diagnostics
/// rendering to keep degradation labels stable across releases.
#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

#[cfg(test)]
mod tests {
    use super::{
        CASS_EXIT_DEGRADED, CASS_EXIT_OK, CassClient, CassContract, CassError, CassExitClass,
        CassInvocation, CassOutcome, DEFAULT_BINARY, DiscoveredBinary, DiscoverySource,
        REQUIRED_API_VERSION, REQUIRED_CAPABILITIES, REQUIRED_CONTRACT_VERSION,
        STABLE_ENV_OVERRIDES, discover, discover_with_override, subsystem_name,
    };
    use std::path::Path;

    #[test]
    fn subsystem_name_is_stable() {
        assert_eq!(subsystem_name(), "cass");
    }

    #[test]
    fn public_api_re_exports_compile() {
        // Names are referenced so a future rename of any public item
        // breaks this test instead of breaking downstream consumers
        // silently. The body builds tiny, valid values so the test
        // remains assertion-free of brittle structural matchers.
        let _ = DEFAULT_BINARY;
        let _ = REQUIRED_API_VERSION;
        let _ = REQUIRED_CONTRACT_VERSION;
        let _ = REQUIRED_CAPABILITIES;
        let _ = STABLE_ENV_OVERRIDES;
        let _ = CASS_EXIT_OK;
        let _ = CASS_EXIT_DEGRADED;

        // EE-101 discovery types
        let _ = DiscoverySource::Path.as_str();
        let _ = DiscoveredBinary::new(
            Path::new("/usr/bin/cass").to_path_buf(),
            DiscoverySource::Path,
        );
        let _ = discover;
        let _ = discover_with_override;

        let client: CassClient = CassClient::new_default();
        let inv: CassInvocation = client.invocation(["health"]);
        let outcome: CassOutcome =
            CassOutcome::synthetic(inv.clone(), b"{}".to_vec(), Vec::new(), Some(CASS_EXIT_OK));
        let class: CassExitClass = outcome.class();
        let contract: CassContract = CassContract::new(
            "0.0.0",
            REQUIRED_API_VERSION,
            REQUIRED_CONTRACT_VERSION,
            REQUIRED_CAPABILITIES.iter().copied(),
        );
        let _: Result<(), CassError> = contract.ensure_compatible();
        assert_eq!(class, CassExitClass::Success);
    }
}
