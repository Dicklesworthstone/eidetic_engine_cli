//! bd-1duvm: real-binary pin test for `ee graph centrality` and
//! `ee graph centrality-refresh`.
//!
//! Mirrors the runtime shape of `graph_neighborhood_smoke.rs` but exercises
//! angles the neighborhood smoke does not assert directly:
//!
//! * `--limit 0` parse-time usage error
//! * read against a freshly-init'd workspace with no snapshot
//! * `centrality-refresh --dry-run` does not persist a snapshot
//! * read with `--algorithm betweenness` returns rows after a regular refresh
//! * read with `--memory-id` filter narrows rows to a single id
//! * read with `--limit 1` truncates the row set to one entry

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
        .join("ee-graph-centrality-pin")
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
                created_by: Some("e2e-graph-centrality-pin".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn read_centrality(workspace_arg: &str, extra: &[&str]) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec![
        "--workspace",
        workspace_arg,
        "--json",
        "graph",
        "centrality",
    ];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("graph centrality stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn refresh_centrality(workspace_arg: &str, extra: &[&str]) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec![
        "--workspace",
        workspace_arg,
        "--json",
        "graph",
        "centrality-refresh",
    ];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("graph centrality-refresh stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn seed_three_memory_graph() -> Result<(PathBuf, String, String, String, String), String> {
    let workspace = unique_workspace("triangle")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let alpha = remember(&workspace_arg, "Pin-test centrality alpha node.")?;
    let beta = remember(&workspace_arg, "Pin-test centrality beta node.")?;
    let gamma = remember(&workspace_arg, "Pin-test centrality gamma node.")?;

    let database_path = workspace.join(".ee").join("ee.db");
    // Triangle: alpha -> beta -> gamma -> alpha. Stable IDs keep golden traces sane.
    insert_link(
        &database_path,
        "link_00000000000000000000pin001",
        &alpha,
        &beta,
        0.9,
        0.8,
    )?;
    insert_link(
        &database_path,
        "link_00000000000000000000pin002",
        &beta,
        &gamma,
        0.9,
        0.8,
    )?;
    insert_link(
        &database_path,
        "link_00000000000000000000pin003",
        &gamma,
        &alpha,
        0.9,
        0.8,
    )?;

    Ok((workspace, workspace_arg, alpha, beta, gamma))
}

#[test]
fn graph_centrality_read_rejects_zero_limit_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-zero-limit")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = read_centrality(&workspace_arg, &["--limit", "0"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph centrality --limit 0 must fail; stdout: {}",
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
        format!("usage message should pin the --limit guard text; got {message}"),
    )?;
    let repair = error["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("--limit 10"),
        format!("usage repair should reference --limit 10; got {repair}"),
    )?;
    Ok(())
}

#[test]
fn graph_centrality_read_reports_scores_unavailable_without_snapshot() -> TestResult {
    let workspace = unique_workspace("no-snapshot")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = read_centrality(&workspace_arg, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph centrality should exit zero when snapshot is missing; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    ensure(
        data["schema"].as_str() == Some("ee.graph.centrality_read.v1"),
        format!("schema must be ee.graph.centrality_read.v1; got {data}"),
    )?;
    ensure(
        data["status"].as_str() == Some("scores_unavailable"),
        format!("status must be scores_unavailable without a snapshot; got {data}"),
    )?;
    let rows = data["rows"]
        .as_array()
        .ok_or_else(|| format!("rows must be an array; got {data}"))?;
    ensure(rows.is_empty(), "rows must be empty without a snapshot")?;
    let degraded = data["degraded"]
        .as_array()
        .ok_or_else(|| format!("degraded must be an array; got {data}"))?;
    let has_missing = degraded
        .iter()
        .any(|entry| entry["code"].as_str() == Some("graph_snapshot_missing"));
    ensure(
        has_missing,
        format!("degraded must include graph_snapshot_missing; got {degraded:?}"),
    )?;
    Ok(())
}

#[test]
fn graph_centrality_refresh_dry_run_does_not_persist_snapshot() -> TestResult {
    let (_workspace, workspace_arg, _alpha, _beta, _gamma) = seed_three_memory_graph()?;

    let (dry_output, dry_parsed) = refresh_centrality(&workspace_arg, &["--dry-run"])?;
    ensure(
        dry_output.status.success(),
        format!(
            "centrality-refresh --dry-run must succeed; stderr: {}",
            String::from_utf8_lossy(&dry_output.stderr)
        ),
    )?;
    ensure(
        dry_parsed["success"].as_bool() == Some(true),
        format!("dry-run refresh should report success=true; got {dry_parsed}"),
    )?;

    let (read_output, read_parsed) = read_centrality(&workspace_arg, &[])?;
    ensure(
        read_output.status.success(),
        format!(
            "read after dry-run must succeed; stderr: {}",
            String::from_utf8_lossy(&read_output.stderr)
        ),
    )?;
    ensure(
        read_parsed["data"]["status"].as_str() == Some("scores_unavailable"),
        format!(
            "dry-run refresh must leave snapshot absent; got status {}",
            read_parsed["data"]["status"]
        ),
    )?;
    Ok(())
}

#[test]
fn graph_centrality_read_returns_betweenness_rows_after_refresh() -> TestResult {
    let (_workspace, workspace_arg, _alpha, _beta, _gamma) = seed_three_memory_graph()?;
    let (refresh_output, _) = refresh_centrality(&workspace_arg, &[])?;
    ensure(
        refresh_output.status.success(),
        format!(
            "centrality-refresh must succeed; stderr: {}",
            String::from_utf8_lossy(&refresh_output.stderr)
        ),
    )?;

    let (output, parsed) = read_centrality(&workspace_arg, &["--algorithm", "betweenness"])?;
    ensure(
        output.status.success(),
        format!(
            "betweenness read must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    ensure(
        data["algorithm"].as_str() == Some("betweenness"),
        format!("algorithm field must echo betweenness; got {data}"),
    )?;
    ensure(
        data["status"].as_str() == Some("available"),
        format!("status must be available after refresh; got {data}"),
    )?;
    let rows = data["rows"]
        .as_array()
        .ok_or_else(|| format!("rows must be an array; got {data}"))?;
    ensure(
        !rows.is_empty(),
        "betweenness read should return at least one row for a 3-node cycle",
    )?;
    for (index, row) in rows.iter().enumerate() {
        let expected_rank = u64::try_from(index + 1).unwrap_or(u64::MAX);
        ensure(
            row["rank"].as_u64() == Some(expected_rank),
            format!("row {index} rank should be {expected_rank}; got {row}"),
        )?;
        ensure(
            row["score"].is_number(),
            format!("row {index} score must be numeric; got {row}"),
        )?;
        ensure(
            row["memoryId"].as_str().is_some(),
            format!("row {index} must carry memoryId; got {row}"),
        )?;
    }
    Ok(())
}

#[test]
fn graph_centrality_read_memory_id_filter_returns_single_row() -> TestResult {
    let (_workspace, workspace_arg, alpha, _beta, _gamma) = seed_three_memory_graph()?;
    let (refresh_output, _) = refresh_centrality(&workspace_arg, &[])?;
    ensure(
        refresh_output.status.success(),
        format!(
            "centrality-refresh must succeed; stderr: {}",
            String::from_utf8_lossy(&refresh_output.stderr)
        ),
    )?;

    let (output, parsed) = read_centrality(&workspace_arg, &["--memory-id", &alpha])?;
    ensure(
        output.status.success(),
        format!(
            "filtered read must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    ensure(
        data["memoryId"].as_str() == Some(alpha.as_str()),
        format!("memoryId echo must equal the filter; got {data}"),
    )?;
    let rows = data["rows"]
        .as_array()
        .ok_or_else(|| format!("rows must be an array; got {data}"))?;
    ensure(
        rows.len() == 1,
        format!(
            "memory-id filter must collapse rows to one entry; got {} rows",
            rows.len()
        ),
    )?;
    ensure(
        rows[0]["memoryId"].as_str() == Some(alpha.as_str()),
        format!("the surviving row must reference alpha; got {}", rows[0]),
    )?;
    Ok(())
}

#[test]
fn graph_centrality_read_limit_truncates_rows() -> TestResult {
    let (_workspace, workspace_arg, _alpha, _beta, _gamma) = seed_three_memory_graph()?;
    let (refresh_output, _) = refresh_centrality(&workspace_arg, &[])?;
    ensure(
        refresh_output.status.success(),
        format!(
            "centrality-refresh must succeed; stderr: {}",
            String::from_utf8_lossy(&refresh_output.stderr)
        ),
    )?;

    let (output, parsed) = read_centrality(&workspace_arg, &["--limit", "1"])?;
    ensure(
        output.status.success(),
        format!(
            "limit=1 read must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    ensure(
        data["limit"].as_u64() == Some(1),
        format!("limit echo must be 1; got {data}"),
    )?;
    let rows = data["rows"]
        .as_array()
        .ok_or_else(|| format!("rows must be an array; got {data}"))?;
    ensure(
        rows.len() == 1,
        format!(
            "--limit 1 must truncate rows to one; got {} rows",
            rows.len()
        ),
    )?;
    Ok(())
}
