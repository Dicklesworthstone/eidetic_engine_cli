#![no_main]

//! Fuzz target for `ee::core::graph_memory_budget` admission decisions.
//!
//! Drives arbitrary policies, node/edge counts, observed-bytes, and
//! algorithm working-set sizes through the three decision helpers
//! shipped in commit `c3bf406` and asserts every invariant the
//! module promises:
//!
//! 1. **Panic-freedom + saturation.** `estimate_snapshot_bytes`,
//!    `check_snapshot_admission`, `check_in_build_growth`, and
//!    `check_algorithm_admission` must never panic on any input,
//!    including `usize::MAX` and `u64::MAX` extremes. The
//!    estimation uses saturating arithmetic so wrap-around can't
//!    silently turn a giant workspace into an admit decision.
//! 2. **Admit ↔ Refuse invariant for snapshot admission.** Every
//!    `Admit` carries `estimate_bytes ≤ snapshot_cap_bytes` and
//!    `headroom_bytes = snapshot_cap_bytes - estimate_bytes`.
//!    Every `Refuse` carries `observed_bytes > limit_bytes` and a
//!    non-empty `repair` hint with the documented
//!    `large_graph_uncached` code.
//! 3. **Advisory threshold consistency.** When `Admit` sets
//!    `approaching_cap = true`, the estimate is ≥ the advisory
//!    threshold computed from `snapshot_cap_bytes` and
//!    `degraded_below_pct`; when `approaching_cap = false`, the
//!    estimate is strictly below it.
//! 4. **Growth tripwire correctness.** `Continue` requires
//!    `observed_bytes ≤ allowed_bytes`; `Abort` requires
//!    `observed_bytes > allowed_bytes` and emits
//!    `unexpected_growth` with a non-empty repair hint.
//! 5. **Refusal ordering for algorithm admission.** When
//!    requested working-set exceeds the per-algorithm cap, the
//!    refusal must use `algorithm_memory_cap` regardless of the
//!    combined total. When requested working-set is within the
//!    per-algorithm cap but combined would breach the snapshot
//!    cap, the refusal must use `memory_pressure`. Both refusals
//!    carry observed/limit bytes plus a non-empty repair hint.
//! 6. **Determinism.** All four helpers produce byte-identical
//!    JSON across two consecutive calls with the same input.
//!
//! Inputs are interpreted as a small fixed-width header plus tail
//! payload. Fields use saturating constructors so any byte
//! sequence yields a valid policy + workload pair.

use libfuzzer_sys::fuzz_target;

use ee::core::graph_memory_budget::{
    ALGORITHM_MEMORY_CAP_CODE, AlgorithmAdmissionDecision, InBuildGrowthDecision,
    LARGE_GRAPH_UNCACHED_CODE, MEMORY_PRESSURE_CODE, MemoryBudgetPolicy, SnapshotAdmissionDecision,
    UNEXPECTED_GROWTH_CODE, check_algorithm_admission, check_in_build_growth,
    check_snapshot_admission, estimate_snapshot_bytes,
};

const MAX_INPUT_BYTES: usize = 256;

fn read_u64(bytes: &[u8]) -> u64 {
    let mut buf = [0u8; 8];
    let len = bytes.len().min(8);
    buf[..len].copy_from_slice(&bytes[..len]);
    u64::from_le_bytes(buf)
}

fn read_u32(bytes: &[u8]) -> u32 {
    let mut buf = [0u8; 4];
    let len = bytes.len().min(4);
    buf[..len].copy_from_slice(&bytes[..len]);
    u32::from_le_bytes(buf)
}

fn read_usize(bytes: &[u8]) -> usize {
    let mut buf = [0u8; 8];
    let len = bytes.len().min(8);
    buf[..len].copy_from_slice(&bytes[..len]);
    u64::from_le_bytes(buf) as usize
}

fn scale_by_basis_points(bytes: u64, basis_points: u32) -> u64 {
    let basis_points = u64::from(basis_points);
    bytes
        .saturating_mul(basis_points)
        .checked_div(10_000)
        .unwrap_or(u64::MAX)
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    // Pad input so we can index without bounds checks. Trailing
    // bytes that don't fit a field are treated as zero.
    let mut padded = [0u8; 64];
    let copy_len = data.len().min(padded.len());
    padded[..copy_len].copy_from_slice(&data[..copy_len]);

    // Header fields shape the policy + workload. Reads are
    // saturating: any byte sequence yields a valid configuration.
    let snapshot_cap_bytes = read_u64(&padded[0..8]).max(1);
    let per_algorithm_cap_bytes = read_u64(&padded[8..16]).max(1);
    let degraded_below_pct = padded[16] % 101;
    let growth_multiplier_basis_points =
        10_000u32.saturating_add(read_u32(&padded[17..21]) % 50_000);
    let node_count = read_usize(&padded[21..29]);
    let edge_count = read_usize(&padded[29..37]);
    let observed_growth_bytes = read_u64(&padded[37..45]);
    let active_resident_bytes = read_u64(&padded[45..53]);
    let requested_algorithm_bytes = read_u64(&padded[53..61]);

    let policy = MemoryBudgetPolicy {
        snapshot_cap_bytes,
        per_algorithm_cap_bytes,
        degraded_below_pct,
        growth_multiplier_basis_points,
    };

    // Invariant 1: estimate must not panic / wrap.
    let estimate = estimate_snapshot_bytes(node_count, edge_count);

    // Invariant 1 + 2: snapshot admission decision.
    let snapshot_decision = check_snapshot_admission(estimate, &policy);
    match snapshot_decision {
        SnapshotAdmissionDecision::Admit {
            estimate_bytes,
            headroom_bytes,
            approaching_cap,
        } => {
            assert_eq!(estimate_bytes, estimate);
            assert!(
                estimate_bytes <= policy.snapshot_cap_bytes,
                "Admit must carry estimate ≤ cap, got {} vs {}",
                estimate_bytes,
                policy.snapshot_cap_bytes
            );
            assert_eq!(
                headroom_bytes,
                policy.snapshot_cap_bytes - estimate_bytes,
                "headroom must equal cap - estimate"
            );
            let advisory = scale_by_basis_points(
                policy.snapshot_cap_bytes,
                u32::from(policy.degraded_below_pct) * 100,
            );
            if approaching_cap {
                assert!(
                    estimate_bytes >= advisory,
                    "approaching_cap=true implies estimate {} ≥ advisory threshold {}",
                    estimate_bytes,
                    advisory
                );
            } else {
                assert!(
                    estimate_bytes < advisory,
                    "approaching_cap=false implies estimate {} < advisory threshold {}",
                    estimate_bytes,
                    advisory
                );
            }
        }
        SnapshotAdmissionDecision::Refuse(refusal) => {
            assert_eq!(refusal.code, LARGE_GRAPH_UNCACHED_CODE);
            assert!(
                refusal.observed_bytes > refusal.limit_bytes,
                "Refuse must carry observed > limit"
            );
            assert_eq!(refusal.observed_bytes, estimate);
            assert_eq!(refusal.limit_bytes, policy.snapshot_cap_bytes);
            assert!(!refusal.repair.is_empty());
            assert!(!refusal.message.is_empty());
        }
    }

    // Invariant 4: in-build growth tripwire.
    let growth_decision = check_in_build_growth(estimate, observed_growth_bytes, &policy);
    let expected_allowed = scale_by_basis_points(estimate, policy.growth_multiplier_basis_points);
    match growth_decision {
        InBuildGrowthDecision::Continue {
            observed_bytes,
            allowed_bytes,
        } => {
            assert_eq!(observed_bytes, observed_growth_bytes);
            assert_eq!(allowed_bytes, expected_allowed);
            assert!(
                observed_bytes <= allowed_bytes,
                "Continue must carry observed ≤ allowed"
            );
        }
        InBuildGrowthDecision::Abort(refusal) => {
            assert_eq!(refusal.code, UNEXPECTED_GROWTH_CODE);
            assert!(
                refusal.observed_bytes > refusal.limit_bytes,
                "Abort must carry observed > limit"
            );
            assert_eq!(refusal.observed_bytes, observed_growth_bytes);
            assert_eq!(refusal.limit_bytes, expected_allowed);
            assert!(!refusal.repair.is_empty());
            assert!(!refusal.message.is_empty());
        }
    }

    // Invariant 5: algorithm admission ordering.
    let algorithm_decision =
        check_algorithm_admission(active_resident_bytes, requested_algorithm_bytes, &policy);
    match algorithm_decision {
        AlgorithmAdmissionDecision::Admit { combined_bytes } => {
            assert_eq!(
                combined_bytes,
                active_resident_bytes.saturating_add(requested_algorithm_bytes)
            );
            assert!(
                requested_algorithm_bytes <= policy.per_algorithm_cap_bytes,
                "Admit must imply per-algo cap respected"
            );
            assert!(
                combined_bytes <= policy.snapshot_cap_bytes,
                "Admit must imply combined ≤ snapshot cap"
            );
        }
        AlgorithmAdmissionDecision::Refuse(refusal) => {
            // The per-algorithm cap fires FIRST: if requested
            // exceeds the per-algo cap, the code MUST be
            // algorithm_memory_cap (never memory_pressure), even
            // when the combined check would also fire.
            if requested_algorithm_bytes > policy.per_algorithm_cap_bytes {
                assert_eq!(
                    refusal.code, ALGORITHM_MEMORY_CAP_CODE,
                    "per-algo cap exceedance must surface as algorithm_memory_cap"
                );
                assert_eq!(refusal.observed_bytes, requested_algorithm_bytes);
                assert_eq!(refusal.limit_bytes, policy.per_algorithm_cap_bytes);
            } else {
                assert_eq!(refusal.code, MEMORY_PRESSURE_CODE);
                let combined = active_resident_bytes.saturating_add(requested_algorithm_bytes);
                assert_eq!(refusal.observed_bytes, combined);
                assert_eq!(refusal.limit_bytes, policy.snapshot_cap_bytes);
                assert!(combined > policy.snapshot_cap_bytes);
            }
            assert!(!refusal.repair.is_empty());
            assert!(!refusal.message.is_empty());
            assert!(
                refusal.observed_bytes > refusal.limit_bytes,
                "every refusal must carry observed > limit"
            );
        }
    }

    // Invariant 6: determinism. Same inputs → byte-identical JSON
    // for every helper. We compare aggregated tuples in one
    // serialization to keep the assertion compact.
    let combined_first = (
        estimate,
        check_snapshot_admission(estimate, &policy),
        check_in_build_growth(estimate, observed_growth_bytes, &policy),
        check_algorithm_admission(active_resident_bytes, requested_algorithm_bytes, &policy),
    );
    let combined_second = (
        estimate_snapshot_bytes(node_count, edge_count),
        check_snapshot_admission(estimate, &policy),
        check_in_build_growth(estimate, observed_growth_bytes, &policy),
        check_algorithm_admission(active_resident_bytes, requested_algorithm_bytes, &policy),
    );
    let json_first = serde_json::to_string(&combined_first).expect("first serialize");
    let json_second = serde_json::to_string(&combined_second).expect("second serialize");
    assert_eq!(
        json_first, json_second,
        "graph_memory_budget decisions are not deterministic"
    );
});
