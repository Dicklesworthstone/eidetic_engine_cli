//! bd-3fr7i: real-binary pin test for `ee graph communities`.
//!
//! Mirrors the runtime shape of `graph_neighborhood_smoke.rs` and the
//! sibling pin tests. Distinct from `e2e_graph_louvain.rs` (bd-1g164)
//! because `communities` dispatches to
//! `fnx_algorithms::label_propagation_communities` (no resolution /
//! threshold / max-level / seed args — only the shared
//! `validate_graph_read_options` surfaces). Both surfaces share the
//! same `graph_communities_data` JSON shape downstream, so this test
//! pins:
//!
//! * three shared `validate_graph_read_options` usage errors
//!   (--limit 0, --min-weight 2.0, --min-confidence -0.5)
//! * empty workspace returns `status=computed` with `communityCount=0`
//!   and an empty `communities` array under the
//!   `ee.graph.algorithm.v1` envelope and `command="graph communities"`
//! * two disjoint two-node components (a -> b and c -> d) yield two
//!   communities of size 2 each via the `graph_communities_data`
//!   shape (`communityId`, `size`, `nodes` sorted ascending), each
//!   community holding exactly one expected src+dst pair
//!
//! Label propagation is not guaranteed byte-deterministic across runs
//! the way Louvain with --seed is, so this test does not assert
//! determinism — only structural invariants the algorithm must hold
//! on disjoint components.

#![cfg(unix)]

use std::collections::BTreeSet;
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
        .join("ee-graph-communities-pin")
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
                created_by: Some("e2e-graph-communities-pin".to_string()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn run_graph_communities(workspace_arg: &str, extra: &[&str]) -> Result<(Output, Value), String> {
    let mut args: Vec<&str> = vec![
        "--workspace",
        workspace_arg,
        "--json",
        "graph",
        "communities",
    ];
    args.extend_from_slice(extra);
    let output = run_ee(&args)?;
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("graph communities stdout must be JSON: {error}"))?;
    Ok((output, parsed))
}

fn assert_envelope_shape(data: &Value) -> TestResult {
    ensure(
        data["schema"].as_str() == Some("ee.graph.algorithm.v1"),
        format!("schema must be ee.graph.algorithm.v1; got {data}"),
    )?;
    ensure(
        data["command"].as_str() == Some("graph communities"),
        format!("command must be graph communities; got {data}"),
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
        data["communities"].is_array(),
        format!("communities must be an array; got {data}"),
    )?;
    ensure(
        data["communityCount"].is_u64(),
        format!("communityCount must be numeric; got {data}"),
    )?;
    ensure(
        data["limited"].is_boolean(),
        format!("limited must be boolean; got {data}"),
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

fn seed_two_disjoint_components()
-> Result<(PathBuf, String, String, String, String, String), String> {
    let workspace = unique_workspace("disjoint")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;
    let a = remember(&workspace_arg, "Pin-test communities component A node 1.")?;
    let b = remember(&workspace_arg, "Pin-test communities component A node 2.")?;
    let c = remember(&workspace_arg, "Pin-test communities component B node 1.")?;
    let d = remember(&workspace_arg, "Pin-test communities component B node 2.")?;
    let database_path = workspace.join(".ee").join("ee.db");
    insert_link(&database_path, "link_00000000000000000000com00001", &a, &b)?;
    insert_link(&database_path, "link_00000000000000000000com00002", &c, &d)?;
    Ok((workspace, workspace_arg, a, b, c, d))
}

#[test]
fn graph_communities_rejects_zero_limit_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-zero-limit")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_communities(&workspace_arg, &["--limit", "0"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph communities --limit 0 must fail; stdout: {}",
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
fn graph_communities_rejects_min_weight_out_of_range_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-weight")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_communities(&workspace_arg, &["--min-weight", "2.0"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph communities --min-weight 2.0 must fail; stdout: {}",
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
fn graph_communities_rejects_min_confidence_out_of_range_with_usage_error() -> TestResult {
    let workspace = unique_workspace("usage-confidence")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_communities(&workspace_arg, &["--min-confidence", "-0.5"])?;
    ensure(
        !output.status.success(),
        format!(
            "graph communities --min-confidence -0.5 must fail; stdout: {}",
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
fn graph_communities_returns_empty_set_on_fresh_workspace() -> TestResult {
    let workspace = unique_workspace("empty")?;
    let workspace_arg = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be UTF-8".to_string())?
        .to_owned();
    init_workspace(&workspace_arg)?;

    let (output, parsed) = run_graph_communities(&workspace_arg, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph communities on a fresh workspace must exit zero; stderr: {}",
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
        data["communityCount"].as_u64() == Some(0),
        format!("communityCount must be 0 with no links; got {data}"),
    )?;
    ensure(
        data["communities"].as_array().map(Vec::len) == Some(0),
        format!("communities must be empty with no links; got {data}"),
    )?;
    ensure(
        data["limited"] == Value::Bool(false),
        format!("limited must be false with no links; got {data}"),
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure(
        stdout.lines().filter(|line| !line.is_empty()).count() == 1,
        format!("--json stdout must be a single line; got {stdout}"),
    )?;
    Ok(())
}

#[test]
fn graph_communities_splits_disjoint_components_into_separate_communities() -> TestResult {
    let (_workspace, workspace_arg, a, b, c, d) = seed_two_disjoint_components()?;

    let (output, parsed) = run_graph_communities(&workspace_arg, &[])?;
    ensure(
        output.status.success(),
        format!(
            "graph communities on disjoint components must succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let data = &parsed["data"];
    assert_envelope_shape(data)?;
    ensure(
        data["graph"]["nodeCount"].as_u64() == Some(4),
        format!("nodeCount must reflect four seeded nodes; got {data}"),
    )?;
    ensure(
        data["graph"]["edgeCount"].as_u64() == Some(2),
        format!("edgeCount must reflect two seeded edges; got {data}"),
    )?;
    let communities = data["communities"]
        .as_array()
        .ok_or_else(|| format!("communities must be an array; got {data}"))?;
    ensure(
        communities.len() == 2,
        format!("two disjoint components must yield exactly two communities; got {communities:?}"),
    )?;
    ensure(
        data["communityCount"].as_u64() == Some(2),
        format!("communityCount must reflect two communities; got {data}"),
    )?;

    // Per graph_communities_data: each entry has communityId=
    // "community_NNNN" (1-indexed in emit order), size, and a
    // node array sorted ascending. Communities sort by descending
    // size then ascending first-node id; with equal-size pairs the
    // first-node-id tiebreak makes the ordering deterministic.
    let mut expected_pair_ab = BTreeSet::new();
    expected_pair_ab.insert(a.clone());
    expected_pair_ab.insert(b.clone());
    let mut expected_pair_cd = BTreeSet::new();
    expected_pair_cd.insert(c.clone());
    expected_pair_cd.insert(d.clone());

    let mut observed_pairs: Vec<BTreeSet<String>> = Vec::new();
    for (index, community) in communities.iter().enumerate() {
        let expected_id = format!("community_{:04}", index + 1);
        ensure(
            community["communityId"].as_str() == Some(expected_id.as_str()),
            format!("community[{index}].communityId must be {expected_id}; got {community}"),
        )?;
        ensure(
            community["size"].as_u64() == Some(2),
            format!("community[{index}].size must be 2; got {community}"),
        )?;
        let nodes = community["nodes"]
            .as_array()
            .ok_or_else(|| format!("community[{index}].nodes must be an array; got {community}"))?;
        ensure(
            nodes.len() == 2,
            format!("community[{index}] must contain two nodes; got {nodes:?}"),
        )?;
        // intra-community nodes sorted ascending (graph_communities_data
        // explicitly sorts each Vec<String> before serializing).
        let node_strings: Vec<String> = nodes
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        let mut sorted_check = node_strings.clone();
        sorted_check.sort();
        ensure(
            node_strings == sorted_check,
            format!("community[{index}] nodes must be sorted ascending; got {node_strings:?}"),
        )?;
        observed_pairs.push(node_strings.into_iter().collect());
    }

    ensure(
        observed_pairs.contains(&expected_pair_ab),
        format!("communities must include the {{a={a}, b={b}}} component; got {observed_pairs:?}"),
    )?;
    ensure(
        observed_pairs.contains(&expected_pair_cd),
        format!("communities must include the {{c={c}, d={d}}} component; got {observed_pairs:?}"),
    )?;
    Ok(())
}
