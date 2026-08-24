//! Contract coverage for public `ee eval` JSON output.
//!
//! Eval now has fixture discovery and execution wired. These tests keep the
//! public CLI contract honest without requiring science analytics fields.

use ee::models::ProcessExitCode;
use serde_json::{Value as JsonValue, json};
use std::process::Output;

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
    crate::common_spawn::serialized_real_ee(args)
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

    // bd-g3yh5: on exit-code mismatch, surface the JSON envelope so the
    // remote artifact shows which error path fired instead of a bare code.
    ensure(
        output.status.code() == Some(ProcessExitCode::EvalFailure as i32),
        format!(
            "eval run --science must fail the process for a failed fixture; got {:?}; stderr: {stderr}; stdout: {stdout}",
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
        json!("ee.response.v2"),
        "response schema",
    )?;
    ensure_json_equal(value.get("success"), JsonValue::Bool(false), "success")?;
    ensure_json_equal(value.pointer("/data/command"), json!("eval run"), "command")?;
    ensure_json_equal(
        value.pointer("/data/report/status"),
        json!("failed"),
        "report status",
    )?;
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
    // bd-g3yh5: include the JSON envelope when the exit code mismatches so
    // remote artifacts show the firing error path, not just a bare code.
    ensure(
        output.status.code() == Some(ProcessExitCode::EvalFailure as i32),
        format!(
            "eval run must fail the process for a failed fixture; got {:?}; stderr: {stderr}; stdout: {stdout}",
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
    ensure_json_equal(value.get("success"), JsonValue::Bool(false), "success")?;
    ensure_json_equal(
        value.pointer("/data/report/status"),
        json!("failed"),
        "report status",
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
