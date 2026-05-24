//! bd-wdrcn: real-binary pin test for `ee memory expire` behaviors.
//!
//! `ee memory expire` is the tombstone-by-validity operation that
//! several earlier pin tests (bd-6trv0 memory-link semantic,
//! bd-3jvp0 memory-revise) rely on for setup — but no test pins the
//! expire command itself end-to-end. `expire_memory` in
//! src/core/memory.rs:4533 has four distinct status/idempotency
//! branches plus the standard database-existence and not-found
//! guards. This pin test locks all of them so a reword cannot
//! silently break downstream agents that read the result.
//!
//! Pins:
//! * Missing database -> Storage repair `"ee init --workspace ."`
//! * Non-existent memory id -> NotFound `"memory"` + `"ee memory
//!   list"`
//! * `--dry-run` preview on an active memory -> data.status=
//!   `would_expire`, dry_run=true, persisted=false, changed=true,
//!   idempotency=`would_change` AND no-mutation contract (a
//!   subsequent expire without --dry-run still expires the memory
//!   for real)
//! * Real expire -> data.status=`expired`, persisted=true,
//!   changed=true, idempotency=`changed`, audit_id present
//! * Re-expire an already-expired memory -> data.status=
//!   `already_expired`, persisted=false, changed=false, idempotency=
//!   `no_change` (idempotent replay contract)

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
        .join("ee-memory-expire-pin")
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

fn remember(workspace_arg: &str, content: &str) -> Result<String, String> {
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "remember",
        "--level",
        "semantic",
        "--kind",
        "fact",
        content,
    ])?;
    if !output.status.success() {
        return Err(format!(
            "remember failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let parsed: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    parsed["data"]["public_id"]
        .as_str()
        .or_else(|| parsed["data"]["memory_id"].as_str())
        .or_else(|| parsed["data"]["id"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "remember response missing memory id: {}",
                serde_json::to_string(&parsed).unwrap_or_default()
            )
        })
}

fn run_expire(
    workspace_arg: &str,
    memory_id: &str,
    extra: &[&str],
) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec!["--workspace", workspace_arg, "--json", "memory", "expire"];
    args.push(memory_id);
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("memory expire stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn tombstone_memory(workspace_arg: &str, memory_id: &str) -> TestResult {
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "curate",
        "tombstone",
        memory_id,
    ])?;
    ensure(
        output.status.success(),
        format!(
            "ee curate tombstone {memory_id} must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )
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
fn memory_expire_surfaces_storage_error_when_database_missing() -> TestResult {
    let workspace = unique_workspace("no-db")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();

    let (output, parsed) = run_expire(&workspace_arg, "mem_any", &[])?;
    ensure(
        !output.status.success(),
        format!(
            "ee memory expire without ee init must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &parsed,
        &["Database not found at"],
        &["ee init --workspace ."],
    )
}

#[test]
fn memory_expire_returns_not_found_for_unknown_memory_id() -> TestResult {
    let workspace = unique_workspace("not-found")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_expire(&workspace_arg, "mem_does_not_exist_in_workspace", &[])?;
    ensure(
        !output.status.success(),
        format!(
            "ee memory expire on unknown memory must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee memory list"),
        format!("not-found repair must point at `ee memory list`; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn memory_expire_dry_run_emits_would_expire_preview_without_mutating() -> TestResult {
    let workspace = unique_workspace("dry-run")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(&workspace_arg, "Pin-test memory-expire dry-run target.")?;

    let (dry_output, dry_parsed) = run_expire(&workspace_arg, &memory_id, &["--dry-run"])?;
    ensure(
        dry_output.status.success(),
        format!(
            "ee memory expire --dry-run must succeed; stderr: {}",
            String::from_utf8_lossy(&dry_output.stderr)
        ),
    )?;
    let dry_data = &dry_parsed["data"];
    ensure(
        dry_data["command"].as_str() == Some("memory expire"),
        format!("dry-run data.command must be `memory expire`; got {dry_data}"),
    )?;
    ensure(
        dry_data["status"].as_str() == Some("would_expire"),
        format!("dry-run status must be `would_expire`; got {dry_data}"),
    )?;
    ensure(
        dry_data["dry_run"] == Value::Bool(true),
        format!("dry-run dry_run must be true; got {dry_data}"),
    )?;
    ensure(
        dry_data["persisted"] == Value::Bool(false),
        format!("dry-run persisted must be false; got {dry_data}"),
    )?;
    ensure(
        dry_data["changed"] == Value::Bool(true),
        format!("dry-run changed must be true (preview is non-trivial); got {dry_data}"),
    )?;
    ensure(
        dry_data["idempotency"].as_str() == Some("would_change"),
        format!("dry-run idempotency must be `would_change`; got {dry_data}"),
    )?;

    // No-mutation contract: a real expire after the dry-run must
    // still expire the memory (status=expired, not already_expired).
    let (real_output, real_parsed) = run_expire(&workspace_arg, &memory_id, &[])?;
    ensure(
        real_output.status.success(),
        format!(
            "follow-up real expire must succeed; stderr: {}",
            String::from_utf8_lossy(&real_output.stderr)
        ),
    )?;
    let real_data = &real_parsed["data"];
    ensure(
        real_data["status"].as_str() == Some("expired"),
        format!(
            "real expire after dry-run must yield status=expired (proving dry-run did not mutate); got {real_data}"
        ),
    )?;
    Ok(())
}

#[test]
fn memory_expire_happy_path_returns_expired_envelope_with_audit() -> TestResult {
    let workspace = unique_workspace("happy")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(&workspace_arg, "Pin-test memory-expire happy-path target.")?;

    let (output, parsed) = run_expire(&workspace_arg, &memory_id, &[])?;
    ensure(
        output.status.success(),
        format!(
            "ee memory expire on active memory must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        parsed["schema"].as_str() == Some("ee.response.v2"),
        format!("envelope schema must be ee.response.v2; got {parsed}"),
    )?;
    let data = &parsed["data"];
    ensure(
        data["command"].as_str() == Some("memory expire"),
        format!("data.command must be `memory expire`; got {data}"),
    )?;
    ensure(
        data["status"].as_str() == Some("expired"),
        format!("happy-path status must be `expired`; got {data}"),
    )?;
    ensure(
        data["persisted"] == Value::Bool(true),
        format!("happy-path persisted must be true; got {data}"),
    )?;
    ensure(
        data["changed"] == Value::Bool(true),
        format!("happy-path changed must be true; got {data}"),
    )?;
    ensure(
        data["idempotency"].as_str() == Some("changed"),
        format!("happy-path idempotency must be `changed`; got {data}"),
    )?;
    ensure(
        data["audit_id"].is_string(),
        format!("happy-path audit_id must be a string; got {data}"),
    )?;
    ensure(
        data["valid_to"].is_string(),
        format!("happy-path valid_to must be set after expire; got {data}"),
    )?;
    Ok(())
}

#[test]
fn memory_expire_rejects_tombstoned_memory_without_include_tombstoned() -> TestResult {
    let workspace = unique_workspace("tombstone-policy")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(
        &workspace_arg,
        "Pin-test memory-expire tombstone-policy target.",
    )?;
    tombstone_memory(&workspace_arg, &memory_id)?;

    let (output, parsed) = run_expire(&workspace_arg, &memory_id, &[])?;
    ensure(
        !output.status.success(),
        format!(
            "ee memory expire on tombstoned memory without --include-tombstoned must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &parsed,
        &["Memory is tombstoned and cannot be expired."],
        &["Use ee memory show to inspect the tombstoned memory."],
    )
}

#[test]
fn memory_expire_include_tombstoned_reports_already_expired_no_change() -> TestResult {
    let workspace = unique_workspace("tombstone-idempotent")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(
        &workspace_arg,
        "Pin-test memory-expire tombstone-idempotent target.",
    )?;
    tombstone_memory(&workspace_arg, &memory_id)?;

    let (output, parsed) = run_expire(&workspace_arg, &memory_id, &["--include-tombstoned"])?;
    ensure(
        output.status.success(),
        format!(
            "ee memory expire --include-tombstoned on tombstoned memory must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    ensure(
        data["status"].as_str() == Some("already_expired"),
        format!("include-tombstoned status must be already_expired; got {data}"),
    )?;
    ensure(
        data["persisted"] == Value::Bool(false),
        format!("include-tombstoned persisted must be false; got {data}"),
    )?;
    ensure(
        data["changed"] == Value::Bool(false),
        format!("include-tombstoned changed must be false; got {data}"),
    )?;
    ensure(
        data["idempotency"].as_str() == Some("no_change"),
        format!("include-tombstoned idempotency must be no_change; got {data}"),
    )?;
    ensure(
        data["tombstoned_at"].is_string(),
        format!("include-tombstoned tombstoned_at must be preserved; got {data}"),
    )?;
    Ok(())
}

#[test]
fn memory_expire_is_idempotent_on_already_expired_memory() -> TestResult {
    let workspace = unique_workspace("idempotent")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let memory_id = remember(&workspace_arg, "Pin-test memory-expire idempotent target.")?;

    // First expire: expected `changed`.
    let (first_output, first_parsed) = run_expire(&workspace_arg, &memory_id, &[])?;
    ensure(
        first_output.status.success(),
        "first expire must succeed".to_string(),
    )?;
    ensure(
        first_parsed["data"]["status"].as_str() == Some("expired"),
        format!(
            "first expire must yield status=expired; got {}",
            first_parsed["data"]
        ),
    )?;

    // Second expire on the same memory: idempotent replay must return
    // status=already_expired, idempotency=no_change.
    let (second_output, second_parsed) = run_expire(&workspace_arg, &memory_id, &[])?;
    ensure(
        second_output.status.success(),
        "second (idempotent) expire must succeed".to_string(),
    )?;
    let second_data = &second_parsed["data"];
    ensure(
        second_data["status"].as_str() == Some("already_expired"),
        format!("second expire must yield status=already_expired; got {second_data}"),
    )?;
    ensure(
        second_data["persisted"] == Value::Bool(false),
        format!("idempotent replay persisted must be false; got {second_data}"),
    )?;
    ensure(
        second_data["changed"] == Value::Bool(false),
        format!("idempotent replay changed must be false; got {second_data}"),
    )?;
    ensure(
        second_data["idempotency"].as_str() == Some("no_change"),
        format!("idempotent replay idempotency must be `no_change`; got {second_data}"),
    )?;
    Ok(())
}
