//! N7.1 (bd-17c65.14.7.2) — Bayesian backfill unit tests.

use ee::core::bayes::{
    BetaPosterior, DEFAULT_HARMFUL_WEIGHT, DEFAULT_PRIOR_ALPHA, DEFAULT_PRIOR_BETA, FeedbackSignal,
};
use ee::core::bayes_backfill::BackfillMode;

fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-12
}

#[test]
fn utility_inverse_backfill_derives_documented_alpha_beta_pair() {
    let posterior = BetaPosterior::from_utility_inverse(0.8, 2.0)
        .expect("0.8 confidence with weight 2.0 is valid");

    assert!(approx_eq(posterior.alpha(), 1.6));
    assert!(approx_eq(posterior.beta(), 0.4));
    assert!(approx_eq(posterior.mean(), 0.8));
}

#[test]
fn utility_inverse_backfill_rejects_degenerate_inputs() {
    for (confidence, weight) in [
        (f64::NAN, 2.0),
        (0.8, f64::INFINITY),
        (-0.1, 2.0),
        (1.1, 2.0),
        (0.8, 0.0),
        (0.0, 2.0),
        (1.0, 2.0),
    ] {
        assert!(
            BetaPosterior::from_utility_inverse(confidence, weight).is_none(),
            "confidence={confidence:?}, weight={weight:?} should be rejected"
        );
    }
}

#[test]
fn feedback_replay_starts_from_jeffreys_prior_and_applies_weights() {
    let posterior = BetaPosterior::from_feedback_events([
        (FeedbackSignal::from_signal_str("helpful"), 1.0),
        (FeedbackSignal::from_signal_str("positive"), 2.0),
        (
            FeedbackSignal::from_signal_str("harmful"),
            DEFAULT_HARMFUL_WEIGHT,
        ),
        (FeedbackSignal::from_signal_str("neutral"), 10.0),
    ]);

    assert!(approx_eq(posterior.alpha(), DEFAULT_PRIOR_ALPHA + 3.0));
    assert!(approx_eq(
        posterior.beta(),
        DEFAULT_PRIOR_BETA + DEFAULT_HARMFUL_WEIGHT
    ));
}

#[test]
fn feedback_replay_normalizes_legacy_signal_spellings() {
    assert_eq!(
        FeedbackSignal::from_signal_str("confirmation"),
        FeedbackSignal::Helpful
    );
    assert_eq!(
        FeedbackSignal::from_signal_str("contradiction"),
        FeedbackSignal::Harmful
    );
    assert_eq!(
        FeedbackSignal::from_signal_str("outdated"),
        FeedbackSignal::Harmful
    );
    assert_eq!(
        FeedbackSignal::from_signal_str("ignored"),
        FeedbackSignal::Neutral
    );
}

#[test]
fn feedback_replay_falls_back_for_invalid_harmful_weight() {
    let posterior = BetaPosterior::from_feedback_events([
        (FeedbackSignal::Harmful, 0.0),
        (FeedbackSignal::Harmful, f64::NAN),
    ]);

    assert!(approx_eq(posterior.alpha(), DEFAULT_PRIOR_ALPHA));
    assert!(approx_eq(
        posterior.beta(),
        DEFAULT_PRIOR_BETA + (DEFAULT_HARMFUL_WEIGHT * 2.0)
    ));
}

#[test]
fn backfill_mode_audit_sources_are_stable() {
    assert_eq!(
        BackfillMode::FromUtility {
            weight_hundredths: 200
        }
        .audit_source(),
        "backfill_from_utility"
    );
    assert_eq!(
        BackfillMode::FromFeedbackEvents.audit_source(),
        "backfill_from_feedback_events"
    );
}
