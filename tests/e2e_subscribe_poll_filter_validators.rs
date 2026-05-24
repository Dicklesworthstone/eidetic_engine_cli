//! bd-13iqb: real-binary pin test for `ee subscribe poll --filter`
//! validator error branches.
//!
//! `parse_subscribe_filter` (src/core/subscribe.rs:263) is shared by
//! `ee subscribe poll` and `ee subscribe stream` and emits five distinct
//! `DomainError::Usage` messages for malformed `--filter` tokens. The
//! recent `tests/subscribe_e2e.rs` and
//! `tests/e2e_subscribe_stream_renderer.rs` (bd-t7k5u) cover the happy
//! path and the renderer-requirement validator, but the filter parser
//! error surface has no real-binary assertions.
//!
//! This pin-test mirrors the
//! `tests/e2e_insights_json_stream_mutex.rs` harness shape and pins four
//! representative filter parse failures via `ee subscribe poll`:
//!
//! * `--filter broken_token_no_equals` -> invalid-token message + KEY=value
//!   repair.
//! * `--filter LEVEL=` -> empty-value message + non-empty-value repair.
//! * `--filter LEVEL=garbage_level` -> invalid-memory-level message +
//!   level repair listing canonical values.
//! * `--filter TAG_MODE=bogus` -> invalid-tag-mode message + tag-mode
//!   repair listing all/any.

#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
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
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ee-subscribe-poll-filter-validators-pin")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

fn init_workspace(workspace_arg: &str) -> TestResult {
    let init = run_ee(&["--workspace", workspace_arg, "--json", "init"])?;
    ensure(
        init.status.success(),
        format!(
            "ee init must succeed; stderr: {}",
            String::from_utf8_lossy(&init.stderr)
        ),
    )
}

fn run_poll_with_filter(workspace_arg: &str, filter: &str) -> Result<(Output, Value), String> {
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "subscribe",
        "poll",
        "--filter",
        filter,
    ])?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("subscribe poll stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn assert_usage_error(
    output: &Output,
    parsed: &Value,
    filter_label: &str,
    expected_message_fragment: &str,
    expected_repair_fragment: &str,
) -> TestResult {
    ensure(
        !output.status.success(),
        format!(
            "ee subscribe poll --filter {filter_label} must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    ensure(
        message.contains(expected_message_fragment),
        format!(
            "usage message must contain {expected_message_fragment:?} for {filter_label}; got {message}"
        ),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains(expected_repair_fragment),
        format!(
            "usage repair must contain {expected_repair_fragment:?} for {filter_label}; got {repair}"
        ),
    )?;
    Ok(())
}

#[test]
fn subscribe_poll_filter_token_without_equals_is_invalid_token_usage_error() -> TestResult {
    let workspace = unique_workspace("no-equals")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_poll_with_filter(&workspace_arg, "broken_token_no_equals")?;
    assert_usage_error(
        &output,
        &parsed,
        "broken_token_no_equals",
        "Invalid subscribe filter token `broken_token_no_equals`.",
        "Use KEY=value tokens",
    )
}

#[test]
fn subscribe_poll_filter_empty_value_is_usage_error_with_empty_value_message() -> TestResult {
    let workspace = unique_workspace("empty-value")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_poll_with_filter(&workspace_arg, "LEVEL=")?;
    assert_usage_error(
        &output,
        &parsed,
        "LEVEL=",
        "Subscribe filter `level` has an empty value.",
        "Provide a non-empty filter value.",
    )
}

#[test]
fn subscribe_poll_filter_unknown_level_is_usage_error_listing_canonical_levels() -> TestResult {
    let workspace = unique_workspace("bad-level")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_poll_with_filter(&workspace_arg, "LEVEL=garbage_level")?;
    assert_usage_error(
        &output,
        &parsed,
        "LEVEL=garbage_level",
        "Invalid memory level `garbage_level`",
        "Use working, episodic, semantic, or procedural.",
    )
}

#[test]
fn subscribe_poll_filter_unknown_tag_mode_is_usage_error_listing_all_or_any() -> TestResult {
    let workspace = unique_workspace("bad-tag-mode")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_poll_with_filter(&workspace_arg, "TAG_MODE=bogus")?;
    assert_usage_error(
        &output,
        &parsed,
        "TAG_MODE=bogus",
        "Invalid tag mode `bogus`.",
        "Use TAG_MODE=all or TAG_MODE=any.",
    )
}
