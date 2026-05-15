//! Contract checks that graph config keys alter graph behavior, not only
//! config-show output.

use ee::config::{
    ConfigFile, ConfigLayers, GraphConfig, PathExpander, built_in_config, merge_config,
};
use ee::graph::algorithms::{
    PprPolicy, SamplingChoice, SamplingPolicy, run_pagerank_with_policy, run_with_sampling_policy,
};
use ee::graph::health::{ContradictionClusterPolicy, detect_contradiction_clusters_with_policy};
use fnx_algorithms::PageRankResult;
use fnx_classes::Graph;
use fnx_classes::digraph::DiGraph;
use fnx_runtime::CompatibilityMode;
use insta::assert_json_snapshot;
use serde_json::{Value, json};

type TestResult = Result<(), String>;

fn merged_graph_config(project_toml: &str) -> Result<GraphConfig, String> {
    let expander = PathExpander::from_process_env();
    let defaults = built_in_config(&expander).map_err(|error| error.to_string())?;
    let project = ConfigFile::parse(project_toml).map_err(|error| error.to_string())?;
    let mut layers = ConfigLayers::with_defaults(defaults);
    layers.project = project;
    Ok(merge_config(&layers).values.graph)
}

fn build_pagerank_fixture() -> Result<DiGraph, String> {
    let mut graph = DiGraph::strict();
    graph
        .add_edge("mem_a", "mem_b")
        .map_err(|error| format!("add mem_a -> mem_b: {error}"))?;
    graph
        .add_edge("mem_b", "mem_c")
        .map_err(|error| format!("add mem_b -> mem_c: {error}"))?;
    Ok(graph)
}

fn rounded_score(result: &PageRankResult, node: &str) -> Result<i64, String> {
    result
        .scores
        .iter()
        .find(|score| score.node == node)
        .map(|score| (score.score * 1_000_000.0).round() as i64)
        .ok_or_else(|| format!("PageRank result missing {node}"))
}

fn pagerank_summary(result: &PageRankResult) -> Result<Value, String> {
    Ok(json!({
        "memA": rounded_score(result, "mem_a")?,
        "memB": rounded_score(result, "mem_b")?,
        "memC": rounded_score(result, "mem_c")?,
        "converged": result.converged,
    }))
}

fn json_bytes(value: &Value) -> Result<Vec<u8>, String> {
    serde_json::to_vec(value).map_err(|error| error.to_string())
}

fn assert_float_eq(actual: f64, expected: f64, context: &str) -> TestResult {
    if (actual - expected).abs() <= f64::EPSILON {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected}, got {actual}"))
    }
}

#[test]
fn ppr_alpha_zero_is_stable_legacy_behavior_and_alpha_changes_scores() -> TestResult {
    let legacy_config = merged_graph_config(
        r#"
[graph.ppr]
alpha = 0.0
"#,
    )?;
    let default_config = merged_graph_config("")?;
    let strong_config = merged_graph_config(
        r#"
[graph.ppr]
alpha = 0.90
"#,
    )?;

    let legacy_policy = PprPolicy::from_optional_config(legacy_config.ppr.alpha);
    let default_policy = PprPolicy::from_optional_config(default_config.ppr.alpha);
    let strong_policy = PprPolicy::from_optional_config(strong_config.ppr.alpha);
    assert_float_eq(legacy_policy.alpha, 0.0, "legacy alpha")?;
    assert_float_eq(default_policy.alpha, 0.30, "default alpha")?;
    assert_float_eq(strong_policy.alpha, 0.90, "strong alpha")?;

    let graph = build_pagerank_fixture()?;
    let legacy = pagerank_summary(&run_pagerank_with_policy(&graph, legacy_policy))?;
    let legacy_repeat = pagerank_summary(&run_pagerank_with_policy(&graph, legacy_policy))?;
    let default = pagerank_summary(&run_pagerank_with_policy(&graph, default_policy))?;
    let strong = pagerank_summary(&run_pagerank_with_policy(&graph, strong_policy))?;

    if json_bytes(&legacy)? != json_bytes(&legacy_repeat)? {
        return Err("alpha=0.0 legacy PageRank output is not byte-stable".to_string());
    }
    if legacy == default {
        return Err("graph.ppr.alpha default did not change PageRank output".to_string());
    }
    if default == strong {
        return Err("graph.ppr.alpha=0.90 did not change PageRank output".to_string());
    }

    assert_json_snapshot!(
        "ppr_alpha_config_behavior",
        json!({
            "schema": "ee.graph.config_behavior.v1",
            "surface": "graph.ppr.alpha",
            "legacyAlpha": legacy_policy.alpha,
            "defaultAlpha": default_policy.alpha,
            "strongAlpha": strong_policy.alpha,
            "legacyJsonStable": true,
            "legacyDiffersFromDefault": legacy != default,
            "defaultDiffersFromStrong": default != strong,
        })
    );
    Ok(())
}

#[test]
fn contradiction_threshold_changes_health_cluster_classification() -> TestResult {
    let permissive_config = merged_graph_config(
        r#"
[graph.health]
contradiction_threshold = 0.50
"#,
    )?;
    let strict_config = merged_graph_config(
        r#"
[graph.health]
contradiction_threshold = 0.75
"#,
    )?;

    let permissive_policy = ContradictionClusterPolicy::from_optional_config(
        permissive_config.health.contradiction_threshold,
    );
    let strict_policy = ContradictionClusterPolicy::from_optional_config(
        strict_config.health.contradiction_threshold,
    );

    let mut graph = Graph::new(CompatibilityMode::Strict);
    let _ = graph.extend_edges_unrecorded([("mem_a", "mem_b"), ("mem_b", "mem_c")]);

    let permissive = detect_contradiction_clusters_with_policy(&graph, permissive_policy);
    let strict = detect_contradiction_clusters_with_policy(&graph, strict_policy);
    if permissive.len() != 1 {
        return Err(format!(
            "permissive contradiction threshold expected 1 cluster, got {}",
            permissive.len()
        ));
    }
    if !strict.is_empty() {
        return Err(format!(
            "strict contradiction threshold expected 0 clusters, got {}",
            strict.len()
        ));
    }

    let cluster = &permissive[0];
    assert_json_snapshot!(
        "contradiction_threshold_config_behavior",
        json!({
            "schema": "ee.graph.config_behavior.v1",
            "surface": "graph.health.contradiction_threshold",
            "permissiveThreshold": permissive_policy.density_threshold,
            "strictThreshold": strict_policy.density_threshold,
            "permissiveClusterCount": permissive.len(),
            "strictClusterCount": strict.len(),
            "boundaryCluster": {
                "size": cluster.size,
                "internalContradictions": cluster.internal_contradictions,
                "densityPpm": (cluster.density * 1_000_000.0).round() as i64,
                "severity": cluster.severity,
                "exemplars": &cluster.exemplar_memory_ids,
            },
        })
    );
    Ok(())
}

#[test]
fn gomory_hu_sampling_config_changes_exact_vs_sampled_choice() -> TestResult {
    let default_config = merged_graph_config("")?;
    let sampled_config = merged_graph_config(
        r#"
[graph.gomory_hu]
sample_threshold = 3
sample_size = 2
"#,
    )?;

    let default_policy = SamplingPolicy::from_optional_sample_config(
        default_config.gomory_hu.sample_threshold,
        default_config.gomory_hu.sample_size,
    );
    let sampled_policy = SamplingPolicy::from_optional_sample_config(
        sampled_config.gomory_hu.sample_threshold,
        sampled_config.gomory_hu.sample_size,
    );

    let exact = run_with_sampling_policy(
        "gomory_hu",
        3,
        default_policy,
        7,
        || "exact",
        |_, _| "approximate",
    );
    let sampled = run_with_sampling_policy(
        "gomory_hu",
        3,
        sampled_policy,
        7,
        || "exact",
        |_, _| "approximate",
    );

    if exact.witness.choice != SamplingChoice::Exact {
        return Err(format!("default policy chose {:?}", exact.witness.choice));
    }
    if sampled.witness.choice != SamplingChoice::Approximate {
        return Err(format!("sampled policy chose {:?}", sampled.witness.choice));
    }
    if sampled.witness.effective_sample_size != 2 {
        return Err(format!(
            "sampled policy expected 2 pivots, got {}",
            sampled.witness.effective_sample_size
        ));
    }

    assert_json_snapshot!(
        "gomory_hu_sampling_config_behavior",
        json!({
            "schema": "ee.graph.config_behavior.v1",
            "surface": "graph.gomory_hu.sample_threshold",
            "nodeCount": 3,
            "snapshotVersion": 7,
            "default": {
                "threshold": exact.witness.sample_threshold,
                "requestedSampleSize": exact.witness.requested_sample_size,
                "choice": exact.witness.choice,
                "effectiveSampleSize": exact.witness.effective_sample_size,
                "result": exact.result,
            },
            "override": {
                "threshold": sampled.witness.sample_threshold,
                "requestedSampleSize": sampled.witness.requested_sample_size,
                "choice": sampled.witness.choice,
                "effectiveSampleSize": sampled.witness.effective_sample_size,
                "result": sampled.result,
            },
        })
    );
    Ok(())
}
