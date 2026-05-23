//! bd-1gka6: real-binary pin test for `ee curate validate` error
//! surfaces.
//!
//! `ee curate validate` is the gateway between `ee curate candidates`
//! (listing, pinned in bd-29acc) and `ee curate apply` (graph
//! mutation) — the validation step that transitions a candidate from
//! pending to approved or marks it needs_evidence.
//! `handle_curate_validate` (src/cli/mod.rs:36721) and
//! `validate_curation_candidate` (src/core/curate.rs:2966) have three
//! distinct error surfaces no test pins end-to-end against the real
//! binary:
//!
//! * Invalid candidate id format (e.g. `bad-id`, not `curate_…`) ->
//!   Usage `"invalid curation candidate ID"` + repair
//!   `"ee curate candidates --json"`
//! * Missing database -> Storage repair `"ee init --workspace ."`
//! * Valid format but non-existent candidate id -> NotFound
//!   `"curation candidate"` + repair `"ee curate candidates --json"`

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
        .join("ee-curate-validate-errors-pin")
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

fn run_validate(
    workspace_arg: &str,
    candidate_id: &str,
    extra: &[&str],
) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec![
        "--workspace",
        workspace_arg,
        "--json",
        "curate",
        "validate",
        candidate_id,
    ];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("curate validate stdout must be JSON: {error}"))?;
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
fn curate_validate_rejects_invalid_candidate_id_format_with_usage_repair() -> TestResult {
    // validate_curate_candidate_id requires "curate_" prefix + 33
    // chars total + alphanumeric suffix. A bare `bad-id` value fails
    // the format check before any database lookup runs.
    let workspace = unique_workspace("bad-id")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_validate(&workspace_arg, "bad-id", &[])?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate validate `bad-id` must fail; stdout: {}",
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
fn curate_validate_surfaces_storage_error_when_database_missing() -> TestResult {
    // Skip ee init so the database-existence guard inside
    // open_existing_database fires before any candidate lookup.
    // Use a valid-format candidate id so the validator passes
    // before we hit the missing-db guard.
    let workspace = unique_workspace("no-db")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();

    // Valid format: curate_ + 26 alphanumerics = 33 chars total.
    let valid_id = "curate_00000000000000000000000001";
    let (output, parsed) = run_validate(&workspace_arg, valid_id, &[])?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate validate without ee init must fail; stdout: {}",
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
fn curate_validate_returns_not_found_for_valid_format_but_missing_candidate() -> TestResult {
    let workspace = unique_workspace("not-found")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let valid_id = "curate_00000000000000000000000099";
    let (output, parsed) = run_validate(&workspace_arg, valid_id, &[])?;
    ensure(
        !output.status.success(),
        format!(
            "ee curate validate on unknown candidate must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    // NotFound errors include the resource name and the id in
    // structured fields. Verify the message OR id field surfaces the
    // candidate id and that the repair points back to listing.
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
