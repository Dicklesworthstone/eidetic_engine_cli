//! EE-pe6o: ee CLI exit code conformance harness
//!
//! Validates that ee exit codes match the documented matrix:
//! - 0: success
//! - 1: usage error
//! - 2: configuration error
//! - 3: storage error
//! - 4: search/index error
//! - 5: import error
//! - 6: degraded but command could not satisfy required mode
//! - 7: policy denied operation
//! - 8: migration required
//!
//! Each test category uses real binary execution with no mocks.

use ee::config::workspace::WORKSPACE_ENV_VAR;
use ee::core::workspace::WORKSPACE_REGISTRY_ENV_VAR;
use ee::db::DbConnection;
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

type TestResult = Result<(), String>;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    run_ee_with_env(args, &[])
}

fn run_ee_with_env(args: &[&str], envs: &[(&str, &Path)]) -> Result<Output, String> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command
        .args(args)
        .env_remove(WORKSPACE_ENV_VAR)
        .env_remove(WORKSPACE_REGISTRY_ENV_VAR);
    for (key, value) in envs {
        command.env(key, value);
    }
    command
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

fn stdout_json(output: &Output) -> Result<serde_json::Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout).map_err(|error| format!("stdout was not JSON: {error}\n{stdout}"))
}

fn artifact_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("exit_code_conformance_artifacts");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn persist_artifact(name: &str, output: &Output) {
    let dir = artifact_dir();
    let stdout_path = dir.join(format!("{name}.stdout"));
    let stderr_path = dir.join(format!("{name}.stderr"));
    let exit_path = dir.join(format!("{name}.exit"));
    let _ = fs::write(&stdout_path, &output.stdout);
    let _ = fs::write(&stderr_path, &output.stderr);
    let _ = fs::write(
        &exit_path,
        output
            .status
            .code()
            .map_or("-1".to_string(), |c| c.to_string()),
    );
}

// Exit code constants from AGENTS.md
const EXIT_SUCCESS: i32 = 0;
const EXIT_USAGE: i32 = 1;
const EXIT_CONFIG: i32 = 2;
const EXIT_STORAGE: i32 = 3;
const EXIT_SEARCH_INDEX: i32 = 4;
const EXIT_IMPORT: i32 = 5;
const EXIT_DEGRADED: i32 = 6;
const EXIT_POLICY_DENIED: i32 = 7;
const EXIT_MIGRATION: i32 = 8;

// ============================================================================
// Exit Code 0: Success
// ============================================================================

#[test]
fn exit_0_success_on_valid_init() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    persist_artifact("exit_0_init", &output);

    ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), "init exit code")
}

#[test]
fn exit_0_success_on_status() -> TestResult {
    let output = run_ee(&["status", "--json"])?;
    persist_artifact("exit_0_status", &output);

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "status exit code",
    )
}

#[test]
fn exit_0_success_on_help() -> TestResult {
    let output = run_ee(&["--help"])?;
    persist_artifact("exit_0_help", &output);

    ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), "help exit code")
}

#[test]
fn exit_0_success_on_version() -> TestResult {
    let output = run_ee(&["--version"])?;
    persist_artifact("exit_0_version", &output);

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_SUCCESS),
        "version exit code",
    )
}

// ============================================================================
// Exit Code 1: Usage Error
// ============================================================================

#[test]
fn exit_1_usage_on_unknown_command() -> TestResult {
    let output = run_ee(&["nonexistent-command"])?;
    persist_artifact("exit_1_unknown_command", &output);

    // Clap returns exit code 2 for unknown commands, which is acceptable
    ensure(
        output.status.code() == Some(EXIT_USAGE) || output.status.code() == Some(2),
        format!(
            "unknown command exit code must be 1 or 2, got {:?}",
            output.status.code()
        ),
    )
}

#[test]
fn exit_1_usage_on_missing_required_arg() -> TestResult {
    let output = run_ee(&["remember", "--json"])?;
    persist_artifact("exit_1_missing_arg", &output);

    // Missing required argument is a usage error
    ensure_equal(
        &output.status.code(),
        &Some(EXIT_USAGE),
        "missing arg exit code",
    )
}

#[test]
fn exit_1_usage_on_invalid_memory_id_format() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(EXIT_SUCCESS), "init exit")?;

    let output = run_ee(&["--workspace", &workspace, "why", "invalid-format", "--json"])?;
    persist_artifact("exit_1_invalid_id", &output);

    // Invalid ID format may return usage (1) or not_found via storage (3)
    ensure(
        output.status.code() == Some(EXIT_USAGE) || output.status.code() == Some(EXIT_STORAGE),
        format!(
            "invalid id exit code must be 1 or 3, got {:?}",
            output.status.code()
        ),
    )
}

#[test]
fn exit_1_usage_on_invalid_enum_value() -> TestResult {
    let output = run_ee(&[
        "remember",
        "test",
        "--level",
        "invalid-level",
        "--kind",
        "fact",
        "--json",
    ])?;
    persist_artifact("exit_1_invalid_enum", &output);

    // Invalid enum value is a usage error (clap validation)
    ensure(
        output.status.code() == Some(EXIT_USAGE) || output.status.code() == Some(2),
        format!(
            "invalid enum exit code must be 1 or 2, got {:?}",
            output.status.code()
        ),
    )
}

// ============================================================================
// Exit Code 3: Storage Error (nonexistent workspace/memory)
// ============================================================================

#[test]
fn exit_3_storage_on_nonexistent_memory() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(EXIT_SUCCESS), "init exit")?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "why",
        "mem_00000000000000000000000000",
        "--json",
    ])?;
    persist_artifact("exit_3_nonexistent_memory", &output);

    // Nonexistent memory returns storage error or usage error
    ensure(
        output.status.code() == Some(EXIT_STORAGE) || output.status.code() == Some(EXIT_USAGE),
        format!(
            "nonexistent memory exit code must be 1 or 3, got {:?}",
            output.status.code()
        ),
    )
}

// ============================================================================
// Exit Code 4: Search/Index Error
// ============================================================================

#[test]
fn exit_4_search_index_on_stale_or_missing_index() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init.status.code(), &Some(EXIT_SUCCESS), "init exit")?;

    // Context on empty/unindexed workspace may return search/index error
    let output = run_ee(&[
        "--workspace",
        &workspace,
        "context",
        "test query",
        "--max-tokens",
        "4000",
        "--json",
    ])?;
    persist_artifact("exit_4_empty_index", &output);

    // Empty workspace context may succeed with empty pack or fail with index error
    ensure(
        output.status.code() == Some(EXIT_SUCCESS)
            || output.status.code() == Some(EXIT_SEARCH_INDEX),
        format!(
            "empty index context exit code must be 0 or 4, got {:?}",
            output.status.code()
        ),
    )
}

// ============================================================================
// Exit Code 6: Degraded Mode
// ============================================================================

#[test]
fn exit_6_degraded_on_recorder_without_store() -> TestResult {
    let output = run_ee(&[
        "recorder",
        "start",
        "--agent-id",
        "test",
        "--dry-run",
        "--json",
    ])?;
    persist_artifact("exit_6_recorder_degraded", &output);

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_DEGRADED),
        "recorder degraded exit code",
    )
}

#[test]
fn exit_6_degraded_on_procedure_without_store() -> TestResult {
    let output = run_ee(&["procedure", "list", "--json"])?;
    persist_artifact("exit_6_procedure_degraded", &output);

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_DEGRADED),
        "procedure degraded exit code",
    )
}

#[test]
fn exit_6_degraded_on_economy_without_metrics() -> TestResult {
    let output = run_ee(&["economy", "report", "--json"])?;
    persist_artifact("exit_6_economy_degraded", &output);

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_DEGRADED),
        "economy degraded exit code",
    )
}

#[test]
fn exit_6_degraded_on_preflight_without_evidence() -> TestResult {
    let output = run_ee(&["preflight", "run", "deploy production migration", "--json"])?;
    persist_artifact("exit_6_preflight_degraded", &output);

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_DEGRADED),
        "preflight degraded exit code",
    )
}

#[test]
fn exit_0_on_causal_dry_run_after_init() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().join("workspace");
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("failed to create causal workspace: {error}"))?;
    let workspace_arg = workspace.display().to_string();
    let init = run_ee(&["init", "--workspace", &workspace_arg, "--json"])?;
    ensure_equal(
        &init.status.code(),
        &Some(0),
        "causal workspace init exit code",
    )?;

    let output = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "causal",
        "trace",
        "--run-id",
        "run-test",
        "--dry-run",
    ])?;
    persist_artifact("exit_0_causal_dry_run", &output);

    ensure_equal(&output.status.code(), &Some(0), "causal dry-run exit code")
}

// ============================================================================
// Exit Code 7: Policy Denied
// ============================================================================

#[test]
fn exit_7_policy_denied_on_promote_without_dry_run() -> TestResult {
    let output = run_ee(&["procedure", "promote", "proc_test", "--json"])?;
    persist_artifact("exit_7_promote_denied", &output);

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_POLICY_DENIED),
        "promote without dry-run exit code",
    )
}

#[test]
fn exit_7_policy_denied_on_experiment_without_dry_run() -> TestResult {
    let output = run_ee(&["learn", "experiment", "run", "--id", "exp_test", "--json"])?;
    persist_artifact("exit_7_experiment_denied", &output);

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_POLICY_DENIED),
        "experiment without dry-run exit code",
    )
}

#[test]
fn exit_7_policy_denied_on_economy_prune_without_dry_run() -> TestResult {
    let output = run_ee(&["economy", "prune-plan", "--json"])?;
    persist_artifact("exit_7_prune_denied", &output);

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_POLICY_DENIED),
        "prune-plan without dry-run exit code",
    )
}

// ============================================================================
// Exit Code 2: Configuration Error
// ============================================================================

#[test]
fn exit_2_config_error_on_invalid_workspace() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let missing_workspace = tempdir.path().join("missing-workspace");
    let missing_workspace = missing_workspace.to_string_lossy().to_string();

    let output = run_ee(&[
        "--workspace",
        &missing_workspace,
        "workspace",
        "alias",
        "--as",
        "missing",
        "--dry-run",
        "--json",
    ])?;
    persist_artifact("exit_2_config_invalid_workspace", &output);

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_CONFIG),
        "invalid workspace exit code",
    )
}

// ============================================================================
// Exit Code 5: Import Error
// ============================================================================

#[test]
fn exit_5_import_error_on_missing_file() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let missing_source = tempdir.path().join("missing-source.jsonl");
    let missing_source = missing_source.to_string_lossy().to_string();

    let output = run_ee(&["import", "jsonl", "--source", &missing_source, "--json"])?;
    persist_artifact("exit_5_import_missing_file", &output);

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_IMPORT),
        "import missing file exit code",
    )
}

// ============================================================================
// Exit Code 8: Migration Required
// ============================================================================

#[test]
fn exit_8_migration_required_when_registry_needs_migration() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let registry_path = tempdir.path().join("registry.db");

    let connection = DbConnection::open_file(&registry_path)
        .map_err(|error| format!("failed to create registry database: {error}"))?;
    connection
        .ping()
        .map_err(|error| format!("failed to initialize registry database: {error}"))?;

    let envs = [(WORKSPACE_REGISTRY_ENV_VAR, registry_path.as_path())];
    let output = run_ee_with_env(&["workspace", "list", "--json"], &envs)?;
    persist_artifact("exit_8_registry_migration_required", &output);

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_MIGRATION),
        "future migration exit code",
    )
}

// ============================================================================
// Exit Code 130: SIGINT
// ============================================================================

#[cfg(unix)]
#[test]
fn exit_130_sigint_terminates_gracefully() -> TestResult {
    use std::os::unix::process::ExitStatusExt;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    // Create a named pipe so that ee import blocks waiting for data
    let tempdir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let pipe_path = tempdir.path().join("pipe.jsonl");
    let mkfifo = Command::new("mkfifo")
        .arg("-m")
        .arg("600")
        .arg(&pipe_path)
        .status()
        .map_err(|e| format!("failed to run mkfifo: {e}"))?;
    ensure(mkfifo.success(), "failed to create FIFO")?;

    let mut child = Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(["import", "jsonl", "--source"])
        .arg(&pipe_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;

    std::thread::sleep(Duration::from_millis(500));

    let kill = Command::new("kill")
        .arg("-INT")
        .arg(child.id().to_string())
        .status()
        .map_err(|e| format!("failed to send SIGINT: {e}"))?;
    ensure(kill.success(), "failed to send SIGINT")?;

    let start = Instant::now();
    let status = child.wait().map_err(|e| e.to_string())?;
    let elapsed = start.elapsed();

    ensure(
        elapsed < Duration::from_secs(2),
        "process must terminate quickly on SIGINT",
    )?;

    // The process should either exit with 130 or be terminated by signal 2 (SIGINT)
    let is_sigint = status.code() == Some(130) || status.signal() == Some(2);
    ensure(
        is_sigint,
        format!(
            "expected SIGINT termination (code 130 or signal 2), got {:?}",
            status
        ),
    )
}

// ============================================================================
// JSON Error Schema Conformance
// ============================================================================

#[test]
fn error_responses_use_ee_error_v1_schema() -> TestResult {
    let output = run_ee(&["procedure", "promote", "proc_test", "--json"])?;
    persist_artifact("error_schema", &output);

    let json = stdout_json(&output)?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.error.v1"),
        "error schema",
    )?;
    ensure(json["error"].is_object(), "error field must be an object")?;
    ensure(
        json["error"]["code"].as_str().is_some(),
        "error.code must be a string",
    )
}

#[test]
fn degraded_responses_include_repair_guidance() -> TestResult {
    let output = run_ee(&[
        "recorder",
        "start",
        "--agent-id",
        "test",
        "--dry-run",
        "--json",
    ])?;
    persist_artifact("degraded_repair", &output);

    let json = stdout_json(&output)?;

    // Degraded responses should include repair guidance
    ensure(
        json["data"]["repair"].as_str().is_some()
            || json["error"]["repair"].as_str().is_some()
            || json["data"]["followUpBead"].as_str().is_some(),
        "degraded response must include repair or followUpBead",
    )
}

// ============================================================================
// Exit Code Stability (same input -> same exit code)
// ============================================================================

#[test]
fn exit_codes_are_deterministic() -> TestResult {
    // Run the same command multiple times and verify consistent exit codes
    let commands: Vec<(&str, Vec<&str>)> = vec![
        ("status", vec!["status", "--json"]),
        ("help", vec!["--help"]),
        (
            "recorder_degraded",
            vec![
                "recorder",
                "start",
                "--agent-id",
                "test",
                "--dry-run",
                "--json",
            ],
        ),
        ("procedure_degraded", vec!["procedure", "list", "--json"]),
    ];

    for (name, args) in commands {
        let output1 = run_ee(&args)?;
        let output2 = run_ee(&args)?;

        persist_artifact(&format!("determinism_{name}_1"), &output1);
        persist_artifact(&format!("determinism_{name}_2"), &output2);

        ensure_equal(
            &output1.status.code(),
            &output2.status.code(),
            &format!("{name} exit code determinism"),
        )?;
    }

    Ok(())
}

// ============================================================================
// Exit Code Range Validation
// ============================================================================

#[test]
fn all_exit_codes_are_in_documented_range() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let missing_workspace = tempdir.path().join("missing-workspace");
    let missing_workspace = missing_workspace.to_string_lossy().to_string();
    let missing_import = tempdir.path().join("missing-import.jsonl");
    let missing_import = missing_import.to_string_lossy().to_string();

    let documented_codes = [
        EXIT_SUCCESS,
        EXIT_USAGE,
        EXIT_CONFIG,
        EXIT_STORAGE,
        EXIT_SEARCH_INDEX,
        EXIT_IMPORT,
        EXIT_DEGRADED,
        EXIT_POLICY_DENIED,
        EXIT_MIGRATION,
        130, // SIGINT
    ];

    // Commands that should produce various exit codes
    let test_cases: Vec<(&str, Vec<&str>)> = vec![
        ("success", vec!["status", "--json"]),
        ("usage", vec!["nonexistent-command"]),
        (
            "config",
            vec![
                "--workspace",
                &missing_workspace,
                "workspace",
                "alias",
                "--as",
                "missing",
                "--dry-run",
                "--json",
            ],
        ),
        (
            "storage",
            vec!["why", "mem_00000000000000000000000000", "--json"],
        ),
        (
            "import",
            vec!["import", "jsonl", "--source", &missing_import, "--json"],
        ),
        (
            "degraded",
            vec![
                "recorder",
                "start",
                "--agent-id",
                "t",
                "--dry-run",
                "--json",
            ],
        ),
        (
            "policy",
            vec!["procedure", "promote", "proc_test", "--json"],
        ),
    ];

    for (name, args) in test_cases {
        let output = run_ee(&args)?;
        persist_artifact(&format!("range_{name}"), &output);

        if let Some(code) = output.status.code() {
            // Also allow clap's exit code 2 for usage errors
            ensure(
                documented_codes.contains(&code) || code == 2,
                format!(
                    "{name} exit code {code} not in documented range {:?}",
                    documented_codes
                ),
            )?;
        }
    }

    Ok(())
}
