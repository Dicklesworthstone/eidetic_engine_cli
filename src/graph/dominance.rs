//! Dominance + LCA wrappers for the memory-revision DAG
//! (bd-a7mm.1 / G7.a).
//!
//! The revision DAG produced by `build_revision_dag_from_logical_ids`
//! captures the partial order of memory revisions: edges point from
//! older to newer rows within a shared `logical_id` chain, plus
//! `derived_from` cross-chain edges. These wrappers expose three
//! dominance/LCA primitives over that DAG:
//!
//! - [`compute_immediate_dominators`] — for every node reachable from
//!   `start`, return its immediate dominator. A node `D` is the
//!   immediate dominator of `N` when every path from `start` to `N`
//!   passes through `D` and `D` is the closest such node.
//! - [`compute_dominance_frontiers`] — for every node, the set of
//!   nodes whose immediate dominator is NOT an ancestor of the
//!   queried node but which can be reached from a successor that the
//!   queried node dominates. Useful for SSA-style "where does this
//!   revision's influence stop" reasoning.
//! - [`compute_all_pairs_lca`] — for every unordered pair of nodes,
//!   their lowest common ancestor (or `None` when no common ancestor
//!   exists in this DAG).
//!
//! All public maps are `BTreeMap` (or sorted `Vec`) so the same graph
//! plus the same input parameters produce byte-identical output (J7).
//!
//! Downstream surfaces (G7.b `ee why dominator`, G7.c
//! `ee.memory.impact_analysis.v1 schema + ee insights --section
//! revisionFrontiers`) consume the maps produced here.

use std::collections::BTreeMap;

use fnx_algorithms::{all_pairs_lowest_common_ancestor, dominance_frontiers, immediate_dominators};

use crate::graph::DiGraph;
use crate::graph::GraphResult;
use crate::graph::algorithms::{DEFAULT_BACKGROUND_BUDGET, run_with_budget};

/// Immediate dominators keyed by node ID. The underlying fnx contract
/// includes `start -> start`; nodes unreachable from `start` are omitted.
pub type ImmediateDominators = BTreeMap<String, String>;

/// Dominance frontier per node keyed by node ID. The value is the
/// sorted, deduplicated list of frontier node IDs.
pub type DominanceFrontiers = BTreeMap<String, Vec<String>>;

/// All-pairs LCA result keyed by `(left, right)` with `left <= right`
/// lexicographically, including self-pairs. The value is `Some(lca)`
/// when the two nodes share a common ancestor in the DAG, otherwise
/// `None`.
pub type AllPairsLca = BTreeMap<(String, String), Option<String>>;

/// Compute immediate dominators starting from `start`. Returns an
/// empty map when `start` is missing from the graph.
pub fn compute_immediate_dominators(
    graph: &DiGraph,
    start: &str,
) -> GraphResult<ImmediateDominators> {
    let graph = graph.clone();
    let start = start.to_owned();
    run_with_budget(
        "immediate_dominators",
        DEFAULT_BACKGROUND_BUDGET,
        move || {
            immediate_dominators(&graph, &start)
                .into_iter()
                .collect::<ImmediateDominators>()
        },
    )
}

/// Compute dominance frontiers starting from `start`. Each frontier
/// list is sorted and deduplicated for deterministic output.
pub fn compute_dominance_frontiers(
    graph: &DiGraph,
    start: &str,
) -> GraphResult<DominanceFrontiers> {
    let graph = graph.clone();
    let start = start.to_owned();
    run_with_budget(
        "dominance_frontiers",
        DEFAULT_BACKGROUND_BUDGET,
        move || {
            dominance_frontiers(&graph, &start)
                .into_iter()
                .map(|(node, mut frontier)| {
                    frontier.sort();
                    frontier.dedup();
                    (node, frontier)
                })
                .collect::<DominanceFrontiers>()
        },
    )
}

/// Compute LCA for every unordered pair of nodes in the graph,
/// including self-pairs. Each pair is stored canonically with `left <=
/// right`. Pairs without a common ancestor (e.g. disconnected
/// components) are stored with value `None`.
pub fn compute_all_pairs_lca(graph: &DiGraph) -> GraphResult<AllPairsLca> {
    let graph = graph.clone();
    run_with_budget("all_pairs_lca", DEFAULT_BACKGROUND_BUDGET, move || {
        let mut nodes: Vec<String> = graph
            .nodes_ordered()
            .into_iter()
            .map(ToOwned::to_owned)
            .collect();
        nodes.sort();
        nodes.dedup();
        let mut distinct_pairs: Vec<(String, String)> = Vec::new();
        let mut result: AllPairsLca = BTreeMap::new();
        for (index, left) in nodes.iter().enumerate() {
            result.insert((left.clone(), left.clone()), Some(left.clone()));
            for right in nodes.iter().skip(index + 1) {
                let pair = (left.clone(), right.clone());
                result.insert(pair.clone(), None);
                distinct_pairs.push(pair);
            }
        }
        for (pair, lca) in all_pairs_lowest_common_ancestor(&graph, &distinct_pairs) {
            result.insert(pair, Some(lca));
        }
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fnx_runtime::CompatibilityMode;

    type TestResult = Result<(), String>;

    fn graph_result<T>(result: GraphResult<T>) -> Result<T, String> {
        result.map_err(|error| error.to_string())
    }

    fn empty_digraph() -> DiGraph {
        DiGraph::new(CompatibilityMode::Strict)
    }

    fn add_edge(graph: &mut DiGraph, source: &str, target: &str) {
        graph
            .add_edge(source, target)
            .unwrap_or_else(|error| panic!("test edge {source}→{target} should add: {error:?}"));
    }

    #[test]
    fn immediate_dominators_linear_chain_walks_predecessors() -> TestResult {
        // a -> b -> c -> d : every node except `a` is dominated by its
        // immediate predecessor.
        let mut graph = empty_digraph();
        add_edge(&mut graph, "a", "b");
        add_edge(&mut graph, "b", "c");
        add_edge(&mut graph, "c", "d");

        let idoms = graph_result(compute_immediate_dominators(&graph, "a"))?;

        assert_eq!(idoms.get("b").map(String::as_str), Some("a"));
        assert_eq!(idoms.get("c").map(String::as_str), Some("b"));
        assert_eq!(idoms.get("d").map(String::as_str), Some("c"));
        // fnx includes the entry node as its own dominator.
        assert_eq!(idoms.get("a").map(String::as_str), Some("a"));
        Ok(())
    }

    #[test]
    fn immediate_dominators_branching_dag_picks_common_predecessor() -> TestResult {
        // Diamond:
        //   a -> b -> d
        //   a -> c -> d
        // d's idom is `a` because both b and c are reachable from a.
        let mut graph = empty_digraph();
        add_edge(&mut graph, "a", "b");
        add_edge(&mut graph, "a", "c");
        add_edge(&mut graph, "b", "d");
        add_edge(&mut graph, "c", "d");

        let idoms = graph_result(compute_immediate_dominators(&graph, "a"))?;

        assert_eq!(idoms.get("b").map(String::as_str), Some("a"));
        assert_eq!(idoms.get("c").map(String::as_str), Some("a"));
        assert_eq!(idoms.get("d").map(String::as_str), Some("a"));
        Ok(())
    }

    #[test]
    fn dominance_frontiers_chain_with_cross_derive_records_join() -> TestResult {
        // Diamond produces a frontier at the join node.
        //   a -> b -> d
        //   a -> c -> d
        // Frontier(b) and Frontier(c) both include d, because d is
        // reachable from each branch but is not dominated by either.
        let mut graph = empty_digraph();
        add_edge(&mut graph, "a", "b");
        add_edge(&mut graph, "a", "c");
        add_edge(&mut graph, "b", "d");
        add_edge(&mut graph, "c", "d");

        let frontiers = graph_result(compute_dominance_frontiers(&graph, "a"))?;

        assert_eq!(frontiers.get("b").cloned().unwrap_or_default(), vec!["d"]);
        assert_eq!(frontiers.get("c").cloned().unwrap_or_default(), vec!["d"]);
        // Nodes whose dominance frontier is empty must still be sorted
        // when present.
        if let Some(start_frontier) = frontiers.get("a") {
            let mut sorted = start_frontier.clone();
            sorted.sort();
            assert_eq!(*start_frontier, sorted);
        }
        Ok(())
    }

    #[test]
    fn dominance_frontiers_multi_root_returns_empty_for_unreachable() -> TestResult {
        // Two disjoint chains; computing frontiers from `a` ignores
        // the second chain entirely.
        let mut graph = empty_digraph();
        add_edge(&mut graph, "a", "b");
        add_edge(&mut graph, "x", "y");

        let frontiers = graph_result(compute_dominance_frontiers(&graph, "a"))?;

        // `b`'s frontier is empty (it has no successors).
        let b_frontier = frontiers.get("b").cloned().unwrap_or_default();
        assert!(
            b_frontier.is_empty(),
            "linear-chain tail has empty frontier; got {b_frontier:?}"
        );
        // Disconnected nodes are absent from the result rather than
        // mapped to bogus frontiers.
        assert!(
            !frontiers.contains_key("y"),
            "unreachable-from-start nodes must not appear in frontiers map"
        );
        Ok(())
    }

    #[test]
    fn all_pairs_lca_branching_chain_finds_join_ancestor() -> TestResult {
        // Diamond where every pair shares `a` as their lowest common
        // ancestor (a is the only top-of-DAG).
        let mut graph = empty_digraph();
        add_edge(&mut graph, "a", "b");
        add_edge(&mut graph, "a", "c");
        add_edge(&mut graph, "b", "d");
        add_edge(&mut graph, "c", "d");
        add_edge(&mut graph, "x", "y");

        let lca = graph_result(compute_all_pairs_lca(&graph))?;

        assert_eq!(
            lca.get(&("b".to_owned(), "b".to_owned()))
                .cloned()
                .unwrap_or(None),
            Some("b".to_owned()),
            "self-pairs must report the node itself as LCA"
        );
        assert_eq!(
            lca.get(&("b".to_owned(), "c".to_owned()))
                .cloned()
                .unwrap_or(None),
            Some("a".to_owned()),
            "siblings b and c must share ancestor a"
        );
        assert_eq!(
            lca.get(&("b".to_owned(), "d".to_owned()))
                .cloned()
                .unwrap_or(None),
            Some("b".to_owned()),
            "ancestor pair (b, d) must report b as LCA"
        );
        assert_eq!(
            lca.get(&("b".to_owned(), "x".to_owned()))
                .cloned()
                .unwrap_or(Some("unexpected".to_owned())),
            None,
            "disconnected roots must not invent an LCA"
        );
        // Canonical key order: every stored pair has left <= right.
        for (left, right) in lca.keys() {
            assert!(
                left <= right,
                "all_pairs LCA keys must be canonical ({left}, {right})"
            );
        }
        Ok(())
    }

    #[test]
    fn dominance_wrappers_are_deterministic_across_three_runs() -> TestResult {
        // Slightly larger DAG so the determinism check is not trivial.
        let mut graph = empty_digraph();
        add_edge(&mut graph, "root", "a");
        add_edge(&mut graph, "root", "b");
        add_edge(&mut graph, "a", "c");
        add_edge(&mut graph, "b", "c");
        add_edge(&mut graph, "c", "leaf");
        add_edge(&mut graph, "a", "leaf");

        let first_idoms = graph_result(compute_immediate_dominators(&graph, "root"))?;
        let second_idoms = graph_result(compute_immediate_dominators(&graph, "root"))?;
        let third_idoms = graph_result(compute_immediate_dominators(&graph, "root"))?;
        assert_eq!(first_idoms, second_idoms);
        assert_eq!(second_idoms, third_idoms);

        let first_frontiers = graph_result(compute_dominance_frontiers(&graph, "root"))?;
        let second_frontiers = graph_result(compute_dominance_frontiers(&graph, "root"))?;
        assert_eq!(first_frontiers, second_frontiers);

        let first_lca = graph_result(compute_all_pairs_lca(&graph))?;
        let second_lca = graph_result(compute_all_pairs_lca(&graph))?;
        assert_eq!(first_lca, second_lca);

        Ok(())
    }
}
