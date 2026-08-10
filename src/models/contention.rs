//! Wire types for the read-only `ee.diag.contention.v1` diagnostic.
//!
//! Under massive agent swarms (dozens of concurrent `ee` processes plus
//! internal threads on 64-core hosts) throughput is bottlenecked at a handful
//! of well-known hot-path locks and queues, but those signals are scattered
//! across subsystems: the write-owner queue depth and wait times, the read
//! pool's acquire-wait p99 plus ad-hoc bypass count, single-flight
//! leader/follower posture, and (once Tracks B/C land) group-commit,
//! incremental-index, and L2-cache statistics. `ee.diag.contention.v1`
//! aggregates these into one read-only posture report with a ranked
//! `topContention` list so an operator or agent can see *where* the swarm is
//! contended without scraping five separate commands.
//!
//! The report is deterministic given its inputs and embeds no wall-clock —
//! only monotonic counters and measured wait nanoseconds — mirroring
//! `ee.diag.plan_cache.v1` (see ADR 0079 and `docs/schemas/ee.diag.plan_cache.v1.json`).
//!
//! This module holds only the serializable wire shapes. The deterministic
//! aggregation / posture / ranking logic lives in [`crate::core::contention`];
//! the live snapshot gathering and the `ee diag contention` CLI surface land in
//! bd-d67os.12. Tracks B (group-commit) and C (incremental index) add their
//! `Option<...>` sub-reports later; those fields are omit-safe here.

use serde::{Deserialize, Serialize};

/// Stable schema tag emitted in the report's `schemaTag` field and registered
/// in `public_schemas()`. Matches `docs/schemas/ee.diag.contention.v1.json`.
pub const CONTENTION_DIAG_SCHEMA_V1: &str = "ee.diag.contention.v1";

/// Coarse contention severity for a single source and for the report overall.
///
/// Declaration order *is* the severity order (`Ok` < `Warm` < `Hot` <
/// `Contended`) so derived `Ord` / [`ContentionPosture::worst`] compute the
/// worst posture across sources deterministically.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentionPosture {
    /// No meaningful contention observed.
    #[default]
    Ok,
    /// Mild pressure; informational (coalescing active, queue non-empty).
    Warm,
    /// Notable pressure that can add latency under load.
    Hot,
    /// Saturation or failure signals; likely degrading throughput now.
    Contended,
}

impl ContentionPosture {
    /// Return the worse (higher-severity) of two postures.
    #[must_use]
    pub fn worst(self, other: Self) -> Self {
        if self >= other { self } else { other }
    }

    /// Stable lowercase string, identical to the serde representation.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warm => "warm",
            Self::Hot => "hot",
            Self::Contended => "contended",
        }
    }
}

/// Write-owner / write-lock contention. Sourced from
/// `crate::core::write_owner::WriteOwnerStatus` plus optional persisted
/// `lock_wait_ms` percentiles from the session-budget ledger.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteLockContention {
    /// Whether a write-owner actor is running (daemon write path).
    pub running: bool,
    /// Pending write requests queued behind the single writer.
    pub queue_depth: usize,
    /// Total write requests processed since start (monotonic).
    pub total_processed: u64,
    /// Rolling average queue wait, milliseconds.
    pub avg_wait_ms: f64,
    /// Maximum observed queue wait, milliseconds.
    pub max_wait_ms: u64,
    /// p50 of persisted per-write `lock_wait_ms`, when ledger samples exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock_wait_ms_p50: Option<u64>,
    /// p99 of persisted per-write `lock_wait_ms`, when ledger samples exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock_wait_ms_p99: Option<u64>,
    /// Classified posture for this source.
    pub posture: ContentionPosture,
}

/// Read-pool contention. Sourced 1:1 from
/// `crate::db::read_pool::PoolStats`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadPoolContention {
    /// Configured maximum pooled read connections.
    pub max_size: usize,
    /// Connections currently checked out.
    pub active: usize,
    /// Idle pooled connections.
    pub idle: usize,
    /// Active snapshot pins (WAL readers).
    pub active_pins: usize,
    /// Pins past their max-pin duration (cooperative expiry).
    pub expired_pins: usize,
    /// High-water mark of concurrent checkouts.
    pub max_seen: usize,
    /// Connections dropped rather than returned (monotonic).
    pub drops: u64,
    /// Failures returning a connection to the pool (monotonic).
    pub release_failures: u64,
    /// Acquisitions that bypassed the pool with an un-pooled connection
    /// because the pool was saturated past the acquire timeout (monotonic).
    pub ad_hoc_bypass_count: u64,
    /// Acquire-wait latency sample count.
    pub acquire_wait_samples: usize,
    /// p50 acquire-wait latency, nanoseconds.
    pub acquire_wait_p50_ns: u128,
    /// p99 acquire-wait latency, nanoseconds.
    pub acquire_wait_p99_ns: u128,
    /// True when the pool was effectively zero-sized under load.
    pub size_was_zero: bool,
    /// True when saturation indicators (bypass / high p99 / zero size) fired.
    pub underized: bool,
    /// Classified posture for this source.
    pub posture: ContentionPosture,
}

/// Single-flight read-coalescing posture. Sourced from
/// `crate::models::singleflight::SingleFlightPostureReport`. Coalescing
/// activity is healthy pressure relief; timeouts / failures / poisoning are
/// the adverse signals.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SingleflightContention {
    /// Aggregate single-flight status string ("idle", "active", …).
    pub status: String,
    /// Number of configured coalescing surfaces.
    pub configured_surface_count: u32,
    /// Leaders currently computing on behalf of followers.
    pub active_leader_count: u32,
    /// Total leader computations started (monotonic).
    pub leader_start_count: u64,
    /// Total followers that joined a leader instead of recomputing.
    pub follower_wait_count: u64,
    /// Followers that timed out waiting and fell back to independent compute.
    pub follower_timeout_count: u64,
    /// Leader computations that failed/panicked (followers still woken).
    pub leader_failure_count: u64,
    /// Results reused by followers (duplicate work avoided).
    pub reused_result_count: u64,
    /// Fraction of would-be-duplicate computations avoided, when measurable:
    /// `reused / (reused + leader_start)`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coalesce_efficiency: Option<f64>,
    /// Classified posture for this source.
    pub posture: ContentionPosture,
}

/// Group-commit write-intake stats. Omit-safe: present only once Track B
/// (bd-d67os.1..4) lands and group-commit is wired.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupCommitContention {
    /// Whether group-commit batching is enabled.
    pub enabled: bool,
    /// Commit batches formed (monotonic).
    pub batches: u64,
    /// Writes folded into a shared batch transaction (monotonic).
    pub writes_coalesced: u64,
    /// Fsyncs avoided by coalescing (monotonic).
    pub fsync_saved: u64,
    /// Mean writes per batch, when measurable.
    pub avg_batch_size: f64,
    /// Classified posture for this source.
    pub posture: ContentionPosture,
}

/// Incremental-index intake stats. Omit-safe: present only once Track C
/// (bd-d67os.5..8) lands.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexIntakeContention {
    /// Current intake mode ("full_rebuild", "incremental", "segment_merge").
    pub intake_mode: String,
    /// Full index rebuilds performed (monotonic).
    pub rebuilds: u64,
    /// Index publish swaps observed to stall readers (monotonic).
    pub swap_stalls: u64,
    /// Mean index-swap duration, milliseconds.
    pub avg_swap_ms: f64,
    /// Classified posture for this source.
    pub posture: ContentionPosture,
}

/// L2 pack-cache contention. Omit-safe: present only once an aggregate
/// `pack_l2` metrics accessor exists.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct L2CacheContention {
    /// Cache hits (monotonic).
    pub hits: u64,
    /// Cache misses (monotonic).
    pub misses: u64,
    /// Evictions (monotonic).
    pub evictions: u64,
    /// `hits / (hits + misses)`, or `null` when no lookups occurred.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hit_rate: Option<f64>,
    /// `evictions / inserts`-style thrash ratio, when measurable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thrash_ratio: Option<f64>,
    /// Classified posture for this source.
    pub posture: ContentionPosture,
}

/// OS advisory-lock (flock) gate contention on `<db>.write.lock` — the gate
/// separate one-shot `ee` processes actually contend on in the no-daemon
/// swarm (bd-d67os.26 audit). Sourced from `crate::db::FlockGateTelemetry`
/// process-local counters; omitted when no snapshot was gathered.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlockGateContention {
    /// Successful gate acquisitions in this process (monotonic).
    pub acquires: u64,
    /// Acquisitions that needed at least one blocked retry (monotonic).
    pub contended_acquires: u64,
    /// Rolling average acquire wait, milliseconds.
    pub avg_wait_ms: f64,
    /// Maximum observed acquire wait, milliseconds.
    pub max_wait_ms: u64,
    /// Acquisitions that exhausted retries and failed with the contention
    /// timeout (monotonic).
    pub timeouts: u64,
    /// Classified posture for this source.
    pub posture: ContentionPosture,
}

/// One ranked entry in `topContention`: a source whose posture is at least
/// `Warm`, with a stable reason code and copy-paste remediation commands.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentionFinding {
    /// Source identifier ("write_lock", "read_pool", "singleflight", …).
    pub source: String,
    /// Severity of this finding.
    pub severity: ContentionPosture,
    /// Stable machine reason code (e.g. "read_pool_ad_hoc_bypass").
    pub reason_code: String,
    /// Human-readable explanation (no secrets, no host-private paths).
    pub detail: String,
    /// Suggested remediation commands, in priority order.
    pub suggested_commands: Vec<String>,
}

/// A core runtime source that was expected but not gathered this run
/// (e.g. the write-owner actor is not running in one-shot CLI mode). Future
/// feature sources (group-commit / incremental-index / L2) are simply omitted,
/// not reported here.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentionSourceGap {
    /// Source identifier that was unavailable.
    pub source: String,
    /// Stable degraded code for the gap.
    pub code: String,
}

/// The full `ee.diag.contention.v1` report payload. Drops straight into the
/// `data.report` slot of the response envelope built by the future
/// `ee diag contention` handler.
///
/// `schema_tag` is a `&'static str`, so this type is `Serialize`-only (the
/// same shape as `PlanCacheDiagReport`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentionDiagReport {
    /// Stable schema tag, always [`CONTENTION_DIAG_SCHEMA_V1`].
    pub schema_tag: &'static str,
    /// Worst posture across all present sources.
    pub overall_posture: ContentionPosture,
    /// Write-lock / write-owner contention (always present).
    pub write_lock: WriteLockContention,
    /// Read-pool contention (always present).
    pub read_pool: ReadPoolContention,
    /// Single-flight coalescing posture (always present).
    pub singleflight: SingleflightContention,
    /// Group-commit stats; omitted until Track B lands.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_commit: Option<GroupCommitContention>,
    /// Incremental-index intake stats; omitted until Track C lands.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_intake: Option<IndexIntakeContention>,
    /// L2 pack-cache stats; omitted until an aggregate accessor exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub l2_cache: Option<L2CacheContention>,
    /// Direct-CLI flock-gate stats; omitted when no snapshot was gathered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flock_gate: Option<FlockGateContention>,
    /// Findings ranked by severity (desc) then source (asc), deterministically.
    pub top_contention: Vec<ContentionFinding>,
    /// Core sources that were expected but not gathered this run.
    pub unavailable_sources: Vec<ContentionSourceGap>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posture_orders_by_severity() {
        assert!(ContentionPosture::Ok < ContentionPosture::Warm);
        assert!(ContentionPosture::Warm < ContentionPosture::Hot);
        assert!(ContentionPosture::Hot < ContentionPosture::Contended);
        assert_eq!(
            ContentionPosture::Warm.worst(ContentionPosture::Contended),
            ContentionPosture::Contended
        );
        assert_eq!(
            ContentionPosture::Hot.worst(ContentionPosture::Ok),
            ContentionPosture::Hot
        );
    }

    #[test]
    fn posture_str_matches_serde() {
        for posture in [
            ContentionPosture::Ok,
            ContentionPosture::Warm,
            ContentionPosture::Hot,
            ContentionPosture::Contended,
        ] {
            let json = serde_json::to_string(&posture).expect("serialize posture");
            assert_eq!(json, format!("\"{}\"", posture.as_str()));
        }
    }
}
