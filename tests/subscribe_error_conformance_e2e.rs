//! Conformance coverage for subscribe error envelopes.
//!
//! This uses the real `ee` binary against a real initialized workspace. The
//! fixture first seeds durable memory state and exercises a successful
//! subscribe poll, then drives an invalid subscribe filter and checks that the
//! emitted `ee.error.v2` recovery action shape conforms to the public schema.

use std::process::{Command, Output};

use serde_json::{Value, json};

type TestResult = Result<(), String>;

const EXIT_USAGE: i32 = 1;

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

fn expect_success(output: &Output, context: &str) -> TestResult {
    ensure(
        output.status.success(),
        format!(
            "{context}: expected success\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn stdout_json(output: &Output, context: &str) -> Result<Value, String> {
    let stdout = String::from_utf8(output.stdout.clone())
        .map_err(|error| format!("{context}: stdout was not UTF-8: {error}"))?;
    serde_json::from_str(&stdout)
        .map_err(|error| format!("{context}: stdout was not JSON: {error}\nstdout: {stdout}"))
}

fn log_event(phase: &str, payload: Value) {
    eprintln!(
        "{}",
        json!({
            "schema": "ee.test_event.v1",
            "kind": "subscribe_error_conformance_e2e",
            "phase": phase,
            "payload": payload,
        })
    );
}

fn remember(workspace: &str, content: &str) -> TestResult {
    let output = run_ee(&[
        "--workspace",
        workspace,
        "remember",
        content,
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--tags",
        "subscribe,conformance",
        "--json",
    ])?;
    expect_success(&output, "remember")
}

fn assert_recovery_action_conforms(action: &Value, context: &str) -> TestResult {
    let priority = action
        .get("priority")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{context}: recovery priority must be an integer"))?;
    ensure(
        priority <= u64::from(u8::MAX),
        format!("{context}: recovery priority must fit u8, got {priority}"),
    )?;

    let kind = action
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{context}: recovery kind must be a string"))?;
    const ALLOWED_KINDS: &[&str] = &[
        "env",
        "config",
        "flag",
        "install",
        "rebuild",
        "permission",
        "migration",
        "broaden",
        "narrow",
        "seed",
        "none",
    ];
    ensure(
        ALLOWED_KINDS.contains(&kind),
        format!("{context}: recovery kind {kind:?} is outside ee.error.v2 schema enum"),
    )?;

    let rationale = action
        .get("rationale")
        .and_then(Value::as_str)
        .unwrap_or_default();
    ensure(
        !rationale.trim().is_empty(),
        format!("{context}: recovery rationale is required by ee.error.v2 schema"),
    )
}

#[test]
fn subscribe_filter_error_recovery_matches_error_v2_contract() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = tempdir.path().to_string_lossy().to_string();
    log_event("setup", json!({ "workspace": workspace }));

    let init = run_ee(&["--workspace", &workspace, "init", "--json"])?;
    expect_success(&init, "init")?;
    remember(
        &workspace,
        "Subscribe error conformance fixture should create a real audit delta.",
    )?;

    let poll = run_ee(&[
        "--workspace",
        &workspace,
        "subscribe",
        "poll",
        "--cursor",
        "0",
        "--filter",
        "LEVEL=procedural,TAG=conformance",
        "--json",
    ])?;
    expect_success(&poll, "subscribe poll")?;
    let poll_json = stdout_json(&poll, "subscribe poll")?;
    ensure(
        poll_json.pointer("/schema").and_then(Value::as_str) == Some("ee.response.v1"),
        "successful subscribe poll should use the normal response envelope",
    )?;
    ensure(
        poll_json
            .pointer("/data/deltas")
            .and_then(Value::as_array)
            .is_some_and(|deltas| !deltas.is_empty()),
        "fixture should produce at least one real durable memory delta",
    )?;
    log_event(
        "exercise",
        json!({
            "successfulDeltaCount": poll_json.pointer("/data/deltaCount"),
        }),
    );

    let invalid = run_ee(&[
        "--workspace",
        &workspace,
        "subscribe",
        "poll",
        "--cursor",
        "0",
        "--filter",
        "TAG_MODE=sideways",
        "--json",
    ])?;
    ensure(
        invalid.status.code() == Some(EXIT_USAGE),
        format!(
            "invalid subscribe filter should exit with usage code {EXIT_USAGE}, got {:?}",
            invalid.status.code()
        ),
    )?;
    ensure(
        String::from_utf8_lossy(&invalid.stderr).trim().is_empty(),
        "JSON error response should be emitted on stdout without stderr noise",
    )?;
    let error_json = stdout_json(&invalid, "invalid subscribe filter")?;
    log_event(
        "assert",
        json!({
            "schema": error_json.pointer("/schema"),
            "code": error_json.pointer("/error/code"),
            "recovery": error_json.pointer("/error/details/recovery"),
        }),
    );

    ensure(
        error_json.pointer("/schema").and_then(Value::as_str) == Some("ee.error.v2"),
        "invalid subscribe filter must emit ee.error.v2",
    )?;
    ensure(
        error_json.pointer("/success").is_none(),
        "ee.error.v2 must not include the success flag from response envelopes",
    )?;
    ensure(
        error_json.pointer("/error/code").and_then(Value::as_str)
            == Some("subscribe_filter_invalid"),
        "invalid subscribe filter should use subscribe_filter_invalid code",
    )?;
    let recovery = error_json
        .pointer("/error/details/recovery")
        .and_then(Value::as_array)
        .ok_or_else(|| "error.details.recovery must be an array".to_string())?;
    ensure(
        !recovery.is_empty(),
        "subscribe_filter_invalid should include at least one recovery action",
    )?;
    for (index, action) in recovery.iter().enumerate() {
        assert_recovery_action_conforms(action, &format!("recovery[{index}]"))?;
    }

    Ok(())
}
