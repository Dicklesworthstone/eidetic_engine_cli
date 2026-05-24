//! bd-zf7y2: real-binary pin test for `ee graph pagerank --include-tombstoned`
//! tombstone-exclusion semantics.
//!
//! `GraphAlgorithmArgs.include_tombstoned` (src/cli/mod.rs:3384) drives
//! `graph_filter_tombstoned_links` (src/cli/mod.rs:24885) which excludes
//! links whose endpoints are tombstoned memories. The graph-algorithm
//! envelope reports excluded tombstoned ids under `graph.excludedNodes`
//! (`graph_metric_data_with_status`, src/cli/mod.rs:24938).
//!
//! tests/e2e_graph_pagerank.rs covers --limit 0, --min-weight out-of-range,
//! and the happy path on a connected graph, but the tombstone-exclusion
//! semantics and the `excludedNodes` surface have no real-binary
//! assertions for any graph algorithm. This pin-test mirrors the
//! e2e_graph_explain_link.rs harness shape and pins, using pagerank as
//! the canonical algorithm and `ee curate tombstone` (the same tombstoning
//! path tests/e2e_memory_expire.rs uses):
//!
//! * default `ee graph pagerank` (no --include-tombstoned) on a seed with
//!   one tombstoned endpoint surfaces `graph.excludedNodes` containing
//!   the tombstoned memory id.
//! * `ee graph pagerank --include-tombstoned` on the same seed surfaces
//!   `graph.excludedNodes` as an empty array, and the tombstoned memory
//!   appears in the scored rows.

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
        .join("ee-graph-pagerank-tombstoned-pin")
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
                created_by: Some("e2e-graph-pagerank-tombstoned-pin".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(())
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
            "curate tombstone must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn run_pagerank(workspace_arg: &str, extra: &[&str]) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec!["--workspace", workspace_arg, "--json", "graph", "pagerank"];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("graph pagerank stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn seed_workspace_with_tombstoned_dst() -> Result<(PathBuf, String, String, String), String> {
    let workspace = unique_workspace("pair-tombstone")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let src = remember(&workspace_arg, "Pin-test pagerank tombstone src.")?;
    let dst = remember(&workspace_arg, "Pin-test pagerank tombstone dst.")?;
    let database_path = workspace.join(".ee").join("ee.db");
    insert_link(
        &database_path,
        "link_00000000000000000000pintomb01",
        &src,
        &dst,
    )?;
    tombstone_memory(&workspace_arg, &dst)?;
    Ok((workspace, workspace_arg, src, dst))
}

#[test]
fn graph_pagerank_default_excludes_tombstoned_node_via_excluded_nodes() -> TestResult {
    let (_workspace, workspace_arg, _src, dst) = seed_workspace_with_tombstoned_dst()?;

    let (output, parsed) = run_pagerank(&workspace_arg, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph pagerank (default, no --include-tombstoned) must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    let excluded = data["graph"]["excludedNodes"]
        .as_array()
        .ok_or_else(|| format!("graph.excludedNodes must be an array; got {data}"))?;
    let dst_excluded = excluded
        .iter()
        .any(|entry| entry.as_str() == Some(dst.as_str()));
    ensure(
        dst_excluded,
        format!("graph.excludedNodes must contain the tombstoned dst id {dst}; got {excluded:?}"),
    )?;
    Ok(())
}

#[test]
fn graph_pagerank_with_include_tombstoned_clears_excluded_nodes() -> TestResult {
    let (_workspace, workspace_arg, _src, dst) = seed_workspace_with_tombstoned_dst()?;

    let (output, parsed) = run_pagerank(&workspace_arg, &["--include-tombstoned"])?;
    ensure(
        output.status.success(),
        format!(
            "graph pagerank --include-tombstoned must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    let excluded = data["graph"]["excludedNodes"]
        .as_array()
        .ok_or_else(|| format!("graph.excludedNodes must be an array; got {data}"))?;
    ensure(
        excluded.is_empty(),
        format!(
            "graph.excludedNodes must be empty when --include-tombstoned is set; got {excluded:?}"
        ),
    )?;
    let scores = data["scores"]
        .as_array()
        .ok_or_else(|| format!("scores must be an array; got {data}"))?;
    let dst_in_scores = scores
        .iter()
        .any(|row| row["memoryId"].as_str() == Some(dst.as_str()));
    ensure(
        dst_in_scores,
        format!(
            "tombstoned dst {dst} must appear in pagerank scores when --include-tombstoned is set; got {scores:?}"
        ),
    )?;
    Ok(())
}
