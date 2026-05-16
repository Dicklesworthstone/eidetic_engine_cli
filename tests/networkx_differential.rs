//! NetworkX differential tests for FrankenNetworkX graph algorithms.
//!
//! The heavyweight Python dependency is deliberately behind the
//! `differential-networkx` feature so ordinary local-first `ee` builds and test
//! runs keep working without Python packages. Nightly CI and explicit RCH runs
//! enable the feature and compare fnx top-K node rankings against NetworkX on a
//! deterministic 100-node fixture.

#![cfg(feature = "graph")]

#[cfg(feature = "differential-networkx")]
use fnx_algorithms::{
    CentralityScore, degree_centrality_directed, in_degree_centrality, out_degree_centrality,
};
#[cfg(feature = "differential-networkx")]
use fnx_classes::digraph::DiGraph;
#[cfg(feature = "differential-networkx")]
use serde::Deserialize;
#[cfg(feature = "differential-networkx")]
use std::io::Write;
#[cfg(feature = "differential-networkx")]
use std::process::{Command, Stdio};

type TestResult<T = ()> = Result<T, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Fixture {
    nodes: Vec<String>,
    edges: Vec<(String, String)>,
}

fn networkx_fixture() -> Fixture {
    let nodes = (0..100)
        .map(|index| format!("n{index:03}"))
        .collect::<Vec<_>>();
    let mut edges = Vec::new();

    for index in 3..100 {
        edges.push((format!("n{index:03}"), "n000".to_owned()));
        edges.push(("n000".to_owned(), format!("n{index:03}")));
    }
    for index in 10..95 {
        edges.push((format!("n{index:03}"), "n001".to_owned()));
    }
    for index in 10..85 {
        edges.push(("n001".to_owned(), format!("n{index:03}")));
    }
    for index in 20..88 {
        edges.push((format!("n{index:03}"), "n002".to_owned()));
    }
    for index in 20..78 {
        edges.push(("n002".to_owned(), format!("n{index:03}")));
    }

    for index in 3..99 {
        edges.push((format!("n{index:03}"), format!("n{:03}", index + 1)));
    }
    for index in (3..96).step_by(3) {
        edges.push((format!("n{index:03}"), format!("n{:03}", index + 3)));
    }

    edges.sort();
    edges.dedup();
    Fixture { nodes, edges }
}

#[test]
fn networkx_fixture_contract_is_stable() -> TestResult {
    let fixture = networkx_fixture();
    if fixture.nodes.len() != 100 {
        return Err(format!(
            "NetworkX differential fixture must have exactly 100 nodes, got {}",
            fixture.nodes.len()
        ));
    }
    if fixture.edges.len() < 300 {
        return Err(format!(
            "NetworkX differential fixture should be dense enough to exercise ranking ties, got {} edges",
            fixture.edges.len()
        ));
    }
    if fixture
        .edges
        .iter()
        .any(|(source, target)| source == target)
    {
        return Err("NetworkX differential fixture must not contain self-loops".to_owned());
    }
    Ok(())
}

#[cfg(feature = "differential-networkx")]
#[derive(Debug, Deserialize)]
struct NetworkxReport {
    degree: Vec<RankedNode>,
    in_degree: Vec<RankedNode>,
    out_degree: Vec<RankedNode>,
}

#[cfg(feature = "differential-networkx")]
#[derive(Debug, Deserialize)]
struct RankedNode {
    node: String,
    score: f64,
}

#[cfg(feature = "differential-networkx")]
fn build_fnx_graph(fixture: &Fixture) -> TestResult<DiGraph> {
    let mut graph = DiGraph::strict();
    for node in &fixture.nodes {
        graph.add_node(node.as_str());
    }
    for (source, target) in &fixture.edges {
        graph
            .add_edge(source.as_str(), target.as_str())
            .map_err(|error| format!("failed to add fixture edge {source}->{target}: {error}"))?;
    }
    Ok(graph)
}

#[cfg(feature = "differential-networkx")]
fn run_networkx(fixture: &Fixture) -> TestResult<NetworkxReport> {
    let payload = serde_json::json!({
        "nodes": &fixture.nodes,
        "edges": &fixture.edges,
    });
    let script = r#"
import json
import sys

try:
    import networkx as nx
except Exception as exc:
    raise SystemExit(f"python networkx import failed: {exc}")

payload = json.load(sys.stdin)
graph = nx.DiGraph()
graph.add_nodes_from(payload["nodes"])
graph.add_edges_from(payload["edges"])

def top_scores(mapping):
    return [
        {"node": node, "score": score}
        for node, score in sorted(mapping.items(), key=lambda item: (-item[1], item[0]))[:10]
    ]

print(json.dumps({
    "degree": top_scores(nx.degree_centrality(graph)),
    "in_degree": top_scores(nx.in_degree_centrality(graph)),
    "out_degree": top_scores(nx.out_degree_centrality(graph)),
}, sort_keys=True))
"#;
    let mut child = Command::new("python3")
        .arg("-c")
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to spawn python3 for NetworkX differential: {error}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "failed to open python3 stdin".to_owned())?
        .write_all(payload.to_string().as_bytes())
        .map_err(|error| format!("failed to write NetworkX fixture JSON: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("failed to wait for NetworkX differential: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "NetworkX differential command failed with status {:?}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("NetworkX differential stdout was not JSON: {error}"))
}

#[cfg(feature = "differential-networkx")]
fn top_scores(mut scores: Vec<CentralityScore>) -> Vec<RankedNode> {
    scores.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.node.cmp(&right.node))
    });
    scores
        .into_iter()
        .take(10)
        .map(|score| RankedNode {
            node: score.node,
            score: score.score,
        })
        .collect()
}

#[cfg(feature = "differential-networkx")]
fn top_ids(scores: &[RankedNode], limit: usize) -> Vec<String> {
    scores
        .iter()
        .take(limit)
        .map(|score| score.node.clone())
        .collect()
}

#[cfg(feature = "differential-networkx")]
fn top10_rank_correlation(fnx: &[RankedNode], networkx: &[RankedNode]) -> f64 {
    let mut sum_squared_delta = 0.0;
    let mut count = 0.0;
    for (fnx_rank, fnx_node) in top_ids(fnx, 10).iter().enumerate() {
        if let Some(networkx_rank) = top_ids(networkx, 10)
            .iter()
            .position(|networkx_node| networkx_node == fnx_node)
        {
            let delta = fnx_rank as f64 - networkx_rank as f64;
            sum_squared_delta += delta * delta;
            count += 1.0;
        }
    }
    if count < 2.0 {
        return 0.0;
    }
    1.0 - (6.0 * sum_squared_delta) / (count * (count * count - 1.0))
}

#[cfg(feature = "differential-networkx")]
fn assert_top3_match(
    algorithm: &str,
    fnx_scores: &[RankedNode],
    networkx_scores: &[RankedNode],
) -> TestResult {
    let fnx_top3 = top_ids(fnx_scores, 3);
    let networkx_top3 = top_ids(networkx_scores, 3);
    let matching = fnx_top3
        .iter()
        .filter(|node| networkx_top3.contains(node))
        .count();
    let rank_correlation = top10_rank_correlation(fnx_scores, networkx_scores);
    let fnx_bad_score = fnx_scores
        .iter()
        .find(|score| !score.score.is_finite())
        .map(|score| format!("{}={}", score.node, score.score));
    let networkx_bad_score = networkx_scores
        .iter()
        .find(|score| !score.score.is_finite())
        .map(|score| format!("{}={}", score.node, score.score));
    if let Some(bad_score) = fnx_bad_score {
        return Err(format!(
            "fnx {algorithm} emitted a non-finite top-K score: {bad_score}"
        ));
    }
    if let Some(bad_score) = networkx_bad_score {
        return Err(format!(
            "NetworkX {algorithm} emitted a non-finite top-K score: {bad_score}"
        ));
    }
    println!(
        "networkx_differential algorithm={algorithm} fnx_top3={fnx_top3:?} networkx_top3={networkx_top3:?} top10_spearman={rank_correlation:.6}"
    );
    if fnx_top3 == networkx_top3 {
        return Ok(());
    }
    Err(format!(
        "CGSE-witness-divergence: {algorithm} top-3 IDs diverged; fnx={fnx_top3:?} networkx={networkx_top3:?} shared_ids={matching} top10_spearman={rank_correlation:.6}"
    ))
}

#[cfg(feature = "differential-networkx")]
#[test]
fn fnx_centrality_topk_matches_networkx_on_fixed_fixture() -> TestResult {
    let fixture = networkx_fixture();
    let graph = build_fnx_graph(&fixture)?;
    let networkx = run_networkx(&fixture)?;

    let fnx_degree = top_scores(degree_centrality_directed(&graph).scores);
    let fnx_in_degree = top_scores(in_degree_centrality(&graph));
    let fnx_out_degree = top_scores(out_degree_centrality(&graph));

    assert_top3_match("degree_centrality_directed", &fnx_degree, &networkx.degree)?;
    assert_top3_match("in_degree_centrality", &fnx_in_degree, &networkx.in_degree)?;
    assert_top3_match(
        "out_degree_centrality",
        &fnx_out_degree,
        &networkx.out_degree,
    )?;

    Ok(())
}
