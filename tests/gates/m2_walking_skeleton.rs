use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fs, str};

use serde_json::Value as JsonValue;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T>(actual: T, expected: T, context: &str) -> TestResult
where
    T: std::fmt::Debug + PartialEq,
{
    ensure(
        actual == expected,
        format!("{context}: expected {expected:?}, got {actual:?}"),
    )
}

fn run_ee(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_ee"))
        .args(args)
        .output()
        .map_err(|error| format!("failed to run ee {}: {error}", args.join(" ")))
}

fn unique_workspace(prefix: &str) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("clock moved backwards: {error}"))?
        .as_nanos();
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-gate-artifacts")
        .join(format!("{prefix}-{}-{now}", std::process::id())))
}

fn assert_no_ansi(output: &str, context: &str) -> TestResult {
    ensure(
        !output.contains("\u{1b}["),
        format!("{context} must not contain terminal styling"),
    )
}

fn assert_machine_json_response(output: &Output, context: &str) -> Result<JsonValue, String> {
    let stdout = str::from_utf8(&output.stdout)
        .map_err(|error| format!("{context} stdout must be UTF-8: {error}"))?;
    let stderr = str::from_utf8(&output.stderr)
        .map_err(|error| format!("{context} stderr must be UTF-8: {error}"))?;

    ensure(
        output.status.success(),
        format!("{context} should succeed; stderr: {stderr}"),
    )?;
    ensure(
        stderr.is_empty(),
        format!("{context} diagnostics must stay out of stderr on success, got: {stderr:?}"),
    )?;
    ensure(
        stdout.starts_with('{') && stdout.ends_with('\n'),
        format!("{context} stdout must be newline-terminated JSON data, got: {stdout:?}"),
    )?;
    assert_no_ansi(stdout, context)?;

    let json: JsonValue = serde_json::from_str(stdout)
        .map_err(|error| format!("{context} stdout must parse as JSON: {error}"))?;
    ensure_equal(
        json.get("schema"),
        Some(&JsonValue::String("ee.response.v1".to_owned())),
        context,
    )?;
    ensure_equal(json.get("success"), Some(&JsonValue::Bool(true)), context)?;
    ensure(
        json.get("data").is_some_and(JsonValue::is_object),
        format!("{context} response must include a data object"),
    )?;

    Ok(json)
}

#[test]
fn m2_walking_skeleton_keeps_public_json_and_markdown_contracts() -> TestResult {
    let workspace = unique_workspace("m2-walking-skeleton")?;
    fs::create_dir_all(&workspace)
        .map_err(|error| format!("failed to create {}: {error}", workspace.display()))?;
    let workspace_arg = workspace_to_arg(&workspace)?;

    let init = assert_machine_json_response(
        &run_ee(&["--workspace", &workspace_arg, "--json", "init"])?,
        "ee init --json",
    )?;
    ensure_equal(
        init.pointer("/data/command"),
        Some(&JsonValue::String("init".to_owned())),
        "init command field",
    )?;

    let remember = assert_machine_json_response(
        &run_ee(&[
            "--workspace",
            &workspace_arg,
            "--json",
            "remember",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--tags",
            "release,format",
            "--confidence",
            "0.95",
            "Run cargo fmt --check before release.",
        ])?,
        "ee remember --json",
    )?;
    ensure_equal(
        remember.pointer("/data/command"),
        Some(&JsonValue::String("remember".to_owned())),
        "remember command field",
    )?;
    let memory_id = remember
        .pointer("/data/memory_id")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| "remember response must include data.memory_id".to_owned())?
        .to_owned();

    let rebuild = assert_machine_json_response(
        &run_ee(&["--workspace", &workspace_arg, "--json", "index", "rebuild"])?,
        "ee index rebuild --json",
    )?;
    ensure_equal(
        rebuild.pointer("/data/command"),
        Some(&JsonValue::String("index_rebuild".to_owned())),
        "index rebuild command field",
    )?;

    let search = assert_machine_json_response(
        &run_ee(&[
            "--workspace",
            &workspace_arg,
            "--json",
            "search",
            "format before release",
        ])?,
        "ee search --json",
    )?;
    ensure(
        search
            .pointer("/data/results")
            .and_then(JsonValue::as_array)
            .is_some_and(|results| !results.is_empty()),
        "search response must include at least one result",
    )?;

    let context = assert_machine_json_response(
        &run_ee(&[
            "--workspace",
            &workspace_arg,
            "--json",
            "context",
            "prepare release",
        ])?,
        "ee context --json",
    )?;
    ensure(
        context.pointer("/data/provenance").is_some()
            || context.pointer("/data/cards").is_some()
            || context.pointer("/data/pack").is_some(),
        "context response must expose selected context material or provenance",
    )?;

    let markdown = run_ee(&[
        "--workspace",
        &workspace_arg,
        "--format",
        "markdown",
        "context",
        "prepare release",
    ])?;
    let markdown_stdout = str::from_utf8(&markdown.stdout)
        .map_err(|error| format!("context markdown stdout must be UTF-8: {error}"))?;
    let markdown_stderr = str::from_utf8(&markdown.stderr)
        .map_err(|error| format!("context markdown stderr must be UTF-8: {error}"))?;
    ensure(
        markdown.status.success(),
        format!("ee context --format markdown should succeed; stderr: {markdown_stderr}"),
    )?;
    ensure(
        markdown_stderr.is_empty(),
        format!("context markdown diagnostics must stay out of stderr, got: {markdown_stderr:?}"),
    )?;
    ensure(
        markdown_stdout.contains("Run cargo fmt --check before release."),
        "context markdown must include remembered release guidance",
    )?;
    assert_no_ansi(markdown_stdout, "context markdown stdout")?;

    let why = assert_machine_json_response(
        &run_ee(&["--workspace", &workspace_arg, "--json", "why", &memory_id])?,
        "ee why --json",
    )?;
    ensure(
        why.pointer("/data/storage").is_some() || why.pointer("/data/memory").is_some(),
        "why response must explain persisted memory state",
    )?;

    Ok(())
}

fn workspace_to_arg(workspace: &Path) -> Result<String, String> {
    workspace
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("workspace path is not UTF-8: {}", workspace.display()))
}
