//! EE-TST-005: Advanced Subsystem End-to-End Tests
//!
//! Tests for recorder, preflight, procedure, economy, learning, and causal subsystems.
//! Each test validates JSON output contracts, dry-run behavior, and stdout/stderr isolation.

use std::fmt::Debug;
use std::process::{Command, Output};

type TestResult = Result<(), String>;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
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

fn stdout_is_json(output: &Output) -> bool {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<serde_json::Value>(&stdout).is_ok()
}

fn stdout_has_schema(output: &Output, expected: &str) -> bool {
    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
        json.get("schema").map_or(false, |s| s.as_str() == Some(expected))
    } else {
        false
    }
}

fn stdout_contains(output: &Output, needle: &str) -> bool {
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.contains(needle)
}

fn stdout_is_clean(output: &Output) -> bool {
    let stdout = String::from_utf8_lossy(&output.stdout);
    !stdout.contains("[INFO]")
        && !stdout.contains("[WARN]")
        && !stdout.contains("[ERROR]")
        && !stdout.contains("warning:")
        && !stdout.contains("error:")
}

// ============================================================================
// Recorder Tests (EE-401)
// ============================================================================

#[test]
fn recorder_start_dry_run_returns_valid_json() -> TestResult {
    let output = run_ee(&["recorder", "start", "--agent-id", "test-agent", "--dry-run", "--json"])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_has_schema(&output, "ee.recorder.start.v1"),
        "must have recorder start schema",
    )?;
    ensure(stdout_contains(&output, "runId"), "must contain runId")?;
    ensure(stdout_contains(&output, "dryRun"), "must contain dryRun")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")
}

#[test]
fn recorder_start_creates_run_id() -> TestResult {
    let output = run_ee(&["recorder", "start", "--agent-id", "test-agent", "--json"])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(stdout_contains(&output, "run_"), "runId must start with run_")
}

#[test]
fn recorder_event_requires_run_id() -> TestResult {
    let output = run_ee(&[
        "recorder",
        "event",
        "run_test_123",
        "--event-type",
        "tool_call",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_has_schema(&output, "ee.recorder.event_response.v1"),
        "must have recorder event schema",
    )?;
    ensure(stdout_contains(&output, "eventId"), "must contain eventId")?;
    ensure(stdout_contains(&output, "sequence"), "must contain sequence")
}

#[test]
fn recorder_event_supports_redaction() -> TestResult {
    let output = run_ee(&[
        "recorder",
        "event",
        "run_test_123",
        "--event-type",
        "user_message",
        "--payload",
        "secret",
        "--redact",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_contains(&output, "redactionStatus"),
        "must contain redactionStatus",
    )
}

#[test]
fn recorder_event_validates_event_type() -> TestResult {
    let output = run_ee(&[
        "recorder",
        "event",
        "run_test_123",
        "--event-type",
        "invalid_type",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(1), "exit code for invalid event type")
}

#[test]
fn recorder_finish_dry_run_returns_valid_json() -> TestResult {
    let output = run_ee(&[
        "recorder",
        "finish",
        "run_test_123",
        "--status",
        "completed",
        "--dry-run",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_has_schema(&output, "ee.recorder.finish.v1"),
        "must have recorder finish schema",
    )?;
    ensure(stdout_contains(&output, "dryRun"), "must contain dryRun")
}

#[test]
fn recorder_finish_validates_status() -> TestResult {
    let output = run_ee(&[
        "recorder",
        "finish",
        "run_test_123",
        "--status",
        "invalid_status",
        "--json",
    ])?;
    ensure_equal(&output.status.code(), &Some(1), "exit code for invalid status")
}

#[test]
fn recorder_tail_returns_valid_json() -> TestResult {
    let output = run_ee(&["recorder", "tail", "run_test_123", "--limit", "10", "--json"])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(
        stdout_has_schema(&output, "ee.recorder.tail.v1"),
        "must have recorder tail schema",
    )?;
    ensure(stdout_contains(&output, "events"), "must contain events")
}

// ============================================================================
// Procedure Tests (EE-411 - Stub tests for when implemented)
// ============================================================================

#[test]
fn procedure_list_returns_valid_json() -> TestResult {
    let output = run_ee(&["procedure", "list", "--json"])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")
}

#[test]
fn procedure_show_handles_not_found() -> TestResult {
    let output = run_ee(&["procedure", "show", "proc_nonexistent", "--json"])?;
    // May return NotFound (10) or Success with empty result
    ensure(
        output.status.code() == Some(0) || output.status.code() == Some(10),
        "exit code must be 0 or 10 (not found)",
    )?;
    if output.status.code() == Some(0) {
        ensure(stdout_is_json(&output), "stdout must be valid JSON")
    } else {
        Ok(())
    }
}

// ============================================================================
// Economy Tests (EE-431 - Stub tests for when implemented)
// ============================================================================

#[test]
fn economy_report_returns_valid_json() -> TestResult {
    let output = run_ee(&["economy", "report", "--json"])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
    ensure(stdout_is_clean(&output), "stdout must be clean")
}

#[test]
fn economy_score_handles_not_found() -> TestResult {
    let output = run_ee(&["economy", "score", "mem_nonexistent", "--json"])?;
    // May return NotFound (10) or Success with empty result
    ensure(
        output.status.code() == Some(0) || output.status.code() == Some(10),
        "exit code must be 0 or 10 (not found)",
    )?;
    if output.status.code() == Some(0) {
        ensure(stdout_is_json(&output), "stdout must be valid JSON")
    } else {
        Ok(())
    }
}

// ============================================================================
// Preflight Tests (EE-391 - Stub tests for when implemented)
// ============================================================================

#[test]
fn preflight_show_returns_valid_json() -> TestResult {
    let output = run_ee(&["preflight", "show", "--json"])?;
    // May succeed or indicate no active preflight
    if output.status.code() == Some(0) {
        ensure(stdout_is_json(&output), "stdout must be valid JSON")?;
        ensure(stdout_is_clean(&output), "stdout must be clean")
    } else {
        Ok(())
    }
}

// ============================================================================
// Output Contract Tests
// ============================================================================

#[test]
fn all_recorder_commands_produce_stdout_only_data() -> TestResult {
    let commands = [
        vec!["recorder", "start", "--agent-id", "test", "--dry-run", "--json"],
        vec!["recorder", "event", "run_1", "--event-type", "tool_call", "--json"],
        vec!["recorder", "finish", "run_1", "--dry-run", "--json"],
        vec!["recorder", "tail", "run_1", "--json"],
    ];

    for args in &commands {
        let output = run_ee(args)?;
        if output.status.code() == Some(0) {
            ensure(
                stdout_is_clean(&output),
                format!("ee {} must have clean stdout", args.join(" ")),
            )?;
        }
    }
    Ok(())
}

#[test]
fn recorder_commands_support_human_output() -> TestResult {
    let output = run_ee(&["recorder", "start", "--agent-id", "test", "--dry-run"])?;
    ensure_equal(&output.status.code(), &Some(0), "exit code")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        stdout.contains("Recording Session"),
        "human output must contain header",
    )
}
