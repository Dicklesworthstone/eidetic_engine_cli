use ee::core::sprt::{
    SPRT_ALPHA, SPRT_BAD_HARMFUL_RATE, SPRT_BENIGN_HARMFUL_RATE, SPRT_BETA, SprtDecision,
    SprtObservation, evaluate_sprt,
};

#[test]
fn harmful_run_quarantines_before_legacy_burst_ceiling() {
    let evaluation = evaluate_sprt([SprtObservation::Harmful; 4]);

    assert_eq!(evaluation.decision, SprtDecision::Quarantine);
    assert_eq!(evaluation.harmful_count, 4);
    assert_eq!(evaluation.helpful_count, 0);
    assert!(evaluation.statistic > evaluation.upper_bound);
}

#[test]
fn benign_rate_stream_stays_within_false_positive_budget() {
    let mut observations = Vec::new();
    for index in 0..100 {
        observations.push(if index % 10 == 0 {
            SprtObservation::Harmful
        } else {
            SprtObservation::Helpful
        });
    }

    let evaluation = evaluate_sprt(observations);

    assert_ne!(evaluation.decision, SprtDecision::Quarantine);
    assert!(evaluation.statistic < evaluation.upper_bound);
}

#[test]
fn helpful_run_releases_source() {
    let evaluation = evaluate_sprt([SprtObservation::Helpful; 8]);

    assert_eq!(evaluation.decision, SprtDecision::Release);
    assert!(evaluation.statistic < evaluation.lower_bound);
}

#[test]
fn configured_wald_thresholds_match_contract() {
    let evaluation = evaluate_sprt([]);
    let expected_upper = ((1.0 - SPRT_BETA) / SPRT_ALPHA).ln();
    let expected_lower = (SPRT_BETA / (1.0 - SPRT_ALPHA)).ln();

    assert!((evaluation.upper_bound - expected_upper).abs() < 0.000_001);
    assert!((evaluation.lower_bound - expected_lower).abs() < 0.000_001);
    assert_eq!(SPRT_BENIGN_HARMFUL_RATE, 0.1);
    assert_eq!(SPRT_BAD_HARMFUL_RATE, 0.4);
}
