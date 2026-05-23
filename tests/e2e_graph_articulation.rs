//! bd-ilxca: real-binary pin test for `ee graph articulation`.
//!
//! Mirrors the runtime shape of `graph_neighborhood_smoke.rs` and the
//! sibling pin tests (pagerank/betweenness/hits/path/centrality/
//! louvain) but exercises CLI angles unique to articulation:
//!
//! * the three shared `validate_graph_read_options` usage errors
//!   (--limit 0, --min-weight 2.0, --min-confidence -0.5)
//! * empty workspace returns `status=computed` with an empty
//!   `articulationPoints` array under the `ee.graph.algorithm.v1`
//!   envelope and `command="graph articulation"`
//! * a bridge graph a->b->c (whose undirected projection makes b the
//!   unique cut vertex) returns `articulationPoints == [b]`, proving
//!   the algorithm is actually evaluating articulation rather than
//!   returning all nodes or a stub
//! * the `articulationPoints` array is sorted alphabetically when
//!   multiple cut vertices exist (deterministic ordering)
//! * witness object carries algorithm/edgesScanned

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
        .join("ee-graph-articulation-pin")
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
                confidence: 0.85,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: Some("2026-05-01T00:00:00Z".to_string()),
                source: MemoryLinkSource::Human,
                created_by: Some("e2e-graph-articulation-pin".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn run_graph_articulation(workspace_arg: &str, extra: &[&str]) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec![
        "--workspace",
        workspace_arg,
        "--json",
        "graph",
        "articulation",
    ];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("graph articulation stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn assert_envelope_shape(data: &Value) -> TestResult {
    ensure(
        data["schema"].as_str() == Some("ee.graph.algorithm.v1"),
        format!("schema must be ee.graph.algorithm.v1; got {data}"),
    )?;
    ensure(
        data["command"].as_str() == Some("graph articulation"),
        format!("command must be graph articulation; got {data}"),
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
        data["articulationPoints"].is_array(),
        format!("articulationPoints must be an array; got {data}"),
    )?;
    ensure(
        data["witness"].is_object(),
        format!("witness must be an object; got {data}"),
    )?;
    Ok(())
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

/// Seeds a bridge graph: a -> b -> c. The undirected projection
/// makes b the unique cut vertex (removing b disconnects a from c).
fn seed_bridge_graph() -> Result<(PathBuf, String, String, String, String), String> {
    let workspace = unique_workspace("bridge")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let a = remember(&workspace_arg, "Pin-test articulation bridge a (leaf).")?;
    let b = remember(&workspace_arg, "Pin-test articulation bridge b (bridge).")?;
    let c = remember(&workspace_arg, "Pin-test articulation bridge c (leaf).")?;
    let database_path = workspace.join(".ee").join("ee.db");
    insert_link(&database_path, "link_00000000000000000000art00001", &a, &b)?;
    insert_link(&database_path, "link_00000000000000000000art00002", &b, &c)?;
    Ok((workspace, workspace_arg, a, b, c))
}

#[test]
fn graph_articulation_rejects_zero_limit_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-zero-limit")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_articulation(&workspace_arg, &["--limit", "0"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph articulation --limit 0 must fail; stdout: {}",
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
fn graph_articulation_rejects_min_weight_out_of_range_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-weight")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_articulation(&workspace_arg, &["--min-weight", "2.0"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph articulation --min-weight 2.0 must fail; stdout: {}",
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
fn graph_articulation_rejects_min_confidence_out_of_range_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-confidence")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_articulation(&workspace_arg, &["--min-confidence", "-0.5"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph articulation --min-confidence -0.5 must fail; stdout: {}",
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
fn graph_articulation_returns_empty_points_on_fresh_workspace() -> TestResult {
    let workspace = unique_workspace("empty")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_articulation(&workspace_arg, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph articulation on a fresh workspace must exit zero; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    ensure(
        parsed["schema"].as_str() == Some("ee.response.v1"),
        format!("envelope schema must be ee.response.v1; got {parsed}"),
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
        data["articulationPoints"].as_array().map(Vec::len) == Some(0),
        format!("articulationPoints must be empty with no links; got {data}"),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        stdout.lines().filter(|line| !line.is_empty()).count() == 1,
        format!("--json stdout must be a single line; got {stdout}"),
    )?;
    Ok(())
}

#[test]
fn graph_articulation_identifies_bridge_node_on_two_edge_path() -> TestResult {
    let (_workspace, workspace_arg, a, b, c) = seed_bridge_graph()?;

    let (output, parsed) = run_graph_articulation(&workspace_arg, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph articulation on bridge graph must succeed; stderr: {}",
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

    let points = data["articulationPoints"]
        .as_array()
        .ok_or_else(|| format!("articulationPoints must be an array; got {data}"))?;
    // Classic articulation property: on the undirected projection of
    // a -> b -> c, b is the unique cut vertex. Removing b disconnects
    // a from c; removing a or c leaves the graph connected (the
    // remaining edge survives). This proves we are actually
    // evaluating articulation, not returning all nodes or a stub.
    ensure(
        points.len() == 1,
        format!("bridge graph a->b->c must yield exactly one articulation point; got {points:?}"),
    )?;
    ensure(
        points[0].as_str() == Some(b.as_str()),
        format!(
            "rank-1 articulation point must be the bridge node b={b}; got {points:?} (a={a}, c={c})"
        ),
    )?;

    // Output must be alphabetically sorted (graph_articulation
    // explicitly calls sorted_nodes.sort() before serializing). With
    // a single-element array this is trivially true; verify the
    // contract anyway by also asserting points equals its own sort.
    let observed: Vec<String> = points
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    let mut sorted_check = observed.clone();
    sorted_check.sort();
    ensure(
        observed == sorted_check,
        format!("articulationPoints must be sorted ascending; got {observed:?}"),
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
fn graph_articulation_emits_sorted_points_for_multi_bridge_chain() -> TestResult {
    // Chain a -> b -> c -> d: undirected projection has two cut
    // vertices b and c. The articulationPoints array must contain
    // exactly {b, c}, sorted alphabetically.
    let workspace = unique_workspace("multi-bridge")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let a = remember(&workspace_arg, "Pin-test articulation chain a.")?;
    let b = remember(&workspace_arg, "Pin-test articulation chain b.")?;
    let c = remember(&workspace_arg, "Pin-test articulation chain c.")?;
    let d = remember(&workspace_arg, "Pin-test articulation chain d.")?;
    let database_path = workspace.join(".ee").join("ee.db");
    insert_link(&database_path, "link_00000000000000000000art00101", &a, &b)?;
    insert_link(&database_path, "link_00000000000000000000art00102", &b, &c)?;
    insert_link(&database_path, "link_00000000000000000000art00103", &c, &d)?;

    let (output, parsed) = run_graph_articulation(&workspace_arg, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph articulation on chain graph must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    assert_envelope_shape(data)?;
    let points: Vec<String> = data["articulationPoints"]
        .as_array()
        .ok_or_else(|| format!("articulationPoints must be an array; got {data}"))?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    ensure(
        points.len() == 2,
        format!(
            "chain a->b->c->d must yield exactly two articulation points (b and c); got {points:?} (a={a}, d={d})"
        ),
    )?;
    let mut expected = vec![b.clone(), c.clone()];
    expected.sort();
    ensure(
        points == expected,
        format!(
            "articulationPoints must equal sorted [b, c]; got {points:?}, expected {expected:?}"
        ),
    )?;
    ensure(
        !points.contains(&a),
        format!("leaf a={a} must not appear in articulationPoints"),
    )?;
    ensure(
        !points.contains(&d),
        format!("leaf d={d} must not appear in articulationPoints"),
    )?;
    Ok(())
}
