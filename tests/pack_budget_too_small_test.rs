//! Real-binary coverage for the `pack_budget_too_small` degradation.
//!
//! This complements the inline pack-assembly unit tests by exercising the
//! public `ee context --json` response shape against an isolated workspace.

use std::fmt::Debug;
use std::process::{Command, Output};

type TestResult = Result<(), String>;

const EXIT_SUCCESS: i32 = 0;
const PACK_BUDGET_TOO_SMALL: &str = "pack_budget_too_small";
const NO_RELEVANT_RESULTS: &str = "no_relevant_results";

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

fn assert_success(output: &Output, context: &str) -> TestResult {
    ensure_equal(&output.status.code(), &Some(EXIT_SUCCESS), context)
}

fn assert_stderr_empty(output: &Output, context: &str) -> TestResult {
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure(
        stderr.trim().is_empty(),
        format!("{context}: stderr should be empty in JSON mode, got: {stderr}"),
    )
}

fn context_json(
    workspace: &str,
    query: &str,
    max_tokens: &str,
) -> Result<serde_json::Value, String> {
    let output = run_ee(&[
        "--workspace",
        workspace,
        "context",
        query,
        "--max-tokens",
        max_tokens,
        "--json",
    ])?;
    assert_success(&output, &format!("context {max_tokens}"))?;
    assert_stderr_empty(&output, &format!("context {max_tokens}"))?;
    stdout_json(&output)
}

fn degraded_entries(value: &serde_json::Value) -> Result<&[serde_json::Value], String> {
    value
        .pointer("/data/degraded")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| "context response data.degraded must be an array".to_string())
}

fn degraded_codes(value: &serde_json::Value) -> Result<Vec<&str>, String> {
    Ok(degraded_entries(value)?
        .iter()
        .filter_map(|entry| entry.get("code").and_then(serde_json::Value::as_str))
        .collect())
}

fn find_degradation<'a>(
    value: &'a serde_json::Value,
    code: &str,
) -> Result<&'a serde_json::Value, String> {
    degraded_entries(value)?
        .iter()
        .find(|entry| entry.get("code").and_then(serde_json::Value::as_str) == Some(code))
        .ok_or_else(|| format!("missing degradation {code} in {value:?}"))
}

fn pack_items(value: &serde_json::Value) -> Result<&[serde_json::Value], String> {
    value
        .pointer("/data/pack/items")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| "context response data.pack.items must be an array".to_string())
}

fn setup_release_workspace() -> Result<(tempfile::TempDir, String), String> {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    assert_success(&init, "init")?;
    assert_stderr_empty(&init, "init")?;

    for index in 1..=10 {
        let content = format!(
            "Release ritual memory {index}: before publishing, preserve provenance, inspect \
             structured degraded recovery actions, run remote-only RCH verification, keep stdout \
             machine-readable, and cite remediation beads when preflight blocks remote proof."
        );
        let remember = run_ee(&[
            "--workspace",
            &workspace,
            "remember",
            &content,
            "--level",
            "semantic",
            "--kind",
            "fact",
            "--json",
        ])?;
        assert_success(&remember, &format!("remember {index}"))?;
        assert_stderr_empty(&remember, &format!("remember {index}"))?;
    }

    Ok((tempdir, workspace))
}

#[test]
fn tight_budget_against_real_corpus_emits_code() -> TestResult {
    let (_tempdir, workspace) = setup_release_workspace()?;

    let value = context_json(&workspace, "release ritual", "1")?;
    let codes = degraded_codes(&value)?;

    ensure(
        codes.contains(&PACK_BUDGET_TOO_SMALL),
        format!("tight budget should emit {PACK_BUDGET_TOO_SMALL}: {codes:?}"),
    )?;
    ensure_equal(&pack_items(&value)?.len(), &0, "tight budget item count")
}

#[test]
fn wide_budget_does_not_emit_pack_budget_too_small() -> TestResult {
    let (_tempdir, workspace) = setup_release_workspace()?;

    let value = context_json(&workspace, "release ritual", "4000")?;
    let codes = degraded_codes(&value)?;

    ensure(
        !codes.contains(&PACK_BUDGET_TOO_SMALL),
        format!("wide budget should not emit {PACK_BUDGET_TOO_SMALL}: {codes:?}"),
    )?;
    ensure(
        !pack_items(&value)?.is_empty(),
        "wide budget should select at least one item",
    )
}

#[test]
fn recovery_actions_machine_readable() -> TestResult {
    let (_tempdir, workspace) = setup_release_workspace()?;

    let value = context_json(&workspace, "release ritual", "1")?;
    let degradation = find_degradation(&value, PACK_BUDGET_TOO_SMALL)?;
    ensure_equal(
        &degradation
            .get("severity")
            .and_then(serde_json::Value::as_str),
        &Some("warning"),
        "pack_budget_too_small severity",
    )?;

    let recovery = degradation
        .pointer("/details/recovery")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "details.recovery must be an array".to_string())?;
    ensure_equal(&recovery.len(), &3, "recovery action count")?;
    ensure_equal(
        &recovery[0].get("kind").and_then(serde_json::Value::as_str),
        &Some("flag"),
        "recovery[0] kind",
    )?;
    ensure_equal(
        &recovery[0]
            .get("flagName")
            .and_then(serde_json::Value::as_str),
        &Some("--max-tokens"),
        "recovery[0] flag",
    )?;
    ensure_equal(
        &recovery[1]
            .get("flagName")
            .and_then(serde_json::Value::as_str),
        &Some("--profile"),
        "recovery[1] flag",
    )?;
    ensure_equal(
        &recovery[2].get("kind").and_then(serde_json::Value::as_str),
        &Some("broaden"),
        "recovery[2] kind",
    )
}

#[test]
fn no_relevant_results_and_pack_budget_too_small_are_mutually_exclusive() -> TestResult {
    let (_tempdir, workspace) = setup_release_workspace()?;

    let value = context_json(&workspace, "xyz nonexistent term abc123", "1")?;
    let codes = degraded_codes(&value)?;

    ensure(
        codes.contains(&NO_RELEVANT_RESULTS),
        format!("empty pool should emit {NO_RELEVANT_RESULTS}: {codes:?}"),
    )?;
    ensure(
        !codes.contains(&PACK_BUDGET_TOO_SMALL),
        format!("empty pool should not emit {PACK_BUDGET_TOO_SMALL}: {codes:?}"),
    )
}
