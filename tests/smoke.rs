use std::fmt::Debug;
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
