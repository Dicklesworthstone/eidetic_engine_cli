use std::fmt::Debug;
use std::fs;
use std::process::{Command, Output};

type TestResult = Result<(), String>;

const DEMO_ID: &str = "demo_00000000000000000000000001";
const SECRET_VALUE: &str = "sk_live_test";
const REDACTED_VALUE: &str = "[REDACTED]";

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

fn stdout_text(output: &Output, context: &str) -> Result<String, String> {
    String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{context}: stdout was not UTF-8: {error}"))
}

fn stdout_json(output: &Output, context: &str) -> Result<serde_json::Value, String> {
    let stdout = stdout_text(output, context)?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{context}: stdout was not JSON: {error}\nstdout: {stdout}"))
}

fn assert_secret_absent_and_key_visible(output: &Output, context: &str) -> TestResult {
    ensure_equal(&output.status.code(), &Some(0), context)?;
    ensure(
        output.stderr.is_empty(),
        format!("{context}: JSON mode stderr should be empty"),
    )?;
    let stdout = stdout_text(output, context)?;
    ensure(
        !stdout.contains(SECRET_VALUE),
        format!("{context}: stdout leaked secret env override value"),
    )?;
    ensure(
        stdout.contains("\"API_KEY\""),
        format!("{context}: stdout should preserve env override key"),
    )?;
    ensure(
        stdout.contains(REDACTED_VALUE),
        format!("{context}: stdout should contain redaction marker"),
    )?;
    Ok(())
}

#[test]
fn demo_json_redacts_env_override_values() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path();
    fs::write(
        workspace.join("demo.yaml"),
        format!(
            "\
schema: ee.demo_file.v1
version: 1
demos:
  - id: {DEMO_ID}
    title: env redaction regression
    description: redacts env override values in JSON output
    env_overrides:
      API_KEY: {SECRET_VALUE}
    commands:
      - command: \"ee status --json\"
        expected_exit_code: 0
"
        ),
    )
    .map_err(|error| format!("failed to write demo.yaml: {error}"))?;
    let workspace_arg = workspace.display().to_string();

    let list = run_ee(&["--workspace", &workspace_arg, "--json", "demo", "list"])?;
    assert_secret_absent_and_key_visible(&list, "demo list")?;
    let list_json = stdout_json(&list, "demo list")?;
    ensure_equal(
        &list_json["data"]["demos"][0]["envOverrides"]["API_KEY"],
        &serde_json::json!(REDACTED_VALUE),
        "demo list env override redaction",
    )?;

    let dry_run = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--json",
        "demo",
        "run",
        DEMO_ID,
        "--dry-run",
    ])?;
    assert_secret_absent_and_key_visible(&dry_run, "demo run dry-run")?;
    let dry_run_json = stdout_json(&dry_run, "demo run dry-run")?;
    ensure_equal(
        &dry_run_json["data"]["demos"][0]["envOverrides"]["API_KEY"],
        &serde_json::json!(REDACTED_VALUE),
        "demo run dry-run env override redaction",
    )
}
