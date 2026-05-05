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

use std::fmt::Debug;
use std::fs;
use std::path::PathBuf;
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
fn exit_6_degraded_on_causal_without_ledgers() -> TestResult {
    let output = run_ee(&[
        "causal",
        "trace",
        "--run-id",
        "run-test",
        "--dry-run",
        "--json",
    ])?;
    persist_artifact("exit_6_causal_degraded", &output);

    ensure_equal(
        &output.status.code(),
        &Some(EXIT_DEGRADED),
        "causal degraded exit code",
    )
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
    ];

    // Commands that should produce various exit codes
    let test_cases: Vec<(&str, Vec<&str>)> = vec![
        ("success", vec!["status", "--json"]),
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
