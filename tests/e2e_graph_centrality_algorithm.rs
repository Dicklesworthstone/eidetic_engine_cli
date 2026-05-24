//! bd-cn1ru: real-binary pin test for `ee graph centrality --algorithm`
//! parsing and the `algorithm_unavailable` status branch.
//!
//! `GraphCentralityReadAlgorithm::parse` (src/cli/mod.rs:25544) accepts
//! several aliases - `page-rank` -> Pagerank, `betweenness-centrality`
//! -> Betweenness, `authority` / `authorities` -> Authority,
//! `hub` / `hubs` / `hits-hubs` -> HitsHubs, `hits-authorities`
//! -> HitsAuthorities - and falls back to `Unknown` for anything else.
//! Only Pagerank and Betweenness are
//! `available_in_memory_link_snapshot()`; every other parsed algorithm
//! (HitsHubs, HitsAuthorities, Authority, Unknown) returns
//! `status="algorithm_unavailable"` with a `graph_algorithm_unavailable`
//! degraded entry that echoes the original `--algorithm` argument in the
//! repair string.
//!
//! `tests/e2e_graph_centrality.rs` covers `--limit 0`, the no-snapshot
//! `scores_unavailable` branch, the dry-run refresh path, the betweenness
//! success path, the `--memory-id` filter, and the `--limit` truncation -
//! but not the alias map or the `algorithm_unavailable` branch. This
//! pin-test reuses the same triangle-graph harness shape and pins:
//!
//! * `--algorithm hits-hubs` on a refreshed workspace surfaces
//!   `status=algorithm_unavailable`, `algorithm=hits-hubs`, and a degraded
//!   entry whose `code=graph_algorithm_unavailable`, `severity=medium`, and
//!   whose `repair` echoes
//!   `ee graph centrality-refresh --algorithm hits-hubs`.
//! * `--algorithm bogus_unknown` on a refreshed workspace surfaces
//!   `algorithm=unknown` and `status=algorithm_unavailable`.
//! * `--algorithm page-rank` (dash alias for pagerank) on a refreshed
//!   workspace surfaces `algorithm=pagerank` and `status=available` with
//!   at least one row.
//! * `--algorithm betweenness-centrality` (long alias for betweenness) on
//!   a refreshed workspace surfaces `algorithm=betweenness` and
//!   `status=available`.

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
        .join("ee-graph-centrality-algorithm-pin")
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
                created_by: Some("e2e-graph-centrality-algorithm-pin".to_string()),
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

    let alpha = remember(&workspace_arg, "Pin-test centrality alpha.")?;
    let beta = remember(&workspace_arg, "Pin-test centrality beta.")?;
    let gamma = remember(&workspace_arg, "Pin-test centrality gamma.")?;

    let database_path = workspace.join(".ee").join("ee.db");
    insert_link(
        &database_path,
        "link_00000000000000000000pinalg01",
        &alpha,
        &beta,
    )?;
    insert_link(
        &database_path,
        "link_00000000000000000000pinalg02",
        &beta,
        &gamma,
    )?;
    insert_link(
        &database_path,
        "link_00000000000000000000pinalg03",
        &gamma,
        &alpha,
    )?;

    refresh_centrality(&workspace_arg)?;
    Ok((workspace, workspace_arg))
}

#[test]
fn graph_centrality_hits_hubs_alias_returns_algorithm_unavailable() -> TestResult {
    let (_workspace, workspace_arg) = seed_refreshed_triangle()?;

    let (output, parsed) = read_centrality(&workspace_arg, &["--algorithm", "hits-hubs"])?;
    ensure(
        output.status.success(),
        format!(
            "graph centrality --algorithm hits-hubs must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    ensure(
        data["schema"].as_str() == Some("ee.graph.centrality_read.v1"),
        format!("schema must be ee.graph.centrality_read.v1; got {data}"),
    )?;
    ensure(
        data["status"].as_str() == Some("algorithm_unavailable"),
        format!(
            "status must be algorithm_unavailable for hits-hubs against a memory-link snapshot; got {data}"
        ),
    )?;
    ensure(
        data["algorithm"].as_str() == Some("hits-hubs"),
        format!("algorithm must echo the parsed hits-hubs alias; got {data}"),
    )?;
    let degraded = data["degraded"]
        .as_array()
        .ok_or_else(|| format!("degraded must be an array; got {data}"))?;
    let unavailable = degraded
        .iter()
        .find(|entry| entry["code"].as_str() == Some("graph_algorithm_unavailable"))
        .ok_or_else(|| {
            format!(
                "degraded must include graph_algorithm_unavailable for hits-hubs; got {degraded:?}"
            )
        })?;
    ensure(
        unavailable["severity"].as_str() == Some("medium"),
        format!("graph_algorithm_unavailable severity must be medium; got {unavailable}"),
    )?;
    let repair = unavailable["repair"].as_str().unwrap_or_default();
    ensure(
        repair.contains("ee graph centrality-refresh --algorithm hits-hubs"),
        format!(
            "graph_algorithm_unavailable repair must echo the original --algorithm value; got {repair}"
        ),
    )?;
    Ok(())
}

#[test]
fn graph_centrality_bogus_algorithm_returns_algorithm_unavailable_with_unknown() -> TestResult {
    let (_workspace, workspace_arg) = seed_refreshed_triangle()?;

    let (output, parsed) = read_centrality(&workspace_arg, &["--algorithm", "bogus_unknown"])?;
    ensure(
        output.status.success(),
        format!(
            "graph centrality --algorithm bogus_unknown must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    ensure(
        data["status"].as_str() == Some("algorithm_unavailable"),
        format!("status must be algorithm_unavailable for an unparseable algorithm; got {data}"),
    )?;
    ensure(
        data["algorithm"].as_str() == Some("unknown"),
        format!(
            "algorithm must be 'unknown' when --algorithm value does not parse to a known alias; got {data}"
        ),
    )?;
    let degraded = data["degraded"]
        .as_array()
        .ok_or_else(|| format!("degraded must be an array; got {data}"))?;
    let has_unavailable = degraded
        .iter()
        .any(|entry| entry["code"].as_str() == Some("graph_algorithm_unavailable"));
    ensure(
        has_unavailable,
        format!(
            "degraded must include graph_algorithm_unavailable for unparseable algorithm; got {degraded:?}"
        ),
    )?;
    Ok(())
}

#[test]
fn graph_centrality_page_rank_alias_returns_pagerank_available() -> TestResult {
    let (_workspace, workspace_arg) = seed_refreshed_triangle()?;

    let (output, parsed) = read_centrality(&workspace_arg, &["--algorithm", "page-rank"])?;
    ensure(
        output.status.success(),
        format!(
            "graph centrality --algorithm page-rank must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    ensure(
        data["status"].as_str() == Some("available"),
        format!(
            "status must be available when page-rank resolves to pagerank with a fresh snapshot; got {data}"
        ),
    )?;
    ensure(
        data["algorithm"].as_str() == Some("pagerank"),
        format!(
            "page-rank alias must normalize to canonical 'pagerank' in the response; got {data}"
        ),
    )?;
    let rows = data["rows"]
        .as_array()
        .ok_or_else(|| format!("rows must be an array; got {data}"))?;
    ensure(
        !rows.is_empty(),
        format!("pagerank rows must be non-empty for a triangle after refresh; got {rows:?}"),
    )?;
    Ok(())
}

#[test]
fn graph_centrality_betweenness_centrality_alias_returns_betweenness_available() -> TestResult {
    let (_workspace, workspace_arg) = seed_refreshed_triangle()?;

    let (output, parsed) =
        read_centrality(&workspace_arg, &["--algorithm", "betweenness-centrality"])?;
    ensure(
        output.status.success(),
        format!(
            "graph centrality --algorithm betweenness-centrality must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    ensure(
        data["status"].as_str() == Some("available"),
        format!(
            "status must be available when betweenness-centrality resolves to betweenness; got {data}"
        ),
    )?;
    ensure(
        data["algorithm"].as_str() == Some("betweenness"),
        format!(
            "betweenness-centrality alias must normalize to canonical 'betweenness' in the response; got {data}"
        ),
    )?;
    Ok(())
}
