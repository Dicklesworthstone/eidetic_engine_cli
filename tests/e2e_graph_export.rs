//! bd-39sxq: real-binary pin test for `ee graph export` error paths.
//!
//! Mirrors the runtime shape of `graph_neighborhood_smoke.rs` and the
//! sibling pin tests but exercises angles unique to the export
//! surface. The happy path requires a full snapshot seed which is
//! intentionally out of scope; this pin test locks the three
//! documented error contracts in `handle_graph_export`
//! (src/cli/mod.rs:26255) so future reworks cannot reword them
//! without a deliberate, reviewed change:
//!
//! * Missing database -> exact addressed-store error and conditional
//!   initialization guidance before any export work runs.
//! * `--graph-type garbage_type` -> Usage repair listing all valid
//!   types (memory_links, session_graph, procedure_graph,
//!   evidence_graph, composite).
//! * `--graph-type memory_links` on a freshly-init'd workspace with
//!   no snapshot -> degraded success with a repair pointing at
//!   `ee graph centrality-refresh` so the user knows the next action.

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
        .join("ee-graph-export-pin")
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

fn run_graph_export(workspace_arg: &str, extra: &[&str]) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec!["--workspace", workspace_arg, "--json", "graph", "export"];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("graph export stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

#[test]
fn graph_export_surfaces_storage_error_when_database_missing() -> TestResult {
    // Deliberately skip `ee init` so the database-existence guard in
    // handle_graph_export fires before any export work. This pins
    // exact addressing guidance without creating a new store.
    let workspace = unique_workspace("usage-no-db")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();

    let (output, parsed) = run_graph_export(&workspace_arg, &[])?;
    ensure(
        output.status.code() == Some(10),
        format!(
            "graph export without ee init must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let error = &parsed["error"];
    ensure(
        parsed["schema"] == "ee.error.v2" && error["code"] == "workspace_store_missing",
        format!("response must retain the canonical missing-store error; got {parsed}"),
    )?;
    let workspace = workspace
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let database = workspace.join(".ee/ee.db");
    let message = error["message"].as_str().unwrap_or_default();
    ensure(
        message == format!("Database not found at {}", database.display())
            && error["details"]["addressedStorePath"] == database.to_string_lossy().as_ref(),
        format!("missing-store error must identify the exact database; got {error}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.starts_with("Re-check --workspace addressing")
            && repair.ends_with(&format!(
                "Only if you intended to create a NEW store here: ee init --workspace {}",
                workspace.display()
            )),
        format!("repair must check addressing before conditional exact-path init; got {repair}"),
    )?;
    ensure(
        !workspace.join(".ee").exists(),
        "missing-store export must not initialize a store",
    )
}

#[test]
fn graph_export_rejects_unknown_graph_type_with_usage_error_listing_valid_types() -> TestResult {
    let workspace = unique_workspace("usage-bad-type")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_export(&workspace_arg, &["--graph-type", "garbage_type"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph export --graph-type garbage_type must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    // The repair string lists every valid GraphSnapshotType variant
    // so an agent can discover the full vocabulary from one error.
    let repair = error["repair"].as_str().unwrap_or_default();
    for valid in [
        "memory_links",
        "session_graph",
        "procedure_graph",
        "evidence_graph",
        "composite",
    ] {
        ensure(
            repair.contains(valid),
            format!(
                "usage repair must list every valid graph-type ({valid} missing); got {repair}"
            ),
        )?;
    }
    Ok(())
}

#[test]
fn graph_export_no_snapshot_returns_degraded_refresh_repair() -> TestResult {
    // Init the workspace so the database exists, but skip
    // `ee graph snapshot refresh` / `ee graph centrality-refresh`.
    // The export call returns a degraded success report whose repair
    // tells the user to run centrality-refresh first. This pins the
    // documented next-action surface for the "no snapshot" branch
    // that future reworks of handle_graph_export must not lose.
    let workspace = unique_workspace("usage-no-snapshot")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_export(&workspace_arg, &["--graph-type", "memory_links"])?;
    ensure(
        output.status.success(),
        format!(
            "graph export on a workspace with no snapshot must return degraded success; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    ensure(
        parsed["success"].as_bool() == Some(true),
        format!("response must be a success envelope; got {parsed}"),
    )?;
    let data = &parsed["data"];
    ensure(
        data["status"].as_str() == Some("no_snapshot"),
        format!("data.status must be no_snapshot; got {data}"),
    )?;
    ensure(
        data["degraded"][0]["code"].as_str() == Some("graph_snapshot_missing"),
        format!(
            "data.degraded must include graph_snapshot_missing; got {}",
            data["degraded"]
        ),
    )?;
    let repair = data["degraded"][0]["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee graph centrality-refresh"),
        format!(
            "degraded repair must point at `ee graph centrality-refresh` so the user knows the next action; got {repair}"
        ),
    )?;
    Ok(())
}
