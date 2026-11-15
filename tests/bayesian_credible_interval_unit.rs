//! N7.1 (bd-17c65.14.7.2 / ADR 0032) — credible-interval properties.
//!
//! ADR 0032's verification contract names this file explicitly:
//!
//! > `tests/bayesian_credible_interval_unit.rs` — nominal 90 percent
//! > credible interval contains the true rate at calibrated frequency.
//!
//! These tests are pure math on [`ee::core::bayes::BetaPosterior`].
//! No DB or fs interaction; runs in milliseconds.

#![allow(clippy::expect_used)]

use ee::core::bayes::{
    BetaPosterior, DEFAULT_HARMFUL_WEIGHT, raw_bernoulli_rate_for_weighted_utility,
    weighted_utility_rate,
};

fn approx_eq(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() < tol
}

// ---------------------------------------------------------------------------
// Containment
// ---------------------------------------------------------------------------

#[test]
fn ci90_contains_posterior_mean() {
    // Trivial sanity property: the 90% CI must always contain the
    // posterior mean. If this ever fails, the inverse-CDF iteration
    // is broken.
    for helpful in [0_usize, 1, 5, 10, 100] {
        let mut p = BetaPosterior::jeffreys();
        for _ in 0..helpful {
            p = p.update_helpful();
        }
        let mean = p.mean();
        let (lo, hi) = p
            .credible_interval(0.90)
            .expect("90% CI should converge for any helpful count");
        assert!(
            lo <= mean && mean <= hi,
            "90% CI [{lo}, {hi}] must contain mean {mean} (helpful={helpful})"
        );
    }
}

#[test]
fn ci50_contained_in_ci90() {
    // Narrower (50%) CI must lie inside the wider (90%) CI for any
    // posterior. Equivalent to the monotonicity of the quantile
    // function in the confidence level.
    let mut p = BetaPosterior::jeffreys();
    for _ in 0..7 {
        p = p.update_helpful();
    }
    let (lo90, hi90) = p.credible_interval(0.90).expect("90% CI");
    let (lo50, hi50) = p.credible_interval(0.50).expect("50% CI");
    assert!(lo90 < lo50, "ci90 lo must be below ci50 lo");
    assert!(hi50 < hi90, "ci50 hi must be below ci90 hi");
    assert!(lo50 < hi50, "ci50 lo must be below ci50 hi");
    assert!(lo90 < hi90, "ci90 lo must be below ci90 hi");
}

// ---------------------------------------------------------------------------
// Narrowing with evidence
// ---------------------------------------------------------------------------

#[test]
fn ci_narrows_strictly_as_evidence_accumulates() {
    // Each helpful event grows the effective sample size; the
    // credible interval must monotonically tighten. This is the
    // sample-size-blindness property that ADR 0032 was filed to fix.
    let mut p = BetaPosterior::jeffreys();
    let mut last_width = f64::INFINITY;
    for _ in 0..20 {
        p = p.update_helpful();
        let (lo, hi) = p.credible_interval(0.90).expect("CI converges");
        let width = hi - lo;
        assert!(
            width < last_width,
            "ci90 width must strictly shrink: prev={last_width}, now={width}"
        );
        last_width = width;
    }
}

#[test]
fn ci_width_decreases_with_harmful_events_too() {
    // Sanity: ADR 0032 says effective_sample_size grows with EITHER
    // helpful or harmful events. The CI width therefore narrows in
    // both directions.
    let mut p = BetaPosterior::jeffreys();
    let mut last_width = f64::INFINITY;
    for _ in 0..10 {
        p = p.update_harmful(DEFAULT_HARMFUL_WEIGHT);
        let (lo, hi) = p.credible_interval(0.90).expect("CI converges");
        let width = hi - lo;
        assert!(width < last_width, "harmful events must also narrow CI");
        last_width = width;
    }
}

// ---------------------------------------------------------------------------
// Calibration — frequentist coverage of the credible interval
// ---------------------------------------------------------------------------
//
// Per ADR 0032: "nominal 90 percent credible interval contains the
// true rate at calibrated frequency". We synthesize N draws from a
// known true rate `p_true`, build a posterior from each draw, and
// count how often the 90% CI brackets `p_true`. With 1000 trials and
// nominal 0.90 coverage, the binomial standard error is sqrt(0.9 *
// 0.1 / 1000) ≈ 0.0095; a ±5 percentage-point tolerance (i.e. 85%
// to 95%) is well outside the noise floor.

fn lcg_next(state: u64) -> u64 {
    // Numerical Recipes LCG. Deterministic for test reproducibility.
    state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407)
}

fn lcg_unit(state: u64) -> f64 {
    // Top 53 bits as a uniform-[0, 1) double.
    (state >> 11) as f64 * 2.0_f64.powi(-53)
}

fn raw_success_probability_for_weighted_rate(weighted_rate: f64) -> f64 {
    raw_bernoulli_rate_for_weighted_utility(weighted_rate, DEFAULT_HARMFUL_WEIGHT)
        .expect("valid weighted rate in (0, 1)")
}

fn coverage_for(p_true: f64, n_per_trial: usize, trials: usize, seed: u64) -> f64 {
    let mut state = seed;
    let mut covered = 0_usize;
    let raw_success_probability = raw_success_probability_for_weighted_rate(p_true);
    for _ in 0..trials {
        let mut p = BetaPosterior::jeffreys();
        for _ in 0..n_per_trial {
            state = lcg_next(state);
            let u = lcg_unit(state);
            if u < raw_success_probability {
                p = p.update_helpful();
            } else {
                p = p.update_harmful(DEFAULT_HARMFUL_WEIGHT);
            }
        }
        if let Some((lo, hi)) = p.weighted_credible_interval(0.90, DEFAULT_HARMFUL_WEIGHT) {
            if lo <= p_true && p_true <= hi {
                covered += 1;
            }
        }
    }
    covered as f64 / trials as f64
}

#[test]
fn ci90_covers_at_calibrated_frequency_for_balanced_rate() {
    // p_true = 0.5 is the weighted utility rate mu_w = p / (p + w*(1 - p)).
    // Because harmful outcomes accumulate with weight w = 2.5, the stored
    // Beta(alpha, beta) distribution has effective sample size inflated on the
    // harmful side; `weighted_credible_interval(0.90, DEFAULT_HARMFUL_WEIGHT)`
    // descales the raw Bernoulli posterior and pushes its quantiles through the
    // monotone bijection `weighted_utility_rate` so nominal 90% coverage holds
    // within ±5pp without altering stored-Beta `credible_interval` semantics.
    let coverage = coverage_for(0.5, 40, 500, 0xfeed_face_d00d_cafe);
    assert!(
        (0.85..=0.95).contains(&coverage),
        "weighted ci90 coverage must stay within ±5pp of 0.90 at mu_w=0.5; got {coverage}"
    );
}

#[test]
fn ci90_covers_at_calibrated_frequency_for_high_rate() {
    let coverage = coverage_for(0.85, 40, 500, 0x00c0_ffee_dead_beef);
    assert!(
        (0.85..=0.95).contains(&coverage),
        "weighted ci90 coverage must stay within ±5pp of 0.90 at mu_w=0.85; got {coverage}"
    );
}

#[test]
fn weighted_utility_bijection_roundtrips_and_matches_stored_ci_at_unit_weight() {
    for rate in [0.1, 0.25, 0.5, 0.714_285_714_285_714_3, 0.85, 0.95] {
        let p_raw = raw_bernoulli_rate_for_weighted_utility(rate, DEFAULT_HARMFUL_WEIGHT)
            .expect("raw Bernoulli rate");
        let recovered =
            weighted_utility_rate(p_raw, DEFAULT_HARMFUL_WEIGHT).expect("weighted utility rate");
        assert!(
            approx_eq(rate, recovered, 1e-12),
            "bijection roundtrip drift for {rate}: got {recovered}"
        );
    }

    let posterior = BetaPosterior::new(12.5, 8.0).expect("valid posterior");
    let stored = posterior.credible_interval(0.90).expect("stored CI");
    let unit_weighted = posterior
        .weighted_credible_interval(0.90, 1.0)
        .expect("unit-weight CI");
    assert!(approx_eq(stored.0, unit_weighted.0, 1e-12));
    assert!(approx_eq(stored.1, unit_weighted.1, 1e-12));
}

// ---------------------------------------------------------------------------
// Symmetry at p_true = 0.5
// ---------------------------------------------------------------------------

#[test]
fn ci_is_symmetric_around_half_when_alpha_equals_beta() {
    // alpha == beta means the posterior is symmetric around 0.5;
    // the 90% CI must therefore satisfy lo + hi ≈ 1.
    let p = BetaPosterior::new(5.0, 5.0).expect("equal alpha/beta valid");
    let (lo, hi) = p.credible_interval(0.90).expect("CI converges");
    assert!(
        approx_eq(lo + hi, 1.0, 1e-3),
        "symmetric posterior must yield symmetric CI: lo={lo}, hi={hi}, sum={}",
        lo + hi
    );
}

#[test]
fn ci_for_uniform_beta_uses_stored_alpha_beta_directly() {
    let p = BetaPosterior::new(1.0, 1.0).expect("uniform beta posterior is valid");
    let (lo, hi) = p.credible_interval(0.90).expect("uniform beta CI");

    assert!(approx_eq(lo, 0.05, 1e-10), "uniform beta lo was {lo}");
    assert!(approx_eq(hi, 0.95, 1e-10), "uniform beta hi was {hi}");
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
fn ci90_bounded_within_unit_interval() {
    // The credible interval must never exceed [0, 1] — it's a
    // distribution over Bernoulli rates.
    for (a, b) in [(1.0, 100.0), (100.0, 1.0), (50.0, 50.0), (0.5, 0.5)] {
        let p = BetaPosterior::new(a, b).expect("valid posterior");
        let (lo, hi) = p.credible_interval(0.90).expect("CI converges");
        assert!(
            (0.0..=1.0).contains(&lo),
            "ci90 lo must be in [0, 1] for ({a}, {b}); got {lo}"
        );
        assert!(
            (0.0..=1.0).contains(&hi),
            "ci90 hi must be in [0, 1] for ({a}, {b}); got {hi}"
        );
    }
}

#[test]
fn ci_returns_none_or_valid_for_extreme_alpha_or_beta() {
    // BetaPosterior::new should reject zero / negative inputs, so
    // any posterior we can construct has finite positive (alpha, beta).
    // For very small alpha or beta the inverse-CDF iteration may
    // legitimately fail to converge to the requested tolerance; in
    // that case the function must return None rather than a bogus
    // interval.
    let extreme = BetaPosterior::new(1e-6, 1.0).expect("tiny but positive alpha is valid");
    if let Some((lo, hi)) = extreme.credible_interval(0.90) {
        assert!(
            lo <= hi && (0.0..=1.0).contains(&lo) && (0.0..=1.0).contains(&hi),
            "any returned CI must be a valid sub-interval of [0, 1]"
        );
    }
}
