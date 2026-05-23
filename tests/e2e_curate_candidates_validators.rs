//! bd-29acc: real-binary pin test for `ee curate candidates`
//! validators.
//!
//! The curation queue is the graph-adjacent review surface that
//! consumes `derivation_source_refs` (the typed source refs that
//! `bd-kxm0c` propose-derived emits) and feeds into `ee curate apply`
//! (which mutates the graph). `handle_curate_candidates`
//! (src/cli/mod.rs:36650) plus `list_curation_candidates` parsers
//! have four distinct Usage paths no test previously pinned
//! end-to-end against the real binary:
//!
//! * `--all` + `--status pending` -> Usage `"--all cannot be
//!   combined with --status"` + repair `"ee curate candidates
//!   --help"`
//! * `--type garbage_type` -> Usage `"unknown candidate type
//!   `garbage_type`"` with the full vocabulary listed in the message
//!   (consolidate, promote, deprecate, supersede, tombstone, merge,
//!   paraphrase_dedup_proposal, split, retract, rule,
//!   anti_pattern_proposal, procedure, create_derived_memory)
//! * `--status garbage_status` -> Usage `"unknown candidate status
//!   `garbage_status`"` with the full vocabulary listed (pending,
//!   approved, rejected, expired, applied)
//! * Missing database -> Storage repair `"ee init --workspace ."`
//! * Happy path on empty workspace -> success envelope with empty
//!   candidates array

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
        .join("ee-curate-candidates-validators-pin")
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

fn run_candidates(workspace_arg: &str, extra: &[&str]) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec![
        "--workspace",
        workspace_arg,
        "--json",
        "curate",
        "candidates",
    ];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("curate candidates stdout must be JSON: {error}"))?;
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
fn curate_candidates_rejects_all_combined_with_status_with_usage_repair() -> TestResult {
    let workspace = unique_workspace("all-plus-status")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_candidates(&workspace_arg, &["--all", "--status", "pending"])?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate candidates --all --status pending must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_error_with_repair(
        &parsed,
        &["--all cannot be combined with --status"],
        &["ee curate candidates --help"],
    )
}

#[test]
fn curate_candidates_rejects_unknown_type_with_vocabulary_message() -> TestResult {
    let workspace = unique_workspace("bad-type")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_candidates(&workspace_arg, &["--type", "garbage_type"])?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate candidates --type garbage_type must fail; stdout: {}",
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
        message.contains("unknown candidate type") && message.contains("garbage_type"),
        format!("usage message must name the unknown type; got {message}"),
    )?;
    // Vocabulary must be complete in the message so an agent can
    // recover from one error.
    for valid in [
        "consolidate",
        "promote",
        "deprecate",
        "supersede",
        "tombstone",
        "merge",
        "paraphrase_dedup_proposal",
        "split",
        "retract",
        "rule",
        "anti_pattern_proposal",
        "procedure",
        "create_derived_memory",
    ] {
        ensure(
            message.contains(valid),
            format!(
                "usage message must list every valid candidate type ({valid} missing); got {message}"
            ),
        )?;
    }
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee curate candidates --help"),
        format!("usage repair must point at help; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn curate_candidates_rejects_unknown_status_with_vocabulary_message() -> TestResult {
    let workspace = unique_workspace("bad-status")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_candidates(&workspace_arg, &["--status", "garbage_status"])?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate candidates --status garbage_status must fail; stdout: {}",
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
        message.contains("unknown candidate status") && message.contains("garbage_status"),
        format!("usage message must name the unknown status; got {message}"),
    )?;
    for valid in ["pending", "approved", "rejected", "expired", "applied"] {
        ensure(
            message.contains(valid),
            format!(
                "usage message must list every valid candidate status ({valid} missing); got {message}"
            ),
        )?;
    }
    Ok(())
}

#[test]
fn curate_candidates_surfaces_storage_error_when_database_missing() -> TestResult {
    let workspace = unique_workspace("no-db")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();

    let (output, parsed) = run_candidates(&workspace_arg, &[])?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate candidates without ee init must fail; stdout: {}",
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
fn curate_candidates_happy_path_on_empty_workspace_returns_empty_listing() -> TestResult {
    let workspace = unique_workspace("empty")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_candidates(&workspace_arg, &[])?;
    ensure(
        output.status.success(),
        format!(
            "ee curate candidates on empty workspace must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        parsed["error"].is_null() || parsed.get("error").is_none(),
        format!("happy path must not surface an error; got {parsed}"),
    )?;
    // The exact envelope shape varies by renderer version, but a
    // successful empty listing must NOT carry a non-empty candidates
    // array on a freshly-init'd workspace.
    let data = parsed.get("data").unwrap_or(&parsed);
    if let Some(candidates) = data["candidates"].as_array() {
        ensure(
            candidates.is_empty(),
            format!("empty workspace must yield empty candidates; got {candidates:?}"),
        )?;
    }
    Ok(())
}
