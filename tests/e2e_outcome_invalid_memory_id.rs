//! eidetic_engine_cli-jp7a: invalid outcome target IDs return an error envelope.
//!
//! This is a real-binary E2E check for the public `ee outcome` surface. It
//! proves a nonexistent memory ID cannot be reported as helpful and that the
//! failure stays machine-readable on stdout.

use std::fmt::Debug;
use std::process::{Command, Output};

type TestResult = Result<(), String>;

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
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
    serde_json::from_str(&stdout)
        .map_err(|error| format!("stdout was not JSON: {error}\nstdout: {stdout}"))
}

#[test]
fn outcome_nonexistent_memory_id_returns_error_envelope() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init_output = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    ensure_equal(&init_output.status.code(), &Some(0), "init exit code")?;

    let output = run_ee(&[
        "--workspace",
        &workspace,
        "outcome",
        "mem_00000000000000000000000000",
        "--target-type",
        "memory",
        "--signal",
        "helpful",
        "--json",
    ])?;

    ensure(
        output.status.code() != Some(0),
        "nonexistent memory outcome should fail",
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "JSON outcome errors should keep stderr clean, got: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;

    let json = stdout_json(&output)?;
    ensure_equal(
        &json["schema"],
        &serde_json::json!("ee.error.v1"),
        "error schema",
    )?;
    ensure_equal(
        &json["error"]["code"],
        &serde_json::json!("not_found"),
        "error code",
    )?;
    ensure(
        json["error"]["repair"].as_str().is_some_and(|repair| {
            repair.contains("memory") || repair.contains("outcome") || repair.contains("id")
        }),
        format!(
            "not_found error should include a useful repair hint, got: {:?}",
            json["error"]["repair"]
        ),
    )
}
