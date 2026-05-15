//! Bipartite HITS wrapper for the rule↔memory provenance projection
//! (bd-2jl2.1 / G9.a).
//!
//! The rule-provenance bipartite graph carries two partitions tagged
//! with the `bipartite_partition` node attribute: `rule` for rule nodes
//! and `memory` for memory nodes. Every edge crosses the partition by
//! construction (see `build_rule_provenance_bipartite_from_tables` in
//! `crate::graph`). This wrapper runs the underlying HITS power
//! iteration over the undirected projection and then partitions the
//! resulting scores so callers receive:
//!
//! - `authorities` keyed by memory ID — memories that many rules cite.
//! - `hubs` keyed by rule ID — rules that anchor against many memories.
//!
//! Determinism follows from the BTreeMap output; the same bipartite
//! graph yields byte-identical scores across repeated runs (J7).
//!
//! Snapshot persistence and downstream surfaces (G9.b `ee why
//! loadBearing` + `ee curate load-bearing protection`, G9.c `ee rule
//! provenance` + insights sections) consume this same `BipartiteHits`
//! shape.

use std::collections::BTreeMap;

use asupersync::Cx;
use fnx_algorithms::hits_centrality;
use fnx_runtime::CgseValue;

use crate::graph::Graph;
use crate::graph::GraphResult;
use crate::graph::algorithms::{DEFAULT_BACKGROUND_BUDGET, current_or_testing_cx, run_with_budget};

/// Node attribute key on the bipartite rule↔memory projection that
/// tags each node with its partition (`rule` or `memory`). Mirrors the
/// constant used inside `build_rule_provenance_bipartite_from_rows`.
pub const BIPARTITE_PARTITION_ATTR: &str = "bipartite_partition";
/// Partition value for rule nodes.
pub const BIPARTITE_PARTITION_RULE: &str = "rule";
/// Partition value for memory nodes.
pub const BIPARTITE_PARTITION_MEMORY: &str = "memory";

/// Partitioned HITS scores for a rule↔memory bipartite projection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BipartiteHits {
    /// Authority scores keyed by memory ID. Memories scored by how
    /// many rules anchor against them, weighted by the connecting
    /// rules' hub scores.
    pub authorities: BTreeMap<String, f64>,
    /// Hub scores keyed by rule ID. Rules scored by how many memories
    /// they cite, weighted by the connecting memories' authority
    /// scores.
    pub hubs: BTreeMap<String, f64>,
}

/// Compute partitioned HITS scores for the rule↔memory bipartite
/// projection.
///
/// Nodes without the `bipartite_partition` attribute are silently
/// dropped — the caller's responsibility is to pass the projection
/// built by `build_rule_provenance_bipartite_from_tables`, which tags
/// every node it adds.
pub fn compute_bipartite_hits(graph: &Graph) -> GraphResult<BipartiteHits> {
    let cx = current_or_testing_cx();
    compute_bipartite_hits_with_cx(&cx, graph)
}

pub fn compute_bipartite_hits_with_cx(cx: &Cx, graph: &Graph) -> GraphResult<BipartiteHits> {
    let graph = graph.clone();
    run_with_budget(cx, "bipartite_hits", DEFAULT_BACKGROUND_BUDGET, move || {
        let result = hits_centrality(&graph);
        let mut authorities: BTreeMap<String, f64> = BTreeMap::new();
        let mut hubs: BTreeMap<String, f64> = BTreeMap::new();
        for score in result.authorities {
            match partition_for(&graph, &score.node) {
                Some(BIPARTITE_PARTITION_MEMORY) => {
                    authorities.insert(score.node, score.score);
                }
                Some(BIPARTITE_PARTITION_RULE) => {
                    hubs.entry(score.node).or_insert(score.score);
                }
                _ => {}
            }
        }
        for score in result.hubs {
            match partition_for(&graph, &score.node) {
                Some(BIPARTITE_PARTITION_RULE) => {
                    hubs.insert(score.node, score.score);
                }
                Some(BIPARTITE_PARTITION_MEMORY) => {
                    authorities.entry(score.node).or_insert(score.score);
                }
                _ => {}
            }
        }
        BipartiteHits { authorities, hubs }
    })
}

fn partition_for<'a>(graph: &'a Graph, node: &str) -> Option<&'a str> {
    let attrs = graph.node_attrs(node)?;
    match attrs.get(BIPARTITE_PARTITION_ATTR)? {
        CgseValue::String(value) => Some(value.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fnx_classes::AttrMap;
    use fnx_runtime::CompatibilityMode;

    type TestResult = Result<(), String>;

    fn graph_result<T>(result: GraphResult<T>) -> Result<T, String> {
        result.map_err(|error| error.to_string())
    }

    fn partition_attrs(partition: &str) -> AttrMap {
        let mut attrs = AttrMap::new();
        attrs.insert(
            BIPARTITE_PARTITION_ATTR.to_owned(),
            CgseValue::String(partition.to_owned()),
        );
        attrs
    }

    fn add_rule(graph: &mut Graph, rule: &str) {
        graph.add_node_with_attrs(rule, partition_attrs(BIPARTITE_PARTITION_RULE));
    }

    fn add_memory(graph: &mut Graph, memory: &str) {
        graph.add_node_with_attrs(memory, partition_attrs(BIPARTITE_PARTITION_MEMORY));
    }

    fn link(graph: &mut Graph, rule: &str, memory: &str) {
        graph
            .add_edge_with_attrs(rule, memory, AttrMap::new())
            .unwrap_or_else(|error| panic!("test edge {rule}→{memory} should add: {error:?}"));
    }

    #[test]
    fn bipartite_hits_empty_graph_returns_empty_partitions() -> TestResult {
        let graph = Graph::new(CompatibilityMode::Strict);

        let result = graph_result(compute_bipartite_hits(&graph))?;

        assert!(
            result.authorities.is_empty(),
            "empty bipartite must yield empty authorities"
        );
        assert!(
            result.hubs.is_empty(),
            "empty bipartite must yield empty hubs"
        );
        Ok(())
    }

    #[test]
    fn bipartite_hits_single_rule_with_one_source_memory() -> TestResult {
        let mut graph = Graph::new(CompatibilityMode::Strict);
        add_rule(&mut graph, "rule_001");
        add_memory(&mut graph, "mem_001");
        link(&mut graph, "rule_001", "mem_001");

        let result = graph_result(compute_bipartite_hits(&graph))?;

        assert_eq!(
            result.authorities.len(),
            1,
            "single memory landed in authorities"
        );
        assert_eq!(result.hubs.len(), 1, "single rule landed in hubs");
        assert!(result.authorities.get("mem_001").copied().unwrap_or(0.0) > 0.0);
        assert!(result.hubs.get("rule_001").copied().unwrap_or(0.0) > 0.0);
        Ok(())
    }

    #[test]
    fn bipartite_hits_multi_rule_shared_memory_lifts_shared_authority() -> TestResult {
        // Three rules all cite mem_shared. mem_solo is cited by only one
        // rule. mem_shared must score higher as an authority than mem_solo.
        let mut graph = Graph::new(CompatibilityMode::Strict);
        for rule in ["rule_a", "rule_b", "rule_c"] {
            add_rule(&mut graph, rule);
        }
        add_memory(&mut graph, "mem_shared");
        add_memory(&mut graph, "mem_solo");
        link(&mut graph, "rule_a", "mem_shared");
        link(&mut graph, "rule_b", "mem_shared");
        link(&mut graph, "rule_c", "mem_shared");
        link(&mut graph, "rule_a", "mem_solo");

        let result = graph_result(compute_bipartite_hits(&graph))?;

        let shared = result.authorities.get("mem_shared").copied().unwrap_or(0.0);
        let solo = result.authorities.get("mem_solo").copied().unwrap_or(0.0);
        assert!(
            shared > solo,
            "shared memory must out-score solo memory as authority ({shared} vs {solo})"
        );
        // rule_a cites both memories so it has more connective load than
        // rule_b or rule_c, but at minimum every rule must register as a hub.
        for rule in ["rule_a", "rule_b", "rule_c"] {
            assert!(
                result.hubs.get(rule).copied().unwrap_or(0.0) > 0.0,
                "rule {rule} must register a positive hub score"
            );
        }
        Ok(())
    }

    #[test]
    fn bipartite_hits_isolated_memory_scores_uniformly_with_other_isolates() -> TestResult {
        // Isolated nodes (no incident edges) cannot accumulate HITS
        // weight through power iteration; they fall to the floor score.
        let mut graph = Graph::new(CompatibilityMode::Strict);
        add_rule(&mut graph, "rule_main");
        add_memory(&mut graph, "mem_connected");
        add_memory(&mut graph, "mem_isolated");
        link(&mut graph, "rule_main", "mem_connected");

        let result = graph_result(compute_bipartite_hits(&graph))?;

        let connected = result
            .authorities
            .get("mem_connected")
            .copied()
            .unwrap_or(0.0);
        let isolated = result
            .authorities
            .get("mem_isolated")
            .copied()
            .unwrap_or(0.0);
        assert!(
            connected > isolated,
            "connected memory must out-score isolated memory ({connected} vs {isolated})"
        );
        Ok(())
    }

    #[test]
    fn bipartite_hits_isolated_rule_falls_below_connected_rules() -> TestResult {
        // Symmetric to the isolated-memory case: a rule with no incident
        // memory edges cannot accumulate hub weight.
        let mut graph = Graph::new(CompatibilityMode::Strict);
        add_rule(&mut graph, "rule_connected");
        add_rule(&mut graph, "rule_isolated");
        add_memory(&mut graph, "mem_anchor");
        link(&mut graph, "rule_connected", "mem_anchor");

        let result = graph_result(compute_bipartite_hits(&graph))?;

        let connected = result.hubs.get("rule_connected").copied().unwrap_or(0.0);
        let isolated = result.hubs.get("rule_isolated").copied().unwrap_or(0.0);
        assert!(
            connected > isolated,
            "connected rule must out-score isolated rule ({connected} vs {isolated})"
        );
        // Determinism sanity check: a second run produces the same result.
        let second = graph_result(compute_bipartite_hits(&graph))?;
        assert_eq!(result, second, "bipartite HITS must be deterministic");
        Ok(())
    }
}
