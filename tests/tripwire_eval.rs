//! Integration tests for the tripwire event-payload evaluator and
//! harm-feedback → tripwire candidate promotion path (eidetic_engine_cli-2mad).

// These tripwire tests use unwrap/expect as direct assertions on fixed fixtures.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use ee::core::tripwire::{
    CheckOptions, CheckResult, ConditionEvaluationResult, DEFAULT_HARM_PROMOTION_THRESHOLD,
    HARM_PROMOTION_SOURCE_TYPE, HarmFeedbackPromotionOptions, HarmFeedbackPromotionOutcome,
    TRIPWIRE_HARM_PROMOTION_SCHEMA_V1, TripwireEventPayload, check_tripwire_record,
    evaluate_tripwire_condition, glob_match, propose_tripwire_from_harmful_feedback,
};
use ee::models::preflight::{Tripwire, TripwireAction, TripwireType};

#[test]
fn glob_supports_star_question_and_literals() {
    assert!(glob_match("*.sh", "deploy.sh"));
    assert!(glob_match("*rm*", "rm -rf /tmp/x"));
    assert!(glob_match("a?c", "abc"));
    assert!(!glob_match("a?c", "abbc"));
    assert!(!glob_match("*.sh", "deploy.txt"));
}

#[test]
fn event_match_condition_satisfied_with_nested_payload() {
    let payload = TripwireEventPayload::default().with_event_data(serde_json::json!({
        "command": {"path": "scripts/deploy.sh", "argv": ["./deploy.sh", "prod"]},
    }));
    let evaluation = evaluate_tripwire_condition("event:command.path=*.sh", &payload);
    assert_eq!(evaluation.result, ConditionEvaluationResult::Satisfied);
    assert_eq!(evaluation.source_key.as_deref(), Some("command.path"));
    assert!(
        evaluation
            .matched_terms
            .iter()
            .any(|term| term.contains("scripts/deploy.sh")),
        "citation present: {:?}",
        evaluation.matched_terms,
    );
}

#[test]
fn event_match_condition_unsatisfied_when_value_differs() {
    let payload = TripwireEventPayload::default()
        .with_event_data(serde_json::json!({"tool": {"name": "Read"}}));
    let evaluation = evaluate_tripwire_condition("event:tool.name=\"Bash\"", &payload);
    assert_eq!(evaluation.result, ConditionEvaluationResult::Unsatisfied);
}

#[test]
fn event_match_condition_missing_input_when_no_event_data() {
    let evaluation =
        evaluate_tripwire_condition("event:command.path=*.sh", &TripwireEventPayload::default());
    assert_eq!(evaluation.result, ConditionEvaluationResult::MissingInput);
}

#[test]
fn check_tripwire_record_triggers_from_event_payload_glob_with_citation() {
    let tripwire = Tripwire::new(
        "tw_event_bash_glob",
        "pf_evt_run",
        TripwireType::Custom,
        "event:command.path=*.sh",
        TripwireAction::Halt,
        "2026-05-06T00:00:00Z",
    );
    let report = check_tripwire_record(
        &tripwire,
        &CheckOptions {
            workspace: PathBuf::from("."),
            tripwire_id: "tw_event_bash_glob".to_owned(),
            event_payload: TripwireEventPayload::default().with_event_data(serde_json::json!({
                "command": {"path": "deploy.sh"}
            })),
            dry_run: true,
            ..Default::default()
        },
    )
    .expect("check_tripwire_record");

    assert_eq!(report.result, CheckResult::Triggered);
    assert!(report.should_halt);
    let evaluation = report
        .condition_evaluation
        .expect("condition_evaluation present");
    assert_eq!(evaluation.source_key.as_deref(), Some("command.path"));
    assert!(
        evaluation
            .matched_terms
            .iter()
            .any(|term| term.contains("deploy.sh")),
        "matched terms include payload value: {:?}",
        evaluation.matched_terms,
    );
}

#[test]
fn harm_feedback_promotion_below_threshold_does_not_promote() {
    let outcome = propose_tripwire_from_harmful_feedback(&HarmFeedbackPromotionOptions {
        workspace_id: "ws_alpha".to_owned(),
        memory_id: "mem_below".to_owned(),
        harm_count: DEFAULT_HARM_PROMOTION_THRESHOLD.saturating_sub(1),
        threshold: DEFAULT_HARM_PROMOTION_THRESHOLD,
        memory_summary: Some("low harm signal".to_owned()),
        window_seconds: 7 * 24 * 3600,
        suggested_condition: None,
    });
    matches!(outcome, HarmFeedbackPromotionOutcome::BelowThreshold { .. })
        .then_some(())
        .expect("expected BelowThreshold");
}

#[test]
fn harm_feedback_promotion_at_threshold_returns_proposal() {
    let outcome = propose_tripwire_from_harmful_feedback(&HarmFeedbackPromotionOptions {
        workspace_id: "ws_alpha".to_owned(),
        memory_id: "mem_42".to_owned(),
        harm_count: DEFAULT_HARM_PROMOTION_THRESHOLD,
        threshold: DEFAULT_HARM_PROMOTION_THRESHOLD,
        memory_summary: Some("destructive command guidance".to_owned()),
        window_seconds: 7 * 24 * 3600,
        suggested_condition: Some("event:command.path=*rm*".to_owned()),
    });
    let HarmFeedbackPromotionOutcome::Promoted(proposal) = outcome else {
        panic!("expected Promoted");
    };
    assert!(proposal.candidate_id.starts_with("curate_"));
    assert_eq!(proposal.candidate_id.len(), 33);
    assert_eq!(proposal.input.candidate_type, "rule");
    assert_eq!(proposal.input.target_memory_id, "mem_42");
    assert_eq!(proposal.input.source_type, HARM_PROMOTION_SOURCE_TYPE);
    assert_eq!(
        proposal.input.source_id.as_deref(),
        Some("harm_feedback:mem_42")
    );
    assert_eq!(proposal.condition, "event:command.path=*rm*");
    assert!(
        proposal.input.reason.contains("harmful feedback events"),
        "reason explains harm: {}",
        proposal.input.reason
    );

    let json = proposal.to_json();
    assert_eq!(
        json["schema"].as_str(),
        Some(TRIPWIRE_HARM_PROMOTION_SCHEMA_V1)
    );
    assert_eq!(json["memoryId"].as_str(), Some("mem_42"));
    assert_eq!(
        json["harmCount"].as_i64(),
        Some(i64::from(DEFAULT_HARM_PROMOTION_THRESHOLD))
    );
    assert_eq!(json["candidateType"].as_str(), Some("rule"));
}

#[test]
fn harm_feedback_promotion_is_deterministic_for_identical_inputs() {
    let opts = HarmFeedbackPromotionOptions {
        workspace_id: "ws_det".to_owned(),
        memory_id: "mem_det".to_owned(),
        harm_count: 5,
        threshold: 3,
        memory_summary: Some("repeated rule".to_owned()),
        window_seconds: 86_400,
        suggested_condition: Some("event:tool.name=\"Bash\"".to_owned()),
    };
    let HarmFeedbackPromotionOutcome::Promoted(first) =
        propose_tripwire_from_harmful_feedback(&opts)
    else {
        panic!("expected Promoted");
    };
    let HarmFeedbackPromotionOutcome::Promoted(second) =
        propose_tripwire_from_harmful_feedback(&opts)
    else {
        panic!("expected Promoted on replay");
    };
    assert_eq!(first.candidate_id, second.candidate_id);
    assert_eq!(first.input.reason, second.input.reason);
    assert_eq!(first.condition, second.condition);
}
