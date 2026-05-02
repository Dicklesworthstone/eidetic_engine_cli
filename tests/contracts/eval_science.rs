//! Contract coverage for `ee eval run --science`.
//!
//! Ensures the CLI exposes deterministic science metrics through the stable
//! response envelope while keeping diagnostics off stdout.

use ee::science::{
    DEGRADATION_CODE_BACKEND_UNAVAILABLE, DEGRADATION_CODE_NOT_COMPILED, ScienceStatus, status,
};
use serde_json::{Value as JsonValue, json};
use std::process::{Command, Output};

type TestResult = Result<(), String>;

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

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn expected_degradation(status: ScienceStatus) -> JsonValue {
    match status {
        ScienceStatus::Available => JsonValue::Null,
        ScienceStatus::NotCompiled => json!(DEGRADATION_CODE_NOT_COMPILED),
        ScienceStatus::BackendUnavailable => json!(DEGRADATION_CODE_BACKEND_UNAVAILABLE),
    }
}

#[test]
fn eval_run_science_json_has_stable_metrics_contract() -> TestResult {
    let output = run_ee(&["--json", "eval", "run", "--science"])?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("eval run --science stdout was not UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("eval run --science stderr was not UTF-8: {error}"))?;

    ensure(
        output.status.success(),
        format!("eval run --science must succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        format!("eval run --science --json stderr must be empty, got: {stderr:?}"),
    )?;
    ensure(
        stdout.ends_with('\n'),
        format!("eval run --science JSON must be newline-terminated, got: {stdout:?}"),
    )?;

    let value: JsonValue = serde_json::from_str(&stdout)
        .map_err(|error| format!("stdout JSON parse failed: {error}"))?;
    let science_status = status();

    ensure_json_equal(
        value.get("schema"),
        json!("ee.response.v1"),
        "response schema",
    )?;
    ensure_json_equal(value.get("success"), JsonValue::Bool(true), "success")?;
    ensure_json_equal(value.pointer("/data/command"), json!("eval run"), "command")?;
    ensure_json_equal(
        value.pointer("/data/status"),
        json!("no_scenarios"),
        "status",
    )?;
    ensure_json_equal(
        value.pointer("/data/scienceMetrics/schema"),
        json!("ee.eval.science_metrics.v1"),
        "science schema",
    )?;
    ensure_json_equal(
        value.pointer("/data/scienceMetrics/status"),
        json!(science_status.as_str()),
        "science status",
    )?;
    ensure_json_equal(
        value.pointer("/data/scienceMetrics/available"),
        JsonValue::Bool(science_status.is_available()),
        "science availability",
    )?;
    ensure_json_equal(
        value.pointer("/data/scienceMetrics/degradationCode"),
        expected_degradation(science_status),
        "science degradation code",
    )?;
    ensure_json_equal(
        value.pointer("/data/scienceMetrics/scenariosEvaluated"),
        json!(0),
        "scenarios evaluated",
    )?;
    ensure_json_equal(
        value.pointer("/data/scienceMetrics/positiveLabel"),
        json!("scenario_passed"),
        "positive label",
    )?;
    ensure_json_equal(
        value.pointer("/data/scienceMetrics/precision"),
        JsonValue::Null,
        "precision",
    )?;
    ensure_json_equal(
        value.pointer("/data/scienceMetrics/recall"),
        JsonValue::Null,
        "recall",
    )?;
    ensure_json_equal(
        value.pointer("/data/scienceMetrics/f1Score"),
        JsonValue::Null,
        "f1 score",
    )
}

#[test]
fn eval_run_without_science_omits_science_metrics() -> TestResult {
    let output = run_ee(&["--json", "eval", "run"])?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("eval run stdout was not UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("eval run stderr was not UTF-8: {error}"))?;
    ensure(
        output.status.success(),
        format!("eval run must succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        format!("eval run --json stderr must be empty, got: {stderr:?}"),
    )?;

    let value: JsonValue = serde_json::from_str(&stdout)
        .map_err(|error| format!("stdout JSON parse failed: {error}"))?;
    ensure(
        value.pointer("/data/scienceMetrics").is_none(),
        "scienceMetrics should be omitted unless --science is set",
    )
}
