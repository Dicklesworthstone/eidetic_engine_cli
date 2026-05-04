//! Contract coverage for public `ee eval` degraded honesty.
//!
//! Eval renderers still have lower-level fixture contracts, but the public CLI
//! must not report no-scenario stub success before fixture discovery and
//! execution are actually wired.

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
fn eval_run_science_json_degrades_until_fixture_runner_exists() -> TestResult {
    let output = run_ee(&["--json", "eval", "run", "--science"])?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("eval run --science stdout was not UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("eval run --science stderr was not UTF-8: {error}"))?;

    ensure(
        output.status.code() == Some(6),
        format!(
            "eval run --science must fail closed with degraded exit; got {:?}; stderr: {stderr}",
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
    ensure_json_equal(value.get("success"), JsonValue::Bool(false), "success")?;
    ensure_json_equal(value.pointer("/data/command"), json!("eval run"), "command")?;
    ensure_json_equal(
        value.pointer("/data/code"),
        json!("eval_fixtures_unavailable"),
        "degraded code",
    )?;
    ensure_json_equal(
        value.pointer("/data/degraded/0/code"),
        json!("eval_fixtures_unavailable"),
        "degraded array code",
    )?;
    ensure_json_equal(
        value.pointer("/data/repair"),
        json!("ee status --json"),
        "repair command",
    )?;
    ensure_json_equal(
        value.pointer("/data/followUpBead"),
        json!("eidetic_engine_cli-uiy3"),
        "follow-up bead",
    )?;
    ensure(
        value.pointer("/data/scienceMetrics").is_none(),
        "scienceMetrics must not be emitted without a real eval report",
    )
}

#[test]
fn eval_run_without_science_degrades_before_metrics_contract() -> TestResult {
    let output = run_ee(&["--json", "eval", "run"])?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("eval run stdout was not UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("eval run stderr was not UTF-8: {error}"))?;
    ensure(
        output.status.code() == Some(6),
        format!(
            "eval run must fail closed with degraded exit; got {:?}; stderr: {stderr}",
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
        value.pointer("/data/code"),
        json!("eval_fixtures_unavailable"),
        "degraded code",
    )?;
    ensure(
        value.pointer("/data/scienceMetrics").is_none(),
        "scienceMetrics should be omitted while eval is unavailable",
    )
}
