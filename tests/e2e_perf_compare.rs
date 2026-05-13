//! EE-mwjq.6: E2E smoke tests for perf compare CLI
//!
//! Validates that `ee perf compare --baseline <path> --candidate <path>` produces
//! correct JSON output for regression, no-regression, and error scenarios.
//!
//! NO MOCKS. Real ee binary, real fixture files.

use std::fmt::Debug;
use std::process::{Command, Output};

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
where
    T: Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn stdout_json(output: &Output) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("stdout was not JSON: {error}\nstdout: {stdout}"))
}

fn assert_response_envelope(json: &serde_json::Value, context: &str) -> TestResult {
    let schema = json
        .get("schema")
        .and_then(|s| s.as_str())
        .ok_or_else(|| format!("{context}: missing schema field"))?;
    ensure_equal(&schema, &"ee.response.v1", &format!("{context} schema"))?;

    let success = json
        .get("success")
        .and_then(|s| s.as_bool())
        .ok_or_else(|| format!("{context}: missing success field"))?;
    ensure(success, format!("{context}: expected success=true"))
}

fn assert_error_envelope(json: &serde_json::Value, context: &str) -> TestResult {
    let schema = json
        .get("schema")
        .and_then(|s| s.as_str())
        .ok_or_else(|| format!("{context}: missing schema field"))?;
    ensure_equal(&schema, &"ee.error.v2", &format!("{context} schema"))
}

fn assert_stderr_empty(output: &Output, context: &str) -> TestResult {
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        stderr.trim().is_empty(),
        format!("{context}: stderr should be empty in JSON mode, got: {stderr}"),
    )
}

#[test]
fn perf_compare_regression_detected() -> TestResult {
    let output = run_ee(&[
        "perf",
        "compare",
        "--baseline",
        "tests/fixtures/golden/perf_artifact/baseline_smoke.json",
        "--candidate",
        "tests/fixtures/golden/perf_artifact/candidate_regressed.json",
        "--json",
    ])?;

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "exit code for regression comparison",
    )?;

    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "regression comparison")?;
    assert_stderr_empty(&output, "regression comparison")?;

    let data = json.get("data").ok_or("missing data field")?;
    let report = data.get("report").ok_or("missing data.report field")?;
    let summary = report
        .get("summary")
        .ok_or("missing data.report.summary field")?;

    let result = summary
        .get("result")
        .and_then(|r| r.as_str())
        .ok_or("missing summary.result")?;
    ensure_equal(&result, &"regressed", "comparison result")?;

    let deltas = report
        .get("deltas")
        .and_then(|d| d.as_array())
        .ok_or("missing deltas array")?;
    ensure(
        !deltas.is_empty(),
        "deltas should not be empty for regression",
    )
}

#[test]
fn perf_compare_no_regression() -> TestResult {
    let output = run_ee(&[
        "perf",
        "compare",
        "--baseline",
        "tests/fixtures/golden/perf_artifact/baseline_smoke.json",
        "--candidate",
        "tests/fixtures/golden/perf_artifact/candidate_unchanged.json",
        "--json",
    ])?;

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "exit code for unchanged comparison",
    )?;

    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "unchanged comparison")?;
    assert_stderr_empty(&output, "unchanged comparison")?;

    let data = json.get("data").ok_or("missing data field")?;
    let report = data.get("report").ok_or("missing data.report field")?;
    let summary = report.get("summary").ok_or("missing summary")?;

    let result = summary
        .get("result")
        .and_then(|r| r.as_str())
        .ok_or("missing summary.result")?;
    ensure_equal(&result, &"unchanged", "comparison result")
}

#[test]
fn perf_compare_missing_baseline_returns_error() -> TestResult {
    let output = run_ee(&[
        "perf",
        "compare",
        "--baseline",
        "/nonexistent/baseline.json",
        "--candidate",
        "tests/fixtures/golden/perf_artifact/candidate_unchanged.json",
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "missing baseline should produce non-zero exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "missing baseline")?;
    assert_stderr_empty(&output, "missing baseline")
}

#[test]
fn perf_compare_missing_candidate_returns_error() -> TestResult {
    let output = run_ee(&[
        "perf",
        "compare",
        "--baseline",
        "tests/fixtures/golden/perf_artifact/baseline_smoke.json",
        "--candidate",
        "/nonexistent/candidate.json",
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "missing candidate should produce non-zero exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "missing candidate")?;
    assert_stderr_empty(&output, "missing candidate")
}

#[test]
fn perf_compare_malformed_json_returns_error() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let malformed_path = tempdir.path().join("malformed.json");
    std::fs::write(&malformed_path, "{ invalid json }").map_err(|e| e.to_string())?;

    let output = run_ee(&[
        "perf",
        "compare",
        "--baseline",
        &malformed_path.to_string_lossy(),
        "--candidate",
        "tests/fixtures/golden/perf_artifact/candidate_unchanged.json",
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "malformed JSON should produce non-zero exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_error_envelope(&json, "malformed JSON")?;
    assert_stderr_empty(&output, "malformed JSON")
}

#[test]
fn perf_compare_json_stdout_contains_only_machine_data() -> TestResult {
    let output = run_ee(&[
        "perf",
        "compare",
        "--baseline",
        "tests/fixtures/golden/perf_artifact/baseline_smoke.json",
        "--candidate",
        "tests/fixtures/golden/perf_artifact/candidate_regressed.json",
        "--json",
    ])?;

    let stdout =
        String::from_utf8(output.stdout.clone()).map_err(|e| format!("stdout not UTF-8: {e}"))?;

    ensure(
        !stdout.contains("warning:"),
        "stdout should not contain warning text",
    )?;
    ensure(
        !stdout.contains("error:"),
        "stdout should not contain error text (except in JSON)",
    )?;
    ensure(
        !stdout.contains("Note:"),
        "stdout should not contain diagnostic notes",
    )?;

    let json: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|e| format!("stdout is not valid JSON: {e}"))?;

    ensure(json.is_object(), "stdout should be a JSON object")
}

#[test]
fn perf_compare_effect_is_read_only() -> TestResult {
    let output = run_ee(&[
        "perf",
        "compare",
        "--baseline",
        "tests/fixtures/golden/perf_artifact/baseline_smoke.json",
        "--candidate",
        "tests/fixtures/golden/perf_artifact/candidate_unchanged.json",
        "--json",
    ])?;

    let json = stdout_json(&output)?;
    let data = json.get("data").ok_or("missing data field")?;
    let effect = data.get("effect").ok_or("missing effect field")?;

    let default_effect = effect
        .get("defaultEffect")
        .and_then(|e| e.as_str())
        .ok_or("missing defaultEffect")?;
    ensure_equal(&default_effect, &"read_only", "default effect")?;

    let side_effect_class = effect
        .get("sideEffectClass")
        .and_then(|e| e.as_str())
        .ok_or("missing sideEffectClass")?;
    ensure(
        side_effect_class.contains("read_only"),
        format!("sideEffectClass should contain read_only, got: {side_effect_class}"),
    )
}
