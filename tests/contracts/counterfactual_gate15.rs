//! Gate 15: Counterfactual Lab Output Contract Test (EE-386).
//!
//! Proves the counterfactual memory lab meets its contract for capturing,
//! replaying, and analyzing task episodes with memory interventions:
//!
//! - Capture reports include episode ID, workspace, and hash
//! - Replay reports identify missing frozen inputs instead of simulating success
//! - Counterfactual reports include hypothesis pack diffs and validation-only entries
//! - Reconstruct reports recover events from recorder traces
//! - All reports serialize to stable JSON schemas
//! - Golden fixtures verify output stability across versions
//! - Dry-run mode produces valid reports without side effects

use ee::core::lab::{
    CaptureOptions, CounterfactualOptions, CounterfactualStatus, InterventionSpec,
    InterventionType, LAB_CAPTURE_SCHEMA_V1, LAB_COUNTERFACTUAL_SCHEMA_V1,
    LAB_RECONSTRUCT_SCHEMA_V1, LAB_REPLAY_SCHEMA_V1, ReconstructOptions, ReconstructStatus,
    ReplayOptions, ReplayStatus, capture_episode, reconstruct_episode, replay_episode,
    run_counterfactual,
};
use ee::output::{render_lab_capture_json, render_lab_counterfactual_json, render_lab_replay_json};

use std::env;
use std::fs;
use std::path::PathBuf;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    ensure(
        haystack.contains(needle),
        format!("{context}: expected to contain '{needle}' but got:\n{haystack}"),
    )
}

fn ensure_not_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    ensure(
        !haystack.contains(needle),
        format!("{context}: expected not to contain '{needle}' but got:\n{haystack}"),
    )
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("counterfactual")
        .join(format!("{name}.golden"))
}

fn assert_golden(name: &str, actual: &str) -> TestResult {
    let update_mode = env::var("UPDATE_GOLDEN").is_ok();
    let path = golden_path(name);

    if update_mode {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("failed to create dir: {e}"))?;
        }
        fs::write(&path, actual).map_err(|e| format!("failed to write golden: {e}"))?;
        eprintln!("Updated golden file: {}", path.display());
        return Ok(());
    }

    let expected = fs::read_to_string(&path).map_err(|e| {
        format!(
            "Golden file not found: {}\nRun with UPDATE_GOLDEN=1 to create it.\nError: {e}",
            path.display()
        )
    })?;

    let expected = expected.strip_suffix('\n').unwrap_or(&expected);

    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "Golden test '{name}' failed.\nGolden file: {}\nRun with UPDATE_GOLDEN=1 to update.\n\n--- expected\n{expected}\n+++ actual\n{actual}",
            path.display()
        ))
    }
}

fn lab_golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("lab")
        .join(format!("{name}.json.golden"))
}

fn assert_lab_golden(name: &str, actual: &str) -> TestResult {
    let update_mode = env::var("UPDATE_GOLDEN").is_ok();
    let path = lab_golden_path(name);

    if update_mode {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("failed to create dir: {e}"))?;
        }
        fs::write(&path, actual).map_err(|e| format!("failed to write golden: {e}"))?;
        eprintln!("Updated golden file: {}", path.display());
        return Ok(());
    }

    let expected = fs::read_to_string(&path).map_err(|e| {
        format!(
            "Lab golden file not found: {}\nRun with UPDATE_GOLDEN=1 to create it.\nError: {e}",
            path.display()
        )
    })?;

    ensure(
        actual == expected,
        format!("lab golden mismatch for {name}\n--- expected\n{expected}\n+++ actual\n{actual}"),
    )
}

// ============================================================================
// Core Capture Contract
// ============================================================================

#[test]
fn capture_report_has_schema_field() -> TestResult {
    let options = CaptureOptions {
        workspace: PathBuf::from("."),
        task_input: Some("test task".to_string()),
        dry_run: true,
        ..Default::default()
    };

    let report = capture_episode(&options).map_err(|e| e.message())?;
    let json = report.to_json();

    ensure_contains(&json, LAB_CAPTURE_SCHEMA_V1, "capture schema present")
}

#[test]
fn capture_report_has_episode_id_with_prefix() -> TestResult {
    let options = CaptureOptions {
        workspace: PathBuf::from("."),
        dry_run: true,
        ..Default::default()
    };

    let report = capture_episode(&options).map_err(|e| e.message())?;

    ensure(
        report.episode_id.starts_with("ep_"),
        "episode ID starts with ep_ prefix",
    )
}

#[test]
fn capture_report_preserves_task_input() -> TestResult {
    let options = CaptureOptions {
        workspace: PathBuf::from("."),
        task_input: Some("fix the failing test".to_string()),
        dry_run: true,
        ..Default::default()
    };

    let report = capture_episode(&options).map_err(|e| e.message())?;

    ensure(
        report.task_input == "fix the failing test",
        "task input preserved in report",
    )
}

#[test]
fn capture_report_includes_workspace() -> TestResult {
    let options = CaptureOptions {
        workspace: PathBuf::from("/test/workspace"),
        dry_run: true,
        ..Default::default()
    };

    let report = capture_episode(&options).map_err(|e| e.message())?;
    let json = report.to_json();

    ensure_contains(&json, "workspace", "workspace field present")
}

#[test]
fn capture_non_dry_run_reports_unavailable_store_without_episode_hash() -> TestResult {
    let options = CaptureOptions {
        workspace: PathBuf::from("."),
        dry_run: false,
        ..Default::default()
    };

    let report = capture_episode(&options).map_err(|e| e.message())?;

    ensure(!report.stored, "capture does not claim persisted storage")?;
    ensure(
        report.episode_hash.is_none(),
        "episode hash is absent until a frozen episode is stored",
    )?;
    ensure(report.pack_hash.is_some(), "pack hash preview is present")
}

#[test]
fn capture_dry_run_omits_hash() -> TestResult {
    let options = CaptureOptions {
        workspace: PathBuf::from("."),
        dry_run: true,
        ..Default::default()
    };

    let report = capture_episode(&options).map_err(|e| e.message())?;

    ensure(report.episode_hash.is_none(), "dry run omits hash")
}

#[test]
fn gate15_capture_records_redacted_episode_evidence() -> TestResult {
    let options = CaptureOptions {
        workspace: PathBuf::from("/repo/ee"),
        session_id: Some("session_gate15".to_string()),
        task_input: Some("fix failing release workflow".to_string()),
        dry_run: false,
        ..Default::default()
    };

    let report = capture_episode(&options).map_err(|e| e.message())?;

    ensure(
        !report.stored,
        "non-dry capture does not claim persisted storage until the episode store exists",
    )?;
    ensure(
        report.redaction_status == "redacted",
        "capture enforces redaction before storage",
    )?;
    ensure(report.pack_hash.is_some(), "pack hash is present")?;
    ensure(!report.policy_ids.is_empty(), "policy IDs are present")?;
    ensure(report.outcome_ref.is_some(), "outcome reference is present")?;
    ensure(
        report.repository_fingerprint.is_some(),
        "repository fingerprint is present",
    )?;
    ensure(
        !report.evidence_ids.is_empty(),
        "capture reports evidence IDs",
    )?;
    ensure(
        report
            .evidence_ids
            .iter()
            .any(|id| id == "cass:session_gate15:session"),
        "capture includes session evidence ID",
    )?;

    let json = render_lab_capture_json(&report);
    ensure_contains(&json, "\"packHash\":", "rendered pack hash")?;
    ensure_contains(&json, "\"policyIds\":", "rendered policy IDs")?;
    ensure_contains(&json, "\"evidenceIds\":", "rendered evidence IDs")?;
    ensure_contains(&json, "\"redactionStatus\":\"redacted\"", "redaction")
}

#[test]
fn gate15_capture_redacts_sensitive_task_payloads() -> TestResult {
    let password_value = "fixture-value-one";
    let token_value = "fixture-value-two";
    let api_key_value = "fixture-value-three";
    let task_input = [
        "debug auth failure ",
        "pass",
        "word=",
        password_value,
        " tok",
        "en=",
        token_value,
        " api",
        "_key=",
        api_key_value,
    ]
    .concat();
    let options = CaptureOptions {
        workspace: PathBuf::from("/repo/ee"),
        session_id: Some("session_secret".to_string()),
        task_input: Some(task_input),
        dry_run: false,
        ..Default::default()
    };

    let report = capture_episode(&options).map_err(|e| e.message())?;

    ensure_not_contains(&report.task_input, password_value, "password redacted")?;
    ensure_not_contains(&report.task_input, token_value, "token redacted")?;
    ensure_not_contains(&report.task_input, api_key_value, "api key redacted")?;
    ensure_contains(
        &report.task_input,
        "***REDACTED:password***",
        "password marker",
    )?;
    ensure(
        report.redaction_classes == vec!["api_key", "password", "token"],
        format!(
            "redaction classes are stable: {:?}",
            report.redaction_classes
        ),
    )?;
    ensure(
        report
            .evidence_ids
            .iter()
            .any(|id| id.starts_with("task_input:")),
        "redacted task input hash is tracked as evidence",
    )?;

    let json = render_lab_capture_json(&report);
    ensure_not_contains(&json, password_value, "rendered password redacted")?;
    ensure_not_contains(&json, token_value, "rendered token redacted")?;
    ensure_contains(
        &json,
        "\"redactionClasses\":[",
        "redaction classes rendered",
    )
}

#[test]
fn gate15_capture_pack_hash_is_stable_for_same_explicit_inputs() -> TestResult {
    let task_input = ["prepare release pass", "word=fixture-value-one"].concat();
    let options = CaptureOptions {
        workspace: PathBuf::from("/repo/ee"),
        session_id: Some("session_stable".to_string()),
        task_input: Some(task_input),
        dry_run: false,
        ..Default::default()
    };

    let first = capture_episode(&options).map_err(|e| e.message())?;
    let second = capture_episode(&options).map_err(|e| e.message())?;

    ensure(
        first.pack_hash == second.pack_hash,
        "pack hash is stable for the same redacted explicit inputs",
    )?;
    ensure(
        first.evidence_ids == second.evidence_ids,
        "evidence IDs are deterministic for the same explicit inputs",
    )
}

// ============================================================================
// Core Replay Contract
// ============================================================================

#[test]
fn replay_report_has_schema_field() -> TestResult {
    let options = ReplayOptions {
        episode_id: "ep_test123".to_string(),
        dry_run: true,
        ..Default::default()
    };

    let report = replay_episode(&options).map_err(|e| e.message())?;
    let json = report.to_json();

    ensure_contains(&json, LAB_REPLAY_SCHEMA_V1, "replay schema present")
}

#[test]
fn replay_report_tracks_episode_id() -> TestResult {
    let options = ReplayOptions {
        episode_id: "ep_original_episode".to_string(),
        dry_run: true,
        ..Default::default()
    };

    let report = replay_episode(&options).map_err(|e| e.message())?;

    ensure(
        report.episode_id == "ep_original_episode",
        "episode ID preserved",
    )
}

#[test]
fn replay_report_has_unique_replay_id() -> TestResult {
    let options = ReplayOptions {
        episode_id: "ep_test".to_string(),
        dry_run: true,
        ..Default::default()
    };

    let report = replay_episode(&options).map_err(|e| e.message())?;

    ensure(
        report.replay_id.starts_with("rpl_"),
        "replay ID has rpl_ prefix",
    )
}

#[test]
fn replay_status_variants_stable() {
    assert_eq!(ReplayStatus::Pending.as_str(), "pending");
    assert_eq!(ReplayStatus::Replayed.as_str(), "replayed");
    assert_eq!(ReplayStatus::Diverged.as_str(), "diverged");
    assert_eq!(ReplayStatus::Failed.as_str(), "failed");
    assert_eq!(ReplayStatus::EpisodeNotFound.as_str(), "episode_not_found");
}

#[test]
fn replay_status_success_semantics() {
    assert!(ReplayStatus::Replayed.is_success());
    assert!(!ReplayStatus::Pending.is_success());
    assert!(!ReplayStatus::Diverged.is_success());
    assert!(!ReplayStatus::Failed.is_success());
}

#[test]
fn replay_dry_run_reports_missing_frozen_inputs() -> TestResult {
    let options = ReplayOptions {
        episode_id: "ep_test".to_string(),
        dry_run: true,
        ..Default::default()
    };

    let report = replay_episode(&options).map_err(|e| e.message())?;

    ensure(
        report.status == ReplayStatus::EpisodeNotFound,
        "dry run replay requires frozen inputs",
    )?;
    ensure(!report.frozen_inputs, "frozen inputs are unavailable")?;
    ensure(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("missing frozen episode manifest")),
        "missing manifest warning",
    )
}

#[test]
fn replay_non_dry_run_reports_missing_frozen_inputs() -> TestResult {
    let options = ReplayOptions {
        episode_id: "ep_test".to_string(),
        dry_run: false,
        ..Default::default()
    };

    let report = replay_episode(&options).map_err(|e| e.message())?;

    ensure(
        report.status == ReplayStatus::EpisodeNotFound,
        "non-dry replay cannot complete without frozen inputs",
    )?;
    ensure(
        !report.replay_evidence_available,
        "replay evidence is unavailable",
    )?;
    ensure(
        report
            .missing_frozen_inputs
            .contains(&"frozen episode manifest".to_string()),
        "missing inputs identify the episode manifest",
    )
}

#[test]
fn gate15_replay_reports_missing_frozen_inputs_without_mutable_state_access() -> TestResult {
    let options = ReplayOptions {
        episode_id: "ep_frozen_inputs".to_string(),
        dry_run: false,
        verify_hash: true,
        ..Default::default()
    };

    let report = replay_episode(&options).map_err(|e| e.message())?;

    ensure(!report.frozen_inputs, "frozen inputs are unavailable")?;
    ensure(
        report.mutable_current_state_access.is_empty(),
        "no mutable current-state access is reported",
    )?;
    ensure(
        !report.episode_hash_verified,
        "episode hash is not verified without frozen inputs",
    )?;

    let json = render_lab_replay_json(&report);
    ensure_contains(&json, "\"frozenInputs\":false", "frozen input field")?;
    ensure_contains(
        &json,
        "\"replayEvidenceAvailable\":false",
        "replay evidence field",
    )?;
    ensure_contains(
        &json,
        "\"missingFrozenInputs\":[\"frozen episode manifest\",\"frozen memory snapshot\",\"frozen action trace\"]",
        "missing frozen input list",
    )?;
    ensure_contains(
        &json,
        "\"mutableCurrentStateAccess\":[]",
        "mutable access field",
    )?;
    ensure_not_contains(&json, "outcome_matches", "no outcome comparison field")?;
    ensure_contains(&json, "lab_replay_unavailable", "degradation warning")
}

// ============================================================================
// Core Counterfactual Contract
// ============================================================================

#[test]
fn counterfactual_report_has_schema_field() -> TestResult {
    let options = CounterfactualOptions {
        episode_id: "ep_test".to_string(),
        dry_run: true,
        ..Default::default()
    };

    let report = run_counterfactual(&options).map_err(|e| e.message())?;
    let json = report.to_json();

    ensure_contains(
        &json,
        LAB_COUNTERFACTUAL_SCHEMA_V1,
        "counterfactual schema present",
    )
}

#[test]
fn counterfactual_report_has_run_id_with_prefix() -> TestResult {
    let options = CounterfactualOptions {
        episode_id: "ep_test".to_string(),
        dry_run: true,
        ..Default::default()
    };

    let report = run_counterfactual(&options).map_err(|e| e.message())?;

    ensure(
        report.run_id.starts_with("cfr_"),
        "counterfactual run ID has cfr_ prefix",
    )
}

#[test]
fn counterfactual_tracks_interventions_applied() -> TestResult {
    let options = CounterfactualOptions {
        episode_id: "ep_test".to_string(),
        interventions: vec![
            InterventionSpec::add_memory("helpful context"),
            InterventionSpec::remove_memory("mem_noisy"),
        ],
        dry_run: false,
        ..Default::default()
    };

    let report = run_counterfactual(&options).map_err(|e| e.message())?;

    ensure(
        report.interventions_applied == 2,
        "intervention count matches",
    )
}

#[test]
fn counterfactual_status_variants_stable() {
    assert_eq!(CounterfactualStatus::Pending.as_str(), "pending");
    assert_eq!(
        CounterfactualStatus::HypothesisReady.as_str(),
        "hypothesis_ready"
    );
    assert_eq!(
        CounterfactualStatus::MissingReplayEvidence.as_str(),
        "missing_replay_evidence"
    );
    assert_eq!(CounterfactualStatus::Failed.as_str(), "failed");
}

#[test]
fn counterfactual_with_intervention_reports_missing_replay_evidence() -> TestResult {
    let options = CounterfactualOptions {
        episode_id: "ep_failure".to_string(),
        interventions: vec![
            InterventionSpec::add_memory("missing context")
                .with_hypothesis("Adding context would prevent failure"),
        ],
        generate_hypotheses: true,
        dry_run: false,
        ..Default::default()
    };

    let report = run_counterfactual(&options).map_err(|e| e.message())?;

    ensure(
        report.behavior_claims.is_empty(),
        "no behavior claims are emitted without replay evidence",
    )?;
    ensure(
        report.status == CounterfactualStatus::MissingReplayEvidence,
        "missing replay evidence produces explicit status",
    )
}

#[test]
fn counterfactual_generates_hypothesis_records() -> TestResult {
    let options = CounterfactualOptions {
        episode_id: "ep_failure".to_string(),
        interventions: vec![
            InterventionSpec::add_memory("context A"),
            InterventionSpec::remove_memory("mem_noisy"),
        ],
        generate_hypotheses: true,
        dry_run: false,
        ..Default::default()
    };

    let report = run_counterfactual(&options).map_err(|e| e.message())?;

    ensure(
        report.hypothesis_records.len() == 2,
        "hypothesis record per intervention",
    )
}

#[test]
fn counterfactual_dry_run_skips_hypothesis_records() -> TestResult {
    let options = CounterfactualOptions {
        episode_id: "ep_test".to_string(),
        interventions: vec![InterventionSpec::add_memory("test")],
        generate_hypotheses: true,
        dry_run: true,
        ..Default::default()
    };

    let report = run_counterfactual(&options).map_err(|e| e.message())?;

    ensure(
        report.hypothesis_records.is_empty(),
        "dry run generates no hypothesis records",
    )
}

#[test]
fn counterfactual_marks_hypothesis_confidence_without_replay_evidence() -> TestResult {
    let options = CounterfactualOptions {
        episode_id: "ep_test".to_string(),
        interventions: vec![InterventionSpec::add_memory("context")],
        dry_run: false,
        ..Default::default()
    };

    let report = run_counterfactual(&options).map_err(|e| e.message())?;

    ensure(
        report.confidence_state == "hypothesis_only_missing_replay_evidence",
        "confidence state identifies missing replay evidence",
    )?;
    ensure(
        !report.replay_evidence_available,
        "replay evidence is unavailable",
    )
}

#[test]
fn gate15_counterfactual_is_non_mutating_and_explains_hypothesis() -> TestResult {
    let options = CounterfactualOptions {
        episode_id: "ep_gate15_failure".to_string(),
        interventions: vec![
            InterventionSpec::add_memory("remember release workflow needs fmt and clippy")
                .with_hypothesis("The missing warning would have entered the context pack"),
        ],
        generate_hypotheses: true,
        dry_run: false,
        ..Default::default()
    };

    let report = run_counterfactual(&options).map_err(|e| e.message())?;

    ensure(
        !report.durable_mutation,
        "counterfactual never mutates durable state",
    )?;
    ensure(
        report.observed_pack_hash.is_none(),
        "observed pack hash is unavailable without frozen inputs",
    )?;
    ensure(
        report.counterfactual_pack_hash.is_some(),
        "counterfactual pack hash",
    )?;
    ensure(
        !report.changed_items.is_empty(),
        "changed items are explicit",
    )?;
    ensure(!report.assumptions.is_empty(), "assumptions are explicit")?;
    ensure(
        report.next_action.contains("validate"),
        "next action requires validation",
    )?;
    ensure(
        report.claim_status == "hypothesis",
        "claim remains hypothesis until externally verified",
    )?;
    ensure(
        report.behavior_claims.is_empty(),
        "no behavior claims are emitted without replay evidence",
    )?;
    ensure(
        report.curation_candidates.iter().all(|candidate| {
            candidate.requires_validate && candidate.requires_apply && !candidate.applied
        }),
        "generated candidates require normal validate/apply steps",
    )?;

    let json = render_lab_counterfactual_json(&report);
    ensure_contains(&json, "\"durableMutation\":false", "no mutation")?;
    ensure_contains(&json, "\"claimStatus\":\"hypothesis\"", "claim posture")?;
    ensure_contains(&json, "\"behaviorClaims\":[]", "no behavior claims")?;
    ensure_not_contains(&json, "wouldHave", "no would-have field")?;
    ensure_not_contains(&json, "outcome_changed", "no outcome changed field")?;
    ensure_contains(
        &json,
        "\"degradationCodes\":[\"lab_replay_unavailable\"]",
        "missing replay degradation",
    )
}

#[test]
fn gate15_hypothesis_records_require_replay_evidence() -> TestResult {
    let options = CounterfactualOptions {
        episode_id: "ep_gate15_hypothesis_taxonomy".to_string(),
        interventions: vec![
            InterventionSpec::add_memory("missed warning"),
            InterventionSpec::strengthen_memory("mem_stale", 0.4),
            InterventionSpec::remove_memory("mem_noisy"),
            InterventionSpec::weaken_memory("mem_harmful", 0.3),
            InterventionSpec::add_memory("overfit policy correction"),
        ],
        generate_hypotheses: true,
        dry_run: false,
        ..Default::default()
    };

    let report = run_counterfactual(&options).map_err(|e| e.message())?;
    let kinds = report
        .hypothesis_records
        .iter()
        .map(|record| record.hypothesis_kind.as_str())
        .collect::<Vec<_>>();

    ensure(
        kinds
            == vec![
                "pack_diff_hypothesis",
                "pack_diff_hypothesis",
                "pack_diff_hypothesis",
                "pack_diff_hypothesis",
                "pack_diff_hypothesis",
            ],
        format!("hypothesis records use mechanical taxonomy: {kinds:?}"),
    )?;
    ensure(
        report
            .hypothesis_records
            .iter()
            .all(|record| record.requires_replay_evidence),
        "hypothesis records require replay evidence",
    )?;
    ensure(
        report
            .hypothesis_records
            .iter()
            .all(|record| record.validation_status == "unvalidated"),
        "hypothesis records require validation",
    )
}

// ============================================================================
// Intervention Spec Contract
// ============================================================================

#[test]
fn intervention_type_variants_stable() {
    assert_eq!(InterventionType::Add.as_str(), "add");
    assert_eq!(InterventionType::Remove.as_str(), "remove");
    assert_eq!(InterventionType::Strengthen.as_str(), "strengthen");
    assert_eq!(InterventionType::Weaken.as_str(), "weaken");
}

#[test]
fn intervention_add_memory_builder() {
    let spec = InterventionSpec::add_memory("test content");

    assert_eq!(spec.intervention_type, InterventionType::Add);
    assert_eq!(spec.memory_content, Some("test content".to_string()));
    assert!(spec.memory_id.is_none());
}

#[test]
fn intervention_remove_memory_builder() {
    let spec = InterventionSpec::remove_memory("mem_12345");

    assert_eq!(spec.intervention_type, InterventionType::Remove);
    assert_eq!(spec.memory_id, Some("mem_12345".to_string()));
    assert!(spec.memory_content.is_none());
}

#[test]
fn intervention_strengthen_memory_builder() {
    let spec = InterventionSpec::strengthen_memory("mem_weak", 0.5);

    assert_eq!(spec.intervention_type, InterventionType::Strengthen);
    assert_eq!(spec.memory_id, Some("mem_weak".to_string()));
    assert_eq!(spec.strength_delta, Some(0.5));
}

#[test]
fn intervention_weaken_memory_builder() -> TestResult {
    let spec = InterventionSpec::weaken_memory("mem_strong", 0.3);

    assert_eq!(spec.intervention_type, InterventionType::Weaken);
    assert_eq!(spec.memory_id, Some("mem_strong".to_string()));
    let Some(delta) = spec.strength_delta else {
        return Err("strength_delta missing".to_string());
    };
    ensure(delta < 0.0, "weaken produces negative delta")
}

#[test]
fn intervention_with_hypothesis() {
    let spec = InterventionSpec::add_memory("context")
        .with_hypothesis("Adding context would improve outcome");

    assert_eq!(
        spec.hypothesis,
        Some("Adding context would improve outcome".to_string())
    );
}

// ============================================================================
// Hypothesis Record Contract
// ============================================================================

#[test]
fn hypothesis_record_includes_intervention_type() -> TestResult {
    let options = CounterfactualOptions {
        episode_id: "ep_test".to_string(),
        interventions: vec![InterventionSpec::add_memory("helpful")],
        generate_hypotheses: true,
        dry_run: false,
        ..Default::default()
    };

    let report = run_counterfactual(&options).map_err(|e| e.message())?;

    ensure(
        !report.hypothesis_records.is_empty(),
        "has hypothesis records",
    )?;
    let record = report
        .hypothesis_records
        .first()
        .ok_or_else(|| "missing hypothesis record".to_string())?;
    ensure(
        record.intervention_type == InterventionType::Add,
        "hypothesis record tracks intervention type",
    )
}

#[test]
fn hypothesis_record_has_unique_id() -> TestResult {
    let options = CounterfactualOptions {
        episode_id: "ep_test".to_string(),
        interventions: vec![
            InterventionSpec::add_memory("a"),
            InterventionSpec::add_memory("b"),
        ],
        generate_hypotheses: true,
        dry_run: false,
        ..Default::default()
    };

    let report = run_counterfactual(&options).map_err(|e| e.message())?;

    ensure(report.hypothesis_records.len() == 2, "has two records")?;
    let first = report
        .hypothesis_records
        .first()
        .ok_or_else(|| "missing first hypothesis record".to_string())?;
    let second = report
        .hypothesis_records
        .get(1)
        .ok_or_else(|| "missing second hypothesis record".to_string())?;
    ensure(first.id != second.id, "hypothesis records have unique IDs")
}

#[test]
fn hypothesis_record_links_to_episode() -> TestResult {
    let options = CounterfactualOptions {
        episode_id: "ep_specific_episode".to_string(),
        interventions: vec![InterventionSpec::add_memory("context")],
        generate_hypotheses: true,
        dry_run: false,
        ..Default::default()
    };

    let report = run_counterfactual(&options).map_err(|e| e.message())?;

    ensure(
        !report.hypothesis_records.is_empty(),
        "has hypothesis records",
    )?;
    let record = report
        .hypothesis_records
        .first()
        .ok_or_else(|| "missing hypothesis record".to_string())?;
    ensure(
        record.episode_id == "ep_specific_episode",
        "hypothesis record links to source episode",
    )
}

// ============================================================================
// Reconstruct Contract
// ============================================================================

#[test]
fn reconstruct_report_has_schema_field() -> TestResult {
    let options = ReconstructOptions {
        run_id: "run_test123".to_string(),
        dry_run: true,
        ..Default::default()
    };

    let report = reconstruct_episode(&options).map_err(|e| e.message())?;
    let json = report.to_json();

    ensure_contains(
        &json,
        LAB_RECONSTRUCT_SCHEMA_V1,
        "reconstruct schema present",
    )
}

#[test]
fn reconstruct_status_variants_stable() {
    assert_eq!(ReconstructStatus::Pending.as_str(), "pending");
    assert_eq!(ReconstructStatus::Reconstructed.as_str(), "reconstructed");
    assert_eq!(
        ReconstructStatus::PartialReconstruction.as_str(),
        "partial_reconstruction"
    );
    assert_eq!(ReconstructStatus::RunNotFound.as_str(), "run_not_found");
    assert_eq!(ReconstructStatus::Failed.as_str(), "failed");
}

#[test]
fn reconstruct_status_success_semantics() {
    assert!(ReconstructStatus::Reconstructed.is_success());
    assert!(ReconstructStatus::PartialReconstruction.is_success());
    assert!(!ReconstructStatus::Pending.is_success());
    assert!(!ReconstructStatus::RunNotFound.is_success());
    assert!(!ReconstructStatus::Failed.is_success());
}

#[test]
fn reconstruct_generates_episode_id() -> TestResult {
    let options = ReconstructOptions {
        run_id: "run_test".to_string(),
        dry_run: false,
        ..Default::default()
    };

    let report = reconstruct_episode(&options).map_err(|e| e.message())?;

    ensure(
        report.episode_id.starts_with("ep_"),
        "generated episode ID has ep_ prefix",
    )
}

#[test]
fn reconstruct_tracks_event_counts() -> TestResult {
    let options = ReconstructOptions {
        run_id: "run_full".to_string(),
        include_memories: true,
        include_tool_calls: true,
        include_user_messages: true,
        include_assistant_responses: true,
        dry_run: false,
        ..Default::default()
    };

    let report = reconstruct_episode(&options).map_err(|e| e.message())?;

    ensure(report.event_count > 0, "has events")?;
    ensure(report.memory_events > 0, "has memory events")?;
    ensure(report.tool_call_events > 0, "has tool call events")?;
    ensure(report.message_events > 0, "has message events")
}

#[test]
fn reconstruct_filters_event_types() -> TestResult {
    let options = ReconstructOptions {
        run_id: "run_filtered".to_string(),
        include_memories: true,
        include_tool_calls: false,
        include_user_messages: false,
        include_assistant_responses: false,
        dry_run: false,
        ..Default::default()
    };

    let report = reconstruct_episode(&options).map_err(|e| e.message())?;

    ensure(report.memory_events > 0, "has memory events")?;
    ensure(report.tool_call_events == 0, "no tool call events")?;
    ensure(report.message_events == 0, "no message events")
}

#[test]
fn reconstruct_empty_run_id_returns_not_found() -> TestResult {
    let options = ReconstructOptions {
        run_id: String::new(),
        dry_run: false,
        ..Default::default()
    };

    let report = reconstruct_episode(&options).map_err(|e| e.message())?;

    ensure(
        report.status == ReconstructStatus::RunNotFound,
        "empty run ID returns not found",
    )?;
    ensure(!report.warnings.is_empty(), "includes warning")
}

#[test]
fn reconstruct_non_dry_run_includes_hash() -> TestResult {
    let options = ReconstructOptions {
        run_id: "run_hash".to_string(),
        dry_run: false,
        ..Default::default()
    };

    let report = reconstruct_episode(&options).map_err(|e| e.message())?;

    ensure(report.episode_hash.is_some(), "episode hash present")?;
    let Some(hash) = report.episode_hash.as_ref() else {
        return Err("episode hash missing".to_string());
    };
    ensure(hash.starts_with("blake3:"), "hash uses blake3 prefix")
}

// ============================================================================
// JSON Serialization Contract
// ============================================================================

#[test]
fn capture_report_json_is_valid() -> TestResult {
    let options = CaptureOptions {
        workspace: PathBuf::from("."),
        task_input: Some("test".to_string()),
        dry_run: true,
        ..Default::default()
    };

    let report = capture_episode(&options).map_err(|e| e.message())?;
    let json = report.to_json();

    ensure(!json.is_empty(), "JSON is not empty")?;
    ensure(json.starts_with('{'), "JSON starts with object brace")?;
    ensure(json.ends_with('}'), "JSON ends with object brace")
}

#[test]
fn capture_report_pretty_json_is_formatted() -> TestResult {
    let options = CaptureOptions {
        workspace: PathBuf::from("."),
        dry_run: true,
        ..Default::default()
    };

    let report = capture_episode(&options).map_err(|e| e.message())?;
    let json = report.to_json_pretty();

    ensure(json.contains('\n'), "pretty JSON has newlines")?;
    ensure(json.contains("  "), "pretty JSON has indentation")
}

#[test]
fn replay_report_json_includes_all_fields() -> TestResult {
    let options = ReplayOptions {
        episode_id: "ep_test".to_string(),
        dry_run: false,
        ..Default::default()
    };

    let report = replay_episode(&options).map_err(|e| e.message())?;
    let json = report.to_json();

    ensure_contains(&json, "schema", "has schema")?;
    ensure_contains(&json, "episode_id", "has episode_id")?;
    ensure_contains(&json, "replay_id", "has replay_id")?;
    ensure_contains(&json, "status", "has status")?;
    ensure_contains(
        &json,
        "replay_evidence_available",
        "has replay_evidence_available",
    )?;
    ensure_contains(&json, "missing_frozen_inputs", "has missing_frozen_inputs")?;
    ensure_not_contains(&json, "outcome_matches", "no outcome comparison field")
}

#[test]
fn counterfactual_report_json_includes_hypothesis_records() -> TestResult {
    let options = CounterfactualOptions {
        episode_id: "ep_test".to_string(),
        interventions: vec![InterventionSpec::add_memory("context")],
        generate_hypotheses: true,
        dry_run: false,
        ..Default::default()
    };

    let report = run_counterfactual(&options).map_err(|e| e.message())?;
    let json = report.to_json();

    ensure_contains(&json, "hypothesis_records", "has hypothesis_records array")?;
    ensure_contains(
        &json,
        "intervention_type",
        "hypothesis record has intervention_type",
    )?;
    ensure_not_contains(&json, "would_have", "no would-have fields")?;
    ensure_not_contains(&json, "outcome_changed", "no outcome change field")
}

// ============================================================================
// Golden Tests
// ============================================================================

#[test]
fn capture_dry_run_matches_golden() -> TestResult {
    let options = CaptureOptions {
        workspace: PathBuf::from("/test/workspace"),
        task_input: Some("fix the bug".to_string()),
        session_id: Some("session_golden".to_string()),
        dry_run: true,
        ..Default::default()
    };

    let report = capture_episode(&options).map_err(|e| e.message())?;

    let normalized = normalize_for_golden(&report.to_json_pretty());
    assert_golden("capture_dry_run", &normalized)
}

#[test]
fn gate15_capture_episode_lab_golden_matches() -> TestResult {
    let options = CaptureOptions {
        workspace: PathBuf::from("/test/workspace"),
        task_input: Some("fix the bug".to_string()),
        session_id: Some("session_golden".to_string()),
        dry_run: false,
        ..Default::default()
    };

    let report = capture_episode(&options).map_err(|e| e.message())?;
    let normalized = normalize_for_golden(&render_lab_capture_json(&report));
    assert_lab_golden("capture_episode", &(normalized + "\n"))
}

#[test]
fn gate15_replay_baseline_lab_golden_matches() -> TestResult {
    let options = ReplayOptions {
        episode_id: "ep_golden_test".to_string(),
        dry_run: false,
        verify_hash: true,
        ..Default::default()
    };

    let report = replay_episode(&options).map_err(|e| e.message())?;
    let normalized = normalize_for_golden(&render_lab_replay_json(&report));
    assert_lab_golden("replay_baseline", &(normalized + "\n"))
}

#[test]
fn counterfactual_with_interventions_matches_golden() -> TestResult {
    let options = CounterfactualOptions {
        episode_id: "ep_golden_test".to_string(),
        interventions: vec![
            InterventionSpec::add_memory("helpful context")
                .with_hypothesis("Adding context prevents failure"),
            InterventionSpec::remove_memory("mem_noisy"),
        ],
        generate_hypotheses: true,
        dry_run: false,
        ..Default::default()
    };

    let report = run_counterfactual(&options).map_err(|e| e.message())?;

    let normalized = normalize_for_golden(&report.to_json());
    assert_golden("counterfactual_with_interventions", &normalized)
}

#[test]
fn gate15_counterfactual_add_memory_lab_golden_matches() -> TestResult {
    let options = CounterfactualOptions {
        episode_id: "ep_golden_test".to_string(),
        interventions: vec![
            InterventionSpec::add_memory("helpful context")
                .with_hypothesis("Adding context prevents failure"),
        ],
        generate_hypotheses: true,
        dry_run: false,
        ..Default::default()
    };

    let report = run_counterfactual(&options).map_err(|e| e.message())?;
    let normalized = normalize_for_golden(&render_lab_counterfactual_json(&report));
    assert_lab_golden("counterfactual_add_memory", &(normalized + "\n"))
}

#[test]
fn gate15_hypothesis_report_lab_golden_matches() -> TestResult {
    let options = CounterfactualOptions {
        episode_id: "ep_golden_test".to_string(),
        interventions: vec![
            InterventionSpec::add_memory("missed warning"),
            InterventionSpec::strengthen_memory("mem_stale", 0.4),
            InterventionSpec::remove_memory("mem_noisy"),
            InterventionSpec::weaken_memory("mem_harmful", 0.3),
            InterventionSpec::add_memory("overfit policy correction"),
        ],
        generate_hypotheses: true,
        dry_run: false,
        ..Default::default()
    };

    let report = run_counterfactual(&options).map_err(|e| e.message())?;
    let json = serde_json::json!({
        "schema": "ee.lab.hypothesis_report.v1",
        "episode_id": report.episode_id,
        "hypothesis_records": report.hypothesis_records,
    });
    let rendered = serde_json::to_string_pretty(&json).map_err(|error| error.to_string())?;
    let normalized = normalize_for_golden(&rendered);
    assert_lab_golden("regret_report", &(normalized + "\n"))
}

#[test]
fn gate15_promote_candidates_dry_run_lab_golden_matches() -> TestResult {
    let options = CounterfactualOptions {
        episode_id: "ep_golden_test".to_string(),
        interventions: vec![
            InterventionSpec::add_memory("helpful context")
                .with_hypothesis("Adding context prevents failure"),
        ],
        generate_hypotheses: true,
        dry_run: false,
        ..Default::default()
    };

    let report = run_counterfactual(&options).map_err(|e| e.message())?;
    let json = serde_json::json!({
        "schema": "ee.lab.promote_candidates_dry_run.v1",
        "episode_id": report.episode_id,
        "dry_run": true,
        "candidates": report.curation_candidates,
        "next_action": report.next_action,
    });
    let rendered = serde_json::to_string_pretty(&json).map_err(|error| error.to_string())?;
    let normalized = normalize_for_golden(&rendered);
    assert_lab_golden("promote_candidates_dry_run", &(normalized + "\n"))
}

#[test]
fn reconstruct_full_matches_golden() -> TestResult {
    let options = ReconstructOptions {
        run_id: "run_golden".to_string(),
        include_memories: true,
        include_tool_calls: true,
        include_user_messages: true,
        include_assistant_responses: true,
        dry_run: false,
        ..Default::default()
    };

    let report = reconstruct_episode(&options).map_err(|e| e.message())?;

    let normalized = normalize_for_golden(&report.to_json_pretty());
    assert_golden("reconstruct_full", &normalized)
}

fn normalize_for_golden(json: &str) -> String {
    let mut normalized = json.to_string();

    normalized = regex_replace(
        &normalized,
        r#""episode_id"\s*:\s*"ep_[a-f0-9]+""#,
        r#""episode_id": "ep_NORMALIZED""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""run_id"\s*:\s*"cfr_[a-f0-9]+""#,
        r#""run_id": "cfr_NORMALIZED""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""run_id"\s*:\s*"run_[^"]+""#,
        r#""run_id": "run_NORMALIZED""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""replay_id"\s*:\s*"rpl_[a-f0-9]+""#,
        r#""replay_id": "rpl_NORMALIZED""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""id"\s*:\s*"hyprec_[a-f0-9_]+""#,
        r#""id": "hyprec_NORMALIZED""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""candidate_id"\s*:\s*"cand_[a-f0-9_]+""#,
        r#""candidate_id": "cand_NORMALIZED""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""captured_at"\s*:\s*"[^"]+""#,
        r#""captured_at": "TIMESTAMP""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""replayed_at"\s*:\s*"[^"]+""#,
        r#""replayed_at": "TIMESTAMP""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""analyzed_at"\s*:\s*"[^"]+""#,
        r#""analyzed_at": "TIMESTAMP""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""reconstructed_at"\s*:\s*"[^"]+""#,
        r#""reconstructed_at": "TIMESTAMP""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""timestamp"\s*:\s*"[^"]+""#,
        r#""timestamp": "TIMESTAMP""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""episode_hash"\s*:\s*"blake3:[a-f0-9]+""#,
        r#""episode_hash": "blake3:NORMALIZED""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""pack_hash"\s*:\s*"blake3:[a-f0-9]+""#,
        r#""pack_hash": "blake3:NORMALIZED""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""observed_pack_hash"\s*:\s*"blake3:[a-f0-9]+""#,
        r#""observed_pack_hash": "blake3:NORMALIZED""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""counterfactual_pack_hash"\s*:\s*"blake3:[a-f0-9]+""#,
        r#""counterfactual_pack_hash": "blake3:NORMALIZED""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""repository_fingerprint"\s*:\s*"repo:[a-f0-9]+""#,
        r#""repository_fingerprint": "repo:NORMALIZED""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""episodeHash"\s*:\s*"blake3:[a-f0-9]+""#,
        r#""episodeHash": "blake3:NORMALIZED""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""packHash"\s*:\s*"blake3:[a-f0-9]+""#,
        r#""packHash": "blake3:NORMALIZED""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""observedPackHash"\s*:\s*"blake3:[a-f0-9]+""#,
        r#""observedPackHash": "blake3:NORMALIZED""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""counterfactualPackHash"\s*:\s*"blake3:[a-f0-9]+""#,
        r#""counterfactualPackHash": "blake3:NORMALIZED""#,
    );
    normalized = regex_replace(
        &normalized,
        r#""repositoryFingerprint"\s*:\s*"repo:[a-f0-9]+""#,
        r#""repositoryFingerprint": "repo:NORMALIZED""#,
    );
    normalized = regex_replace(
        &normalized,
        r#"task_input:[a-f0-9]+"#,
        "task_input:NORMALIZED",
    );
    normalized = regex_replace(
        &normalized,
        r#"pack:blake3:[a-f0-9]+"#,
        "pack:blake3:NORMALIZED",
    );
    normalized = regex_replace(&normalized, r#"repo:[a-f0-9]+"#, "repo:NORMALIZED");

    normalized
}

#[allow(clippy::expect_used)]
fn regex_replace(text: &str, pattern: &str, replacement: &str) -> String {
    use regex_lite::Regex;
    use std::borrow::Cow;
    let re = Regex::new(pattern).expect("valid regex pattern");
    match re.replace_all(text, replacement) {
        Cow::Borrowed(_) => text.to_string(),
        Cow::Owned(s) => s,
    }
}
