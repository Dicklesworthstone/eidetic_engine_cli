//! bd-i0iiw.4 - committed golden summaries for harness conformance fixtures.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use ee::hooks::{
    HarnessConformanceCase, HarnessConformanceSimulationOptions, simulate_harness_conformance,
};
use serde::Serialize;
use tempfile::TempDir;

type TestResult = Result<(), String>;

const GOLDEN_SCHEMA: &str = "ee.harness_conformance.golden_summary.v1";
const FIXTURES_REL: &str = "tests/fixtures/harness_conformance";
const GOLDENS_REL: &str = "tests/fixtures/golden/harness_conformance";

#[derive(Clone, Copy)]
struct GoldenCase {
    fixture_name: &'static str,
    command: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GoldenSummary {
    schema: &'static str,
    source_schema: String,
    case_id: String,
    harness: String,
    fixture_kind: String,
    event_name: String,
    payload_shape: String,
    command_template: Option<String>,
    expected: GoldenExpected,
    artifact_policy: GoldenArtifactPolicy,
    assertions: Vec<GoldenAssertion>,
    transcript: GoldenTranscript,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GoldenExpected {
    conformance_verdict: String,
    event_outcome: String,
    exit_policy: String,
    degraded_policy: String,
    output_budget_bytes: u64,
    local_cargo_fallback_allowed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GoldenArtifactPolicy {
    raw_transcript_allowed: bool,
    secret_material_allowed: bool,
    inline_transcript_max_bytes: u64,
    max_artifact_bytes: u64,
    allowed_artifact_kinds: Vec<String>,
}

#[derive(Serialize)]
struct GoldenAssertion {
    kind: String,
    status: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GoldenTranscript {
    kind: String,
    line_count: u64,
    max_line_bytes: u64,
    line_classes: Vec<String>,
    redaction_marker_line_count: u64,
    private_path_present: bool,
    secret_like_present: bool,
    contains_cargo_denial_marker: bool,
    contains_truncated_line: bool,
}

fn manifest_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn fixture(name: &str) -> PathBuf {
    manifest_path(&format!("{FIXTURES_REL}/{name}.json"))
}

fn golden(name: &str) -> PathBuf {
    manifest_path(&format!("{GOLDENS_REL}/{name}.json.golden"))
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

fn cases() -> [GoldenCase; 6] {
    [
        GoldenCase {
            fixture_name: "codex_session_start",
            command: response_ok_command(),
        },
        GoldenCase {
            fixture_name: "claude_pre_tool_edit",
            command: response_ok_command(),
        },
        GoldenCase {
            fixture_name: "codex_post_tool_success",
            command: response_ok_command(),
        },
        GoldenCase {
            fixture_name: "claude_compaction_resume",
            command: response_degraded_command(),
        },
        GoldenCase {
            fixture_name: "mcp_client_post_tool_failure",
            command: response_error_exit_two_command(),
        },
        GoldenCase {
            fixture_name: "generic_shell_pre_tool_shell",
            command: cargo_denial_command(),
        },
    ]
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

fn assert_all_assertions_pass(report: &HarnessConformanceCase) -> TestResult {
    if report.expected.conformance_verdict != "pass" {
        return Err(format!(
            "{} should pass, got {}",
            report.case_id, report.expected.conformance_verdict
        ));
    }
    for assertion in &report.assertions {
        if assertion.expected_status != "pass" {
            return Err(format!(
                "{} assertion {} should pass, got {}",
                report.case_id, assertion.kind, assertion.expected_status
            ));
        }
    }
    Ok(())
}

fn summarize(report: &HarnessConformanceCase) -> GoldenSummary {
    let transcript_text = report.input.transcript.lines.join("\n");
    let lower_transcript = transcript_text.to_ascii_lowercase();
    GoldenSummary {
        schema: GOLDEN_SCHEMA,
        source_schema: report.schema.clone(),
        case_id: report.case_id.clone(),
        harness: report.harness.clone(),
        fixture_kind: report.fixture_kind.clone(),
        event_name: report.event_name.clone(),
        payload_shape: report.input.payload_shape.clone(),
        command_template: report.input.command_template.clone(),
        expected: GoldenExpected {
            conformance_verdict: report.expected.conformance_verdict.clone(),
            event_outcome: report.expected.event_outcome.clone(),
            exit_policy: report.expected.exit_policy.clone(),
            degraded_policy: report.expected.degraded_policy.clone(),
            output_budget_bytes: report.expected.output_budget_bytes,
            local_cargo_fallback_allowed: report.expected.local_cargo_fallback_allowed,
        },
        artifact_policy: GoldenArtifactPolicy {
            raw_transcript_allowed: report.artifact_policy.raw_transcript_allowed,
            secret_material_allowed: report.artifact_policy.secret_material_allowed,
            inline_transcript_max_bytes: report.artifact_policy.inline_transcript_max_bytes,
            max_artifact_bytes: report.artifact_policy.max_artifact_bytes,
            allowed_artifact_kinds: report.artifact_policy.allowed_artifact_kinds.clone(),
        },
        assertions: report
            .assertions
            .iter()
            .map(|assertion| GoldenAssertion {
                kind: assertion.kind.clone(),
                status: assertion.expected_status.clone(),
            })
            .collect(),
        transcript: GoldenTranscript {
            kind: report.input.transcript.kind.clone(),
            line_count: report.input.transcript.line_count,
            max_line_bytes: report.input.transcript.max_line_bytes,
            line_classes: report
                .input
                .transcript
                .lines
                .iter()
                .map(|line| classify_transcript_line(line).to_owned())
                .collect(),
            redaction_marker_line_count: report
                .input
                .transcript
                .lines
                .iter()
                .filter(|line| line.contains("[REDACTED:"))
                .count() as u64,
            private_path_present: lower_transcript.contains("/users/")
                || lower_transcript.contains("/home/"),
            secret_like_present: lower_transcript.contains("sk-")
                || lower_transcript.contains("bearer ")
                || lower_transcript.contains("begin openssh")
                || lower_transcript.contains("begin rsa"),
            contains_cargo_denial_marker: report
                .input
                .transcript
                .lines
                .iter()
                .any(|line| line.contains("fixtureCommandTemplate=cargo synthetic only")),
            contains_truncated_line: report
                .input
                .transcript
                .lines
                .iter()
                .any(|line| line.contains("...[truncated]")),
        },
    }
}

fn classify_transcript_line(line: &str) -> &'static str {
    if line.starts_with("exitCode=") {
        "exit_code"
    } else if line.starts_with("elapsedMs=") {
        "elapsed_ms"
    } else if line.starts_with("stdout=") && line.contains("ee.error.v2") {
        "stdout_error_envelope"
    } else if line.starts_with("stdout=") {
        "stdout_response_envelope"
    } else if line.starts_with("stderr=") && line.contains("[REDACTED:") {
        "stderr_redacted"
    } else if line.starts_with("stderr=") {
        "stderr"
    } else if line.starts_with("fixtureCommandTemplate=") {
        "fixture_command_template"
    } else {
        "other"
    }
}

fn pretty_json(summary: &GoldenSummary) -> Result<String, String> {
    let body = serde_json::to_string_pretty(summary).map_err(|error| error.to_string())?;
    Ok(format!("{body}\n"))
}

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn read_text(relative: &str) -> Result<String, String> {
    let path = manifest_path(relative);
    std::fs::read_to_string(&path).map_err(|error| format!("read {}: {error}", path.display()))
}

#[test]
fn committed_harness_conformance_reports_match_goldens() -> TestResult {
    let temp = TempDir::new().map_err(|error| error.to_string())?;
    for case in cases() {
        let report = simulate_harness_conformance(&options(
            case.fixture_name,
            case.command,
            temp.path().to_path_buf(),
        ))
        .map_err(|error| error.message())?;
        assert_all_assertions_pass(&report)?;
        let actual = pretty_json(&summarize(&report))?;
        let golden_path = golden(case.fixture_name);
        let expected = std::fs::read_to_string(&golden_path)
            .map_err(|error| format!("read {}: {error}", golden_path.display()))?;
        ensure(
            actual == expected,
            format!(
                "{} drifted from committed golden {}\nactual:\n{}",
                case.fixture_name,
                golden_path.display(),
                actual
            ),
        )?;
    }
    Ok(())
}

#[test]
fn fixture_corpus_documents_provenance_and_rch_path() -> TestResult {
    let provenance = read_text("tests/fixtures/harness_conformance/PROVENANCE.md")?;
    ensure(
        provenance.contains("ee.harness_conformance.v1"),
        "fixture provenance must name the schema",
    )?;
    ensure(
        provenance.contains("scripts/rch_verify.sh"),
        "fixture provenance must preserve the RCH proof command",
    )?;
    ensure(
        provenance.contains("Never replace that proof with local Cargo"),
        "fixture provenance must forbid local Cargo proof replacement",
    )?;

    let docs = read_text("docs/agent-ux/harness-conformance.md")?;
    ensure(
        docs.contains("tests/fixtures/golden/harness_conformance/"),
        "agent docs must point at committed goldens",
    )?;
    ensure(
        docs.contains("scripts/rch_verify.sh"),
        "agent docs must show the RCH verification command",
    )?;
    ensure(
        docs.contains("bd-37ugy"),
        "agent docs must preserve the proof-owed blocker citation",
    )?;
    ensure(
        docs.contains("claim-gate"),
        "agent docs must describe the degraded-gate claim path",
    )
}
