//! Contract coverage for public `ee eval` JSON output.
//!
//! Eval now has fixture discovery and execution wired. These tests keep the
//! public CLI contract honest without requiring science analytics fields.

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

#[test]
fn eval_run_science_json_reports_fixture_without_science_metrics() -> TestResult {
    let output = run_ee(&[
        "--json",
        "eval",
        "run",
        "fx.release_failure.v1",
        "--science",
    ])?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("eval run --science stdout was not UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("eval run --science stderr was not UTF-8: {error}"))?;

    ensure(
        output.status.success(),
        format!(
            "eval run --science must succeed for a fixture; got {:?}; stderr: {stderr}",
            output.status.code()
        ),
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

    ensure_json_equal(
        value.get("schema"),
        json!("ee.response.v1"),
        "response schema",
    )?;
    ensure_json_equal(value.get("success"), JsonValue::Bool(true), "success")?;
    ensure_json_equal(value.pointer("/data/command"), json!("eval run"), "command")?;
    ensure_json_equal(
        value.pointer("/data/report/schema"),
        json!("ee.eval.report.v1"),
        "report schema",
    )?;
    ensure_json_equal(
        value.pointer("/data/report/fixture_id"),
        json!("fx.release_failure.v1"),
        "fixture id",
    )?;
    ensure(
        value.pointer("/data/scienceMetrics").is_none(),
        "scienceMetrics must not be emitted by the default build",
    )
}

#[test]
fn eval_run_without_science_reports_fixture_metrics_contract() -> TestResult {
    let output = run_ee(&["--json", "eval", "run", "fx.release_failure.v1"])?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("eval run stdout was not UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("eval run stderr was not UTF-8: {error}"))?;
    ensure(
        output.status.success(),
        format!(
            "eval run must succeed for a fixture; got {:?}; stderr: {stderr}",
            output.status.code()
        ),
    )?;
    ensure(
        stderr.is_empty(),
        format!("eval run --json stderr must be empty, got: {stderr:?}"),
    )?;

    let value: JsonValue = serde_json::from_str(&stdout)
        .map_err(|error| format!("stdout JSON parse failed: {error}"))?;
    ensure_json_equal(
        value.pointer("/data/report/schema"),
        json!("ee.eval.report.v1"),
        "report schema",
    )?;
    ensure_json_equal(
        value.pointer("/data/report/metrics/queries_evaluated"),
        json!(5),
        "queries evaluated",
    )?;
    ensure(
        value.pointer("/data/scienceMetrics").is_none(),
        "scienceMetrics should be omitted without science output",
    )
}
