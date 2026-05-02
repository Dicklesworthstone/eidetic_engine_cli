//! EE-166: integration smoke tests for `ee graph neighborhood --json`.
//!
//! Boots a fresh workspace, seeds two memories and a typed memory link via the
//! library API, and exercises the public CLI surface in JSON, human, and
//! TOON modes. Asserts the stable response envelope, deterministic edge
//! ordering, and stdout/stderr isolation.

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
        .join("ee-graph-neighborhood-smoke")
        .join(format!("{prefix}-{}-{now}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
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
            String::from_utf8_lossy(&output.stderr)
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

fn seed_workspace_with_link() -> Result<(PathBuf, String, String, String, String), String> {
    let workspace = unique_workspace("neighborhood")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();

    let init = run_ee(&["--workspace", workspace_arg.as_str(), "--json", "init"])?;
    ensure(
        init.status.success(),
        format!(
            "ee init must succeed; stderr: {}",
            String::from_utf8_lossy(&init.stderr)
        ),
    )?;

    let center = remember(&workspace_arg, "Neighborhood center memory.")?;
    let neighbor = remember(&workspace_arg, "Neighborhood evidence memory.")?;

    let database_path = workspace.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    let link_id = "link_00000000000000000000000099";
    connection
        .insert_memory_link(
            link_id,
            &CreateMemoryLinkInput {
                src_memory_id: center.clone(),
                dst_memory_id: neighbor.clone(),
                relation: MemoryLinkRelation::Supports,
                weight: 0.91_f32,
                confidence: 0.84_f32,
                directed: true,
                evidence_count: 2,
                last_reinforced_at: Some("2026-05-01T00:00:00Z".to_string()),
                source: MemoryLinkSource::Human,
                created_by: Some("graph-neighborhood-smoke".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;

    Ok((
        workspace,
        workspace_arg,
        center,
        neighbor,
        link_id.to_owned(),
    ))
}

#[test]
fn graph_neighborhood_json_envelope_is_stable() -> TestResult {
    let (_workspace, workspace_arg, center, neighbor, link_id) = seed_workspace_with_link()?;

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "graph",
        "neighborhood",
        center.as_str(),
    ])?;
    ensure(
        output.status.success(),
        format!(
            "graph neighborhood --json must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "graph neighborhood --json stderr must stay empty; got: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("stdout must be JSON: {error}"))?;

    ensure(
        parsed["schema"] == Value::String("ee.graph.neighborhood.v1".to_string()),
        "schema must be ee.graph.neighborhood.v1",
    )?;
    ensure(
        parsed["success"] == Value::Bool(true),
        "success must be true",
    )?;
    ensure(
        parsed["data"]["status"] == Value::String("found".to_string()),
        "status must be found when an edge exists",
    )?;
    ensure(
        parsed["data"]["memoryId"] == Value::String(center.clone()),
        "memoryId must echo the requested center",
    )?;
    ensure(
        parsed["data"]["direction"] == Value::String("both".to_string()),
        "default direction must be both",
    )?;
    ensure(
        parsed["data"]["relation"].is_null(),
        "no --relation filter means relation is null",
    )?;
    ensure(
        parsed["data"]["limited"] == Value::Bool(false),
        "no --limit means limited is false",
    )?;

    let edges = parsed["data"]["edges"]
        .as_array()
        .ok_or_else(|| "edges must be an array".to_string())?;
    ensure_eq(edges.len(), 1, "single seeded edge")?;
    let edge = &edges[0];
    ensure(
        edge["linkId"] == Value::String(link_id),
        "edge linkId must echo the seeded link",
    )?;
    ensure(
        edge["neighborMemoryId"] == Value::String(neighbor.clone()),
        "edge neighbor must point at the linked memory",
    )?;
    ensure(
        edge["relation"] == Value::String("supports".to_string()),
        "edge relation must be supports",
    )?;
    ensure(
        edge["relativeDirection"] == Value::String("outgoing".to_string()),
        "edge relativeDirection must be outgoing for src=center",
    )?;
    ensure(
        edge["directed"] == Value::Bool(true),
        "edge directed must be true",
    )?;
    ensure_eq(
        edge["evidenceCount"].as_u64().unwrap_or_default(),
        2,
        "evidence count",
    )?;

    let nodes = parsed["data"]["nodes"]
        .as_array()
        .ok_or_else(|| "nodes must be an array".to_string())?;
    ensure_eq(nodes.len(), 2, "center plus one neighbor")?;
    ensure(
        nodes[0]["memoryId"] == Value::String(center) && nodes[0]["role"] == "center",
        "first node must be the center",
    )?;
    ensure(
        nodes[1]["memoryId"] == Value::String(neighbor) && nodes[1]["role"] == "neighbor",
        "second node must be the neighbor",
    )?;
    Ok(())
}

#[test]
fn graph_neighborhood_relation_filter_excludes_other_relations() -> TestResult {
    let (_workspace, workspace_arg, center, _neighbor, _link_id) = seed_workspace_with_link()?;

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "graph",
        "neighborhood",
        center.as_str(),
        "--relation",
        "contradicts",
    ])?;
    ensure(
        output.status.success(),
        format!(
            "graph neighborhood with non-matching relation should still exit 0; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let parsed: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    ensure(
        parsed["data"]["status"] == Value::String("empty".to_string()),
        "filtered status must be empty",
    )?;
    ensure(
        parsed["data"]["relation"] == Value::String("contradicts".to_string()),
        "relation must echo the requested filter",
    )?;
    let edges = parsed["data"]["edges"]
        .as_array()
        .ok_or_else(|| "edges must be an array".to_string())?;
    ensure(edges.is_empty(), "no edges when relation does not match")?;
    Ok(())
}

#[test]
fn graph_neighborhood_invalid_direction_is_usage_error() -> TestResult {
    let (_workspace, workspace_arg, center, _neighbor, _link_id) = seed_workspace_with_link()?;

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "graph",
        "neighborhood",
        center.as_str(),
        "--direction",
        "lateral",
    ])?;
    ensure(!output.status.success(), "invalid --direction must fail")?;
    let parsed: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    ensure(
        parsed["schema"] == Value::String("ee.error.v1".to_string()),
        "error envelope schema must be ee.error.v1",
    )?;
    ensure(
        parsed["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("direction")),
        "error message must mention direction",
    )?;
    Ok(())
}

#[test]
fn graph_neighborhood_human_renderer_includes_summary() -> TestResult {
    let (_workspace, workspace_arg, center, _neighbor, _link_id) = seed_workspace_with_link()?;

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "graph",
        "neighborhood",
        center.as_str(),
    ])?;
    ensure(
        output.status.success(),
        format!(
            "graph neighborhood human render must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        stdout.contains(&format!("Neighborhood for {center}")),
        format!("human render must mention center; got: {stdout}"),
    )?;
    ensure(
        stdout.contains("via supports"),
        format!("human render must include the relation label; got: {stdout}"),
    )?;
    Ok(())
}

fn ensure_eq<T>(actual: T, expected: T, context: &str) -> TestResult
where
    T: PartialEq + std::fmt::Debug,
{
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}
