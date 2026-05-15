//! Invariant gate for the README trauma-guard promise (bd-3usjw.21).
//!
//! The destructive command comes from the shared fixture catalog so this
//! test tracks the evolving guard pattern set instead of freezing one
//! ad hoc command in test code.

use std::fmt::Debug;
use std::path::Path;
use std::process::{Command, Output};

use serde::Deserialize;
use serde_json::Value;

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;
const EXIT_POLICY_DENIED: i32 = 7;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DestructivePatternFixture {
    implemented_cases: Vec<DestructivePatternCase>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DestructivePatternCase {
    id: String,
    command: String,
    expected_action: String,
    expected_exit_code: i32,
    expected_rule_ids: Vec<String>,
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

fn ensure_exit_code(output: &Output, expected: i32, context: &str) -> TestResult {
    ensure_equal(
        output.status.code(),
        Some(expected),
        &format!(
            "{context}; stdout: {}; stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
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

fn fixture_case() -> Result<DestructivePatternCase, String> {
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("destructive_patterns")
        .join("commands.json");
    let fixture_text = std::fs::read_to_string(&fixture_path)
        .map_err(|error| format!("read {}: {error}", fixture_path.display()))?;
    let fixture: DestructivePatternFixture = serde_json::from_str(&fixture_text)
        .map_err(|error| format!("parse {}: {error}", fixture_path.display()))?;
    fixture
        .implemented_cases
        .into_iter()
        .find(|case| {
            case.expected_action == "halt" && case.expected_exit_code == EXIT_POLICY_DENIED
        })
        .ok_or_else(|| "fixture must contain at least one halt case with exit 7".to_owned())
}

#[test]
fn preflight_check_surface_is_registered() -> TestResult {
    let cli_source = include_str!("../src/cli/mod.rs");
    ensure(
        cli_source.contains("Preflight(PreflightCommand)")
            && cli_source.contains("handle_preflight_guard")
            && cli_source.contains("PreflightCommand::Check"),
        "src/cli/mod.rs must register ee preflight check",
    )
}

#[test]
fn destructive_fixture_command_blocks_with_risk_memory_provenance() -> TestResult {
    let case = fixture_case()?;
    let tempdir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let workspace = tempdir.path().to_string_lossy().into_owned();
    let provenance_input = format!("cass-session://trauma-guard-fixture-{}#L1-L3", case.id);
    let provenance_canonical = format!("cass-session://trauma-guard-fixture-{}#L1-3", case.id);
    let risk_content = format!(
        "Prior destructive-action incident for fixture {}: command `{}` caused workspace loss.",
        case.id, case.command
    );

    let init = run_ee(&["--workspace", &workspace, "--json", "init"])?;
    ensure_exit_code(&init, EXIT_SUCCESS, "ee init exit")?;
    assert_clean_stderr(&init, "ee init")?;

    let remember = run_ee(&[
        "--workspace",
        &workspace,
        "remember",
        &risk_content,
        "--level",
        "procedural",
        "--kind",
        "risk",
        "--source",
        &provenance_input,
        "--no-auto-link",
        "--no-propose-candidates",
        "--json",
    ])?;
    ensure_exit_code(&remember, EXIT_SUCCESS, "risk memory remember exit")?;
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
        &case.command,
    ])?;
    ensure_exit_code(
        &preflight,
        case.expected_exit_code,
        "destructive preflight exit",
    )?;
    assert_clean_stderr(&preflight, "destructive preflight")?;
    let report = stdout_json(&preflight, "destructive preflight")?;

    ensure_equal(
        report.get("schema").and_then(Value::as_str),
        Some("ee.preflight.guard.v1"),
        "preflight schema",
    )?;
    ensure_equal(
        report.get("exitCode").and_then(Value::as_i64),
        Some(i64::from(case.expected_exit_code)),
        "preflight exitCode",
    )?;

    let matched_rule_ids = report
        .get("matches")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("preflight response missing matches: {report}"))?
        .iter()
        .filter_map(|entry| entry.get("ruleId").and_then(Value::as_str))
        .collect::<Vec<_>>();
    for expected_rule_id in &case.expected_rule_ids {
        ensure(
            matched_rule_ids.contains(&expected_rule_id.as_str()),
            format!("expected rule {expected_rule_id} in matches: {report}"),
        )?;
    }

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
        matched.get("provenance_uri").and_then(Value::as_str),
        Some(provenance_canonical.as_str()),
        "matched memory provenance",
    )?;

    let non_destructive = run_ee(&[
        "--workspace",
        &workspace,
        "--json",
        "preflight",
        "check",
        "--cmd",
        "cargo fmt --check",
    ])?;
    ensure_exit_code(
        &non_destructive,
        EXIT_SUCCESS,
        "non-destructive preflight exit",
    )?;
    assert_clean_stderr(&non_destructive, "non-destructive preflight")?;
    let non_destructive_report = stdout_json(&non_destructive, "non-destructive preflight")?;
    ensure(
        non_destructive_report
            .get("matches")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        format!("non-destructive command should have no rule matches: {non_destructive_report}"),
    )?;
    ensure(
        non_destructive_report
            .get("matchedMemories")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        format!("non-destructive command should have no memory matches: {non_destructive_report}"),
    )
}
