//! Integration tests for the extended situation taxonomy and the
//! evidence-density confidence lift introduced for
//! eidetic_engine_cli-oofg.
//!
//! Lives in `tests/` so it runs even when other agents' in-flight changes
//! break unrelated `#[cfg(test)]` blocks elsewhere in the lib.

// These taxonomy tests use unwrap/expect as direct assertions on fixed fixtures.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::str::FromStr;

use ee::core::situation::classify_task;
use ee::models::situation::{SituationCategory, SituationConfidence};

/// Bead acceptance: a release-flavored task returns release with ≥0.7 confidence.
#[test]
fn release_flavored_task_classifies_release_above_threshold() {
    let result = classify_task("prepare release v0.2.0 changelog and tag");
    assert_eq!(
        result.category,
        SituationCategory::Release,
        "category was {:?}, signals: {:?}",
        result.category,
        result.signals,
    );
    assert!(
        result.confidence_score >= 0.7,
        "confidence score {} < 0.7 (signals: {:?})",
        result.confidence_score,
        result.signals,
    );
    assert!(
        matches!(
            result.confidence,
            SituationConfidence::Medium | SituationConfidence::High
        ),
        "confidence band {:?} below medium",
        result.confidence,
    );
}

/// A second release-flavored fixture phrased differently still classifies release.
#[test]
fn cargo_publish_release_fixture_classifies_release() {
    let result = classify_task("bump version, update changelog, run cargo publish for release");
    assert_eq!(result.category, SituationCategory::Release);
    assert!(result.confidence_score >= 0.7);
}

/// Single-keyword task should remain capped below medium (honesty rule).
#[test]
fn single_signal_task_stays_in_low_band() {
    let result = classify_task("ship the change");
    // "ship" alone is one keyword; should not climb above 0.49 even though
    // it matches Deployment.
    assert!(
        result.confidence_score < 0.5,
        "single-keyword score {} should not cross 0.5",
        result.confidence_score
    );
    assert_eq!(result.confidence, SituationConfidence::Low);
}

#[test]
fn unknown_task_returns_unknown_category() {
    let result = classify_task("zxcvbnm qwerty asdf");
    assert_eq!(result.category, SituationCategory::Unknown);
    assert!(result.signals.is_empty());
    assert_eq!(result.confidence, SituationConfidence::Low);
}

#[test]
fn ambiguous_task_keeps_alternatives_visible() {
    // "fix bug in deploy script" overlaps BugFix and Deployment. The winner
    // should be the higher-scoring one and the loser should be visible in
    // alternative_categories.
    let result = classify_task("fix bug in deploy script");
    assert!(!result.signals.is_empty());
    assert!(
        !result.alternative_categories.is_empty(),
        "ambiguous task should expose alternatives, got: {:?}",
        result,
    );
}

#[test]
fn incident_response_classifies_with_high_confidence_when_multi_signal() {
    let result = classify_task("hotfix p0 incident outage rollback the deploy");
    assert_eq!(
        result.category,
        SituationCategory::IncidentResponse,
        "category was {:?}, signals: {:?}",
        result.category,
        result.signals,
    );
    assert!(
        result.confidence_score >= 0.7,
        "incident-flavored multi-signal task should clear 0.7, got {}",
        result.confidence_score
    );
}

#[test]
fn exploration_classifies_when_multiple_research_signals_match() {
    let result =
        classify_task("spike: explore prototype design space for the cache, evaluate feasibility");
    assert_eq!(result.category, SituationCategory::Exploration);
    assert!(result.confidence_score >= 0.7);
}

#[test]
fn refactor_remains_correct_with_two_signals_lift() {
    let result = classify_task("refactor and simplify the auth pipeline");
    assert_eq!(result.category, SituationCategory::Refactor);
    // Two refactor keywords each at 0.4 = 0.8 raw, two-signal lift caps at 0.85.
    assert!(result.confidence_score >= 0.7);
}

#[test]
fn new_categories_round_trip_through_from_str_and_as_str() {
    for category in [
        SituationCategory::Release,
        SituationCategory::Exploration,
        SituationCategory::IncidentResponse,
    ] {
        let s = category.as_str();
        let parsed = SituationCategory::from_str(s).unwrap_or_else(|_| panic!("round-trip: {s}"));
        assert_eq!(parsed, category, "{s} did not round-trip");
    }
}

#[test]
fn from_str_accepts_alias_forms_for_new_categories() {
    assert_eq!(
        SituationCategory::from_str("incident-response").unwrap(),
        SituationCategory::IncidentResponse
    );
    assert_eq!(
        SituationCategory::from_str("oncall").unwrap(),
        SituationCategory::IncidentResponse
    );
    assert_eq!(
        SituationCategory::from_str("spike").unwrap(),
        SituationCategory::Exploration
    );
    assert_eq!(
        SituationCategory::from_str("cut_release").unwrap(),
        SituationCategory::Release
    );
}

#[test]
fn deployment_no_longer_owns_release_keyword() {
    // Pure release vocabulary should pick Release, not Deployment.
    let result = classify_task("cut release tag and update changelog");
    assert_eq!(
        result.category,
        SituationCategory::Release,
        "release keywords should land Release, not Deployment; got {:?}",
        result.category,
    );
}

#[test]
fn deployment_still_fires_on_pure_rollout_vocabulary() {
    let result = classify_task("deploy canary rollout to staging");
    assert_eq!(result.category, SituationCategory::Deployment);
    assert!(result.confidence_score >= 0.7);
}

#[test]
fn classify_result_data_json_carries_new_category() {
    let result = classify_task("prepare release v1.0 changelog tag");
    let json = result.data_json();
    assert_eq!(json["category"].as_str(), Some("release"));
    let confidence_score = json["confidenceScore"]
        .as_f64()
        .expect("confidenceScore is numeric");
    assert!(confidence_score >= 0.7, "got {confidence_score}");
}

#[test]
fn category_all_includes_new_variants() {
    for v in [
        SituationCategory::Release,
        SituationCategory::Exploration,
        SituationCategory::IncidentResponse,
    ] {
        assert!(SituationCategory::ALL.contains(&v), "ALL is missing {v:?}",);
    }
}
