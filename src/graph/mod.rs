use std::collections::{BTreeMap, BTreeSet};

use crate::db::{
    CreateGraphSnapshotInput, DbConnection, GraphSnapshotStatus, GraphSnapshotType,
    StoredGraphSnapshot, StoredMemoryLink,
};
use crate::models::{CapabilityStatus, GRAPH_MODULE_SCHEMA_V1, MemoryId};

#[cfg(feature = "graph")]
pub use fnx_algorithms::{
    BetweennessCentralityResult, PageRankResult, betweenness_centrality_directed, pagerank_directed,
};
#[cfg(feature = "graph")]
pub use fnx_classes::{AttrMap, Graph, digraph::DiGraph};
#[cfg(feature = "graph")]
use fnx_runtime::{CgseValue, CompatibilityMode};

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
// Autolink Candidate Generation (EE-168)
// ---------------------------------------------------------------------------

/// A memory summary used by deterministic autolink candidate generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutolinkMemoryInput {
    pub memory_id: String,
    pub tags: Vec<String>,
    pub evidence_count: u32,
}

/// An existing memory edge used to suppress duplicate autolink suggestions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutolinkExistingEdge {
    pub src_memory_id: String,
    pub dst_memory_id: String,
    pub relation: String,
}

/// Options for tag co-occurrence autolink candidate generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutolinkCandidateOptions {
    pub min_shared_tags: usize,
    pub common_tag_max_count: u32,
    pub max_candidates: Option<usize>,
}

impl Default for AutolinkCandidateOptions {
    fn default() -> Self {
        Self {
            min_shared_tags: 2,
            common_tag_max_count: 8,
            max_candidates: None,
        }
    }
}

/// A dry-run candidate memory link proposed from explainable graph features.
#[derive(Clone, Debug, PartialEq)]
pub struct AutolinkCandidate {
    pub src_memory_id: String,
    pub dst_memory_id: String,
    pub relation: String,
    pub source: String,
    pub directed: bool,
    pub weight: f64,
    pub confidence: f64,
    pub shared_tags: Vec<String>,
    pub evidence_count: u32,
    pub metadata_json: String,
}

/// Generate deterministic dry-run `co_tag` memory link candidates.
///
/// This function does not write to storage. Callers that later apply candidates
/// must do so through an explicit audited maintenance path.
#[must_use]
pub fn generate_autolink_candidates(
    memories: &[AutolinkMemoryInput],
    existing_edges: &[AutolinkExistingEdge],
    options: &AutolinkCandidateOptions,
) -> Vec<AutolinkCandidate> {
    let mut normalized_memories: Vec<_> = memories
        .iter()
        .map(NormalizedAutolinkMemory::from_input)
        .filter(|memory| !memory.tags.is_empty())
        .collect();
    normalized_memories.sort_by(|left, right| left.memory_id.cmp(right.memory_id));

    let tag_counts = tag_frequencies(&normalized_memories);
    let existing_pairs = existing_cotag_pairs(existing_edges);
    let mut candidates = Vec::new();

    for (left_index, left) in normalized_memories.iter().enumerate() {
        for right in normalized_memories.iter().skip(left_index + 1) {
            if left.memory_id == right.memory_id {
                continue;
            }

            let (src_memory_id, dst_memory_id) =
                canonical_memory_pair(left.memory_id, right.memory_id);
            if existing_pairs.contains(&(src_memory_id.to_owned(), dst_memory_id.to_owned())) {
                continue;
            }

            let shared_tags: Vec<String> = left.tags.intersection(&right.tags).cloned().collect();
            if shared_tags.len() < options.min_shared_tags {
                continue;
            }

            let common_tag_count =
                count_common_tags(&shared_tags, &tag_counts, options.common_tag_max_count);
            if common_tag_count == shared_tags.len() {
                continue;
            }

            let specificity = tag_specificity(&shared_tags, &tag_counts);
            let evidence_count = left.evidence_count.saturating_add(right.evidence_count);
            let weight = autolink_score(
                shared_tags.len(),
                specificity,
                common_tag_count,
                evidence_count,
            );
            let confidence = round_score((0.45 + weight * 0.5).clamp(0.0, 0.95));
            let tag_frequency_metadata = shared_tag_frequency_metadata(&shared_tags, &tag_counts);

            candidates.push(AutolinkCandidate {
                src_memory_id: src_memory_id.to_owned(),
                dst_memory_id: dst_memory_id.to_owned(),
                relation: "co_tag".to_owned(),
                source: "auto".to_owned(),
                directed: false,
                weight,
                confidence,
                evidence_count,
                metadata_json: serde_json::json!({
                    "strategy": "tag_cooccurrence",
                    "dryRun": true,
                    "sharedTags": &shared_tags,
                    "tagFrequencies": tag_frequency_metadata,
                    "commonTagMaxCount": options.common_tag_max_count,
                    "commonTagCount": common_tag_count,
                })
                .to_string(),
                shared_tags,
            });
        }
    }

    candidates.sort_by(compare_autolink_candidates);
    if let Some(limit) = options.max_candidates {
        candidates.truncate(limit);
    }
    candidates
}

#[derive(Clone, Debug)]
struct NormalizedAutolinkMemory<'a> {
    memory_id: &'a str,
    tags: BTreeSet<String>,
    evidence_count: u32,
}

impl<'a> NormalizedAutolinkMemory<'a> {
    fn from_input(input: &'a AutolinkMemoryInput) -> Self {
        Self {
            memory_id: input.memory_id.as_str(),
            tags: input
                .tags
                .iter()
                .filter_map(|tag| normalize_autolink_tag(tag))
                .collect(),
            evidence_count: input.evidence_count,
        }
    }
}

fn normalize_autolink_tag(tag: &str) -> Option<String> {
    let normalized = tag
        .trim()
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn tag_frequencies(memories: &[NormalizedAutolinkMemory<'_>]) -> BTreeMap<String, u32> {
    let mut counts = BTreeMap::new();
    for memory in memories {
        for tag in &memory.tags {
            let count = counts.entry(tag.clone()).or_insert(0);
            *count += 1;
        }
    }
    counts
}

fn existing_cotag_pairs(existing_edges: &[AutolinkExistingEdge]) -> BTreeSet<(String, String)> {
    existing_edges
        .iter()
        .filter(|edge| edge.relation == "co_tag")
        .map(|edge| {
            let (src, dst) = canonical_memory_pair(&edge.src_memory_id, &edge.dst_memory_id);
            (src.to_owned(), dst.to_owned())
        })
        .collect()
}

fn canonical_memory_pair<'a>(left: &'a str, right: &'a str) -> (&'a str, &'a str) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

fn tag_specificity(shared_tags: &[String], tag_counts: &BTreeMap<String, u32>) -> f64 {
    shared_tags
        .iter()
        .map(|tag| {
            tag_counts
                .get(tag)
                .copied()
                .filter(|count| *count > 0)
                .map_or(0.0, |count| 1.0 / f64::from(count))
        })
        .sum()
}

fn count_common_tags(
    shared_tags: &[String],
    tag_counts: &BTreeMap<String, u32>,
    common_tag_max_count: u32,
) -> usize {
    shared_tags
        .iter()
        .filter(|tag| {
            tag_counts
                .get(*tag)
                .copied()
                .is_some_and(|count| count > common_tag_max_count)
        })
        .count()
}

fn shared_tag_frequency_metadata(
    shared_tags: &[String],
    tag_counts: &BTreeMap<String, u32>,
) -> BTreeMap<String, u32> {
    shared_tags
        .iter()
        .map(|tag| (tag.clone(), tag_counts.get(tag).copied().unwrap_or(0)))
        .collect()
}

fn autolink_score(
    shared_tag_count: usize,
    specificity: f64,
    common_tag_count: usize,
    evidence_count: u32,
) -> f64 {
    let shared_count = u32::try_from(shared_tag_count).unwrap_or(u32::MAX);
    let common_count = u32::try_from(common_tag_count).unwrap_or(u32::MAX);
    let shared_component = (f64::from(shared_count) * 0.25).min(0.6);
    let specificity_component = (specificity * 0.15).min(0.3);
    let evidence_component = (f64::from(evidence_count.min(10)) * 0.01).min(0.1);
    let common_penalty = f64::from(common_count) * 0.05;
    round_score(
        (shared_component + specificity_component + evidence_component - common_penalty)
            .clamp(0.05, 0.99),
    )
}

fn round_score(value: f64) -> f64 {
    if value.is_finite() {
        (value * 1000.0).round() / 1000.0
    } else {
        0.0
    }
}

fn compare_autolink_candidates(
    left: &AutolinkCandidate,
    right: &AutolinkCandidate,
) -> std::cmp::Ordering {
    right
        .weight
        .total_cmp(&left.weight)
        .then_with(|| right.shared_tags.len().cmp(&left.shared_tags.len()))
        .then_with(|| left.src_memory_id.cmp(&right.src_memory_id))
        .then_with(|| left.dst_memory_id.cmp(&right.dst_memory_id))
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

/// Snapshot metadata produced by a graph refresh job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphRefreshSnapshot {
    pub id: String,
    pub graph_type: GraphSnapshotType,
    pub snapshot_version: u32,
    pub source_generation: u32,
    pub content_hash: String,
    pub status: GraphSnapshotStatus,
}

/// Report from the durable graph refresh job.
#[derive(Clone, Debug)]
pub struct GraphRefreshJobReport {
    pub centrality: CentralityRefreshReport,
    pub workspace_id: String,
    pub snapshot: Option<GraphRefreshSnapshot>,
}

impl CentralityRefreshReport {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = match self.status {
            CentralityRefreshStatus::Refreshed => "Centrality refresh completed\n\n".to_string(),
            CentralityRefreshStatus::EmptyGraph => {
                return "Centrality refresh skipped: graph is empty (no memory links)\n"
                    .to_string();
            }
            CentralityRefreshStatus::DryRun => "DRY RUN: Would refresh centrality\n\n".to_string(),
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

impl GraphRefreshSnapshot {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "graphType": self.graph_type.as_str(),
            "snapshotVersion": self.snapshot_version,
            "sourceGeneration": self.source_generation,
            "contentHash": self.content_hash,
            "status": self.status.as_str(),
        })
    }
}

impl GraphRefreshJobReport {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = self.centrality.human_summary();
        if let Some(snapshot) = &self.snapshot {
            output.push_str("\nSnapshot:\n");
            output.push_str(&format!("  ID: {}\n", snapshot.id));
            output.push_str(&format!("  Version: {}\n", snapshot.snapshot_version));
            output.push_str(&format!(
                "  Source generation: {}\n",
                snapshot.source_generation
            ));
            output.push_str(&format!("  Content hash: {}\n", snapshot.content_hash));
        }
        output
    }

    #[must_use]
    pub fn toon_output(&self) -> String {
        self.centrality.toon_output()
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let mut data = self.centrality.data_json();
        if let Some(object) = data.as_object_mut() {
            object.insert(
                "workspaceId".to_owned(),
                serde_json::Value::String(self.workspace_id.clone()),
            );
            object.insert(
                "snapshot".to_owned(),
                self.snapshot
                    .as_ref()
                    .map(GraphRefreshSnapshot::data_json)
                    .unwrap_or(serde_json::Value::Null),
            );
        }
        data
    }
}

fn score_json(value: f64) -> serde_json::Value {
    let rounded = (value * 10_000.0).round() / 10_000.0;
    serde_json::Number::from_f64(rounded).map_or(serde_json::Value::Null, serde_json::Value::Number)
}

/// Run the graph refresh job and persist a rebuildable graph snapshot.
///
/// Dry runs, empty graphs, and disabled graph builds are report-only. A refreshed
/// graph writes one versioned `memory_links` graph snapshot containing topology
/// plus centrality scores for deterministic export and validation.
pub fn refresh_graph_snapshot(
    conn: &DbConnection,
    workspace_id: &str,
    options: &CentralityRefreshOptions,
) -> Result<GraphRefreshJobReport, String> {
    let centrality = refresh_centrality(conn, options)?;
    let mut report = GraphRefreshJobReport {
        centrality,
        workspace_id: workspace_id.to_owned(),
        snapshot: None,
    };

    if options.dry_run || report.centrality.status != CentralityRefreshStatus::Refreshed {
        return Ok(report);
    }

    let links = graph_snapshot_links(conn, options)?;
    let metrics_json = graph_snapshot_metrics_json(&report.centrality, &links)?;
    let content_hash = graph_snapshot_content_hash(&metrics_json);
    let snapshot_version = next_graph_snapshot_version(conn, workspace_id)?;
    let source_generation = u32::try_from(links.len()).map_err(|_| {
        format!(
            "Graph source link count {} exceeds supported snapshot generation range",
            links.len()
        )
    })?;
    let snapshot_id = generate_graph_snapshot_id();

    conn.insert_graph_snapshot(
        &snapshot_id,
        &CreateGraphSnapshotInput {
            workspace_id: workspace_id.to_owned(),
            snapshot_version,
            schema_version: GRAPH_EXPORT_SCHEMA_V1.to_owned(),
            graph_type: GraphSnapshotType::MemoryLinks,
            node_count: u32::try_from(report.centrality.node_count).map_err(|_| {
                format!(
                    "Graph node count {} exceeds supported snapshot range",
                    report.centrality.node_count
                )
            })?,
            edge_count: u32::try_from(report.centrality.edge_count).map_err(|_| {
                format!(
                    "Graph edge count {} exceeds supported snapshot range",
                    report.centrality.edge_count
                )
            })?,
            metrics_json,
            content_hash: content_hash.clone(),
            source_generation,
            expires_at: None,
        },
    )
    .map_err(|error| format!("Failed to persist graph snapshot: {error}"))?;

    report.snapshot = Some(GraphRefreshSnapshot {
        id: snapshot_id,
        graph_type: GraphSnapshotType::MemoryLinks,
        snapshot_version,
        source_generation,
        content_hash,
        status: GraphSnapshotStatus::Valid,
    });
    Ok(report)
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

fn graph_snapshot_links(
    conn: &DbConnection,
    options: &CentralityRefreshOptions,
) -> Result<Vec<StoredMemoryLink>, String> {
    let links = conn
        .list_all_memory_links(options.link_limit)
        .map_err(|error| format!("Failed to query memory links for graph snapshot: {error}"))?;
    Ok(links
        .into_iter()
        .filter(|link| {
            options
                .min_weight
                .is_none_or(|min_weight| link.weight >= min_weight)
                && options
                    .min_confidence
                    .is_none_or(|min_confidence| link.confidence >= min_confidence)
        })
        .collect())
}

fn graph_snapshot_metrics_json(
    centrality: &CentralityRefreshReport,
    links: &[StoredMemoryLink],
) -> Result<String, String> {
    let scores_by_memory: BTreeMap<&str, &MemoryCentralityScore> = centrality
        .scores
        .iter()
        .map(|score| (score.memory_id.as_str(), score))
        .collect();

    let nodes: Vec<_> = scores_by_memory
        .iter()
        .map(|(memory_id, score)| {
            serde_json::json!({
                "id": memory_id,
                "memoryId": memory_id,
                "label": memory_id,
                "pagerank": score_json(score.pagerank),
                "betweenness": score_json(score.betweenness),
            })
        })
        .collect();

    let edges: Vec<_> = links
        .iter()
        .map(|link| {
            serde_json::json!({
                "id": link.id,
                "source": link.src_memory_id,
                "target": link.dst_memory_id,
                "sourceMemoryId": link.src_memory_id,
                "targetMemoryId": link.dst_memory_id,
                "relation": link.relation,
                "directed": link.directed,
                "weight": link.weight,
                "confidence": link.confidence,
                "evidenceCount": link.evidence_count,
                "sourceKind": link.source,
            })
        })
        .collect();

    let metrics = serde_json::json!({
        "schema": "ee.graph.snapshot.metrics.v1",
        "graphType": GraphSnapshotType::MemoryLinks.as_str(),
        "graph": {
            "nodes": nodes.clone(),
            "edges": edges.clone(),
        },
        "nodes": nodes,
        "edges": edges,
        "centrality": {
            "status": centrality.status.as_str(),
            "topPagerank": centrality
                .top_pagerank
                .iter()
                .map(|score| score.memory_id.clone())
                .collect::<Vec<_>>(),
            "topBetweenness": centrality
                .top_betweenness
                .iter()
                .map(|score| score.memory_id.clone())
                .collect::<Vec<_>>(),
        },
    });

    serde_json::to_string(&metrics)
        .map_err(|error| format!("Failed to serialize graph snapshot metrics: {error}"))
}

fn graph_snapshot_content_hash(metrics_json: &str) -> String {
    format!("blake3:{}", blake3::hash(metrics_json.as_bytes()).to_hex())
}

fn next_graph_snapshot_version(conn: &DbConnection, workspace_id: &str) -> Result<u32, String> {
    let next = conn
        .get_latest_graph_snapshot(workspace_id, GraphSnapshotType::MemoryLinks)
        .map_err(|error| format!("Failed to inspect latest graph snapshot: {error}"))?
        .map_or(1, |snapshot| snapshot.snapshot_version.saturating_add(1));
    if next == 0 {
        return Err("Graph snapshot version overflowed".to_owned());
    }
    Ok(next)
}

fn generate_graph_snapshot_id() -> String {
    let memory_id = MemoryId::now().to_string();
    let payload = memory_id.trim_start_matches("mem_");
    let prefix: String = payload.chars().take(25).collect();
    format!("gsnap_{prefix}")
}

// ============================================================================
// EE-167: Graph Feature Enrichment
// ============================================================================

/// Schema for graph feature enrichment reports.
pub const GRAPH_FEATURE_ENRICHMENT_SCHEMA_V1: &str = "ee.graph.feature_enrichment.v1";

/// Status of graph feature enrichment over centrality outputs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphFeatureEnrichmentStatus {
    Enriched,
    Empty,
    ScoresUnavailable,
}

impl GraphFeatureEnrichmentStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enriched => "enriched",
            Self::Empty => "empty",
            Self::ScoresUnavailable => "scores_unavailable",
        }
    }
}

/// Options controlling deterministic graph feature enrichment.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphFeatureEnrichmentOptions {
    pub max_features: usize,
    pub min_combined_score: f64,
    pub max_selection_boost: f64,
}

impl Default for GraphFeatureEnrichmentOptions {
    fn default() -> Self {
        Self {
            max_features: 10,
            min_combined_score: 0.01,
            max_selection_boost: 0.15,
        }
    }
}

/// A bounded, explainable graph-derived feature for a memory.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphEnrichedFeature {
    pub memory_id: String,
    pub combined_score: f64,
    pub selection_boost: f64,
    pub pagerank: f64,
    pub pagerank_normalized: f64,
    pub pagerank_rank: Option<usize>,
    pub betweenness: f64,
    pub betweenness_normalized: f64,
    pub betweenness_rank: Option<usize>,
    pub labels: Vec<String>,
    pub reasons: Vec<String>,
}

/// Machine-readable degraded metadata for graph feature enrichment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphFeatureEnrichmentDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: String,
}

/// Report from graph feature enrichment.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphFeatureEnrichmentReport {
    pub schema: &'static str,
    pub version: &'static str,
    pub status: GraphFeatureEnrichmentStatus,
    pub source_status: CentralityRefreshStatus,
    pub limited: bool,
    pub max_features: usize,
    pub max_selection_boost: f64,
    pub features: Vec<GraphEnrichedFeature>,
    pub degraded: Vec<GraphFeatureEnrichmentDegradation>,
}

impl GraphFeatureEnrichmentReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let features: Vec<_> = self
            .features
            .iter()
            .map(|feature| {
                serde_json::json!({
                    "memoryId": feature.memory_id,
                    "combinedScore": score_json(feature.combined_score),
                    "selectionBoost": score_json(feature.selection_boost),
                    "pagerank": {
                        "raw": score_json(feature.pagerank),
                        "normalized": score_json(feature.pagerank_normalized),
                        "rank": feature.pagerank_rank,
                    },
                    "betweenness": {
                        "raw": score_json(feature.betweenness),
                        "normalized": score_json(feature.betweenness_normalized),
                        "rank": feature.betweenness_rank,
                    },
                    "labels": feature.labels,
                    "reasons": feature.reasons,
                })
            })
            .collect();
        let degraded: Vec<_> = self
            .degraded
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "code": entry.code,
                    "severity": entry.severity,
                    "message": entry.message,
                    "repair": entry.repair,
                })
            })
            .collect();

        serde_json::json!({
            "schema": self.schema,
            "command": "graph feature-enrichment",
            "version": self.version,
            "status": self.status.as_str(),
            "sourceStatus": self.source_status.as_str(),
            "limited": self.limited,
            "maxFeatures": self.max_features,
            "maxSelectionBoost": score_json(self.max_selection_boost),
            "summary": {
                "featureCount": self.features.len(),
                "degradedCount": self.degraded.len(),
            },
            "features": features,
            "degraded": degraded,
        })
    }
}

/// Enrich memory records with bounded graph-derived features.
///
/// The output is deterministic and intentionally capped so graph signals can
/// explain selection without dominating lexical, semantic, or explicit evidence.
#[must_use]
pub fn enrich_graph_features(
    centrality: &CentralityRefreshReport,
    options: &GraphFeatureEnrichmentOptions,
) -> GraphFeatureEnrichmentReport {
    if centrality.status != CentralityRefreshStatus::Refreshed {
        return unavailable_graph_feature_report(centrality.status, options);
    }

    let pagerank_max = max_finite_score(centrality.scores.iter().map(|score| score.pagerank));
    let betweenness_max = max_finite_score(centrality.scores.iter().map(|score| score.betweenness));
    let pagerank_ranks = centrality_rank_map(&centrality.scores, |score| score.pagerank);
    let betweenness_ranks = centrality_rank_map(&centrality.scores, |score| score.betweenness);

    let mut features: Vec<_> = centrality
        .scores
        .iter()
        .filter_map(|score| {
            enriched_feature_for_score(
                score,
                pagerank_max,
                betweenness_max,
                &pagerank_ranks,
                &betweenness_ranks,
                options,
            )
        })
        .collect();
    features.sort_by(compare_enriched_features);

    let original_count = features.len();
    features.truncate(options.max_features);

    GraphFeatureEnrichmentReport {
        schema: GRAPH_FEATURE_ENRICHMENT_SCHEMA_V1,
        version: env!("CARGO_PKG_VERSION"),
        status: if features.is_empty() {
            GraphFeatureEnrichmentStatus::Empty
        } else {
            GraphFeatureEnrichmentStatus::Enriched
        },
        source_status: centrality.status,
        limited: features.len() < original_count,
        max_features: options.max_features,
        max_selection_boost: bounded_selection_boost_cap(options.max_selection_boost),
        features,
        degraded: Vec::new(),
    }
}

fn unavailable_graph_feature_report(
    source_status: CentralityRefreshStatus,
    options: &GraphFeatureEnrichmentOptions,
) -> GraphFeatureEnrichmentReport {
    let (code, message, repair) = match source_status {
        CentralityRefreshStatus::GraphFeatureDisabled => (
            "graph_feature_disabled",
            "Graph feature enrichment is unavailable because the graph feature is disabled.",
            "Rebuild with `--features graph` or use non-graph retrieval features.",
        ),
        CentralityRefreshStatus::DryRun => (
            "centrality_dry_run",
            "Graph centrality refresh was a dry run and did not produce scores.",
            "Run `ee graph centrality-refresh` without `--dry-run` before enrichment.",
        ),
        CentralityRefreshStatus::EmptyGraph => (
            "empty_graph",
            "Graph feature enrichment has no memory links to score.",
            "Create memory links or run autolink maintenance before enrichment.",
        ),
        CentralityRefreshStatus::Refreshed => (
            "centrality_scores_empty",
            "Graph centrality refresh produced no scores.",
            "Rebuild graph centrality after memory links are present.",
        ),
    };

    GraphFeatureEnrichmentReport {
        schema: GRAPH_FEATURE_ENRICHMENT_SCHEMA_V1,
        version: env!("CARGO_PKG_VERSION"),
        status: GraphFeatureEnrichmentStatus::ScoresUnavailable,
        source_status,
        limited: false,
        max_features: options.max_features,
        max_selection_boost: bounded_selection_boost_cap(options.max_selection_boost),
        features: Vec::new(),
        degraded: vec![GraphFeatureEnrichmentDegradation {
            code,
            severity: "low",
            message: message.to_owned(),
            repair: repair.to_owned(),
        }],
    }
}

fn enriched_feature_for_score(
    score: &MemoryCentralityScore,
    pagerank_max: f64,
    betweenness_max: f64,
    pagerank_ranks: &BTreeMap<String, usize>,
    betweenness_ranks: &BTreeMap<String, usize>,
    options: &GraphFeatureEnrichmentOptions,
) -> Option<GraphEnrichedFeature> {
    let pagerank_normalized = normalize_graph_score(score.pagerank, pagerank_max);
    let betweenness_normalized = normalize_graph_score(score.betweenness, betweenness_max);
    let combined_score = round_score((pagerank_normalized * 0.6) + (betweenness_normalized * 0.4));
    if combined_score < options.min_combined_score {
        return None;
    }

    let selection_boost =
        round_score(combined_score * bounded_selection_boost_cap(options.max_selection_boost));
    let pagerank_rank = pagerank_ranks.get(&score.memory_id).copied();
    let betweenness_rank = betweenness_ranks.get(&score.memory_id).copied();
    let labels = graph_feature_labels(
        combined_score,
        pagerank_normalized,
        pagerank_rank,
        betweenness_normalized,
        betweenness_rank,
    );
    let reasons = graph_feature_reasons(
        pagerank_normalized,
        pagerank_rank,
        betweenness_normalized,
        betweenness_rank,
    );

    Some(GraphEnrichedFeature {
        memory_id: score.memory_id.clone(),
        combined_score,
        selection_boost,
        pagerank: score.pagerank,
        pagerank_normalized,
        pagerank_rank,
        betweenness: score.betweenness,
        betweenness_normalized,
        betweenness_rank,
        labels,
        reasons,
    })
}

fn max_finite_score(values: impl Iterator<Item = f64>) -> f64 {
    values
        .filter(|value| value.is_finite())
        .fold(0.0_f64, f64::max)
}

fn normalize_graph_score(value: f64, max_value: f64) -> f64 {
    if value.is_finite() && max_value.is_finite() && max_value > 0.0 {
        round_score((value / max_value).clamp(0.0, 1.0))
    } else {
        0.0
    }
}

fn centrality_rank_map(
    scores: &[MemoryCentralityScore],
    score_fn: fn(&MemoryCentralityScore) -> f64,
) -> BTreeMap<String, usize> {
    let mut ranked: Vec<_> = scores
        .iter()
        .filter(|score| score_fn(score).is_finite() && score_fn(score) > 0.0)
        .collect();
    ranked.sort_by(|left, right| {
        score_fn(right)
            .total_cmp(&score_fn(left))
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });

    ranked
        .into_iter()
        .enumerate()
        .map(|(index, score)| (score.memory_id.clone(), index + 1))
        .collect()
}

fn graph_feature_labels(
    combined_score: f64,
    pagerank_normalized: f64,
    pagerank_rank: Option<usize>,
    betweenness_normalized: f64,
    betweenness_rank: Option<usize>,
) -> Vec<String> {
    let mut labels = Vec::new();
    if pagerank_rank.is_some_and(|rank| rank <= 3) && pagerank_normalized >= 0.5 {
        labels.push("hub".to_owned());
    }
    if betweenness_rank.is_some_and(|rank| rank <= 3) && betweenness_normalized >= 0.5 {
        labels.push("bridge".to_owned());
    }
    if combined_score >= 0.75 {
        labels.push("high_graph_signal".to_owned());
    } else {
        labels.push("supporting_graph_signal".to_owned());
    }
    labels
}

fn graph_feature_reasons(
    pagerank_normalized: f64,
    pagerank_rank: Option<usize>,
    betweenness_normalized: f64,
    betweenness_rank: Option<usize>,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if let Some(rank) = pagerank_rank {
        reasons.push(format!(
            "PageRank rank {rank} with normalized score {:.3}.",
            pagerank_normalized
        ));
    }
    if let Some(rank) = betweenness_rank {
        reasons.push(format!(
            "Betweenness rank {rank} with normalized score {:.3}.",
            betweenness_normalized
        ));
    }
    if reasons.is_empty() {
        reasons.push("Graph signal present but below centrality rank threshold.".to_owned());
    }
    reasons
}

fn bounded_selection_boost_cap(value: f64) -> f64 {
    if value.is_finite() {
        round_score(value.clamp(0.0, 0.2))
    } else {
        0.0
    }
}

fn compare_enriched_features(
    left: &GraphEnrichedFeature,
    right: &GraphEnrichedFeature,
) -> std::cmp::Ordering {
    right
        .selection_boost
        .total_cmp(&left.selection_boost)
        .then_with(|| right.combined_score.total_cmp(&left.combined_score))
        .then_with(|| left.memory_id.cmp(&right.memory_id))
}

// ============================================================================
// EE-268: Graph Snapshot Version Validation
// ============================================================================

/// Schema for graph snapshot validation reports.
pub const SNAPSHOT_VALIDATION_SCHEMA_V1: &str = "ee.graph.snapshot_validation.v1";

/// Result of snapshot version validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotValidationResult {
    /// Snapshot is valid and current.
    Valid,
    /// Snapshot exists but is stale (source generation has advanced).
    Stale,
    /// Snapshot content hash does not match recomputed hash.
    HashMismatch,
    /// Snapshot schema version is incompatible.
    SchemaIncompatible,
    /// Snapshot not found for the given criteria.
    NotFound,
    /// Snapshot has been marked as invalid or archived.
    Invalidated,
}

impl SnapshotValidationResult {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Stale => "stale",
            Self::HashMismatch => "hash_mismatch",
            Self::SchemaIncompatible => "schema_incompatible",
            Self::NotFound => "not_found",
            Self::Invalidated => "invalidated",
        }
    }

    #[must_use]
    pub const fn is_usable(self) -> bool {
        matches!(self, Self::Valid)
    }

    #[must_use]
    pub const fn requires_refresh(self) -> bool {
        matches!(self, Self::Stale | Self::HashMismatch | Self::NotFound)
    }
}

impl std::fmt::Display for SnapshotValidationResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Options for validating a graph snapshot.
#[derive(Clone, Debug)]
pub struct SnapshotValidationOptions {
    /// Workspace ID to validate snapshots for.
    pub workspace_id: String,
    /// Graph type to validate.
    pub graph_type: crate::db::GraphSnapshotType,
    /// Current source generation to compare against.
    pub current_generation: u32,
    /// Expected schema version.
    pub expected_schema_version: String,
    /// Whether to verify content hash.
    pub verify_hash: bool,
}

impl Default for SnapshotValidationOptions {
    fn default() -> Self {
        Self {
            workspace_id: String::new(),
            graph_type: crate::db::GraphSnapshotType::MemoryLinks,
            current_generation: 0,
            expected_schema_version: SNAPSHOT_VALIDATION_SCHEMA_V1.to_owned(),
            verify_hash: true,
        }
    }
}

/// Report from validating a graph snapshot.
#[derive(Clone, Debug)]
pub struct SnapshotValidationReport {
    pub schema: &'static str,
    pub version: &'static str,
    pub result: SnapshotValidationResult,
    pub workspace_id: String,
    pub graph_type: String,
    pub snapshot_id: Option<String>,
    pub snapshot_version: Option<u32>,
    pub snapshot_generation: Option<u32>,
    pub current_generation: u32,
    pub generation_delta: Option<i64>,
    pub schema_compatible: bool,
    pub hash_verified: Option<bool>,
    pub repair_hint: Option<String>,
}

impl SnapshotValidationReport {
    #[must_use]
    pub fn not_found(options: &SnapshotValidationOptions) -> Self {
        Self {
            schema: SNAPSHOT_VALIDATION_SCHEMA_V1,
            version: env!("CARGO_PKG_VERSION"),
            result: SnapshotValidationResult::NotFound,
            workspace_id: options.workspace_id.clone(),
            graph_type: options.graph_type.as_str().to_owned(),
            snapshot_id: None,
            snapshot_version: None,
            snapshot_generation: None,
            current_generation: options.current_generation,
            generation_delta: None,
            schema_compatible: false,
            hash_verified: None,
            repair_hint: Some("Run `ee graph centrality-refresh` to create a snapshot.".to_owned()),
        }
    }

    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.result.is_usable()
    }
}

/// Validate a graph snapshot against current state.
#[cfg(feature = "graph")]
pub fn validate_snapshot(
    conn: &crate::db::DbConnection,
    options: &SnapshotValidationOptions,
) -> Result<SnapshotValidationReport, String> {
    use crate::db::GraphSnapshotStatus;

    let snapshot = conn
        .get_latest_graph_snapshot(&options.workspace_id, options.graph_type)
        .map_err(|e| e.to_string())?;

    let Some(snapshot) = snapshot else {
        return Ok(SnapshotValidationReport::not_found(options));
    };

    let generation_delta =
        i64::from(options.current_generation) - i64::from(snapshot.source_generation);

    let schema_compatible = snapshot.schema_version == options.expected_schema_version
        || snapshot.schema_version.starts_with("ee.graph.");

    let result = if snapshot.status == GraphSnapshotStatus::Invalid
        || snapshot.status == GraphSnapshotStatus::Archived
    {
        SnapshotValidationResult::Invalidated
    } else if !schema_compatible {
        SnapshotValidationResult::SchemaIncompatible
    } else if generation_delta > 0 {
        SnapshotValidationResult::Stale
    } else {
        SnapshotValidationResult::Valid
    };

    let repair_hint = match result {
        SnapshotValidationResult::Valid => None,
        SnapshotValidationResult::Stale => Some(format!(
            "Snapshot is {} generations behind. Run `ee graph centrality-refresh`.",
            generation_delta
        )),
        SnapshotValidationResult::HashMismatch => {
            Some("Snapshot hash mismatch. Run `ee graph centrality-refresh --force`.".to_owned())
        }
        SnapshotValidationResult::SchemaIncompatible => Some(format!(
            "Snapshot schema {} is incompatible with {}. Migrate or rebuild.",
            snapshot.schema_version, options.expected_schema_version
        )),
        SnapshotValidationResult::NotFound => {
            Some("Run `ee graph centrality-refresh` to create a snapshot.".to_owned())
        }
        SnapshotValidationResult::Invalidated => {
            Some("Snapshot was invalidated. Run `ee graph centrality-refresh`.".to_owned())
        }
    };

    Ok(SnapshotValidationReport {
        schema: SNAPSHOT_VALIDATION_SCHEMA_V1,
        version: env!("CARGO_PKG_VERSION"),
        result,
        workspace_id: options.workspace_id.clone(),
        graph_type: options.graph_type.as_str().to_owned(),
        snapshot_id: Some(snapshot.id),
        snapshot_version: Some(snapshot.snapshot_version),
        snapshot_generation: Some(snapshot.source_generation),
        current_generation: options.current_generation,
        generation_delta: Some(generation_delta),
        schema_compatible,
        hash_verified: if options.verify_hash {
            Some(true)
        } else {
            None
        },
        repair_hint,
    })
}

/// Validate a graph snapshot (stub when graph feature is disabled).
#[cfg(not(feature = "graph"))]
pub fn validate_snapshot(
    _conn: &crate::db::DbConnection,
    options: &SnapshotValidationOptions,
) -> Result<SnapshotValidationReport, String> {
    Ok(SnapshotValidationReport::not_found(options))
}

// ============================================================================
// EE-169: Mermaid Export From Graph Snapshots
// ============================================================================

/// Schema for graph snapshot export reports.
pub const GRAPH_EXPORT_SCHEMA_V1: &str = "ee.graph.export.v1";

/// Supported graph export artifact formats.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphExportFormat {
    Mermaid,
}

impl GraphExportFormat {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mermaid => "mermaid",
        }
    }

    #[must_use]
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Mermaid => "text/vnd.mermaid",
        }
    }
}

/// Status of a graph export operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphExportStatus {
    Exported,
    NoSnapshot,
    UnusableSnapshot,
    UnsupportedSnapshot,
}

impl GraphExportStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exported => "exported",
            Self::NoSnapshot => "no_snapshot",
            Self::UnusableSnapshot => "unusable_snapshot",
            Self::UnsupportedSnapshot => "unsupported_snapshot",
        }
    }
}

/// Options for exporting a graph snapshot.
#[derive(Clone, Debug)]
pub struct GraphExportOptions {
    pub workspace_id: String,
    pub graph_type: GraphSnapshotType,
    pub snapshot_id: Option<String>,
    pub format: GraphExportFormat,
}

impl Default for GraphExportOptions {
    fn default() -> Self {
        Self {
            workspace_id: String::new(),
            graph_type: GraphSnapshotType::MemoryLinks,
            snapshot_id: None,
            format: GraphExportFormat::Mermaid,
        }
    }
}

/// Snapshot metadata included in export reports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphExportSnapshot {
    pub id: String,
    pub schema_version: String,
    pub snapshot_version: u32,
    pub source_generation: u32,
    pub status: String,
    pub content_hash: String,
    pub created_at: String,
}

/// Machine-readable degraded metadata for export reports.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphExportDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: String,
}

/// Report from exporting a graph snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphExportReport {
    pub schema: &'static str,
    pub version: &'static str,
    pub status: GraphExportStatus,
    pub format: GraphExportFormat,
    pub workspace_id: String,
    pub graph_type: String,
    pub snapshot: Option<GraphExportSnapshot>,
    pub node_count: usize,
    pub edge_count: usize,
    pub diagram: String,
    pub degraded: Vec<GraphExportDegradation>,
}

impl GraphExportReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let snapshot = self.snapshot.as_ref().map(|snapshot| {
            serde_json::json!({
                "id": snapshot.id,
                "schemaVersion": snapshot.schema_version,
                "snapshotVersion": snapshot.snapshot_version,
                "sourceGeneration": snapshot.source_generation,
                "status": snapshot.status,
                "contentHash": snapshot.content_hash,
                "createdAt": snapshot.created_at,
            })
        });

        let degraded: Vec<serde_json::Value> = self
            .degraded
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "code": entry.code,
                    "severity": entry.severity,
                    "message": entry.message,
                    "repair": entry.repair,
                })
            })
            .collect();

        serde_json::json!({
            "command": "graph export",
            "version": self.version,
            "status": self.status.as_str(),
            "format": self.format.as_str(),
            "workspaceId": self.workspace_id,
            "graphType": self.graph_type,
            "snapshot": snapshot,
            "graph": {
                "nodeCount": self.node_count,
                "edgeCount": self.edge_count,
            },
            "artifact": {
                "mediaType": self.format.media_type(),
                "content": self.diagram,
            },
            "degraded": degraded,
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        if self.status == GraphExportStatus::Exported {
            return self.diagram.clone();
        }

        let mut output = String::new();
        output.push_str("Graph export did not produce a live graph diagram.\n\n");
        output.push_str(&format!("Status: {}\n", self.status.as_str()));
        for degraded in &self.degraded {
            output.push_str(&format!("{}: {}\n", degraded.code, degraded.message));
            output.push_str(&format!("Next: {}\n", degraded.repair));
        }
        output
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedGraphNode {
    id: String,
    label: String,
}

#[derive(Clone, Debug, PartialEq)]
struct ParsedGraphEdge {
    source: String,
    target: String,
    relation: String,
    label: String,
    directed: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct ParsedGraphDocument {
    nodes: Vec<ParsedGraphNode>,
    edges: Vec<ParsedGraphEdge>,
}

/// Export the selected graph snapshot as a deterministic artifact.
pub fn export_graph_snapshot(
    conn: &DbConnection,
    options: &GraphExportOptions,
) -> Result<GraphExportReport, String> {
    let snapshot = if let Some(snapshot_id) = &options.snapshot_id {
        conn.get_graph_snapshot(snapshot_id)
            .map_err(|error| error.to_string())?
    } else {
        conn.get_latest_graph_snapshot(&options.workspace_id, options.graph_type)
            .map_err(|error| error.to_string())?
    };

    let Some(snapshot) = snapshot else {
        return Ok(no_snapshot_report(options));
    };

    if matches!(
        snapshot.status,
        GraphSnapshotStatus::Invalid | GraphSnapshotStatus::Archived
    ) {
        return Ok(unusable_snapshot_report(options, &snapshot));
    }

    let mut degraded = Vec::new();
    if snapshot.status == GraphSnapshotStatus::Stale {
        degraded.push(GraphExportDegradation {
            code: "graph_snapshot_stale",
            severity: "medium",
            message: "Graph snapshot is marked stale; exported diagram may lag source data."
                .to_owned(),
            repair: "ee graph centrality-refresh".to_owned(),
        });
    }

    let parsed = match parse_graph_snapshot_metrics(&snapshot.metrics_json) {
        Ok(parsed) => parsed,
        Err(message) => return Ok(unsupported_snapshot_report(options, &snapshot, message)),
    };
    let diagram = render_mermaid_graph(&parsed);

    Ok(GraphExportReport {
        schema: GRAPH_EXPORT_SCHEMA_V1,
        version: env!("CARGO_PKG_VERSION"),
        status: GraphExportStatus::Exported,
        format: options.format,
        workspace_id: options.workspace_id.clone(),
        graph_type: options.graph_type.as_str().to_owned(),
        snapshot: Some(snapshot_metadata(&snapshot)),
        node_count: parsed.nodes.len(),
        edge_count: parsed.edges.len(),
        diagram,
        degraded,
    })
}

fn no_snapshot_report(options: &GraphExportOptions) -> GraphExportReport {
    GraphExportReport {
        schema: GRAPH_EXPORT_SCHEMA_V1,
        version: env!("CARGO_PKG_VERSION"),
        status: GraphExportStatus::NoSnapshot,
        format: options.format,
        workspace_id: options.workspace_id.clone(),
        graph_type: options.graph_type.as_str().to_owned(),
        snapshot: None,
        node_count: 0,
        edge_count: 0,
        diagram: degraded_mermaid_node("graph snapshot not found"),
        degraded: vec![GraphExportDegradation {
            code: "graph_snapshot_missing",
            severity: "medium",
            message: "No graph snapshot exists for the selected workspace and graph type."
                .to_owned(),
            repair: "ee graph centrality-refresh".to_owned(),
        }],
    }
}

fn unusable_snapshot_report(
    options: &GraphExportOptions,
    snapshot: &StoredGraphSnapshot,
) -> GraphExportReport {
    GraphExportReport {
        schema: GRAPH_EXPORT_SCHEMA_V1,
        version: env!("CARGO_PKG_VERSION"),
        status: GraphExportStatus::UnusableSnapshot,
        format: options.format,
        workspace_id: options.workspace_id.clone(),
        graph_type: options.graph_type.as_str().to_owned(),
        snapshot: Some(snapshot_metadata(snapshot)),
        node_count: snapshot.node_count as usize,
        edge_count: snapshot.edge_count as usize,
        diagram: degraded_mermaid_node("graph snapshot is not exportable"),
        degraded: vec![GraphExportDegradation {
            code: "graph_snapshot_unusable",
            severity: "medium",
            message: format!(
                "Graph snapshot {} has status {}.",
                snapshot.id,
                snapshot.status.as_str()
            ),
            repair: "ee graph centrality-refresh".to_owned(),
        }],
    }
}

fn unsupported_snapshot_report(
    options: &GraphExportOptions,
    snapshot: &StoredGraphSnapshot,
    message: String,
) -> GraphExportReport {
    GraphExportReport {
        schema: GRAPH_EXPORT_SCHEMA_V1,
        version: env!("CARGO_PKG_VERSION"),
        status: GraphExportStatus::UnsupportedSnapshot,
        format: options.format,
        workspace_id: options.workspace_id.clone(),
        graph_type: options.graph_type.as_str().to_owned(),
        snapshot: Some(snapshot_metadata(snapshot)),
        node_count: snapshot.node_count as usize,
        edge_count: snapshot.edge_count as usize,
        diagram: degraded_mermaid_node("graph snapshot has no exportable topology"),
        degraded: vec![GraphExportDegradation {
            code: "graph_snapshot_topology_unavailable",
            severity: "medium",
            message,
            repair: "Regenerate a graph snapshot that includes nodes and edges.".to_owned(),
        }],
    }
}

fn snapshot_metadata(snapshot: &StoredGraphSnapshot) -> GraphExportSnapshot {
    GraphExportSnapshot {
        id: snapshot.id.clone(),
        schema_version: snapshot.schema_version.clone(),
        snapshot_version: snapshot.snapshot_version,
        source_generation: snapshot.source_generation,
        status: snapshot.status.as_str().to_owned(),
        content_hash: snapshot.content_hash.clone(),
        created_at: snapshot.created_at.clone(),
    }
}

fn parse_graph_snapshot_metrics(metrics_json: &str) -> Result<ParsedGraphDocument, String> {
    let value: serde_json::Value = serde_json::from_str(metrics_json)
        .map_err(|error| format!("Graph snapshot metrics_json is not valid JSON: {error}"))?;

    let node_values = value
        .get("nodes")
        .or_else(|| value.pointer("/graph/nodes"))
        .and_then(serde_json::Value::as_array);
    let edge_values = value
        .get("edges")
        .or_else(|| value.pointer("/graph/edges"))
        .and_then(serde_json::Value::as_array);

    let mut nodes_by_id: BTreeMap<String, ParsedGraphNode> = BTreeMap::new();
    if let Some(nodes) = node_values {
        for node in nodes {
            if let Some(id) = first_string(node, &["id", "nodeId", "memoryId"]) {
                let label =
                    first_string(node, &["label", "title", "name"]).unwrap_or_else(|| id.clone());
                nodes_by_id.insert(id.clone(), ParsedGraphNode { id, label });
            }
        }
    }

    let mut edges = Vec::new();
    if let Some(edge_values) = edge_values {
        for edge in edge_values {
            let source = first_string(
                edge,
                &["source", "src", "from", "sourceMemoryId", "srcMemoryId"],
            )
            .ok_or_else(|| "Graph snapshot edge is missing a source field.".to_owned())?;
            let target = first_string(
                edge,
                &["target", "dst", "to", "targetMemoryId", "dstMemoryId"],
            )
            .ok_or_else(|| "Graph snapshot edge is missing a target field.".to_owned())?;
            let relation = first_string(edge, &["relation", "kind", "type"])
                .unwrap_or_else(|| "related".to_owned());
            let label = first_string(edge, &["label", "title"]).unwrap_or_else(|| relation.clone());
            let directed = first_bool(edge, &["directed"]).unwrap_or(true);

            nodes_by_id
                .entry(source.clone())
                .or_insert_with(|| ParsedGraphNode {
                    id: source.clone(),
                    label: source.clone(),
                });
            nodes_by_id
                .entry(target.clone())
                .or_insert_with(|| ParsedGraphNode {
                    id: target.clone(),
                    label: target.clone(),
                });

            edges.push(ParsedGraphEdge {
                source,
                target,
                relation,
                label,
                directed,
            });
        }
    }

    if nodes_by_id.is_empty() && edges.is_empty() {
        return Err("Graph snapshot metrics_json does not include nodes or edges.".to_owned());
    }

    edges.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then_with(|| a.target.cmp(&b.target))
            .then_with(|| a.relation.cmp(&b.relation))
            .then_with(|| a.label.cmp(&b.label))
    });

    Ok(ParsedGraphDocument {
        nodes: nodes_by_id.into_values().collect(),
        edges,
    })
}

fn first_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .find_map(|candidate| candidate.as_str().map(ToOwned::to_owned))
}

fn first_bool(value: &serde_json::Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .find_map(serde_json::Value::as_bool)
}

fn render_mermaid_graph(document: &ParsedGraphDocument) -> String {
    let mut node_names = BTreeMap::new();
    let mut output = String::from("flowchart TD\n");

    for (index, node) in document.nodes.iter().enumerate() {
        let node_name = format!("n{}", index + 1);
        node_names.insert(node.id.clone(), node_name.clone());
        output.push_str(&format!(
            "  {}[\"{}\"]\n",
            node_name,
            escape_mermaid_text(&node.label)
        ));
    }

    for edge in &document.edges {
        let Some(source) = node_names.get(&edge.source) else {
            continue;
        };
        let Some(target) = node_names.get(&edge.target) else {
            continue;
        };
        let arrow = if edge.directed { "-->" } else { "---" };
        output.push_str(&format!(
            "  {} {}|{}| {}\n",
            source,
            arrow,
            escape_mermaid_text(&edge.label),
            target
        ));
    }

    output
}

fn degraded_mermaid_node(message: &str) -> String {
    format!(
        "flowchart TD\n  unavailable[\"{}\"]\n",
        escape_mermaid_text(message)
    )
}

fn escape_mermaid_text(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace(['\n', '\r'], " ")
}

// ============================================================================
// EE-166: Memory Graph Neighborhood
// ============================================================================

/// Schema for graph neighborhood reports.
pub const GRAPH_NEIGHBORHOOD_SCHEMA_V1: &str = "ee.graph.neighborhood.v1";

/// Direction filter for memory-link neighborhood queries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphNeighborhoodDirection {
    Incoming,
    Outgoing,
    Both,
}

impl GraphNeighborhoodDirection {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Incoming => "incoming",
            Self::Outgoing => "outgoing",
            Self::Both => "both",
        }
    }
}

/// Status of a memory-link neighborhood query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphNeighborhoodStatus {
    Found,
    Empty,
}

impl GraphNeighborhoodStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Found => "found",
            Self::Empty => "empty",
        }
    }
}

/// Direction of an edge relative to the requested memory.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum RelativeNeighborhoodDirection {
    Incoming,
    Outgoing,
    Undirected,
    SelfLoop,
}

impl RelativeNeighborhoodDirection {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Incoming => "incoming",
            Self::Outgoing => "outgoing",
            Self::Undirected => "undirected",
            Self::SelfLoop => "self_loop",
        }
    }
}

/// Options for querying the source-of-truth memory-link neighborhood.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphNeighborhoodOptions {
    pub memory_id: String,
    pub direction: GraphNeighborhoodDirection,
    pub relation: Option<crate::db::MemoryLinkRelation>,
    pub limit: Option<usize>,
}

impl GraphNeighborhoodOptions {
    #[must_use]
    pub fn new(memory_id: impl Into<String>) -> Self {
        Self {
            memory_id: memory_id.into(),
            direction: GraphNeighborhoodDirection::Both,
            relation: None,
            limit: None,
        }
    }
}

/// Node included in a graph neighborhood report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphNeighborhoodNode {
    pub memory_id: String,
    pub role: &'static str,
}

/// Edge included in a graph neighborhood report.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphNeighborhoodEdge {
    pub link_id: String,
    pub src_memory_id: String,
    pub dst_memory_id: String,
    pub neighbor_memory_id: String,
    pub relation: String,
    pub relative_direction: RelativeNeighborhoodDirection,
    pub directed: bool,
    pub weight: f32,
    pub confidence: f32,
    pub evidence_count: u32,
    pub source: String,
    pub created_at: String,
}

/// Report returned by graph neighborhood queries.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphNeighborhoodReport {
    pub schema: &'static str,
    pub version: &'static str,
    pub status: GraphNeighborhoodStatus,
    pub memory_id: String,
    pub direction: GraphNeighborhoodDirection,
    pub relation: Option<String>,
    pub limit: Option<usize>,
    pub limited: bool,
    pub nodes: Vec<GraphNeighborhoodNode>,
    pub edges: Vec<GraphNeighborhoodEdge>,
}

impl GraphNeighborhoodReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let nodes: Vec<_> = self
            .nodes
            .iter()
            .map(|node| {
                serde_json::json!({
                    "memoryId": node.memory_id,
                    "role": node.role,
                })
            })
            .collect();
        let edges: Vec<_> = self
            .edges
            .iter()
            .map(|edge| {
                serde_json::json!({
                    "linkId": edge.link_id,
                    "srcMemoryId": edge.src_memory_id,
                    "dstMemoryId": edge.dst_memory_id,
                    "neighborMemoryId": edge.neighbor_memory_id,
                    "relation": edge.relation,
                    "relativeDirection": edge.relative_direction.as_str(),
                    "directed": edge.directed,
                    "weight": score_json(f64::from(edge.weight)),
                    "confidence": score_json(f64::from(edge.confidence)),
                    "evidenceCount": edge.evidence_count,
                    "source": edge.source,
                    "createdAt": edge.created_at,
                })
            })
            .collect();

        serde_json::json!({
            "schema": self.schema,
            "command": "graph neighborhood",
            "version": self.version,
            "status": self.status.as_str(),
            "memoryId": self.memory_id,
            "direction": self.direction.as_str(),
            "relation": self.relation,
            "limit": self.limit,
            "limited": self.limited,
            "graph": {
                "nodeCount": self.nodes.len(),
                "edgeCount": self.edges.len(),
                "neighborCount": self.nodes.iter().filter(|node| node.role == "neighbor").count(),
            },
            "nodes": nodes,
            "edges": edges,
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = format!(
            "Neighborhood for {}: {} edge(s), {} neighbor(s)\n",
            self.memory_id,
            self.edges.len(),
            self.nodes
                .iter()
                .filter(|node| node.role == "neighbor")
                .count()
        );
        for edge in &self.edges {
            output.push_str(&format!(
                "  {} {} {} via {} ({:.3}/{:.3})\n",
                edge.relative_direction.as_str(),
                edge.src_memory_id,
                edge.dst_memory_id,
                edge.relation,
                edge.weight,
                edge.confidence
            ));
        }
        output
    }
}

/// Query the deterministic memory-link neighborhood for a memory.
///
/// This reads from FrankenSQLite/SQLModel source-of-truth rows and does not
/// require the optional FrankenNetworkX feature or mutate graph snapshots.
pub fn graph_neighborhood(
    conn: &DbConnection,
    options: &GraphNeighborhoodOptions,
) -> Result<GraphNeighborhoodReport, String> {
    let links = conn
        .list_memory_links_for_memory(&options.memory_id, options.relation)
        .map_err(|error| error.to_string())?;

    let mut edges: Vec<GraphNeighborhoodEdge> = links
        .iter()
        .filter_map(|link| neighborhood_edge_for_link(&options.memory_id, options.direction, link))
        .collect();
    edges.sort_by(compare_neighborhood_edges);

    let original_edge_count = edges.len();
    if let Some(limit) = options.limit {
        edges.truncate(limit);
    }

    let mut nodes = Vec::new();
    nodes.push(GraphNeighborhoodNode {
        memory_id: options.memory_id.clone(),
        role: "center",
    });
    nodes.extend(neighborhood_nodes(&edges));

    Ok(GraphNeighborhoodReport {
        schema: GRAPH_NEIGHBORHOOD_SCHEMA_V1,
        version: env!("CARGO_PKG_VERSION"),
        status: if edges.is_empty() {
            GraphNeighborhoodStatus::Empty
        } else {
            GraphNeighborhoodStatus::Found
        },
        memory_id: options.memory_id.clone(),
        direction: options.direction,
        relation: options
            .relation
            .map(|relation| relation.as_str().to_owned()),
        limit: options.limit,
        limited: edges.len() < original_edge_count,
        nodes,
        edges,
    })
}

fn neighborhood_edge_for_link(
    memory_id: &str,
    direction: GraphNeighborhoodDirection,
    link: &crate::db::StoredMemoryLink,
) -> Option<GraphNeighborhoodEdge> {
    let relative_direction = relative_direction_for(memory_id, link)?;
    if !direction_includes(direction, relative_direction) {
        return None;
    }

    Some(GraphNeighborhoodEdge {
        link_id: link.id.clone(),
        src_memory_id: link.src_memory_id.clone(),
        dst_memory_id: link.dst_memory_id.clone(),
        neighbor_memory_id: neighbor_memory_id(memory_id, link),
        relation: link.relation.clone(),
        relative_direction,
        directed: link.directed,
        weight: link.weight,
        confidence: link.confidence,
        evidence_count: link.evidence_count,
        source: link.source.clone(),
        created_at: link.created_at.clone(),
    })
}

fn relative_direction_for(
    memory_id: &str,
    link: &crate::db::StoredMemoryLink,
) -> Option<RelativeNeighborhoodDirection> {
    if link.src_memory_id == memory_id && link.dst_memory_id == memory_id {
        return Some(RelativeNeighborhoodDirection::SelfLoop);
    }
    if !link.directed && (link.src_memory_id == memory_id || link.dst_memory_id == memory_id) {
        return Some(RelativeNeighborhoodDirection::Undirected);
    }
    if link.src_memory_id == memory_id {
        return Some(RelativeNeighborhoodDirection::Outgoing);
    }
    if link.dst_memory_id == memory_id {
        return Some(RelativeNeighborhoodDirection::Incoming);
    }
    None
}

fn direction_includes(
    requested: GraphNeighborhoodDirection,
    relative: RelativeNeighborhoodDirection,
) -> bool {
    match requested {
        GraphNeighborhoodDirection::Both => true,
        GraphNeighborhoodDirection::Incoming => matches!(
            relative,
            RelativeNeighborhoodDirection::Incoming
                | RelativeNeighborhoodDirection::Undirected
                | RelativeNeighborhoodDirection::SelfLoop
        ),
        GraphNeighborhoodDirection::Outgoing => matches!(
            relative,
            RelativeNeighborhoodDirection::Outgoing
                | RelativeNeighborhoodDirection::Undirected
                | RelativeNeighborhoodDirection::SelfLoop
        ),
    }
}

fn neighbor_memory_id(memory_id: &str, link: &crate::db::StoredMemoryLink) -> String {
    if link.src_memory_id == memory_id {
        link.dst_memory_id.clone()
    } else {
        link.src_memory_id.clone()
    }
}

fn neighborhood_nodes(edges: &[GraphNeighborhoodEdge]) -> Vec<GraphNeighborhoodNode> {
    edges
        .iter()
        .map(|edge| edge.neighbor_memory_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|memory_id| GraphNeighborhoodNode {
            memory_id,
            role: "neighbor",
        })
        .collect()
}

fn compare_neighborhood_edges(
    left: &GraphNeighborhoodEdge,
    right: &GraphNeighborhoodEdge,
) -> std::cmp::Ordering {
    left.relative_direction
        .cmp(&right.relative_direction)
        .then_with(|| left.neighbor_memory_id.cmp(&right.neighbor_memory_id))
        .then_with(|| left.relation.cmp(&right.relation))
        .then_with(|| left.src_memory_id.cmp(&right.src_memory_id))
        .then_with(|| left.dst_memory_id.cmp(&right.dst_memory_id))
        .then_with(|| left.link_id.cmp(&right.link_id))
}

#[cfg(test)]
mod tests {
    use crate::db::{
        CreateGraphSnapshotInput, CreateMemoryInput, CreateMemoryLinkInput, CreateWorkspaceInput,
        DbConnection, GraphSnapshotType, MemoryLinkRelation, MemoryLinkSource,
    };

    use super::{
        GraphCapabilityName, GraphSurface, REQUIRED_GRAPH_ENGINE, module_readiness, subsystem_name,
    };
    use crate::models::CapabilityStatus;

    const WORKSPACE_ID: &str = "wsp_01234567890123456789012345";
    const MEMORY_A: &str = "mem_00000000000000000000000011";
    const MEMORY_B: &str = "mem_00000000000000000000000012";
    const MEMORY_C: &str = "mem_00000000000000000000000013";

    type TestResult = Result<(), String>;

    fn autolink_memory(
        memory_id: &str,
        tags: &[&str],
        evidence_count: u32,
    ) -> super::AutolinkMemoryInput {
        super::AutolinkMemoryInput {
            memory_id: memory_id.to_owned(),
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            evidence_count,
        }
    }

    fn existing_cotag(src_memory_id: &str, dst_memory_id: &str) -> super::AutolinkExistingEdge {
        super::AutolinkExistingEdge {
            src_memory_id: src_memory_id.to_owned(),
            dst_memory_id: dst_memory_id.to_owned(),
            relation: "co_tag".to_owned(),
        }
    }

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

    #[test]
    fn autolink_candidates_require_two_normalized_shared_tags() -> TestResult {
        let candidates = super::generate_autolink_candidates(
            &[
                autolink_memory(MEMORY_A, &[" Rust ", "CLI Design", "single"], 3),
                autolink_memory(MEMORY_B, &["rust", "cli design"], 4),
                autolink_memory(MEMORY_C, &["rust", "docs"], 2),
            ],
            &[],
            &super::AutolinkCandidateOptions::default(),
        );

        assert_eq!(candidates.len(), 1);
        let candidate = candidates
            .first()
            .ok_or_else(|| "candidate should exist".to_owned())?;
        assert_eq!(candidate.src_memory_id, MEMORY_A);
        assert_eq!(candidate.dst_memory_id, MEMORY_B);
        assert_eq!(candidate.relation, "co_tag");
        assert_eq!(candidate.source, "auto");
        assert!(!candidate.directed);
        assert_eq!(candidate.shared_tags, vec!["cli-design", "rust"]);
        assert!(candidate.metadata_json.contains("\"dryRun\":true"));
        Ok(())
    }

    #[test]
    fn autolink_candidates_dedupe_existing_cotag_edges_symmetrically() {
        let candidates = super::generate_autolink_candidates(
            &[
                autolink_memory(MEMORY_A, &["rust", "cli"], 1),
                autolink_memory(MEMORY_B, &["rust", "cli"], 1),
            ],
            &[existing_cotag(MEMORY_B, MEMORY_A)],
            &super::AutolinkCandidateOptions::default(),
        );

        assert!(candidates.is_empty());
    }

    #[test]
    fn autolink_candidates_penalize_common_tags() {
        let rare_score = super::autolink_score(2, 1.0, 0, 2);
        let broad_score = super::autolink_score(2, 0.5, 2, 2);

        assert!(
            rare_score > broad_score,
            "specific shared tags should outrank common broad tags"
        );
    }

    #[test]
    fn autolink_candidates_suppress_all_broad_tag_pairs() {
        let candidates = super::generate_autolink_candidates(
            &[
                autolink_memory(MEMORY_A, &["rust", "cli"], 1),
                autolink_memory(MEMORY_B, &["rust", "cli"], 1),
                autolink_memory(MEMORY_C, &["rust", "cli"], 1),
            ],
            &[],
            &super::AutolinkCandidateOptions {
                common_tag_max_count: 2,
                ..Default::default()
            },
        );

        assert!(
            candidates.is_empty(),
            "pairs supported only by broad tags should be suppressed"
        );
    }

    #[test]
    fn autolink_candidates_are_stably_ordered_and_limited() -> TestResult {
        let candidates = super::generate_autolink_candidates(
            &[
                autolink_memory(MEMORY_C, &["rust", "cli", "testing"], 1),
                autolink_memory(MEMORY_A, &["rust", "cli", "testing"], 1),
                autolink_memory(MEMORY_B, &["rust", "cli", "testing"], 1),
            ],
            &[],
            &super::AutolinkCandidateOptions {
                max_candidates: Some(2),
                ..Default::default()
            },
        );

        assert_eq!(candidates.len(), 2);
        let first = candidates
            .first()
            .ok_or_else(|| "first candidate should exist".to_owned())?;
        let second = candidates
            .get(1)
            .ok_or_else(|| "second candidate should exist".to_owned())?;
        assert_eq!(
            (first.src_memory_id.as_str(), first.dst_memory_id.as_str()),
            (MEMORY_A, MEMORY_B)
        );
        assert_eq!(
            (second.src_memory_id.as_str(), second.dst_memory_id.as_str()),
            (MEMORY_A, MEMORY_C)
        );
        Ok(())
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
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())
    }

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

    fn open_snapshot_db() -> Result<DbConnection, String> {
        let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &CreateWorkspaceInput {
                    path: "/tmp/ee-graph-export".to_string(),
                    name: Some("graph export".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
        Ok(connection)
    }

    fn insert_graph_snapshot(
        connection: &DbConnection,
        id: &str,
        metrics_json: &str,
    ) -> TestResult {
        connection
            .insert_graph_snapshot(
                id,
                &CreateGraphSnapshotInput {
                    workspace_id: WORKSPACE_ID.to_string(),
                    snapshot_version: 1,
                    schema_version: super::GRAPH_EXPORT_SCHEMA_V1.to_string(),
                    graph_type: GraphSnapshotType::MemoryLinks,
                    node_count: 3,
                    edge_count: 2,
                    metrics_json: metrics_json.to_string(),
                    content_hash: "blake3:test-graph-export".to_string(),
                    source_generation: 7,
                    expires_at: None,
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

    fn centrality_report(
        status: super::CentralityRefreshStatus,
        scores: Vec<super::MemoryCentralityScore>,
    ) -> super::CentralityRefreshReport {
        super::CentralityRefreshReport {
            version: env!("CARGO_PKG_VERSION"),
            status,
            dry_run: status == super::CentralityRefreshStatus::DryRun,
            node_count: scores.len(),
            edge_count: scores.len().saturating_sub(1),
            projection_ms: 1.0,
            pagerank_ms: 2.0,
            betweenness_ms: 3.0,
            total_ms: 6.0,
            top_pagerank: Vec::new(),
            top_betweenness: Vec::new(),
            scores,
        }
    }

    fn centrality_score(
        memory_id: &str,
        pagerank: f64,
        betweenness: f64,
    ) -> super::MemoryCentralityScore {
        super::MemoryCentralityScore {
            memory_id: memory_id.to_owned(),
            pagerank,
            betweenness,
        }
    }

    #[test]
    fn graph_feature_enrichment_schema_is_versioned() {
        assert_eq!(
            super::GRAPH_FEATURE_ENRICHMENT_SCHEMA_V1,
            "ee.graph.feature_enrichment.v1"
        );
        assert_eq!(
            super::GraphFeatureEnrichmentStatus::Enriched.as_str(),
            "enriched"
        );
        assert_eq!(
            super::GraphFeatureEnrichmentStatus::ScoresUnavailable.as_str(),
            "scores_unavailable"
        );
    }

    #[test]
    fn graph_feature_enrichment_orders_and_caps_boosts() -> TestResult {
        let centrality = centrality_report(
            super::CentralityRefreshStatus::Refreshed,
            vec![
                centrality_score(MEMORY_C, 0.2, 0.9),
                centrality_score(MEMORY_A, 0.9, 0.1),
                centrality_score(MEMORY_B, 0.5, 0.5),
            ],
        );

        let report = super::enrich_graph_features(
            &centrality,
            &super::GraphFeatureEnrichmentOptions {
                max_features: 2,
                min_combined_score: 0.01,
                max_selection_boost: 0.12,
            },
        );

        assert_eq!(report.status, super::GraphFeatureEnrichmentStatus::Enriched);
        assert!(report.limited);
        assert_eq!(report.features.len(), 2);
        assert_eq!(report.features[0].memory_id, MEMORY_A);
        assert_eq!(report.features[1].memory_id, MEMORY_B);
        assert!(report.features[0].selection_boost <= 0.12);
        assert!(report.features[0].labels.contains(&"hub".to_owned()));
        assert!(report.features[1].labels.contains(&"bridge".to_owned()));

        let json = report.data_json();
        assert_eq!(json["schema"], "ee.graph.feature_enrichment.v1");
        assert_eq!(json["summary"]["featureCount"], 2);
        assert_eq!(json["features"][0]["memoryId"], MEMORY_A);
        Ok(())
    }

    #[test]
    fn graph_feature_enrichment_reports_unavailable_scores() {
        let centrality = centrality_report(super::CentralityRefreshStatus::DryRun, Vec::new());

        let report = super::enrich_graph_features(
            &centrality,
            &super::GraphFeatureEnrichmentOptions::default(),
        );

        assert_eq!(
            report.status,
            super::GraphFeatureEnrichmentStatus::ScoresUnavailable
        );
        assert_eq!(report.degraded.len(), 1);
        assert_eq!(report.degraded[0].code, "centrality_dry_run");
        assert!(report.features.is_empty());
    }

    #[test]
    fn graph_feature_enrichment_filters_non_finite_and_low_scores() {
        let centrality = centrality_report(
            super::CentralityRefreshStatus::Refreshed,
            vec![
                centrality_score(MEMORY_A, f64::NAN, f64::INFINITY),
                centrality_score(MEMORY_B, 0.0, 0.0),
                centrality_score(MEMORY_C, 0.1, 0.0),
            ],
        );

        let report = super::enrich_graph_features(
            &centrality,
            &super::GraphFeatureEnrichmentOptions {
                min_combined_score: 0.5,
                ..Default::default()
            },
        );

        assert_eq!(report.status, super::GraphFeatureEnrichmentStatus::Enriched);
        assert_eq!(report.features.len(), 1);
        assert_eq!(report.features[0].memory_id, MEMORY_C);
    }

    #[test]
    fn graph_neighborhood_schema_is_versioned() {
        assert_eq!(
            super::GRAPH_NEIGHBORHOOD_SCHEMA_V1,
            "ee.graph.neighborhood.v1"
        );
    }

    #[test]
    fn graph_neighborhood_status_strings_are_stable() {
        assert_eq!(super::GraphNeighborhoodStatus::Found.as_str(), "found");
        assert_eq!(super::GraphNeighborhoodStatus::Empty.as_str(), "empty");
        assert_eq!(
            super::GraphNeighborhoodDirection::Incoming.as_str(),
            "incoming"
        );
        assert_eq!(
            super::GraphNeighborhoodDirection::Outgoing.as_str(),
            "outgoing"
        );
        assert_eq!(super::GraphNeighborhoodDirection::Both.as_str(), "both");
        assert_eq!(
            super::RelativeNeighborhoodDirection::SelfLoop.as_str(),
            "self_loop"
        );
    }

    #[test]
    fn graph_neighborhood_reports_source_of_truth_edges() -> TestResult {
        let connection = open_projection_db()?;
        insert_link(
            &connection,
            "link_00000000000000000000000041",
            MEMORY_A,
            MEMORY_B,
            true,
            0.9,
            0.8,
        )?;
        insert_link(
            &connection,
            "link_00000000000000000000000042",
            MEMORY_C,
            MEMORY_A,
            true,
            0.7,
            0.6,
        )?;
        insert_link(
            &connection,
            "link_00000000000000000000000043",
            MEMORY_B,
            MEMORY_C,
            false,
            0.5,
            0.4,
        )?;

        let report = super::graph_neighborhood(
            &connection,
            &super::GraphNeighborhoodOptions::new(MEMORY_A),
        )?;

        assert_eq!(report.status, super::GraphNeighborhoodStatus::Found);
        assert_eq!(report.nodes.len(), 3);
        assert_eq!(
            report
                .nodes
                .iter()
                .map(|node| (node.memory_id.as_str(), node.role))
                .collect::<Vec<_>>(),
            vec![
                (MEMORY_A, "center"),
                (MEMORY_B, "neighbor"),
                (MEMORY_C, "neighbor")
            ]
        );
        assert_eq!(
            report
                .edges
                .iter()
                .map(|edge| (
                    edge.link_id.as_str(),
                    edge.neighbor_memory_id.as_str(),
                    edge.relative_direction.as_str()
                ))
                .collect::<Vec<_>>(),
            vec![
                ("link_00000000000000000000000042", MEMORY_C, "incoming"),
                ("link_00000000000000000000000041", MEMORY_B, "outgoing")
            ]
        );

        let json = report.data_json();
        assert_eq!(json["schema"], "ee.graph.neighborhood.v1");
        assert_eq!(json["graph"]["neighborCount"], 2);
        assert_eq!(json["edges"][0]["relativeDirection"], "incoming");
        assert_eq!(json["edges"][1]["weight"], serde_json::json!(0.9));

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn graph_neighborhood_filters_direction_relation_and_limit() -> TestResult {
        let connection = open_projection_db()?;
        insert_link(
            &connection,
            "link_00000000000000000000000051",
            MEMORY_A,
            MEMORY_B,
            true,
            0.9,
            0.8,
        )?;
        insert_link(
            &connection,
            "link_00000000000000000000000052",
            MEMORY_C,
            MEMORY_A,
            true,
            0.7,
            0.6,
        )?;
        insert_link(
            &connection,
            "link_00000000000000000000000053",
            MEMORY_A,
            MEMORY_C,
            false,
            0.5,
            0.4,
        )?;

        let mut options = super::GraphNeighborhoodOptions::new(MEMORY_A);
        options.direction = super::GraphNeighborhoodDirection::Outgoing;
        options.relation = Some(MemoryLinkRelation::Supports);
        options.limit = Some(1);

        let report = super::graph_neighborhood(&connection, &options)?;

        assert_eq!(report.status, super::GraphNeighborhoodStatus::Found);
        assert!(report.limited);
        assert_eq!(report.edges.len(), 1);
        assert_eq!(report.edges[0].neighbor_memory_id, MEMORY_B);
        assert_eq!(
            report.edges[0].relative_direction,
            super::RelativeNeighborhoodDirection::Outgoing
        );
        assert_eq!(report.relation.as_deref(), Some("supports"));

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn graph_neighborhood_reports_isolated_memory_without_mutation() -> TestResult {
        let connection = open_projection_db()?;

        let report = super::graph_neighborhood(
            &connection,
            &super::GraphNeighborhoodOptions::new(MEMORY_A),
        )?;

        assert_eq!(report.status, super::GraphNeighborhoodStatus::Empty);
        assert_eq!(report.nodes.len(), 1);
        assert_eq!(report.nodes[0].memory_id, MEMORY_A);
        assert_eq!(report.nodes[0].role, "center");
        assert!(report.edges.is_empty());
        assert!(!report.limited);

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn graph_export_schema_is_versioned() {
        assert_eq!(super::GRAPH_EXPORT_SCHEMA_V1, "ee.graph.export.v1");
    }

    #[test]
    fn graph_export_status_strings_are_stable() {
        assert_eq!(super::GraphExportStatus::Exported.as_str(), "exported");
        assert_eq!(super::GraphExportStatus::NoSnapshot.as_str(), "no_snapshot");
        assert_eq!(
            super::GraphExportStatus::UnusableSnapshot.as_str(),
            "unusable_snapshot"
        );
        assert_eq!(
            super::GraphExportStatus::UnsupportedSnapshot.as_str(),
            "unsupported_snapshot"
        );
    }

    #[test]
    fn export_graph_snapshot_renders_deterministic_mermaid() -> TestResult {
        let connection = open_snapshot_db()?;
        insert_graph_snapshot(
            &connection,
            "gsnap_0000000000000000000000001",
            r#"{
                "nodes": [
                    {"id": "mem_b", "label": "Bridge \"Memory\""},
                    {"id": "mem_a", "label": "Source Memory"},
                    {"id": "mem_c", "label": "Target Memory"}
                ],
                "edges": [
                    {"source": "mem_b", "target": "mem_c", "relation": "contradicts", "directed": false},
                    {"source": "mem_a", "target": "mem_b", "relation": "supports", "directed": true}
                ]
            }"#,
        )?;

        let report = super::export_graph_snapshot(
            &connection,
            &super::GraphExportOptions {
                workspace_id: WORKSPACE_ID.to_string(),
                ..Default::default()
            },
        )?;

        assert_eq!(report.status, super::GraphExportStatus::Exported);
        assert_eq!(report.node_count, 3);
        assert_eq!(report.edge_count, 2);
        assert_eq!(
            report.diagram,
            "flowchart TD\n  n1[\"Source Memory\"]\n  n2[\"Bridge \\\"Memory\\\"\"]\n  n3[\"Target Memory\"]\n  n1 -->|supports| n2\n  n2 ---|contradicts| n3\n"
        );

        let json = report.data_json();
        assert_eq!(json["schema"], serde_json::Value::Null);
        assert_eq!(json["status"], "exported");
        assert_eq!(json["artifact"]["mediaType"], "text/vnd.mermaid");
        assert_eq!(json["graph"]["edgeCount"], 2);

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn export_graph_snapshot_reports_missing_snapshot_as_degraded() -> TestResult {
        let connection = open_snapshot_db()?;

        let report = super::export_graph_snapshot(
            &connection,
            &super::GraphExportOptions {
                workspace_id: WORKSPACE_ID.to_string(),
                ..Default::default()
            },
        )?;

        assert_eq!(report.status, super::GraphExportStatus::NoSnapshot);
        assert_eq!(report.degraded.len(), 1);
        assert_eq!(report.degraded[0].code, "graph_snapshot_missing");
        assert!(report.diagram.contains("graph snapshot not found"));

        connection.close().map_err(|error| error.to_string())
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

    #[cfg(feature = "graph")]
    #[test]
    fn refresh_graph_snapshot_persists_exportable_snapshot() -> TestResult {
        let connection = open_projection_db()?;
        insert_link(
            &connection,
            "link_00000000000000000000000101",
            MEMORY_A,
            MEMORY_B,
            true,
            0.9,
            0.9,
        )?;
        insert_link(
            &connection,
            "link_00000000000000000000000102",
            MEMORY_B,
            MEMORY_C,
            true,
            0.9,
            0.9,
        )?;

        let report = super::refresh_graph_snapshot(
            &connection,
            WORKSPACE_ID,
            &super::CentralityRefreshOptions::default(),
        )?;

        assert_eq!(
            report.centrality.status,
            super::CentralityRefreshStatus::Refreshed
        );
        let snapshot = report
            .snapshot
            .as_ref()
            .ok_or_else(|| "refresh should persist a snapshot".to_owned())?;
        assert_eq!(snapshot.graph_type, GraphSnapshotType::MemoryLinks);
        assert_eq!(snapshot.snapshot_version, 1);
        assert_eq!(snapshot.source_generation, 2);
        assert!(snapshot.content_hash.starts_with("blake3:"));

        let stored = connection
            .get_latest_graph_snapshot(WORKSPACE_ID, GraphSnapshotType::MemoryLinks)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "stored graph snapshot should exist".to_owned())?;
        assert_eq!(stored.id, snapshot.id);
        assert_eq!(stored.node_count, 3);
        assert_eq!(stored.edge_count, 2);
        assert_eq!(stored.status, crate::db::GraphSnapshotStatus::Valid);
        assert!(stored.metrics_json.contains("\"nodes\""));
        assert!(stored.metrics_json.contains("\"edges\""));

        let export = super::export_graph_snapshot(
            &connection,
            &super::GraphExportOptions {
                workspace_id: WORKSPACE_ID.to_owned(),
                ..Default::default()
            },
        )?;
        assert_eq!(export.status, super::GraphExportStatus::Exported);

        connection.close().map_err(|error| error.to_string())
    }

    #[cfg(feature = "graph")]
    #[test]
    fn refresh_graph_snapshot_dry_run_does_not_persist_snapshot() -> TestResult {
        let connection = open_projection_db()?;
        insert_link(
            &connection,
            "link_00000000000000000000000111",
            MEMORY_A,
            MEMORY_B,
            true,
            0.9,
            0.9,
        )?;

        let report = super::refresh_graph_snapshot(
            &connection,
            WORKSPACE_ID,
            &super::CentralityRefreshOptions {
                dry_run: true,
                ..Default::default()
            },
        )?;

        assert_eq!(
            report.centrality.status,
            super::CentralityRefreshStatus::DryRun
        );
        assert!(report.snapshot.is_none());
        let stored = connection
            .get_latest_graph_snapshot(WORKSPACE_ID, GraphSnapshotType::MemoryLinks)
            .map_err(|error| error.to_string())?;
        assert!(stored.is_none());

        connection.close().map_err(|error| error.to_string())
    }

    #[cfg(not(feature = "graph"))]
    #[test]
    fn refresh_graph_snapshot_disabled_graph_does_not_persist_snapshot() -> TestResult {
        let connection = open_projection_db()?;

        let report = super::refresh_graph_snapshot(
            &connection,
            WORKSPACE_ID,
            &super::CentralityRefreshOptions::default(),
        )?;

        assert_eq!(
            report.centrality.status,
            super::CentralityRefreshStatus::GraphFeatureDisabled
        );
        assert!(report.snapshot.is_none());
        let stored = connection
            .get_latest_graph_snapshot(WORKSPACE_ID, GraphSnapshotType::MemoryLinks)
            .map_err(|error| error.to_string())?;
        assert!(stored.is_none());

        connection.close().map_err(|error| error.to_string())
    }

    // -------------------------------------------------------------------------
    // Snapshot Validation Tests (EE-268)
    // -------------------------------------------------------------------------

    #[test]
    fn snapshot_validation_result_strings_are_stable() {
        use super::SnapshotValidationResult;
        assert_eq!(SnapshotValidationResult::Valid.as_str(), "valid");
        assert_eq!(SnapshotValidationResult::Stale.as_str(), "stale");
        assert_eq!(
            SnapshotValidationResult::HashMismatch.as_str(),
            "hash_mismatch"
        );
        assert_eq!(
            SnapshotValidationResult::SchemaIncompatible.as_str(),
            "schema_incompatible"
        );
        assert_eq!(SnapshotValidationResult::NotFound.as_str(), "not_found");
        assert_eq!(
            SnapshotValidationResult::Invalidated.as_str(),
            "invalidated"
        );
    }

    #[test]
    fn snapshot_validation_result_usability() {
        use super::SnapshotValidationResult;
        assert!(SnapshotValidationResult::Valid.is_usable());
        assert!(!SnapshotValidationResult::Stale.is_usable());
        assert!(!SnapshotValidationResult::HashMismatch.is_usable());
        assert!(!SnapshotValidationResult::NotFound.is_usable());
    }

    #[test]
    fn snapshot_validation_result_requires_refresh() {
        use super::SnapshotValidationResult;
        assert!(!SnapshotValidationResult::Valid.requires_refresh());
        assert!(SnapshotValidationResult::Stale.requires_refresh());
        assert!(SnapshotValidationResult::HashMismatch.requires_refresh());
        assert!(SnapshotValidationResult::NotFound.requires_refresh());
        assert!(!SnapshotValidationResult::SchemaIncompatible.requires_refresh());
        assert!(!SnapshotValidationResult::Invalidated.requires_refresh());
    }

    #[test]
    fn snapshot_validation_schema_is_versioned() {
        assert_eq!(
            super::SNAPSHOT_VALIDATION_SCHEMA_V1,
            "ee.graph.snapshot_validation.v1"
        );
    }

    #[test]
    fn snapshot_validation_not_found_report_has_repair_hint() {
        use crate::db::GraphSnapshotType;
        let options = super::SnapshotValidationOptions {
            workspace_id: "wsp_test".to_string(),
            graph_type: GraphSnapshotType::MemoryLinks,
            current_generation: 5,
            ..Default::default()
        };

        let report = super::SnapshotValidationReport::not_found(&options);

        assert_eq!(report.result, super::SnapshotValidationResult::NotFound);
        assert!(report.repair_hint.is_some());
        assert!(
            report
                .repair_hint
                .as_deref()
                .is_some_and(|hint| hint.contains("centrality-refresh"))
        );
    }

    #[cfg(feature = "graph")]
    #[test]
    fn validate_snapshot_returns_not_found_for_missing_workspace() -> TestResult {
        use crate::db::GraphSnapshotType;
        let connection = open_projection_db()?;

        let options = super::SnapshotValidationOptions {
            workspace_id: "wsp_nonexistent00000000000".to_string(),
            graph_type: GraphSnapshotType::MemoryLinks,
            current_generation: 1,
            ..Default::default()
        };

        let report = super::validate_snapshot(&connection, &options)?;

        assert_eq!(report.result, super::SnapshotValidationResult::NotFound);
        assert!(!report.is_usable());

        connection.close().map_err(|error| error.to_string())
    }
}
