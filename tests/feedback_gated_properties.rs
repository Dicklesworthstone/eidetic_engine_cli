//! Property / contract tests for the feedback-gated learning cores
//! (bd-1n0np.13.1 Token ROI + 13.2 Regime-Shift; advances 13.4) over the landed
//! pub logic in `ee::core::outcome`.
//!
//! The in-module tests cover a few point cases; these lock the load-bearing
//! invariants more broadly:
//! - Token ROI is conservative (Wilson lower bound never exceeds the point hit
//!   rate; thin lucky hits cannot out-rank dense evidence), deterministic
//!   (stable `table_hash`), ranked by utility, and divide-by-zero-safe;
//! - Regime-shift detection only proposes (never auto-demotes), keys solely off
//!   the TRAILING window so a flipped rule is caught despite a long helpful
//!   history, stays quiet on recovery and on thin data, and is deterministic.

use ee::core::outcome::{TokenRoiBucketInput, compute_token_roi, detect_regime_shift};
use ee::core::sprt::SprtObservation;

fn bucket(key: &str, helpful: u32, total: u32, tokens: u64) -> TokenRoiBucketInput {
    TokenRoiBucketInput {
        key: key.to_string(),
        helpful_count: helpful,
        total_count: total,
        total_tokens: tokens,
    }
}

#[test]
fn token_roi_is_deterministic_with_a_stable_table_hash() {
    let inputs = vec![
        bucket("kind:rule", 40, 50, 5_000),
        bucket("kind:note", 5, 30, 9_000),
    ];
    let first = compute_token_roi(&inputs, 10);
    let second = compute_token_roi(&inputs, 10);
    assert_eq!(first, second, "same inputs must yield an identical report");
    assert!(
        first.table_hash.starts_with("blake3:"),
        "table_hash pins determinism"
    );
    assert_eq!(first.schema, "ee.token_roi.v1");
    assert_eq!(first.bucket_count, 2);
}

#[test]
fn token_roi_ranks_by_utility_per_1k_tokens_descending_with_key_tiebreak() {
    let inputs = vec![
        bucket("z_low", 1, 50, 50_000),
        bucket("a_high", 49, 50, 1_000),
        bucket("m_mid", 25, 50, 5_000),
    ];
    let report = compute_token_roi(&inputs, 10);
    for pair in report.buckets.windows(2) {
        let a = &pair[0];
        let b = &pair[1];
        assert!(
            a.utility_per_1k_tokens > b.utility_per_1k_tokens
                || (a.utility_per_1k_tokens == b.utility_per_1k_tokens && a.key <= b.key),
            "buckets must be ranked by utility desc, ties broken by key"
        );
    }
}

#[test]
fn token_roi_is_conservative_thin_lucky_hits_cannot_out_rank_dense_evidence() {
    // Two buckets at a perfect point hit-rate (1.0): one thin (1/1), one dense
    // (100/100). The Wilson lower bound must rank the dense one strictly higher.
    let thin = compute_token_roi(&[bucket("thin", 1, 1, 1)], 10);
    let dense = compute_token_roi(&[bucket("dense", 100, 100, 1)], 10);
    let thin_b = &thin.buckets[0];
    let dense_b = &dense.buckets[0];
    assert_eq!(thin_b.hit_rate, 1.0);
    assert_eq!(dense_b.hit_rate, 1.0);
    assert!(
        thin_b.conservative_hit_rate < dense_b.conservative_hit_rate,
        "Wilson lower bound must penalize thin evidence: thin {} vs dense {}",
        thin_b.conservative_hit_rate,
        dense_b.conservative_hit_rate
    );
    // The conservative rate is never above the point estimate, for any bucket.
    for b in thin.buckets.iter().chain(dense.buckets.iter()) {
        assert!(b.conservative_hit_rate <= b.hit_rate + 1e-9);
    }
}

#[test]
fn token_roi_flags_low_sample_buckets_and_is_divide_by_zero_safe() {
    let report = compute_token_roi(
        &[
            bucket("sparse", 2, 3, 0), // below min_samples + zero tokens
            bucket("dense", 80, 100, 10_000),
        ],
        10,
    );
    let sparse = report.buckets.iter().find(|b| b.key == "sparse").unwrap();
    let dense = report.buckets.iter().find(|b| b.key == "dense").unwrap();
    assert!(sparse.abstained, "below-min-samples bucket is abstained");
    assert!(!dense.abstained, "dense bucket is not abstained");
    assert_eq!(
        sparse.utility_per_1k_tokens, 0.0,
        "zero tokens must not divide by zero"
    );
    // Empty input is a valid empty report.
    let empty = compute_token_roi(&[], 10);
    assert_eq!(empty.bucket_count, 0);
    assert!(empty.buckets.is_empty());
}

fn obs(n_helpful: usize, n_harmful: usize) -> Vec<SprtObservation> {
    let mut v = vec![SprtObservation::Helpful; n_helpful];
    v.extend(std::iter::repeat_n(SprtObservation::Harmful, n_harmful));
    v
}

#[test]
fn regime_shift_proposes_on_a_flip_despite_a_long_helpful_history() {
    // A rule that was helpful for a long time then flips harmful after an upgrade.
    // The trailing window is harmful-dominant, so a demotion is proposed even
    // though the full history is mostly helpful (trailing-only, no stale masking).
    let outcomes = obs(40, 20);
    let proposal = detect_regime_shift("mem_flip", &outcomes, 20);
    assert!(
        proposal.proposed_demotion,
        "a harmful trailing regime must propose a demotion (decision={}, harmful={}/{})",
        proposal.decision, proposal.trailing_harmful, proposal.trailing_event_count
    );
}

#[test]
fn regime_shift_stays_quiet_after_recovery() {
    // Harmful history but a recovered (helpful) trailing window -> no proposal.
    let outcomes = obs(0, 20).into_iter().chain(obs(20, 0)).collect::<Vec<_>>();
    let proposal = detect_regime_shift("mem_recovered", &outcomes, 20);
    assert!(
        !proposal.proposed_demotion,
        "a recovered trailing regime must stay quiet (decision={})",
        proposal.decision
    );
}

#[test]
fn regime_shift_stays_quiet_on_thin_windows_and_only_proposes_never_mutates() {
    // Thin data: a couple harmful events is below the SPRT threshold -> quiet.
    let proposal = detect_regime_shift("mem_thin", &obs(0, 2), 20);
    assert!(
        !proposal.proposed_demotion,
        "thin windows must not propose (decision={})",
        proposal.decision
    );
    assert_eq!(proposal.memory_id, "mem_thin");
    // Determinism: same input, same proposal.
    let again = detect_regime_shift("mem_thin", &obs(0, 2), 20);
    assert_eq!(proposal, again);
}

#[test]
fn regime_shift_keys_only_off_the_trailing_window() {
    // Same recent regime (20 harmful) with vs. without a long helpful prefix must
    // reach the same proposal — the prefix beyond the window is ignored.
    let with_prefix = detect_regime_shift("m", &obs(100, 20), 20);
    let without_prefix = detect_regime_shift("m", &obs(0, 20), 20);
    assert_eq!(
        with_prefix.proposed_demotion, without_prefix.proposed_demotion,
        "only the trailing window decides; the helpful prefix must not mask the flip"
    );
    assert!(with_prefix.proposed_demotion);
}
