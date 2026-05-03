use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ee::core::degraded_honesty::validate_no_fake_success_output;
use serde_json::{Value, json};

type TestResult = Result<(), String>;

struct LoggedCommand {
    stdout: String,
    stderr: String,
    exit_code: i32,
    parsed: Value,
    log_path: PathBuf,
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
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn ensure_json_pointer(value: &Value, pointer: &str, expected: Value, context: &str) -> TestResult {
    let actual = value
        .pointer(pointer)
        .ok_or_else(|| format!("{context}: missing JSON pointer {pointer}"))?;
    ensure_equal(actual, &expected, context)
}

fn ensure_no_ansi(text: &str, context: &str) -> TestResult {
    ensure(
        !text.contains("\u{1b}["),
        format!("{context} must not contain ANSI styling"),
    )
}

fn unique_artifact_dir(name: &str) -> Result<PathBuf, String> {
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock before UNIX_EPOCH: {error}"))?
        .as_nanos();
    let dir = target_dir
        .join("ee-test-artifacts")
        .join("degraded-honesty")
        .join(format!("{}-{}-{nanos}", name, std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create artifact dir {}: {error}", dir.display()))?;
    Ok(dir)
}

fn collect_degradation_codes(value: &Value) -> Vec<String> {
    let mut codes = Vec::new();

    if let Some(code) = value.pointer("/error/code").and_then(Value::as_str) {
        codes.push(code.to_owned());
    }

    for pointer in ["/degraded", "/data/degraded"] {
        if let Some(items) = value.pointer(pointer).and_then(Value::as_array) {
            for item in items {
                if let Some(code) = item.get("code").and_then(Value::as_str) {
                    codes.push(code.to_owned());
                }
            }
        }
    }

    codes.sort();
    codes.dedup();
    codes
}

fn run_ee_logged(
    name: &str,
    workspace: Option<&Path>,
    args: Vec<String>,
) -> Result<LoggedCommand, String> {
    let dossier_dir = unique_artifact_dir(name)?;
    let stdout_path = dossier_dir.join("stdout.json");
    let stderr_path = dossier_dir.join("stderr.txt");
    let log_path = dossier_dir.join("e2e-log.json");
    let cwd = env::current_dir().map_err(|error| format!("failed to resolve cwd: {error}"))?;

    let start = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(&args)
        .current_dir(&cwd)
        .output()
        .map_err(|error| format!("failed to run ee {:?}: {error}", args))?;
    let elapsed = start.elapsed();

    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("stdout was not valid UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("stderr was not valid UTF-8: {error}"))?;
    fs::write(&stdout_path, &stdout)
        .map_err(|error| format!("failed to write {}: {error}", stdout_path.display()))?;
    fs::write(&stderr_path, &stderr)
        .map_err(|error| format!("failed to write {}: {error}", stderr_path.display()))?;

    let parsed_result: Result<Value, _> = serde_json::from_str(&stdout);
    let (parsed_json_schema, degradation_codes, first_failure_diagnosis) = match &parsed_result {
        Ok(value) => (
            value
                .get("schema")
                .and_then(Value::as_str)
                .unwrap_or("<missing>")
                .to_owned(),
            collect_degradation_codes(value),
            if stderr.is_empty() {
                None
            } else {
                Some("stderr_not_empty".to_owned())
            },
        ),
        Err(error) => (
            "<invalid-json>".to_owned(),
            Vec::new(),
            Some(format!("stdout_json_parse_failed: {error}")),
        ),
    };

    let exit_code = output.status.code().unwrap_or(-1);
    let log = json!({
        "schema": "ee.degraded_honesty.e2e_log.v1",
        "command": "ee",
        "argv": args,
        "cwd": cwd.display().to_string(),
        "workspace": workspace.map(|path| path.display().to_string()),
        "env": {
            "CARGO_TARGET_DIR": env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "<unset>".to_owned()),
            "RUST_LOG": "<redacted>"
        },
        "elapsedMs": elapsed.as_millis(),
        "exitCode": exit_code,
        "stdoutPath": stdout_path.display().to_string(),
        "stderrPath": stderr_path.display().to_string(),
        "parsedJsonSchema": parsed_json_schema,
        "goldenValidation": "not_applicable",
        "redactionStatus": "not_applicable",
        "evidenceIds": [],
        "degradationCodes": degradation_codes,
        "firstFailureDiagnosis": first_failure_diagnosis
    });
    let mut log_text = serde_json::to_string_pretty(&log)
        .map_err(|error| format!("failed to serialize e2e log: {error}"))?;
    log_text.push('\n');
    fs::write(&log_path, log_text)
        .map_err(|error| format!("failed to write {}: {error}", log_path.display()))?;

    let parsed = parsed_result.map_err(|error| format!("stdout must be JSON: {error}"))?;
    Ok(LoggedCommand {
        stdout,
        stderr,
        exit_code,
        parsed,
        log_path,
    })
}

#[test]
fn context_without_database_uses_honest_error_envelope_and_e2e_log() -> TestResult {
    let workspace_root = unique_artifact_dir("context-missing-db-workspace")?;
    let workspace = workspace_root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| {
        format!(
            "failed to create workspace {}: {error}",
            workspace.display()
        )
    })?;
    let workspace_arg = workspace.display().to_string();

    let result = run_ee_logged(
        "context-missing-db",
        Some(&workspace),
        vec![
            "--workspace".to_owned(),
            workspace_arg,
            "--json".to_owned(),
            "context".to_owned(),
            "prepare release".to_owned(),
        ],
    )?;

    ensure_equal(&result.exit_code, &3, "storage error exit code")?;
    ensure(
        result.stderr.is_empty(),
        "JSON error path must keep stderr empty",
    )?;
    ensure_no_ansi(&result.stdout, "context error stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.error.v1"),
        "error envelope schema",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/error/code",
        json!("storage"),
        "error code",
    )?;
    ensure_json_pointer(
        &result.parsed,
        "/error/repair",
        json!("ee init --workspace ."),
        "repair command",
    )?;

    let fake_success = validate_no_fake_success_output("context", false, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("failed commands should not be treated as fake success: {fake_success:?}"),
    )?;

    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log_json: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;
    ensure_json_pointer(
        &log_json,
        "/schema",
        json!("ee.degraded_honesty.e2e_log.v1"),
        "e2e log schema",
    )?;
    ensure_json_pointer(&log_json, "/exitCode", json!(3), "logged exit code")?;
    ensure_json_pointer(
        &log_json,
        "/parsedJsonSchema",
        json!("ee.error.v1"),
        "logged parsed schema",
    )?;
    ensure_json_pointer(
        &log_json,
        "/degradationCodes",
        json!(["storage"]),
        "logged degradation/error code",
    )
}

#[test]
fn successful_capabilities_output_has_no_fake_success_markers() -> TestResult {
    let result = run_ee_logged(
        "capabilities-success",
        None,
        vec!["--json".to_owned(), "capabilities".to_owned()],
    )?;

    ensure_equal(&result.exit_code, &0, "capabilities exit code")?;
    ensure(
        result.stderr.is_empty(),
        "capabilities stderr must be empty",
    )?;
    ensure_no_ansi(&result.stdout, "capabilities stdout")?;
    ensure_json_pointer(
        &result.parsed,
        "/schema",
        json!("ee.response.v1"),
        "response envelope schema",
    )?;
    ensure_json_pointer(&result.parsed, "/success", json!(true), "success flag")?;

    let fake_success = validate_no_fake_success_output("capabilities", true, false, &result.stdout);
    ensure(
        fake_success.passed,
        format!("capabilities output contains fake success marker: {fake_success:?}"),
    )
}
