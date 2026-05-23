//! bd-3vf6j: real-binary pin test for `ee graph path`.
//!
//! Mirrors the runtime shape of `graph_neighborhood_smoke.rs` and
//! `e2e_graph_centrality.rs` but exercises CLI angles the existing graph
//! tests do not assert against the real `ee` binary:
//!
//! * empty source/destination memory id surfaces the documented
//!   `DomainError::Usage` repair text
//! * `--min-weight` outside `[0.0, 1.0]` surfaces the documented threshold
//!   usage error
//! * a fresh workspace with no links returns the `no_path` branch with a
//!   null `path` and a well-formed surface envelope
//! * a directed two-edge path src -> mid -> dst surfaces `path_found`,
//!   `pathLength = 2`, and the three memory ids in order
//! * two memories with no connecting link surface `no_path` with `path` set
//!   to `null`
//! * the `ee.graph.algorithm.v1` envelope carries `command="graph path"`,
//!   `srcMemoryId`, `dstMemoryId`, `path`, `pathLength`, `witness`, and a
//!   populated `graph` block

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
        .join("ee-graph-path-pin")
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

fn insert_link(
    database_path: &std::path::Path,
    link_id: &str,
    src: &str,
    dst: &str,
    weight: f32,
    confidence: f32,
) -> TestResult {
    let connection = DbConnection::open_file(database_path).map_err(|error| error.to_string())?;
    connection
        .insert_memory_link(
            link_id,
            &CreateMemoryLinkInput {
                src_memory_id: src.to_owned(),
                dst_memory_id: dst.to_owned(),
                relation: MemoryLinkRelation::Supports,
                weight,
                confidence,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: Some("2026-05-01T00:00:00Z".to_string()),
                source: MemoryLinkSource::Human,
                created_by: Some("e2e-graph-path-pin".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn run_graph_path(
    workspace_arg: &str,
    src: &str,
    dst: &str,
    extra: &[&str],
) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec![
        "--workspace",
        workspace_arg,
        "--json",
        "graph",
        "path",
        src,
        dst,
    ];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("graph path stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn assert_envelope_shape(data: &Value, expected_status: &str) -> TestResult {
    ensure(
        data["schema"].as_str() == Some("ee.graph.algorithm.v1"),
        format!("schema must be ee.graph.algorithm.v1; got {data}"),
    )?;
    ensure(
        data["command"].as_str() == Some("graph path"),
        format!("command must be graph path; got {data}"),
    )?;
    ensure(
        data["status"].as_str() == Some(expected_status),
        format!("status must be {expected_status}; got {data}"),
    )?;
    ensure(
        data["graph"].is_object(),
        format!("graph block must be present; got {data}"),
    )?;
    ensure(
        data["graph"]["sourceLinkCount"].is_u64(),
        format!("graph.sourceLinkCount must be numeric; got {data}"),
    )?;
    ensure(
        data["srcMemoryId"].is_string(),
        format!("srcMemoryId must echo as string; got {data}"),
    )?;
    ensure(
        data["dstMemoryId"].is_string(),
        format!("dstMemoryId must echo as string; got {data}"),
    )?;
    ensure(
        data.get("witness").is_some(),
        format!("witness key must be present; got {data}"),
    )?;
    Ok(())
}

fn seed_two_edge_path() -> Result<(PathBuf, String, String, String, String), String> {
    let workspace = unique_workspace("two-edge")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let src = remember(&workspace_arg, "Pin-test graph path src node.")?;
    let mid = remember(&workspace_arg, "Pin-test graph path mid node.")?;
    let dst = remember(&workspace_arg, "Pin-test graph path dst node.")?;

    let database_path = workspace.join(".ee").join("ee.db");
    insert_link(
        &database_path,
        "link_00000000000000000000pinpath01",
        &src,
        &mid,
        0.9,
        0.8,
    )?;
    insert_link(
        &database_path,
        "link_00000000000000000000pinpath02",
        &mid,
        &dst,
        0.9,
        0.8,
    )?;

    Ok((workspace, workspace_arg, src, mid, dst))
}

#[test]
fn graph_path_rejects_empty_memory_pair_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-empty")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_path(&workspace_arg, "", "mem_dst", &[])?;
    ensure(
        !output.status.success(),
        format!(
            "graph path with empty source must fail; stdout: {}",
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
        message.contains("source and destination memory IDs must be non-empty"),
        format!("usage message must pin validate_graph_memory_pair text; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee graph path mem_a mem_b"),
        format!("usage repair must reference the documented example; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn graph_path_rejects_min_weight_out_of_range_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-weight")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_path(
        &workspace_arg,
        "mem_src",
        "mem_dst",
        &["--min-weight", "2.0"],
    )?;
    ensure(
        !output.status.success(),
        format!(
            "graph path with --min-weight 2.0 must fail; stdout: {}",
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
        message.contains("--min-weight") && message.contains("finite value in [0.0, 1.0]"),
        format!("usage message must pin validate_graph_threshold text; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("--min-weight"),
        format!("usage repair must reference --min-weight; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn graph_path_returns_no_path_on_fresh_workspace() -> TestResult {
    let workspace = unique_workspace("no-links")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let src = remember(&workspace_arg, "Pin-test isolated source.")?;
    let dst = remember(&workspace_arg, "Pin-test isolated destination.")?;

    let (output, parsed) = run_graph_path(&workspace_arg, &src, &dst, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph path on a fresh workspace must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    assert_envelope_shape(data, "no_path")?;
    ensure(
        data["srcMemoryId"].as_str() == Some(src.as_str()),
        format!("srcMemoryId must echo source; got {data}"),
    )?;
    ensure(
        data["dstMemoryId"].as_str() == Some(dst.as_str()),
        format!("dstMemoryId must echo destination; got {data}"),
    )?;
    ensure(
        data["path"].is_null(),
        format!("path must be null when no_path; got {data}"),
    )?;
    ensure(
        data["pathLength"].is_null(),
        format!("pathLength must be null when no_path; got {data}"),
    )?;
    Ok(())
}

#[test]
fn graph_path_returns_path_found_on_two_edge_directed_path() -> TestResult {
    let (_workspace, workspace_arg, src, mid, dst) = seed_two_edge_path()?;

    let (output, parsed) = run_graph_path(&workspace_arg, &src, &dst, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph path with a connecting path must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    assert_envelope_shape(data, "path_found")?;
    ensure(
        data["pathLength"].as_u64() == Some(2),
        format!("pathLength must be 2 (two edges) when src->mid->dst; got {data}"),
    )?;
    let path = data["path"]
        .as_array()
        .ok_or_else(|| format!("path must be an array when path_found; got {data}"))?;
    ensure(
        path.len() == 3,
        format!("path must contain three node ids; got {path:?}"),
    )?;
    ensure(
        path[0].as_str() == Some(src.as_str()),
        format!("path[0] must be src; got {path:?}"),
    )?;
    ensure(
        path[1].as_str() == Some(mid.as_str()),
        format!("path[1] must be mid; got {path:?}"),
    )?;
    ensure(
        path[2].as_str() == Some(dst.as_str()),
        format!("path[2] must be dst; got {path:?}"),
    )?;
    ensure(
        data["graph"]["sourceLinkCount"].as_u64() == Some(2),
        format!("sourceLinkCount must reflect two seeded links; got {data}"),
    )?;
    Ok(())
}

#[test]
fn graph_path_returns_no_path_when_memories_unlinked() -> TestResult {
    let (workspace, workspace_arg, src, _mid, dst) = seed_two_edge_path()?;
    // src -> mid -> dst is connected. Add an isolated pair (lone, alone) and
    // assert there is no path between them even though other links exist.
    let lone = remember(&workspace_arg, "Pin-test lone node.")?;
    let alone = remember(&workspace_arg, "Pin-test alone node.")?;
    // Use the dst variable so the seed continues to compile if the helper
    // changes; this keeps src/dst alive without affecting the assertion.
    let _ = (workspace, src, dst);

    let (output, parsed) = run_graph_path(&workspace_arg, &lone, &alone, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph path between unlinked memories must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    assert_envelope_shape(data, "no_path")?;
    ensure(
        data["path"].is_null(),
        format!("path must be null when unlinked; got {data}"),
    )?;
    ensure(
        data["pathLength"].is_null(),
        format!("pathLength must be null when unlinked; got {data}"),
    )?;
    Ok(())
}
