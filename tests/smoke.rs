#[cfg(unix)]
use std::ffi::OsString;
use std::fmt::Debug;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

type TestResult = Result<(), String>;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

#[cfg(unix)]
fn run_ee_with_env(args: &[&str], envs: &[(&str, OsString)]) -> Result<Output, String> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

#[cfg(unix)]
fn unique_artifact_dir(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-smoke-artifacts")
        .join(format!("{prefix}-{}-{now}", std::process::id())))
}

#[cfg(unix)]
fn path_with_fake_cass(fake_dir: &Path) -> Result<OsString, String> {
    let mut entries = vec![fake_dir.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        entries.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(entries).map_err(|error| error.to_string())
}

#[cfg(unix)]
fn write_fake_cass_binary(path: &Path) -> TestResult {
    let script = r#"#!/bin/sh
set -eu
cmd="${1:-}"
case "$cmd" in
  sessions)
    printf '{"sessions":[{"path":"%s","workspace":"%s","agent":"codex","started_at":"2026-04-30T00:00:00Z","message_count":2,"token_count":42,"content_hash":"hash-session-a"}]}\n' "$EE_FAKE_CASS_SESSION" "$EE_FAKE_CASS_WORKSPACE"
    ;;
  view)
    printf '{"lines":[{"line":1,"content":"{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"remember this\"}}"}]}\n'
    ;;
  *)
    echo "unexpected cass command: $cmd" >&2
    exit 64
    ;;
esac
"#;
    fs::write(path, script).map_err(|error| error.to_string())?;
    let mut permissions = fs::metadata(path)
        .map_err(|error| error.to_string())?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).map_err(|error| error.to_string())
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
        format!("{context}: expected output to contain {needle:?}, got {haystack:?}"),
    )
}

fn ensure_starts_with(haystack: &str, prefix: &str, context: &str) -> TestResult {
    ensure(
        haystack.starts_with(prefix),
        format!("{context}: expected output to start with {prefix:?}, got {haystack:?}"),
    )
}

fn ensure_ends_with(haystack: &str, suffix: char, context: &str) -> TestResult {
    ensure(
        haystack.ends_with(suffix),
        format!("{context}: expected output to end with {suffix:?}, got {haystack:?}"),
    )
}

#[test]
fn status_json_stdout_is_stable_machine_data() -> TestResult {
    let output = run_ee(&["status", "--json"])?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("status --json should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "stderr must be empty for JSON status".to_string(),
    )?;
    ensure_starts_with(
        &stdout,
        "{\"schema\":\"ee.response.v1\"",
        "status JSON schema",
    )?;
    ensure_contains(&stdout, "\"success\":true", "status JSON success flag")?;
    ensure_contains(&stdout, "\"command\":\"status\"", "status JSON command")?;
    ensure_contains(
        &stdout,
        "\"runtime\":\"ready\"",
        "status JSON runtime state",
    )?;
    ensure_contains(
        &stdout,
        "\"engine\":\"asupersync\"",
        "status JSON runtime engine",
    )?;
    ensure_ends_with(&stdout, '\n', "status JSON trailing newline")
}

#[test]
fn global_json_flag_is_order_independent() -> TestResult {
    let before = run_ee(&["--json", "status"])?;
    let after = run_ee(&["status", "--json"])?;

    ensure(before.status.success(), "--json status should succeed")?;
    ensure(after.status.success(), "status --json should succeed")?;
    ensure_equal(
        &before.stdout,
        &after.stdout,
        "global --json output must be order independent",
    )?;
    ensure(
        before.stderr.is_empty(),
        "--json status stderr must be empty",
    )?;
    ensure(
        after.stderr.is_empty(),
        "status --json stderr must be empty",
    )
}

#[test]
fn format_json_global_selects_machine_output() -> TestResult {
    let output = run_ee(&["status", "--format", "json"])?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("status --format json should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "stderr must be empty for JSON status".to_string(),
    )?;
    ensure_starts_with(
        &stdout,
        "{\"schema\":\"ee.response.v1\"",
        "format JSON schema",
    )
}

#[test]
fn robot_global_selects_machine_output() -> TestResult {
    let output = run_ee(&["status", "--robot"])?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("status --robot should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "stderr must be empty for robot status".to_string(),
    )?;
    ensure_starts_with(
        &stdout,
        "{\"schema\":\"ee.response.v1\"",
        "robot JSON schema",
    )
}

#[test]
fn clap_help_keeps_stderr_clean() -> TestResult {
    let output = run_ee(&["--help"])?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        output.status.success(),
        format!("--help should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        "help must not write diagnostics".to_string(),
    )?;
    ensure_contains(&stdout, "Usage:", "help usage line")?;
    ensure_contains(&stdout, "status", "help status subcommand")
}

#[test]
fn unknown_command_keeps_stdout_clean() -> TestResult {
    let output = run_ee(&["unknown"])?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure(
        !output.status.success(),
        "unknown command must fail with usage error",
    )?;
    ensure(
        stdout.is_empty(),
        "stdout must stay clean on usage errors".to_string(),
    )?;
    ensure_contains(
        &stderr,
        "error: unrecognized subcommand",
        "unknown command diagnostic",
    )
}

#[cfg(unix)]
#[test]
fn import_cass_json_uses_cass_robot_contract_and_is_idempotent() -> TestResult {
    let root = unique_artifact_dir("import-cass")?;
    let workspace = root.join("workspace");
    let fake_bin_dir = root.join("bin");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    fs::create_dir_all(&fake_bin_dir).map_err(|error| error.to_string())?;
    let cass_binary = fake_bin_dir.join("cass");
    write_fake_cass_binary(&cass_binary)?;

    let database = workspace.join(".ee").join("ee.db");
    let session_path = workspace.join("session-a.jsonl");
    let workspace_arg = workspace.to_string_lossy().into_owned();
    let database_arg = database.to_string_lossy().into_owned();
    let session_arg = session_path.to_string_lossy().into_owned();
    let path = path_with_fake_cass(&fake_bin_dir)?;
    let envs = [
        ("PATH", path),
        ("EE_FAKE_CASS_SESSION", OsString::from(session_arg)),
        (
            "EE_FAKE_CASS_WORKSPACE",
            OsString::from(workspace_arg.clone()),
        ),
    ];
    let args = [
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "import",
        "cass",
        "--database",
        database_arg.as_str(),
        "--limit",
        "1",
    ];

    let first = run_ee_with_env(&args, &envs)?;
    let first_stderr = String::from_utf8_lossy(&first.stderr);
    ensure(
        first.status.success(),
        format!("first import should succeed; stderr: {first_stderr}"),
    )?;
    ensure(
        first.stderr.is_empty(),
        "first import stderr must stay clean",
    )?;
    let first_json: serde_json::Value = serde_json::from_slice(&first.stdout)
        .map_err(|error| format!("first import stdout must be JSON: {error}"))?;
    ensure_equal(
        &first_json["schema"],
        &serde_json::json!("ee.response.v1"),
        "first envelope schema",
    )?;
    ensure_equal(
        &first_json["success"],
        &serde_json::json!(true),
        "first success",
    )?;
    ensure_equal(
        &first_json["data"]["command"],
        &serde_json::json!("import cass"),
        "first command",
    )?;
    ensure_equal(
        &first_json["data"]["status"],
        &serde_json::json!("completed"),
        "first import status",
    )?;
    ensure_equal(
        &first_json["data"]["sessionsImported"],
        &serde_json::json!(1),
        "first imported count",
    )?;
    ensure_equal(
        &first_json["data"]["spansImported"],
        &serde_json::json!(1),
        "first span count",
    )?;
    ensure(database.exists(), "import should create the database")?;

    let second = run_ee_with_env(&args, &envs)?;
    let second_stderr = String::from_utf8_lossy(&second.stderr);
    ensure(
        second.status.success(),
        format!("second import should succeed; stderr: {second_stderr}"),
    )?;
    ensure(
        second.stderr.is_empty(),
        "second import stderr must stay clean",
    )?;
    let second_json: serde_json::Value = serde_json::from_slice(&second.stdout)
        .map_err(|error| format!("second import stdout must be JSON: {error}"))?;
    ensure_equal(
        &second_json["data"]["sessionsImported"],
        &serde_json::json!(0),
        "second imported count",
    )?;
    ensure_equal(
        &second_json["data"]["sessionsSkipped"],
        &serde_json::json!(1),
        "second skipped count",
    )?;
    ensure_equal(
        &second_json["data"]["sessions"][0]["status"],
        &serde_json::json!("skipped"),
        "second session status",
    )
}

// =============================================================================
// Integration Foundation Smoke Tests (EE-313)
//
// These tests verify that the foundational integrations are working:
// - Response envelope schema (ee.response.v1)
// - Asupersync runtime bootstrap
// - SQLModel/FrankenSQLite repository shape (reports degraded until wired)
// - Frankensearch persistent index (reports degraded until wired)
// - CLI runtime boundary and exit codes
// =============================================================================

#[test]
fn integration_foundation_response_envelope_schema() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(
        output.status.success(),
        "status --json must succeed for envelope test",
    )?;

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("response envelope must be valid JSON: {e}"))?;

    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.response.v1"),
        "response envelope schema must be ee.response.v1",
    )?;
    ensure(
        json["success"].as_bool().unwrap_or(false),
        "response envelope success must be true",
    )?;
    ensure(
        json["data"].is_object(),
        "response envelope must have data object",
    )
}

#[test]
fn integration_foundation_asupersync_runtime_reports_correctly() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(
        output.status.success(),
        "status must succeed for runtime test",
    )?;

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("status output must be valid JSON: {e}"))?;

    let runtime = &json["data"]["runtime"];
    ensure(runtime.is_object(), "status must include runtime object")?;
    ensure_equal(
        &runtime["engine"],
        &serde_json::json!("asupersync"),
        "runtime engine must be asupersync",
    )?;
    ensure(
        runtime["profile"].is_string(),
        "runtime profile must be a string",
    )?;
    ensure(
        runtime["workerThreads"].is_number(),
        "runtime workerThreads must be a number",
    )
}

#[test]
fn integration_foundation_capability_status_structure() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(
        output.status.success(),
        "status must succeed for capability test",
    )?;

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("status output must be valid JSON: {e}"))?;

    let capabilities = &json["data"]["capabilities"];
    ensure(
        capabilities.is_object(),
        "status must include capabilities object",
    )?;
    ensure_equal(
        &capabilities["runtime"],
        &serde_json::json!("ready"),
        "runtime capability must be ready",
    )?;
    ensure(
        capabilities["storage"].is_string(),
        "storage capability status must be a string",
    )?;
    ensure(
        capabilities["search"].is_string(),
        "search capability status must be a string",
    )
}

#[test]
fn integration_foundation_degradation_codes_present_when_unimplemented() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(
        output.status.success(),
        "status must succeed for degradation test",
    )?;

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("status output must be valid JSON: {e}"))?;

    let capabilities = &json["data"]["capabilities"];
    let degradations = &json["data"]["degraded"];

    ensure(
        degradations.is_array(),
        "status must include degraded array",
    )?;

    let storage_status = capabilities["storage"].as_str().unwrap_or("");
    let search_status = capabilities["search"].as_str().unwrap_or("");

    if storage_status == "unimplemented" {
        let has_storage_degradation = degradations
            .as_array()
            .map(|arr| {
                arr.iter()
                    .any(|d| d["code"].as_str() == Some("storage_not_implemented"))
            })
            .unwrap_or(false);
        ensure(
            has_storage_degradation,
            "storage_not_implemented degradation must be present when storage is unimplemented",
        )?;
    }

    if search_status == "unimplemented" {
        let has_search_degradation = degradations
            .as_array()
            .map(|arr| {
                arr.iter()
                    .any(|d| d["code"].as_str() == Some("search_not_implemented"))
            })
            .unwrap_or(false);
        ensure(
            has_search_degradation,
            "search_not_implemented degradation must be present when search is unimplemented",
        )?;
    }

    Ok(())
}

#[test]
fn integration_foundation_degradation_includes_repair_hints() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(
        output.status.success(),
        "status must succeed for repair hint test",
    )?;

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("status output must be valid JSON: {e}"))?;

    let degradations = &json["data"]["degraded"];
    let arr = degradations.as_array().ok_or("degraded must be an array")?;

    for degradation in arr {
        ensure(
            degradation["code"].is_string(),
            "each degradation must have a code string",
        )?;
        ensure(
            degradation["severity"].is_string(),
            "each degradation must have a severity string",
        )?;
        ensure(
            degradation["message"].is_string(),
            "each degradation must have a message string",
        )?;
        ensure(
            degradation["repair"].is_string(),
            "each degradation must have a repair hint string",
        )?;
    }

    Ok(())
}

#[test]
fn integration_foundation_exit_code_zero_on_success() -> TestResult {
    let output = run_ee(&["status"])?;

    ensure(
        output.status.success(),
        "status command must exit with code 0",
    )?;
    ensure_equal(
        &output.status.code(),
        &Some(0),
        "exit code must be exactly 0 for successful status",
    )
}

#[test]
fn integration_foundation_version_reported_in_status() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(
        output.status.success(),
        "status must succeed for version test",
    )?;

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("status output must be valid JSON: {e}"))?;

    let version = json["data"]["version"]
        .as_str()
        .ok_or("version must be a string in status data")?;
    ensure(!version.is_empty(), "version string must not be empty")?;
    ensure(
        version
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false),
        "version must start with a digit (semantic versioning)",
    )
}

#[test]
fn integration_foundation_memory_health_structure() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    ensure(
        output.status.success(),
        "status must succeed for memory health test",
    )?;

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("status output must be valid JSON: {e}"))?;

    let memory_health = &json["data"]["memoryHealth"];
    ensure(
        memory_health.is_object(),
        "status must include memoryHealth object",
    )?;
    ensure(
        memory_health["status"].is_string(),
        "memoryHealth must have status string",
    )?;
    ensure(
        memory_health["totalCount"].is_number(),
        "memoryHealth must have totalCount number",
    )?;
    ensure(
        memory_health["activeCount"].is_number(),
        "memoryHealth must have activeCount number",
    )
}
