//! N7.1 (bd-17c65.14.7.2 / ADR 0032) — harmful-weight asymmetry.
//!
//! ADR 0032's verification contract names this file explicitly:
//!
//! > `tests/bayesian_harmful_asymmetry_unit.rs` — harmful event adds
//! > `harmful_weight` to beta, helpful adds 1 to alpha.
//!
//! The asymmetry (`DEFAULT_HARMFUL_WEIGHT = 2.5`, helpful = 1.0) is a
//! load-bearing decision: the README-documented harmful-bias rule
//! makes harmful evidence count for 2.5× as much as helpful evidence
//! per event, encoding a precautionary stance toward memories that
//! demonstrably mislead. ADR 0032 preserves the existing scalar
//! `harmful_weight` from the legacy linear-delta lifecycle.

use ee::core::bayes::{BetaPosterior, DEFAULT_HARMFUL_WEIGHT};

fn approx_eq(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() < tol
}

// ---------------------------------------------------------------------------
// Numeric pinning of the asymmetry constants
// ---------------------------------------------------------------------------

#[test]
fn default_harmful_weight_is_2_5() {
    // Pinned by README and the legacy linear-delta system. If this
    // changes, every downstream calibration shifts and ADR 0032 must
    // be amended.
    assert!(approx_eq(DEFAULT_HARMFUL_WEIGHT, 2.5, 1e-12));
}

// ---------------------------------------------------------------------------
// Helpful events add exactly 1.0 to alpha
// ---------------------------------------------------------------------------

#[test]
fn helpful_event_adds_exactly_one_to_alpha() {
    let p0 = BetaPosterior::jeffreys();
    let p1 = p0.update_helpful();
    assert!(approx_eq(p1.alpha(), p0.alpha() + 1.0, 1e-12));
    assert!(approx_eq(p1.beta(), p0.beta(), 1e-12));
}

#[test]
fn n_helpful_events_add_exactly_n_to_alpha() {
    let mut p = BetaPosterior::jeffreys();
    let starting_alpha = p.alpha();
    let starting_beta = p.beta();
    for _ in 0..17 {
        p = p.update_helpful();
    }
    assert!(approx_eq(p.alpha(), starting_alpha + 17.0, 1e-12));
    assert!(approx_eq(p.beta(), starting_beta, 1e-12));
}

// ---------------------------------------------------------------------------
// Harmful events add exactly `harmful_weight` to beta
// ---------------------------------------------------------------------------

#[test]
fn harmful_event_at_default_weight_adds_2_5_to_beta() {
    let p0 = BetaPosterior::jeffreys();
    let p1 = p0.update_harmful(DEFAULT_HARMFUL_WEIGHT);
    assert!(approx_eq(p1.alpha(), p0.alpha(), 1e-12));
    assert!(approx_eq(p1.beta(), p0.beta() + 2.5, 1e-12));
}

#[test]
fn harmful_event_at_custom_weight_adds_that_weight_to_beta() {
    // The harmful_weight is parameterized — operators can override
    // it via config to tune precaution. The math must respect any
    // positive weight.
    for &w in &[0.5_f64, 1.0, 1.5, 2.5, 5.0, 10.0] {
        let p0 = BetaPosterior::jeffreys();
        let p1 = p0.update_harmful(w);
        assert!(
            approx_eq(p1.beta(), p0.beta() + w, 1e-12),
            "harmful weight {w} should add exactly {w} to beta"
        );
        assert!(
            approx_eq(p1.alpha(), p0.alpha(), 1e-12),
            "harmful weight must never touch alpha"
        );
    }
}

#[test]
fn n_harmful_events_at_default_weight_accumulate_linearly() {
    let mut p = BetaPosterior::jeffreys();
    let starting_alpha = p.alpha();
    let starting_beta = p.beta();
    for _ in 0..11 {
        p = p.update_harmful(DEFAULT_HARMFUL_WEIGHT);
    }
    assert!(approx_eq(p.alpha(), starting_alpha, 1e-12));
    assert!(
        approx_eq(
            p.beta(),
            starting_beta + 11.0 * DEFAULT_HARMFUL_WEIGHT,
            1e-12
        ),
        "11 harmful events at weight 2.5 must add exactly 27.5 to beta"
    );
}

// ---------------------------------------------------------------------------
// Non-positive / non-finite harmful weights are clamped (no-op)
// ---------------------------------------------------------------------------

#[test]
fn non_positive_harmful_weight_is_clamped_to_zero() {
    // ADR 0032 / N7.1 invariant: alpha and beta must stay strictly
    // positive. A zero or negative harmful_weight cannot reduce beta
    // (that would corrupt the posterior); it must be silently
    // clamped to zero.
    let p0 = BetaPosterior::jeffreys();
    for &w in &[0.0_f64, -1.0, -100.0, f64::NEG_INFINITY] {
        let p1 = p0.update_harmful(w);
        assert!(
            approx_eq(p1.beta(), p0.beta(), 1e-12),
            "non-positive weight {w} must be a no-op on beta"
        );
        assert!(
            approx_eq(p1.alpha(), p0.alpha(), 1e-12),
            "harmful update must never touch alpha"
        );
    }
}

#[test]
fn nan_harmful_weight_is_clamped_to_zero() {
    let p0 = BetaPosterior::jeffreys();
    let p1 = p0.update_harmful(f64::NAN);
    assert!(approx_eq(p1.beta(), p0.beta(), 1e-12));
    assert!(approx_eq(p1.alpha(), p0.alpha(), 1e-12));
}

#[test]
fn positive_infinity_harmful_weight_is_clamped_to_zero() {
    // PosInf is non-finite, so the safety clamp kicks in.
    let p0 = BetaPosterior::jeffreys();
    let p1 = p0.update_harmful(f64::INFINITY);
    assert!(approx_eq(p1.beta(), p0.beta(), 1e-12));
    assert!(approx_eq(p1.alpha(), p0.alpha(), 1e-12));
}

// ---------------------------------------------------------------------------
// Asymmetry behavior at the posterior-mean level
// ---------------------------------------------------------------------------

#[test]
fn one_harmful_event_at_default_weight_outweighs_two_helpful_events() {
    // Concrete consequence of the 2.5× harmful weight: a single
    // harmful event drops the posterior mean more than two helpful
    // events raise it. This is the precautionary intent of the
    // asymmetry.
    let only_helpful = BetaPosterior::jeffreys().update_helpful().update_helpful();
    let only_harmful = BetaPosterior::jeffreys().update_harmful(DEFAULT_HARMFUL_WEIGHT);

    let baseline = BetaPosterior::jeffreys();
    let helpful_delta_up = only_helpful.mean() - baseline.mean();
    let harmful_delta_down = baseline.mean() - only_harmful.mean();

    assert!(
        harmful_delta_down > helpful_delta_up,
        "single harmful event must drop mean more than two helpful events raise it: \
         helpful_delta_up={helpful_delta_up}, harmful_delta_down={harmful_delta_down}"
    );
}

#[test]
fn equal_event_counts_yield_lower_mean_than_jeffreys() {
    // 5 helpful + 5 harmful from Jeffreys: alpha=5.5, beta=13.0,
    // mean ≈ 0.297. This is BELOW 0.5 — the harmful events bias the
    // posterior toward "untrustworthy" by virtue of their 2.5×
    // weight. Documented as a feature, not a bug, of the model.
    let mut p = BetaPosterior::jeffreys();
    for _ in 0..5 {
        p = p.update_helpful();
    }
    for _ in 0..5 {
        p = p.update_harmful(DEFAULT_HARMFUL_WEIGHT);
    }
    assert!(approx_eq(p.alpha(), 5.5, 1e-12));
    assert!(approx_eq(p.beta(), 13.0, 1e-12));
    assert!(
        p.mean() < 0.5,
        "5 helpful + 5 harmful events should yield mean < 0.5 due to 2.5× harmful weight; got {}",
        p.mean()
    );
    // Tight numeric pin: 5.5 / 18.5 = 0.297297...
    assert!(approx_eq(p.mean(), 5.5 / 18.5, 1e-10));
}
