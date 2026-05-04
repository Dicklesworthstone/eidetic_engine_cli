//! Gate 21 active learning agenda contract coverage.
//!
//! Freezes the public JSON contracts for agenda, uncertainty, and safe
//! experiment proposal outputs using fixed fixtures. The real CLI smoke below
//! separately proves machine output stays on stdout without diagnostics.

use ee::core::learn::{
    AgendaItem, ExperimentBudget, ExperimentDecisionImpact, ExperimentProposal,
    ExperimentSafetyPlan, LEARN_AGENDA_SCHEMA_V1, LEARN_CLOSE_SCHEMA_V1,
    LEARN_EXPERIMENT_PROPOSAL_SCHEMA_V1, LEARN_EXPERIMENT_RUN_SCHEMA_V1, LEARN_OBSERVE_SCHEMA_V1,
    LEARN_UNCERTAINTY_SCHEMA_V1, LearnAgendaReport, LearnCloseOptions,
    LearnExperimentProposalReport, LearnExperimentRunOptions, LearnObserveOptions,
    LearnUncertaintyReport, UncertaintyItem, close_experiment, observe_experiment, run_experiment,
};
use ee::models::{ExperimentOutcomeStatus, LearningObservationSignal};
use ee::output::{
    render_learn_agenda_json, render_learn_close_json, render_learn_experiment_proposal_json,
    render_learn_experiment_run_json, render_learn_observe_json, render_learn_uncertainty_json,
};
use serde_json::Value as JsonValue;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

type TestResult = Result<(), String>;

const FIXED_TIME: &str = "2026-01-02T03:04:05Z";

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_json_equal(actual: Option<&JsonValue>, expected: JsonValue, context: &str) -> TestResult {
    let actual = actual.ok_or_else(|| format!("{context}: missing JSON field"))?;
    ensure(
        actual == &expected,
        format!("{context}: expected {expected:?}, got {actual:?}"),
    )
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("learning")
        .join(format!("{name}.json.golden"))
}

fn canonical_json(raw: &str) -> Result<String, String> {
    let value: JsonValue =
        serde_json::from_str(raw).map_err(|error| format!("output must be JSON: {error}"))?;
    serde_json::to_string_pretty(&value)
        .map(|json| format!("{json}\n"))
        .map_err(|error| format!("failed to canonicalize JSON: {error}"))
}

fn assert_json_golden(name: &str, actual: &str) -> TestResult {
    let canonical = canonical_json(actual)?;
    let path = golden_path(name);
    if env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!("failed to create golden dir {}: {error}", parent.display())
            })?;
        }
        fs::write(&path, &canonical)
            .map_err(|error| format!("failed to write golden {}: {error}", path.display()))?;
        eprintln!("Updated golden file: {}", path.display());
        return Ok(());
    }

    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("missing golden {}: {error}", path.display()))?;
    ensure(
        canonical == expected,
        format!(
            "learning golden mismatch for {name}\n--- expected\n{expected}\n+++ actual\n{canonical}"
        ),
    )
}

fn agenda_fixture() -> LearnAgendaReport {
    LearnAgendaReport {
        schema: LEARN_AGENDA_SCHEMA_V1.to_string(),
        total_gaps: 2,
        high_priority_count: 2,
        resolved_count: 0,
        generated_at: FIXED_TIME.to_string(),
        items: vec![
            AgendaItem {
                id: "gap_gate21_procedure".to_string(),
                topic: "procedure".to_string(),
                gap_description: "Promotion rules need replay-backed evidence.".to_string(),
                priority: 88,
                uncertainty: 0.74,
                source: "procedure_drift_review".to_string(),
                status: "open".to_string(),
                created_at: "2026-01-02T02:00:00Z".to_string(),
            },
            AgendaItem {
                id: "gap_gate21_economy".to_string(),
                topic: "economy".to_string(),
                gap_description: "Attention budget demotions need dry-run comparison.".to_string(),
                priority: 73,
                uncertainty: 0.61,
                source: "budget_audit".to_string(),
                status: "open".to_string(),
                created_at: "2026-01-02T02:05:00Z".to_string(),
            },
        ],
    }
}

fn uncertainty_fixture() -> LearnUncertaintyReport {
    LearnUncertaintyReport {
        schema: LEARN_UNCERTAINTY_SCHEMA_V1.to_string(),
        mean_uncertainty: 0.755,
        high_uncertainty_count: 1,
        sampling_candidates: 2,
        generated_at: FIXED_TIME.to_string(),
        items: vec![
            UncertaintyItem {
                memory_id: "mem_gate21_procedure".to_string(),
                content_preview: "Procedure promotion passed once but lacks replay evidence."
                    .to_string(),
                kind: "procedural".to_string(),
                uncertainty: 0.82,
                confidence: 0.42,
                retrieval_count: 4,
                last_accessed: Some("2026-01-02T02:15:00Z".to_string()),
            },
            UncertaintyItem {
                memory_id: "mem_gate21_budget".to_string(),
                content_preview: "Budget pruning recommendation has no counterfactual baseline."
                    .to_string(),
                kind: "economic".to_string(),
                uncertainty: 0.69,
                confidence: 0.51,
                retrieval_count: 2,
                last_accessed: None,
            },
        ],
    }
}

fn proposal_fixture() -> Result<String, String> {
    let report = LearnExperimentProposalReport {
        schema: LEARN_EXPERIMENT_PROPOSAL_SCHEMA_V1.to_string(),
        total_candidates: 3,
        returned: 2,
        min_expected_value: 0.3,
        max_attention_tokens: 800,
        max_runtime_seconds: 180,
        generated_at: FIXED_TIME.to_string(),
        proposals: vec![
            ExperimentProposal {
                experiment_id: "exp_replay_error_boundary".to_string(),
                question_id: "gap_001".to_string(),
                title: "Replay async error boundary failures".to_string(),
                hypothesis: "A dry-run replay can distinguish missing propagation evidence from a weak procedural rule.".to_string(),
                status: "proposed".to_string(),
                topic: "error_handling".to_string(),
                expected_value: 0.539,
                uncertainty_reduction: 0.32,
                confidence: 0.48,
                budget: ExperimentBudget {
                    attention_tokens: 800,
                    max_runtime_seconds: 180,
                    dry_run_required: true,
                    budget_class: "medium".to_string(),
                },
                safety: ExperimentSafetyPlan {
                    boundary: "human_review".to_string(),
                    dry_run_first: true,
                    mutation_allowed: false,
                    review_required: true,
                    stop_conditions: vec![
                        "Stop after the replay produces a pass/fail explanation or safety finding.".to_string(),
                        "Stop before any durable memory mutation; close with observe/close evidence first.".to_string(),
                    ],
                    denied_reasons: vec![],
                },
                decision_impact: ExperimentDecisionImpact {
                    decision_id: "decision_error_boundary_rule".to_string(),
                    target_artifact_ids: vec!["mem_002".to_string()],
                    current_decision: "Keep async error propagation guidance at low confidence."
                        .to_string(),
                    possible_change: "Promote or demote the rule based on replayed failure evidence."
                        .to_string(),
                    impact_score: 0.85,
                },
                evidence_ids: vec!["gap_001".to_string(), "mem_002".to_string()],
                next_command: "ee learn experiment run --dry-run --id exp_replay_error_boundary --json"
                    .to_string(),
            },
            ExperimentProposal {
                experiment_id: "exp_database_contract_fixture".to_string(),
                question_id: "gap_002".to_string(),
                title: "Run database-operation contract fixture".to_string(),
                hypothesis: "A fixture-only dry run can show whether database-operation memories need stronger integration-test evidence.".to_string(),
                status: "proposed".to_string(),
                topic: "testing".to_string(),
                expected_value: 0.392,
                uncertainty_reduction: 0.28,
                confidence: 0.55,
                budget: ExperimentBudget {
                    attention_tokens: 700,
                    max_runtime_seconds: 180,
                    dry_run_required: true,
                    budget_class: "medium".to_string(),
                },
                safety: ExperimentSafetyPlan {
                    boundary: "human_review".to_string(),
                    dry_run_first: true,
                    mutation_allowed: false,
                    review_required: true,
                    stop_conditions: vec![
                        "Stop after fixture output records stdout/stderr separation and mutation posture.".to_string(),
                        "Stop before any durable memory mutation; close with observe/close evidence first.".to_string(),
                    ],
                    denied_reasons: vec![],
                },
                decision_impact: ExperimentDecisionImpact {
                    decision_id: "decision_database_test_pattern".to_string(),
                    target_artifact_ids: vec!["mem_001".to_string()],
                    current_decision: "Treat database integration guidance as plausible but under-sampled."
                        .to_string(),
                    possible_change: "Increase confidence or request more evidence before promotion."
                        .to_string(),
                    impact_score: 0.7,
                },
                evidence_ids: vec!["gap_002".to_string(), "mem_001".to_string()],
                next_command:
                    "ee learn experiment run --dry-run --id exp_database_contract_fixture --json"
                        .to_string(),
            },
        ],
    };
    Ok(render_learn_experiment_proposal_json(&report))
}

fn experiment_run_dry_run_fixture() -> Result<String, String> {
    let mut report = run_experiment(&LearnExperimentRunOptions {
        workspace: PathBuf::new(),
        experiment_id: "exp_database_contract_fixture".to_string(),
        max_attention_tokens: 800,
        max_runtime_seconds: 180,
        dry_run: true,
    })
    .map_err(|error| error.message())?;
    report.generated_at = FIXED_TIME.to_string();
    Ok(render_learn_experiment_run_json(&report))
}

fn observe_negative_fixture() -> Result<String, String> {
    let mut report = observe_experiment(&LearnObserveOptions {
        workspace: PathBuf::new(),
        database_path: None,
        workspace_id: None,
        experiment_id: "exp_database_contract_fixture".to_string(),
        observation_id: Some("lobs_gate21_negative".to_string()),
        observed_at: Some("2026-01-02T03:05:00Z".to_string()),
        observer: Some("MistySalmon".to_string()),
        signal: LearningObservationSignal::Negative,
        measurement_name: "fixture_replay_pass".to_string(),
        measurement_value: Some(0.0),
        evidence_ids: vec![
            "fixture_async_error_boundary".to_string(),
            "mem_gate21_procedure".to_string(),
        ],
        note: Some("Replay contradicted promotion confidence.".to_string()),
        redaction_status: Some("redacted".to_string()),
        session_id: Some("sess_gate21".to_string()),
        event_id: None,
        actor: Some("MistySalmon".to_string()),
        dry_run: true,
    })
    .map_err(|error| error.message())?;
    report.generated_at = FIXED_TIME.to_string();
    Ok(render_learn_observe_json(&report))
}

fn close_inconclusive_fixture() -> Result<String, String> {
    let mut report = close_experiment(&LearnCloseOptions {
        workspace: PathBuf::new(),
        database_path: None,
        workspace_id: None,
        experiment_id: "exp_database_contract_fixture".to_string(),
        outcome_id: Some("lout_gate21_inconclusive".to_string()),
        closed_at: Some("2026-01-02T03:06:00Z".to_string()),
        status: ExperimentOutcomeStatus::Inconclusive,
        decision_impact: "Insufficient confidence to promote policy change yet.".to_string(),
        confidence_delta: -0.12,
        priority_delta: -3,
        promoted_artifact_ids: vec![],
        demoted_artifact_ids: vec!["mem_gate21_budget".to_string()],
        safety_notes: vec![
            "ask_before_acting".to_string(),
            "requires_human_risk_tolerance".to_string(),
        ],
        audit_ids: vec!["audit_gate21_001".to_string()],
        session_id: Some("sess_gate21".to_string()),
        event_id: None,
        actor: Some("MistySalmon".to_string()),
        dry_run: true,
    })
    .map_err(|error| error.message())?;
    report.generated_at = FIXED_TIME.to_string();
    Ok(render_learn_close_json(&report))
}

fn run_ee(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn unique_workspace_dir(prefix: &str) -> Result<PathBuf, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-e2e")
        .join(prefix)
        .join(format!("{}-{now}", std::process::id()));
    fs::create_dir_all(&path).map_err(|error| error.to_string())?;
    Ok(path)
}

#[test]
fn gate21_agenda_json_matches_golden() -> TestResult {
    let rendered = render_learn_agenda_json(&agenda_fixture());
    let value: JsonValue =
        serde_json::from_str(&rendered).map_err(|error| format!("agenda JSON: {error}"))?;
    ensure_json_equal(
        value.get("schema"),
        JsonValue::String(LEARN_AGENDA_SCHEMA_V1.to_string()),
        "agenda schema",
    )?;
    ensure_json_equal(
        value.get("success"),
        JsonValue::Bool(true),
        "agenda success",
    )?;
    assert_json_golden("agenda", &rendered)
}

#[test]
fn gate21_uncertainty_json_matches_golden() -> TestResult {
    let rendered = render_learn_uncertainty_json(&uncertainty_fixture());
    let value: JsonValue =
        serde_json::from_str(&rendered).map_err(|error| format!("uncertainty JSON: {error}"))?;
    ensure_json_equal(
        value.get("schema"),
        JsonValue::String(LEARN_UNCERTAINTY_SCHEMA_V1.to_string()),
        "uncertainty schema",
    )?;
    ensure_json_equal(
        value.get("samplingCandidates"),
        JsonValue::from(2),
        "sampling candidates",
    )?;
    assert_json_golden("uncertainty", &rendered)
}

#[test]
fn gate21_experiment_proposal_json_matches_golden() -> TestResult {
    let rendered = proposal_fixture()?;
    let value: JsonValue =
        serde_json::from_str(&rendered).map_err(|error| format!("proposal JSON: {error}"))?;
    ensure_json_equal(
        value.get("schema"),
        JsonValue::String(LEARN_EXPERIMENT_PROPOSAL_SCHEMA_V1.to_string()),
        "proposal schema",
    )?;
    ensure_json_equal(value.get("returned"), JsonValue::from(2), "proposal count")?;
    ensure(
        rendered.contains("\"human_review\""),
        "proposal output must include safety boundary",
    )?;
    assert_json_golden("experiment_proposal", &rendered)
}

#[test]
fn gate21_experiment_run_dry_run_json_matches_golden() -> TestResult {
    let rendered = experiment_run_dry_run_fixture()?;
    let value: JsonValue =
        serde_json::from_str(&rendered).map_err(|error| format!("experiment run JSON: {error}"))?;
    ensure_json_equal(
        value.get("schema"),
        JsonValue::String(LEARN_EXPERIMENT_RUN_SCHEMA_V1.to_string()),
        "experiment run schema",
    )?;
    ensure_json_equal(
        value.get("dryRun"),
        JsonValue::Bool(true),
        "experiment run dryRun",
    )?;
    ensure(
        value
            .get("steps")
            .and_then(JsonValue::as_array)
            .is_some_and(|steps| !steps.is_empty()),
        "experiment run steps must not be empty",
    )?;
    assert_json_golden("experiment_run_dry_run", &rendered)
}

#[test]
fn gate21_observe_negative_result_json_matches_golden() -> TestResult {
    let rendered = observe_negative_fixture()?;
    let value: JsonValue =
        serde_json::from_str(&rendered).map_err(|error| format!("observe JSON: {error}"))?;
    ensure_json_equal(
        value.get("schema"),
        JsonValue::String(LEARN_OBSERVE_SCHEMA_V1.to_string()),
        "observe schema",
    )?;
    ensure_json_equal(value.get("dryRun"), JsonValue::Bool(true), "observe dryRun")?;
    ensure_json_equal(
        value.pointer("/observation/signal"),
        JsonValue::String("negative".to_string()),
        "observe signal",
    )?;
    assert_json_golden("observe_negative_result", &rendered)
}

#[test]
fn gate21_close_inconclusive_json_matches_golden() -> TestResult {
    let rendered = close_inconclusive_fixture()?;
    let value: JsonValue =
        serde_json::from_str(&rendered).map_err(|error| format!("close JSON: {error}"))?;
    ensure_json_equal(
        value.get("schema"),
        JsonValue::String(LEARN_CLOSE_SCHEMA_V1.to_string()),
        "close schema",
    )?;
    ensure_json_equal(value.get("dryRun"), JsonValue::Bool(true), "close dryRun")?;
    ensure_json_equal(
        value.pointer("/outcome/status"),
        JsonValue::String("inconclusive".to_string()),
        "close outcome status",
    )?;
    ensure(
        value
            .pointer("/outcome/safetyNotes")
            .and_then(JsonValue::as_array)
            .is_some_and(|notes| notes.iter().any(|note| note == "ask_before_acting")),
        "inconclusive close must retain ask_before_acting safety note",
    )?;
    assert_json_golden("close_inconclusive", &rendered)
}

#[test]
fn gate21_learn_cli_json_keeps_diagnostics_off_stdout() -> TestResult {
    let output = run_ee(&["--json", "learn", "agenda", "--limit", "2"])?;
    ensure(
        output.status.code() == Some(6),
        format!(
            "learn agenda CLI should report degraded unavailable exit 6, got {:?}",
            output.status.code()
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "learn agenda --json must keep diagnostics off stderr for degraded output, got: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("learn agenda stdout must be UTF-8: {error}"))?;
    let value: JsonValue =
        serde_json::from_str(&stdout).map_err(|error| format!("stdout must be JSON: {error}"))?;
    ensure_json_equal(
        value.get("schema"),
        JsonValue::String("ee.response.v1".to_string()),
        "cli schema",
    )?;
    ensure_json_equal(value.get("success"), JsonValue::Bool(false), "cli success")?;
    ensure_json_equal(
        value.pointer("/data/code"),
        JsonValue::String("learning_records_unavailable".to_string()),
        "agenda degraded code",
    )?;

    let workspace = unique_workspace_dir("gate21-cli-dry-run")?;
    let workspace_arg = workspace.to_string_lossy().into_owned();

    let run = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "learn",
        "experiment",
        "run",
        "--id",
        "exp_database_contract_fixture",
        "--dry-run",
    ])?;
    ensure(
        run.status.success(),
        format!(
            "learn experiment run dry-run should succeed, got {:?}",
            run.status.code()
        ),
    )?;
    ensure(
        run.stderr.is_empty(),
        format!(
            "learn experiment run --json must keep diagnostics off stderr, got: {}",
            String::from_utf8_lossy(&run.stderr)
        ),
    )?;
    let run_value: JsonValue = serde_json::from_slice(&run.stdout)
        .map_err(|error| format!("learn experiment run stdout must be JSON: {error}"))?;
    ensure_json_equal(
        run_value.get("schema"),
        JsonValue::String("ee.learn.experiment_run.v1".to_string()),
        "run schema",
    )?;
    ensure_json_equal(run_value.get("dryRun"), JsonValue::Bool(true), "run dryRun")?;
    ensure_json_equal(
        run_value.get("status"),
        JsonValue::String("dry_run".to_string()),
        "run status",
    )?;

    let observe = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "learn",
        "observe",
        "exp_database_contract_fixture",
        "--signal",
        "negative",
        "--measurement-name",
        "fixture_replay_pass",
        "--measurement-value",
        "0",
        "--evidence-id",
        "fixture_async_error_boundary",
        "--redaction-status",
        "redacted",
        "--note",
        "Replay contradicted promotion confidence.",
        "--dry-run",
    ])?;
    ensure(
        observe.status.success(),
        "learn observe dry-run should succeed",
    )?;
    ensure(
        observe.stderr.is_empty(),
        format!(
            "learn observe --json must keep diagnostics off stderr, got: {}",
            String::from_utf8_lossy(&observe.stderr)
        ),
    )?;
    let observe_value: JsonValue = serde_json::from_slice(&observe.stdout)
        .map_err(|error| format!("learn observe stdout must be JSON: {error}"))?;
    ensure_json_equal(
        observe_value.get("schema"),
        JsonValue::String(LEARN_OBSERVE_SCHEMA_V1.to_string()),
        "observe schema",
    )?;
    ensure_json_equal(
        observe_value.get("dryRun"),
        JsonValue::Bool(true),
        "observe dryRun",
    )?;

    let close = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "learn",
        "close",
        "exp_database_contract_fixture",
        "--status",
        "inconclusive",
        "--decision-impact",
        "Need more evidence before promotion.",
        "--confidence-delta=-0.1",
        "--priority-delta=-2",
        "--safety-note",
        "ask_before_acting",
        "--dry-run",
    ])?;
    ensure(close.status.success(), "learn close dry-run should succeed")?;
    ensure(
        close.stderr.is_empty(),
        format!(
            "learn close --json must keep diagnostics off stderr, got: {}",
            String::from_utf8_lossy(&close.stderr)
        ),
    )?;
    let close_value: JsonValue = serde_json::from_slice(&close.stdout)
        .map_err(|error| format!("learn close stdout must be JSON: {error}"))?;
    ensure_json_equal(
        close_value.get("schema"),
        JsonValue::String(LEARN_CLOSE_SCHEMA_V1.to_string()),
        "close schema",
    )?;
    ensure_json_equal(
        close_value.get("dryRun"),
        JsonValue::Bool(true),
        "close dryRun",
    )?;
    ensure(
        close_value
            .pointer("/outcome/safetyNotes")
            .and_then(JsonValue::as_array)
            .is_some_and(|notes| notes.iter().any(|note| note == "ask_before_acting")),
        "learn close dry-run must preserve ask_before_acting safety note",
    )?;

    ensure(
        !workspace.join(".ee").exists(),
        "learn dry-run CLI commands must not mutate storage",
    )
}
