//! Real-binary E2E coverage for `ee diag plan-cache`.
//!
//! Unit tests already pin the plan-cache report model. This test pins the
//! user-facing route by spawning the compiled `ee` binary and asserting the
//! standard JSON response envelope.

use serde_json::Value as JsonValue;
use std::process::{Command, Output};

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn parse_stdout_json(output: &Output, context: &str) -> Result<JsonValue, String> {
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "{context} stdout was not JSON: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn diag_plan_cache_real_binary_emits_response_envelope() -> TestResult {
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .env("EE_QUERY_PLAN_CACHE_ENTRIES", "4")
        .args(["--json", "diag", "plan-cache"])
        .output()
        .map_err(|error| format!("failed to run ee diag plan-cache: {error}"))?;

    ensure(
        output.status.success(),
        format!(
            "diag plan-cache failed: status={:?}; stdout={}; stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "diag plan-cache should not write JSON diagnostics to stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stdout.ends_with(b"\n"),
        "diag plan-cache JSON stdout should end with a newline",
    )?;

    let response = parse_stdout_json(&output, "diag plan-cache")?;
    ensure(
        response.pointer("/schema").and_then(JsonValue::as_str) == Some("ee.response.v2"),
        "top-level response envelope schema",
    )?;
    ensure(
        response.pointer("/success").and_then(JsonValue::as_bool) == Some(true),
        "top-level response success",
    )?;
    ensure(
        response
            .pointer("/degraded")
            .and_then(JsonValue::as_array)
            .is_some_and(Vec::is_empty),
        "top-level degraded array should be empty",
    )?;
    ensure(
        response
            .pointer("/data/command")
            .and_then(JsonValue::as_str)
            == Some("diag plan-cache"),
        "diag plan-cache command field",
    )?;
    ensure(
        response
            .pointer("/data/report/schemaTag")
            .and_then(JsonValue::as_str)
            == Some("ee.diag.plan_cache.v1"),
        "plan-cache report schema",
    )?;
    ensure(
        response
            .pointer("/data/report/enabled")
            .and_then(JsonValue::as_bool)
            == Some(true),
        "plan-cache should be enabled with explicit positive capacity",
    )?;
    ensure(
        response
            .pointer("/data/report/capacity")
            .and_then(JsonValue::as_u64)
            == Some(4),
        "plan-cache capacity should honor process env override",
    )?;
    ensure(
        response
            .pointer("/data/report/envVarName")
            .and_then(JsonValue::as_str)
            == Some("EE_QUERY_PLAN_CACHE_ENTRIES"),
        "plan-cache env var name",
    )?;
    ensure(
        response
            .pointer("/data/report/envVarValueSource")
            .and_then(JsonValue::as_str)
            == Some("process_env"),
        "plan-cache env var value source",
    )?;
    ensure(
        response
            .pointer("/data/report/topKeys")
            .and_then(JsonValue::as_array)
            .is_some(),
        "plan-cache top keys array",
    )
}
