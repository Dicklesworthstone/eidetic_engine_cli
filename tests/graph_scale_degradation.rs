use std::collections::BTreeSet;

use ee::graph::scale_policy::{
    ALL_PAIRS_LCA_LAZY_THRESHOLD_NODES, CAUSAL_DEPTH_CAP, GOMORY_HU_SKIP_THRESHOLD_NODES,
    GraphScaleAction, GraphScaleAlgorithm, INSIGHTS_100K_BUDGET_MS,
    SIMRANK_JACCARD_THRESHOLD_NODES, graph_scale_decision, graph_scale_decisions,
    graph_scale_total_budget_ms,
};
use serde_json::Value;

const FIXTURES: &[(&str, &str, usize, usize)] = &[
    (
        "tests/fixtures/scale/graph_10k.jsonl",
        include_str!("fixtures/scale/graph_10k.jsonl"),
        10_000,
        25_000,
    ),
    (
        "tests/fixtures/scale/graph_50k.jsonl",
        include_str!("fixtures/scale/graph_50k.jsonl"),
        50_000,
        125_000,
    ),
    (
        "tests/fixtures/scale/graph_100k.jsonl",
        include_str!("fixtures/scale/graph_100k.jsonl"),
        100_000,
        250_000,
    ),
];

type TestResult = Result<(), String>;

fn fixture_json(path: &str, source: &str) -> Result<Value, String> {
    let lines = source.lines().collect::<Vec<_>>();
    if lines.len() != 1 {
        return Err(format!("{path} must contain exactly one JSONL spec row"));
    }
    serde_json::from_str(lines[0]).map_err(|error| format!("{path} invalid JSON: {error}"))
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a Value, String> {
    value
        .get(name)
        .ok_or_else(|| format!("missing field `{name}`"))
}

fn string_field<'a>(value: &'a Value, name: &str) -> Result<&'a str, String> {
    field(value, name)?
        .as_str()
        .ok_or_else(|| format!("field `{name}` must be a string"))
}

fn usize_field(value: &Value, name: &str) -> Result<usize, String> {
    let raw = field(value, name)?
        .as_u64()
        .ok_or_else(|| format!("field `{name}` must be an unsigned integer"))?;
    usize::try_from(raw).map_err(|error| format!("field `{name}` does not fit usize: {error}"))
}

fn ensure(condition: bool, context: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(context.to_owned())
    }
}

fn stable_memory_id(prefix: &str, ordinal: usize) -> String {
    format!("{prefix}{ordinal:06}")
}

#[test]
fn graph_scale_fixtures_pin_10k_50k_100k_shapes() -> TestResult {
    let mut ids = BTreeSet::new();
    for (path, source, expected_memories, expected_edges) in FIXTURES {
        let json = fixture_json(path, source)?;
        ensure(
            ids.insert(string_field(&json, "fixtureId")?.to_owned()),
            "fixture IDs must be unique",
        )?;
        ensure(
            string_field(&json, "schema")? == "ee.graph.scale_fixture.v1",
            "fixture schema",
        )?;
        ensure(
            string_field(&json, "owningBeadId")? == "bd-bife.17",
            "owning bead",
        )?;
        ensure(
            string_field(&json, "seedFamily")? == "seed.graph_scale.v1",
            "seed family",
        )?;
        ensure(
            !field(&json, "materializedInCi")?.as_bool().unwrap_or(true),
            "large graph fixtures must stay generator-backed in normal CI",
        )?;
        ensure(
            usize_field(&json, "memoryCount")? == *expected_memories,
            "memory count",
        )?;
        ensure(
            usize_field(&json, "edgeCount")? == *expected_edges,
            "edge count",
        )?;
        let prefix = string_field(&json, "memoryIdPrefix")?;
        let expected_first = stable_memory_id(prefix, 1);
        let expected_last = stable_memory_id(prefix, *expected_memories);
        ensure(
            string_field(&json, "expectedFirstMemoryId")? == expected_first.as_str(),
            "first memory ID",
        )?;
        ensure(
            string_field(&json, "expectedLastMemoryId")? == expected_last.as_str(),
            "last memory ID",
        )?;
    }
    ensure(ids.len() == 3, "expected three scale fixture specs")
}

#[test]
fn graph_scale_policy_skips_or_caps_expensive_algorithms_at_100k() -> TestResult {
    let nodes = 100_000;
    let edges = 250_000;

    let gomory = graph_scale_decision(GraphScaleAlgorithm::GomoryHu, nodes, edges);
    ensure(gomory.action == GraphScaleAction::Skip, "Gomory-Hu skips")?;
    ensure(
        gomory.degraded_code == Some("graph_scale_gomory_hu_skipped"),
        "Gomory-Hu degraded code",
    )?;
    ensure(
        gomory.cap == Some(GOMORY_HU_SKIP_THRESHOLD_NODES),
        "Gomory-Hu threshold",
    )?;

    let lca = graph_scale_decision(GraphScaleAlgorithm::AllPairsLca, nodes, edges);
    ensure(lca.action == GraphScaleAction::LazyOnDemand, "LCA lazy")?;
    ensure(
        lca.cap == Some(ALL_PAIRS_LCA_LAZY_THRESHOLD_NODES),
        "LCA threshold",
    )?;

    let simrank = graph_scale_decision(GraphScaleAlgorithm::SimRank, nodes, edges);
    ensure(
        simrank.action == GraphScaleAction::FallbackJaccard,
        "SimRank fallback",
    )?;
    ensure(
        simrank.cap == Some(SIMRANK_JACCARD_THRESHOLD_NODES),
        "SimRank threshold",
    )?;

    let causal = graph_scale_decision(GraphScaleAlgorithm::TransitiveClosure, nodes, edges);
    ensure(
        causal.action == GraphScaleAction::CapDepth,
        "causal closure cap",
    )?;
    ensure(causal.cap == Some(CAUSAL_DEPTH_CAP), "causal cap value")?;
    ensure(
        causal.degraded_code == Some("causal_depth_capped"),
        "causal degraded code",
    )?;

    let flow = graph_scale_decision(GraphScaleAlgorithm::MinCostFlow, nodes, edges);
    ensure(
        flow.action == GraphScaleAction::CapIterations,
        "min-cost flow cap",
    )?;

    let betweenness = graph_scale_decision(GraphScaleAlgorithm::Betweenness, nodes, edges);
    ensure(
        betweenness.action == GraphScaleAction::PivotSample,
        "betweenness pivots",
    )
}

#[test]
fn graph_scale_policy_keeps_100k_insights_under_budget() -> TestResult {
    let decisions = graph_scale_decisions(100_000, 250_000);
    ensure(decisions.len() == 17, "all graph algorithms covered")?;
    ensure(
        graph_scale_total_budget_ms(100_000, 250_000) <= INSIGHTS_100K_BUDGET_MS,
        "100k insights policy budget",
    )?;
    ensure(
        decisions
            .iter()
            .any(|decision| decision.degraded_code == Some("causal_depth_capped")),
        "causal cap degraded code present",
    )?;
    ensure(
        decisions
            .iter()
            .any(|decision| decision.degraded_code == Some("graph_scale_gomory_hu_skipped")),
        "Gomory-Hu skip degraded code present",
    )?;
    ensure(
        decisions
            .iter()
            .any(|decision| decision.degraded_code == Some("graph_scale_simrank_jaccard_fallback")),
        "SimRank fallback degraded code present",
    )
}
