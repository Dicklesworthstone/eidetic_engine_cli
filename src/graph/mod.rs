use crate::models::{CapabilityStatus, GRAPH_MODULE_SCHEMA_V1};

#[cfg(feature = "graph")]
pub use fnx_algorithms::{
    BetweennessCentralityResult, PageRankResult, betweenness_centrality_directed, pagerank_directed,
};
#[cfg(feature = "graph")]
pub use fnx_classes::{AttrMap, Graph, digraph::DiGraph};
#[cfg(feature = "graph")]
use fnx_runtime::{CgseValue, CompatibilityMode};

#[cfg(feature = "graph")]
use crate::db::DbConnection;

pub const SUBSYSTEM: &str = "graph";
pub const MODULE_CONTRACT: &str = GRAPH_MODULE_SCHEMA_V1;
pub const REQUIRED_GRAPH_ENGINE: &str = "franken_networkx";

#[cfg(feature = "graph")]
static GRAPH_CAPABILITIES: [GraphCapability; 6] = [
    GraphCapability::ready(
        GraphCapabilityName::ModuleBoundary,
        GraphSurface::Status,
        "Graph module is present.",
    ),
    GraphCapability::ready(
        GraphCapabilityName::FrankenNetworkXDependency,
        GraphSurface::Projection,
        "FrankenNetworkX dependency is wired.",
    ),
    GraphCapability::ready(
        GraphCapabilityName::MemoryLinkTable,
        GraphSurface::Storage,
        "memory_links storage migration is available.",
    ),
    GraphCapability::ready(
        GraphCapabilityName::ProjectionBuilder,
        GraphSurface::Projection,
        "Graph projection from memory links is wired.",
    ),
    GraphCapability::ready(
        GraphCapabilityName::CentralityMetrics,
        GraphSurface::Analytics,
        "PageRank and betweenness centrality metrics available.",
    ),
    GraphCapability::pending(
        GraphCapabilityName::JsonGraph,
        GraphSurface::Query,
        "Expose graph metrics through stable JSON response envelope.",
    ),
];

#[cfg(not(feature = "graph"))]
static GRAPH_CAPABILITIES: [GraphCapability; 6] = [
    GraphCapability::ready(
        GraphCapabilityName::ModuleBoundary,
        GraphSurface::Status,
        "Graph module is present.",
    ),
    GraphCapability::pending(
        GraphCapabilityName::FrankenNetworkXDependency,
        GraphSurface::Projection,
        "Add the franken_networkx dependency before graph projections.",
    ),
    GraphCapability::ready(
        GraphCapabilityName::MemoryLinkTable,
        GraphSurface::Storage,
        "memory_links storage migration is available.",
    ),
    GraphCapability::pending(
        GraphCapabilityName::ProjectionBuilder,
        GraphSurface::Projection,
        "Wire graph projection from memory links.",
    ),
    GraphCapability::pending(
        GraphCapabilityName::CentralityMetrics,
        GraphSurface::Analytics,
        "Compute centrality metrics (PageRank, betweenness).",
    ),
    GraphCapability::pending(
        GraphCapabilityName::JsonGraph,
        GraphSurface::Query,
        "Expose graph metrics through stable JSON response envelope.",
    ),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphModuleReadiness {
    contract: &'static str,
    subsystem: &'static str,
    graph_engine: &'static str,
    capabilities: &'static [GraphCapability],
}

impl GraphModuleReadiness {
    #[must_use]
    pub const fn contract(&self) -> &'static str {
        self.contract
    }

    #[must_use]
    pub const fn subsystem(&self) -> &'static str {
        self.subsystem
    }

    #[must_use]
    pub const fn graph_engine(&self) -> &'static str {
        self.graph_engine
    }

    #[must_use]
    pub const fn capabilities(&self) -> &'static [GraphCapability] {
        self.capabilities
    }

    #[must_use]
    pub fn status(&self) -> CapabilityStatus {
        if self
            .capabilities
            .iter()
            .all(|capability| capability.status() == CapabilityStatus::Ready)
        {
            CapabilityStatus::Ready
        } else {
            CapabilityStatus::Pending
        }
    }

    pub fn missing_capabilities(&self) -> impl Iterator<Item = GraphCapability> + '_ {
        self.capabilities
            .iter()
            .copied()
            .filter(|capability| capability.status() != CapabilityStatus::Ready)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphCapability {
    name: GraphCapabilityName,
    surface: GraphSurface,
    status: CapabilityStatus,
    repair: &'static str,
}

impl GraphCapability {
    const fn ready(name: GraphCapabilityName, surface: GraphSurface, repair: &'static str) -> Self {
        Self {
            name,
            surface,
            status: CapabilityStatus::Ready,
            repair,
        }
    }

    const fn pending(
        name: GraphCapabilityName,
        surface: GraphSurface,
        repair: &'static str,
    ) -> Self {
        Self {
            name,
            surface,
            status: CapabilityStatus::Pending,
            repair,
        }
    }

    #[must_use]
    pub const fn name(self) -> GraphCapabilityName {
        self.name
    }

    #[must_use]
    pub const fn surface(self) -> GraphSurface {
        self.surface
    }

    #[must_use]
    pub const fn status(self) -> CapabilityStatus {
        self.status
    }

    #[must_use]
    pub const fn repair(self) -> &'static str {
        self.repair
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphCapabilityName {
    ModuleBoundary,
    FrankenNetworkXDependency,
    MemoryLinkTable,
    ProjectionBuilder,
    CentralityMetrics,
    JsonGraph,
}

impl GraphCapabilityName {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModuleBoundary => "module_boundary",
            Self::FrankenNetworkXDependency => "frankennetworkx_dependency",
            Self::MemoryLinkTable => "memory_link_table",
            Self::ProjectionBuilder => "projection_builder",
            Self::CentralityMetrics => "centrality_metrics",
            Self::JsonGraph => "json_graph",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphSurface {
    Status,
    Storage,
    Projection,
    Analytics,
    Query,
}

impl GraphSurface {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Storage => "storage",
            Self::Projection => "projection",
            Self::Analytics => "analytics",
            Self::Query => "query",
        }
    }
}

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

#[must_use]
pub const fn module_readiness() -> GraphModuleReadiness {
    GraphModuleReadiness {
        contract: MODULE_CONTRACT,
        subsystem: SUBSYSTEM,
        graph_engine: REQUIRED_GRAPH_ENGINE,
        capabilities: &GRAPH_CAPABILITIES,
    }
}

// ---------------------------------------------------------------------------
// Graph Projection (EE-164)
// ---------------------------------------------------------------------------

/// Result of building a memory graph projection.
#[cfg(feature = "graph")]
#[derive(Debug, Clone)]
pub struct MemoryGraphProjection {
    /// The directed graph of memory relationships.
    pub graph: DiGraph,
    /// Number of nodes (memories) in the graph.
    pub node_count: usize,
    /// Number of edges (links) in the graph.
    pub edge_count: usize,
    /// Elapsed time to build the projection in milliseconds.
    pub build_ms: f64,
}

/// Options for building a memory graph.
#[cfg(feature = "graph")]
#[derive(Debug, Clone, Default)]
pub struct ProjectionOptions {
    /// Maximum links to process (for testing/debugging).
    pub link_limit: Option<u32>,
    /// Minimum weight threshold for including edges.
    pub min_weight: Option<f32>,
    /// Minimum confidence threshold for including edges.
    pub min_confidence: Option<f32>,
}

/// Build a graph projection from memory links in the database.
///
/// Each memory becomes a node. Each memory_link becomes a directed edge
/// from src_memory_id to dst_memory_id, with weight and confidence as
/// edge attributes.
#[cfg(feature = "graph")]
pub fn build_memory_graph(
    conn: &DbConnection,
    options: &ProjectionOptions,
) -> Result<MemoryGraphProjection, String> {
    use std::time::Instant;

    let start = Instant::now();

    let links = conn
        .list_all_memory_links(options.link_limit)
        .map_err(|e| format!("Failed to query memory links: {e}"))?;

    let mut graph = DiGraph::new(CompatibilityMode::Strict);
    for link in &links {
        if let Some(min_w) = options.min_weight {
            if link.weight < min_w {
                continue;
            }
        }
        if let Some(min_c) = options.min_confidence {
            if link.confidence < min_c {
                continue;
            }
        }

        let mut attrs = AttrMap::new();
        attrs.insert(
            "weight".to_string(),
            CgseValue::Float(f64::from(link.weight)),
        );
        attrs.insert(
            "confidence".to_string(),
            CgseValue::Float(f64::from(link.confidence)),
        );
        attrs.insert(
            "relation".to_string(),
            CgseValue::String(link.relation.clone()),
        );
        attrs.insert("source".to_string(), CgseValue::String(link.source.clone()));
        attrs.insert(
            "evidence_count".to_string(),
            CgseValue::Int(i64::from(link.evidence_count)),
        );
        if let Some(ref reinforced) = link.last_reinforced_at {
            attrs.insert(
                "last_reinforced_at".to_string(),
                CgseValue::String(reinforced.clone()),
            );
        }

        graph.add_node(&link.src_memory_id);
        graph.add_node(&link.dst_memory_id);

        if link.directed {
            add_projection_edge(&mut graph, &link.src_memory_id, &link.dst_memory_id, attrs)?;
        } else {
            let attrs_rev = attrs.clone();
            add_projection_edge(&mut graph, &link.src_memory_id, &link.dst_memory_id, attrs)?;
            add_projection_edge(
                &mut graph,
                &link.dst_memory_id,
                &link.src_memory_id,
                attrs_rev,
            )?;
        }
    }

    let build_ms = start.elapsed().as_secs_f64() * 1000.0;
    let node_count = graph.node_count();
    let edge_count = graph.edge_count();

    Ok(MemoryGraphProjection {
        graph,
        node_count,
        edge_count,
        build_ms,
    })
}

#[cfg(feature = "graph")]
fn add_projection_edge(
    graph: &mut DiGraph,
    src_memory_id: &str,
    dst_memory_id: &str,
    attrs: AttrMap,
) -> Result<(), String> {
    graph
        .add_edge_with_attrs(src_memory_id, dst_memory_id, attrs)
        .map_err(|error| {
            format!("Failed to add graph edge {src_memory_id}->{dst_memory_id}: {error}")
        })
}

/// Compute PageRank centrality on a memory graph projection.
#[cfg(feature = "graph")]
#[must_use]
pub fn compute_pagerank(projection: &MemoryGraphProjection) -> PageRankResult {
    pagerank_directed(&projection.graph)
}

/// Compute betweenness centrality on a memory graph projection.
#[cfg(feature = "graph")]
#[must_use]
pub fn compute_betweenness(projection: &MemoryGraphProjection) -> BetweennessCentralityResult {
    betweenness_centrality_directed(&projection.graph)
}

// ---------------------------------------------------------------------------
// Centrality Refresh Job (EE-165)
// ---------------------------------------------------------------------------

/// Schema for centrality refresh response envelope.
pub const CENTRALITY_REFRESH_SCHEMA_V1: &str = "ee.graph.centrality_refresh.v1";

/// Status of a centrality refresh operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CentralityRefreshStatus {
    /// Centrality metrics were successfully computed.
    Refreshed,
    /// Operation completed but the graph was empty.
    EmptyGraph,
    /// Operation would refresh but dry_run was enabled.
    DryRun,
    /// Graph feature is not enabled.
    GraphFeatureDisabled,
}

impl CentralityRefreshStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Refreshed => "refreshed",
            Self::EmptyGraph => "empty_graph",
            Self::DryRun => "dry_run",
            Self::GraphFeatureDisabled => "graph_feature_disabled",
        }
    }
}

/// Options for centrality refresh operation.
#[derive(Clone, Debug, Default)]
pub struct CentralityRefreshOptions {
    /// Report what would be done without computing.
    pub dry_run: bool,
    /// Minimum link weight to include.
    pub min_weight: Option<f32>,
    /// Minimum link confidence to include.
    pub min_confidence: Option<f32>,
    /// Maximum links to process.
    pub link_limit: Option<u32>,
}

/// Individual memory centrality scores.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryCentralityScore {
    pub memory_id: String,
    pub pagerank: f64,
    pub betweenness: f64,
}

/// Report from a centrality refresh operation.
#[derive(Clone, Debug)]
pub struct CentralityRefreshReport {
    pub version: &'static str,
    pub status: CentralityRefreshStatus,
    pub dry_run: bool,
    pub node_count: usize,
    pub edge_count: usize,
    pub projection_ms: f64,
    pub pagerank_ms: f64,
    pub betweenness_ms: f64,
    pub total_ms: f64,
    pub scores: Vec<MemoryCentralityScore>,
    pub top_pagerank: Vec<MemoryCentralityScore>,
    pub top_betweenness: Vec<MemoryCentralityScore>,
}

impl CentralityRefreshReport {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = match self.status {
            CentralityRefreshStatus::Refreshed => {
                "Centrality refresh completed\n\n".to_string()
            }
            CentralityRefreshStatus::EmptyGraph => {
                return "Centrality refresh skipped: graph is empty (no memory links)\n"
                    .to_string();
            }
            CentralityRefreshStatus::DryRun => {
                "DRY RUN: Would refresh centrality\n\n".to_string()
            }
            CentralityRefreshStatus::GraphFeatureDisabled => {
                return "Centrality refresh skipped: graph feature is not enabled\n\
                        Next: Rebuild with `--features graph`\n"
                    .to_string();
            }
        };

        output.push_str(&format!("  Nodes: {}\n", self.node_count));
        output.push_str(&format!("  Edges: {}\n", self.edge_count));
        output.push_str(&format!(
            "  Time: {:.1}ms (projection: {:.1}ms, pagerank: {:.1}ms, betweenness: {:.1}ms)\n",
            self.total_ms, self.projection_ms, self.pagerank_ms, self.betweenness_ms
        ));

        if !self.top_pagerank.is_empty() {
            output.push_str("\n  Top PageRank:\n");
            for (i, score) in self.top_pagerank.iter().take(5).enumerate() {
                output.push_str(&format!(
                    "    {}. {} (pr={:.4})\n",
                    i + 1,
                    score.memory_id,
                    score.pagerank
                ));
            }
        }

        if !self.top_betweenness.is_empty() {
            output.push_str("\n  Top Betweenness:\n");
            for (i, score) in self.top_betweenness.iter().take(5).enumerate() {
                output.push_str(&format!(
                    "    {}. {} (bc={:.4})\n",
                    i + 1,
                    score.memory_id,
                    score.betweenness
                ));
            }
        }

        output
    }

    #[must_use]
    pub fn toon_output(&self) -> String {
        format!(
            "CENTRALITY_REFRESH|{}|{}|{}|{:.1}",
            self.status.as_str(),
            self.node_count,
            self.edge_count,
            self.total_ms
        )
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let scores: Vec<serde_json::Value> = self
            .scores
            .iter()
            .map(|s| {
                serde_json::json!({
                    "memoryId": s.memory_id,
                    "pagerank": score_json(s.pagerank),
                    "betweenness": score_json(s.betweenness),
                })
            })
            .collect();

        let top_pagerank: Vec<serde_json::Value> = self
            .top_pagerank
            .iter()
            .take(10)
            .map(|s| {
                serde_json::json!({
                    "memoryId": s.memory_id,
                    "pagerank": score_json(s.pagerank),
                })
            })
            .collect();

        let top_betweenness: Vec<serde_json::Value> = self
            .top_betweenness
            .iter()
            .take(10)
            .map(|s| {
                serde_json::json!({
                    "memoryId": s.memory_id,
                    "betweenness": score_json(s.betweenness),
                })
            })
            .collect();

        serde_json::json!({
            "command": "graph centrality refresh",
            "version": self.version,
            "status": self.status.as_str(),
            "dryRun": self.dry_run,
            "graph": {
                "nodeCount": self.node_count,
                "edgeCount": self.edge_count,
            },
            "timing": {
                "projectionMs": score_json(self.projection_ms),
                "pagerankMs": score_json(self.pagerank_ms),
                "betweennessMs": score_json(self.betweenness_ms),
                "totalMs": score_json(self.total_ms),
            },
            "scores": scores,
            "topPagerank": top_pagerank,
            "topBetweenness": top_betweenness,
        })
    }
}

fn score_json(value: f64) -> serde_json::Value {
    let rounded = (value * 10_000.0).round() / 10_000.0;
    serde_json::Number::from_f64(rounded).map_or(serde_json::Value::Null, serde_json::Value::Number)
}

/// Refresh centrality metrics for all memories in the graph.
///
/// This builds a fresh projection from memory_links, computes PageRank and
/// betweenness centrality, and returns a report with all scores.
#[cfg(feature = "graph")]
pub fn refresh_centrality(
    conn: &DbConnection,
    options: &CentralityRefreshOptions,
) -> Result<CentralityRefreshReport, String> {
    use std::time::Instant;

    let total_start = Instant::now();

    if options.dry_run {
        let projection_opts = ProjectionOptions {
            link_limit: options.link_limit,
            min_weight: options.min_weight,
            min_confidence: options.min_confidence,
        };
        let projection = build_memory_graph(conn, &projection_opts)?;
        return Ok(CentralityRefreshReport {
            version: env!("CARGO_PKG_VERSION"),
            status: CentralityRefreshStatus::DryRun,
            dry_run: true,
            node_count: projection.node_count,
            edge_count: projection.edge_count,
            projection_ms: projection.build_ms,
            pagerank_ms: 0.0,
            betweenness_ms: 0.0,
            total_ms: total_start.elapsed().as_secs_f64() * 1000.0,
            scores: vec![],
            top_pagerank: vec![],
            top_betweenness: vec![],
        });
    }

    let projection_opts = ProjectionOptions {
        link_limit: options.link_limit,
        min_weight: options.min_weight,
        min_confidence: options.min_confidence,
    };
    let projection = build_memory_graph(conn, &projection_opts)?;

    if projection.node_count == 0 {
        return Ok(CentralityRefreshReport {
            version: env!("CARGO_PKG_VERSION"),
            status: CentralityRefreshStatus::EmptyGraph,
            dry_run: false,
            node_count: 0,
            edge_count: 0,
            projection_ms: projection.build_ms,
            pagerank_ms: 0.0,
            betweenness_ms: 0.0,
            total_ms: total_start.elapsed().as_secs_f64() * 1000.0,
            scores: vec![],
            top_pagerank: vec![],
            top_betweenness: vec![],
        });
    }

    let pagerank_start = Instant::now();
    let pagerank = compute_pagerank(&projection);
    let pagerank_ms = pagerank_start.elapsed().as_secs_f64() * 1000.0;

    let betweenness_start = Instant::now();
    let betweenness = compute_betweenness(&projection);
    let betweenness_ms = betweenness_start.elapsed().as_secs_f64() * 1000.0;

    let mut scores: Vec<MemoryCentralityScore> = pagerank
        .scores
        .iter()
        .map(|pr| {
            let bc = betweenness
                .scores
                .iter()
                .find(|b| b.node == pr.node)
                .map(|b| b.score)
                .unwrap_or(0.0);
            MemoryCentralityScore {
                memory_id: pr.node.clone(),
                pagerank: pr.score,
                betweenness: bc,
            }
        })
        .collect();

    scores.sort_by(|a, b| {
        b.pagerank
            .partial_cmp(&a.pagerank)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut top_pagerank = scores.clone();
    top_pagerank.truncate(10);

    let mut top_betweenness = scores.clone();
    top_betweenness.sort_by(|a, b| {
        b.betweenness
            .partial_cmp(&a.betweenness)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    top_betweenness.truncate(10);

    let total_ms = total_start.elapsed().as_secs_f64() * 1000.0;

    Ok(CentralityRefreshReport {
        version: env!("CARGO_PKG_VERSION"),
        status: CentralityRefreshStatus::Refreshed,
        dry_run: false,
        node_count: projection.node_count,
        edge_count: projection.edge_count,
        projection_ms: projection.build_ms,
        pagerank_ms,
        betweenness_ms,
        total_ms,
        scores,
        top_pagerank,
        top_betweenness,
    })
}

/// Refresh centrality metrics (stub when graph feature is disabled).
#[cfg(not(feature = "graph"))]
pub fn refresh_centrality(
    _conn: &crate::db::DbConnection,
    options: &CentralityRefreshOptions,
) -> Result<CentralityRefreshReport, String> {
    Ok(CentralityRefreshReport {
        version: env!("CARGO_PKG_VERSION"),
        status: CentralityRefreshStatus::GraphFeatureDisabled,
        dry_run: options.dry_run,
        node_count: 0,
        edge_count: 0,
        projection_ms: 0.0,
        pagerank_ms: 0.0,
        betweenness_ms: 0.0,
        total_ms: 0.0,
        scores: vec![],
        top_pagerank: vec![],
        top_betweenness: vec![],
    })
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "graph")]
    use crate::db::{
        CreateMemoryInput, CreateMemoryLinkInput, CreateWorkspaceInput, DbConnection,
        MemoryLinkRelation, MemoryLinkSource,
    };
    use crate::models::CapabilityStatus;

    use super::{
        GraphCapabilityName, GraphSurface, REQUIRED_GRAPH_ENGINE, module_readiness, subsystem_name,
    };

    #[cfg(feature = "graph")]
    const WORKSPACE_ID: &str = "wsp_01234567890123456789012345";
    #[cfg(feature = "graph")]
    const MEMORY_A: &str = "mem_00000000000000000000000011";
    #[cfg(feature = "graph")]
    const MEMORY_B: &str = "mem_00000000000000000000000012";
    #[cfg(feature = "graph")]
    const MEMORY_C: &str = "mem_00000000000000000000000013";

    #[cfg(feature = "graph")]
    type TestResult = Result<(), String>;

    #[test]
    fn subsystem_name_is_stable() {
        assert_eq!(subsystem_name(), "graph");
    }

    #[test]
    fn module_contract_names_frankennetworkx_boundary() {
        let readiness = module_readiness();

        assert_eq!(readiness.contract(), "ee.graph.module.v1");
        assert_eq!(readiness.subsystem(), "graph");
        assert_eq!(readiness.graph_engine(), REQUIRED_GRAPH_ENGINE);
        assert_eq!(readiness.graph_engine(), "franken_networkx");
    }

    #[test]
    fn readiness_reports_pending_until_integration_lands() {
        let readiness = module_readiness();

        assert_eq!(readiness.status(), CapabilityStatus::Pending);
        assert_eq!(
            readiness
                .capabilities()
                .first()
                .map(|capability| capability.status()),
            Some(CapabilityStatus::Ready)
        );
        #[cfg(feature = "graph")]
        assert_eq!(readiness.missing_capabilities().count(), 1);
        #[cfg(not(feature = "graph"))]
        assert_eq!(readiness.missing_capabilities().count(), 4);
    }

    #[test]
    fn capabilities_are_in_dependency_order() {
        let names: Vec<&str> = module_readiness()
            .capabilities()
            .iter()
            .map(|capability| capability.name().as_str())
            .collect();

        assert_eq!(
            names,
            vec![
                "module_boundary",
                "frankennetworkx_dependency",
                "memory_link_table",
                "projection_builder",
                "centrality_metrics",
                "json_graph",
            ]
        );
    }

    #[test]
    fn capability_surfaces_are_stable() {
        let surfaces: Vec<&str> = module_readiness()
            .capabilities()
            .iter()
            .map(|capability| capability.surface().as_str())
            .collect();

        assert_eq!(
            surfaces,
            vec![
                "status",
                "projection",
                "storage",
                "projection",
                "analytics",
                "query",
            ]
        );
    }

    #[cfg(not(feature = "graph"))]
    #[test]
    fn missing_capabilities_keep_repair_metadata() {
        let missing: Vec<_> = module_readiness().missing_capabilities().collect();

        assert_eq!(
            missing.first().map(|capability| capability.name()),
            Some(GraphCapabilityName::FrankenNetworkXDependency)
        );
        assert_eq!(
            missing.first().map(|capability| capability.surface()),
            Some(GraphSurface::Projection)
        );
        assert!(
            missing
                .first()
                .map(|capability| capability.repair().contains("franken_networkx"))
                .unwrap_or(false)
        );
    }

    #[cfg(feature = "graph")]
    #[test]
    fn missing_capabilities_keep_repair_metadata() {
        let missing: Vec<_> = module_readiness().missing_capabilities().collect();

        assert_eq!(
            missing.first().map(|capability| capability.name()),
            Some(GraphCapabilityName::JsonGraph)
        );
        assert_eq!(
            missing.first().map(|capability| capability.surface()),
            Some(GraphSurface::Query)
        );
        assert!(
            missing
                .first()
                .map(|capability| capability.repair().contains("JSON"))
                .unwrap_or(false)
        );
    }

    #[cfg(feature = "graph")]
    #[test]
    fn projection_builder_capabilities_ready() {
        let readiness = module_readiness();
        let cap = readiness
            .capabilities()
            .iter()
            .find(|c| c.name() == GraphCapabilityName::ProjectionBuilder);
        assert_eq!(cap.map(|c| c.status()), Some(CapabilityStatus::Ready));
    }

    #[cfg(feature = "graph")]
    #[test]
    fn centrality_metrics_capabilities_ready() {
        let readiness = module_readiness();
        let cap = readiness
            .capabilities()
            .iter()
            .find(|c| c.name() == GraphCapabilityName::CentralityMetrics);
        assert_eq!(cap.map(|c| c.status()), Some(CapabilityStatus::Ready));
    }

    #[cfg(feature = "graph")]
    #[test]
    fn graph_feature_missing_count_is_one() {
        let readiness = module_readiness();
        assert_eq!(readiness.missing_capabilities().count(), 1);
    }

    #[cfg(feature = "graph")]
    fn open_projection_db() -> Result<DbConnection, String> {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &CreateWorkspaceInput {
                    path: "/tmp/ee-graph-projection".to_string(),
                    name: Some("graph projection".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
        insert_memory(&connection, MEMORY_A, "Graph source memory")?;
        insert_memory(&connection, MEMORY_B, "Graph bridge memory")?;
        insert_memory(&connection, MEMORY_C, "Graph target memory")?;
        Ok(connection)
    }

    #[cfg(feature = "graph")]
    fn insert_memory(connection: &DbConnection, id: &str, content: &str) -> TestResult {
        connection
            .insert_memory(
                id,
                &CreateMemoryInput {
                    workspace_id: WORKSPACE_ID.to_string(),
                    level: "semantic".to_string(),
                    kind: "fact".to_string(),
                    content: content.to_string(),
                    confidence: 0.8,
                    utility: 0.6,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "agent_assertion".to_string(),
                    trust_subclass: None,
                    tags: vec![],
                },
            )
            .map_err(|error| error.to_string())
    }

    #[cfg(feature = "graph")]
    fn insert_link(
        connection: &DbConnection,
        id: &str,
        src: &str,
        dst: &str,
        directed: bool,
        weight: f32,
        confidence: f32,
    ) -> TestResult {
        connection
            .insert_memory_link(
                id,
                &CreateMemoryLinkInput {
                    src_memory_id: src.to_string(),
                    dst_memory_id: dst.to_string(),
                    relation: MemoryLinkRelation::Supports,
                    weight,
                    confidence,
                    directed,
                    evidence_count: 2,
                    last_reinforced_at: Some("2026-04-29T20:00:00Z".to_string()),
                    source: MemoryLinkSource::Agent,
                    created_by: Some("agent:test".to_string()),
                    metadata_json: None,
                },
            )
            .map_err(|error| error.to_string())
    }

    #[cfg(feature = "graph")]
    #[test]
    fn projection_includes_directed_and_undirected_memory_links() -> TestResult {
        let connection = open_projection_db()?;
        insert_link(
            &connection,
            "link_00000000000000000000000011",
            MEMORY_A,
            MEMORY_B,
            true,
            0.9,
            0.9,
        )?;
        insert_link(
            &connection,
            "link_00000000000000000000000012",
            MEMORY_B,
            MEMORY_C,
            false,
            0.7,
            0.8,
        )?;

        let projection =
            super::build_memory_graph(&connection, &super::ProjectionOptions::default())?;

        assert_eq!(projection.node_count, 3);
        assert_eq!(projection.edge_count, 3);
        assert!(projection.graph.has_edge(MEMORY_A, MEMORY_B));
        assert!(!projection.graph.has_edge(MEMORY_B, MEMORY_A));
        assert!(projection.graph.has_edge(MEMORY_B, MEMORY_C));
        assert!(projection.graph.has_edge(MEMORY_C, MEMORY_B));

        connection.close().map_err(|error| error.to_string())
    }

    #[cfg(feature = "graph")]
    #[test]
    fn projection_filters_by_weight_and_confidence() -> TestResult {
        let connection = open_projection_db()?;
        insert_link(
            &connection,
            "link_00000000000000000000000021",
            MEMORY_A,
            MEMORY_B,
            true,
            0.8,
            0.9,
        )?;
        insert_link(
            &connection,
            "link_00000000000000000000000022",
            MEMORY_B,
            MEMORY_C,
            true,
            0.2,
            0.9,
        )?;
        insert_link(
            &connection,
            "link_00000000000000000000000023",
            MEMORY_C,
            MEMORY_A,
            true,
            0.8,
            0.3,
        )?;

        let projection = super::build_memory_graph(
            &connection,
            &super::ProjectionOptions {
                link_limit: None,
                min_weight: Some(0.5),
                min_confidence: Some(0.8),
            },
        )?;

        assert_eq!(projection.node_count, 2);
        assert_eq!(projection.edge_count, 1);
        assert!(projection.graph.has_edge(MEMORY_A, MEMORY_B));
        assert!(!projection.graph.has_edge(MEMORY_B, MEMORY_C));
        assert!(!projection.graph.has_edge(MEMORY_C, MEMORY_A));

        connection.close().map_err(|error| error.to_string())
    }

    #[cfg(feature = "graph")]
    #[test]
    fn centrality_wrappers_return_scores_for_projection_nodes() -> TestResult {
        let connection = open_projection_db()?;
        insert_link(
            &connection,
            "link_00000000000000000000000031",
            MEMORY_A,
            MEMORY_B,
            true,
            0.9,
            0.9,
        )?;
        insert_link(
            &connection,
            "link_00000000000000000000000032",
            MEMORY_B,
            MEMORY_C,
            true,
            0.9,
            0.9,
        )?;

        let projection =
            super::build_memory_graph(&connection, &super::ProjectionOptions::default())?;
        let pagerank = super::compute_pagerank(&projection);
        let betweenness = super::compute_betweenness(&projection);

        assert_eq!(pagerank.scores.len(), projection.node_count);
        assert_eq!(betweenness.scores.len(), projection.node_count);
        assert!(pagerank.scores.iter().any(|score| score.node == MEMORY_A));
        assert!(
            betweenness
                .scores
                .iter()
                .any(|score| score.node == MEMORY_B)
        );

        connection.close().map_err(|error| error.to_string())
    }

    // -------------------------------------------------------------------------
    // Centrality Refresh Tests (EE-165)
    // -------------------------------------------------------------------------

    #[test]
    fn centrality_refresh_status_strings_are_stable() {
        use super::CentralityRefreshStatus;
        assert_eq!(CentralityRefreshStatus::Refreshed.as_str(), "refreshed");
        assert_eq!(CentralityRefreshStatus::EmptyGraph.as_str(), "empty_graph");
        assert_eq!(CentralityRefreshStatus::DryRun.as_str(), "dry_run");
        assert_eq!(
            CentralityRefreshStatus::GraphFeatureDisabled.as_str(),
            "graph_feature_disabled"
        );
    }

    #[test]
    fn centrality_refresh_schema_is_versioned() {
        assert_eq!(
            super::CENTRALITY_REFRESH_SCHEMA_V1,
            "ee.graph.centrality_refresh.v1"
        );
    }

    #[cfg(feature = "graph")]
    #[test]
    fn refresh_centrality_with_empty_graph_returns_empty_status() -> TestResult {
        let connection = open_projection_db()?;

        let report =
            super::refresh_centrality(&connection, &super::CentralityRefreshOptions::default())?;

        assert_eq!(report.status, super::CentralityRefreshStatus::EmptyGraph);
        assert_eq!(report.node_count, 0);
        assert_eq!(report.edge_count, 0);
        assert!(report.scores.is_empty());

        connection.close().map_err(|error| error.to_string())
    }

    #[cfg(feature = "graph")]
    #[test]
    fn refresh_centrality_computes_scores_for_linked_memories() -> TestResult {
        let connection = open_projection_db()?;
        insert_link(
            &connection,
            "link_00000000000000000000000041",
            MEMORY_A,
            MEMORY_B,
            true,
            0.9,
            0.9,
        )?;
        insert_link(
            &connection,
            "link_00000000000000000000000042",
            MEMORY_B,
            MEMORY_C,
            true,
            0.9,
            0.9,
        )?;

        let report =
            super::refresh_centrality(&connection, &super::CentralityRefreshOptions::default())?;

        assert_eq!(report.status, super::CentralityRefreshStatus::Refreshed);
        assert_eq!(report.node_count, 3);
        assert_eq!(report.edge_count, 2);
        assert_eq!(report.scores.len(), 3);
        assert!(report.scores.iter().any(|s| s.memory_id == MEMORY_A));
        assert!(report.scores.iter().any(|s| s.memory_id == MEMORY_B));
        assert!(report.scores.iter().any(|s| s.memory_id == MEMORY_C));
        assert!(report.total_ms > 0.0);

        connection.close().map_err(|error| error.to_string())
    }

    #[cfg(feature = "graph")]
    #[test]
    fn refresh_centrality_dry_run_skips_computation() -> TestResult {
        let connection = open_projection_db()?;
        insert_link(
            &connection,
            "link_00000000000000000000000051",
            MEMORY_A,
            MEMORY_B,
            true,
            0.9,
            0.9,
        )?;

        let report = super::refresh_centrality(
            &connection,
            &super::CentralityRefreshOptions {
                dry_run: true,
                ..Default::default()
            },
        )?;

        assert_eq!(report.status, super::CentralityRefreshStatus::DryRun);
        assert!(report.dry_run);
        assert_eq!(report.node_count, 2);
        assert_eq!(report.edge_count, 1);
        assert!(report.scores.is_empty());
        assert_eq!(report.pagerank_ms, 0.0);
        assert_eq!(report.betweenness_ms, 0.0);

        connection.close().map_err(|error| error.to_string())
    }

    #[cfg(feature = "graph")]
    #[test]
    fn refresh_centrality_top_lists_are_sorted() -> TestResult {
        let connection = open_projection_db()?;
        insert_link(
            &connection,
            "link_00000000000000000000000061",
            MEMORY_A,
            MEMORY_B,
            true,
            0.9,
            0.9,
        )?;
        insert_link(
            &connection,
            "link_00000000000000000000000062",
            MEMORY_A,
            MEMORY_C,
            true,
            0.9,
            0.9,
        )?;
        insert_link(
            &connection,
            "link_00000000000000000000000063",
            MEMORY_B,
            MEMORY_C,
            true,
            0.9,
            0.9,
        )?;

        let report =
            super::refresh_centrality(&connection, &super::CentralityRefreshOptions::default())?;

        assert!(!report.top_pagerank.is_empty());
        assert!(!report.top_betweenness.is_empty());

        let pr_scores: Vec<f64> = report.top_pagerank.iter().map(|s| s.pagerank).collect();
        let bc_scores: Vec<f64> = report
            .top_betweenness
            .iter()
            .map(|s| s.betweenness)
            .collect();

        for window in pr_scores.windows(2) {
            assert!(
                window[0] >= window[1],
                "pagerank should be sorted descending"
            );
        }
        for window in bc_scores.windows(2) {
            assert!(
                window[0] >= window[1],
                "betweenness should be sorted descending"
            );
        }

        connection.close().map_err(|error| error.to_string())
    }

    #[cfg(feature = "graph")]
    #[test]
    fn refresh_centrality_report_human_summary_is_not_empty() -> TestResult {
        let connection = open_projection_db()?;
        insert_link(
            &connection,
            "link_00000000000000000000000071",
            MEMORY_A,
            MEMORY_B,
            true,
            0.9,
            0.9,
        )?;

        let report =
            super::refresh_centrality(&connection, &super::CentralityRefreshOptions::default())?;

        let summary = report.human_summary();
        assert!(summary.contains("Centrality refresh completed"));
        assert!(summary.contains("Nodes: 2"));
        assert!(summary.contains("Edges: 1"));

        connection.close().map_err(|error| error.to_string())
    }

    #[cfg(feature = "graph")]
    #[test]
    fn refresh_centrality_report_toon_output_is_parseable() -> TestResult {
        let connection = open_projection_db()?;
        insert_link(
            &connection,
            "link_00000000000000000000000081",
            MEMORY_A,
            MEMORY_B,
            true,
            0.9,
            0.9,
        )?;

        let report =
            super::refresh_centrality(&connection, &super::CentralityRefreshOptions::default())?;

        let toon = report.toon_output();
        assert!(toon.starts_with("CENTRALITY_REFRESH|refreshed|2|1|"));

        connection.close().map_err(|error| error.to_string())
    }

    #[cfg(feature = "graph")]
    #[test]
    fn refresh_centrality_report_json_has_required_fields() -> TestResult {
        let connection = open_projection_db()?;
        insert_link(
            &connection,
            "link_00000000000000000000000091",
            MEMORY_A,
            MEMORY_B,
            true,
            0.9,
            0.9,
        )?;

        let report =
            super::refresh_centrality(&connection, &super::CentralityRefreshOptions::default())?;

        let json = report.data_json();
        assert_eq!(json["command"], "graph centrality refresh");
        assert_eq!(json["status"], "refreshed");
        assert_eq!(json["graph"]["nodeCount"], 2);
        assert_eq!(json["graph"]["edgeCount"], 1);
        assert!(json["timing"]["totalMs"].as_f64().is_some());
        assert!(json["scores"].as_array().is_some());
        assert!(json["topPagerank"].as_array().is_some());
        assert!(json["topBetweenness"].as_array().is_some());

        connection.close().map_err(|error| error.to_string())
    }
}
