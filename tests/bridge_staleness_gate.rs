#![allow(clippy::expect_used)]
// This contract test intentionally fails fast when the script output shape drifts.

//! Bridge staleness advisory gate (bd-3usjw.33 / CLOSE_THE_GAP §36).
//!
//! Drives `scripts/bridge-staleness.sh --json` and asserts the report
//! conforms to the `ee.bridge.staleness.v1` schema: required top-level
//! fields, an `inputs` block carrying the raw measurements, and a
//! `signals[]` array where each entry pins `{ code, severity, message,
//! repair }` plus the relevant `details`. The gate is non-blocking by
//! design (the script always exits 0 and the test only flags drift via
//! a single advisory assertion at the bottom of the report shape).

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use serde_json::Value;

const VALID_CODES: &[&str] = &[
    "plan_mtime_age_days",
    "vision_coverage_gap_low",
    "in_progress_beads_mtime",
];

const VALID_SEVERITIES: &[&str] = &["info", "low", "medium", "high", "critical"];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn trace_bridge_staleness_gate(phase: &'static str, elapsed_ms: u64, degraded_codes: &[&str]) {
    tracing::info!(
        workspace_id = "repo",
        request_id = "bridge_staleness_gate_contract",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-3usjw.33"),
        surface = "bridge_staleness_gate",
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "bridge staleness gate contract checkpoint"
    );
}

fn run_script() -> Result<Value, String> {
    let started = Instant::now();
    trace_bridge_staleness_gate("input", 0, &[]);
    let script = workspace_root().join("scripts").join("bridge-staleness.sh");
    let output = Command::new("bash")
        .arg(script.as_os_str())
        .arg("--json")
        .output()
        .map_err(|error| format!("failed to spawn bridge-staleness.sh: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "bridge-staleness.sh exited with status {:?}; stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim_end(),
        ));
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("bridge-staleness.sh stdout was not UTF-8: {error}"))?;
    let report = serde_json::from_str::<Value>(&stdout).map_err(|error| {
        format!("bridge-staleness.sh stdout was not JSON: {error}\nstdout: {stdout}")
    })?;
    trace_bridge_staleness_gate("response", started.elapsed().as_millis() as u64, &[]);
    Ok(report)
}

#[test]
fn bridge_staleness_report_matches_schema_v1() {
    let report = run_script().expect("bridge-staleness.sh must produce JSON");

    assert_eq!(
        report.get("schema").and_then(Value::as_str),
        Some("ee.bridge.staleness.v1"),
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
        "planPresent",
        "planAgeDays",
        "visionCoverageReportPresent",
        "partIIOpenCount",
        "partIIInProgressCount",
        "partIIMaxStaleDays",
    ] {
        assert!(
            inputs.contains_key(required),
            "inputs.{required} missing from report: {report}"
        );
    }

    let signals = report
        .get("signals")
        .and_then(Value::as_array)
        .expect("signals must be an array");
    for signal in signals {
        let code = signal
            .get("code")
            .and_then(Value::as_str)
            .expect("signal must carry code");
        assert!(
            VALID_CODES.contains(&code),
            "unknown bridge-staleness signal code {code:?}; expected one of {VALID_CODES:?}"
        );
        let severity = signal
            .get("severity")
            .and_then(Value::as_str)
            .expect("signal must carry severity");
        assert!(
            VALID_SEVERITIES.contains(&severity),
            "unknown severity {severity:?}; expected one of {VALID_SEVERITIES:?}"
        );
        assert!(
            signal
                .get("message")
                .and_then(Value::as_str)
                .is_some_and(|message| !message.is_empty()),
            "signal {code:?} must carry non-empty message"
        );
        assert!(
            signal.get("repair").and_then(Value::as_str).is_some(),
            "signal {code:?} must carry repair hint"
        );
        assert!(
            signal.get("details").and_then(Value::as_object).is_some(),
            "signal {code:?} must carry details object"
        );
    }
}

#[test]
fn bridge_staleness_writes_report_file_matching_stdout() {
    let report = run_script().expect("bridge-staleness.sh must produce JSON");
    let report_path = workspace_root().join(".bridge-staleness-report.json");
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
fn bridge_staleness_inputs_are_numeric_or_boolean() {
    let report = run_script().expect("bridge-staleness.sh must produce JSON");
    let inputs = report
        .get("inputs")
        .and_then(Value::as_object)
        .expect("inputs object must be present");

    for (field, expect_bool) in [("planPresent", true), ("visionCoverageReportPresent", true)] {
        let value = inputs
            .get(field)
            .unwrap_or_else(|| panic!("inputs.{field} missing"));
        assert_eq!(
            value.is_boolean(),
            expect_bool,
            "inputs.{field} must be boolean (got {value:?})"
        );
    }

    for field in [
        "planAgeDays",
        "partIIOpenCount",
        "partIIInProgressCount",
        "partIIMaxStaleDays",
    ] {
        let value = inputs
            .get(field)
            .unwrap_or_else(|| panic!("inputs.{field} missing"));
        assert!(
            value.is_number() || value.is_null(),
            "inputs.{field} must be numeric or null (got {value:?})"
        );
        if let Some(integer) = value.as_i64() {
            assert!(
                integer >= 0,
                "inputs.{field} must be non-negative (got {integer})"
            );
        }
    }
}

#[test]
fn bridge_staleness_signal_codes_are_unique_when_present() {
    let report = run_script().expect("bridge-staleness.sh must produce JSON");
    let signals = report
        .get("signals")
        .and_then(Value::as_array)
        .expect("signals must be array");
    let mut seen = std::collections::BTreeSet::new();
    for signal in signals {
        let code = signal
            .get("code")
            .and_then(Value::as_str)
            .expect("signal code");
        assert!(
            seen.insert(code.to_owned()),
            "duplicate signal code {code} in bridge-staleness report"
        );
    }
}
