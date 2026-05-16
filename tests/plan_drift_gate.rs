#![allow(clippy::expect_used)]
// This contract test intentionally fails fast when the script output shape drifts.

//! Plan-drift advisory gate (bd-3usjw.43 / CLOSE_THE_GAP §47).
//!
//! Drives `scripts/plan-drift.sh --json --bead bd-3usjw.43` and asserts the
//! report conforms to the `ee.plan_drift.v1` schema. The gate is advisory: it
//! helps operators see stale plan/bead text before claiming work, but it does
//! not mutate Beads state or fail verification by itself.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use serde_json::{Value, json};

const VALID_CODES: &[&str] = &[
    "missing_plan_doc_section",
    "plan_doc_section_missing",
    "plan_drift_warning",
];

const VALID_SEVERITIES: &[&str] = &["info", "low", "warning", "medium", "high", "critical"];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn trace_plan_drift_gate(phase: &'static str, elapsed_ms: u64, degraded_codes: &[&str]) {
    tracing::info!(
        workspace_id = "repo",
        request_id = "plan_drift_gate_contract",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-3usjw.43"),
        surface = "plan_drift_warning",
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "plan drift gate contract checkpoint"
    );
}

fn run_script_with_args(args: &[&str]) -> Result<Value, String> {
    let started = Instant::now();
    trace_plan_drift_gate("input", 0, &[]);
    let script = workspace_root().join("scripts").join("plan-drift.sh");
    let output = Command::new("bash")
        .arg(script.as_os_str())
        .args(args)
        .output()
        .map_err(|error| format!("failed to spawn plan-drift.sh: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "plan-drift.sh exited with status {:?}; stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim_end(),
        ));
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("plan-drift.sh stdout was not UTF-8: {error}"))?;
    let report = serde_json::from_str::<Value>(&stdout)
        .map_err(|error| format!("plan-drift.sh stdout was not JSON: {error}\nstdout: {stdout}"))?;
    trace_plan_drift_gate("response", started.elapsed().as_millis() as u64, &[]);
    Ok(report)
}

fn run_script() -> Result<Value, String> {
    run_script_with_args(&["--json", "--bead", "bd-3usjw.43"])
}

#[test]
fn plan_drift_report_matches_schema_v1() {
    let report = run_script().expect("plan-drift.sh must produce JSON");

    assert_eq!(
        report.get("schema").and_then(Value::as_str),
        Some("ee.plan_drift.v1"),
        "schema field must pin v1 contract: {report}"
    );
    assert!(
        report
            .get("generatedAt")
            .and_then(Value::as_str)
            .is_some_and(|generated| generated.ends_with('Z')),
        "generatedAt must be an RFC 3339 UTC timestamp: {report}"
    );
    assert!(
        report
            .get("dataHash")
            .and_then(Value::as_str)
            .is_some_and(|hash| !hash.is_empty() && hash != "null"),
        "dataHash must be present and non-empty: {report}"
    );

    let inputs = report
        .get("inputs")
        .and_then(Value::as_object)
        .expect("inputs object must be present in report");
    for required in [
        "planPath",
        "planPresent",
        "planMtimeEpoch",
        "beadsPath",
        "beadsPresent",
        "candidateCount",
        "missingMetadataCount",
        "missingSectionCount",
        "driftWarningCount",
    ] {
        assert!(
            inputs.contains_key(required),
            "inputs.{required} missing from report: {report}"
        );
    }

    let warnings = report
        .get("warnings")
        .and_then(Value::as_array)
        .expect("warnings must be an array");
    for warning in warnings {
        let code = warning
            .get("code")
            .and_then(Value::as_str)
            .expect("warning must carry code");
        assert!(
            VALID_CODES.contains(&code),
            "unknown plan-drift warning code {code:?}; expected one of {VALID_CODES:?}"
        );
        let severity = warning
            .get("severity")
            .and_then(Value::as_str)
            .expect("warning must carry severity");
        assert!(
            VALID_SEVERITIES.contains(&severity),
            "unknown severity {severity:?}; expected one of {VALID_SEVERITIES:?}"
        );
        assert!(
            warning
                .get("beadId")
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with("bd-3usjw.")),
            "warning must identify a bd-3usjw bead: {warning}"
        );
        assert!(
            warning
                .get("message")
                .and_then(Value::as_str)
                .is_some_and(|message| !message.is_empty()),
            "warning {code:?} must carry non-empty message"
        );
        assert!(
            warning.get("repair").and_then(Value::as_str).is_some(),
            "warning {code:?} must carry repair hint"
        );
        assert!(
            warning.get("details").and_then(Value::as_object).is_some(),
            "warning {code:?} must carry details object"
        );
    }

    assert!(
        report
            .get("bvRobotTriageHints")
            .and_then(Value::as_array)
            .is_some(),
        "report must include BV-friendly triage hints: {report}"
    );
}

#[test]
fn plan_drift_writes_report_file_matching_stdout() {
    let report = run_script().expect("plan-drift.sh must produce JSON");
    let report_path = workspace_root().join(".plan-drift-report.json");
    let disk_report = fs::read_to_string(&report_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", report_path.display()));
    let disk_json = serde_json::from_str::<Value>(&disk_report)
        .unwrap_or_else(|error| panic!("disk report was not JSON: {error}"));

    assert_eq!(
        disk_json, report,
        "disk report should match stdout JSON for one script invocation"
    );
}

#[test]
fn plan_drift_fixture_emits_warning_for_changed_low_overlap_section() {
    let fixture_dir =
        std::env::temp_dir().join(format!("ee-plan-drift-fixture-{}", std::process::id()));
    fs::create_dir_all(&fixture_dir)
        .unwrap_or_else(|error| panic!("failed to create {}: {error}", fixture_dir.display()));
    let plan_path = fixture_dir.join("CLOSE_THE_GAP_PLAN.md");
    let beads_path = fixture_dir.join("issues.jsonl");
    let output_path = fixture_dir.join("plan-drift-report.json");

    fs::write(
        &plan_path,
        "# Fixture\n\n## 47. Plan-drift warning in bv triage\n\nCurrent section text mentions a fresh operator warning, BV triage hints, plan_doc_section labels, and re-reading changed sections before claiming work.\n",
    )
    .unwrap_or_else(|error| panic!("failed to write {}: {error}", plan_path.display()));

    let bead = json!({
        "id": "bd-3usjw.fixture",
        "title": "implements-surface:fixture_plan_drift",
        "status": "open",
        "created_at": "2020-01-01T00:00:00Z",
        "updated_at": "2020-01-01T00:00:00Z",
        "description": "Old unrelated wording about release packaging.",
        "labels": ["implements-surface:fixture_plan_drift", "plan_doc_section:47"]
    });
    fs::write(&beads_path, format!("{bead}\n"))
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", beads_path.display()));

    let report = run_script_with_args(&[
        "--json",
        "--plan",
        plan_path
            .to_str()
            .expect("fixture plan path must be valid UTF-8"),
        "--beads",
        beads_path
            .to_str()
            .expect("fixture beads path must be valid UTF-8"),
        "--output",
        output_path
            .to_str()
            .expect("fixture output path must be valid UTF-8"),
    ])
    .expect("plan-drift.sh fixture run must produce JSON");

    assert_eq!(
        report
            .get("inputs")
            .and_then(|inputs| inputs.get("candidateCount"))
            .and_then(Value::as_u64),
        Some(1),
        "fixture should scan one candidate: {report}"
    );
    let warnings = report
        .get("warnings")
        .and_then(Value::as_array)
        .expect("warnings must be an array");
    assert_eq!(
        warnings.len(),
        1,
        "fixture should emit one warning: {report}"
    );
    assert_eq!(
        warnings[0].get("code").and_then(Value::as_str),
        Some("plan_drift_warning"),
        "fixture warning should be the drift warning: {report}"
    );
    assert_eq!(
        report
            .get("bvRobotTriageHints")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1),
        "drift warning should create one BV triage hint: {report}"
    );
}
