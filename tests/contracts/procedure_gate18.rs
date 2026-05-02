//! Gate 18 procedure distillation contract coverage.
//!
//! Freezes the public JSON shape for procedure proposal, detail, skill capsule
//! export, and verification outputs. Fixtures use fixed IDs and timestamps so
//! procedure distillation remains auditable without relying on wall-clock data.

use ee::core::procedure::{
    PROCEDURE_EXPORT_REPORT_SCHEMA_V1, PROCEDURE_PROPOSE_REPORT_SCHEMA_V1,
    PROCEDURE_SHOW_REPORT_SCHEMA_V1, PROCEDURE_VERIFY_REPORT_SCHEMA_V1, ProcedureDetail,
    ProcedureExportReport, ProcedureProposeReport, ProcedureShowReport, ProcedureStepDetail,
    ProcedureVerifyReport, StepVerificationResult, VerificationDetail, VerificationSourceResult,
};
use ee::output::{
    render_procedure_export_json, render_procedure_propose_json, render_procedure_show_json,
};
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
            "procedure golden mismatch for {name}\n--- expected\n{expected}\n+++ actual\n{actual}"
        ),
    )
}

fn procedure_propose_fixture() -> ProcedureProposeReport {
    ProcedureProposeReport {
        schema: PROCEDURE_PROPOSE_REPORT_SCHEMA_V1.to_string(),
        procedure_id: "proc_gate18_candidate".to_string(),
        title: "Stabilize release workflow".to_string(),
        summary: "Evidence-backed workflow for release verification.".to_string(),
        status: "candidate".to_string(),
        source_run_count: 2,
        evidence_count: 3,
        dry_run: true,
        created_at: "2026-01-02T03:04:05Z".to_string(),
    }
}

fn procedure_show_fixture() -> ProcedureShowReport {
    ProcedureShowReport {
        schema: PROCEDURE_SHOW_REPORT_SCHEMA_V1.to_string(),
        procedure: ProcedureDetail {
            procedure_id: "proc_gate18_candidate".to_string(),
            title: "Stabilize release workflow".to_string(),
            summary: "Evidence-backed workflow for release verification.".to_string(),
            status: "candidate".to_string(),
            step_count: 2,
            source_run_ids: vec![
                "run_gate18_success_001".to_string(),
                "run_gate18_success_002".to_string(),
            ],
            evidence_ids: vec![
                "ev_gate18_release_log".to_string(),
                "ev_gate18_ci_trace".to_string(),
            ],
            created_at: "2026-01-02T03:04:05Z".to_string(),
            updated_at: "2026-01-02T03:05:06Z".to_string(),
            verified_at: None,
        },
        steps: vec![
            ProcedureStepDetail {
                step_id: "step_gate18_prepare".to_string(),
                sequence: 1,
                title: "Prepare release workspace".to_string(),
                instruction: "Inspect workspace status and confirm no unrelated edits are staged."
                    .to_string(),
                command_hint: Some("git status --short".to_string()),
                required: true,
            },
            ProcedureStepDetail {
                step_id: "step_gate18_verify".to_string(),
                sequence: 2,
                title: "Run verification gates".to_string(),
                instruction:
                    "Run formatter, clippy, focused tests, and artifact checks through RCH."
                        .to_string(),
                command_hint: Some(
                    "rch exec -- cargo test release_gate -- --test-threads=1".to_string(),
                ),
                required: true,
            },
        ],
        verification: Some(VerificationDetail {
            status: "pending".to_string(),
            verified_at: None,
            verified_by: None,
            pass_count: 0,
            fail_count: 0,
        }),
    }
}

fn procedure_export_fixture() -> ProcedureExportReport {
    let content = concat!(
        "---\n",
        "schema: \"ee.skill_capsule.v1\"\n",
        "capsule_id: \"capsule_gate18_release\"\n",
        "procedure_id: \"proc_gate18_candidate\"\n",
        "install_mode: \"render_only\"\n",
        "---\n\n",
        "# Stabilize release workflow\n\n",
        "Render-only capsule. No automatic installation occurs.\n"
    )
    .to_string();
    let content_length = content.len();

    ProcedureExportReport {
        schema: PROCEDURE_EXPORT_REPORT_SCHEMA_V1.to_string(),
        export_id: "exp_gate18_skill".to_string(),
        procedure_id: "proc_gate18_candidate".to_string(),
        format: "skill_capsule".to_string(),
        artifact_kind: "skill_capsule".to_string(),
        output_path: None,
        content,
        content_length,
        content_hash: "blake3:gate18skillcapsulehash".to_string(),
        includes_evidence: true,
        redaction_status: "not_required".to_string(),
        install_mode: Some("render_only".to_string()),
        warnings: vec![
            "skill capsule is render-only; no files are installed".to_string(),
            "manual review is required before copying into a skill directory".to_string(),
        ],
        exported_at: "2026-01-02T03:06:07Z".to_string(),
    }
}

fn procedure_verify_fixture() -> ProcedureVerifyReport {
    ProcedureVerifyReport {
        schema: PROCEDURE_VERIFY_REPORT_SCHEMA_V1.to_string(),
        procedure_id: "proc_gate18_candidate".to_string(),
        verification_id: "ver_gate18_release".to_string(),
        status: "passed".to_string(),
        source_kind: "eval_fixture".to_string(),
        sources_checked: vec![VerificationSourceResult {
            source_id: "fixture_gate18_release".to_string(),
            source_kind: "eval_fixture".to_string(),
            result: "passed".to_string(),
            step_results: vec![
                StepVerificationResult {
                    step_id: "step_gate18_prepare".to_string(),
                    sequence: 1,
                    result: "passed".to_string(),
                    expected: Some("workspace inspected".to_string()),
                    actual: Some("workspace inspected".to_string()),
                },
                StepVerificationResult {
                    step_id: "step_gate18_verify".to_string(),
                    sequence: 2,
                    result: "passed".to_string(),
                    expected: Some("verification gates passed".to_string()),
                    actual: Some("verification gates passed".to_string()),
                },
            ],
            message: None,
        }],
        pass_count: 1,
        fail_count: 0,
        skip_count: 0,
        overall_result: "passed".to_string(),
        verified_at: "2026-01-02T03:07:08Z".to_string(),
        dry_run: true,
        confidence: 1.0,
        next_actions: Vec::new(),
    }
}

#[test]
fn gate18_procedure_propose_matches_golden() -> TestResult {
    let rendered = render_procedure_propose_json(&procedure_propose_fixture());
    let (value, pretty) = pretty_rendered_json(&rendered)?;

    ensure_json_equal(
        value.get("schema"),
        serde_json::json!(PROCEDURE_PROPOSE_REPORT_SCHEMA_V1),
        "propose schema",
    )?;
    ensure_json_equal(
        value.get("dryRun"),
        serde_json::json!(true),
        "propose dry run",
    )?;
    ensure(
        !pretty.contains("raw recorder transcript"),
        "proposal output must not expose raw trace payload text",
    )?;
    assert_golden("gate18_procedure_propose", &pretty)
}

#[test]
fn gate18_procedure_show_matches_golden() -> TestResult {
    let rendered = render_procedure_show_json(&procedure_show_fixture());
    let (value, pretty) = pretty_rendered_json(&rendered)?;

    ensure_json_equal(
        value.get("schema"),
        serde_json::json!(PROCEDURE_SHOW_REPORT_SCHEMA_V1),
        "show schema",
    )?;
    let step_count = value
        .get("steps")
        .and_then(JsonValue::as_array)
        .map(Vec::len);
    ensure_equal(&step_count, &Some(2usize), "show step count")?;
    ensure_json_equal(
        value
            .get("verification")
            .and_then(|verification| verification.get("status")),
        serde_json::json!("pending"),
        "verification status",
    )?;
    assert_golden("gate18_procedure_show", &pretty)
}

#[test]
fn gate18_procedure_export_skill_capsule_matches_golden() -> TestResult {
    let rendered = render_procedure_export_json(&procedure_export_fixture());
    let (value, pretty) = pretty_rendered_json(&rendered)?;

    ensure_json_equal(
        value.get("schema"),
        serde_json::json!(ee::models::RESPONSE_SCHEMA_V1),
        "export response schema",
    )?;
    ensure_json_equal(
        value.get("data").and_then(|data| data.get("installMode")),
        serde_json::json!("render_only"),
        "skill capsule install mode",
    )?;
    ensure(
        value
            .get("data")
            .and_then(|data| data.get("content"))
            .and_then(JsonValue::as_str)
            .is_some_and(|content| content.contains("install_mode: \"render_only\"")),
        "skill capsule content must stay render-only",
    )?;
    ensure(
        !pretty.contains("automatic install enabled"),
        "skill capsule export must not imply automatic installation",
    )?;
    assert_golden("gate18_procedure_export_skill_capsule", &pretty)
}

#[test]
fn gate18_procedure_verify_matches_golden() -> TestResult {
    let rendered = procedure_verify_fixture().to_json();
    let (value, pretty) = pretty_rendered_json(&rendered)?;

    ensure_json_equal(
        value.get("schema"),
        serde_json::json!(PROCEDURE_VERIFY_REPORT_SCHEMA_V1),
        "verify schema",
    )?;
    ensure_json_equal(
        value.get("dry_run"),
        serde_json::json!(true),
        "verify dry run",
    )?;
    ensure_json_equal(
        value.get("overall_result"),
        serde_json::json!("passed"),
        "overall verification result",
    )?;
    assert_golden("gate18_procedure_verify", &pretty)
}
