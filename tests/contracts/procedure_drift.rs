//! Procedure drift detection contract coverage (EE-415).
//!
//! Freezes the public JSON shape for failed-verification, stale-evidence, and
//! dependency-contract drift signals. The fixture uses fixed IDs and
//! timestamps so drift decisions remain deterministic and auditable.

use ee::core::procedure::{
    PROCEDURE_DRIFT_REPORT_SCHEMA_V1, PROCEDURE_VERIFY_REPORT_SCHEMA_V1,
    ProcedureDependencyContractInput, ProcedureDriftEvidenceInput, ProcedureDriftOptions,
    ProcedureVerifyReport, StepVerificationResult, VerificationSourceResult,
    detect_procedure_drift,
};
use ee::output::render_procedure_drift_json;
use serde_json::Value as JsonValue;
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

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn ensure_json_equal(actual: Option<&JsonValue>, expected: JsonValue, context: &str) -> TestResult {
    let actual = actual.ok_or_else(|| format!("{context}: missing JSON field"))?;
    ensure_equal(actual, &expected, context)
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("procedure")
        .join(format!("{name}.json.golden"))
}

fn pretty_rendered_json(rendered: &str) -> Result<(JsonValue, String), String> {
    let value: JsonValue =
        serde_json::from_str(rendered).map_err(|error| format!("json parse failed: {error}"))?;
    let pretty = serde_json::to_string_pretty(&value)
        .map_err(|error| format!("json render failed: {error}"))?;
    Ok((value, pretty))
}

fn assert_golden(name: &str, actual: &str) -> TestResult {
    let path = golden_path(name);
    if env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!("failed to create golden dir {}: {error}", parent.display())
            })?;
        }
        fs::write(&path, actual)
            .map_err(|error| format!("failed to write golden {}: {error}", path.display()))?;
        eprintln!("Updated golden file: {}", path.display());
        return Ok(());
    }

    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("missing golden {}: {error}", path.display()))?;
    let expected = expected.strip_suffix('\n').unwrap_or(&expected);
    ensure(
        actual == expected,
        format!(
            "procedure drift golden mismatch for {name}\n--- expected\n{expected}\n+++ actual\n{actual}"
        ),
    )
}

fn failed_verification_fixture() -> ProcedureVerifyReport {
    ProcedureVerifyReport {
        schema: PROCEDURE_VERIFY_REPORT_SCHEMA_V1.to_string(),
        procedure_id: "proc_gate18_candidate".to_string(),
        verification_id: "ver_gate18_failed".to_string(),
        status: "failed".to_string(),
        source_kind: "eval_fixture".to_string(),
        sources_checked: vec![VerificationSourceResult {
            source_id: "fixture_gate18_regression".to_string(),
            source_kind: "eval_fixture".to_string(),
            result: "failed".to_string(),
            step_results: vec![StepVerificationResult {
                step_id: "step_gate18_verify".to_string(),
                sequence: 2,
                result: "failed".to_string(),
                expected: Some("verification gates passed".to_string()),
                actual: Some("clippy failed".to_string()),
            }],
            message: Some("verification gate failed after dependency update".to_string()),
        }],
        pass_count: 0,
        fail_count: 1,
        skip_count: 0,
        overall_result: "failed".to_string(),
        verified_at: "2026-05-01T10:30:00Z".to_string(),
        dry_run: true,
        confidence: 0.0,
        next_actions: Vec::new(),
    }
}

fn drift_options_fixture() -> ProcedureDriftOptions {
    ProcedureDriftOptions {
        procedure_id: "proc_gate18_candidate".to_string(),
        checked_at: Some("2026-05-01T12:00:00Z".to_string()),
        staleness_threshold_days: 30,
        verification: Some(failed_verification_fixture()),
        evidence: vec![
            ProcedureDriftEvidenceInput {
                evidence_id: "ev_gate18_ci_trace".to_string(),
                last_seen_at: "2026-03-20T12:00:00Z".to_string(),
                source_kind: "recorder_run".to_string(),
            },
            ProcedureDriftEvidenceInput {
                evidence_id: "ev_gate18_recent_review".to_string(),
                last_seen_at: "2026-04-30T12:00:00Z".to_string(),
                source_kind: "curation_event".to_string(),
            },
        ],
        dependency_contracts: vec![ProcedureDependencyContractInput {
            dependency_name: "cass".to_string(),
            owning_surface: "procedure verification import".to_string(),
            expected_contract: "cass.robot.v1:source-map".to_string(),
            actual_contract: "cass.robot.v2:event-map".to_string(),
            compatibility: "breaking".to_string(),
        }],
        dry_run: true,
        ..Default::default()
    }
}

#[test]
fn procedure_drift_detects_failed_stale_and_dependency_signals() -> TestResult {
    let report = detect_procedure_drift(&drift_options_fixture())
        .map_err(|error| format!("drift detection failed: {}", error.message()))?;
    let rendered = render_procedure_drift_json(&report);
    let (value, pretty) = pretty_rendered_json(&rendered)?;

    ensure_json_equal(
        value.get("schema"),
        serde_json::json!(ee::models::RESPONSE_SCHEMA_V1),
        "response schema",
    )?;
    ensure_json_equal(
        value.get("data").and_then(|data| data.get("schema")),
        serde_json::json!(PROCEDURE_DRIFT_REPORT_SCHEMA_V1),
        "drift schema",
    )?;
    ensure_json_equal(
        value.get("data").and_then(|data| data.get("status")),
        serde_json::json!("drifted"),
        "drift status",
    )?;
    ensure_json_equal(
        value
            .get("data")
            .and_then(|data| data.get("mutation"))
            .and_then(|mutation| mutation.get("applied")),
        serde_json::json!(false),
        "mutation applied",
    )?;
    ensure_json_equal(
        value
            .get("data")
            .and_then(|data| data.get("counts"))
            .and_then(|counts| counts.get("total")),
        serde_json::json!(3),
        "signal count",
    )?;
    ensure(
        !pretty.contains("sk_live") && !pretty.contains("secret"),
        "drift output must not include raw secret-shaped payloads",
    )?;
    assert_golden("procedure_drift_detection", &pretty)
}
