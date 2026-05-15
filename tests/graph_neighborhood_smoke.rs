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
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ee::db::{
    CreateCausalEvidenceInput, CreateMemoryLinkInput, DbConnection, GraphSnapshotStatus,
    GraphSnapshotType, MemoryLinkRelation, MemoryLinkSource,
};
use serde_json::{Value, json};

type TestResult = Result<(), String>;

const GRAPH_CENTRALITY_READ_SURFACE: &str = "graph_centrality_read";

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn pretty_json(value: &Value) -> Result<String, String> {
    let mut rendered =
        serde_json::to_string_pretty(value).map_err(|error| format!("render JSON: {error}"))?;
    rendered.push('\n');
    Ok(rendered)
}

fn elapsed_ms_since(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn trace_graph_centrality_read(phase: &'static str, elapsed_ms: u64, degraded_codes: &[&str]) {
    tracing::info!(
        workspace_id = "graph_neighborhood_smoke_workspace",
        request_id = "graph_neighborhood_smoke_request",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-3usjw.2"),
        surface = GRAPH_CENTRALITY_READ_SURFACE,
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "graph centrality read checkpoint"
    );
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

fn revise_memory(workspace_arg: &str, source_id: &str, content: &str) -> Result<String, String> {
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "memory",
        "revise",
        source_id,
        "--content",
        content,
    ])?;
    if !output.status.success() {
        return Err(format!(
            "memory revise failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let parsed: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    parsed["data"]["new_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "memory revise response missing new_id: {}",
                serde_json::to_string(&parsed).unwrap_or_default()
            )
        })
}

fn normalize_revision_id(memory_id: &str, root: &str, left: &str, join: &str) -> String {
    if memory_id == root {
        "<root>".to_string()
    } else if memory_id == left {
        "<left>".to_string()
    } else if memory_id == join {
        "<join>".to_string()
    } else {
        format!("<other:{memory_id}>")
    }
}

fn normalize_revision_value(value: &Value, root: &str, left: &str, join: &str) -> Value {
    value
        .as_str()
        .map(|memory_id| Value::String(normalize_revision_id(memory_id, root, left, join)))
        .unwrap_or(Value::Null)
}

fn normalize_revision_array(value: &Value, root: &str, left: &str, join: &str) -> Value {
    let normalized = value
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| normalize_revision_value(item, root, left, join))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Value::Array(normalized)
}

fn branch_frontier_impact_golden_view(
    impact: &Value,
    root: &str,
    left: &str,
    join: &str,
) -> Result<Value, String> {
    let frontier_items = impact["frontiers"]
        .as_array()
        .ok_or_else(|| "impactAnalysis.frontiers should be an array".to_string())?;
    let left_frontier = frontier_items
        .iter()
        .find(|item| item["memoryId"].as_str() == Some(left))
        .ok_or_else(|| "frontiers should include the queried left branch".to_string())?;
    let lineage = impact["revisionLineage"]
        .as_array()
        .ok_or_else(|| "impactAnalysis.revisionLineage should be an array".to_string())?;
    let normalized_lineage = lineage
        .iter()
        .map(|item| {
            json!({
                "memoryId": normalize_revision_value(&item["memoryId"], root, left, join),
                "logicalId": normalize_revision_value(&item["logicalId"], root, left, join),
                "depth": item["depth"].clone(),
                "relation": item["relation"].clone(),
                "validFrom": item["validFrom"].clone(),
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "schema": impact["schema"].clone(),
        "memoryId": normalize_revision_value(&impact["memoryId"], root, left, join),
        "impactAnalysis": {
            "immediateDominator": normalize_revision_value(
                &impact["impactAnalysis"]["immediateDominator"],
                root,
                left,
                join,
            ),
            "dominanceFrontier": normalize_revision_array(
                &impact["impactAnalysis"]["dominanceFrontier"],
                root,
                left,
                join,
            ),
            "affectedMemoryCount": impact["impactAnalysis"]["affectedMemoryCount"].clone(),
            "validationStatus": impact["impactAnalysis"]["validationStatus"].clone(),
        },
        "revisionLineage": normalized_lineage,
        "queriedFrontier": {
            "memoryId": normalize_revision_value(&left_frontier["memoryId"], root, left, join),
            "dominanceFrontierSize": left_frontier["dominanceFrontierSize"].clone(),
            "affectedMemoryIds": normalize_revision_array(
                &left_frontier["affectedMemoryIds"],
                root,
                left,
                join,
            ),
            "evidence": left_frontier["evidence"].clone(),
        },
        "degraded": impact["degraded"].clone(),
    }))
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

fn seed_workspace_with_causal_evidence() -> Result<(PathBuf, String, String, String, String), String>
{
    let workspace = unique_workspace("causal-bottlenecks")?;
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

    let failure = remember(&workspace_arg, "Causal failure memory.")?;
    let bridge = remember(&workspace_arg, "Causal bridge memory.")?;
    let root = remember(&workspace_arg, "Causal root memory.")?;

    let database_path = workspace.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    let workspace_id = connection
        .get_memory(&failure)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "failure memory should exist".to_string())?
        .workspace_id;
    connection
        .insert_causal_evidence(
            "cev_00000000000000000000000100",
            &CreateCausalEvidenceInput {
                workspace_id: workspace_id.clone(),
                failure_id: failure.clone(),
                candidate_cause_id: bridge.clone(),
                contribution_score: 0.82,
                evidence_uris: vec!["agent-mail://causal/bridge".to_string()],
                computed_at: Some("2026-05-15T12:30:00Z".to_string()),
                method: "manual".to_string(),
            },
        )
        .map_err(|error| error.to_string())?;
    connection
        .insert_causal_evidence(
            "cev_00000000000000000000000101",
            &CreateCausalEvidenceInput {
                workspace_id,
                failure_id: bridge.clone(),
                candidate_cause_id: root.clone(),
                contribution_score: 0.91,
                evidence_uris: vec!["agent-mail://causal/root".to_string()],
                computed_at: Some("2026-05-15T12:31:00Z".to_string()),
                method: "manual".to_string(),
            },
        )
        .map_err(|error| error.to_string())?;

    Ok((workspace, workspace_arg, failure, bridge, root))
}

#[test]
fn graph_neighborhood_json_envelope_is_stable() -> TestResult {
    let (_workspace, workspace_arg, center, neighbor, link_id) = seed_workspace_with_link()?;
    let trace_started = Instant::now();
    trace_graph_centrality_read("input", 0, &[]);

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "graph",
        "neighborhood",
        center.as_str(),
    ])?;
    trace_graph_centrality_read("dispatch", elapsed_ms_since(trace_started), &[]);
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
    trace_graph_centrality_read("response", elapsed_ms_since(trace_started), &[]);

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

#[cfg(feature = "graph")]
fn refresh_graph_centrality_snapshot(workspace_arg: &str) -> TestResult {
    let output = run_ee(&[
        "--workspace",
        workspace_arg,
        "--json",
        "graph",
        "centrality-refresh",
    ])?;
    ensure(
        output.status.success(),
        format!(
            "graph centrality-refresh should succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "graph centrality-refresh stderr must stay empty; got: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

#[cfg(feature = "graph")]
fn mark_latest_centrality_snapshot_stale(workspace: &PathBuf, workspace_arg: &str) -> TestResult {
    let database_path = workspace.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    let workspace = connection
        .get_workspace_by_path(workspace_arg)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("workspace not found for path {workspace_arg}"))?;
    let snapshot = connection
        .get_latest_graph_snapshot(&workspace.id, GraphSnapshotType::MemoryLinks)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "expected a persisted memory-link graph snapshot".to_string())?;
    ensure(
        connection
            .update_graph_snapshot_status(&snapshot.id, GraphSnapshotStatus::Stale)
            .map_err(|error| error.to_string())?,
        "latest graph snapshot should be marked stale",
    )
}

#[cfg(feature = "graph")]
fn graph_centrality_read_golden_view(
    data: &Value,
    center: &str,
    neighbor: &str,
) -> Result<Value, String> {
    let snapshot = &data["snapshot"];
    let rows = data["rows"]
        .as_array()
        .ok_or_else(|| "graph centrality rows should be an array".to_string())?;
    let mut normalized_rows = Vec::with_capacity(rows.len());
    for row in rows {
        let memory_id = row["memoryId"]
            .as_str()
            .ok_or_else(|| "graph centrality row memoryId should be a string".to_string())?;
        let normalized_id = if memory_id == center {
            "<center>"
        } else if memory_id == neighbor {
            "<neighbor>"
        } else {
            "<other>"
        };
        let score = row["score"]
            .as_f64()
            .ok_or_else(|| "graph centrality row score should be a number".to_string())?;
        normalized_rows.push(json!({
            "rank": row["rank"].clone(),
            "memoryId": normalized_id,
            "scoreClass": if score > 0.0 { "positive" } else { "zero" },
        }));
    }

    Ok(json!({
        "schema": data["schema"].clone(),
        "status": data["status"].clone(),
        "graphType": data["graphType"].clone(),
        "algorithm": data["algorithm"].clone(),
        "snapshotHash": "<snapshot-hash>",
        "computedAt": "<rfc3339>",
        "algorithmVersion": data["algorithmVersion"].clone(),
        "limit": data["limit"].clone(),
        "memoryId": data["memoryId"].clone(),
        "snapshot": {
            "status": snapshot["status"].clone(),
            "sourceGeneration": snapshot["sourceGeneration"].clone(),
            "nodeCount": snapshot["nodeCount"].clone(),
            "edgeCount": snapshot["edgeCount"].clone(),
        },
        "rows": normalized_rows,
        "degraded": data["degraded"].clone(),
    }))
}

#[cfg(feature = "graph")]
#[test]
fn graph_centrality_read_json_toon_and_golden_are_stable() -> TestResult {
    let (_workspace, workspace_arg, center, neighbor, _link_id) = seed_workspace_with_link()?;
    refresh_graph_centrality_snapshot(&workspace_arg)?;

    let first = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "graph",
        "centrality",
        "--limit",
        "2",
    ])?;
    let second = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "graph",
        "centrality",
        "--limit",
        "2",
    ])?;
    ensure(
        first.status.success(),
        format!(
            "graph centrality --json should succeed; stderr: {}",
            String::from_utf8_lossy(&first.stderr)
        ),
    )?;
    ensure(
        first.stderr.is_empty(),
        format!(
            "graph centrality --json stderr must stay empty; got: {}",
            String::from_utf8_lossy(&first.stderr)
        ),
    )?;
    ensure(
        first.stdout == second.stdout,
        "graph centrality --json should be byte-identical across repeated reads",
    )?;

    let parsed: Value = serde_json::from_slice(&first.stdout)
        .map_err(|error| format!("graph centrality stdout must be JSON: {error}"))?;
    ensure(
        parsed["schema"] == Value::String("ee.response.v1".to_string()),
        "graph centrality should use the response envelope",
    )?;
    ensure(
        parsed["success"] == Value::Bool(true),
        "graph centrality should be successful after refresh",
    )?;
    let data = &parsed["data"];
    ensure(
        data["schema"] == Value::String("ee.graph.centrality_read.v1".to_string()),
        "graph centrality data schema should be ee.graph.centrality_read.v1",
    )?;
    ensure(
        data["status"] == Value::String("available".to_string()),
        "graph centrality should report available scores",
    )?;
    ensure(
        data["algorithm"] == Value::String("pagerank".to_string()),
        "default graph centrality algorithm should be pagerank",
    )?;
    let computed_at = data["computedAt"]
        .as_str()
        .ok_or_else(|| "graph centrality should expose computedAt provenance".to_string())?;
    ensure(
        chrono::DateTime::parse_from_rfc3339(computed_at).is_ok(),
        "computedAt should be an RFC3339 UTC timestamp",
    )?;
    ensure(
        data["algorithmVersion"] == Value::String("fnx-algorithms@0.1.0".to_string()),
        "graph centrality should expose the fnx algorithm version",
    )?;
    ensure(
        data["snapshotHash"]
            .as_str()
            .is_some_and(|hash| !hash.is_empty()),
        "graph centrality should expose the source snapshot hash",
    )?;
    ensure(
        data["snapshot"]["status"] == Value::String("valid".to_string()),
        "graph centrality should name the persisted valid snapshot",
    )?;
    ensure(
        data["snapshot"]["sourceGeneration"] == serde_json::json!(1),
        "seeded fixture has one source memory link",
    )?;
    ensure(
        data["snapshot"]["nodeCount"] == serde_json::json!(2),
        "seeded fixture has two graph nodes",
    )?;
    ensure(
        data["snapshot"]["edgeCount"] == serde_json::json!(1),
        "seeded fixture has one graph edge",
    )?;

    let rows = data["rows"]
        .as_array()
        .ok_or_else(|| "graph centrality rows should be an array".to_string())?;
    ensure_eq(rows.len(), 2, "graph centrality row count")?;
    ensure(
        rows.iter()
            .all(|row| row["score"].as_f64().is_some_and(|score| score > 0.0)),
        "all seeded graph centrality scores should be positive",
    )?;
    ensure(
        rows.iter()
            .any(|row| row["memoryId"] == Value::String(center.clone())),
        "graph centrality rows should include the center memory",
    )?;
    ensure(
        rows.iter()
            .any(|row| row["memoryId"] == Value::String(neighbor.clone())),
        "graph centrality rows should include the neighbor memory",
    )?;

    let actual_golden = graph_centrality_read_golden_view(data, &center, &neighbor)?;
    let expected_golden: Value = serde_json::from_str(include_str!("golden/graph_centrality.snap"))
        .map_err(|error| format!("parse graph_centrality golden: {error}"))?;
    let actual_rendered = pretty_json(&actual_golden)?;
    let expected_rendered = pretty_json(&expected_golden)?;
    ensure(
        actual_rendered == expected_rendered,
        format!(
            "graph centrality golden drift\n--- expected\n{expected_rendered}\n--- actual\n{actual_rendered}"
        ),
    )?;

    let toon = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--format",
        "toon",
        "graph",
        "centrality",
        "--limit",
        "2",
    ])?;
    ensure(
        toon.status.success(),
        format!(
            "graph centrality --format toon should succeed; stderr: {}",
            String::from_utf8_lossy(&toon.stderr)
        ),
    )?;
    let toon_stdout = String::from_utf8_lossy(&toon.stdout);
    ensure(
        toon_stdout.contains("schema: ee.response.v1"),
        "TOON output should identify the response schema",
    )?;
    ensure(
        toon_stdout.contains("command: graph centrality"),
        "TOON output should identify the graph centrality command",
    )
}

#[cfg(feature = "graph")]
#[test]
fn graph_centrality_read_reports_unavailable_algorithm() -> TestResult {
    let (_workspace, workspace_arg, _center, _neighbor, _link_id) = seed_workspace_with_link()?;
    refresh_graph_centrality_snapshot(&workspace_arg)?;

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "graph",
        "centrality",
        "--algorithm",
        "authority",
    ])?;
    ensure(
        output.status.success(),
        format!(
            "graph centrality unavailable algorithm should still emit JSON; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "graph centrality unavailable algorithm stderr must stay empty; got: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("graph centrality stdout must be JSON: {error}"))?;
    ensure(
        parsed["success"] == Value::Bool(false),
        "unavailable centrality algorithm should mark the response unsuccessful",
    )?;
    ensure(
        parsed["data"]["status"] == Value::String("algorithm_unavailable".to_string()),
        "unavailable centrality algorithm should report algorithm_unavailable",
    )?;
    ensure(
        parsed["data"]["rows"]
            .as_array()
            .is_some_and(std::vec::Vec::is_empty),
        "unavailable centrality algorithm should not return rows",
    )?;
    let degraded = parsed["degraded"]
        .as_array()
        .ok_or_else(|| "top-level degraded should be an array".to_string())?;
    ensure_eq(
        degraded.len(),
        1,
        "unavailable centrality algorithm degraded count",
    )?;
    ensure(
        degraded[0]["code"] == Value::String("graph_algorithm_unavailable".to_string()),
        "unavailable centrality algorithm should emit graph_algorithm_unavailable",
    )?;
    ensure(
        degraded[0]["repair"]
            == Value::String("ee graph centrality-refresh --algorithm authority".to_string()),
        "unavailable centrality algorithm should include the targeted repair",
    )
}

#[cfg(feature = "graph")]
#[test]
fn graph_centrality_read_require_fresh_exits_six_when_snapshot_is_stale() -> TestResult {
    let (workspace, workspace_arg, _center, _neighbor, _link_id) = seed_workspace_with_link()?;
    refresh_graph_centrality_snapshot(&workspace_arg)?;
    mark_latest_centrality_snapshot_stale(&workspace, &workspace_arg)?;

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "graph",
        "centrality",
        "--require-fresh",
    ])?;
    ensure_eq(
        output.status.code(),
        Some(6),
        "require-fresh stale graph centrality exit code",
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "require-fresh graph centrality stderr must stay empty; got: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("graph centrality stdout must be JSON: {error}"))?;
    ensure(
        parsed["data"]["status"] == Value::String("available".to_string()),
        "stale snapshot should still return available scores",
    )?;
    ensure(
        parsed["degraded"].as_array().is_some_and(|degraded| {
            degraded
                .iter()
                .any(|entry| entry["code"] == Value::String("graph_snapshot_stale".to_string()))
        }),
        "require-fresh stale graph centrality should emit graph_snapshot_stale",
    )
}

#[cfg(feature = "graph")]
#[test]
fn insights_causal_bottlenecks_returns_seeded_bridge_memory() -> TestResult {
    let (_workspace, workspace_arg, _failure, bridge, _root) =
        seed_workspace_with_causal_evidence()?;

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "insights",
        "--section",
        "causalBottlenecks",
    ])?;
    ensure(
        output.status.success(),
        format!(
            "insights causalBottlenecks must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "insights causalBottlenecks stderr must stay empty; got: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let parsed: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    ensure(
        parsed["schema"] == Value::String("ee.response.v1".to_string()),
        "insights uses the response envelope",
    )?;
    ensure(
        parsed["data"]["selectedSection"] == Value::String("causalBottlenecks".to_string()),
        "selectedSection should echo causalBottlenecks",
    )?;
    ensure(
        parsed["data"]["degradedSignals"]
            .as_array()
            .is_some_and(std::vec::Vec::is_empty),
        "workspace-backed causalBottlenecks should not emit empty-workspace degradation",
    )?;

    let items = parsed["data"]["sections"][0]["items"]
        .as_array()
        .ok_or_else(|| "causalBottlenecks items must be an array".to_string())?;
    ensure_eq(
        items.len(),
        1,
        "seeded path has one positive-betweenness bridge",
    )?;
    let item = &items[0];
    ensure(
        item["rank"].as_u64() == Some(1),
        "causal bottleneck rank should be 1",
    )?;
    ensure(
        item["memoryId"] == Value::String(bridge),
        "causal bridge memory should be the top bottleneck",
    )?;
    ensure(
        item["betweenness"]
            .as_f64()
            .is_some_and(|score| score > 0.0),
        "causal bridge must have positive betweenness",
    )?;
    ensure(
        item["evidence"]["schema"]
            == Value::String("ee.graph.causal_evidence_projection.v1".to_string()),
        "causal bottleneck evidence schema should identify the projection",
    )?;
    ensure(
        item["evidence"]["algorithm"]
            == Value::String("betweenness_centrality_directed".to_string()),
        "causal bottleneck evidence algorithm should identify betweenness",
    )?;

    Ok(())
}

#[cfg(feature = "graph")]
#[test]
fn why_causal_explain_returns_seeded_min_cost_path() -> TestResult {
    let (_workspace, workspace_arg, failure, bridge, root) = seed_workspace_with_causal_evidence()?;

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "why",
        failure.as_str(),
        "--causal-explain",
    ])?;
    ensure(
        output.status.success(),
        format!(
            "why --causal-explain must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "why --causal-explain stderr must stay empty; got: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let parsed: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    ensure(
        parsed["schema"] == Value::String("ee.response.v1".to_string()),
        "why uses the response envelope",
    )?;
    let causal = &parsed["data"]["causalExplanation"];
    ensure(
        causal["schema"] == Value::String("ee.why.causal.v1".to_string()),
        "causalExplanation schema should be ee.why.causal.v1",
    )?;
    ensure(
        causal["memoryId"] == Value::String(failure.clone()),
        "causalExplanation memoryId should echo the why target",
    )?;
    ensure(
        causal["degraded"]
            .as_array()
            .is_some_and(std::vec::Vec::is_empty),
        "seeded causal chain should not be degraded",
    )?;
    let paths = causal["paths"]
        .as_array()
        .ok_or_else(|| "causalExplanation paths must be an array".to_string())?;
    ensure_eq(paths.len(), 1, "seeded chain should have one min-cost path")?;
    let path = &paths[0];
    ensure(
        path["sourceMemoryId"] == Value::String(root.clone()),
        "terminal root cause should be the sourceMemoryId",
    )?;
    ensure(
        path["targetMemoryId"] == Value::String(failure.clone()),
        "failure memory should be the targetMemoryId",
    )?;
    ensure(
        path["edgeCount"] == serde_json::json!(2),
        "seeded causal path should have two edges",
    )?;
    let steps = path["steps"]
        .as_array()
        .ok_or_else(|| "causalExplanation path steps must be an array".to_string())?;
    ensure_eq(steps.len(), 2, "seeded causal path steps")?;
    ensure(
        steps[0]["source"] == Value::String(failure.clone())
            && steps[0]["target"] == Value::String(bridge.clone()),
        "first causal step should connect failure to bridge",
    )?;
    ensure(
        steps[1]["source"] == Value::String(bridge) && steps[1]["target"] == Value::String(root),
        "second causal step should connect bridge to root",
    )?;

    Ok(())
}

#[cfg(feature = "graph")]
#[test]
fn memory_revise_and_why_emit_revision_impact_blocks() -> TestResult {
    let workspace = unique_workspace("revision-lineage")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path should be utf8".to_string())?
        .to_string();
    let root = remember(&workspace_arg, "Revision impact root memory.")?;
    let revised_id = revise_memory(&workspace_arg, &root, "Revision impact child memory.")?;

    let preview = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "memory",
        "revise",
        revised_id.as_str(),
        "--content",
        "Revision impact preview memory.",
        "--dry-run",
    ])?;
    ensure(
        preview.status.success(),
        format!(
            "memory revise --dry-run should succeed; stderr: {}",
            String::from_utf8_lossy(&preview.stderr)
        ),
    )?;
    let preview_json: Value =
        serde_json::from_slice(&preview.stdout).map_err(|error| error.to_string())?;
    let impact = &preview_json["data"]["impactAnalysis"];
    ensure(
        impact["schema"] == Value::String("ee.memory.impact_analysis.v1".to_string()),
        "memory revise dry-run should include impactAnalysis schema",
    )?;
    ensure(
        impact["memoryId"] == Value::String(revised_id.clone()),
        "impactAnalysis memoryId should echo the revised memory",
    )?;
    ensure(
        impact["impactAnalysis"]["validationStatus"].as_str() == Some("valid"),
        "revised memory with an ancestor should produce valid impact analysis",
    )?;

    let why = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "why",
        revised_id.as_str(),
    ])?;
    ensure(
        why.status.success(),
        format!(
            "why should succeed for revised memory; stderr: {}",
            String::from_utf8_lossy(&why.stderr)
        ),
    )?;
    let why_json: Value = serde_json::from_slice(&why.stdout).map_err(|error| error.to_string())?;
    let lineage = &why_json["data"]["revisionLineage"];
    ensure(
        lineage["sourceSchema"] == Value::String("ee.memory.impact_analysis.v1".to_string()),
        "why revisionLineage should cite the impact-analysis source schema",
    )?;
    ensure(
        lineage["immediateDominator"] == Value::String(root.clone()),
        "why revisionLineage should report the root as immediate dominator",
    )?;
    ensure(
        lineage["ancestorsAtDepth"]["0"][0] == Value::String(revised_id),
        "why revisionLineage depth 0 should be the queried revision",
    )?;
    ensure(
        lineage["ancestorsAtDepth"]["1"][0] == Value::String(root),
        "why revisionLineage depth 1 should be the previous revision",
    )
}

#[cfg(feature = "graph")]
#[test]
fn memory_revise_dry_run_impact_analysis_reports_branch_frontier() -> TestResult {
    let workspace = unique_workspace("revision-frontier-branch")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path should be utf8".to_string())?
        .to_string();
    let root = remember(&workspace_arg, "Revision frontier root memory.")?;
    let left = revise_memory(&workspace_arg, &root, "Revision frontier left revision.")?;
    let right = remember(&workspace_arg, "Revision frontier right branch memory.")?;
    let join = remember(&workspace_arg, "Revision frontier join memory.")?;

    let database_path = workspace.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    connection
        .insert_memory_link(
            "link_00000000000000000000000130",
            &CreateMemoryLinkInput {
                src_memory_id: root.clone(),
                dst_memory_id: right.clone(),
                relation: MemoryLinkRelation::DerivedFrom,
                weight: 0.86_f32,
                confidence: 0.91_f32,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: Some("2026-05-15T12:00:00Z".to_string()),
                source: MemoryLinkSource::Human,
                created_by: Some("g7-revision-frontier-branch".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    connection
        .insert_memory_link(
            "link_00000000000000000000000131",
            &CreateMemoryLinkInput {
                src_memory_id: left.clone(),
                dst_memory_id: join.clone(),
                relation: MemoryLinkRelation::DerivedFrom,
                weight: 0.82_f32,
                confidence: 0.88_f32,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: Some("2026-05-15T12:01:00Z".to_string()),
                source: MemoryLinkSource::Human,
                created_by: Some("g7-revision-frontier-branch".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    connection
        .insert_memory_link(
            "link_00000000000000000000000132",
            &CreateMemoryLinkInput {
                src_memory_id: right.clone(),
                dst_memory_id: join.clone(),
                relation: MemoryLinkRelation::DerivedFrom,
                weight: 0.79_f32,
                confidence: 0.87_f32,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: Some("2026-05-15T12:02:00Z".to_string()),
                source: MemoryLinkSource::Human,
                created_by: Some("g7-revision-frontier-branch".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())?;

    let preview = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "memory",
        "revise",
        left.as_str(),
        "--content",
        "Revision frontier dry-run preview.",
        "--dry-run",
    ])?;
    ensure(
        preview.status.success(),
        format!(
            "memory revise --dry-run should succeed for branched revision DAG; stderr: {}",
            String::from_utf8_lossy(&preview.stderr)
        ),
    )?;
    let preview_json: Value =
        serde_json::from_slice(&preview.stdout).map_err(|error| error.to_string())?;
    let impact = &preview_json["data"]["impactAnalysis"];
    ensure(
        impact["schema"] == Value::String("ee.memory.impact_analysis.v1".to_string()),
        "branch dry-run should include impactAnalysis schema",
    )?;
    ensure(
        impact["memoryId"] == Value::String(left.clone()),
        "branch impactAnalysis memoryId should echo the requested memory",
    )?;
    ensure(
        impact["impactAnalysis"]["validationStatus"].as_str() == Some("valid"),
        "branched revision DAG should report valid impact analysis",
    )?;
    ensure(
        impact["impactAnalysis"]["immediateDominator"] == Value::String(root.clone()),
        "branch impactAnalysis should identify the root as immediate dominator",
    )?;
    ensure(
        impact["impactAnalysis"]["dominanceFrontier"][0] == Value::String(join.clone()),
        "left branch frontier should include the join memory",
    )?;
    ensure(
        impact["revisionLineage"][0]["memoryId"] == Value::String(left.clone()),
        "revisionLineage depth 0 should be the queried left branch",
    )?;
    ensure(
        impact["revisionLineage"][1]["memoryId"] == Value::String(root.clone()),
        "revisionLineage depth 1 should be the root revision",
    )?;
    let frontier_items = impact["frontiers"]
        .as_array()
        .ok_or_else(|| "impactAnalysis.frontiers should be an array".to_string())?;
    let left_frontier = frontier_items
        .iter()
        .find(|item| item["memoryId"].as_str() == Some(left.as_str()))
        .ok_or_else(|| "frontiers should include the queried left branch".to_string())?;
    ensure(
        left_frontier["affectedMemoryIds"][0] == Value::String(join.clone()),
        "queried left branch frontier item should name the join memory",
    )?;

    let actual_golden = branch_frontier_impact_golden_view(impact, &root, &left, &join)?;
    let expected_golden: Value =
        serde_json::from_str(include_str!("golden/graph-dominance-impact.snap"))
            .map_err(|error| format!("parse graph-dominance-impact golden: {error}"))?;
    let actual_rendered = pretty_json(&actual_golden)?;
    let expected_rendered = pretty_json(&expected_golden)?;
    ensure(
        actual_rendered == expected_rendered,
        format!(
            "branch/frontier impact golden drift\n--- expected\n{expected_rendered}\n--- actual\n{actual_rendered}"
        ),
    )?;

    for run_index in 2..=3 {
        let repeat_preview = run_ee(&[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "memory",
            "revise",
            left.as_str(),
            "--content",
            "Revision frontier dry-run preview.",
            "--dry-run",
        ])?;
        ensure(
            repeat_preview.status.success(),
            format!(
                "memory revise --dry-run repeat {run_index} should succeed; stderr: {}",
                String::from_utf8_lossy(&repeat_preview.stderr)
            ),
        )?;
        let repeat_json: Value =
            serde_json::from_slice(&repeat_preview.stdout).map_err(|error| error.to_string())?;
        let repeat_impact = &repeat_json["data"]["impactAnalysis"];
        let repeat_golden = branch_frontier_impact_golden_view(repeat_impact, &root, &left, &join)?;
        let repeat_rendered = pretty_json(&repeat_golden)?;
        ensure(
            repeat_rendered == actual_rendered,
            format!(
                "branch/frontier normalized impact drift between run 1 and run {run_index}\n--- run 1\n{actual_rendered}\n--- run {run_index}\n{repeat_rendered}"
            ),
        )?;
    }

    Ok(())
}

#[cfg(feature = "graph")]
#[test]
fn memory_revise_dry_run_impact_analysis_reports_singleton_revision_gap() -> TestResult {
    let workspace = unique_workspace("revision-impact-singleton")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path should be utf8".to_string())?
        .to_string();
    let root = remember(&workspace_arg, "Revision singleton root memory.")?;

    let preview = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "memory",
        "revise",
        root.as_str(),
        "--content",
        "Revision singleton preview memory.",
        "--dry-run",
    ])?;
    ensure(
        preview.status.success(),
        format!(
            "memory revise --dry-run should succeed for singleton revision; stderr: {}",
            String::from_utf8_lossy(&preview.stderr)
        ),
    )?;
    let preview_json: Value =
        serde_json::from_slice(&preview.stdout).map_err(|error| error.to_string())?;
    let impact = &preview_json["data"]["impactAnalysis"];
    ensure(
        impact["schema"] == Value::String("ee.memory.impact_analysis.v1".to_string()),
        "singleton dry-run should include impactAnalysis schema",
    )?;
    ensure(
        impact["memoryId"] == Value::String(root),
        "singleton impactAnalysis memoryId should echo the requested memory",
    )?;
    ensure(
        impact["impactAnalysis"]["validationStatus"].as_str() == Some("unavailable"),
        "singleton revision DAG should report unavailable validation status",
    )?;
    ensure(
        impact["impactAnalysis"]["immediateDominator"].is_null(),
        "singleton revision DAG should not report an immediate dominator",
    )
}

#[cfg(feature = "graph")]
#[test]
fn why_revision_lineage_reports_ancestor_depths_for_revision_chain() -> TestResult {
    let workspace = unique_workspace("revision-lineage-depths")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path should be utf8".to_string())?
        .to_string();
    let root = remember(&workspace_arg, "Revision lineage root memory.")?;
    let child = revise_memory(&workspace_arg, &root, "Revision lineage child memory.")?;
    let grandchild = revise_memory(
        &workspace_arg,
        &child,
        "Revision lineage grandchild memory.",
    )?;

    let why = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "why",
        grandchild.as_str(),
    ])?;
    ensure(
        why.status.success(),
        format!(
            "why should succeed for deep revised memory; stderr: {}",
            String::from_utf8_lossy(&why.stderr)
        ),
    )?;
    let why_json: Value = serde_json::from_slice(&why.stdout).map_err(|error| error.to_string())?;
    let lineage = &why_json["data"]["revisionLineage"];
    ensure(
        lineage["sourceSchema"] == Value::String("ee.memory.impact_analysis.v1".to_string()),
        "why revisionLineage should cite the impact-analysis source schema",
    )?;
    ensure(
        lineage["memoryId"] == Value::String(grandchild.clone()),
        "why revisionLineage memoryId should echo the queried revision",
    )?;
    ensure(
        lineage["immediateDominator"] == Value::String(child.clone()),
        "why revisionLineage should report the direct predecessor as immediate dominator",
    )?;
    ensure(
        lineage["rootMemoryId"] == Value::String(root.clone()),
        "why revisionLineage should report the chain root",
    )?;
    ensure(
        lineage["validationStatus"].as_str() == Some("valid"),
        "multi-revision chain should have valid lineage analysis",
    )?;
    ensure(
        lineage["ancestorsAtDepth"]["0"][0] == Value::String(grandchild),
        "depth 0 should contain the queried revision",
    )?;
    ensure(
        lineage["ancestorsAtDepth"]["1"][0] == Value::String(child),
        "depth 1 should contain the direct predecessor",
    )?;
    ensure(
        lineage["ancestorsAtDepth"]["2"][0] == Value::String(root),
        "depth 2 should contain the root revision",
    )
}

#[cfg(feature = "graph")]
#[test]
fn proximity_json_reports_min_cut_for_seeded_memory_pair() -> TestResult {
    let (_workspace, workspace_arg, center, neighbor, _link_id) = seed_workspace_with_link()?;

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "proximity",
        center.as_str(),
        neighbor.as_str(),
    ])?;
    ensure(
        output.status.success(),
        format!(
            "proximity --json must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "proximity --json stderr must stay empty; got: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let parsed: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    ensure(
        parsed["schema"] == Value::String("ee.proximity.v1".to_string()),
        "schema must be ee.proximity.v1",
    )?;
    ensure(
        parsed["memoryA"] == Value::String(center.clone()),
        "memoryA must echo the left memory",
    )?;
    ensure(
        parsed["memoryB"] == Value::String(neighbor.clone()),
        "memoryB must echo the right memory",
    )?;
    let min_cut = parsed["minCut"]
        .as_f64()
        .ok_or_else(|| "minCut must be numeric for linked memories".to_string())?;
    ensure(
        (0.90..=0.92).contains(&min_cut),
        format!("minCut should reflect the seeded link weight; got {min_cut}"),
    )?;
    ensure(
        parsed["interpretation"] == Value::String("weak".to_string()),
        "0.91 min-cut is interpreted as weak",
    )?;
    ensure(
        parsed["treePath"] == serde_json::json!([center.clone(), neighbor.clone()]),
        "treePath should be the direct seeded pair",
    )?;
    ensure(
        parsed["degraded"]
            .as_array()
            .is_some_and(std::vec::Vec::is_empty),
        "linked proximity should not be degraded",
    )?;

    let first_stdout = output.stdout.clone();
    for run_index in 2..=3 {
        let repeat = run_ee(&[
            "--workspace",
            workspace_arg.as_str(),
            "--json",
            "proximity",
            center.as_str(),
            neighbor.as_str(),
        ])?;
        ensure(
            repeat.status.success(),
            format!(
                "proximity --json repeat {run_index} must succeed; stderr: {}",
                String::from_utf8_lossy(&repeat.stderr)
            ),
        )?;
        ensure(
            repeat.stderr.is_empty(),
            format!(
                "proximity --json repeat {run_index} stderr must stay empty; got: {}",
                String::from_utf8_lossy(&repeat.stderr)
            ),
        )?;
        ensure(
            repeat.stdout == first_stdout,
            format!(
                "proximity --json output must be byte-identical between run 1 and run {run_index}"
            ),
        )?;
    }

    Ok(())
}

#[cfg(feature = "graph")]
#[test]
fn insights_proximity_hotspots_returns_seeded_min_cut_pair() -> TestResult {
    let (_workspace, workspace_arg, center, neighbor, _link_id) = seed_workspace_with_link()?;

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "insights",
        "--section",
        "proximityHotspots",
    ])?;
    ensure(
        output.status.success(),
        format!(
            "insights proximityHotspots must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        output.stderr.is_empty(),
        format!(
            "insights proximityHotspots stderr must stay empty; got: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let parsed: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    ensure(
        parsed["schema"] == Value::String("ee.response.v1".to_string()),
        "insights uses the response envelope",
    )?;
    ensure(
        parsed["data"]["selectedSection"] == Value::String("proximityHotspots".to_string()),
        "selectedSection should echo proximityHotspots",
    )?;
    ensure(
        parsed["data"]["degradedSignals"]
            .as_array()
            .is_some_and(std::vec::Vec::is_empty),
        "workspace-backed proximityHotspots should not emit empty-workspace degradation",
    )?;
    let items = parsed["data"]["sections"][0]["items"]
        .as_array()
        .ok_or_else(|| "proximityHotspots items must be an array".to_string())?;
    ensure_eq(
        items.len(),
        1,
        "seeded two-node workspace has one proximity pair",
    )?;
    let item = &items[0];
    ensure(item["rank"].as_u64() == Some(1), "hotspot rank should be 1")?;
    ensure(
        item["memoryA"] == Value::String(center.clone()),
        "memoryA should be the deterministic first endpoint",
    )?;
    ensure(
        item["memoryB"] == Value::String(neighbor.clone()),
        "memoryB should be the deterministic second endpoint",
    )?;
    let min_cut = item["minCut"]
        .as_f64()
        .ok_or_else(|| "hotspot minCut must be numeric".to_string())?;
    ensure(
        (0.90..=0.92).contains(&min_cut),
        format!("hotspot minCut should reflect seeded link weight; got {min_cut}"),
    )?;
    ensure(
        item["interpretation"] == Value::String("weak".to_string()),
        "0.91 min-cut is interpreted as weak",
    )?;
    ensure(
        item["treePath"] == serde_json::json!([center, neighbor]),
        "treePath should be the direct seeded pair",
    )?;
    ensure(
        item["evidence"]["schema"] == Value::String("ee.proximity.v1".to_string()),
        "hotspot evidence schema should be ee.proximity.v1",
    )?;
    ensure(
        item["evidence"]["algorithm"] == Value::String("gomory_hu_tree".to_string()),
        "hotspot evidence algorithm should be gomory_hu_tree",
    )?;
    Ok(())
}

#[cfg(feature = "graph")]
#[test]
fn proximity_human_renderer_includes_pair_and_interpretation() -> TestResult {
    let (_workspace, workspace_arg, center, neighbor, _link_id) = seed_workspace_with_link()?;

    let output = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "proximity",
        center.as_str(),
        neighbor.as_str(),
    ])?;
    ensure(
        output.status.success(),
        format!(
            "proximity human render must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        stdout.contains(&format!("Proximity between {center} and {neighbor}")),
        format!("human output must mention the pair; got: {stdout}"),
    )?;
    ensure(
        stdout.contains("Interpretation: weak"),
        format!("human output must include interpretation; got: {stdout}"),
    )?;
    ensure(
        stdout.contains("Tree path:"),
        format!("human output must include tree path; got: {stdout}"),
    )?;
    Ok(())
}

#[cfg(feature = "graph")]
#[test]
fn status_json_exposes_graph_result_cache_hit_rate_metric() -> TestResult {
    let (_workspace, workspace_arg, _center, _neighbor, _link_id) = seed_workspace_with_link()?;

    let output = run_ee(&["--workspace", workspace_arg.as_str(), "--json", "status"])?;
    ensure(
        output.status.success(),
        format!(
            "status --json must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let parsed: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    let cache = parsed
        .pointer("/data/graphCompute/resultCache")
        .ok_or_else(|| "status JSON missing graphCompute.resultCache".to_owned())?;
    ensure(
        cache["cacheHitRate"].is_number() || cache["cacheHitRate"].is_null(),
        format!("cacheHitRate must be numeric or null; got {cache}"),
    )?;
    ensure(
        cache["cachedResultCount"].is_u64(),
        format!("cachedResultCount must be an unsigned count; got {cache}"),
    )?;
    Ok(())
}

#[cfg(feature = "graph")]
#[test]
fn graph_link_path_and_why_outputs_compose_for_real_memory_edges() -> TestResult {
    let workspace = unique_workspace("link-path")?;
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

    let source = remember(
        &workspace_arg,
        "Graph link path source alpha evidence memory.",
    )?;
    let bridge = remember(
        &workspace_arg,
        "Graph link path bridge memory with linked provenance.",
    )?;
    let target = remember(
        &workspace_arg,
        "Graph link path target memory refined by the bridge.",
    )?;

    let database_path = workspace.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    connection
        .insert_memory_link(
            "link_00000000000000000000000110",
            &CreateMemoryLinkInput {
                src_memory_id: source.clone(),
                dst_memory_id: bridge.clone(),
                relation: MemoryLinkRelation::Supports,
                weight: 0.93_f32,
                confidence: 0.89_f32,
                directed: true,
                evidence_count: 3,
                last_reinforced_at: Some("2026-05-06T04:00:00Z".to_string()),
                source: MemoryLinkSource::Human,
                created_by: Some("graph-link-path-e2e".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    connection
        .insert_memory_link(
            "link_00000000000000000000000111",
            &CreateMemoryLinkInput {
                src_memory_id: bridge.clone(),
                dst_memory_id: target.clone(),
                relation: MemoryLinkRelation::Supersedes,
                weight: 0.81_f32,
                confidence: 0.77_f32,
                directed: true,
                evidence_count: 2,
                last_reinforced_at: Some("2026-05-06T04:01:00Z".to_string()),
                source: MemoryLinkSource::Human,
                created_by: Some("graph-link-path-e2e".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())?;

    let neighborhood = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "graph",
        "neighborhood",
        bridge.as_str(),
        "--direction",
        "both",
    ])?;
    ensure(
        neighborhood.status.success(),
        format!(
            "graph neighborhood must succeed; stderr: {}",
            String::from_utf8_lossy(&neighborhood.stderr)
        ),
    )?;
    ensure(
        neighborhood.stderr.is_empty(),
        format!(
            "graph neighborhood stderr must stay empty; got: {}",
            String::from_utf8_lossy(&neighborhood.stderr)
        ),
    )?;
    let neighborhood_json: Value =
        serde_json::from_slice(&neighborhood.stdout).map_err(|error| error.to_string())?;
    let edges = neighborhood_json["data"]["edges"]
        .as_array()
        .ok_or_else(|| "neighborhood edges must be an array".to_string())?;
    ensure_eq(edges.len(), 2, "two incident edges around bridge")?;
    ensure(
        edges.iter().any(|edge| {
            edge["linkId"] == "link_00000000000000000000000110"
                && edge["neighborMemoryId"] == source
                && edge["relativeDirection"] == "incoming"
                && edge["relation"] == "supports"
        }),
        "bridge neighborhood must include incoming support from source",
    )?;
    ensure(
        edges.iter().any(|edge| {
            edge["linkId"] == "link_00000000000000000000000111"
                && edge["neighborMemoryId"] == target
                && edge["relativeDirection"] == "outgoing"
                && edge["relation"] == "supersedes"
        }),
        "bridge neighborhood must include outgoing supersedes edge to target",
    )?;

    let path = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "graph",
        "explain-link",
        source.as_str(),
        target.as_str(),
    ])?;
    ensure(
        path.status.success(),
        format!(
            "graph explain-link must succeed; stderr: {}",
            String::from_utf8_lossy(&path.stderr)
        ),
    )?;
    ensure(
        path.stderr.is_empty(),
        format!(
            "graph explain-link stderr must stay empty; got: {}",
            String::from_utf8_lossy(&path.stderr)
        ),
    )?;
    let path_json: Value =
        serde_json::from_slice(&path.stdout).map_err(|error| error.to_string())?;
    ensure(
        path_json["data"]["status"] == "path_found",
        "source to target should be connected by a two-hop path",
    )?;
    ensure(
        path_json["data"]["path"] == serde_json::json!([source, bridge, target]),
        "graph path should traverse the bridge memory",
    )?;
    ensure(
        path_json["data"]["pathLength"] == serde_json::json!(2),
        "graph path length should be two edges",
    )?;

    let why = run_ee(&[
        "--workspace",
        workspace_arg.as_str(),
        "--json",
        "why",
        bridge.as_str(),
    ])?;
    ensure(
        why.status.success(),
        format!(
            "why must succeed; stderr: {}",
            String::from_utf8_lossy(&why.stderr)
        ),
    )?;
    ensure(
        why.stderr.is_empty(),
        format!(
            "why stderr must stay empty; got: {}",
            String::from_utf8_lossy(&why.stderr)
        ),
    )?;
    let why_json: Value = serde_json::from_slice(&why.stdout).map_err(|error| error.to_string())?;
    let why_links = why_json["data"]["links"]
        .as_array()
        .ok_or_else(|| "why links must be an array".to_string())?;
    ensure(
        why_links.iter().any(|link| {
            link["linkId"] == "link_00000000000000000000000110"
                && link["linkedMemoryId"] == source
                && link["direction"] == "incoming"
                && link["relation"] == "supports"
        }),
        "why must explain the incoming support link",
    )?;
    ensure(
        why_links.iter().any(|link| {
            link["linkId"] == "link_00000000000000000000000111"
                && link["linkedMemoryId"] == target
                && link["direction"] == "outgoing"
                && link["relation"] == "supersedes"
        }),
        "why must explain the outgoing supersedes link",
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
        parsed["schema"] == Value::String("ee.error.v2".to_string()),
        "error envelope schema must be ee.error.v2",
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
