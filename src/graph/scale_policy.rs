//! Scale-admission policy for expensive graph insight algorithms.
//!
//! This module is deliberately pure: callers can ask what should happen for a
//! graph shape before allocating a large projection or running an expensive
//! algorithm. The policy is used by tests and benchmark gates for bd-bife.17
//! and is suitable for wiring into future `ee insights` runtime admission.

use serde::Serialize;

pub const GRAPH_SCALE_POLICY_SCHEMA_V1: &str = "ee.graph.scale_policy.v1";
pub const SCALE_LOCALITY_ADVISOR_SCHEMA_V1: &str = "ee.scale_envelope.locality_advisor.v1";
pub const SCALE_LOCALITY_REDACTION_STATUS: &str = "counts_hashes_paths_no_content";
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleLocalityPlatform {
    Linux,
    Macos,
    Windows,
    Other,
    Unknown,
}

impl ScaleLocalityPlatform {
    const fn has_measurable_numa(self) -> bool {
        matches!(self, Self::Linux)
    }

    const fn fallback_name(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Macos => "macos_platform_fallback",
            Self::Windows => "windows_platform_fallback",
            Self::Other => "other_platform_fallback",
            Self::Unknown => "unknown_platform_fallback",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleLocalityTopologyKind {
    MultiSocketNuma,
    SingleSocket,
    PlatformFallback,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleLocalityReadPoolState {
    Adequate,
    Undersized,
    Saturated,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleLocalityQueueState {
    Idle,
    Moderate,
    Saturated,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleLocalityAffinity {
    Strong,
    Acceptable,
    Weak,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleLocalityContention {
    Low,
    Moderate,
    High,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleLocalityShardStrategy {
    NumaShardGroups,
    SingleSocketCoLocated,
    PlatformFallbackRoundRobin,
    ConservativeSingleShard,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleLocalitySurface {
    Search,
    Pack,
    GraphProjection,
    LargeCorpusMaintenance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleLocalityPlanAction {
    UseNumaShardGroups,
    CoLocateShardGroups,
    UsePlatformFallback,
    IncreaseReadPool,
    PrewarmHotset,
    StaggerMaintenance,
    ReduceCrossPoolConcurrency,
    ConservativeSingleShard,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScaleLocalityAdvisorInput {
    pub fixture_profile_id: String,
    pub platform: ScaleLocalityPlatform,
    pub logical_cpu_count: Option<u16>,
    pub physical_core_count: Option<u16>,
    pub numa_node_count: Option<u16>,
    pub read_pool_size: u16,
    pub shard_count: u16,
    pub hotset_entry_count: u32,
    pub hotset_bytes: u64,
    pub resource_admission_queue_depth: u32,
    pub read_pool_queue_depth: u32,
    pub maintenance_queue_depth: u32,
    pub search_worker_count: u16,
    pub pack_worker_count: u16,
    pub graph_worker_count: u16,
    pub maintenance_worker_count: u16,
}

impl ScaleLocalityAdvisorInput {
    #[must_use]
    pub fn high_core_numa_fixture() -> Self {
        Self {
            fixture_profile_id: "scale_locality_high_core_numa".to_owned(),
            platform: ScaleLocalityPlatform::Linux,
            logical_cpu_count: Some(128),
            physical_core_count: Some(64),
            numa_node_count: Some(2),
            read_pool_size: 12,
            shard_count: 8,
            hotset_entry_count: 8_192,
            hotset_bytes: 512 * 1024 * 1024,
            resource_admission_queue_depth: 8,
            read_pool_queue_depth: 6,
            maintenance_queue_depth: 2,
            search_worker_count: 32,
            pack_worker_count: 16,
            graph_worker_count: 8,
            maintenance_worker_count: 4,
        }
    }

    #[must_use]
    pub fn single_socket_fixture() -> Self {
        Self {
            fixture_profile_id: "scale_locality_single_socket".to_owned(),
            platform: ScaleLocalityPlatform::Linux,
            logical_cpu_count: Some(16),
            physical_core_count: Some(8),
            numa_node_count: Some(1),
            read_pool_size: 4,
            shard_count: 2,
            hotset_entry_count: 1_024,
            hotset_bytes: 64 * 1024 * 1024,
            resource_admission_queue_depth: 1,
            read_pool_queue_depth: 1,
            maintenance_queue_depth: 0,
            search_worker_count: 4,
            pack_worker_count: 2,
            graph_worker_count: 1,
            maintenance_worker_count: 0,
        }
    }

    #[must_use]
    pub fn macos_platform_fallback_fixture() -> Self {
        Self {
            fixture_profile_id: "scale_locality_macos_fallback".to_owned(),
            platform: ScaleLocalityPlatform::Macos,
            logical_cpu_count: Some(12),
            physical_core_count: Some(8),
            numa_node_count: None,
            read_pool_size: 2,
            shard_count: 2,
            hotset_entry_count: 256,
            hotset_bytes: 16 * 1024 * 1024,
            resource_admission_queue_depth: 1,
            read_pool_queue_depth: 2,
            maintenance_queue_depth: 1,
            search_worker_count: 4,
            pack_worker_count: 2,
            graph_worker_count: 1,
            maintenance_worker_count: 1,
        }
    }

    #[must_use]
    pub fn unknown_topology_fixture() -> Self {
        Self {
            fixture_profile_id: "scale_locality_unknown_topology".to_owned(),
            platform: ScaleLocalityPlatform::Unknown,
            logical_cpu_count: None,
            physical_core_count: None,
            numa_node_count: None,
            read_pool_size: 1,
            shard_count: 1,
            hotset_entry_count: 0,
            hotset_bytes: 0,
            resource_admission_queue_depth: 0,
            read_pool_queue_depth: 0,
            maintenance_queue_depth: 0,
            search_worker_count: 1,
            pack_worker_count: 1,
            graph_worker_count: 0,
            maintenance_worker_count: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaleLocalityAdvisorReport {
    pub schema: &'static str,
    pub fixture_profile_id: String,
    pub redaction_status: &'static str,
    pub host: ScaleLocalityHostReport,
    pub topology: ScaleLocalityTopologyReport,
    pub read_pool: ScaleLocalityReadPoolReport,
    pub shard_placement: ScaleLocalityShardPlacementReport,
    pub queue_pressure: ScaleLocalityQueuePressureReport,
    pub cache_affinity: ScaleLocalityCacheAffinityReport,
    pub cross_pool_contention: ScaleLocalityCrossPoolContentionReport,
    pub execution_plan: Vec<ScaleLocalityExecutionPlanStep>,
    pub degraded: Vec<ScaleLocalityDegradedEntry>,
    pub provenance: Vec<ScaleLocalityProvenanceRef>,
}

impl ScaleLocalityAdvisorReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        crate::core::serialize_or_error(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaleLocalityHostReport {
    pub platform: ScaleLocalityPlatform,
    pub logical_cpu_count: Option<u16>,
    pub physical_core_count: Option<u16>,
    pub effective_cpu_capacity: u16,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaleLocalityTopologyReport {
    pub kind: ScaleLocalityTopologyKind,
    pub numa_node_count: Option<u16>,
    pub measured: bool,
    pub fallback: Option<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaleLocalityReadPoolReport {
    pub state: ScaleLocalityReadPoolState,
    pub current_size: u16,
    pub recommended_size: u16,
    pub active_queue_depth: u32,
    pub rationale: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaleLocalityShardPlacementReport {
    pub strategy: ScaleLocalityShardStrategy,
    pub shard_count: u16,
    pub recommended_shard_groups: u16,
    pub shards_per_group: u16,
    pub rationale: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaleLocalityQueuePressureReport {
    pub state: ScaleLocalityQueueState,
    pub resource_admission_queue_depth: u32,
    pub read_pool_queue_depth: u32,
    pub maintenance_queue_depth: u32,
    pub combined_queue_depth: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaleLocalityCacheAffinityReport {
    pub affinity: ScaleLocalityAffinity,
    pub hotset_entry_count: u32,
    pub hotset_bytes: u64,
    pub rationale: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaleLocalityCrossPoolContentionReport {
    pub state: ScaleLocalityContention,
    pub total_worker_count: u16,
    pub search_worker_count: u16,
    pub pack_worker_count: u16,
    pub graph_worker_count: u16,
    pub maintenance_worker_count: u16,
    pub rationale: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaleLocalityExecutionPlanStep {
    pub surface: ScaleLocalitySurface,
    pub action: ScaleLocalityPlanAction,
    pub recommended_workers: u16,
    pub shard_group: Option<u16>,
    pub cache_policy: &'static str,
    pub rationale: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaleLocalityDegradedEntry {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: &'static str,
    pub repair: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaleLocalityProvenanceRef {
    pub kind: &'static str,
    #[serde(rename = "ref")]
    pub reference: String,
    pub hash: Option<String>,
}

#[must_use]
pub fn advise_scale_locality(input: &ScaleLocalityAdvisorInput) -> ScaleLocalityAdvisorReport {
    let effective_cpu_capacity = input.logical_cpu_count.unwrap_or(1).max(1);
    let topology = scale_locality_topology(input);
    let recommended_read_pool_size = recommended_scale_read_pool_size(input);
    let queue_pressure = scale_locality_queue_pressure(input);
    let read_pool =
        scale_locality_read_pool(input, recommended_read_pool_size, queue_pressure.state);
    let shard_placement = scale_locality_shard_placement(input, &topology);
    let cache_affinity = scale_locality_cache_affinity(input, &topology);
    let cross_pool_contention = scale_locality_cross_pool_contention(input, queue_pressure.state);
    let execution_plan = scale_locality_execution_plan(
        input,
        &topology,
        &read_pool,
        &cache_affinity,
        &cross_pool_contention,
    );
    let degraded = scale_locality_degraded_entries(
        input,
        &topology,
        &read_pool,
        &cache_affinity,
        &cross_pool_contention,
    );

    ScaleLocalityAdvisorReport {
        schema: SCALE_LOCALITY_ADVISOR_SCHEMA_V1,
        fixture_profile_id: input.fixture_profile_id.clone(),
        redaction_status: SCALE_LOCALITY_REDACTION_STATUS,
        host: ScaleLocalityHostReport {
            platform: input.platform,
            logical_cpu_count: input.logical_cpu_count,
            physical_core_count: input.physical_core_count,
            effective_cpu_capacity,
        },
        topology,
        read_pool,
        shard_placement,
        queue_pressure,
        cache_affinity,
        cross_pool_contention,
        execution_plan,
        degraded,
        provenance: scale_locality_provenance(input),
    }
}

fn scale_locality_topology(input: &ScaleLocalityAdvisorInput) -> ScaleLocalityTopologyReport {
    if input.platform.has_measurable_numa() {
        match input.numa_node_count {
            Some(nodes) if nodes >= 2 => ScaleLocalityTopologyReport {
                kind: ScaleLocalityTopologyKind::MultiSocketNuma,
                numa_node_count: Some(nodes),
                measured: true,
                fallback: None,
            },
            Some(1) => ScaleLocalityTopologyReport {
                kind: ScaleLocalityTopologyKind::SingleSocket,
                numa_node_count: Some(1),
                measured: true,
                fallback: None,
            },
            _ => ScaleLocalityTopologyReport {
                kind: ScaleLocalityTopologyKind::Unknown,
                numa_node_count: input.numa_node_count,
                measured: false,
                fallback: Some("linux_topology_unavailable"),
            },
        }
    } else if input.platform == ScaleLocalityPlatform::Unknown {
        ScaleLocalityTopologyReport {
            kind: ScaleLocalityTopologyKind::Unknown,
            numa_node_count: input.numa_node_count,
            measured: false,
            fallback: Some(input.platform.fallback_name()),
        }
    } else {
        ScaleLocalityTopologyReport {
            kind: ScaleLocalityTopologyKind::PlatformFallback,
            numa_node_count: input.numa_node_count,
            measured: false,
            fallback: Some(input.platform.fallback_name()),
        }
    }
}

fn recommended_scale_read_pool_size(input: &ScaleLocalityAdvisorInput) -> u16 {
    let shard_floor = input.shard_count.max(1).saturating_mul(2);
    let cpu_floor = input
        .logical_cpu_count
        .map_or(1, |logical| (logical / 8).max(1));
    let numa_floor = input.numa_node_count.unwrap_or(1).max(1);
    shard_floor.max(cpu_floor).max(numa_floor).min(32)
}

fn scale_locality_queue_pressure(
    input: &ScaleLocalityAdvisorInput,
) -> ScaleLocalityQueuePressureReport {
    let combined_queue_depth = input
        .resource_admission_queue_depth
        .saturating_add(input.read_pool_queue_depth)
        .saturating_add(input.maintenance_queue_depth);
    let pool_capacity = u32::from(input.read_pool_size.max(1));
    let cpu_capacity = input.logical_cpu_count.map(u32::from);
    let state = if input.logical_cpu_count.is_none()
        && input.resource_admission_queue_depth == 0
        && input.read_pool_queue_depth == 0
        && input.maintenance_queue_depth == 0
    {
        ScaleLocalityQueueState::Unknown
    } else if combined_queue_depth >= pool_capacity.saturating_mul(2)
        || cpu_capacity.is_some_and(|capacity| combined_queue_depth >= capacity)
    {
        ScaleLocalityQueueState::Saturated
    } else if combined_queue_depth > pool_capacity {
        ScaleLocalityQueueState::Moderate
    } else {
        ScaleLocalityQueueState::Idle
    };

    ScaleLocalityQueuePressureReport {
        state,
        resource_admission_queue_depth: input.resource_admission_queue_depth,
        read_pool_queue_depth: input.read_pool_queue_depth,
        maintenance_queue_depth: input.maintenance_queue_depth,
        combined_queue_depth,
    }
}

fn scale_locality_read_pool(
    input: &ScaleLocalityAdvisorInput,
    recommended_size: u16,
    queue_state: ScaleLocalityQueueState,
) -> ScaleLocalityReadPoolReport {
    let state = if input.logical_cpu_count.is_none() && input.read_pool_size == 0 {
        ScaleLocalityReadPoolState::Unknown
    } else if queue_state == ScaleLocalityQueueState::Saturated {
        ScaleLocalityReadPoolState::Saturated
    } else if input.read_pool_size < recommended_size {
        ScaleLocalityReadPoolState::Undersized
    } else {
        ScaleLocalityReadPoolState::Adequate
    };
    let rationale = match state {
        ScaleLocalityReadPoolState::Adequate => {
            "read pool is large enough for the shard and CPU envelope"
        }
        ScaleLocalityReadPoolState::Undersized => {
            "read pool is below the deterministic shard and CPU floor"
        }
        ScaleLocalityReadPoolState::Saturated => {
            "queue pressure exceeds the current read pool capacity envelope"
        }
        ScaleLocalityReadPoolState::Unknown => "host CPU and read-pool capacity are unavailable",
    };

    ScaleLocalityReadPoolReport {
        state,
        current_size: input.read_pool_size,
        recommended_size,
        active_queue_depth: input.read_pool_queue_depth,
        rationale,
    }
}

fn scale_locality_shard_placement(
    input: &ScaleLocalityAdvisorInput,
    topology: &ScaleLocalityTopologyReport,
) -> ScaleLocalityShardPlacementReport {
    let (strategy, recommended_shard_groups, rationale) = match topology.kind {
        ScaleLocalityTopologyKind::MultiSocketNuma => (
            ScaleLocalityShardStrategy::NumaShardGroups,
            topology.numa_node_count.unwrap_or(2).max(2),
            "place shard groups by NUMA node and keep read-pool workers local to their group",
        ),
        ScaleLocalityTopologyKind::SingleSocket => (
            ScaleLocalityShardStrategy::SingleSocketCoLocated,
            1,
            "single-socket hosts should co-locate shard groups and avoid fake NUMA partitions",
        ),
        ScaleLocalityTopologyKind::PlatformFallback => (
            ScaleLocalityShardStrategy::PlatformFallbackRoundRobin,
            1,
            "platform does not expose NUMA data, so use stable round-robin placement",
        ),
        ScaleLocalityTopologyKind::Unknown => (
            ScaleLocalityShardStrategy::ConservativeSingleShard,
            1,
            "topology is unknown, so keep placement conservative until host data is available",
        ),
    };
    let shards_per_group = ceil_div_u16(input.shard_count.max(1), recommended_shard_groups.max(1));

    ScaleLocalityShardPlacementReport {
        strategy,
        shard_count: input.shard_count,
        recommended_shard_groups,
        shards_per_group,
        rationale,
    }
}

fn scale_locality_cache_affinity(
    input: &ScaleLocalityAdvisorInput,
    topology: &ScaleLocalityTopologyReport,
) -> ScaleLocalityCacheAffinityReport {
    let affinity = if input.hotset_entry_count == 0 {
        ScaleLocalityAffinity::Weak
    } else {
        match topology.kind {
            ScaleLocalityTopologyKind::MultiSocketNuma
                if input.shard_count >= topology.numa_node_count.unwrap_or(2).saturating_mul(2) =>
            {
                ScaleLocalityAffinity::Strong
            }
            ScaleLocalityTopologyKind::MultiSocketNuma
            | ScaleLocalityTopologyKind::SingleSocket => ScaleLocalityAffinity::Acceptable,
            ScaleLocalityTopologyKind::PlatformFallback => ScaleLocalityAffinity::Weak,
            ScaleLocalityTopologyKind::Unknown => ScaleLocalityAffinity::Unknown,
        }
    };
    let rationale = match affinity {
        ScaleLocalityAffinity::Strong => {
            "hotset and shard counts are sufficient to prewarm cache by locality group"
        }
        ScaleLocalityAffinity::Acceptable => {
            "hotset is present, but locality grouping is bounded by the host topology"
        }
        ScaleLocalityAffinity::Weak => {
            "hotset data is absent or cannot be tied to measured locality groups"
        }
        ScaleLocalityAffinity::Unknown => {
            "cache affinity cannot be classified without topology data"
        }
    };

    ScaleLocalityCacheAffinityReport {
        affinity,
        hotset_entry_count: input.hotset_entry_count,
        hotset_bytes: input.hotset_bytes,
        rationale,
    }
}

fn scale_locality_cross_pool_contention(
    input: &ScaleLocalityAdvisorInput,
    queue_state: ScaleLocalityQueueState,
) -> ScaleLocalityCrossPoolContentionReport {
    let total_worker_count = input
        .search_worker_count
        .saturating_add(input.pack_worker_count)
        .saturating_add(input.graph_worker_count)
        .saturating_add(input.maintenance_worker_count);
    let state = match input.logical_cpu_count {
        None => ScaleLocalityContention::Unknown,
        Some(logical) if queue_state == ScaleLocalityQueueState::Saturated => {
            let saturation_floor = logical.saturating_mul(3) / 2;
            if total_worker_count > saturation_floor {
                ScaleLocalityContention::High
            } else {
                ScaleLocalityContention::Moderate
            }
        }
        Some(logical) if total_worker_count > logical => ScaleLocalityContention::High,
        Some(_)
            if queue_state == ScaleLocalityQueueState::Moderate
                || (input.graph_worker_count > 0 && input.maintenance_worker_count > 0) =>
        {
            ScaleLocalityContention::Moderate
        }
        Some(_) => ScaleLocalityContention::Low,
    };
    let rationale = match state {
        ScaleLocalityContention::Low => {
            "worker pools fit under the host CPU envelope with low queue pressure"
        }
        ScaleLocalityContention::Moderate => {
            "foreground and maintenance pools should be staggered to preserve read locality"
        }
        ScaleLocalityContention::High => {
            "combined worker pools exceed the CPU envelope or saturated queue pressure"
        }
        ScaleLocalityContention::Unknown => {
            "worker contention cannot be classified without CPU capacity"
        }
    };

    ScaleLocalityCrossPoolContentionReport {
        state,
        total_worker_count,
        search_worker_count: input.search_worker_count,
        pack_worker_count: input.pack_worker_count,
        graph_worker_count: input.graph_worker_count,
        maintenance_worker_count: input.maintenance_worker_count,
        rationale,
    }
}

fn scale_locality_execution_plan(
    input: &ScaleLocalityAdvisorInput,
    topology: &ScaleLocalityTopologyReport,
    read_pool: &ScaleLocalityReadPoolReport,
    cache_affinity: &ScaleLocalityCacheAffinityReport,
    contention: &ScaleLocalityCrossPoolContentionReport,
) -> Vec<ScaleLocalityExecutionPlanStep> {
    let preferred_group =
        (topology.kind == ScaleLocalityTopologyKind::MultiSocketNuma).then_some(0);
    let search_action = if read_pool.state == ScaleLocalityReadPoolState::Undersized {
        ScaleLocalityPlanAction::IncreaseReadPool
    } else {
        placement_action(topology.kind)
    };
    let pack_action = if input.hotset_entry_count > 0 {
        ScaleLocalityPlanAction::PrewarmHotset
    } else {
        placement_action(topology.kind)
    };
    let graph_action = placement_action(topology.kind);
    let maintenance_action = match contention.state {
        ScaleLocalityContention::High => ScaleLocalityPlanAction::ReduceCrossPoolConcurrency,
        ScaleLocalityContention::Moderate => ScaleLocalityPlanAction::StaggerMaintenance,
        _ => placement_action(topology.kind),
    };

    vec![
        ScaleLocalityExecutionPlanStep {
            surface: ScaleLocalitySurface::Search,
            action: search_action,
            recommended_workers: bounded_worker_count(
                input.logical_cpu_count,
                4,
                read_pool.recommended_size,
            ),
            shard_group: preferred_group,
            cache_policy: cache_policy(cache_affinity.affinity),
            rationale: "search should acquire read-pool slots near the shard group that owns its hotset",
        },
        ScaleLocalityExecutionPlanStep {
            surface: ScaleLocalitySurface::Pack,
            action: pack_action,
            recommended_workers: bounded_worker_count(
                input.logical_cpu_count,
                8,
                read_pool.recommended_size,
            ),
            shard_group: preferred_group,
            cache_policy: cache_policy(cache_affinity.affinity),
            rationale: "pack assembly should reuse warmed search candidates before broad graph projection",
        },
        ScaleLocalityExecutionPlanStep {
            surface: ScaleLocalitySurface::GraphProjection,
            action: graph_action,
            recommended_workers: topology.numa_node_count.unwrap_or(1).max(1),
            shard_group: preferred_group,
            cache_policy: "graph_snapshot_local_to_shard_group",
            rationale: "graph projections should run beside their shard group and avoid cross-socket scans",
        },
        ScaleLocalityExecutionPlanStep {
            surface: ScaleLocalitySurface::LargeCorpusMaintenance,
            action: maintenance_action,
            recommended_workers: if contention.state == ScaleLocalityContention::Low {
                2
            } else {
                1
            },
            shard_group: None,
            cache_policy: "maintenance_after_foreground_read_burst",
            rationale: "maintenance should not compete with foreground read-pool and hotset work",
        },
    ]
}

fn scale_locality_degraded_entries(
    input: &ScaleLocalityAdvisorInput,
    topology: &ScaleLocalityTopologyReport,
    read_pool: &ScaleLocalityReadPoolReport,
    cache_affinity: &ScaleLocalityCacheAffinityReport,
    contention: &ScaleLocalityCrossPoolContentionReport,
) -> Vec<ScaleLocalityDegradedEntry> {
    let mut entries = Vec::new();
    if topology.kind == ScaleLocalityTopologyKind::PlatformFallback {
        push_scale_locality_degraded(
            &mut entries,
            ScaleLocalityDegradedEntry {
                code: "scale_locality_platform_fallback",
                severity: "warning",
                message: "Host platform does not expose NUMA topology to this advisor.",
                repair: "Run the advisor on a Linux host with NUMA data for measured locality placement.",
            },
        );
    }
    if topology.kind == ScaleLocalityTopologyKind::Unknown {
        push_scale_locality_degraded(
            &mut entries,
            ScaleLocalityDegradedEntry {
                code: "scale_locality_topology_unknown",
                severity: "warning",
                message: "Host topology is unavailable, so placement is conservative.",
                repair: "Provide CPU and NUMA topology evidence before tuning shard placement.",
            },
        );
    }
    if matches!(
        read_pool.state,
        ScaleLocalityReadPoolState::Undersized | ScaleLocalityReadPoolState::Saturated
    ) {
        push_scale_locality_degraded(
            &mut entries,
            ScaleLocalityDegradedEntry {
                code: "read_pool_undersized",
                severity: "warning",
                message: "Read-pool capacity is below the locality advisor recommendation.",
                repair: "Increase storage.read_pool.size or lower foreground read concurrency.",
            },
        );
    }
    if input.hotset_entry_count == 0 || cache_affinity.affinity == ScaleLocalityAffinity::Weak {
        push_scale_locality_degraded(
            &mut entries,
            ScaleLocalityDegradedEntry {
                code: "hotset_prewarm_no_signals",
                severity: "low",
                message: "Hotset input cannot anchor cache affinity to shard placement.",
                repair: "Capture a current hotset manifest before measuring scale-envelope SLOs.",
            },
        );
    }
    if contention.state == ScaleLocalityContention::High {
        push_scale_locality_degraded(
            &mut entries,
            ScaleLocalityDegradedEntry {
                code: "scale_locality_cross_pool_contention",
                severity: "medium",
                message: "Search, pack, graph, and maintenance worker pools exceed the CPU envelope.",
                repair: "Stagger maintenance or reduce background graph/index workers during foreground reads.",
            },
        );
    }

    entries
}

fn push_scale_locality_degraded(
    entries: &mut Vec<ScaleLocalityDegradedEntry>,
    entry: ScaleLocalityDegradedEntry,
) {
    if !entries.iter().any(|existing| existing.code == entry.code) {
        entries.push(entry);
    }
}

fn scale_locality_provenance(input: &ScaleLocalityAdvisorInput) -> Vec<ScaleLocalityProvenanceRef> {
    vec![
        ScaleLocalityProvenanceRef {
            kind: "schema",
            reference: "ee.scale_envelope.v1".to_owned(),
            hash: Some(scale_locality_hash(input, "schema")),
        },
        ScaleLocalityProvenanceRef {
            kind: "bead",
            reference: "bd-ssoco.4".to_owned(),
            hash: None,
        },
        ScaleLocalityProvenanceRef {
            kind: "resource_admission",
            reference: format!(
                "queue_depths:{}/{}/{}",
                input.resource_admission_queue_depth,
                input.read_pool_queue_depth,
                input.maintenance_queue_depth
            ),
            hash: Some(scale_locality_hash(input, "resource-admission")),
        },
        ScaleLocalityProvenanceRef {
            kind: "hotset",
            reference: format!(
                "entries:{} bytes:{}",
                input.hotset_entry_count, input.hotset_bytes
            ),
            hash: Some(scale_locality_hash(input, "hotset")),
        },
    ]
}

fn placement_action(kind: ScaleLocalityTopologyKind) -> ScaleLocalityPlanAction {
    match kind {
        ScaleLocalityTopologyKind::MultiSocketNuma => ScaleLocalityPlanAction::UseNumaShardGroups,
        ScaleLocalityTopologyKind::SingleSocket => ScaleLocalityPlanAction::CoLocateShardGroups,
        ScaleLocalityTopologyKind::PlatformFallback => ScaleLocalityPlanAction::UsePlatformFallback,
        ScaleLocalityTopologyKind::Unknown => ScaleLocalityPlanAction::ConservativeSingleShard,
    }
}

fn cache_policy(affinity: ScaleLocalityAffinity) -> &'static str {
    match affinity {
        ScaleLocalityAffinity::Strong => "prewarm_hotset_by_shard_group",
        ScaleLocalityAffinity::Acceptable => "prewarm_hotset_before_foreground_reads",
        ScaleLocalityAffinity::Weak => "recapture_hotset_before_tuning",
        ScaleLocalityAffinity::Unknown => "measure_hotset_before_tuning",
    }
}

fn bounded_worker_count(logical_cpu_count: Option<u16>, divisor: u16, read_pool_limit: u16) -> u16 {
    let cpu_floor = logical_cpu_count.map_or(1, |logical| (logical / divisor.max(1)).max(1));
    cpu_floor.min(read_pool_limit.max(1))
}

fn ceil_div_u16(value: u16, divisor: u16) -> u16 {
    let value = u32::from(value);
    let divisor = u32::from(divisor.max(1));
    u16::try_from(value.div_ceil(divisor)).unwrap_or(u16::MAX)
}

fn scale_locality_hash(input: &ScaleLocalityAdvisorInput, suffix: &str) -> String {
    format!(
        "blake3:{}",
        blake3::hash(
            format!(
                "{}:{}:{}:{}:{}:{}",
                SCALE_LOCALITY_ADVISOR_SCHEMA_V1,
                input.fixture_profile_id,
                input.read_pool_size,
                input.shard_count,
                input.hotset_entry_count,
                suffix
            )
            .as_bytes()
        )
        .to_hex()
    )
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
