//! EE-038: Agent golden baselines for health/status/search/context/doctor/api-version/agent-docs
//!
//! This module provides golden baseline tests for all agent-facing commands. Each command's
//! JSON output is captured and compared against a golden file to ensure stable contracts.
//!
//! Run with `UPDATE_GOLDEN=1 cargo test agent_golden` to update golden files.

use std::env;
use std::fmt::Debug;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

type TestResult = Result<(), String>;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn golden_path(category: &str, name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join(category)
        .join(format!("{name}.golden"))
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

fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    ensure(
        haystack.contains(needle),
        format!("{context}: expected output to contain {needle:?}"),
    )
}

fn ensure_starts_with(haystack: &str, prefix: &str, context: &str) -> TestResult {
    ensure(
        haystack.starts_with(prefix),
        format!("{context}: expected output to start with {prefix:?}"),
    )
}

/// Normalize JSON for comparison by removing volatile fields like timestamps and UUIDs.
fn normalize_json_for_golden(json: &str) -> String {
    json.trim().to_string()
}

/// Assert that the actual output matches the golden file, or update the golden if UPDATE_GOLDEN=1.
fn assert_golden(category: &str, name: &str, actual: &str) -> TestResult {
    let path = golden_path(category, name);
    let update_mode = env::var("UPDATE_GOLDEN").is_ok();

    if update_mode {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        fs::write(&path, actual).map_err(|e| format!("write {}: {e}", path.display()))?;
        eprintln!("Updated golden file: {}", path.display());
        return Ok(());
    }

    let expected = fs::read_to_string(&path).map_err(|e| {
        format!(
            "Golden file not found: {}\nRun with UPDATE_GOLDEN=1 to create it.\nError: {e}",
            path.display()
        )
    })?;

    let actual_normalized = normalize_json_for_golden(actual);
    let expected_normalized = normalize_json_for_golden(&expected);

    if actual_normalized == expected_normalized {
        Ok(())
    } else {
        Err(format!(
            "Golden test '{category}/{name}' failed.\n\
             Golden file: {}\n\
             Run with UPDATE_GOLDEN=1 to update.\n\n\
             --- expected\n{expected_normalized}\n\n\
             +++ actual\n{actual_normalized}",
            path.display()
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContractFormat {
    Json,
    Toon,
    Text,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FailureClass {
    CommandFailure,
    GoldenDrift,
    SchemaMismatch,
    StdoutPollution,
    RedactionFailure,
}

impl FailureClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CommandFailure => "command_failure",
            Self::GoldenDrift => "golden_drift",
            Self::SchemaMismatch => "schema_mismatch",
            Self::StdoutPollution => "stdout_stderr_pollution",
            Self::RedactionFailure => "redaction_failure",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ContractCase {
    name: &'static str,
    args: &'static [&'static str],
    category: &'static str,
    golden_name: &'static str,
    format: ContractFormat,
    expected_success: bool,
    expected_schema: Option<&'static str>,
    expected_command: Option<&'static str>,
}

impl ContractCase {
    fn command_display(self) -> String {
        format!("ee {}", self.args.join(" "))
    }

    fn fixture_path(self) -> PathBuf {
        golden_path(self.category, self.golden_name)
    }

    fn stdout_artifact_path(self) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("ee-contract-artifacts")
            .join(format!("{}.stdout", self.name))
    }

    fn stderr_artifact_path(self) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("ee-contract-artifacts")
            .join(format!("{}.stderr", self.name))
    }
}

#[derive(Debug)]
struct ContractFailure {
    case: ContractCase,
    class: FailureClass,
    pointer: &'static str,
    expected: String,
    actual: String,
    exit_code: Option<i32>,
}

impl ContractFailure {
    fn render(&self) -> String {
        format!(
            "Contract failure: {name}\n\
             class: {class}\n\
             command: {command}\n\
             exit_code: {exit_code:?}\n\
             schema: {schema}\n\
             json_pointer: {pointer}\n\
             fixture: {fixture}\n\
             stdout_artifact: {stdout_artifact}\n\
             stderr_artifact: {stderr_artifact}\n\
             expected: {expected}\n\
             actual: {actual}",
            name = self.case.name,
            class = self.class.as_str(),
            command = self.case.command_display(),
            exit_code = self.exit_code,
            schema = self.case.expected_schema.unwrap_or("n/a"),
            pointer = self.pointer,
            fixture = self.case.fixture_path().display(),
            stdout_artifact = self.case.stdout_artifact_path().display(),
            stderr_artifact = self.case.stderr_artifact_path().display(),
            expected = self.expected,
            actual = self.actual,
        )
    }
}

fn contract_failure(
    case: ContractCase,
    class: FailureClass,
    pointer: &'static str,
    expected: impl Into<String>,
    actual: impl Into<String>,
    exit_code: Option<i32>,
) -> String {
    ContractFailure {
        case,
        class,
        pointer,
        expected: expected.into(),
        actual: actual.into(),
        exit_code,
    }
    .render()
}

fn current_stage_contract_cases() -> &'static [ContractCase] {
    &[
        ContractCase {
            name: "check_json",
            args: &["check", "--json"],
            category: "check",
            golden_name: "check_json",
            format: ContractFormat::Json,
            expected_success: true,
            expected_schema: Some("ee.response.v1"),
            expected_command: Some("check"),
        },
        ContractCase {
            name: "check_toon",
            args: &["check", "--format", "toon"],
            category: "check",
            golden_name: "check_toon",
            format: ContractFormat::Toon,
            expected_success: true,
            expected_schema: Some("ee.response.v1"),
            expected_command: Some("check"),
        },
        ContractCase {
            name: "doctor_json",
            args: &["doctor", "--json"],
            category: "doctor",
            golden_name: "doctor_json",
            format: ContractFormat::Json,
            expected_success: true,
            expected_schema: Some("ee.response.v1"),
            expected_command: Some("doctor"),
        },
        ContractCase {
            name: "doctor_toon",
            args: &["doctor", "--format", "toon"],
            category: "doctor",
            golden_name: "doctor_toon",
            format: ContractFormat::Toon,
            expected_success: true,
            expected_schema: Some("ee.response.v1"),
            expected_command: Some("doctor"),
        },
        ContractCase {
            name: "capabilities_json",
            args: &["capabilities", "--json"],
            category: "capabilities",
            golden_name: "capabilities_json",
            format: ContractFormat::Json,
            expected_success: true,
            expected_schema: Some("ee.response.v1"),
            expected_command: Some("capabilities"),
        },
        ContractCase {
            name: "capabilities_toon",
            args: &["capabilities", "--format", "toon"],
            category: "capabilities",
            golden_name: "capabilities_toon",
            format: ContractFormat::Toon,
            expected_success: true,
            expected_schema: Some("ee.response.v1"),
            expected_command: Some("capabilities"),
        },
        ContractCase {
            name: "status_json",
            args: &["status", "--json"],
            category: "status",
            golden_name: "status_json",
            format: ContractFormat::Json,
            expected_success: true,
            expected_schema: Some("ee.response.v1"),
            expected_command: Some("status"),
        },
        ContractCase {
            name: "version_output",
            args: &["version"],
            category: "version",
            golden_name: "version_output",
            format: ContractFormat::Text,
            expected_success: true,
            expected_schema: None,
            expected_command: None,
        },
        ContractCase {
            name: "agent_docs_json",
            args: &["--agent-docs"],
            category: "agent_docs",
            golden_name: "agent_docs_json",
            format: ContractFormat::Json,
            expected_success: true,
            expected_schema: Some("ee.response.v1"),
            expected_command: Some("agent-docs"),
        },
        ContractCase {
            name: "schema_json",
            args: &["--schema"],
            category: "schema",
            golden_name: "schema_json",
            format: ContractFormat::Json,
            expected_success: true,
            expected_schema: Some("ee.response.v1"),
            expected_command: Some("schema"),
        },
        ContractCase {
            name: "health_unavailable_json",
            args: &["--json", "health"],
            category: "agent",
            golden_name: "health_unavailable.json",
            format: ContractFormat::Json,
            expected_success: false,
            expected_schema: Some("ee.error.v1"),
            expected_command: None,
        },
    ]
}

fn validate_contract_case(case: ContractCase) -> TestResult {
    let output = run_ee(case.args)?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("{} stdout was not UTF-8: {error}", case.command_display()))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("{} stderr was not UTF-8: {error}", case.command_display()))?;
    let exit_code = output.status.code();

    if output.status.success() != case.expected_success {
        return Err(contract_failure(
            case,
            FailureClass::CommandFailure,
            "/exit_code",
            case.expected_success.to_string(),
            format!("{:?}", output.status.code()),
            exit_code,
        ));
    }

    if !stderr.is_empty() {
        return Err(contract_failure(
            case,
            FailureClass::StdoutPollution,
            "/stderr",
            "",
            stderr,
            exit_code,
        ));
    }

    if contains_unredacted_secret(&stdout) {
        return Err(contract_failure(
            case,
            FailureClass::RedactionFailure,
            "/",
            "redacted output",
            "secret-like token present",
            exit_code,
        ));
    }

    validate_contract_schema(case, &stdout, exit_code)?;
    validate_contract_golden(case, &stdout, exit_code)
}

fn validate_contract_schema(
    case: ContractCase,
    stdout: &str,
    exit_code: Option<i32>,
) -> TestResult {
    match case.format {
        ContractFormat::Json => validate_json_contract(case, stdout, exit_code),
        ContractFormat::Toon => validate_toon_contract(case, stdout, exit_code),
        ContractFormat::Text => Ok(()),
    }
}

fn validate_json_contract(case: ContractCase, stdout: &str, exit_code: Option<i32>) -> TestResult {
    let value: Value = serde_json::from_str(stdout).map_err(|error| {
        contract_failure(
            case,
            FailureClass::SchemaMismatch,
            "/",
            "valid JSON",
            error.to_string(),
            exit_code,
        )
    })?;

    let actual_schema = value.get("schema").and_then(Value::as_str);
    if actual_schema != case.expected_schema {
        return Err(contract_failure(
            case,
            FailureClass::SchemaMismatch,
            "/schema",
            format!("{:?}", case.expected_schema),
            format!("{actual_schema:?}"),
            exit_code,
        ));
    }

    match case.expected_schema {
        Some("ee.response.v1") => {
            if value.get("success").and_then(Value::as_bool).is_none() {
                return Err(contract_failure(
                    case,
                    FailureClass::SchemaMismatch,
                    "/success",
                    "boolean",
                    format!("{:?}", value.get("success")),
                    exit_code,
                ));
            }
            let data = value.get("data").ok_or_else(|| {
                contract_failure(
                    case,
                    FailureClass::SchemaMismatch,
                    "/data",
                    "object",
                    "missing",
                    exit_code,
                )
            })?;
            if let Some(expected_command) = case.expected_command {
                let actual_command = data.get("command").and_then(Value::as_str);
                if actual_command != Some(expected_command) {
                    return Err(contract_failure(
                        case,
                        FailureClass::SchemaMismatch,
                        "/data/command",
                        expected_command,
                        format!("{actual_command:?}"),
                        exit_code,
                    ));
                }
            }
        }
        Some("ee.error.v1") => {
            let error = value.get("error").ok_or_else(|| {
                contract_failure(
                    case,
                    FailureClass::SchemaMismatch,
                    "/error",
                    "object",
                    "missing",
                    exit_code,
                )
            })?;
            if error.get("code").and_then(Value::as_str).is_none() {
                return Err(contract_failure(
                    case,
                    FailureClass::SchemaMismatch,
                    "/error/code",
                    "string",
                    format!("{:?}", error.get("code")),
                    exit_code,
                ));
            }
        }
        _ => {}
    }

    Ok(())
}

fn validate_toon_contract(case: ContractCase, stdout: &str, exit_code: Option<i32>) -> TestResult {
    let expected_schema = case.expected_schema.unwrap_or("ee.response.v1");
    let schema_line = format!("schema: {expected_schema}");
    if !stdout.starts_with(&schema_line) {
        return Err(contract_failure(
            case,
            FailureClass::SchemaMismatch,
            "/schema",
            schema_line,
            stdout.lines().next().unwrap_or_default(),
            exit_code,
        ));
    }
    if let Some(expected_command) = case.expected_command {
        let command_line = format!("  command: {expected_command}");
        if !stdout.contains(&command_line) {
            return Err(contract_failure(
                case,
                FailureClass::SchemaMismatch,
                "/data/command",
                command_line,
                "missing",
                exit_code,
            ));
        }
    }
    Ok(())
}

fn validate_contract_golden(
    case: ContractCase,
    stdout: &str,
    exit_code: Option<i32>,
) -> TestResult {
    let path = case.fixture_path();
    let expected = fs::read_to_string(&path).map_err(|error| {
        contract_failure(
            case,
            FailureClass::GoldenDrift,
            "/fixture",
            path.display().to_string(),
            error.to_string(),
            exit_code,
        )
    })?;
    let expected_normalized = normalize_json_for_golden(&expected);
    let actual_normalized = normalize_json_for_golden(stdout);
    if expected_normalized == actual_normalized {
        return Ok(());
    }

    let pointer =
        first_json_diff_pointer(&expected_normalized, &actual_normalized).unwrap_or("/stdout");
    Err(contract_failure(
        case,
        FailureClass::GoldenDrift,
        pointer,
        expected_normalized,
        actual_normalized,
        exit_code,
    ))
}

fn first_json_diff_pointer(expected: &str, actual: &str) -> Option<&'static str> {
    let expected_value = serde_json::from_str::<Value>(expected).ok()?;
    let actual_value = serde_json::from_str::<Value>(actual).ok()?;
    Some(first_value_diff_pointer(&expected_value, &actual_value))
}

fn first_value_diff_pointer(expected: &Value, actual: &Value) -> &'static str {
    if expected == actual {
        return "/";
    }

    for pointer in [
        "/schema",
        "/success",
        "/data/command",
        "/data",
        "/error/code",
        "/error/message",
        "/error",
    ] {
        if expected.pointer(pointer) != actual.pointer(pointer) {
            return pointer;
        }
    }

    "/"
}

fn contains_unredacted_secret(output: &str) -> bool {
    output.contains("BEGIN PRIVATE KEY")
        || output.contains("sk-")
        || output.contains("ghp_")
        || output.contains("token=")
        || output.contains("api_key")
}

// =============================================================================
// Check command (health posture)
// =============================================================================

#[test]
fn check_json_output_matches_golden() -> TestResult {
    let output = run_ee(&["check", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("check --json should succeed; stderr: {stderr}"),
    )?;
    ensure(stderr.is_empty(), "check --json stderr must be empty")?;
    ensure_starts_with(
        &stdout,
        "{\"schema\":\"ee.response.v1\"",
        "check JSON schema",
    )?;
    ensure_contains(&stdout, "\"command\":\"check\"", "check JSON command")?;

    assert_golden("check", "check_json", &stdout)
}

#[test]
fn check_toon_output_matches_golden() -> TestResult {
    let output = run_ee(&["check", "--format", "toon"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("check --format toon should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "check --format toon stderr must be empty",
    )?;
    ensure_contains(&stdout, "schema: ee.response.v1", "check TOON schema")?;

    assert_golden("check", "check_toon", &stdout)
}

// =============================================================================
// Doctor command
// =============================================================================

#[test]
fn doctor_json_output_matches_golden() -> TestResult {
    let output = run_ee(&["doctor", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("doctor --json should succeed; stderr: {stderr}"),
    )?;
    ensure(stderr.is_empty(), "doctor --json stderr must be empty")?;
    ensure_starts_with(
        &stdout,
        "{\"schema\":\"ee.response.v1\"",
        "doctor JSON schema",
    )?;
    ensure_contains(&stdout, "\"command\":\"doctor\"", "doctor JSON command")?;
    ensure_contains(&stdout, "\"checks\":[", "doctor JSON checks array")?;

    assert_golden("doctor", "doctor_json", &stdout)
}

#[test]
fn doctor_toon_output_matches_golden() -> TestResult {
    let output = run_ee(&["doctor", "--format", "toon"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("doctor --format toon should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "doctor --format toon stderr must be empty",
    )?;
    ensure_contains(&stdout, "schema: ee.response.v1", "doctor TOON schema")?;

    assert_golden("doctor", "doctor_toon", &stdout)
}

// =============================================================================
// Capabilities command
// =============================================================================

#[test]
fn capabilities_json_output_matches_golden() -> TestResult {
    let output = run_ee(&["capabilities", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("capabilities --json should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "capabilities --json stderr must be empty",
    )?;
    ensure_starts_with(
        &stdout,
        "{\"schema\":\"ee.response.v1\"",
        "capabilities JSON schema",
    )?;
    ensure_contains(
        &stdout,
        "\"command\":\"capabilities\"",
        "capabilities JSON command",
    )?;
    ensure_contains(&stdout, "\"subsystems\":[", "capabilities JSON subsystems")?;
    ensure_contains(&stdout, "\"features\":[", "capabilities JSON features")?;
    ensure_contains(&stdout, "\"commands\":[", "capabilities JSON commands")?;

    assert_golden("capabilities", "capabilities_json", &stdout)
}

#[test]
fn capabilities_toon_output_matches_golden() -> TestResult {
    let output = run_ee(&["capabilities", "--format", "toon"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("capabilities --format toon should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "capabilities --format toon stderr must be empty",
    )?;
    ensure_contains(
        &stdout,
        "schema: ee.response.v1",
        "capabilities TOON schema",
    )?;

    assert_golden("capabilities", "capabilities_toon", &stdout)
}

// =============================================================================
// Status command
// =============================================================================

#[test]
fn status_json_output_matches_golden() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("status --json should succeed; stderr: {stderr}"),
    )?;
    ensure(stderr.is_empty(), "status --json stderr must be empty")?;
    ensure_starts_with(
        &stdout,
        "{\"schema\":\"ee.response.v1\"",
        "status JSON schema",
    )?;
    ensure_contains(&stdout, "\"command\":\"status\"", "status JSON command")?;

    assert_golden("status", "status_json", &stdout)
}

// =============================================================================
// Version / API version
// =============================================================================

#[test]
fn version_subcommand_matches_golden() -> TestResult {
    let output = run_ee(&["version"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure_equal(&output.status.success(), &true, "version should succeed")?;
    ensure(stderr.is_empty(), "version stderr must be empty")?;
    ensure_contains(&stdout, "ee ", "version output prefix")?;

    assert_golden("version", "version_output", &stdout)
}

// =============================================================================
// Agent docs (--agent-docs flag)
// =============================================================================

#[test]
fn agent_docs_flag_matches_golden() -> TestResult {
    let output = run_ee(&["--agent-docs"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(output.status.success(), "--agent-docs should succeed")?;
    ensure(stderr.is_empty(), "--agent-docs stderr must be empty")?;
    ensure_starts_with(
        &stdout,
        "{\"schema\":\"ee.response.v1\"",
        "agent-docs JSON schema",
    )?;
    ensure_contains(
        &stdout,
        "\"command\":\"agent-docs\"",
        "agent-docs JSON command",
    )?;
    ensure_contains(
        &stdout,
        "\"primaryWorkflow\":",
        "agent-docs primary workflow",
    )?;
    ensure_contains(&stdout, "\"coreCommands\":[", "agent-docs core commands")?;

    assert_golden("agent_docs", "agent_docs_json", &stdout)
}

// =============================================================================
// Schema (--schema flag, API version indicator)
// =============================================================================

#[test]
fn schema_flag_matches_golden() -> TestResult {
    let output = run_ee(&["--schema"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(output.status.success(), "--schema should succeed")?;
    ensure(stderr.is_empty(), "--schema stderr must be empty")?;
    ensure_starts_with(
        &stdout,
        "{\"schema\":\"ee.response.v1\"",
        "schema JSON envelope",
    )?;
    ensure_contains(&stdout, "\"command\":\"schema\"", "schema JSON command")?;

    assert_golden("schema", "schema_json", &stdout)
}

// =============================================================================
// Contract stability tests
// =============================================================================

#[test]
fn all_json_commands_have_schema_envelope() -> TestResult {
    let commands: &[&[&str]] = &[
        &["status", "--json"],
        &["check", "--json"],
        &["doctor", "--json"],
        &["capabilities", "--json"],
        &["--schema"],
        &["--help-json"],
        &["--agent-docs"],
    ];

    for args in commands {
        let output = run_ee(args)?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        ensure(
            output.status.success(),
            format!("{} should succeed; stderr: {stderr}", args.join(" ")),
        )?;
        ensure(
            stderr.is_empty(),
            format!("{} stderr must be empty", args.join(" ")),
        )?;
        ensure_starts_with(
            &stdout,
            "{\"schema\":\"ee.response.v1\"",
            &format!("{} JSON schema envelope", args.join(" ")),
        )?;
    }

    Ok(())
}

#[test]
fn error_responses_have_error_schema_envelope() -> TestResult {
    let output = run_ee(&["--json", "not-a-command"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(!output.status.success(), "error should not succeed")?;
    ensure(stderr.is_empty(), "json error stderr must be empty")?;
    ensure_starts_with(
        &stdout,
        "{\"schema\":\"ee.error.v1\"",
        "error JSON schema envelope",
    )?;
    ensure_contains(&stdout, "\"code\":\"usage\"", "error code")?;

    Ok(())
}

#[test]
fn golden_schema_contract_runner_validates_current_stage() -> TestResult {
    for case in current_stage_contract_cases() {
        validate_contract_case(*case)?;
    }

    Ok(())
}

#[test]
fn contract_failure_report_includes_debugging_context() -> TestResult {
    let case = ContractCase {
        name: "status_json",
        args: &["status", "--json"],
        category: "status",
        golden_name: "status_json",
        format: ContractFormat::Json,
        expected_success: true,
        expected_schema: Some("ee.response.v1"),
        expected_command: Some("status"),
    };

    let report = contract_failure(
        case,
        FailureClass::SchemaMismatch,
        "/data/command",
        "status",
        "doctor",
        Some(0),
    );

    ensure_contains(&report, "class: schema_mismatch", "failure class")?;
    ensure_contains(&report, "command: ee status --json", "command")?;
    ensure_contains(&report, "exit_code: Some(0)", "exit code")?;
    ensure_contains(&report, "schema: ee.response.v1", "schema")?;
    ensure_contains(&report, "json_pointer: /data/command", "pointer")?;
    ensure_contains(
        &report,
        "tests/fixtures/golden/status/status_json.golden",
        "fixture path",
    )?;
    ensure_contains(&report, "stdout_artifact:", "stdout artifact")?;
    ensure_contains(&report, "stderr_artifact:", "stderr artifact")?;
    ensure_contains(&report, "expected: status", "expected value")?;
    ensure_contains(&report, "actual: doctor", "actual value")
}
