//! bd-1340j: real-binary pin test for `ee graph centrality --memory-id` when
//! the supplied id does not appear in the latest centrality snapshot.
//!
//! `build_graph_centrality_read_report` (src/cli/mod.rs:25706) filters
//! centrality scores by `--memory-id` with
//! `is_none_or(|memory_id| memory_id == &score.memory_id)`. When the supplied
//! id matches nothing, rows ends up empty but the report continues with
//! `status="available"`, echoes `memoryId`, and keeps the snapshot block
//! populated. tests/e2e_graph_centrality.rs:369 covers only the
//! happy path (filter narrows rows to a single matching entry); a refactor
//! that flipped the no-match branch to `algorithm_unavailable` or
//! `scores_unavailable` would silently change agent-visible behavior without
//! breaking the existing happy-path test.
//!
//! This pin-test mirrors the triangle-graph harness shape of
//! tests/e2e_graph_centrality_algorithm.rs and pins the four observable
//! aspects of the no-match branch.

#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use ee::db::{CreateMemoryLinkInput, DbConnection, MemoryLinkRelation, MemoryLinkSource};
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
        .join("ee-graph-centrality-memory-id-no-match-pin")
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

fn insert_link(database_path: &std::path::Path, link_id: &str, src: &str, dst: &str) -> TestResult {
    let connection = DbConnection::open_file(database_path).map_err(|error| error.to_string())?;
    connection
        .insert_memory_link(
            link_id,
            &CreateMemoryLinkInput {
                src_memory_id: src.to_owned(),
                dst_memory_id: dst.to_owned(),
                relation: MemoryLinkRelation::Supports,
                weight: 0.9,
                confidence: 0.8,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: Some("2026-05-01T00:00:00Z".to_string()),
                source: MemoryLinkSource::Human,
                created_by: Some("e2e-graph-centrality-memory-id-no-match-pin".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn refresh_centrality(workspace_arg: &str) -> TestResult {
    let refresh = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "graph",
        "centrality-refresh",
    ])?;
    ensure(
        refresh.status.success(),
        format!(
            "centrality-refresh must succeed; stderr: {}",
            String::from_utf8_lossy(&refresh.stderr)
        ),
    )
}

fn seed_refreshed_triangle() -> Result<(PathBuf, String), String> {
    let workspace = unique_workspace("triangle")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let alpha = remember(&workspace_arg, "Pin-test memory-id no-match alpha.")?;
    let beta = remember(&workspace_arg, "Pin-test memory-id no-match beta.")?;
    let gamma = remember(&workspace_arg, "Pin-test memory-id no-match gamma.")?;

    let database_path = workspace.join(".ee").join("ee.db");
    insert_link(
        &database_path,
        "link_00000000000000000000pinnom01",
        &alpha,
        &beta,
    )?;
    insert_link(
        &database_path,
        "link_00000000000000000000pinnom02",
        &beta,
        &gamma,
    )?;
    insert_link(
        &database_path,
        "link_00000000000000000000pinnom03",
        &gamma,
        &alpha,
    )?;

    refresh_centrality(&workspace_arg)?;
    Ok((workspace, workspace_arg))
}

#[test]
fn graph_centrality_memory_id_no_match_returns_available_with_empty_rows() -> TestResult {
    let (_workspace, workspace_arg) = seed_refreshed_triangle()?;

    let phantom = "mem_phantom_not_in_centrality_snapshot_xyz";
    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "graph",
        "centrality",
        "--memory-id",
        phantom,
    ])?;
    ensure(
        output.status.success(),
        format!(
            "graph centrality --memory-id <phantom> must exit zero on a refreshed workspace; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("graph centrality stdout must be JSON: {error}"))?;
    let data = &parsed["data"];
    ensure(
        data["schema"].as_str() == Some("ee.graph.centrality_read.v1"),
        format!("schema must be ee.graph.centrality_read.v1; got {data}"),
    )?;
    ensure(
        data["status"].as_str() == Some("available"),
        format!(
            "status must remain 'available' when the snapshot exists even if --memory-id matches nothing; got {data}"
        ),
    )?;
    ensure(
        data["memoryId"].as_str() == Some(phantom),
        format!("memoryId echo must equal the supplied phantom id; got {data}"),
    )?;
    let rows = data["rows"]
        .as_array()
        .ok_or_else(|| format!("rows must be an array; got {data}"))?;
    ensure(
        rows.is_empty(),
        format!("rows must be empty when --memory-id matches no scored memory; got {rows:?}"),
    )?;
    ensure(
        data["snapshotHash"].is_string(),
        format!("snapshotHash must be populated for an available snapshot; got {data}"),
    )?;
    ensure(
        data["computedAt"].is_string(),
        format!("computedAt must be populated for an available snapshot; got {data}"),
    )?;
    Ok(())
}
