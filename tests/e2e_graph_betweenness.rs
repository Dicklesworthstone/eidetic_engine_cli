//! bd-2r363: real-binary pin test for `ee graph betweenness`.
//!
//! Mirrors the runtime shape of `graph_neighborhood_smoke.rs` and the
//! sibling `e2e_graph_pagerank.rs` (bd-2wr4f) but exercises CLI angles
//! the existing graph tests do not assert against the real `ee` binary:
//!
//! * `--limit 0` surfaces the documented `DomainError::Usage` repair text
//! * `--min-weight` / `--min-confidence` outside `[0.0, 1.0]` surface the
//!   documented threshold usage error
//! * a fresh workspace with no links returns `status=computed` with an
//!   empty `scores` array and a well-formed surface envelope carrying
//!   `command="graph betweenness"`
//! * a directed two-edge path src -> mid -> dst surfaces `status=computed`
//!   with `nodeCount=3`, `edgeCount=2`, the middle node scoring strictly
//!   highest (classic betweenness bridge property), scores sorted by
//!   descending score then ascending memoryId, ranks starting at 1, and
//!   a witness whose `edgesScanned > 0`
//! * `--limit 1` truncates the scores array deterministically: the single
//!   surviving row matches the rank-1 row from the unbounded run

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

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
        .join("ee-graph-betweenness-pin")
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
                created_by: Some("e2e-graph-betweenness-pin".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn run_graph_betweenness(workspace_arg: &str, extra: &[&str]) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec![
        "--workspace",
        workspace_arg,
        "--json",
        "graph",
        "betweenness",
    ];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("graph betweenness stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn assert_envelope_shape(data: &Value) -> TestResult {
    ensure(
        data["schema"].as_str() == Some("ee.graph.algorithm.v1"),
        format!("schema must be ee.graph.algorithm.v1; got {data}"),
    )?;
    ensure(
        data["command"].as_str() == Some("graph betweenness"),
        format!("command must be graph betweenness; got {data}"),
    )?;
    ensure(
        data["status"].as_str() == Some("computed"),
        format!("status must be computed; got {data}"),
    )?;
    ensure(
        data["graph"].is_object(),
        format!("graph block must be present; got {data}"),
    )?;
    ensure(
        data["graph"]["nodeCount"].is_u64(),
        format!("graph.nodeCount must be numeric; got {data}"),
    )?;
    ensure(
        data["graph"]["edgeCount"].is_u64(),
        format!("graph.edgeCount must be numeric; got {data}"),
    )?;
    ensure(
        data["graph"]["sourceLinkCount"].is_u64(),
        format!("graph.sourceLinkCount must be numeric; got {data}"),
    )?;
    ensure(
        data["scores"].is_array(),
        format!("scores must be an array; got {data}"),
    )?;
    ensure(
        data["witness"].is_object(),
        format!("witness must be an object; got {data}"),
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

    let src = remember(&workspace_arg, "Pin-test betweenness src node.")?;
    let mid = remember(&workspace_arg, "Pin-test betweenness mid node.")?;
    let dst = remember(&workspace_arg, "Pin-test betweenness dst node.")?;

    let database_path = workspace.join(".ee").join("ee.db");
    insert_link(
        &database_path,
        "link_00000000000000000000btwns001",
        &src,
        &mid,
        0.9,
        0.8,
    )?;
    insert_link(
        &database_path,
        "link_00000000000000000000btwns002",
        &mid,
        &dst,
        0.9,
        0.8,
    )?;

    Ok((workspace, workspace_arg, src, mid, dst))
}

fn assert_usage_error(parsed: &Value, message_needles: &[&str], repair_needle: &str) -> TestResult {
    let error = &parsed["error"];
    ensure(
        error.is_object(),
        format!("response must include an error object; got {parsed}"),
    )?;
    let message = error["message"].as_str().unwrap_or_default();
    for needle in message_needles {
        ensure(
            message.contains(needle),
            format!("usage message must contain {needle:?}; got {message}"),
        )?;
    }
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains(repair_needle),
        format!("usage repair must contain {repair_needle:?}; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn graph_betweenness_rejects_zero_limit_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-zero-limit")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_betweenness(&workspace_arg, &["--limit", "0"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph betweenness --limit 0 must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_usage_error(
        &parsed,
        &["--limit must be greater than zero"],
        "Omit --limit or pass a positive value.",
    )
}

#[test]
fn graph_betweenness_rejects_min_weight_out_of_range_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-weight")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_betweenness(&workspace_arg, &["--min-weight", "2.0"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph betweenness --min-weight 2.0 must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_usage_error(
        &parsed,
        &["--min-weight", "finite value in [0.0, 1.0]"],
        "--min-weight",
    )
}

#[test]
fn graph_betweenness_rejects_min_confidence_out_of_range_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-confidence")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_betweenness(&workspace_arg, &["--min-confidence", "-0.5"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph betweenness --min-confidence -0.5 must fail; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        ),
    )?;
    assert_usage_error(
        &parsed,
        &["--min-confidence", "finite value in [0.0, 1.0]"],
        "--min-confidence",
    )
}

#[test]
fn graph_betweenness_returns_empty_scores_on_fresh_workspace() -> TestResult {
    let workspace = unique_workspace("empty")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_betweenness(&workspace_arg, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph betweenness on a fresh workspace must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        parsed["schema"].as_str() == Some("ee.response.v2"),
        format!("envelope schema must be ee.response.v2; got {parsed}"),
    )?;
    ensure(
        parsed["success"] == Value::Bool(true),
        format!("success must be true; got {parsed}"),
    )?;
    let data = &parsed["data"];
    assert_envelope_shape(data)?;
    ensure(
        data["graph"]["nodeCount"].as_u64() == Some(0),
        format!("nodeCount must be 0 with no links; got {data}"),
    )?;
    ensure(
        data["graph"]["edgeCount"].as_u64() == Some(0),
        format!("edgeCount must be 0 with no links; got {data}"),
    )?;
    ensure(
        data["graph"]["sourceLinkCount"].as_u64() == Some(0),
        format!("sourceLinkCount must be 0 with no links; got {data}"),
    )?;
    ensure(
        data["scores"].as_array().map(Vec::len) == Some(0),
        format!("scores must be empty with no links; got {data}"),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        stdout.lines().filter(|line| !line.is_empty()).count() == 1,
        format!("--json stdout must be a single line; got {stdout}"),
    )?;
    Ok(())
}

#[test]
fn graph_betweenness_ranks_bridge_node_highest_on_two_edge_path() -> TestResult {
    let (_workspace, workspace_arg, src, mid, dst) = seed_two_edge_path()?;

    let (output, parsed) = run_graph_betweenness(&workspace_arg, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph betweenness with seeded links must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    assert_envelope_shape(data)?;
    ensure(
        data["graph"]["nodeCount"].as_u64() == Some(3),
        format!("nodeCount must reflect three seeded nodes; got {data}"),
    )?;
    ensure(
        data["graph"]["edgeCount"].as_u64() == Some(2),
        format!("edgeCount must reflect two seeded edges; got {data}"),
    )?;
    ensure(
        data["graph"]["sourceLinkCount"].as_u64() == Some(2),
        format!("sourceLinkCount must reflect two seeded links; got {data}"),
    )?;

    let scores = data["scores"]
        .as_array()
        .ok_or_else(|| format!("scores must be an array; got {data}"))?;
    ensure(
        scores.len() == 3,
        format!("scores must contain three rows for three nodes; got {scores:?}"),
    )?;
    for (index, score) in scores.iter().enumerate() {
        let expected_rank = u64::try_from(index + 1).expect("rank fits u64");
        ensure(
            score["rank"].as_u64() == Some(expected_rank),
            format!("scores[{index}].rank must be {expected_rank}; got {score}"),
        )?;
        ensure(
            score["memoryId"].is_string(),
            format!("scores[{index}].memoryId must be a string; got {score}"),
        )?;
        ensure(
            score["score"].is_number(),
            format!("scores[{index}].score must be numeric; got {score}"),
        )?;
    }

    // Scores must be sorted by descending score then ascending memoryId
    // (the deterministic order graph_scores_json constructs).
    for window in scores.windows(2) {
        let left = window[0]["score"].as_f64().unwrap_or(0.0);
        let right = window[1]["score"].as_f64().unwrap_or(0.0);
        if (left - right).abs() < f64::EPSILON {
            let left_id = window[0]["memoryId"].as_str().unwrap_or("");
            let right_id = window[1]["memoryId"].as_str().unwrap_or("");
            ensure(
                left_id <= right_id,
                format!(
                    "tied scores must break ties on ascending memoryId; got {left_id} then {right_id}"
                ),
            )?;
        } else {
            ensure(
                left >= right,
                format!("scores must be sorted descending; got {left} before {right}"),
            )?;
        }
    }

    let observed_ids = scores
        .iter()
        .filter_map(|score| score["memoryId"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    for expected in [&src, &mid, &dst] {
        ensure(
            observed_ids.iter().any(|id| id == expected),
            format!("scores must include {expected}; got {observed_ids:?}"),
        )?;
    }

    // Classic betweenness bridge property: on a directed src -> mid -> dst
    // path, mid lies on the only shortest path between src and dst, so it
    // carries strictly higher betweenness than either endpoint. This proves
    // we are actually evaluating betweenness centrality (not just a stub
    // returning uniform scores).
    let mid_score = scores
        .iter()
        .find(|score| score["memoryId"].as_str() == Some(mid.as_str()))
        .ok_or_else(|| format!("scores must include mid {mid}"))?["score"]
        .as_f64()
        .unwrap_or(0.0);
    let src_score = scores
        .iter()
        .find(|score| score["memoryId"].as_str() == Some(src.as_str()))
        .ok_or_else(|| format!("scores must include src {src}"))?["score"]
        .as_f64()
        .unwrap_or(0.0);
    let dst_score = scores
        .iter()
        .find(|score| score["memoryId"].as_str() == Some(dst.as_str()))
        .ok_or_else(|| format!("scores must include dst {dst}"))?["score"]
        .as_f64()
        .unwrap_or(0.0);
    ensure(
        mid_score > src_score,
        format!(
            "betweenness(mid)={mid_score} must exceed betweenness(src)={src_score} on a src->mid->dst path"
        ),
    )?;
    ensure(
        mid_score > dst_score,
        format!(
            "betweenness(mid)={mid_score} must exceed betweenness(dst)={dst_score} on a src->mid->dst path"
        ),
    )?;
    ensure(
        scores[0]["memoryId"].as_str() == Some(mid.as_str()),
        format!(
            "rank-1 row must be the bridge node mid={mid}; got {}",
            scores[0]
        ),
    )?;

    let witness = &data["witness"];
    ensure(
        witness["algorithm"].is_string(),
        format!("witness.algorithm must be a string; got {witness}"),
    )?;
    ensure(
        witness["edgesScanned"].as_u64().unwrap_or(0) > 0,
        format!("witness.edgesScanned must be > 0 after seeding edges; got {witness}"),
    )?;
    Ok(())
}

#[test]
fn graph_betweenness_limit_truncates_deterministically() -> TestResult {
    let (_workspace, workspace_arg, _src, _mid, _dst) = seed_two_edge_path()?;

    let (unbounded_output, unbounded_parsed) = run_graph_betweenness(&workspace_arg, &[])?;
    ensure(
        unbounded_output.status.success(),
        "unbounded graph betweenness must succeed".to_string(),
    )?;
    let unbounded_scores = unbounded_parsed["data"]["scores"]
        .as_array()
        .ok_or_else(|| "unbounded scores must be an array".to_string())?;
    ensure(
        !unbounded_scores.is_empty(),
        "unbounded scores must not be empty after seeding".to_string(),
    )?;
    let rank_one = &unbounded_scores[0];

    let (limited_output, limited_parsed) =
        run_graph_betweenness(&workspace_arg, &["--limit", "1"])?;
    ensure(
        limited_output.status.success(),
        "graph betweenness --limit 1 must succeed".to_string(),
    )?;
    let limited_scores = limited_parsed["data"]["scores"]
        .as_array()
        .ok_or_else(|| "limited scores must be an array".to_string())?;
    ensure(
        limited_scores.len() == 1,
        format!("--limit 1 must emit exactly one row; got {limited_scores:?}"),
    )?;
    let limited_row = &limited_scores[0];
    ensure(
        limited_row["rank"].as_u64() == Some(1),
        format!("--limit 1 row must keep rank=1; got {limited_row}"),
    )?;
    ensure(
        limited_row["memoryId"] == rank_one["memoryId"],
        format!(
            "--limit 1 row must match rank-1 memory from unbounded run; limited={limited_row}, rank1={rank_one}"
        ),
    )?;
    Ok(())
}
