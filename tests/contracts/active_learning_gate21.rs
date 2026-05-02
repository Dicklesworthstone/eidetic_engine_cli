//! Gate 21 active learning agenda contract coverage.
//!
//! Freezes the public JSON contracts for agenda, uncertainty, and safe
//! experiment proposal outputs using fixed fixtures. The real CLI smoke below
//! separately proves machine output stays on stdout without diagnostics.

use ee::core::learn::{
    AgendaItem, LEARN_AGENDA_SCHEMA_V1, LEARN_EXPERIMENT_PROPOSAL_SCHEMA_V1,
    LEARN_UNCERTAINTY_SCHEMA_V1, LearnAgendaReport, LearnExperimentProposeOptions,
    LearnUncertaintyReport, UncertaintyItem, propose_experiments,
};
use ee::models::ExperimentSafetyBoundary;
use ee::output::{
    render_learn_agenda_json, render_learn_experiment_proposal_json, render_learn_uncertainty_json,
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
    let mut report = propose_experiments(&LearnExperimentProposeOptions {
        limit: 2,
        min_expected_value: 0.3,
        max_attention_tokens: 800,
        max_runtime_seconds: 180,
        safety_boundary: ExperimentSafetyBoundary::HumanReview,
        ..LearnExperimentProposeOptions::default()
    })
    .map_err(|error| error.message())?;
    report.generated_at = FIXED_TIME.to_string();
    Ok(render_learn_experiment_proposal_json(&report))
}

fn run_ee(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
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
fn gate21_learn_agenda_cli_json_keeps_diagnostics_off_stdout() -> TestResult {
    let output = run_ee(&["--json", "learn", "agenda", "--limit", "2"])?;
    ensure(output.status.success(), "learn agenda CLI should succeed")?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "learn agenda --json must keep diagnostics off stderr for clean fixture, got: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("learn agenda stdout must be UTF-8: {error}"))?;
    let value: JsonValue =
        serde_json::from_str(&stdout).map_err(|error| format!("stdout must be JSON: {error}"))?;
    ensure_json_equal(
        value.get("schema"),
        JsonValue::String(LEARN_AGENDA_SCHEMA_V1.to_string()),
        "cli schema",
    )?;
    ensure_json_equal(value.get("success"), JsonValue::Bool(true), "cli success")
}
