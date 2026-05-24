//! bd-1pd9d: real-binary pin test for `ee graph explain-link`.
//!
//! `handle_graph_explain_link` (src/cli/mod.rs:24476) emits three distinct
//! status branches — `direct_link_found`, `path_found`, and `no_link_path` —
//! plus a `directLinks` array unique to this command. The existing
//! `graph_neighborhood_smoke.rs` test only covers the `path_found` branch
//! through a single bridge memory; the other branches, the validator errors
//! shared with the rest of the graph surface, and the
//! `ee.graph.algorithm.v1` envelope shape for `command="graph explain-link"`
//! have no dedicated assertions.
//!
//! This pin-test mirrors `tests/e2e_graph_path.rs`'s harness to exercise the
//! real `ee` binary and pin:
//!
//! * empty src/dst surfaces the documented `validate_graph_memory_pair`
//!   usage error and repair
//! * `--min-confidence` outside `[0.0, 1.0]` surfaces the documented
//!   `validate_graph_threshold` usage error and repair
//! * a direct link src -> dst surfaces `status=direct_link_found` with a
//!   single-entry `directLinks` array carrying the link id, relation, and
//!   directed flag
//! * a two-edge path src -> mid -> dst with no direct edge surfaces
//!   `status=path_found`, `path=[src,mid,dst]`, `pathLength=2`, and an empty
//!   `directLinks` array
//! * two memories with no connecting link surface `status=no_link_path` with
//!   `path=null` and `pathLength=null`
//! * the surface envelope pins `schema=ee.graph.algorithm.v1`,
//!   `command="graph explain-link"`, and a populated `graph` block.

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
        .join("ee-graph-explain-link-pin")
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
                created_by: Some("e2e-graph-explain-link-pin".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn run_explain_link(
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
        "explain-link",
        src,
        dst,
    ];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("graph explain-link stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn assert_envelope_shape(data: &Value, expected_status: &str) -> TestResult {
    ensure(
        data["schema"].as_str() == Some("ee.graph.algorithm.v1"),
        format!("schema must be ee.graph.algorithm.v1; got {data}"),
    )?;
    ensure(
        data["command"].as_str() == Some("graph explain-link"),
        format!("command must be 'graph explain-link'; got {data}"),
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
        data.get("directLinks").is_some(),
        format!("directLinks key must be present; got {data}"),
    )?;
    ensure(
        data.get("witness").is_some(),
        format!("witness key must be present; got {data}"),
    )?;
    Ok(())
}

#[test]
fn graph_explain_link_rejects_empty_memory_pair_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-empty")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_explain_link(&workspace_arg, "", "mem_dst", &[])?;
    ensure(
        !output.status.success(),
        format!(
            "graph explain-link with empty source must fail; stdout: {}",
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
fn graph_explain_link_rejects_min_confidence_out_of_range_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-confidence")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_explain_link(
        &workspace_arg,
        "mem_src",
        "mem_dst",
        &["--min-confidence", "1.5"],
    )?;
    ensure(
        !output.status.success(),
        format!(
            "graph explain-link with --min-confidence 1.5 must fail; stdout: {}",
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
        message.contains("--min-confidence") && message.contains("finite value in [0.0, 1.0]"),
        format!("usage message must pin validate_graph_threshold text; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("--min-confidence"),
        format!("usage repair must reference --min-confidence; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn graph_explain_link_returns_direct_link_found_when_pair_has_direct_edge() -> TestResult {
    let workspace = unique_workspace("direct-link")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let src = remember(&workspace_arg, "Pin-test explain-link direct src.")?;
    let dst = remember(&workspace_arg, "Pin-test explain-link direct dst.")?;

    let database_path = workspace.join(".ee").join("ee.db");
    let link_id = "link_00000000000000000000pindl001";
    insert_link(&database_path, link_id, &src, &dst, 0.9, 0.8)?;

    let (output, parsed) = run_explain_link(&workspace_arg, &src, &dst, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph explain-link with a direct edge must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    assert_envelope_shape(data, "direct_link_found")?;
    ensure(
        data["srcMemoryId"].as_str() == Some(src.as_str()),
        format!("srcMemoryId must echo source; got {data}"),
    )?;
    ensure(
        data["dstMemoryId"].as_str() == Some(dst.as_str()),
        format!("dstMemoryId must echo destination; got {data}"),
    )?;
    let direct_links = data["directLinks"]
        .as_array()
        .ok_or_else(|| format!("directLinks must be an array; got {data}"))?;
    ensure(
        direct_links.len() == 1,
        format!("directLinks must contain exactly one entry; got {direct_links:?}"),
    )?;
    let only = &direct_links[0];
    ensure(
        only["linkId"].as_str() == Some(link_id),
        format!("directLinks[0].linkId must echo the seeded link id; got {only}"),
    )?;
    ensure(
        only["srcMemoryId"].as_str() == Some(src.as_str()),
        format!("directLinks[0].srcMemoryId must echo src; got {only}"),
    )?;
    ensure(
        only["dstMemoryId"].as_str() == Some(dst.as_str()),
        format!("directLinks[0].dstMemoryId must echo dst; got {only}"),
    )?;
    ensure(
        only["directed"].as_bool() == Some(true),
        format!("directLinks[0].directed must be true; got {only}"),
    )?;
    ensure(
        only["relation"].as_str() == Some("supports"),
        format!("directLinks[0].relation must serialize as 'supports'; got {only}"),
    )?;
    ensure(
        only["evidenceCount"].as_u64() == Some(1),
        format!("directLinks[0].evidenceCount must echo seeded value; got {only}"),
    )?;
    ensure(
        data["graph"]["sourceLinkCount"].as_u64() == Some(1),
        format!("sourceLinkCount must reflect the single seeded link; got {data}"),
    )?;
    Ok(())
}

#[test]
fn graph_explain_link_returns_path_found_on_two_edge_indirect_path() -> TestResult {
    let workspace = unique_workspace("path-only")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let src = remember(&workspace_arg, "Pin-test explain-link indirect src.")?;
    let mid = remember(&workspace_arg, "Pin-test explain-link indirect mid.")?;
    let dst = remember(&workspace_arg, "Pin-test explain-link indirect dst.")?;

    let database_path = workspace.join(".ee").join("ee.db");
    insert_link(
        &database_path,
        "link_00000000000000000000pinpl001",
        &src,
        &mid,
        0.9,
        0.8,
    )?;
    insert_link(
        &database_path,
        "link_00000000000000000000pinpl002",
        &mid,
        &dst,
        0.9,
        0.8,
    )?;

    let (output, parsed) = run_explain_link(&workspace_arg, &src, &dst, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph explain-link with a connecting path must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    assert_envelope_shape(data, "path_found")?;
    let direct_links = data["directLinks"]
        .as_array()
        .ok_or_else(|| format!("directLinks must be an array when path_found; got {data}"))?;
    ensure(
        direct_links.is_empty(),
        format!("directLinks must be empty when no direct edge exists; got {direct_links:?}"),
    )?;
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
fn graph_explain_link_returns_no_link_path_when_pair_unconnected() -> TestResult {
    let workspace = unique_workspace("no-link")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let src = remember(&workspace_arg, "Pin-test explain-link isolated src.")?;
    let dst = remember(&workspace_arg, "Pin-test explain-link isolated dst.")?;

    let (output, parsed) = run_explain_link(&workspace_arg, &src, &dst, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph explain-link between unlinked memories must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    assert_envelope_shape(data, "no_link_path")?;
    let direct_links = data["directLinks"]
        .as_array()
        .ok_or_else(|| format!("directLinks must be an array; got {data}"))?;
    ensure(
        direct_links.is_empty(),
        format!("directLinks must be empty when no_link_path; got {direct_links:?}"),
    )?;
    ensure(
        data["path"].is_null(),
        format!("path must be null when no_link_path; got {data}"),
    )?;
    ensure(
        data["pathLength"].is_null(),
        format!("pathLength must be null when no_link_path; got {data}"),
    )?;
    ensure(
        data["graph"]["sourceLinkCount"].as_u64() == Some(0),
        format!("sourceLinkCount must be zero in a workspace with no links; got {data}"),
    )?;
    Ok(())
}
