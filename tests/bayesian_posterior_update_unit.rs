//! N7.1 (bd-17c65.14.7.2) — unit tests for the Beta-Bernoulli
//! posterior update rule applied by `ee outcome` events.
//!
//! These tests exercise the pure-math layer
//! (`ee::core::bayes::BetaPosterior`) against fixed outcome streams.
//! The DB integration is covered by record_outcome's existing
//! integration tests (which run against a real FrankenSQLite DB);
//! this file is the deterministic unit-level layer.

use ee::core::bayes::{BetaPosterior, DEFAULT_HARMFUL_WEIGHT};

fn approx_eq(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() < tol
}

#[test]
fn jeffreys_prior_starts_at_one_half() {
    let p = BetaPosterior::jeffreys();
    assert!(approx_eq(p.mean(), 0.5, 1e-12));
    assert_eq!(p.alpha(), 0.5);
    assert_eq!(p.beta(), 0.5);
    assert!(approx_eq(p.effective_sample_size(), 1.0, 1e-12));
}

#[test]
fn ten_helpful_outcomes_produce_known_posterior() {
    // Starting from Jeffreys (0.5, 0.5) and applying 10 helpful events
    // each adding 1.0 to alpha: final = (10.5, 0.5).
    // Mean = 10.5 / 11.0 ≈ 0.9545.
    let mut p = BetaPosterior::jeffreys();
    for _ in 0..10 {
        p = p.update_helpful();
    }
    assert!(approx_eq(p.alpha(), 10.5, 1e-12));
    assert!(approx_eq(p.beta(), 0.5, 1e-12));
    assert!(approx_eq(p.mean(), 10.5 / 11.0, 1e-12));
}

#[test]
fn five_harmful_outcomes_with_default_weight_produce_known_posterior() {
    // 5 harmful events with weight 2.5: beta increases by 12.5.
    // Final = (0.5, 13.0). Mean = 0.5 / 13.5 ≈ 0.037.
    let mut p = BetaPosterior::jeffreys();
    for _ in 0..5 {
        p = p.update_harmful(DEFAULT_HARMFUL_WEIGHT);
    }
    assert!(approx_eq(p.alpha(), 0.5, 1e-12));
    assert!(approx_eq(p.beta(), 13.0, 1e-12));
    assert!(approx_eq(p.mean(), 0.5 / 13.5, 1e-12));
}

#[test]
fn mixed_outcome_stream_matches_closed_form() {
    // 8 helpful, 2 harmful (weight 2.5):
    //   alpha = 0.5 + 8 = 8.5
    //   beta  = 0.5 + 5.0 = 5.5
    //   mean  = 8.5 / 14.0 ≈ 0.607
    let mut p = BetaPosterior::jeffreys();
    for _ in 0..8 {
        p = p.update_helpful();
    }
    for _ in 0..2 {
        p = p.update_harmful(DEFAULT_HARMFUL_WEIGHT);
    }
    assert!(approx_eq(p.alpha(), 8.5, 1e-12));
    assert!(approx_eq(p.beta(), 5.5, 1e-12));
    assert!(approx_eq(p.mean(), 8.5 / 14.0, 1e-12));
}

#[test]
fn harmful_asymmetry_is_preserved() {
    // ADR 0032 contract: harmful events hit harder than helpful.
    // Two streams with the same arithmetic count of events:
    //   stream A: 5 helpful, 5 harmful
    //   stream B: 10 helpful (mirror baseline)
    // Stream A should have lower mean than stream B because each
    // harmful event adds 2.5 to beta vs 1.0 to alpha.
    let mut a = BetaPosterior::jeffreys();
    for _ in 0..5 {
        a = a.update_helpful();
    }
    for _ in 0..5 {
        a = a.update_harmful(DEFAULT_HARMFUL_WEIGHT);
    }
    let mut b = BetaPosterior::jeffreys();
    for _ in 0..10 {
        b = b.update_helpful();
    }
    assert!(
        a.mean() < b.mean(),
        "harmful asymmetry: stream-A mean {} should be < stream-B mean {}",
        a.mean(),
        b.mean()
    );
    // Stream A is also LOWER than 0.5 — the harmful weight dominates.
    assert!(a.mean() < 0.5);
}

#[test]
fn credible_interval_narrows_with_evidence() {
    // ADR 0032: trust-class transitions gate on credible-interval
    // width AND the sample-size gate (alpha + beta >= 6). Sanity:
    // a 100-event posterior has a narrower CI than a 10-event one.
    let mut few = BetaPosterior::jeffreys();
    for _ in 0..10 {
        few = few.update_helpful();
    }
    let mut many = BetaPosterior::jeffreys();
    for _ in 0..100 {
        many = many.update_helpful();
    }
    let (few_lo, few_hi) = few.credible_interval(0.90).unwrap();
    let (many_lo, many_hi) = many.credible_interval(0.90).unwrap();
    let few_width = few_hi - few_lo;
    let many_width = many_hi - many_lo;
    assert!(
        many_width < few_width,
        "many-evidence ci90 width {many_width} should be < few-evidence ci90 width {few_width}"
    );
}

#[test]
fn effective_sample_size_is_alpha_plus_beta() {
    // Direct: ess = alpha + beta. Used by the ADR 0032 sample-size
    // gate at agent_assertion → agent_validated transition.
    let mut p = BetaPosterior::jeffreys();
    for _ in 0..3 {
        p = p.update_helpful();
    }
    for _ in 0..2 {
        p = p.update_harmful(DEFAULT_HARMFUL_WEIGHT);
    }
    // alpha = 3.5, beta = 5.5 → ess = 9.0
    assert!(approx_eq(p.effective_sample_size(), 9.0, 1e-12));
    // 9.0 satisfies the >= 6 sample-size gate.
    assert!(p.effective_sample_size() >= 6.0);
}

#[test]
fn deterministic_under_replay() {
    // Determinism contract: replaying the same outcome stream
    // produces the same (alpha, beta) exactly. This is the property
    // N4.5 proptest harness exploits — the Bayes path adds no
    // ambient randomness.
    let make_posterior = || {
        let mut p = BetaPosterior::jeffreys();
        for _ in 0..7 {
            p = p.update_helpful();
        }
        for _ in 0..3 {
            p = p.update_harmful(DEFAULT_HARMFUL_WEIGHT);
        }
        p
    };
    let a = make_posterior();
    let b = make_posterior();
    assert_eq!(a, b);
    assert_eq!(a.alpha(), b.alpha());
    assert_eq!(a.beta(), b.beta());
    assert_eq!(a.mean(), b.mean());
}

#[test]
fn default_harmful_weight_matches_readme_config() {
    // README §Configuration documents `[curation] harmful_weight =
    // 2.5`. The Bayes code lifts that into a const so the value is
    // discoverable from src. This test is a tripwire: if either side
    // drifts, the assertion fails loudly.
    assert!(approx_eq(DEFAULT_HARMFUL_WEIGHT, 2.5, 1e-12));
}
