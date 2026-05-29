//! bd-177gh: real-binary pin test for `ee graph hits`.
//!
//! Mirrors the runtime shape of `graph_neighborhood_smoke.rs` and the
//! sibling `e2e_graph_pagerank.rs` (bd-2wr4f) / `e2e_graph_betweenness.rs`
//! (bd-2r363) but exercises CLI angles the existing graph tests do not
//! assert against the real `ee` binary, with extra coverage for the
//! hits-specific surface (split hubs/authorities maps, reportSchema, and
//! the per-algorithm degraded array shape emitted by
//! crate::graph::hits::compute_hits_report):
//!
//! * `--limit 0` surfaces the documented `DomainError::Usage` repair text
//! * `--min-weight` / `--min-confidence` outside `[0.0, 1.0]` surface the
//!   documented threshold usage error
//! * a fresh workspace with no links returns `status=computed` with empty
//!   `hubs` and `authorities` arrays, `command="graph hits"`, and a
//!   well-formed surface envelope
//! * a directed two-edge path src -> mid -> dst surfaces hubs ranking src
//!   first (the only node with outbound shortest paths through mid) and
//!   authorities ranking dst first (the only node receiving inbound
//!   paths), proving real HITS evaluation
//! * `--limit 1` truncates BOTH hubs and authorities deterministically:
//!   each surviving row matches the rank-1 row from the unbounded run

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
        .join("ee-graph-hits-pin")
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
                created_by: Some("e2e-graph-hits-pin".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn run_graph_hits(workspace_arg: &str, extra: &[&str]) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec!["--workspace", workspace_arg, "--json", "graph", "hits"];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("graph hits stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn assert_envelope_shape(data: &Value) -> TestResult {
    ensure(
        data["schema"].as_str() == Some("ee.graph.algorithm.v1"),
        format!("schema must be ee.graph.algorithm.v1; got {data}"),
    )?;
    ensure(
        data["command"].as_str() == Some("graph hits"),
        format!("command must be graph hits; got {data}"),
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
        data["hubs"].is_array(),
        format!("hubs must be an array; got {data}"),
    )?;
    ensure(
        data["authorities"].is_array(),
        format!("authorities must be an array; got {data}"),
    )?;
    ensure(
        data["degraded"].is_array(),
        format!("degraded must be an array; got {data}"),
    )?;
    ensure(
        data["reportSchema"].is_string(),
        format!("reportSchema must be a string; got {data}"),
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

    let src = remember(&workspace_arg, "Pin-test hits src node.")?;
    let mid = remember(&workspace_arg, "Pin-test hits mid node.")?;
    let dst = remember(&workspace_arg, "Pin-test hits dst node.")?;

    let database_path = workspace.join(".ee").join("ee.db");
    insert_link(
        &database_path,
        "link_00000000000000000000hits0001",
        &src,
        &mid,
        0.9,
        0.8,
    )?;
    insert_link(
        &database_path,
        "link_00000000000000000000hits0002",
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

fn assert_score_array_well_formed(scores: &[Value], label: &str) -> TestResult {
    for (index, score) in scores.iter().enumerate() {
        let expected_rank = u64::try_from(index + 1).expect("rank fits u64");
        ensure(
            score["rank"].as_u64() == Some(expected_rank),
            format!("{label}[{index}].rank must be {expected_rank}; got {score}"),
        )?;
        ensure(
            score["memoryId"].is_string(),
            format!("{label}[{index}].memoryId must be a string; got {score}"),
        )?;
        ensure(
            score["score"].is_number(),
            format!("{label}[{index}].score must be numeric; got {score}"),
        )?;
    }
    // descending score, tie-break ascending memoryId — the deterministic
    // order graph_score_map_json constructs.
    for window in scores.windows(2) {
        let left = window[0]["score"].as_f64().unwrap_or(0.0);
        let right = window[1]["score"].as_f64().unwrap_or(0.0);
        if (left - right).abs() < f64::EPSILON {
            let left_id = window[0]["memoryId"].as_str().unwrap_or("");
            let right_id = window[1]["memoryId"].as_str().unwrap_or("");
            ensure(
                left_id <= right_id,
                format!(
                    "{label}: tied scores must break on ascending memoryId; got {left_id} then {right_id}"
                ),
            )?;
        } else {
            ensure(
                left >= right,
                format!("{label}: scores must be sorted descending; got {left} before {right}"),
            )?;
        }
    }
    Ok(())
}

#[test]
fn graph_hits_rejects_zero_limit_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-zero-limit")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_hits(&workspace_arg, &["--limit", "0"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph hits --limit 0 must fail; stdout: {}",
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
fn graph_hits_rejects_min_weight_out_of_range_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-weight")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_hits(&workspace_arg, &["--min-weight", "2.0"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph hits --min-weight 2.0 must fail; stdout: {}",
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
fn graph_hits_rejects_min_confidence_out_of_range_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-confidence")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_hits(&workspace_arg, &["--min-confidence", "-0.5"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph hits --min-confidence -0.5 must fail; stdout: {}",
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
fn graph_hits_returns_empty_score_maps_on_fresh_workspace() -> TestResult {
    let workspace = unique_workspace("empty")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_hits(&workspace_arg, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph hits on a fresh workspace must exit zero; stderr: {}",
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
        data["hubs"].as_array().map(Vec::len) == Some(0),
        format!("hubs must be empty with no links; got {data}"),
    )?;
    ensure(
        data["authorities"].as_array().map(Vec::len) == Some(0),
        format!("authorities must be empty with no links; got {data}"),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        stdout.lines().filter(|line| !line.is_empty()).count() == 1,
        format!("--json stdout must be a single line; got {stdout}"),
    )?;
    Ok(())
}

#[test]
fn graph_hits_ranks_src_as_top_hub_and_dst_as_top_authority() -> TestResult {
    let (_workspace, workspace_arg, src, mid, dst) = seed_two_edge_path()?;

    let (output, parsed) = run_graph_hits(&workspace_arg, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph hits with seeded links must succeed; stderr: {}",
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

    let hubs = data["hubs"]
        .as_array()
        .ok_or_else(|| format!("hubs must be an array; got {data}"))?;
    let authorities = data["authorities"]
        .as_array()
        .ok_or_else(|| format!("authorities must be an array; got {data}"))?;
    ensure(
        hubs.len() == 3,
        format!("hubs must contain three rows for three nodes; got {hubs:?}"),
    )?;
    ensure(
        authorities.len() == 3,
        format!("authorities must contain three rows for three nodes; got {authorities:?}"),
    )?;
    assert_score_array_well_formed(hubs, "hubs")?;
    assert_score_array_well_formed(authorities, "authorities")?;

    // Classic HITS shape on a directed src -> mid -> dst path: src is the
    // pure hub (only node with outbound edges that lead to authorities)
    // and dst is the pure authority (only node receiving inbound edges
    // from hubs). This proves we are actually evaluating HITS, not a
    // stub returning uniform or swapped scores.
    let hubs_ids = hubs
        .iter()
        .filter_map(|score| score["memoryId"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let authorities_ids = authorities
        .iter()
        .filter_map(|score| score["memoryId"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    for expected in [&src, &mid, &dst] {
        ensure(
            hubs_ids.iter().any(|id| id == expected),
            format!("hubs must include {expected}; got {hubs_ids:?}"),
        )?;
        ensure(
            authorities_ids.iter().any(|id| id == expected),
            format!("authorities must include {expected}; got {authorities_ids:?}"),
        )?;
    }

    ensure(
        hubs[0]["memoryId"].as_str() == Some(src.as_str()),
        format!(
            "rank-1 hub must be src={src} on a src->mid->dst path; got {}",
            hubs[0]
        ),
    )?;
    ensure(
        authorities[0]["memoryId"].as_str() == Some(dst.as_str()),
        format!(
            "rank-1 authority must be dst={dst} on a src->mid->dst path; got {}",
            authorities[0]
        ),
    )?;
    Ok(())
}

#[test]
fn graph_hits_limit_truncates_hubs_and_authorities_deterministically() -> TestResult {
    let (_workspace, workspace_arg, _src, _mid, _dst) = seed_two_edge_path()?;

    let (unbounded_output, unbounded_parsed) = run_graph_hits(&workspace_arg, &[])?;
    ensure(
        unbounded_output.status.success(),
        "unbounded graph hits must succeed".to_string(),
    )?;
    let unbounded_hubs = unbounded_parsed["data"]["hubs"]
        .as_array()
        .ok_or_else(|| "unbounded hubs must be an array".to_string())?;
    let unbounded_authorities = unbounded_parsed["data"]["authorities"]
        .as_array()
        .ok_or_else(|| "unbounded authorities must be an array".to_string())?;
    ensure(
        !unbounded_hubs.is_empty() && !unbounded_authorities.is_empty(),
        "unbounded hubs and authorities must not be empty after seeding".to_string(),
    )?;
    let hub_rank_one = &unbounded_hubs[0];
    let authority_rank_one = &unbounded_authorities[0];

    let (limited_output, limited_parsed) = run_graph_hits(&workspace_arg, &["--limit", "1"])?;
    ensure(
        limited_output.status.success(),
        "graph hits --limit 1 must succeed".to_string(),
    )?;
    let limited_hubs = limited_parsed["data"]["hubs"]
        .as_array()
        .ok_or_else(|| "limited hubs must be an array".to_string())?;
    let limited_authorities = limited_parsed["data"]["authorities"]
        .as_array()
        .ok_or_else(|| "limited authorities must be an array".to_string())?;
    ensure(
        limited_hubs.len() == 1,
        format!("--limit 1 must emit exactly one hub row; got {limited_hubs:?}"),
    )?;
    ensure(
        limited_authorities.len() == 1,
        format!("--limit 1 must emit exactly one authority row; got {limited_authorities:?}"),
    )?;
    let limited_hub = &limited_hubs[0];
    let limited_authority = &limited_authorities[0];
    ensure(
        limited_hub["rank"].as_u64() == Some(1),
        format!("--limit 1 hub row must keep rank=1; got {limited_hub}"),
    )?;
    ensure(
        limited_authority["rank"].as_u64() == Some(1),
        format!("--limit 1 authority row must keep rank=1; got {limited_authority}"),
    )?;
    ensure(
        limited_hub["memoryId"] == hub_rank_one["memoryId"],
        format!(
            "--limit 1 hub row must match rank-1 from unbounded run; limited={limited_hub}, rank1={hub_rank_one}"
        ),
    )?;
    ensure(
        limited_authority["memoryId"] == authority_rank_one["memoryId"],
        format!(
            "--limit 1 authority row must match rank-1 from unbounded run; limited={limited_authority}, rank1={authority_rank_one}"
        ),
    )?;
    Ok(())
}
