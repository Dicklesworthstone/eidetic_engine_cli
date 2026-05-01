use std::collections::{BTreeMap, BTreeSet};

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

            let specificity = tag_specificity(&shared_tags, &tag_counts);
            let common_tag_count =
                count_common_tags(&shared_tags, &tag_counts, options.common_tag_max_count);
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
                    "sharedTags": shared_tags,
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
