//! bd-2r8vp: real-binary pin test for `ee curate auto-promote`.
//!
//! Locks the three primary surfaces:
//!
//! * Missing database -> Storage repair `"ee init --workspace ."`
//! * Empty workspace dry-run -> envelope with scanned=0, eligible=0,
//!   `dryRun=true`, no `memory.level_transition` mutations
//! * Explicit help renders the subcommand
//!
//! Deeper threshold + apply-mode coverage lives in the inline unit
//! tests in `src/core/curate.rs` (search for
//! `auto_promote_proposes_eligible_memories_and_writes_no_audit_rows_in_dry_run`).

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
        .join("ee-curate-auto-promote-pin")
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

fn run_auto_promote(workspace_arg: &str, extra: &[&str]) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec![
        "--workspace",
        workspace_arg,
        "--json",
        "curate",
        "auto-promote",
    ];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("curate auto-promote stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

#[test]
fn curate_auto_promote_surfaces_storage_error_when_database_missing() -> TestResult {
    let workspace = unique_workspace("no-db")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();

    let (output, parsed) = run_auto_promote(&workspace_arg, &[])?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate auto-promote without ee init must fail; stdout: {}",
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
        message.contains("Database not found at"),
        format!("message must surface missing-db; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee init --workspace ."),
        format!("repair must point at ee init; got {repair}"),
    )
}

#[test]
fn curate_auto_promote_dry_run_on_empty_workspace_emits_zero_proposals_and_no_mutation()
-> TestResult {
    let workspace = unique_workspace("empty")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_auto_promote(&workspace_arg, &["--propose", "--dry-run"])?;
    ensure(
        output.status.success(),
        format!(
            "ee curate auto-promote on empty workspace must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = parsed
        .get("data")
        .cloned()
        .unwrap_or_else(|| parsed.clone());
    ensure(
        data.get("schema")
            .and_then(Value::as_str)
            .is_some_and(|schema| schema == "ee.curate.auto_promote.v1"),
        format!(
            "schema must be ee.curate.auto_promote.v1; got {:?}",
            data.get("schema")
        ),
    )?;
    ensure(
        data.get("dryRun").and_then(Value::as_bool) == Some(true),
        format!("dryRun must be true; got {:?}", data.get("dryRun")),
    )?;
    ensure(
        data.get("scannedMemoryCount").and_then(Value::as_u64) == Some(0),
        format!(
            "scannedMemoryCount must be 0 on empty workspace; got {:?}",
            data.get("scannedMemoryCount")
        ),
    )?;
    ensure(
        data.get("eligibleCount").and_then(Value::as_u64) == Some(0),
        "eligibleCount must be 0 on empty workspace",
    )?;
    ensure(
        data.get("appliedCount").and_then(Value::as_u64) == Some(0),
        "appliedCount must be 0 in dry-run",
    )?;
    ensure(
        data.get("durableMutation").and_then(Value::as_bool) == Some(false),
        "durableMutation must be false in dry-run",
    )?;
    Ok(())
}

#[test]
fn curate_auto_promote_dry_run_default_when_apply_not_set() -> TestResult {
    // bd-2r8vp safety invariant: without --apply, the surface must
    // never report `apply=true`, regardless of whether --propose or
    // --dry-run are also set.
    let workspace = unique_workspace("default-dry")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_auto_promote(&workspace_arg, &[])?;
    ensure(
        output.status.success(),
        format!(
            "ee curate auto-promote must default to dry-run; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = parsed
        .get("data")
        .cloned()
        .unwrap_or_else(|| parsed.clone());
    ensure(
        data.get("apply").and_then(Value::as_bool) == Some(false),
        "apply must default to false",
    )?;
    ensure(
        data.get("dryRun").and_then(Value::as_bool) == Some(true),
        "dryRun must be true by default",
    )?;
    ensure(
        data.get("durableMutation").and_then(Value::as_bool) == Some(false),
        "default invocation must not mutate state",
    )?;
    Ok(())
}
