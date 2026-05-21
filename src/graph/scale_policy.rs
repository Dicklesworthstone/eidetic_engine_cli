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
