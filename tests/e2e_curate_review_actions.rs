//! bd-cilvv: real-binary pin test for `ee curate accept`/`reject`/
//! `snooze`/`apply` unique error paths.
//!
//! Three distinct validators across the review-action family had no
//! end-to-end coverage. They join the broader curate-family pin
//! tests (bd-29acc candidates, bd-1gka6 validate, bd-kxm0c
//! propose-derived) so the whole queue surface is locked:
//!
//! * `parse_snoozed_until` is unique to snooze. Pinned shape:
//!   `--until garbage_timestamp` -> Usage `"invalid --until
//!   timestamp"` + repair `"ee curate snooze <candidate-id>
//!   --until <RFC3339> --json"`
//! * `validate_curate_review_reason` is shared across review
//!   actions but its 4 KiB cap had no end-to-end check.
//!   Pinned shape: `accept <id> --reason <5KB-text>` -> Usage
//!   `"curate review --reason must be <= 4096 bytes"` + repair
//!   `"Store long rationale in an external note and pass a short
//!   reason pointer."`
//! * `validate_curate_candidate_id` is reused by apply but bd-1gka6
//!   only pinned it via validate. Pinned shape: `apply bad-id` ->
//!   Usage `"invalid curation candidate ID"` + repair
//!   `"ee curate candidates --json"`
//! * NotFound on a valid-format but missing candidate via reject.
//!   Pinned shape: `reject <valid-format-but-missing>` -> NotFound
//!   `"curation candidate"` + `"ee curate candidates --json"`
//! * NotFound on a valid-format but missing candidate via retire
//!   dry-run. Pinned shape: `retire <valid-format-but-missing>
//!   --dry-run` -> NotFound before any retirement plan is reported.

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
        .join("ee-curate-review-actions-pin")
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

fn run_curate(workspace_arg: &str, action_args: &[&str]) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec!["--workspace", workspace_arg, "--json", "curate"];
    args.extend_from_slice(action_args);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("curate stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn assert_error_with_repair(
    parsed: &Value,
    message_needles: &[&str],
    repair_needles: &[&str],
) -> TestResult {
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    for needle in message_needles {
        ensure(
            message.contains(needle),
            format!("message must contain {needle:?}; got {message}"),
        )?;
    }
    let repair = error["repair"].as_str().unwrap_or_default();
    for needle in repair_needles {
        ensure(
            repair.contains(needle),
            format!("repair must contain {needle:?}; got {repair}"),
        )?;
    }
    Ok(())
}

#[test]
fn curate_snooze_rejects_invalid_until_timestamp_with_usage_repair() -> TestResult {
    let workspace = unique_workspace("snooze-bad-until")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    // Valid candidate id format so we reach parse_snoozed_until
    // before any DB lookup. The actual candidate doesn't need to
    // exist — the snoozed_until parser runs first.
    let valid_id = "curate_00000000000000000000000123";
    let (output, parsed) = run_curate(
        &workspace_arg,
        &["snooze", valid_id, "--until", "garbage_timestamp"],
    )?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate snooze --until garbage_timestamp must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &parsed,
        &["invalid --until timestamp"],
        &["ee curate snooze", "<RFC3339>"],
    )
}

#[test]
fn curate_accept_rejects_reason_over_4kib_with_size_repair() -> TestResult {
    let workspace = unique_workspace("accept-reason-too-long")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    // validate_curate_review_reason enforces a 4 KiB cap. Generate
    // a 5 KiB rationale to trip the guard. The candidate id is in
    // valid format so the reason validator runs before the lookup.
    let valid_id = "curate_00000000000000000000000124";
    let big_reason = "x".repeat(5 * 1024);
    let (output, parsed) = run_curate(
        &workspace_arg,
        &["accept", valid_id, "--reason", big_reason.as_str()],
    )?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate accept --reason <5KB> must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &parsed,
        &["curate review --reason must be <= 4096 bytes"],
        &["Store long rationale in an external note"],
    )
}

#[test]
fn curate_apply_rejects_invalid_candidate_id_format_with_usage_repair() -> TestResult {
    let workspace = unique_workspace("apply-bad-id")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_curate(&workspace_arg, &["apply", "bad-id"])?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate apply bad-id must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &parsed,
        &["invalid curation candidate ID", "bad-id"],
        &["ee curate candidates --json"],
    )
}

#[test]
fn curate_reject_returns_not_found_for_valid_format_but_missing_candidate() -> TestResult {
    let workspace = unique_workspace("reject-not-found")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let valid_id = "curate_00000000000000000000000077";
    let (output, parsed) = run_curate(&workspace_arg, &["reject", valid_id])?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate reject on unknown candidate must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    let error_id = error["id"].as_str().unwrap_or_default();
    ensure(
        message.contains("curation candidate") || error_id == valid_id,
        format!(
            "not-found error must reference curation candidate; got message={message}, id={error_id}"
        ),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee curate candidates --json"),
        format!("not-found repair must point at `ee curate candidates --json`; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn curate_retire_dry_run_returns_not_found_for_valid_format_but_missing_candidate() -> TestResult {
    let workspace = unique_workspace("retire-dry-run-not-found")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let valid_id = "curate_00000000000000000000000999";
    let (output, parsed) = run_curate(&workspace_arg, &["retire", valid_id, "--dry-run"])?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate retire --dry-run on unknown candidate must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let code = error["code"].as_str().unwrap_or_default();
    ensure(
        code == "not_found",
        format!("retire dry-run must return not_found for missing candidate; got {code}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    let error_id = error["id"].as_str().unwrap_or_default();
    ensure(
        message.contains("curation candidate") || error_id == valid_id,
        format!(
            "not-found error must reference curation candidate; got message={message}, id={error_id}"
        ),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee curate candidates --json"),
        format!("not-found repair must point at `ee curate candidates --json`; got {repair}"),
    )?;
    Ok(())
}
