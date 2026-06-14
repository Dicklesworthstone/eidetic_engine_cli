//! bd-i0iiw.2 - process-based harness conformance simulator tests.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use ee::hooks::{HarnessConformanceSimulationOptions, simulate_harness_conformance};
use ee::models::DomainError;
use serde_json::Value;
use tempfile::TempDir;

type TestResult = Result<(), String>;

fn manifest_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn fixture(name: &str) -> PathBuf {
    manifest_path(&format!("tests/fixtures/harness_conformance/{name}.json"))
}

fn options(
    fixture_name: &str,
    command: &str,
    workspace: PathBuf,
) -> HarnessConformanceSimulationOptions {
    let mut options =
        HarnessConformanceSimulationOptions::with_defaults(fixture(fixture_name), workspace);
    options.hook_command = Some(command.to_owned());
    options.timeout_seconds = 5;
    options
}

fn assert_all_assertions_pass(value: &ee::hooks::HarnessConformanceCase) -> TestResult {
    if value.expected.conformance_verdict != "pass" {
        return Err(format!(
            "{} should pass, got {}",
            value.case_id, value.expected.conformance_verdict
        ));
    }
    for assertion in &value.assertions {
        if assertion.expected_status != "pass" {
            return Err(format!(
                "{} assertion {} should pass, got {}",
                value.case_id, assertion.kind, assertion.expected_status
            ));
        }
    }
    Ok(())
}

fn response_ok_command() -> &'static str {
    "printf '%s\\n' '{\"schema\":\"ee.response.v2\",\"success\":true,\"data\":{\"hook\":\"ok\"},\"degraded\":[]}'"
}

fn response_degraded_command() -> &'static str {
    "printf '%s\\n' '{\"schema\":\"ee.response.v2\",\"success\":true,\"data\":{\"hook\":\"degraded\"},\"degraded\":[{\"code\":\"agent_mail_unavailable\",\"severity\":\"warning\",\"message\":\"Agent Mail unavailable\",\"repair\":\"retry later\"}]}' ; printf '%s\\n' 'Agent Mail unavailable api_key=sk-proj-1234567890abcdef1234567890abcdef /Users/jemanuel/private' >&2"
}

fn response_error_exit_two_command() -> &'static str {
    "printf '%s\\n' '{\"schema\":\"ee.error.v2\",\"error\":{\"code\":\"tool_failed\",\"message\":\"tool failed\",\"severity\":\"medium\"}}' ; printf '%s\\n' 'tool failed' >&2 ; exit 2"
}

fn cargo_denial_command() -> &'static str {
    "printf '%s\\n' 'rch remote required; refusing local fallback' >&2 ; exit 1"
}

#[test]
fn simulator_runs_committed_fixture_matrix_and_bounds_transcripts() -> TestResult {
    let temp = TempDir::new().map_err(|error| error.to_string())?;
    let cases = [
        ("codex_session_start", response_ok_command()),
        ("claude_pre_tool_edit", response_ok_command()),
        ("codex_post_tool_success", response_ok_command()),
        ("claude_compaction_resume", response_degraded_command()),
        (
            "mcp_client_post_tool_failure",
            response_error_exit_two_command(),
        ),
        ("generic_shell_pre_tool_shell", cargo_denial_command()),
    ];

    for (fixture_name, command) in cases {
        let report = simulate_harness_conformance(&options(
            fixture_name,
            command,
            temp.path().to_path_buf(),
        ))
        .map_err(|error| error.message())?;
        assert_all_assertions_pass(&report)?;
        let byte_budget = report
            .artifact_policy
            .inline_transcript_max_bytes
            .min(report.artifact_policy.max_artifact_bytes);
        if report.input.transcript.byte_count > byte_budget {
            return Err(format!(
                "{} transcript exceeded byte budget",
                report.case_id
            ));
        }
        if report.input.transcript.lines.iter().any(|line| {
            line.len() > usize::try_from(report.input.transcript.max_line_bytes).unwrap_or(256)
        }) {
            return Err(format!(
                "{} transcript exceeded line budget",
                report.case_id
            ));
        }
    }
    Ok(())
}

#[test]
fn simulator_redacts_secret_like_hook_output() -> TestResult {
    let temp = TempDir::new().map_err(|error| error.to_string())?;
    let report = simulate_harness_conformance(&options(
        "claude_compaction_resume",
        response_degraded_command(),
        temp.path().to_path_buf(),
    ))
    .map_err(|error| error.message())?;
    let transcript = report.input.transcript.lines.join("\n");
    if transcript.contains("sk-proj-1234567890abcdef1234567890abcdef") {
        return Err("raw secret leaked into transcript".to_owned());
    }
    if transcript.contains("/Users/jemanuel") {
        return Err("private absolute path leaked into transcript".to_owned());
    }
    if !transcript.contains("[REDACTED:") {
        return Err("transcript should carry redaction markers".to_owned());
    }
    Ok(())
}

#[test]
fn simulator_rejects_destructive_fixture_command_templates() -> TestResult {
    let temp = TempDir::new().map_err(|error| error.to_string())?;
    let mut fixture_json: Value = serde_json::from_slice(
        &std::fs::read(fixture("generic_shell_pre_tool_shell"))
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    fixture_json["input"]["commandTemplate"] = Value::String("rm -rf /tmp/unsafe".to_owned());
    let fixture_path = temp.path().join("destructive_fixture.json");
    std::fs::write(
        &fixture_path,
        serde_json::to_vec_pretty(&fixture_json).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    let mut options =
        HarnessConformanceSimulationOptions::with_defaults(fixture_path, temp.path().to_path_buf());
    options.hook_command = Some(response_ok_command().to_owned());
    let error = simulate_harness_conformance(&options)
        .expect_err("destructive fixture command template must be rejected");
    match error {
        DomainError::PolicyDenied { message, .. } if message.contains("destructive") => Ok(()),
        other => Err(format!("expected destructive PolicyDenied, got {other:?}")),
    }
}
