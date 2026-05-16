//! Pure admission helpers for graph snapshot + algorithm memory
//! budgeting (bd-bife.24).
//!
//! `fnx-classes::DiGraph` in RAM scales roughly as `32 * |V| + 96 *
//! |E|` (per-node string id + AttrMap overhead, plus per-edge
//! tuple + AttrMap). At 100k memories with typed edges this can
//! easily exceed RX7's 250 MB snapshot footprint target. Today
//! there's no runtime check, so a workspace that grew past the
//! target would just OOM the `ee` process. This module is the
//! pure decision layer that lets the F1 snapshot builders and F2
//! algorithm wrappers refuse work *before* allocating, with a
//! deterministic degradation envelope the caller can render as
//! either a `degraded[]` row or an audit-log entry.
//!
//! ## Decisions exposed
//!
//! - [`estimate_snapshot_bytes`] — the `32|V| + 96|E|` pre-build
//!   estimate, with explicit `usize → u64` saturation so a node
//!   count overflow can't silently wrap to a small estimate.
//! - [`check_snapshot_admission`] — apply the configured
//!   `snapshot_cap_bytes` to a pre-build estimate. Returns
//!   [`SnapshotAdmissionDecision::Admit { headroom_bytes,
//!   approaching_cap }`] when the build can proceed (with the
//!   `approaching_cap` flag set when the estimate is past
//!   `degraded_below_pct` of the cap so the caller can fire an
//!   advisory degraded code), or [`SnapshotAdmissionDecision::Refuse`]
//!   with the documented `degraded.large_graph_uncached` code.
//! - [`check_in_build_growth`] — the F1.b/c/d in-build measurement
//!   check. If observed allocated bytes exceed `1.5 ×` the
//!   pre-build estimate the build must abort with the documented
//!   `degraded.unexpected_growth` code.
//! - [`check_algorithm_admission`] — the F2 in-query measurement
//!   check. Given currently-resident snapshot bytes plus the
//!   requested algorithm's working-set estimate, returns
//!   [`AlgorithmAdmissionDecision::Refuse`] (`degraded.memory_pressure`)
//!   when the combined total would breach `snapshot_cap_bytes`,
//!   or [`AlgorithmAdmissionDecision::Refuse`]
//!   (`degraded.algorithm_memory_cap`) when the requested working
//!   set alone exceeds `per_algorithm_cap_bytes`.
//!
//! All decisions return a deterministic `Refuse` payload that the
//! call site can lower directly into the existing
//! `DegradationReport` shape; this module deliberately does not
//! emit `tracing` events or write audit rows itself so the helper
//! stays drop-in-pure for unit tests and can compose with the
//! existing `graph_audit` / `graph_telemetry` modules at the
//! wiring layer.

use serde::Serialize;

/// Default snapshot footprint cap per RX7.
pub const DEFAULT_SNAPSHOT_CAP_MB: u64 = 250;
/// Default per-algorithm working-set cap.
pub const DEFAULT_PER_ALGORITHM_CAP_MB: u64 = 100;
/// Default advisory degraded-code threshold, expressed as a
/// percentage of `snapshot_cap_bytes`. 80% means the
/// `approaching_cap` flag fires at the 80%-of-cap line so the
/// caller can warn before the next workspace growth tips into a
/// hard refusal.
pub const DEFAULT_DEGRADED_BELOW_PCT: u8 = 80;

/// Per-node byte estimate used by [`estimate_snapshot_bytes`]:
/// roughly `String id (24)` + `AttrMap entry overhead (8)`.
pub const PER_NODE_BYTES: u64 = 32;
/// Per-edge byte estimate used by [`estimate_snapshot_bytes`]:
/// roughly `(src, dst) String refs (48)` + `AttrMap (32)` + alignment.
pub const PER_EDGE_BYTES: u64 = 96;

/// Degraded code for "the pre-build estimate already exceeds
/// `snapshot_cap_bytes`; the snapshot cannot be built." Stable
/// wire string consumed by both `degraded[]` arrays and audit
/// rows.
pub const LARGE_GRAPH_UNCACHED_CODE: &str = "large_graph_uncached";

/// Degraded code for "the in-build allocation grew past 1.5× the
/// pre-build estimate; the build was aborted to avoid runaway
/// memory."
pub const UNEXPECTED_GROWTH_CODE: &str = "unexpected_growth";

/// Degraded code for "active snapshots plus the requested
/// algorithm's working-set would breach the configured
/// `snapshot_cap_bytes`; the algorithm was refused before
/// allocating."
pub const MEMORY_PRESSURE_CODE: &str = "memory_pressure";

/// Degraded code for "the requested algorithm's working-set alone
/// exceeds `per_algorithm_cap_bytes`; the algorithm was refused
/// before allocating."
pub const ALGORITHM_MEMORY_CAP_CODE: &str = "algorithm_memory_cap";

/// Advisory degraded code emitted with [`SnapshotAdmissionDecision::Admit`]
/// when the estimate is past `degraded_below_pct` of the cap;
/// callers should surface it in the response envelope so an
/// operator notices the workspace is approaching the hard limit
/// before the next growth tips it over.
pub const APPROACHING_CAP_CODE: &str = "snapshot_approaching_cap";

/// Hard ratio for the in-build growth tripwire: observed allocated
/// bytes greater than `growth_multiplier × pre_build_estimate`
/// triggers the abort path.
const DEFAULT_GROWTH_MULTIPLIER_BASIS_POINTS: u32 = 15_000;

/// Effective runtime memory budget for graph snapshots + per-
/// algorithm working sets. The fields mirror the documented
/// `graph.memory.*` config keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryBudgetPolicy {
    pub snapshot_cap_bytes: u64,
    pub per_algorithm_cap_bytes: u64,
    /// Advisory threshold in percent (0-100). Estimates that land
    /// at or above this fraction of `snapshot_cap_bytes` cause
    /// [`check_snapshot_admission`] to set `approaching_cap` so
    /// the caller can emit [`APPROACHING_CAP_CODE`].
    pub degraded_below_pct: u8,
    /// Growth tripwire ratio in basis points (10_000 = 1.0×).
    /// Defaults to 15_000 (1.5×) per the bd-bife.24 spec.
    pub growth_multiplier_basis_points: u32,
}

impl MemoryBudgetPolicy {
    /// Defaults: 250 MB snapshot cap, 100 MB per-algorithm cap,
    /// 80% advisory threshold, 1.5× growth multiplier.
    #[must_use]
    pub const fn defaults() -> Self {
        Self {
            snapshot_cap_bytes: DEFAULT_SNAPSHOT_CAP_MB * 1024 * 1024,
            per_algorithm_cap_bytes: DEFAULT_PER_ALGORITHM_CAP_MB * 1024 * 1024,
            degraded_below_pct: DEFAULT_DEGRADED_BELOW_PCT,
            growth_multiplier_basis_points: DEFAULT_GROWTH_MULTIPLIER_BASIS_POINTS,
        }
    }
}

impl Default for MemoryBudgetPolicy {
    fn default() -> Self {
        Self::defaults()
    }
}

/// Refusal payload returned by every `Refuse` variant. The shape
/// matches `DegradationReport` field-for-field so the call site
/// can lower it directly into a `degraded[]` row.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryBudgetRefusal {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: &'static str,
    pub repair: &'static str,
    /// Bytes observed (estimate or measurement, depending on the
    /// decision).
    pub observed_bytes: u64,
    /// Bytes allowed by the policy that triggered the refusal.
    pub limit_bytes: u64,
}

/// Outcome of [`check_snapshot_admission`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SnapshotAdmissionDecision {
    /// Build can proceed. `approaching_cap` fires when the
    /// estimate is past `degraded_below_pct` of the cap.
    Admit {
        estimate_bytes: u64,
        headroom_bytes: u64,
        approaching_cap: bool,
    },
    /// Build must be skipped; the snapshot table should record
    /// `Skipped` status and the response should carry the refusal
    /// as a `degraded[]` row.
    Refuse(MemoryBudgetRefusal),
}

/// Outcome of [`check_in_build_growth`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InBuildGrowthDecision {
    /// Observed bytes are within the growth tripwire; continue
    /// building.
    Continue {
        observed_bytes: u64,
        allowed_bytes: u64,
    },
    /// Observed bytes exceeded the tripwire; abort + roll back.
    Abort(MemoryBudgetRefusal),
}

/// Outcome of [`check_algorithm_admission`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AlgorithmAdmissionDecision {
    /// Algorithm may run; `combined_bytes` is the running total
    /// after the request lands so the caller can update its
    /// internal accounting before invoking.
    Admit { combined_bytes: u64 },
    /// Algorithm refused before allocating; surface the refusal
    /// in the response envelope.
    Refuse(MemoryBudgetRefusal),
}

/// Pre-build byte estimate for a snapshot of `node_count` nodes
/// and `edge_count` edges. The computation uses saturating
/// arithmetic so an attacker-controlled `node_count` of
/// `usize::MAX` can't silently wrap to a tiny value that would
/// allow an unbounded build.
#[must_use]
pub fn estimate_snapshot_bytes(node_count: usize, edge_count: usize) -> u64 {
    let nodes = u64::try_from(node_count).unwrap_or(u64::MAX);
    let edges = u64::try_from(edge_count).unwrap_or(u64::MAX);
    let node_bytes = nodes.saturating_mul(PER_NODE_BYTES);
    let edge_bytes = edges.saturating_mul(PER_EDGE_BYTES);
    node_bytes.saturating_add(edge_bytes)
}

/// Apply a [`MemoryBudgetPolicy`] to the pre-build estimate and
/// decide whether the snapshot build may proceed.
#[must_use]
pub fn check_snapshot_admission(
    estimate_bytes: u64,
    policy: &MemoryBudgetPolicy,
) -> SnapshotAdmissionDecision {
    if estimate_bytes > policy.snapshot_cap_bytes {
        return SnapshotAdmissionDecision::Refuse(MemoryBudgetRefusal {
            code: LARGE_GRAPH_UNCACHED_CODE,
            severity: "high",
            message: "graph snapshot estimate exceeds the configured cap; build skipped",
            repair: "raise graph.memory.snapshot_cap_mb or shrink the workspace",
            observed_bytes: estimate_bytes,
            limit_bytes: policy.snapshot_cap_bytes,
        });
    }
    let headroom_bytes = policy.snapshot_cap_bytes.saturating_sub(estimate_bytes);
    let advisory_threshold = scale_by_basis_points(
        policy.snapshot_cap_bytes,
        u32::from(policy.degraded_below_pct) * 100,
    );
    SnapshotAdmissionDecision::Admit {
        estimate_bytes,
        headroom_bytes,
        approaching_cap: estimate_bytes >= advisory_threshold,
    }
}

/// Apply the policy's growth tripwire to an observed allocation
/// during a snapshot build. If `observed_bytes` exceeds
/// `growth_multiplier × pre_build_estimate`, return
/// [`InBuildGrowthDecision::Abort`] so the caller can roll back.
#[must_use]
pub fn check_in_build_growth(
    pre_build_estimate: u64,
    observed_bytes: u64,
    policy: &MemoryBudgetPolicy,
) -> InBuildGrowthDecision {
    let allowed = scale_by_basis_points(pre_build_estimate, policy.growth_multiplier_basis_points);
    if observed_bytes > allowed {
        InBuildGrowthDecision::Abort(MemoryBudgetRefusal {
            code: UNEXPECTED_GROWTH_CODE,
            severity: "warning",
            message: "in-build allocation grew past the tripwire; aborted and rolled back",
            repair: "shrink the workspace, rerun snapshot refresh, or raise the growth multiplier",
            observed_bytes,
            limit_bytes: allowed,
        })
    } else {
        InBuildGrowthDecision::Continue {
            observed_bytes,
            allowed_bytes: allowed,
        }
    }
}

/// Decide whether an algorithm may run, given currently-resident
/// snapshot bytes (`active_resident_bytes`) and the requested
/// algorithm's working-set estimate (`requested_bytes`).
///
/// Two separate refusal codes fire:
///
/// - [`ALGORITHM_MEMORY_CAP_CODE`] when the requested working-set
///   alone exceeds the per-algorithm cap (refuses *before* the
///   combined check so the operator sees the right remediation).
/// - [`MEMORY_PRESSURE_CODE`] when active snapshots plus the
///   request would breach the snapshot cap.
#[must_use]
pub fn check_algorithm_admission(
    active_resident_bytes: u64,
    requested_bytes: u64,
    policy: &MemoryBudgetPolicy,
) -> AlgorithmAdmissionDecision {
    if requested_bytes > policy.per_algorithm_cap_bytes {
        return AlgorithmAdmissionDecision::Refuse(MemoryBudgetRefusal {
            code: ALGORITHM_MEMORY_CAP_CODE,
            severity: "warning",
            message: "requested algorithm working-set exceeds the per-algorithm cap; refused before allocating",
            repair: "raise graph.memory.per_algorithm_cap_mb or use a sampled algorithm",
            observed_bytes: requested_bytes,
            limit_bytes: policy.per_algorithm_cap_bytes,
        });
    }
    let combined_bytes = active_resident_bytes.saturating_add(requested_bytes);
    if combined_bytes > policy.snapshot_cap_bytes {
        return AlgorithmAdmissionDecision::Refuse(MemoryBudgetRefusal {
            code: MEMORY_PRESSURE_CODE,
            severity: "warning",
            message: "active snapshots plus the requested algorithm would exceed the snapshot cap; refused before allocating",
            repair: "wait for an active snapshot to release, raise graph.memory.snapshot_cap_mb, or skip the algorithm",
            observed_bytes: combined_bytes,
            limit_bytes: policy.snapshot_cap_bytes,
        });
    }
    AlgorithmAdmissionDecision::Admit { combined_bytes }
}

fn scale_by_basis_points(bytes: u64, basis_points: u32) -> u64 {
    let basis_points = u64::from(basis_points);
    bytes
        .saturating_mul(basis_points)
        .checked_div(10_000)
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mb(megabytes: u64) -> u64 {
        megabytes * 1024 * 1024
    }

    #[test]
    fn estimate_uses_documented_per_node_and_per_edge_constants() {
        let estimate = estimate_snapshot_bytes(10, 25);
        assert_eq!(estimate, 10 * PER_NODE_BYTES + 25 * PER_EDGE_BYTES);
    }

    /// `estimate_snapshot_bytes` must saturate on extreme inputs
    /// rather than wrap. A wrap-to-small result would let an
    /// attacker-controlled workspace bypass the cap.
    #[test]
    fn estimate_saturates_at_u64_max_on_extreme_inputs() {
        let extreme = estimate_snapshot_bytes(usize::MAX, usize::MAX);
        assert_eq!(extreme, u64::MAX);
    }

    /// Estimate under the cap admits with positive headroom and
    /// `approaching_cap` off when below the 80% threshold.
    #[test]
    fn snapshot_under_cap_admits_with_headroom() {
        let policy = MemoryBudgetPolicy::defaults();
        let decision = check_snapshot_admission(mb(50), &policy);
        let SnapshotAdmissionDecision::Admit {
            estimate_bytes,
            headroom_bytes,
            approaching_cap,
        } = decision
        else {
            panic!("expected Admit");
        };
        assert_eq!(estimate_bytes, mb(50));
        assert_eq!(headroom_bytes, mb(200));
        assert!(!approaching_cap);
    }

    /// Estimate past the 80% threshold but under the cap admits
    /// with `approaching_cap = true` so the caller can fire the
    /// advisory `snapshot_approaching_cap` code.
    #[test]
    fn snapshot_past_threshold_admits_with_approaching_cap_flag() {
        let policy = MemoryBudgetPolicy::defaults();
        // 200 MB = 80% of 250 MB.
        let decision = check_snapshot_admission(mb(200), &policy);
        let SnapshotAdmissionDecision::Admit {
            approaching_cap, ..
        } = decision
        else {
            panic!("expected Admit");
        };
        assert!(approaching_cap, "200 MB hits the 80% threshold");
    }

    /// Estimate over the cap refuses with the documented code +
    /// observed/limit byte fields populated.
    #[test]
    fn snapshot_over_cap_refuses_with_large_graph_uncached_code() {
        let policy = MemoryBudgetPolicy::defaults();
        let decision = check_snapshot_admission(mb(300), &policy);
        let SnapshotAdmissionDecision::Refuse(refusal) = decision else {
            panic!("expected Refuse");
        };
        assert_eq!(refusal.code, LARGE_GRAPH_UNCACHED_CODE);
        assert_eq!(refusal.severity, "high");
        assert_eq!(refusal.observed_bytes, mb(300));
        assert_eq!(refusal.limit_bytes, mb(250));
    }

    /// In-build growth within 1.5× of the pre-build estimate
    /// continues; growth past triggers the abort.
    #[test]
    fn in_build_growth_within_tripwire_continues() {
        let policy = MemoryBudgetPolicy::defaults();
        let decision = check_in_build_growth(mb(100), mb(140), &policy);
        let InBuildGrowthDecision::Continue {
            observed_bytes,
            allowed_bytes,
        } = decision
        else {
            panic!("expected Continue");
        };
        assert_eq!(observed_bytes, mb(140));
        // 1.5 * 100 = 150 MB allowed.
        assert_eq!(allowed_bytes, mb(150));
    }

    #[test]
    fn in_build_growth_past_tripwire_aborts_with_unexpected_growth_code() {
        let policy = MemoryBudgetPolicy::defaults();
        let decision = check_in_build_growth(mb(100), mb(160), &policy);
        let InBuildGrowthDecision::Abort(refusal) = decision else {
            panic!("expected Abort");
        };
        assert_eq!(refusal.code, UNEXPECTED_GROWTH_CODE);
        assert_eq!(refusal.observed_bytes, mb(160));
        assert_eq!(refusal.limit_bytes, mb(150));
    }

    /// Per-algorithm cap fires *before* the combined snapshot
    /// check so the operator sees the right remediation
    /// (`per_algorithm_cap_mb`, not `snapshot_cap_mb`).
    #[test]
    fn algorithm_admission_refuses_per_algorithm_cap_before_combined_check() {
        let policy = MemoryBudgetPolicy::defaults();
        // 150 MB requested, per-algo cap is 100 MB. Active is 0
        // so the combined check would pass on its own.
        let decision = check_algorithm_admission(0, mb(150), &policy);
        let AlgorithmAdmissionDecision::Refuse(refusal) = decision else {
            panic!("expected Refuse");
        };
        assert_eq!(refusal.code, ALGORITHM_MEMORY_CAP_CODE);
        assert_eq!(refusal.observed_bytes, mb(150));
        assert_eq!(refusal.limit_bytes, mb(100));
    }

    /// Combined snapshot cap fires when active + requested would
    /// breach `snapshot_cap_bytes`, even when each individually
    /// is within their own cap.
    #[test]
    fn algorithm_admission_refuses_combined_pressure_when_total_breaches_cap() {
        let policy = MemoryBudgetPolicy::defaults();
        // Active 200 MB + requested 80 MB = 280 MB > 250 MB cap.
        // Requested alone is under the 100 MB per-algo cap, so the
        // per-algo check must NOT fire first.
        let decision = check_algorithm_admission(mb(200), mb(80), &policy);
        let AlgorithmAdmissionDecision::Refuse(refusal) = decision else {
            panic!("expected Refuse");
        };
        assert_eq!(refusal.code, MEMORY_PRESSURE_CODE);
        assert_eq!(refusal.observed_bytes, mb(280));
        assert_eq!(refusal.limit_bytes, mb(250));
    }

    /// Admit: active + requested both within their caps and within
    /// the combined budget.
    #[test]
    fn algorithm_admission_admits_when_within_all_caps() {
        let policy = MemoryBudgetPolicy::defaults();
        let decision = check_algorithm_admission(mb(80), mb(50), &policy);
        let AlgorithmAdmissionDecision::Admit { combined_bytes } = decision else {
            panic!("expected Admit");
        };
        assert_eq!(combined_bytes, mb(130));
    }

    /// Worst-case integration: 100k-memory fixture estimated via
    /// `estimate_snapshot_bytes` triggers the
    /// `large_graph_uncached` refusal (the bead's headline
    /// acceptance: "100k-memory fixture triggers graceful
    /// degradation, NOT OOM").
    #[test]
    fn one_hundred_k_memory_estimate_triggers_graceful_refusal_not_oom() {
        let policy = MemoryBudgetPolicy::defaults();
        // 100k memories with ~5 typed edges per memory.
        let estimate = estimate_snapshot_bytes(100_000, 500_000);
        // 100k * 32 = 3.2 MB nodes; 500k * 96 = 48 MB edges = ~51 MB.
        // That's still under the 250 MB cap, so admission succeeds.
        // The bead's "100k triggers" case is at higher
        // density: 100k * 200 edges/memory.
        let SnapshotAdmissionDecision::Admit { .. } = check_snapshot_admission(estimate, &policy)
        else {
            panic!("light density should still admit");
        };
        let dense_estimate = estimate_snapshot_bytes(100_000, 3_000_000);
        let SnapshotAdmissionDecision::Refuse(refusal) =
            check_snapshot_admission(dense_estimate, &policy)
        else {
            panic!("dense 100k fixture must refuse");
        };
        assert_eq!(refusal.code, LARGE_GRAPH_UNCACHED_CODE);
    }

    /// Every refusal carries a non-empty `repair` hint so the
    /// operator always sees a remediation action in the
    /// `degraded[]` row.
    #[test]
    fn every_refusal_variant_carries_a_non_empty_repair_hint() {
        let policy = MemoryBudgetPolicy::defaults();
        let snapshot_refusal = match check_snapshot_admission(mb(1000), &policy) {
            SnapshotAdmissionDecision::Refuse(r) => r,
            other => panic!("expected Refuse, got {other:?}"),
        };
        let growth_refusal = match check_in_build_growth(mb(100), mb(500), &policy) {
            InBuildGrowthDecision::Abort(r) => r,
            other => panic!("expected Abort, got {other:?}"),
        };
        let algo_cap_refusal = match check_algorithm_admission(0, mb(500), &policy) {
            AlgorithmAdmissionDecision::Refuse(r) => r,
            other => panic!("expected Refuse, got {other:?}"),
        };
        let pressure_refusal = match check_algorithm_admission(mb(240), mb(20), &policy) {
            AlgorithmAdmissionDecision::Refuse(r) => r,
            other => panic!("expected Refuse, got {other:?}"),
        };
        for refusal in [
            snapshot_refusal,
            growth_refusal,
            algo_cap_refusal,
            pressure_refusal,
        ] {
            assert!(!refusal.repair.is_empty(), "repair hint must be present");
            assert!(!refusal.message.is_empty(), "message must be present");
            assert!(refusal.observed_bytes > 0);
            assert!(refusal.limit_bytes > 0);
        }
    }

    /// Determinism: same inputs → byte-stable JSON across runs.
    #[test]
    fn decisions_serialize_byte_stable_across_runs() {
        let policy = MemoryBudgetPolicy::defaults();
        let first = check_snapshot_admission(mb(50), &policy);
        let second = check_snapshot_admission(mb(50), &policy);
        let json_first = serde_json::to_string(&first).expect("serialize");
        let json_second = serde_json::to_string(&second).expect("serialize");
        assert_eq!(json_first, json_second);
    }
}
