#![cfg(unix)]

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;
const EXIT_IMPORT: i32 = 5;

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

fn unique_artifact_dir(name: &str) -> Result<PathBuf, String> {
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock before UNIX_EPOCH: {error}"))?
        .as_nanos();
    let dir = target_dir
        .join("ee-test-artifacts")
        .join("no-silent-fallback-e2e")
        .join(format!("{}-{}-{nanos}", name, std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create artifact dir {}: {error}", dir.display()))?;
    Ok(dir)
}

fn collect_failure_codes(value: &Value) -> Vec<String> {
    let mut codes = BTreeSet::new();

    if let Some(code) = value.pointer("/error/code").and_then(Value::as_str) {
        codes.insert(code.to_owned());
    }

    for pointer in ["/data/issues", "/data/degraded", "/degraded"] {
        if let Some(items) = value.pointer(pointer).and_then(Value::as_array) {
            for item in items {
                if let Some(code) = item.get("code").and_then(Value::as_str) {
                    codes.insert(code.to_owned());
                }
            }
        }
    }

    codes.into_iter().collect()
}

fn first_failure_diagnosis(value: &Value, stderr: &str) -> Option<String> {
    value
        .pointer("/error/code")
        .and_then(Value::as_str)
        .map(|code| format!("error:{code}"))
        .or_else(|| {
            value
                .pointer("/data/issues")
                .and_then(Value::as_array)
                .and_then(|issues| issues.first())
                .and_then(|issue| issue.get("code"))
                .and_then(Value::as_str)
                .map(|code| format!("issue:{code}"))
        })
        .or_else(|| {
            if stderr.is_empty() {
                None
            } else {
                Some("stderr_not_empty".to_owned())
            }
        })
}

fn run_ee_logged<I, S>(
    name: &str,
    workspace: &Path,
    args: I,
    env_overrides: &[(&str, String)],
) -> Result<LoggedCommand, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let dossier_dir = unique_artifact_dir(name)?;
    let stdout_path = dossier_dir.join("stdout.json");
    let stderr_path = dossier_dir.join("stderr.txt");
    let log_path = dossier_dir.join("e2e-log.json");
    let cwd = env::current_dir().map_err(|error| format!("failed to resolve cwd: {error}"))?;

    let mut argv = vec![
        "--workspace".to_owned(),
        workspace.display().to_string(),
        "--json".to_owned(),
    ];
    argv.extend(
        args.into_iter()
            .map(|arg| arg.as_ref().to_string_lossy().into_owned()),
    );

    let start = Instant::now();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command
        .args(&argv)
        .current_dir(&cwd)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env_remove("EE_CASS_BINARY")
        .env("NO_COLOR", "1");
    for (key, value) in env_overrides {
        command.env(key, value);
    }
    let output = command
        .output()
        .map_err(|error| format!("failed to run ee {:?}: {error}", argv))?;
    let elapsed = start.elapsed();

    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("stdout was not valid UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("stderr was not valid UTF-8: {error}"))?;
    fs::write(&stdout_path, &stdout)
        .map_err(|error| format!("failed to write {}: {error}", stdout_path.display()))?;
    fs::write(&stderr_path, &stderr)
        .map_err(|error| format!("failed to write {}: {error}", stderr_path.display()))?;

    let parsed: Value = serde_json::from_str(&stdout)
        .map_err(|error| format!("stdout must be JSON: {error}\nstdout: {stdout}"))?;
    let failure_codes = collect_failure_codes(&parsed);
    let diagnosis = first_failure_diagnosis(&parsed, &stderr);
    let parsed_schema = parsed
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or("<missing>")
        .to_owned();
    let exit_code = output.status.code().unwrap_or(-1);

    let log = json!({
        "schema": "ee.no_silent_fallback.e2e_log.v1",
        "command": "ee",
        "argv": argv,
        "cwd": cwd.display().to_string(),
        "workspace": workspace.display().to_string(),
        "env": {
            "CARGO_TARGET_DIR": env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "<unset>".to_owned()),
            "EE_CASS_BINARY": if env_overrides.iter().any(|(key, _)| *key == "EE_CASS_BINARY") { "<set>" } else { "<unset>" },
            "NO_COLOR": "1"
        },
        "elapsedMs": elapsed.as_millis(),
        "exitCode": exit_code,
        "stdoutPath": stdout_path.display().to_string(),
        "stderrPath": stderr_path.display().to_string(),
        "parsedJsonSchema": parsed_schema,
        "failureCodes": failure_codes,
        "firstFailureDiagnosis": diagnosis
    });
    let mut log_text = serde_json::to_string_pretty(&log)
        .map_err(|error| format!("failed to serialize e2e log: {error}"))?;
    log_text.push('\n');
    fs::write(&log_path, log_text)
        .map_err(|error| format!("failed to write {}: {error}", log_path.display()))?;

    Ok(LoggedCommand {
        stdout,
        stderr,
        exit_code,
        parsed,
        log_path,
    })
}

fn assert_logged_failure_shape(
    result: &LoggedCommand,
    expected_diagnosis: &str,
    expected_codes: &[&str],
) -> TestResult {
    let log_text = fs::read_to_string(&result.log_path)
        .map_err(|error| format!("failed to read {}: {error}", result.log_path.display()))?;
    let log: Value = serde_json::from_str(&log_text)
        .map_err(|error| format!("e2e log must be JSON: {error}"))?;

    ensure_equal(
        &log.pointer("/schema"),
        &Some(&json!("ee.no_silent_fallback.e2e_log.v1")),
        "e2e log schema",
    )?;
    ensure(
        log.pointer("/stdoutPath")
            .and_then(Value::as_str)
            .is_some_and(|path| path.ends_with("stdout.json")),
        "log records stdout artifact path",
    )?;
    ensure(
        log.pointer("/stderrPath")
            .and_then(Value::as_str)
            .is_some_and(|path| path.ends_with("stderr.txt")),
        "log records stderr artifact path",
    )?;
    ensure_equal(
        &log.pointer("/firstFailureDiagnosis"),
        &Some(&json!(expected_diagnosis)),
        "logged first failure diagnosis",
    )?;

    for expected in expected_codes {
        ensure(
            log.pointer("/failureCodes")
                .and_then(Value::as_array)
                .is_some_and(|codes| codes.iter().any(|code| code.as_str() == Some(*expected))),
            format!("log must include failure code {expected}: {log}"),
        )?;
    }

    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> TestResult {
    let mut permissions = fs::metadata(path)
        .map_err(|error| format!("metadata {}: {error}", path.display()))?
        .permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)
        .map_err(|error| format!("chmod {}: {error}", path.display()))
}

fn write_failing_cass_binary(path: &Path) -> TestResult {
    let script = r#"#!/bin/sh
set -eu
cmd="${1:-}"
case "$cmd" in
  sessions)
    echo "sos5.6 cass sessions subprocess failed" >&2
    exit 64
    ;;
  view)
    printf '{"lines":[]}\n'
    ;;
  *)
    echo "unexpected cass command: $cmd" >&2
    exit 65
    ;;
esac
"#;
    fs::write(path, script).map_err(|error| format!("write {}: {error}", path.display()))?;
    set_mode(path, 0o755)
}

#[test]
fn cass_subprocess_failure_uses_json_error_envelope_and_logged_evidence() -> TestResult {
    let root = unique_artifact_dir("cass-subprocess-failure")?;
    let workspace = root.join("workspace");
    let bin_dir = root.join("bin");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    fs::create_dir_all(&bin_dir).map_err(|error| error.to_string())?;
    set_mode(&bin_dir, 0o755)?;

    let cass_binary = bin_dir.join("cass");
    write_failing_cass_binary(&cass_binary)?;
    let cass_binary = cass_binary
        .canonicalize()
        .map_err(|error| format!("canonicalize cass binary: {error}"))?;

    let result = run_ee_logged(
        "cass-subprocess-failure",
        &workspace,
        ["import", "cass", "--limit", "1", "--dry-run"],
        &[("EE_CASS_BINARY", cass_binary.to_string_lossy().into_owned())],
    )?;

    ensure_equal(
        &result.exit_code,
        &EXIT_IMPORT,
        "cass subprocess failure exit code",
    )?;
    ensure(
        result.stderr.is_empty(),
        "json error path must keep ee stderr empty",
    )?;
    ensure_equal(
        &result.parsed.pointer("/schema"),
        &Some(&json!("ee.error.v2")),
        "cass error schema",
    )?;
    ensure_equal(
        &result.parsed.pointer("/error/code"),
        &Some(&json!("import")),
        "cass error code",
    )?;
    ensure_equal(
        &result.parsed.pointer("/error/repair"),
        &Some(&json!("run cass health --json")),
        "cass repair hint",
    )?;
    ensure(
        result
            .parsed
            .pointer("/error/message")
            .and_then(Value::as_str)
            .is_some_and(|message| {
                message.contains("cass sessions")
                    && message.contains("exit Some(64)")
                    && message.contains("sos5.6 cass sessions subprocess failed")
            }),
        format!(
            "cass error message must preserve command, exit, and child stderr: {}",
            result.stdout
        ),
    )?;
    ensure(
        !result.stdout.contains("\"success\":true"),
        "cass subprocess failure must not look like a successful empty import",
    )?;
    assert_logged_failure_shape(&result, "error:import", &["import"])
}

#[test]
fn malformed_jsonl_import_reports_rejected_contract_with_issue_codes() -> TestResult {
    let root = unique_artifact_dir("malformed-jsonl-import")?;
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;

    let source = root.join("malformed-export.jsonl");
    fs::write(&source, "{ not json\n")
        .map_err(|error| format!("write {}: {error}", source.display()))?;
    let source_arg = source.to_string_lossy().into_owned();

    let result = run_ee_logged(
        "malformed-jsonl-import",
        &workspace,
        [
            "import",
            "jsonl",
            "--source",
            source_arg.as_str(),
            "--dry-run",
        ],
        &[],
    )?;

    ensure_equal(
        &result.exit_code,
        &EXIT_SUCCESS,
        "malformed JSONL rejection remains a parseable report",
    )?;
    ensure(
        result.stderr.is_empty(),
        "malformed JSONL report must keep stderr empty in JSON mode",
    )?;
    ensure_equal(
        &result.parsed.pointer("/schema"),
        &Some(&json!("ee.response.v1")),
        "jsonl response envelope",
    )?;
    ensure_equal(
        &result.parsed.pointer("/success"),
        &Some(&json!(true)),
        "jsonl response success flag",
    )?;
    ensure_equal(
        &result.parsed.pointer("/data/schema"),
        &Some(&json!("ee.import.jsonl.v1")),
        "jsonl import schema",
    )?;
    ensure_equal(
        &result.parsed.pointer("/data/status"),
        &Some(&json!("rejected")),
        "jsonl rejected status",
    )?;
    ensure_equal(
        &result.parsed.pointer("/data/memoriesImported"),
        &Some(&json!(0)),
        "jsonl rejected import count",
    )?;

    let issues = result
        .parsed
        .pointer("/data/issues")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("jsonl import must expose issues: {}", result.stdout))?;
    ensure(
        issues.iter().any(|issue| {
            issue.get("severity").and_then(Value::as_str) == Some("error")
                && issue.get("code").and_then(Value::as_str) == Some("invalid_json")
        }),
        format!("jsonl import must report invalid_json: {issues:?}"),
    )?;
    ensure(
        issues
            .iter()
            .any(|issue| issue.get("code").and_then(Value::as_str) == Some("missing_header")),
        format!("jsonl import must report missing_header: {issues:?}"),
    )?;
    ensure(
        !issues.is_empty() && result.stdout.contains("\"issues\""),
        "malformed JSONL must not be represented as an empty successful data object",
    )?;

    assert_logged_failure_shape(
        &result,
        "issue:invalid_json",
        &["invalid_json", "missing_header"],
    )
}
