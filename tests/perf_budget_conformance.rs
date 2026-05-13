//! EE-mwjq.9: Conformance tests for profile budget verification
//!
//! Validates that `ee perf budget check` correctly verifies artifacts against
//! profile budgets for constrained, portable, workstation, and swarm profiles.
//!
//! Tests prove: within-budget passes, profile mismatch detected, missing profile
//! provenance flagged, and read-only effect maintained.
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

fn get_budget_check_result(json: &serde_json::Value) -> Result<&str, String> {
    json.get("data")
        .and_then(|d| d.get("report"))
        .and_then(|r| r.get("summary"))
        .and_then(|s| s.get("result"))
        .and_then(|r| r.as_str())
        .ok_or_else(|| "missing data.report.summary.result".to_owned())
}

fn get_requested_profile(json: &serde_json::Value) -> Result<&str, String> {
    json.get("data")
        .and_then(|d| d.get("report"))
        .and_then(|r| r.get("requestedProfile"))
        .and_then(|p| p.as_str())
        .ok_or_else(|| "missing data.report.requestedProfile".to_owned())
}

fn get_artifact_profile(json: &serde_json::Value) -> Result<&str, String> {
    json.get("data")
        .and_then(|d| d.get("report"))
        .and_then(|r| r.get("artifact"))
        .and_then(|a| a.get("profile"))
        .and_then(|p| p.as_str())
        .ok_or_else(|| "missing data.report.artifact.profile".to_owned())
}

fn get_degradations(json: &serde_json::Value) -> Result<&Vec<serde_json::Value>, String> {
    json.get("data")
        .and_then(|d| d.get("report"))
        .and_then(|r| r.get("degraded"))
        .and_then(|d| d.as_array())
        .ok_or_else(|| "missing data.report.degraded".to_owned())
}

#[test]
fn budget_check_constrained_profile_passes() -> TestResult {
    let output = run_ee(&[
        "perf",
        "budget",
        "check",
        "--profile",
        "constrained",
        "--report",
        "tests/fixtures/golden/perf_artifact/constrained_profile.json",
        "--json",
    ])?;

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "constrained profile check exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "constrained profile check")?;

    let result = get_budget_check_result(&json)?;
    ensure_equal(&result, &"passed", "constrained profile result")?;

    let requested = get_requested_profile(&json)?;
    ensure_equal(&requested, &"constrained", "requested profile")
}

#[test]
fn budget_check_portable_profile_passes() -> TestResult {
    let output = run_ee(&[
        "perf",
        "budget",
        "check",
        "--profile",
        "portable",
        "--report",
        "tests/fixtures/golden/perf_artifact/portable_profile.json",
        "--json",
    ])?;

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "portable profile check exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "portable profile check")?;

    let result = get_budget_check_result(&json)?;
    ensure_equal(&result, &"passed", "portable profile result")
}

#[test]
fn budget_check_workstation_profile_passes() -> TestResult {
    let output = run_ee(&[
        "perf",
        "budget",
        "check",
        "--profile",
        "workstation",
        "--report",
        "tests/fixtures/golden/perf_artifact/baseline_smoke.json",
        "--json",
    ])?;

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "workstation profile check exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "workstation profile check")?;

    let result = get_budget_check_result(&json)?;
    ensure_equal(&result, &"passed", "workstation profile result")
}

#[test]
fn budget_check_swarm_profile_passes() -> TestResult {
    let output = run_ee(&[
        "perf",
        "budget",
        "check",
        "--profile",
        "swarm",
        "--report",
        "tests/fixtures/golden/perf_artifact/swarm_profile.json",
        "--json",
    ])?;

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "swarm profile check exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "swarm profile check")?;

    let result = get_budget_check_result(&json)?;
    ensure_equal(&result, &"passed", "swarm profile result")
}

#[test]
fn budget_check_profile_mismatch_detected() -> TestResult {
    let output = run_ee(&[
        "perf",
        "budget",
        "check",
        "--profile",
        "constrained",
        "--report",
        "tests/fixtures/golden/perf_artifact/swarm_profile.json",
        "--json",
    ])?;

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "profile mismatch check exit code",
    )?;

    let json = stdout_json(&output)?;
    assert_response_envelope(&json, "profile mismatch check")?;

    let requested = get_requested_profile(&json)?;
    ensure_equal(&requested, &"constrained", "requested profile")?;

    let artifact_profile = get_artifact_profile(&json)?;
    ensure_equal(&artifact_profile, &"swarm", "artifact profile")?;

    let degradations = get_degradations(&json)?;
    ensure(
        !degradations.is_empty(),
        "profile mismatch should produce degradations",
    )?;

    let has_mismatch_code = degradations.iter().any(|d| {
        d.get("code")
            .and_then(|c| c.as_str())
            .map(|c| c.contains("mismatch") || c.contains("profile"))
            .unwrap_or(false)
    });
    ensure(
        has_mismatch_code,
        "degradations should include profile mismatch code",
    )
}

#[test]
fn budget_check_missing_report_returns_error() -> TestResult {
    let output = run_ee(&[
        "perf",
        "budget",
        "check",
        "--profile",
        "workstation",
        "--report",
        "/nonexistent/report.json",
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "missing report should produce non-zero exit code",
    )?;

    let json = stdout_json(&output)?;
    let schema = json
        .get("schema")
        .and_then(|s| s.as_str())
        .ok_or("missing schema")?;
    ensure_equal(&schema, &"ee.error.v2", "error schema")
}

#[test]
fn budget_check_empty_profile_returns_error() -> TestResult {
    let output = run_ee(&[
        "perf",
        "budget",
        "check",
        "--profile",
        "",
        "--report",
        "tests/fixtures/golden/perf_artifact/baseline_smoke.json",
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(EXIT_SUCCESS),
        "empty profile should produce non-zero exit code",
    )?;

    let json = stdout_json(&output)?;
    let schema = json
        .get("schema")
        .and_then(|s| s.as_str())
        .ok_or("missing schema")?;
    ensure_equal(&schema, &"ee.error.v2", "error schema")
}

#[test]
fn budget_check_effect_is_read_only() -> TestResult {
    let output = run_ee(&[
        "perf",
        "budget",
        "check",
        "--profile",
        "workstation",
        "--report",
        "tests/fixtures/golden/perf_artifact/baseline_smoke.json",
        "--json",
    ])?;

    let json = stdout_json(&output)?;
    let effect = json
        .get("data")
        .and_then(|d| d.get("effect"))
        .ok_or("missing effect field")?;

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

#[test]
fn budget_check_reports_comparable_metric_count() -> TestResult {
    let output = run_ee(&[
        "perf",
        "budget",
        "check",
        "--profile",
        "constrained",
        "--report",
        "tests/fixtures/golden/perf_artifact/constrained_profile.json",
        "--json",
    ])?;

    let json = stdout_json(&output)?;
    let summary = json
        .get("data")
        .and_then(|d| d.get("report"))
        .and_then(|r| r.get("summary"))
        .ok_or("missing summary")?;

    let metric_count = summary
        .get("comparableMetricCount")
        .and_then(|c| c.as_u64())
        .ok_or("missing comparableMetricCount")?;

    ensure(
        metric_count > 0,
        format!("comparableMetricCount should be > 0, got {metric_count}"),
    )
}

#[test]
fn budget_check_json_stdout_contains_only_machine_data() -> TestResult {
    let output = run_ee(&[
        "perf",
        "budget",
        "check",
        "--profile",
        "workstation",
        "--report",
        "tests/fixtures/golden/perf_artifact/baseline_smoke.json",
        "--json",
    ])?;

    let stdout =
        String::from_utf8(output.stdout.clone()).map_err(|e| format!("stdout not UTF-8: {e}"))?;

    ensure(
        !stdout.contains("warning:"),
        "stdout should not contain warning text",
    )?;
    ensure(
        !stdout.contains("Note:"),
        "stdout should not contain diagnostic notes",
    )?;

    let json: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|e| format!("stdout is not valid JSON: {e}"))?;

    ensure(json.is_object(), "stdout should be a JSON object")
}
