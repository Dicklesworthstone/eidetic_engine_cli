//! Scale-admission policy for expensive graph insight algorithms.
//!
//! This module is deliberately pure: callers can ask what should happen for a
//! graph shape before allocating a large projection or running an expensive
//! algorithm. The policy is used by tests and benchmark gates for bd-bife.17
//! and is suitable for wiring into future `ee insights` runtime admission.

use serde::Serialize;

pub const GRAPH_SCALE_POLICY_SCHEMA_V1: &str = "ee.graph.scale_policy.v1";
pub const INSIGHTS_100K_BUDGET_MS: u64 = 5_000;

pub const GOMORY_HU_SKIP_THRESHOLD_NODES: usize = 2_000;
pub const ALL_PAIRS_LCA_LAZY_THRESHOLD_NODES: usize = 1_000;
pub const SIMRANK_JACCARD_THRESHOLD_NODES: usize = 500;
pub const CAUSAL_DEPTH_CAP: usize = 10;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphScaleAlgorithm {
    PersonalizedPageRank,
    Hits,
    PageRank,
    Betweenness,
    CommunicabilityBetweenness,
    KTruss,
    Louvain,
    OnionLayers,
    ArticulationPoints,
    GomoryHu,
    VoronoiCells,
    EgoGraph,
    TransitiveClosure,
    MinCostFlow,
    DominanceFrontiers,
    AllPairsLca,
    SimRank,
}

impl GraphScaleAlgorithm {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PersonalizedPageRank => "personalized_pagerank",
            Self::Hits => "hits",
            Self::PageRank => "pagerank",
            Self::Betweenness => "betweenness",
            Self::CommunicabilityBetweenness => "communicability_betweenness",
            Self::KTruss => "k_truss",
            Self::Louvain => "louvain",
            Self::OnionLayers => "onion_layers",
            Self::ArticulationPoints => "articulation_points",
            Self::GomoryHu => "gomory_hu",
            Self::VoronoiCells => "voronoi_cells",
            Self::EgoGraph => "ego_graph",
            Self::TransitiveClosure => "transitive_closure",
            Self::MinCostFlow => "min_cost_flow",
            Self::DominanceFrontiers => "dominance_frontiers",
            Self::AllPairsLca => "all_pairs_lca",
            Self::SimRank => "simrank",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphScaleAction {
    RunExact,
    PivotSample,
    Skip,
    CapDepth,
    CapIterations,
    LazyOnDemand,
    FallbackJaccard,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphScaleDecision {
    pub schema: &'static str,
    pub algorithm: GraphScaleAlgorithm,
    pub action: GraphScaleAction,
    pub node_count: usize,
    pub edge_count: usize,
    pub degraded_code: Option<&'static str>,
    pub cap: Option<usize>,
    pub target_budget_ms: u64,
    pub reason: &'static str,
}

impl GraphScaleDecision {
    #[must_use]
    pub const fn runs_expensive_full_graph(self) -> bool {
        matches!(
            self.action,
            GraphScaleAction::RunExact | GraphScaleAction::PivotSample
        )
    }
}

#[must_use]
pub fn graph_scale_decision(
    algorithm: GraphScaleAlgorithm,
    node_count: usize,
    edge_count: usize,
) -> GraphScaleDecision {
    let (action, degraded_code, cap, target_budget_ms, reason) = match algorithm {
        GraphScaleAlgorithm::PersonalizedPageRank
        | GraphScaleAlgorithm::Hits
        | GraphScaleAlgorithm::PageRank
        | GraphScaleAlgorithm::KTruss
        | GraphScaleAlgorithm::Louvain
        | GraphScaleAlgorithm::OnionLayers
        | GraphScaleAlgorithm::ArticulationPoints
        | GraphScaleAlgorithm::VoronoiCells
        | GraphScaleAlgorithm::EgoGraph
        | GraphScaleAlgorithm::DominanceFrontiers => (
            GraphScaleAction::RunExact,
            None,
            None,
            250,
            "algorithm is linear, local, or already bounded enough for the scale fixture",
        ),
        GraphScaleAlgorithm::Betweenness | GraphScaleAlgorithm::CommunicabilityBetweenness => (
            GraphScaleAction::PivotSample,
            Some("graph_scale_pivot_sampled"),
            None,
            750,
            "centrality is approximated with deterministic pivots at scale",
        ),
        GraphScaleAlgorithm::GomoryHu if node_count > GOMORY_HU_SKIP_THRESHOLD_NODES => (
            GraphScaleAction::Skip,
            Some("graph_scale_gomory_hu_skipped"),
            Some(GOMORY_HU_SKIP_THRESHOLD_NODES),
            10,
            "Gomory-Hu tree construction is skipped above the node threshold",
        ),
        GraphScaleAlgorithm::GomoryHu => (
            GraphScaleAction::RunExact,
            None,
            Some(GOMORY_HU_SKIP_THRESHOLD_NODES),
            1_000,
            "Gomory-Hu is below the exact-build threshold",
        ),
        GraphScaleAlgorithm::TransitiveClosure => (
            GraphScaleAction::CapDepth,
            Some("causal_depth_capped"),
            Some(CAUSAL_DEPTH_CAP),
            250,
            "causal transitive closure is depth-capped at scale",
        ),
        GraphScaleAlgorithm::MinCostFlow => (
            GraphScaleAction::CapIterations,
            Some("graph_scale_min_cost_flow_iteration_capped"),
            None,
            500,
            "min-cost flow uses a deterministic iteration cap at scale",
        ),
        GraphScaleAlgorithm::AllPairsLca if node_count > ALL_PAIRS_LCA_LAZY_THRESHOLD_NODES => (
            GraphScaleAction::LazyOnDemand,
            Some("graph_scale_all_pairs_lca_lazy"),
            Some(ALL_PAIRS_LCA_LAZY_THRESHOLD_NODES),
            50,
            "all-pairs LCA is replaced by lazy pair queries above the node threshold",
        ),
        GraphScaleAlgorithm::AllPairsLca => (
            GraphScaleAction::RunExact,
            None,
            Some(ALL_PAIRS_LCA_LAZY_THRESHOLD_NODES),
            750,
            "all-pairs LCA is below the exact threshold",
        ),
        GraphScaleAlgorithm::SimRank if node_count > SIMRANK_JACCARD_THRESHOLD_NODES => (
            GraphScaleAction::FallbackJaccard,
            Some("graph_scale_simrank_jaccard_fallback"),
            Some(SIMRANK_JACCARD_THRESHOLD_NODES),
            100,
            "SimRank is replaced by deterministic Jaccard similarity above the node threshold",
        ),
        GraphScaleAlgorithm::SimRank => (
            GraphScaleAction::RunExact,
            None,
            Some(SIMRANK_JACCARD_THRESHOLD_NODES),
            750,
            "SimRank is below the exact threshold",
        ),
    };

    GraphScaleDecision {
        schema: GRAPH_SCALE_POLICY_SCHEMA_V1,
        algorithm,
        action,
        node_count,
        edge_count,
        degraded_code,
        cap,
        target_budget_ms,
        reason,
    }
}

#[must_use]
pub fn graph_scale_algorithms() -> &'static [GraphScaleAlgorithm] {
    &[
        GraphScaleAlgorithm::PersonalizedPageRank,
        GraphScaleAlgorithm::Hits,
        GraphScaleAlgorithm::PageRank,
        GraphScaleAlgorithm::Betweenness,
        GraphScaleAlgorithm::CommunicabilityBetweenness,
        GraphScaleAlgorithm::KTruss,
        GraphScaleAlgorithm::Louvain,
        GraphScaleAlgorithm::OnionLayers,
        GraphScaleAlgorithm::ArticulationPoints,
        GraphScaleAlgorithm::GomoryHu,
        GraphScaleAlgorithm::VoronoiCells,
        GraphScaleAlgorithm::EgoGraph,
        GraphScaleAlgorithm::TransitiveClosure,
        GraphScaleAlgorithm::MinCostFlow,
        GraphScaleAlgorithm::DominanceFrontiers,
        GraphScaleAlgorithm::AllPairsLca,
        GraphScaleAlgorithm::SimRank,
    ]
}

#[must_use]
pub fn graph_scale_decisions(node_count: usize, edge_count: usize) -> Vec<GraphScaleDecision> {
    graph_scale_algorithms()
        .iter()
        .copied()
        .map(|algorithm| graph_scale_decision(algorithm, node_count, edge_count))
        .collect()
}

#[must_use]
pub fn graph_scale_total_budget_ms(node_count: usize, edge_count: usize) -> u64 {
    graph_scale_decisions(node_count, edge_count)
        .into_iter()
        .map(|decision| decision.target_budget_ms)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    type TestResult = Result<(), String>;

    const ALL_ALGORITHMS: &[GraphScaleAlgorithm] = &[
        GraphScaleAlgorithm::PersonalizedPageRank,
        GraphScaleAlgorithm::Hits,
        GraphScaleAlgorithm::PageRank,
        GraphScaleAlgorithm::Betweenness,
        GraphScaleAlgorithm::CommunicabilityBetweenness,
        GraphScaleAlgorithm::KTruss,
        GraphScaleAlgorithm::Louvain,
        GraphScaleAlgorithm::OnionLayers,
        GraphScaleAlgorithm::ArticulationPoints,
        GraphScaleAlgorithm::GomoryHu,
        GraphScaleAlgorithm::VoronoiCells,
        GraphScaleAlgorithm::EgoGraph,
        GraphScaleAlgorithm::TransitiveClosure,
        GraphScaleAlgorithm::MinCostFlow,
        GraphScaleAlgorithm::DominanceFrontiers,
        GraphScaleAlgorithm::AllPairsLca,
        GraphScaleAlgorithm::SimRank,
    ];

    const ALWAYS_EXACT_ALGORITHMS: &[GraphScaleAlgorithm] = &[
        GraphScaleAlgorithm::PersonalizedPageRank,
        GraphScaleAlgorithm::Hits,
        GraphScaleAlgorithm::PageRank,
        GraphScaleAlgorithm::KTruss,
        GraphScaleAlgorithm::Louvain,
        GraphScaleAlgorithm::OnionLayers,
        GraphScaleAlgorithm::ArticulationPoints,
        GraphScaleAlgorithm::VoronoiCells,
        GraphScaleAlgorithm::EgoGraph,
        GraphScaleAlgorithm::DominanceFrontiers,
    ];

    fn ensure(condition: bool, context: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(context.into())
        }
    }

    fn ensure_equal<T: std::fmt::Debug + PartialEq>(
        actual: T,
        expected: T,
        context: &str,
    ) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn synthetic_decision(action: GraphScaleAction) -> GraphScaleDecision {
        GraphScaleDecision {
            schema: GRAPH_SCALE_POLICY_SCHEMA_V1,
            algorithm: GraphScaleAlgorithm::PageRank,
            action,
            node_count: 1,
            edge_count: 0,
            degraded_code: None,
            cap: None,
            target_budget_ms: 1,
            reason: "unit test synthetic decision",
        }
    }

    #[test]
    fn algorithm_catalog_is_complete_unique_and_stable() -> TestResult {
        ensure_equal(
            graph_scale_algorithms(),
            ALL_ALGORITHMS,
            "graph_scale_algorithms catalog",
        )?;

        let mut names = BTreeSet::new();
        for algorithm in graph_scale_algorithms() {
            let name = algorithm.as_str();
            ensure(!name.is_empty(), format!("{algorithm:?} as_str is empty"))?;
            ensure(
                name.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
                format!("{algorithm:?} as_str is not snake_case: {name}"),
            )?;
            ensure(
                names.insert(name),
                format!("duplicate algorithm name: {name}"),
            )?;
        }

        ensure_equal(
            names.len(),
            ALL_ALGORITHMS.len(),
            "unique algorithm name count",
        )
    }

    #[test]
    fn decisions_pin_schema_algorithm_inputs_and_budget_sum() -> TestResult {
        let node_count = 123;
        let edge_count = 456;
        let decisions = graph_scale_decisions(node_count, edge_count);

        ensure_equal(
            decisions.len(),
            ALL_ALGORITHMS.len(),
            "decision count matches algorithm count",
        )?;
        for (decision, expected_algorithm) in decisions.iter().zip(ALL_ALGORITHMS.iter()) {
            ensure_equal(
                decision.schema,
                GRAPH_SCALE_POLICY_SCHEMA_V1,
                "decision schema",
            )?;
            ensure_equal(
                decision.algorithm,
                *expected_algorithm,
                "decision algorithm order",
            )?;
            ensure_equal(decision.node_count, node_count, "decision node count")?;
            ensure_equal(decision.edge_count, edge_count, "decision edge count")?;
        }

        let summed_budget = decisions
            .iter()
            .map(|decision| decision.target_budget_ms)
            .sum::<u64>();
        ensure_equal(
            graph_scale_total_budget_ms(node_count, edge_count),
            summed_budget,
            "total budget equals decision budget sum",
        )
    }

    #[test]
    fn threshold_boundaries_are_strictly_greater_than_caps() -> TestResult {
        let gomory_exact = graph_scale_decision(
            GraphScaleAlgorithm::GomoryHu,
            GOMORY_HU_SKIP_THRESHOLD_NODES,
            1,
        );
        ensure_equal(
            gomory_exact.action,
            GraphScaleAction::RunExact,
            "Gomory-Hu at threshold",
        )?;
        ensure_equal(gomory_exact.degraded_code, None, "Gomory-Hu exact code")?;
        ensure_equal(
            gomory_exact.cap,
            Some(GOMORY_HU_SKIP_THRESHOLD_NODES),
            "Gomory-Hu exact cap",
        )?;

        let gomory_skipped = graph_scale_decision(
            GraphScaleAlgorithm::GomoryHu,
            GOMORY_HU_SKIP_THRESHOLD_NODES + 1,
            1,
        );
        ensure_equal(
            gomory_skipped.action,
            GraphScaleAction::Skip,
            "Gomory-Hu above threshold",
        )?;
        ensure_equal(
            gomory_skipped.degraded_code,
            Some("graph_scale_gomory_hu_skipped"),
            "Gomory-Hu degraded code",
        )?;

        let lca_exact = graph_scale_decision(
            GraphScaleAlgorithm::AllPairsLca,
            ALL_PAIRS_LCA_LAZY_THRESHOLD_NODES,
            1,
        );
        ensure_equal(
            lca_exact.action,
            GraphScaleAction::RunExact,
            "all-pairs LCA at threshold",
        )?;
        ensure_equal(lca_exact.degraded_code, None, "all-pairs LCA exact code")?;

        let lca_lazy = graph_scale_decision(
            GraphScaleAlgorithm::AllPairsLca,
            ALL_PAIRS_LCA_LAZY_THRESHOLD_NODES + 1,
            1,
        );
        ensure_equal(
            lca_lazy.action,
            GraphScaleAction::LazyOnDemand,
            "all-pairs LCA above threshold",
        )?;
        ensure_equal(
            lca_lazy.degraded_code,
            Some("graph_scale_all_pairs_lca_lazy"),
            "all-pairs LCA degraded code",
        )?;

        let simrank_exact = graph_scale_decision(
            GraphScaleAlgorithm::SimRank,
            SIMRANK_JACCARD_THRESHOLD_NODES,
            1,
        );
        ensure_equal(
            simrank_exact.action,
            GraphScaleAction::RunExact,
            "SimRank at threshold",
        )?;
        ensure_equal(simrank_exact.degraded_code, None, "SimRank exact code")?;

        let simrank_jaccard = graph_scale_decision(
            GraphScaleAlgorithm::SimRank,
            SIMRANK_JACCARD_THRESHOLD_NODES + 1,
            1,
        );
        ensure_equal(
            simrank_jaccard.action,
            GraphScaleAction::FallbackJaccard,
            "SimRank above threshold",
        )?;
        ensure_equal(
            simrank_jaccard.degraded_code,
            Some("graph_scale_simrank_jaccard_fallback"),
            "SimRank degraded code",
        )
    }

    #[test]
    fn always_exact_algorithms_remain_exact_at_large_scale() -> TestResult {
        for algorithm in ALWAYS_EXACT_ALGORITHMS {
            let decision = graph_scale_decision(*algorithm, 100_000, 250_000);
            ensure_equal(
                decision.action,
                GraphScaleAction::RunExact,
                &format!("{algorithm:?} action"),
            )?;
            ensure_equal(
                decision.degraded_code,
                None,
                &format!("{algorithm:?} degraded code"),
            )?;
            ensure(
                decision.runs_expensive_full_graph(),
                format!("{algorithm:?} should run an exact full graph algorithm"),
            )?;
        }
        Ok(())
    }

    #[test]
    fn expensive_full_graph_action_matrix_is_pinned() -> TestResult {
        for action in [GraphScaleAction::RunExact, GraphScaleAction::PivotSample] {
            ensure(
                synthetic_decision(action).runs_expensive_full_graph(),
                format!("{action:?} should be full-graph work"),
            )?;
        }

        for action in [
            GraphScaleAction::Skip,
            GraphScaleAction::CapDepth,
            GraphScaleAction::CapIterations,
            GraphScaleAction::LazyOnDemand,
            GraphScaleAction::FallbackJaccard,
        ] {
            ensure(
                !synthetic_decision(action).runs_expensive_full_graph(),
                format!("{action:?} should not be full-graph work"),
            )?;
        }

        Ok(())
    }

    #[test]
    fn bounded_non_exact_actions_and_degraded_codes_are_pinned() -> TestResult {
        let nodes = 100_000;
        let edges = 250_000;

        for algorithm in [
            GraphScaleAlgorithm::Betweenness,
            GraphScaleAlgorithm::CommunicabilityBetweenness,
        ] {
            let decision = graph_scale_decision(algorithm, nodes, edges);
            ensure_equal(
                decision.action,
                GraphScaleAction::PivotSample,
                &format!("{algorithm:?} action"),
            )?;
            ensure_equal(
                decision.degraded_code,
                Some("graph_scale_pivot_sampled"),
                &format!("{algorithm:?} degraded code"),
            )?;
        }

        let transitive = graph_scale_decision(GraphScaleAlgorithm::TransitiveClosure, nodes, edges);
        ensure_equal(
            transitive.action,
            GraphScaleAction::CapDepth,
            "transitive closure action",
        )?;
        ensure_equal(
            transitive.degraded_code,
            Some("causal_depth_capped"),
            "transitive closure degraded code",
        )?;
        ensure_equal(
            transitive.cap,
            Some(CAUSAL_DEPTH_CAP),
            "transitive closure cap",
        )?;

        let min_cost = graph_scale_decision(GraphScaleAlgorithm::MinCostFlow, nodes, edges);
        ensure_equal(
            min_cost.action,
            GraphScaleAction::CapIterations,
            "min-cost flow action",
        )?;
        ensure_equal(
            min_cost.degraded_code,
            Some("graph_scale_min_cost_flow_iteration_capped"),
            "min-cost flow degraded code",
        )
    }
}
