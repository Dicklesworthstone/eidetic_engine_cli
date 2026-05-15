//! E2E coverage for trauma-guard preflight behavior.
//!
//! This exercises the public CLI rather than calling `core::preflight_guard`
//! directly: a risk memory with provenance is stored through `ee remember`,
//! then `ee preflight check` must surface it before a destructive command.

use std::fmt::Debug;
use std::process::{Command, Output};

use serde_json::Value;

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;
const EXIT_POLICY_DENIED: i32 = 7;

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

fn ensure_equal<T>(actual: T, expected: T, context: &str) -> TestResult
where
    T: Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn ensure_exit(output: &Output, expected: i32, context: &str) -> TestResult {
    if output.status.code() == Some(expected) {
        Ok(())
    } else {
        Err(format!(
            "{context}: expected Some({expected}), got {:?}; stdout: {}; stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn stdout_json(output: &Output, context: &str) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{context} stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{context} stdout was not JSON: {error}; stdout: {stdout}"))
}

fn assert_clean_stderr(output: &Output, context: &str) -> TestResult {
    ensure(
        output.stderr.is_empty(),
        format!(
            "{context} stderr should be empty; got {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

#[test]
fn destructive_command_surfaces_matching_risk_memory_provenance() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = tempdir.path().to_string_lossy().into_owned();
    let provenance_input = "cass-session://incident-rm-rf#L1-L3";
    let provenance_canonical = "cass-session://incident-rm-rf#L1-3";
    let risk_content =
        "Prior incident: rm -rf /tmp/work recursively removed another agent workspace.";

    let init = run_ee(&["--workspace", &workspace, "--json", "init"])?;
    ensure_exit(&init, EXIT_SUCCESS, "ee init exit")?;
    assert_clean_stderr(&init, "ee init")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        risk_content,
        "--level",
        "procedural",
        "--kind",
        "risk",
        "--source",
        provenance_input,
        "--no-auto-link",
        "--no-propose-candidates",
        "--json",
    ])?;
    ensure_exit(&remember, EXIT_SUCCESS, "risk memory remember exit")?;
    assert_clean_stderr(&remember, "risk memory remember")?;
    let remembered = stdout_json(&remember, "risk memory remember")?;
    let memory_id = remembered
        .pointer("/data/memory_id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("remember response missing memory_id: {remembered}"))?;

    let preflight = run_ee(&[
        "--workspace",
        &workspace,
        "--json",
        "preflight",
        "check",
        "--cmd",
        "rm -rf /tmp/work",
    ])?;
    ensure_exit(&preflight, EXIT_POLICY_DENIED, "destructive preflight exit")?;
    assert_clean_stderr(&preflight, "destructive preflight")?;
    let report = stdout_json(&preflight, "destructive preflight")?;

    ensure_equal(
        report.get("schema").and_then(Value::as_str),
        Some("ee.preflight.guard.v1"),
        "preflight schema",
    )?;
    ensure_equal(
        report.get("exitCode").and_then(Value::as_i64),
        Some(i64::from(EXIT_POLICY_DENIED)),
        "preflight exitCode",
    )?;
    ensure(
        report
            .get("matches")
            .and_then(Value::as_array)
            .is_some_and(|matches| !matches.is_empty()),
        format!("destructive preflight should include guard matches: {report}"),
    )?;

    let matched_memories = report
        .get("matchedMemories")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("preflight response missing matchedMemories: {report}"))?;
    ensure_equal(matched_memories.len(), 1, "matchedMemories length")?;
    let matched = &matched_memories[0];
    ensure_equal(
        matched.get("memory_id").and_then(Value::as_str),
        Some(memory_id),
        "matched memory id",
    )?;
    ensure_equal(
        matched.get("kind").and_then(Value::as_str),
        Some("risk"),
        "matched memory kind",
    )?;
    ensure_equal(
        matched.get("provenance_uri").and_then(Value::as_str),
        Some(provenance_canonical),
        "matched memory provenance",
    )?;
    ensure(
        matched
            .get("matched_terms")
            .and_then(Value::as_array)
            .is_some_and(|terms| !terms.is_empty()),
        format!("matched memory should include matched_terms: {matched}"),
    )
}

#[test]
fn non_destructive_command_returns_success_without_matches() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = tempdir.path().to_string_lossy().into_owned();

    let init = run_ee(&["--workspace", &workspace, "--json", "init"])?;
    ensure_exit(&init, EXIT_SUCCESS, "ee init exit")?;
    assert_clean_stderr(&init, "ee init")?;

    let preflight = run_ee(&[
        "--workspace",
        &workspace,
        "--json",
        "preflight",
        "check",
        "--cmd",
        "cargo fmt --check",
    ])?;
    ensure_exit(&preflight, EXIT_SUCCESS, "non-destructive preflight exit")?;
    assert_clean_stderr(&preflight, "non-destructive preflight")?;
    let report = stdout_json(&preflight, "non-destructive preflight")?;

    ensure_equal(
        report.get("schema").and_then(Value::as_str),
        Some("ee.preflight.guard.v1"),
        "preflight schema",
    )?;
    ensure_equal(
        report.get("exitCode").and_then(Value::as_i64),
        Some(i64::from(EXIT_SUCCESS)),
        "preflight exitCode",
    )?;
    ensure(
        report
            .get("matches")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        format!("non-destructive command should not match guard rules: {report}"),
    )?;
    ensure(
        report
            .get("matchedMemories")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        format!("non-destructive command should not match memories: {report}"),
    )
}
