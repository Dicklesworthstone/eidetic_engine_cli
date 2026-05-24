//! bd-13b2n: real-binary pin test for `ee graph neighborhood` validator
//! surfaces that the existing tests do not cover.
//!
//! `handle_graph_neighborhood` (src/cli/mod.rs:26473) emits four distinct
//! validator/storage errors before the graph read runs:
//!
//! * unknown `--direction` value → `DomainError::Usage` with
//!   "Unknown direction filter: {other}" and a `Use one of incoming,
//!   outgoing, both.` repair (already partially covered by
//!   `tests/graph_neighborhood_smoke.rs:1859`, which only asserts that the
//!   command fails and the message contains "direction").
//! * unknown `--relation` value → `MemoryLinkRelation::parse` returns
//!   `None` → `DomainError::Usage` with
//!   "Unknown memory link relation: {raw}" and a repair enumerating
//!   `supports, contradicts, derived_from, supersedes, related, co_tag,
//!   co_mention`.
//! * `--limit 0` → `DomainError::Usage` with
//!   "--limit must be greater than zero" and
//!   "Omit --limit to keep all neighbors." as repair.
//! * Database not found (no `ee init`) → `DomainError::Storage` with
//!   "Database not found at <path>" and "ee init --workspace ." as repair.
//!
//! The last three branches have no real-binary assertions today. This
//! pin-test mirrors the `tests/e2e_graph_explain_link.rs` harness shape and
//! pins them.

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
        .join("ee-graph-neighborhood-validators-pin")
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

fn run_neighborhood(
    workspace_arg: &str,
    memory_id: &str,
    extra: &[&str],
) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec![
        "--workspace",
        workspace_arg,
        "--json",
        "graph",
        "neighborhood",
        memory_id,
    ];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("graph neighborhood stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

#[test]
fn graph_neighborhood_rejects_unknown_relation_with_usage_error_and_enumerated_repair() -> TestResult
{
    let workspace = unique_workspace("usage-relation")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_neighborhood(
        &workspace_arg,
        "mem_anything",
        &["--relation", "garbage_relation"],
    )?;
    ensure(
        !output.status.success(),
        format!(
            "graph neighborhood --relation garbage_relation must fail; stdout: {}",
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
        message.contains("Unknown memory link relation: garbage_relation"),
        format!("usage message must pin MemoryLinkRelation::parse text; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("supports")
            && repair.contains("contradicts")
            && repair.contains("derived_from")
            && repair.contains("supersedes")
            && repair.contains("related")
            && repair.contains("co_tag")
            && repair.contains("co_mention"),
        format!("usage repair must enumerate the canonical relation list; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn graph_neighborhood_rejects_zero_limit_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-limit")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_neighborhood(&workspace_arg, "mem_anything", &["--limit", "0"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph neighborhood --limit 0 must fail; stdout: {}",
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
        message.contains("--limit must be greater than zero"),
        format!("usage message must pin the --limit guard text; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("Omit --limit to keep all neighbors."),
        format!("usage repair must reference `Omit --limit to keep all neighbors.`; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn graph_neighborhood_without_init_surfaces_database_missing_storage_error() -> TestResult {
    let workspace = unique_workspace("no-init")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    // Intentionally skip `ee init` so .ee/ee.db does not exist.

    let (output, parsed) = run_neighborhood(&workspace_arg, "mem_anything", &[])?;
    ensure(
        !output.status.success(),
        format!(
            "graph neighborhood on uninitialized workspace must fail; stdout: {}",
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
        message.contains("Database not found"),
        format!("storage message must pin the Database not found guard; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee init --workspace ."),
        format!("storage repair must point at `ee init --workspace .`; got {repair}"),
    )?;
    Ok(())
}
