use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use clap::Args;
use fnx_algorithms::{
    CentralityScore, articulation_points, bridges as fnx_bridges, number_connected_components,
};
use fnx_classes::{AttrMap, Graph};
use fnx_runtime::{CgseValue, CompatibilityMode};
use serde::Serialize;
use serde::ser::SerializeStruct;
use serde_json::Value as JsonValue;

use crate::config::{
    GRAPH_FEATURE_CAUSAL_EXPLAIN_ENABLED_KEY, GRAPH_FEATURE_HITS_PROFILES_ENABLED_KEY,
    GRAPH_FEATURE_LOAD_BEARING_ENABLED_KEY, GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY,
    GRAPH_FEATURE_SKYLINE_ENABLED_KEY,
};
use crate::core::config_surface::{ConfigSurfaceOptions, get_config};
use crate::core::degraded_aggregation::{AggregatedDegradation, aggregate_degraded};
use crate::core::status::DegradationReport;
use crate::db::{DbConnection, StoredMemory, StoredMemoryLink};
use crate::graph::gomory_hu::{
    GOMORY_HU_WEIGHT_ATTR, PROXIMITY_SCHEMA_V1, build_gomory_hu_tree, query_proximity,
};
use crate::graph::hits::{HITS_REPORT_SCHEMA_V1, HitsScores, compute_hits_report};
use crate::graph::skyline::{
    KNOWLEDGE_SKYLINE_SCHEMA_V1, KnowledgeSkyline, KnowledgeSkylineInput, KnowledgeSkylineMemory,
    compute_knowledge_skyline,
};
use crate::models::{DomainError, RESPONSE_SCHEMA_V2};
use crate::output::render_toon_from_json;

pub const INSIGHTS_SCHEMA_V1: &str = "ee.insights.v1";
pub const INSIGHTS_JSON_STREAM_HEADER_SCHEMA_V1: &str = "ee.insights.json_stream.header.v1";
pub const INSIGHTS_JSON_STREAM_SECTION_SCHEMA_V1: &str = "ee.insights.json_stream.section.v1";
pub const INSIGHTS_JSON_STREAM_FOOTER_SCHEMA_V1: &str = "ee.insights.json_stream.footer.v1";
const PROXIMITY_REPORT_SCHEMA_V1: &str = PROXIMITY_SCHEMA_V1;
const CAUSAL_BOTTLENECK_REPORT_SCHEMA_V1: &str = "ee.graph.causal_evidence_projection.v1";
const DEFAULT_SECTION_LIMIT: usize = 10;
const MAX_SECTION_LIMIT: usize = 100;
const EMPTY_WORKSPACE_GENERATED_AT: &str = "1970-01-01T00:00:00Z";
const INSIGHTS_SECTION_UNAVAILABLE_CODE: &str = "insights_section_unavailable";
const INSIGHTS_SECTION_UNAVAILABLE_MESSAGE: &str =
    "One or more registered insights sections do not have DB-backed evidence yet.";
const INSIGHTS_SECTION_UNAVAILABLE_REPAIR: &str =
    "Use sections with non-empty evidence, or implement the unavailable section builder.";
const BLIND_SPOT_SCHEMA_V1: &str = "ee.insights.blind_spots.v1";
const BRIDGE_INSIGHT_SCHEMA_V1: &str = "ee.graph.bridge_insight.v1";
const KNOWLEDGE_GAP_SCHEMA_V1: &str = "ee.graph.knowledge_gap.v1";
const TOP_MEMORY_INSIGHT_SCHEMA_V1: &str = "ee.graph.top_memory.v1";
const KNOWLEDGE_GAP_THIN_EVIDENCE_MAX_SPANS: u32 = 2;
const KNOWLEDGE_GAP_LOW_CONFIDENCE_MAX: f32 = 0.50;

type SectionBuilder = fn() -> InsightsSection;
type SectionRegistryEntry = (&'static str, &'static str, SectionBuilder);

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct InsightsArgs {
    /// Emit only one insight section by name. Names are case-insensitive and
    /// accept both lowercase and canonical-camelCase form. Available:
    /// authorities, blindSpots, bridges, causalBottlenecks, comprehensiveRules,
    /// contradictionClusters, hubs, kCore, kTruss, knowledgeGaps,
    /// knowledgeSkyline, loadBearingMemories, proximityHotspots, revisionFrontiers,
    /// topMemories.
    #[arg(long, value_name = "NAME")]
    pub section: Option<String>,

    /// Frame the insights bundle around a memory explanation target.
    #[arg(long, value_name = "MEMORY_ID", conflicts_with = "section")]
    pub explain: Option<String>,

    /// Maximum items to return for --section output.
    #[arg(long, default_value_t = DEFAULT_SECTION_LIMIT, value_name = "N")]
    pub limit: usize,

    /// Number of section items to skip for --section output.
    #[arg(long, default_value_t = 0, value_name = "N")]
    pub offset: usize,

    /// Emit insights as newline-delimited JSON: header, sections, footer.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub json_stream: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InsightsReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub mode: InsightsMode,
    pub snapshot_version: u64,
    pub generated_at: &'static str,
    pub run_duration_ms: u64,
    pub selected_section: Option<String>,
    pub explain_memory_id: Option<String>,
    pub explain_command: Option<String>,
    pub pagination: Option<InsightsPagination>,
    pub available_sections: Vec<&'static str>,
    pub sections: Vec<InsightsSection>,
    pub degraded_signals: Vec<InsightsDegradedSignal>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InsightsMode {
    FullBundle,
    Section,
    Explain,
}

impl InsightsMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::FullBundle => "full_bundle",
            Self::Section => "section",
            Self::Explain => "explain",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InsightsSection {
    pub name: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
    pub why_it_matters: &'static str,
    pub items: Vec<JsonValue>,
    pub next_commands: Vec<&'static str>,
}

impl Serialize for InsightsSection {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let is_knowledge_gaps = self.name == "knowledgeGaps";
        let mut state = serializer
            .serialize_struct("InsightsSection", if is_knowledge_gaps { 8 } else { 6 })?;
        state.serialize_field("name", self.name)?;
        state.serialize_field("title", self.title)?;
        state.serialize_field("summary", self.summary)?;
        state.serialize_field("whyItMatters", self.why_it_matters)?;
        state.serialize_field("items", &self.items)?;
        state.serialize_field("nextCommands", &self.next_commands)?;
        if is_knowledge_gaps {
            state.serialize_field("section", self.name)?;
            state.serialize_field(
                "recommendations",
                &knowledge_gap_recommendations_from_items(&self.items),
            )?;
        }
        state.end()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InsightsPagination {
    pub limit: usize,
    pub offset: usize,
    pub returned: usize,
    pub total: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InsightsDegradedSignal {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: Option<String>,
    pub sources: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct ProximityHotspotInput {
    memory_a: String,
    memory_b: String,
    snapshot_version: u64,
    min_cut: Option<f64>,
    interpretation: String,
    tree_path: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq)]
struct CausalBottleneckInput {
    memory_id: String,
    betweenness: f64,
    snapshot_version: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BridgeInsightInput {
    memory_id: String,
    degree: usize,
    bridge_edge_count: usize,
    cluster_disconnection_magnitude: usize,
    component_count_before: usize,
    component_count_after: usize,
    snapshot_version: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct BlindSpotInput {
    symbol_id: String,
    path: String,
    canonical_name: String,
    symbol_kind: &'static str,
    start_line: u32,
    end_line: u32,
    snapshot_hash: String,
    total_node_count: usize,
    covered_node_count: usize,
    coverage_ratio: f64,
}

#[derive(Clone, Debug, Default)]
struct BlindSpotCoverageIndex {
    file_provenance_paths: BTreeSet<String>,
    lexical_corpus: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlindSpotCoverageSource {
    FileProvenance,
    LexicalPathMention,
    LexicalSymbolMention,
}

impl BlindSpotCoverageSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::FileProvenance => "file_provenance",
            Self::LexicalPathMention => "lexical_path_mention",
            Self::LexicalSymbolMention => "lexical_symbol_mention",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct KnowledgeGapInput {
    category: &'static str,
    source_memory_ids: Vec<String>,
    metric_evidence: JsonValue,
    explanation: String,
    confidence: f64,
    priority: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct HitsInsightInput {
    memory_id: String,
    score: f64,
    snapshot_version: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct TopMemoryInsightInput {
    memory_id: String,
    level: String,
    kind: String,
    trust_class: String,
    confidence: f64,
    utility: f64,
    importance: f64,
    pagerank: f64,
    retrieval_posture: f64,
    link_degree: usize,
    incoming_link_count: usize,
    outgoing_link_count: usize,
    created_at: String,
    snapshot_version: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct HitsSectionSpec {
    name: &'static str,
    title: &'static str,
    summary: &'static str,
    why_it_matters: &'static str,
    interpretation: &'static str,
    score_field: &'static str,
    next_commands: Vec<&'static str>,
}

struct BuiltSection {
    section: InsightsSection,
    degraded_signal: Option<InsightsDegradedInput>,
}

type InsightsDegradedInput = (&'static str, DegradationReport);

#[derive(Clone, Copy, Debug)]
struct GraphFeatureGate {
    key: &'static str,
    message: &'static str,
    repair: &'static str,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InsightsBuildOptions<'a> {
    pub workspace: Option<&'a Path>,
}

fn section_registry() -> Vec<SectionRegistryEntry> {
    vec![
        ("authorities", "authorities", authorities_section),
        ("blindspots", "blindSpots", blind_spots_section),
        ("bridges", "bridges", bridges_section),
        (
            "causalbottlenecks",
            "causalBottlenecks",
            causal_bottlenecks_section,
        ),
        (
            "comprehensiverules",
            "comprehensiveRules",
            comprehensive_rules_section,
        ),
        (
            "contradictionclusters",
            "contradictionClusters",
            contradiction_clusters_section,
        ),
        ("hubs", "hubs", hubs_section),
        ("kcore", "kCore", k_core_section),
        ("ktruss", "kTruss", k_truss_section),
        ("knowledgegaps", "knowledgeGaps", knowledge_gaps_section),
        (
            "knowledgeskyline",
            "knowledgeSkyline",
            knowledge_skyline_section,
        ),
        (
            "loadbearingmemories",
            "loadBearingMemories",
            load_bearing_memories_section,
        ),
        (
            "proximityhotspots",
            "proximityHotspots",
            proximity_hotspots_section,
        ),
        (
            "revisionfrontiers",
            "revisionFrontiers",
            revision_frontiers_section,
        ),
        ("topmemories", "topMemories", top_memories_section),
    ]
}

#[cfg(test)]
pub fn build_insights_report(args: &InsightsArgs) -> Result<InsightsReport, DomainError> {
    build_insights_report_with_options(args, InsightsBuildOptions::default())
}

pub fn build_insights_report_with_options(
    args: &InsightsArgs,
    options: InsightsBuildOptions<'_>,
) -> Result<InsightsReport, DomainError> {
    let registry = section_registry();
    let available_sections: Vec<&'static str> = registry.iter().map(|(_, name, _)| *name).collect();
    let mode = if args.explain.is_some() {
        InsightsMode::Explain
    } else if args.section.is_some() {
        InsightsMode::Section
    } else {
        InsightsMode::FullBundle
    };

    let (selected_section, sections, pagination, gated_degraded_signals) = if let Some(section) =
        args.section.as_deref()
    {
        let normalized = normalize_section_name(section);
        let Some((_, display_name, builder)) = registry
            .iter()
            .find(|(lookup_name, _, _)| *lookup_name == normalized)
        else {
            let available = available_sections.join(", ");
            return Err(DomainError::Usage {
                message: format!(
                    "Unknown insights section `{section}`. Available sections: {available}."
                ),
                repair: Some("ee insights --help".to_owned()),
            });
        };
        let built =
            build_registry_section_with_runtime_gate(display_name, *builder, options.workspace)?;
        let section = paginate_section(built.section, args.offset, args.limit);
        (
            Some((*display_name).to_owned()),
            vec![section.section],
            Some(section.pagination),
            built.degraded_signal.into_iter().collect::<Vec<_>>(),
        )
    } else {
        (
            None,
            registry
                .iter()
                .map(|(_, display_name, builder)| {
                    build_registry_section(display_name, *builder, options.workspace)
                })
                .collect::<Result<Vec<_>, DomainError>>()?,
            None,
            Vec::new(),
        )
    };

    let explain_memory_id = args.explain.clone();
    let explain_command = explain_memory_id
        .as_ref()
        .map(|memory_id| format!("ee why {memory_id} --json"));
    let raw_degraded_signals = if gated_degraded_signals.is_empty() {
        // Load the workspace graph counts so an all-empty bundle can explain *why* it is
        // empty (no memories vs. memories-but-no-links) instead of returning silent success.
        let insights_graph_data = load_workspace_insights_graph_data(options.workspace)
            .ok()
            .flatten();
        degraded_signals_for_sections(&sections, insights_graph_data.as_ref())
    } else {
        gated_degraded_signals
    };
    let degraded_signals = aggregate_insights_degraded(raw_degraded_signals);

    Ok(InsightsReport {
        schema: INSIGHTS_SCHEMA_V1,
        command: "insights",
        mode,
        snapshot_version: 0,
        generated_at: EMPTY_WORKSPACE_GENERATED_AT,
        run_duration_ms: 0,
        selected_section,
        explain_memory_id,
        explain_command,
        pagination,
        available_sections,
        sections,
        degraded_signals,
    })
}

// bd-113r0: these registered sections are still metadata-only builders.
// Emit an explicit section-unavailable degradation so empty `items[]`
// never has to mean "no graph data", "feature disabled", and
// "placeholder implementation" at the same time. When a real builder
// lands for one of these names, remove it from this list in the same
// change as the `build_registry_section` arm.
const PLACEHOLDER_BACKED_SECTIONS: &[&str] =
    &["comprehensiveRules", "kCore", "kTruss", "revisionFrontiers"];

fn build_registry_section_with_runtime_gate(
    display_name: &'static str,
    builder: SectionBuilder,
    workspace: Option<&Path>,
) -> Result<BuiltSection, DomainError> {
    if let Some(gate) = graph_feature_gate_for_section(display_name) {
        if !runtime_graph_feature_enabled(workspace, gate.key)? {
            return Ok(BuiltSection {
                section: builder(),
                degraded_signal: Some((
                    display_name,
                    DegradationReport {
                        code: "graph_feature_disabled",
                        severity: "medium",
                        message: gate.message,
                        repair: gate.repair,
                    },
                )),
            });
        }
    }

    let section = build_registry_section(display_name, builder, workspace)?;
    // Keep selected-section output honest even when broad full-bundle
    // degraded aggregation is not running.
    let degraded_signal = if PLACEHOLDER_BACKED_SECTIONS.contains(&display_name) {
        Some((
            display_name,
            DegradationReport {
                code: INSIGHTS_SECTION_UNAVAILABLE_CODE,
                severity: "info",
                message: INSIGHTS_SECTION_UNAVAILABLE_MESSAGE,
                repair: INSIGHTS_SECTION_UNAVAILABLE_REPAIR,
            },
        ))
    } else {
        None
    };

    Ok(BuiltSection {
        section,
        degraded_signal,
    })
}

fn build_registry_section(
    display_name: &str,
    builder: SectionBuilder,
    workspace: Option<&Path>,
) -> Result<InsightsSection, DomainError> {
    match display_name {
        "authorities" => {
            let scores = load_hits_scores(workspace)?;
            Ok(authorities_section_from_scores(&scores))
        }
        "blindSpots" => {
            let inputs = load_blind_spot_inputs(workspace)?;
            Ok(blind_spots_section_from_inputs(&inputs))
        }
        "causalBottlenecks" => {
            let reports = load_causal_bottleneck_reports(workspace)?;
            Ok(causal_bottlenecks_section_from_reports(&reports))
        }
        "bridges" => {
            let inputs = load_bridge_inputs(workspace)?;
            Ok(bridges_section_from_inputs(&inputs))
        }
        "contradictionClusters" => {
            let clusters = load_contradiction_clusters(workspace)?;
            Ok(contradiction_clusters_section_from_clusters(&clusters))
        }
        "hubs" => {
            let scores = load_hits_scores(workspace)?;
            Ok(hubs_section_from_scores(&scores))
        }
        "knowledgeSkyline" => {
            let skyline = load_knowledge_skyline(workspace)?;
            Ok(knowledge_skyline_section_from_report(skyline.as_ref()))
        }
        "knowledgeGaps" => {
            let gaps = load_knowledge_gap_inputs(workspace)?;
            Ok(knowledge_gaps_section_from_inputs(&gaps))
        }
        "loadBearingMemories" => {
            let items = load_bearing_memory_items(workspace)?;
            Ok(load_bearing_memories_section_from_items(&items))
        }
        "proximityHotspots" => {
            let reports = load_proximity_hotspot_reports(workspace)?;
            Ok(proximity_hotspots_section_from_reports(&reports))
        }
        "topMemories" => {
            let inputs = load_top_memory_inputs(workspace)?;
            Ok(top_memories_section_from_inputs(&inputs))
        }
        _ => Ok(builder()),
    }
}

fn graph_feature_gate_for_section(display_name: &str) -> Option<GraphFeatureGate> {
    match display_name {
        "authorities" | "hubs" => Some(GraphFeatureGate {
            key: GRAPH_FEATURE_HITS_PROFILES_ENABLED_KEY,
            message: "HITS profile insights are disabled by graph.feature.hits_profiles.enabled.",
            repair: "ee config set graph.feature.hits_profiles.enabled true",
        }),
        "causalBottlenecks" => Some(GraphFeatureGate {
            key: GRAPH_FEATURE_CAUSAL_EXPLAIN_ENABLED_KEY,
            message: "Causal bottleneck insights are disabled by graph.feature.causal_explain.enabled.",
            repair: "ee config set graph.feature.causal_explain.enabled true",
        }),
        "knowledgeSkyline" => Some(GraphFeatureGate {
            key: GRAPH_FEATURE_SKYLINE_ENABLED_KEY,
            message: "Knowledge skyline insights are disabled by graph.feature.skyline.enabled.",
            repair: "ee config set graph.feature.skyline.enabled true",
        }),
        "loadBearingMemories" => Some(GraphFeatureGate {
            key: GRAPH_FEATURE_LOAD_BEARING_ENABLED_KEY,
            message: "Load-bearing memory insights are disabled by graph.feature.load_bearing.enabled.",
            repair: "ee config set graph.feature.load_bearing.enabled true",
        }),
        "revisionFrontiers" => Some(GraphFeatureGate {
            key: GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY,
            message: "Revision frontier insights are disabled by graph.feature.revision_dominance.enabled.",
            repair: "ee config set graph.feature.revision_dominance.enabled true",
        }),
        _ => None,
    }
}

fn runtime_graph_feature_enabled(
    workspace: Option<&Path>,
    key: &'static str,
) -> Result<bool, DomainError> {
    let Some(workspace) = workspace else {
        return Ok(true);
    };
    let options = ConfigSurfaceOptions {
        workspace_root: workspace.to_path_buf(),
        config_path: None,
    };
    get_config(&options, key)
        .map(|report| report.value == "true")
        .map_err(|error| DomainError::Configuration {
            message: format!("Could not read graph feature flag `{key}`: {error}"),
            repair: Some("ee config show graph.feature.* --json".to_owned()),
        })
}

fn degraded_signals_for_sections(
    sections: &[InsightsSection],
    graph_data: Option<&WorkspaceInsightsGraphData>,
) -> Vec<InsightsDegradedInput> {
    let mut degraded = sections
        .iter()
        .filter(|section| section.items.is_empty())
        .filter_map(|section| placeholder_section_degraded_input(section.name))
        .collect::<Vec<_>>();

    if sections.iter().all(|section| section.items.is_empty()) {
        // Distinguish "no memories at all" from "memories exist but the link graph is
        // empty". The latter is the common silent-empty case: graph insights (PageRank,
        // HITS, bridges, proximity) need edges, and remember-time auto-linking only
        // connects memories within the same explicit workflow — so a hand-built corpus
        // of unrelated `ee remember` calls leaves every section empty with no hint why.
        let has_memories_without_links =
            graph_data.is_some_and(|data| !data.memories.is_empty() && data.links.is_empty());
        let signal = if has_memories_without_links {
            DegradationReport {
                code: "graph.no_links",
                severity: "low",
                message: "Graph insights are empty: this workspace has memories but no links between them, so PageRank, HITS, bridges, and proximity have nothing to analyze. Remember-time auto-linking only connects memories in the same explicit workflow.",
                repair: "populate the link graph: `ee import cass --workspace . --json` or `ee link <id-a> <id-b> --relation related --workspace .`",
            }
        } else {
            DegradationReport {
                code: "graph.workspace_empty",
                severity: "info",
                message: "No graph memories are available for insights yet.",
                repair: "run: ee remember --workspace . \"<memory>\" --json",
            }
        };
        degraded.push(("insights", signal));
    }

    degraded
}

fn placeholder_section_degraded_input(section_name: &'static str) -> Option<InsightsDegradedInput> {
    if PLACEHOLDER_BACKED_SECTIONS.contains(&section_name) {
        Some((
            section_name,
            DegradationReport {
                code: INSIGHTS_SECTION_UNAVAILABLE_CODE,
                severity: "info",
                message: INSIGHTS_SECTION_UNAVAILABLE_MESSAGE,
                repair: INSIGHTS_SECTION_UNAVAILABLE_REPAIR,
            },
        ))
    } else {
        None
    }
}

fn aggregate_insights_degraded(entries: Vec<InsightsDegradedInput>) -> Vec<InsightsDegradedSignal> {
    aggregate_degraded(entries)
        .into_iter()
        .map(InsightsDegradedSignal::from)
        .collect()
}

impl From<AggregatedDegradation> for InsightsDegradedSignal {
    fn from(entry: AggregatedDegradation) -> Self {
        Self {
            code: entry.code,
            severity: entry.severity,
            message: entry.message,
            repair: Some(entry.repair),
            sources: entry.sources,
        }
    }
}

struct WorkspaceInsightsGraphData {
    memories: Vec<StoredMemory>,
    links: Vec<StoredMemoryLink>,
}

fn load_workspace_insights_graph_data(
    workspace: Option<&Path>,
) -> Result<Option<WorkspaceInsightsGraphData>, DomainError> {
    let Some(workspace) = workspace else {
        return Ok(None);
    };
    let database_path = workspace.join(".ee").join("ee.db");
    if !database_path.exists() {
        return Ok(None);
    }

    let connection =
        DbConnection::open_file(&database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open workspace database: {error}"),
            repair: Some("Run `ee doctor --workspace . --json`.".to_owned()),
        })?;
    let Some(workspace_id) = insights_workspace_id(&connection, workspace)? else {
        return Ok(None);
    };
    let memories = connection
        .list_memories(&workspace_id, None, false)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query workspace memories: {error}"),
            repair: Some("Run `ee doctor --workspace . --json`.".to_owned()),
        })?;
    let memory_ids = memories
        .iter()
        .map(|memory| memory.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let links = connection
        .list_all_memory_links(None)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query memory links: {error}"),
            repair: Some("Run `ee doctor --workspace . --json`.".to_owned()),
        })?
        .into_iter()
        .filter(|link| {
            memory_ids.contains(link.src_memory_id.as_str())
                && memory_ids.contains(link.dst_memory_id.as_str())
        })
        .collect::<Vec<_>>();

    Ok(Some(WorkspaceInsightsGraphData { memories, links }))
}

fn load_bridge_inputs(workspace: Option<&Path>) -> Result<Vec<BridgeInsightInput>, DomainError> {
    let Some(data) = load_workspace_insights_graph_data(workspace)? else {
        return Ok(Vec::new());
    };
    bridge_inputs_from_links(&data.links)
}

fn load_blind_spot_inputs(workspace: Option<&Path>) -> Result<Vec<BlindSpotInput>, DomainError> {
    let Some(workspace) = workspace else {
        return Ok(Vec::new());
    };
    let rust_paths = collect_rust_source_paths(workspace)?;
    if rust_paths.is_empty() {
        return Ok(Vec::new());
    }
    let snapshot = crate::core::symbol_graph::SymbolGraphExtractor::default()
        .extract_paths(workspace, rust_paths);
    let memories = load_workspace_insights_graph_data(Some(workspace))?
        .map(|data| data.memories)
        .unwrap_or_default();
    Ok(blind_spot_inputs_from_symbol_snapshot(
        &snapshot,
        &memories,
        Some(workspace),
    ))
}

fn load_contradiction_clusters(
    workspace: Option<&Path>,
) -> Result<Vec<crate::graph::health::ContradictionCluster>, DomainError> {
    let Some(data) = load_workspace_insights_graph_data(workspace)? else {
        return Ok(Vec::new());
    };
    contradiction_clusters_from_links(&data.links)
}

fn load_knowledge_gap_inputs(
    workspace: Option<&Path>,
) -> Result<Vec<KnowledgeGapInput>, DomainError> {
    let Some(data) = load_workspace_insights_graph_data(workspace)? else {
        return Ok(Vec::new());
    };
    knowledge_gap_inputs_from_graph_data(&data)
}

fn load_top_memory_inputs(
    workspace: Option<&Path>,
) -> Result<Vec<TopMemoryInsightInput>, DomainError> {
    let Some(data) = load_workspace_insights_graph_data(workspace)? else {
        return Ok(Vec::new());
    };
    top_memory_inputs_from_graph_data(&data)
}

fn load_knowledge_skyline(
    workspace: Option<&Path>,
) -> Result<Option<KnowledgeSkyline>, DomainError> {
    let Some(data) = load_workspace_insights_graph_data(workspace)? else {
        return Ok(None);
    };
    if data.memories.is_empty() {
        return Ok(None);
    }

    let mut skyline_graph = proximity_graph_from_links(&data.links)?;
    let mut skyline_memories = Vec::with_capacity(data.memories.len());
    for memory in &data.memories {
        skyline_graph.add_node(memory.id.clone());
        let created_at = DateTime::parse_from_rfc3339(&memory.created_at)
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Failed to parse memory created_at for knowledge skyline `{}`: {error}",
                    memory.id
                ),
                repair: Some("Run `ee doctor --workspace . --json`.".to_owned()),
            })?
            .with_timezone(&Utc);
        skyline_memories.push(KnowledgeSkylineMemory {
            memory_id: memory.id.clone(),
            trust_class: memory.trust_class.clone(),
            created_at,
        });
    }

    let ppr_scores = pagerank_scores_for_skyline(&data.links)?;
    let Some(as_of) = skyline_memories
        .iter()
        .map(|memory| memory.created_at)
        .max()
    else {
        return Ok(None);
    };
    Ok(Some(compute_knowledge_skyline(&KnowledgeSkylineInput {
        graph: skyline_graph,
        memories: skyline_memories,
        ppr_scores,
        as_of,
    })))
}

fn load_proximity_hotspot_reports(
    workspace: Option<&Path>,
) -> Result<Vec<ProximityHotspotInput>, DomainError> {
    let Some(workspace) = workspace else {
        return Ok(Vec::new());
    };
    let database_path = workspace.join(".ee").join("ee.db");
    if !database_path.exists() {
        return Ok(Vec::new());
    }

    let connection =
        DbConnection::open_file(&database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open workspace database: {error}"),
            repair: Some("Run `ee doctor --workspace . --json`.".to_owned()),
        })?;
    let links = connection
        .list_all_memory_links(None)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query memory links: {error}"),
            repair: Some("Run `ee doctor --workspace . --json`.".to_owned()),
        })?;

    proximity_hotspot_reports_from_links(&links)
}

fn load_causal_bottleneck_reports(
    workspace: Option<&Path>,
) -> Result<Vec<CausalBottleneckInput>, DomainError> {
    let Some(workspace) = workspace else {
        return Ok(Vec::new());
    };
    let database_path = workspace.join(".ee").join("ee.db");
    if !database_path.exists() {
        return Ok(Vec::new());
    }

    let connection =
        DbConnection::open_file(&database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open workspace database: {error}"),
            repair: Some("Run `ee doctor --workspace . --json`.".to_owned()),
        })?;
    let Some(workspace_id) = insights_workspace_id(&connection, workspace)? else {
        return Ok(Vec::new());
    };
    let graph = crate::graph::build_causal_evidence_graph_from_table(&connection, &workspace_id)
        .map_err(|error| DomainError::Graph {
            message: format!("Failed to build causal-evidence graph projection: {error}"),
            repair: Some(
                "Run `ee graph snapshot refresh --graph causal_evidence --workspace . --json`."
                    .to_owned(),
            ),
        })?;
    let betweenness = crate::graph::betweenness_centrality_directed(&graph);
    Ok(causal_bottleneck_reports_from_scores(&betweenness.scores))
}

fn load_hits_scores(workspace: Option<&Path>) -> Result<HitsScores, DomainError> {
    let Some(workspace) = workspace else {
        return Ok(HitsScores::default());
    };
    let database_path = workspace.join(".ee").join("ee.db");
    if !database_path.exists() {
        return Ok(HitsScores::default());
    }

    let connection =
        DbConnection::open_file(&database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open workspace database: {error}"),
            repair: Some("Run `ee doctor --workspace . --json`.".to_owned()),
        })?;
    let projection =
        crate::graph::build_memory_graph(&connection, &crate::graph::ProjectionOptions::default())
            .map_err(|error| DomainError::Graph {
                message: format!("Failed to build HITS memory-link projection: {error}"),
                repair: Some(
                    "Run `ee graph snapshot refresh --graph memory_links --workspace . --json`."
                        .to_owned(),
                ),
            })?;
    if projection.node_count == 0 {
        return Ok(HitsScores::default());
    }

    compute_hits_report(&projection.graph)
        .map(|report| report.scores)
        .map_err(|error| DomainError::Graph {
            message: format!("Failed to compute HITS insights: {error}"),
            repair: Some(
                "Run `ee graph snapshot refresh --graph memory_links --workspace . --json`."
                    .to_owned(),
            ),
        })
}

fn load_bearing_memory_items(
    workspace: Option<&Path>,
) -> Result<Vec<crate::graph::bipartite_provenance::LoadBearingMemoryItem>, DomainError> {
    let Some(workspace) = workspace else {
        return Ok(Vec::new());
    };
    let database_path = workspace.join(".ee").join("ee.db");
    if !database_path.exists() {
        return Ok(Vec::new());
    }

    let connection =
        DbConnection::open_file(&database_path).map_err(|error| DomainError::Storage {
            message: format!("Failed to open workspace database: {error}"),
            repair: Some("Run `ee doctor --workspace . --json`.".to_owned()),
        })?;
    let Some(workspace_id) = insights_workspace_id(&connection, workspace)? else {
        return Ok(Vec::new());
    };
    let graph =
        crate::graph::build_rule_provenance_bipartite_from_tables(&connection, &workspace_id)
            .map_err(|error| DomainError::Graph {
                message: format!("Failed to build rule-provenance bipartite projection: {error}"),
                repair: Some(
                    "Run `ee graph snapshot refresh --graph rule_provenance --workspace . --json`."
                        .to_owned(),
                ),
            })?;
    if graph.node_count() == 0 {
        return Ok(Vec::new());
    }
    let snapshot_version = connection
        .get_latest_graph_snapshot(&workspace_id, crate::db::GraphSnapshotType::RuleProvenance)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query rule-provenance graph snapshot: {error}"),
            repair: Some("Run `ee doctor --workspace . --json`.".to_owned()),
        })?
        .map_or(0, |snapshot| u64::from(snapshot.snapshot_version));
    let hits =
        crate::graph::bipartite_provenance::compute_bipartite_hits(&graph).map_err(|error| {
            DomainError::Graph {
                message: format!("Failed to compute bipartite HITS insights: {error}"),
                repair: Some(
                    "Run `ee graph snapshot refresh --graph rule_provenance --workspace . --json`."
                        .to_owned(),
                ),
            }
        })?;
    Ok(
        crate::graph::bipartite_provenance::load_bearing_memory_items(
            &graph,
            &hits,
            snapshot_version,
        ),
    )
}

fn insights_workspace_id(
    connection: &DbConnection,
    workspace: &Path,
) -> Result<Option<String>, DomainError> {
    for candidate in workspace_path_candidates(workspace) {
        let key = candidate.to_string_lossy();
        let row = connection
            .get_workspace_by_path(key.as_ref())
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to query workspace row: {error}"),
                repair: Some("Run `ee doctor --workspace . --json`.".to_owned()),
            })?;
        if let Some(workspace) = row {
            return Ok(Some(workspace.id));
        }
    }

    connection
        .list_workspaces()
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to list workspace rows: {error}"),
            repair: Some("Run `ee doctor --workspace . --json`.".to_owned()),
        })
        .map(|workspaces| workspaces.into_iter().next().map(|workspace| workspace.id))
}

fn workspace_path_candidates(workspace: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(canonical) = workspace.canonicalize() {
        candidates.push(canonical);
    }
    if !candidates.iter().any(|candidate| candidate == workspace) {
        candidates.push(workspace.to_path_buf());
    }
    candidates
}

fn proximity_hotspot_reports_from_links(
    links: &[StoredMemoryLink],
) -> Result<Vec<ProximityHotspotInput>, DomainError> {
    if links.is_empty() {
        return Ok(Vec::new());
    }

    let graph = proximity_graph_from_links(links)?;
    if graph.node_count() < 2 {
        return Ok(Vec::new());
    }

    let tree = build_gomory_hu_tree(&graph).map_err(|error| DomainError::Graph {
        message: format!("Failed to build Gomory-Hu proximity tree: {error}"),
        repair: Some(
            "Run `ee graph snapshot refresh --graph memory_links --workspace . --json`.".to_owned(),
        ),
    })?;
    let nodes = tree
        .tree
        .nodes_ordered()
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let mut reports = Vec::new();
    for (index, left) in nodes.iter().enumerate() {
        for right in nodes.iter().skip(index + 1) {
            let report = query_proximity(&tree, left, right, 0);
            reports.push(ProximityHotspotInput {
                memory_a: report.memory_a,
                memory_b: report.memory_b,
                snapshot_version: report.snapshot_version,
                min_cut: report.min_cut,
                interpretation: report.interpretation,
                tree_path: report.tree_path,
            });
        }
    }
    Ok(reports)
}

fn bridge_inputs_from_links(
    links: &[StoredMemoryLink],
) -> Result<Vec<BridgeInsightInput>, DomainError> {
    let graph = proximity_graph_from_links(links)?;
    if graph.node_count() < 3 {
        return Ok(Vec::new());
    }

    let articulation = articulation_points(&graph);
    let bridge_edges = fnx_bridges(&graph).edges;
    let component_count_before = number_connected_components(&graph).count;
    let mut inputs = articulation
        .nodes
        .into_iter()
        .map(|memory_id| {
            let degree = graph
                .neighbors(&memory_id)
                .map_or(0, |neighbors| neighbors.len());
            let bridge_edge_count = bridge_edges
                .iter()
                .filter(|(left, right)| left == &memory_id || right == &memory_id)
                .count();
            let mut graph_without_memory = graph.clone();
            graph_without_memory.remove_node(&memory_id);
            let component_count_after = if graph_without_memory.node_count() == 0 {
                0
            } else {
                number_connected_components(&graph_without_memory).count
            };
            let cluster_disconnection_magnitude =
                component_count_after.saturating_sub(component_count_before);
            BridgeInsightInput {
                memory_id,
                degree,
                bridge_edge_count,
                cluster_disconnection_magnitude,
                component_count_before,
                component_count_after,
                snapshot_version: 0,
            }
        })
        .collect::<Vec<_>>();
    inputs.sort_by(|left, right| {
        right
            .cluster_disconnection_magnitude
            .cmp(&left.cluster_disconnection_magnitude)
            .then_with(|| right.bridge_edge_count.cmp(&left.bridge_edge_count))
            .then_with(|| right.degree.cmp(&left.degree))
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
    Ok(inputs)
}

fn contradiction_cluster_relation(relation: &str) -> bool {
    matches!(relation, "contradicts" | "resolves" | "supersedes")
}

fn contradiction_clusters_from_links(
    links: &[StoredMemoryLink],
) -> Result<Vec<crate::graph::health::ContradictionCluster>, DomainError> {
    let contradiction_links = links
        .iter()
        .filter(|link| contradiction_cluster_relation(&link.relation))
        .cloned()
        .collect::<Vec<_>>();
    if contradiction_links.is_empty() {
        return Ok(Vec::new());
    }
    let graph = proximity_graph_from_links(&contradiction_links)?;
    Ok(crate::graph::health::detect_contradiction_clusters(&graph))
}

fn collect_rust_source_paths(workspace: &Path) -> Result<Vec<PathBuf>, DomainError> {
    let mut paths = Vec::new();
    collect_rust_source_paths_inner(workspace, workspace, &mut paths)?;
    paths.sort_by_key(|path| normalize_path_for_insights_order(workspace, path));
    Ok(paths)
}

fn collect_rust_source_paths_inner(
    workspace: &Path,
    dir: &Path,
    paths: &mut Vec<PathBuf>,
) -> Result<(), DomainError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if dir == workspace && error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(error) => {
            return Err(DomainError::Storage {
                message: format!(
                    "Failed to read Rust source directory `{}`: {error}",
                    dir.display()
                ),
                repair: Some("Run `ee doctor --workspace . --json`.".to_owned()),
            });
        }
    };
    let mut entries =
        entries
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Failed to enumerate Rust source directory `{}`: {error}",
                    dir.display()
                ),
                repair: Some("Run `ee doctor --workspace . --json`.".to_owned()),
            })?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if should_skip_blind_spot_dir(&file_name) {
            continue;
        }
        let metadata = entry.metadata().map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to inspect Rust source path `{}`: {error}",
                path.display()
            ),
            repair: Some("Run `ee doctor --workspace . --json`.".to_owned()),
        })?;
        if metadata.is_dir() {
            collect_rust_source_paths_inner(workspace, &path, paths)?;
        } else if metadata.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == std::ffi::OsStr::new("rs"))
        {
            paths.push(path);
        }
    }
    Ok(())
}

fn should_skip_blind_spot_dir(file_name: &str) -> bool {
    matches!(
        file_name,
        ".git" | ".hg" | ".svn" | ".ee" | ".beads" | "target" | "node_modules"
    )
}

fn normalize_path_for_insights_order(workspace: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(workspace).unwrap_or(path);
    normalize_insights_path(&relative.to_string_lossy())
}

fn blind_spot_inputs_from_symbol_snapshot(
    snapshot: &crate::models::SymbolSnapshot,
    memories: &[StoredMemory],
    workspace: Option<&Path>,
) -> Vec<BlindSpotInput> {
    if snapshot.symbols.is_empty() {
        return Vec::new();
    }
    let coverage = blind_spot_coverage_index(memories, workspace);
    let total_node_count = snapshot.symbols.len();
    let mut covered_node_count = 0usize;
    let mut uncovered = Vec::new();

    let mut symbols = snapshot.symbols.iter().collect::<Vec<_>>();
    symbols.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.range.start_line.cmp(&right.range.start_line))
            .then_with(|| left.canonical_name.cmp(&right.canonical_name))
            .then_with(|| left.id.cmp(&right.id))
    });

    for symbol in symbols {
        if coverage.source_for(symbol).is_some() {
            covered_node_count += 1;
            continue;
        }
        uncovered.push(symbol);
    }

    let coverage_ratio = if total_node_count == 0 {
        1.0
    } else {
        covered_node_count as f64 / total_node_count as f64
    };

    uncovered
        .into_iter()
        .map(|symbol| BlindSpotInput {
            symbol_id: symbol.id.clone(),
            path: symbol.path.clone(),
            canonical_name: symbol.canonical_name.clone(),
            symbol_kind: symbol.kind.as_str(),
            start_line: symbol.range.start_line,
            end_line: symbol.range.end_line,
            snapshot_hash: snapshot.snapshot_hash.clone(),
            total_node_count,
            covered_node_count,
            coverage_ratio,
        })
        .collect()
}

fn blind_spot_coverage_index(
    memories: &[StoredMemory],
    workspace: Option<&Path>,
) -> BlindSpotCoverageIndex {
    let mut index = BlindSpotCoverageIndex::default();
    for memory in memories {
        if let Some(provenance_uri) = memory.provenance_uri.as_deref() {
            if let Some(path) = file_provenance_path_for_blind_spots(provenance_uri, workspace) {
                index.file_provenance_paths.insert(path.clone());
                index.lexical_corpus.push(' ');
                index.lexical_corpus.push_str(&path.to_ascii_lowercase());
            }
        }
        index.lexical_corpus.push(' ');
        index
            .lexical_corpus
            .push_str(&memory.content.to_ascii_lowercase());
    }
    index
}

impl BlindSpotCoverageIndex {
    fn source_for(&self, symbol: &crate::models::SymbolRecord) -> Option<BlindSpotCoverageSource> {
        if self.file_provenance_paths.contains(&symbol.path) {
            return Some(BlindSpotCoverageSource::FileProvenance);
        }
        let path = symbol.path.to_ascii_lowercase();
        if !path.is_empty() && self.lexical_corpus.contains(&path) {
            return Some(BlindSpotCoverageSource::LexicalPathMention);
        }
        if contains_identifier_mention(&self.lexical_corpus, &symbol.canonical_name) {
            return Some(BlindSpotCoverageSource::LexicalSymbolMention);
        }
        symbol
            .canonical_name
            .rsplit("::")
            .next()
            .filter(|short_name| *short_name != symbol.canonical_name)
            .and_then(|short_name| {
                contains_identifier_mention(&self.lexical_corpus, short_name)
                    .then_some(BlindSpotCoverageSource::LexicalSymbolMention)
            })
    }
}

fn file_provenance_path_for_blind_spots(
    provenance_uri: &str,
    workspace: Option<&Path>,
) -> Option<String> {
    let path = provenance_uri.strip_prefix("file://")?;
    let path = path.split('#').next().unwrap_or(path);
    if path.trim().is_empty() {
        return None;
    }
    let path = Path::new(path);
    let relative = workspace.and_then(|workspace| path.strip_prefix(workspace).ok());
    let normalized = relative.unwrap_or(path).to_string_lossy();
    Some(normalize_insights_path(&normalized))
}

fn normalize_insights_path(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_owned()
}

fn contains_identifier_mention(corpus: &str, needle: &str) -> bool {
    let needle = needle.to_ascii_lowercase();
    if needle.len() < 4 {
        return false;
    }
    let mut search_from = 0usize;
    while let Some(offset) = corpus[search_from..].find(&needle) {
        let start = search_from + offset;
        let end = start + needle.len();
        let before = corpus[..start].chars().next_back();
        let after = corpus[end..].chars().next();
        if before.is_none_or(|character| !is_identifier_char(character))
            && after.is_none_or(|character| !is_identifier_char(character))
        {
            return true;
        }
        search_from = end;
    }
    false
}

fn is_identifier_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn knowledge_gap_inputs_from_graph_data(
    data: &WorkspaceInsightsGraphData,
) -> Result<Vec<KnowledgeGapInput>, DomainError> {
    if data.memories.is_empty() && data.links.is_empty() {
        return Ok(Vec::new());
    }

    let memories_by_id = data
        .memories
        .iter()
        .map(|memory| (memory.id.as_str(), memory))
        .collect::<BTreeMap<_, _>>();
    let incident_evidence = incident_evidence_span_counts(&data.links);
    let mut gaps = Vec::new();

    for bridge in bridge_inputs_from_links(&data.links)? {
        let evidence_span_count = incident_evidence
            .get(&bridge.memory_id)
            .copied()
            .unwrap_or_default();
        if bridge.cluster_disconnection_magnitude == 0
            || evidence_span_count > KNOWLEDGE_GAP_THIN_EVIDENCE_MAX_SPANS
        {
            continue;
        }
        gaps.push(KnowledgeGapInput {
            category: "thin_evidence_bridge",
            source_memory_ids: vec![bridge.memory_id.clone()],
            metric_evidence: serde_json::json!({
                "schema": KNOWLEDGE_GAP_SCHEMA_V1,
                "signal": "articulation_bridge",
                "algorithm": "tarjan_articulation_points",
                "clusterDisconnectionMagnitude": bridge.cluster_disconnection_magnitude,
                "bridgeEdgeCount": bridge.bridge_edge_count,
                "evidenceSpanCount": evidence_span_count,
            }),
            explanation: format!(
                "Memory `{}` disconnects graph neighborhoods but has only {} evidence span(s).",
                bridge.memory_id, evidence_span_count
            ),
            confidence: 0.82,
            priority: 90 + bridge.cluster_disconnection_magnitude as u64,
        });
    }

    for cluster in contradiction_clusters_from_links(&data.links)? {
        let source_memory_ids =
            sorted_unique_memory_ids(cluster.exemplar_memory_ids.iter().map(String::as_str));
        if source_memory_ids.is_empty()
            || contradiction_cluster_has_resolution(&source_memory_ids, &data.links)
        {
            continue;
        }
        gaps.push(KnowledgeGapInput {
            category: "unresolved_contradiction_cluster",
            source_memory_ids,
            metric_evidence: serde_json::json!({
                "schema": KNOWLEDGE_GAP_SCHEMA_V1,
                "signal": "contradiction_cluster",
                "algorithm": "louvain_communities",
                "louvainId": cluster.louvain_id,
                "size": cluster.size,
                "internalContradictions": cluster.internal_contradictions,
                "density": cluster.density,
            }),
            explanation: format!(
                "Contradiction cluster {} has no resolving or superseding edge.",
                cluster.louvain_id
            ),
            confidence: 0.76,
            priority: 80 + cluster.internal_contradictions as u64,
        });
    }

    for memory in data
        .memories
        .iter()
        .filter(|memory| memory_is_harmful_outcome(memory))
    {
        let source_memory_ids =
            harmful_neighborhood_source_ids(memory, &data.links, &memories_by_id);
        let has_rule = source_memory_ids
            .iter()
            .filter_map(|memory_id| memories_by_id.get(memory_id.as_str()))
            .any(|memory| memory_is_procedural_rule(memory));
        if has_rule {
            continue;
        }
        gaps.push(KnowledgeGapInput {
            category: "harmful_neighborhood_without_rule",
            metric_evidence: serde_json::json!({
                "schema": KNOWLEDGE_GAP_SCHEMA_V1,
                "signal": "harmful_outcome_neighborhood",
                "algorithm": "one_hop_rule_presence",
                "harmfulMemoryId": memory.id,
                "neighborCount": source_memory_ids.len().saturating_sub(1),
                "proceduralRuleCount": 0,
            }),
            explanation: format!(
                "Harmful outcome `{}` has no adjacent procedural rule memory.",
                memory.id
            ),
            confidence: 0.70,
            priority: 70,
            source_memory_ids,
        });
    }

    for link in data.links.iter().filter(|link| {
        crate::graph::memory_link_mesh_metadata_visible(link.metadata_json.as_deref())
            && matches!(link.relation.as_str(), "supports" | "derived_from")
            && link.confidence.is_finite()
            && link.confidence <= KNOWLEDGE_GAP_LOW_CONFIDENCE_MAX
    }) {
        let source_memory_ids =
            sorted_unique_memory_ids([link.src_memory_id.as_str(), link.dst_memory_id.as_str()]);
        gaps.push(KnowledgeGapInput {
            category: "underdetermined_causal_chain",
            source_memory_ids,
            metric_evidence: serde_json::json!({
                "schema": KNOWLEDGE_GAP_SCHEMA_V1,
                "signal": "low_confidence_causal_link",
                "algorithm": "link_confidence_threshold",
                "relation": link.relation,
                "linkConfidence": link.confidence,
                "threshold": KNOWLEDGE_GAP_LOW_CONFIDENCE_MAX,
            }),
            explanation: format!(
                "{} link `{}` -> `{}` has confidence {:.3}.",
                link.relation, link.src_memory_id, link.dst_memory_id, link.confidence
            ),
            confidence: 0.66,
            priority: 60,
        });
    }

    sort_and_dedup_knowledge_gaps(&mut gaps);
    Ok(gaps)
}

fn incident_evidence_span_counts(links: &[StoredMemoryLink]) -> BTreeMap<String, u32> {
    let mut counts = BTreeMap::<String, u32>::new();
    for link in links {
        if !crate::graph::memory_link_mesh_metadata_visible(link.metadata_json.as_deref()) {
            continue;
        }
        for memory_id in [&link.src_memory_id, &link.dst_memory_id] {
            let count = counts.entry(memory_id.clone()).or_default();
            *count = count.saturating_add(link.evidence_count);
        }
    }
    counts
}

fn contradiction_cluster_has_resolution(
    source_memory_ids: &[String],
    links: &[StoredMemoryLink],
) -> bool {
    let cluster_ids = source_memory_ids.iter().cloned().collect::<BTreeSet<_>>();
    links.iter().any(|link| {
        crate::graph::memory_link_mesh_metadata_visible(link.metadata_json.as_deref())
            && matches!(link.relation.as_str(), "resolves" | "supersedes")
            && cluster_ids.contains(&link.src_memory_id)
            && cluster_ids.contains(&link.dst_memory_id)
    })
}

fn harmful_neighborhood_source_ids(
    memory: &StoredMemory,
    links: &[StoredMemoryLink],
    memories_by_id: &BTreeMap<&str, &StoredMemory>,
) -> Vec<String> {
    let mut source_ids = BTreeSet::from([memory.id.clone()]);
    for link in links {
        if !crate::graph::memory_link_mesh_metadata_visible(link.metadata_json.as_deref()) {
            continue;
        }
        if link.src_memory_id == memory.id
            && memories_by_id.contains_key(link.dst_memory_id.as_str())
        {
            source_ids.insert(link.dst_memory_id.clone());
        }
        if link.dst_memory_id == memory.id
            && memories_by_id.contains_key(link.src_memory_id.as_str())
        {
            source_ids.insert(link.src_memory_id.clone());
        }
    }
    source_ids.into_iter().collect()
}

fn memory_is_procedural_rule(memory: &StoredMemory) -> bool {
    memory.level.eq_ignore_ascii_case("procedural")
        || memory.kind.to_ascii_lowercase().contains("rule")
}

fn memory_is_harmful_outcome(memory: &StoredMemory) -> bool {
    let kind = memory.kind.to_ascii_lowercase();
    let content = memory.content.to_ascii_lowercase();
    kind.contains("failure")
        || kind.contains("harm")
        || content.contains("harmful")
        || content.contains("unsafe")
}

fn sorted_unique_memory_ids<'a>(ids: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    ids.into_iter()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn top_memory_inputs_from_graph_data(
    data: &WorkspaceInsightsGraphData,
) -> Result<Vec<TopMemoryInsightInput>, DomainError> {
    if data.memories.is_empty() || data.links.is_empty() {
        return Ok(Vec::new());
    }
    let pagerank_scores = pagerank_scores_for_skyline(&data.links)?;
    if pagerank_scores.is_empty() {
        return Ok(Vec::new());
    }
    let link_counts = top_memory_link_counts(&data.links);
    let mut inputs = data
        .memories
        .iter()
        .filter_map(|memory| {
            let pagerank = pagerank_scores.get(memory.id.as_str()).copied()?;
            if !pagerank.is_finite() || pagerank <= 0.0 {
                return None;
            }
            let retrieval_posture = retrieval_posture_score(memory);
            let counts = link_counts
                .get(memory.id.as_str())
                .copied()
                .unwrap_or_default();
            Some(TopMemoryInsightInput {
                memory_id: memory.id.clone(),
                level: memory.level.clone(),
                kind: memory.kind.clone(),
                trust_class: memory.trust_class.clone(),
                confidence: finite_score(memory.confidence),
                utility: finite_score(memory.utility),
                importance: finite_score(memory.importance),
                pagerank,
                retrieval_posture,
                link_degree: counts.link_degree,
                incoming_link_count: counts.incoming_link_count,
                outgoing_link_count: counts.outgoing_link_count,
                created_at: memory.created_at.clone(),
                snapshot_version: 0,
            })
        })
        .collect::<Vec<_>>();
    sort_top_memory_inputs(&mut inputs);
    Ok(inputs)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct TopMemoryLinkCounts {
    link_degree: usize,
    incoming_link_count: usize,
    outgoing_link_count: usize,
}

fn top_memory_link_counts(links: &[StoredMemoryLink]) -> BTreeMap<String, TopMemoryLinkCounts> {
    let mut incoming = BTreeMap::<String, usize>::new();
    let mut outgoing = BTreeMap::<String, usize>::new();
    let mut neighbors = BTreeMap::<String, BTreeSet<String>>::new();

    for link in links {
        if !crate::graph::memory_link_mesh_metadata_visible(link.metadata_json.as_deref()) {
            continue;
        }
        record_top_memory_link_direction(
            &mut incoming,
            &mut outgoing,
            &mut neighbors,
            &link.src_memory_id,
            &link.dst_memory_id,
        );
        if !link.directed {
            record_top_memory_link_direction(
                &mut incoming,
                &mut outgoing,
                &mut neighbors,
                &link.dst_memory_id,
                &link.src_memory_id,
            );
        }
    }

    let ids = incoming
        .keys()
        .chain(outgoing.keys())
        .chain(neighbors.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    ids.into_iter()
        .map(|memory_id| {
            let counts = TopMemoryLinkCounts {
                link_degree: neighbors.get(&memory_id).map_or(0, BTreeSet::len),
                incoming_link_count: incoming.get(&memory_id).copied().unwrap_or_default(),
                outgoing_link_count: outgoing.get(&memory_id).copied().unwrap_or_default(),
            };
            (memory_id, counts)
        })
        .collect()
}

fn record_top_memory_link_direction(
    incoming: &mut BTreeMap<String, usize>,
    outgoing: &mut BTreeMap<String, usize>,
    neighbors: &mut BTreeMap<String, BTreeSet<String>>,
    source: &str,
    target: &str,
) {
    *outgoing.entry(source.to_owned()).or_default() += 1;
    *incoming.entry(target.to_owned()).or_default() += 1;
    neighbors
        .entry(source.to_owned())
        .or_default()
        .insert(target.to_owned());
    neighbors
        .entry(target.to_owned())
        .or_default()
        .insert(source.to_owned());
}

fn finite_score(score: f32) -> f64 {
    if score.is_finite() {
        f64::from(score).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn retrieval_posture_score(memory: &StoredMemory) -> f64 {
    (finite_score(memory.confidence) * 0.40)
        + (finite_score(memory.utility) * 0.35)
        + (finite_score(memory.importance) * 0.25)
}

fn sort_top_memory_inputs(inputs: &mut [TopMemoryInsightInput]) {
    inputs.sort_by(|left, right| {
        right
            .pagerank
            .total_cmp(&left.pagerank)
            .then_with(|| right.retrieval_posture.total_cmp(&left.retrieval_posture))
            .then_with(|| right.link_degree.cmp(&left.link_degree))
            .then_with(|| right.incoming_link_count.cmp(&left.incoming_link_count))
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });
}

fn sort_and_dedup_knowledge_gaps(gaps: &mut Vec<KnowledgeGapInput>) {
    gaps.retain(|gap| !gap.source_memory_ids.is_empty());
    gaps.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.category.cmp(right.category))
            .then_with(|| left.source_memory_ids.cmp(&right.source_memory_ids))
    });
    let mut seen = BTreeSet::<(String, Vec<String>)>::new();
    gaps.retain(|gap| seen.insert((gap.category.to_owned(), gap.source_memory_ids.clone())));
}

fn pagerank_scores_for_skyline(
    links: &[StoredMemoryLink],
) -> Result<std::collections::BTreeMap<String, f64>, DomainError> {
    if links.is_empty() {
        return Ok(std::collections::BTreeMap::new());
    }

    let mut graph = crate::graph::DiGraph::new(CompatibilityMode::Strict);
    let mut inserted_edges = std::collections::BTreeSet::<(String, String)>::new();
    for link in links {
        if !crate::graph::memory_link_mesh_metadata_visible(link.metadata_json.as_deref()) {
            continue;
        }
        graph.add_node(link.src_memory_id.clone());
        graph.add_node(link.dst_memory_id.clone());
        add_skyline_pagerank_edge(
            &mut graph,
            &mut inserted_edges,
            &link.src_memory_id,
            &link.dst_memory_id,
        )?;
        if !link.directed {
            add_skyline_pagerank_edge(
                &mut graph,
                &mut inserted_edges,
                &link.dst_memory_id,
                &link.src_memory_id,
            )?;
        }
    }
    if graph.node_count() == 0 {
        return Ok(std::collections::BTreeMap::new());
    }

    let node_count = graph.node_count();
    let edge_count = graph.edge_count();
    let projection = crate::graph::MemoryGraphProjection {
        graph,
        node_count,
        edge_count,
        build_ms: 0.0,
        snapshot_version: 0,
    };
    crate::graph::compute_pagerank(&projection)
        .map(|report| {
            report
                .scores
                .into_iter()
                .map(|score| (score.node, score.score))
                .collect()
        })
        .map_err(|error| DomainError::Graph {
            message: format!("Failed to compute knowledge-skyline PageRank scores: {error}"),
            repair: Some(
                "Run `ee graph snapshot refresh --graph memory_links --workspace . --json`."
                    .to_owned(),
            ),
        })
}

fn add_skyline_pagerank_edge(
    graph: &mut crate::graph::DiGraph,
    inserted_edges: &mut std::collections::BTreeSet<(String, String)>,
    source: &str,
    target: &str,
) -> Result<(), DomainError> {
    if !inserted_edges.insert((source.to_owned(), target.to_owned())) {
        return Ok(());
    }
    graph
        .add_edge(source, target)
        .map_err(|error| DomainError::Graph {
            message: format!("Failed to build knowledge-skyline PageRank graph: {error}"),
            repair: Some("Validate memory link rows with `ee doctor --json`.".to_owned()),
        })
}

fn causal_bottleneck_reports_from_scores(scores: &[CentralityScore]) -> Vec<CausalBottleneckInput> {
    scores
        .iter()
        .map(|score| CausalBottleneckInput {
            memory_id: score.node.clone(),
            betweenness: score.score,
            snapshot_version: 0,
        })
        .collect()
}

fn hits_inputs_from_scores(
    scores: &std::collections::BTreeMap<String, f64>,
) -> Vec<HitsInsightInput> {
    scores
        .iter()
        .map(|(memory_id, score)| HitsInsightInput {
            memory_id: memory_id.clone(),
            score: *score,
            snapshot_version: 0,
        })
        .collect()
}

fn proximity_graph_from_links(links: &[StoredMemoryLink]) -> Result<Graph, DomainError> {
    let mut graph = Graph::strict();
    for link in links {
        if !crate::graph::memory_link_mesh_metadata_visible(link.metadata_json.as_deref()) {
            continue;
        }
        let mut attrs = AttrMap::new();
        attrs.insert(
            GOMORY_HU_WEIGHT_ATTR.to_owned(),
            CgseValue::Float(f64::from(link.weight)),
        );
        attrs.insert(
            "confidence".to_owned(),
            CgseValue::Float(f64::from(link.confidence)),
        );
        attrs.insert(
            "relation".to_owned(),
            CgseValue::String(link.relation.clone()),
        );
        attrs.insert("source".to_owned(), CgseValue::String(link.source.clone()));
        attrs.insert(
            "evidence_count".to_owned(),
            CgseValue::Int(i64::from(link.evidence_count)),
        );
        graph
            .add_edge_with_attrs(
                link.src_memory_id.clone(),
                link.dst_memory_id.clone(),
                attrs,
            )
            .map_err(|error| DomainError::Graph {
                message: format!("Failed to build proximity graph projection: {error}"),
                repair: Some("Validate memory link rows with `ee doctor --json`.".to_owned()),
            })?;
    }
    Ok(graph)
}

fn normalize_section_name(section: &str) -> String {
    section
        .trim()
        .chars()
        .filter(|character| *character != '-' && *character != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

struct PaginatedSection {
    section: InsightsSection,
    pagination: InsightsPagination,
}

fn paginate_section(
    mut section: InsightsSection,
    offset: usize,
    requested_limit: usize,
) -> PaginatedSection {
    let limit = requested_limit.clamp(1, MAX_SECTION_LIMIT);
    let total = section.items.len();
    section.items = section.items.into_iter().skip(offset).take(limit).collect();
    let returned = section.items.len();

    PaginatedSection {
        section,
        pagination: InsightsPagination {
            limit,
            offset,
            returned,
            total,
        },
    }
}

#[must_use]
pub fn render_insights_json(report: &InsightsReport) -> String {
    // Migrated from RESPONSE_SCHEMA_V1 to V2 (G2 / docs-schemas drift work):
    // every other top-level CLI surface (status, doctor, capabilities, why,
    // context, search…) now emits the ee.response.v2 envelope. v2 is a
    // pure-superset of v1's shape (extends the degraded[] severity enum and
    // adds optional `repairKind`/`sources`/`details` fields), so consumers
    // that branched on `schema` const get the same fields they always did.
    // Insights was the last v1 holdout — leaving it on v1 caused
    // `render_serve_sse_event` (src/serve.rs) to double-wrap insights
    // payloads under a fresh v2 envelope when streaming over SSE.
    serde_json::json!({
        "schema": RESPONSE_SCHEMA_V2,
        "success": true,
        "data": report,
    })
    .to_string()
}

pub fn render_insights_json_stream(report: &InsightsReport) -> String {
    let mut rendered = String::new();
    let _ = writeln!(
        rendered,
        "{}",
        serde_json::json!({
            "schema": INSIGHTS_JSON_STREAM_HEADER_SCHEMA_V1,
            "kind": "header",
            "reportSchema": report.schema,
            "command": report.command,
            "mode": report.mode.as_str(),
            "snapshotVersion": report.snapshot_version,
            "generatedAt": report.generated_at,
            "selectedSection": &report.selected_section,
            "explainMemoryId": &report.explain_memory_id,
            "explainCommand": &report.explain_command,
            "pagination": &report.pagination,
            "availableSections": &report.available_sections,
            "sectionCount": report.sections.len(),
        })
    );

    for (index, section) in report.sections.iter().enumerate() {
        let _ = writeln!(
            rendered,
            "{}",
            serde_json::json!({
                "schema": INSIGHTS_JSON_STREAM_SECTION_SCHEMA_V1,
                "kind": "section",
                "index": index,
                "name": section.name,
                "section": section,
            })
        );
    }

    let _ = writeln!(
        rendered,
        "{}",
        serde_json::json!({
            "schema": INSIGHTS_JSON_STREAM_FOOTER_SCHEMA_V1,
            "kind": "footer",
            "degraded": &report.degraded_signals,
            "runDurationMs": report.run_duration_ms,
        })
    );

    rendered
}

/// Render the insights report as TOON through the canonical JSON envelope.
#[must_use]
pub fn render_insights_toon(report: &InsightsReport) -> String {
    render_toon_from_json(&render_insights_json(report))
}

/// Render the insights report as Markdown.
#[must_use]
pub fn render_insights_markdown(report: &InsightsReport) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "# Insights");
    let _ = writeln!(output);
    let _ = writeln!(output, "- Schema: `{}`", report.schema);
    let _ = writeln!(output, "- Mode: `{}`", report.mode.as_str());
    let _ = writeln!(output, "- Snapshot version: {}", report.snapshot_version);
    let _ = writeln!(output, "- Generated at: `{}`", report.generated_at);
    let _ = writeln!(output, "- Run duration ms: {}", report.run_duration_ms);
    let _ = writeln!(
        output,
        "- Available sections: {}",
        report.available_sections.join(", ")
    );
    match report.selected_section.as_deref() {
        Some(section) => {
            let _ = writeln!(output, "- Selected section: `{section}`");
        }
        None => {
            let _ = writeln!(output, "- Selected section: none");
        }
    }
    if let Some(memory_id) = report.explain_memory_id.as_deref() {
        let _ = writeln!(output, "- Explain target: `{memory_id}`");
    }
    if let Some(command) = report.explain_command.as_deref() {
        let _ = writeln!(output, "- Explain command: `{command}`");
    }
    if let Some(pagination) = &report.pagination {
        let _ = writeln!(
            output,
            "- Pagination: limit={} offset={} returned={} total={}",
            pagination.limit, pagination.offset, pagination.returned, pagination.total
        );
    }

    let _ = writeln!(output);
    let _ = writeln!(output, "## Sections");
    for section in &report.sections {
        let _ = writeln!(output);
        let _ = writeln!(output, "### {}", section.title);
        let _ = writeln!(output, "- Name: `{}`", section.name);
        let _ = writeln!(output, "- Summary: {}", section.summary);
        let _ = writeln!(output, "- Why it matters: {}", section.why_it_matters);
        let _ = writeln!(output, "- Items: {}", section.items.len());
        if !section.items.is_empty() {
            for (index, item) in section.items.iter().enumerate() {
                let item_json = serde_json::to_string(item).unwrap_or_else(|_| "null".to_owned());
                let _ = writeln!(output, "- Item {}:", index + 1);
                let _ = writeln!(output, "  ```json");
                let _ = writeln!(output, "  {item_json}");
                let _ = writeln!(output, "  ```");
            }
        }
        if !section.next_commands.is_empty() {
            let _ = writeln!(output, "- Next commands:");
            for command in &section.next_commands {
                let _ = writeln!(output, "  - `{command}`");
            }
        }
    }

    if !report.degraded_signals.is_empty() {
        let _ = writeln!(output);
        let _ = writeln!(output, "## Degraded");
        for degraded in &report.degraded_signals {
            let _ = writeln!(
                output,
                "- **{}** `{}`: {}",
                degraded.severity, degraded.code, degraded.message
            );
            if let Some(repair) = degraded.repair.as_deref() {
                let _ = writeln!(output, "  - Repair: `{repair}`");
            }
            if !degraded.sources.is_empty() {
                let _ = writeln!(output, "  - Sources: {}", degraded.sources.join(", "));
            }
        }
    }

    output
}

#[must_use]
pub fn render_insights_human(report: &InsightsReport) -> String {
    let mut output = String::new();
    output.push_str("Insights\n");
    output.push_str(&format!("  Mode: {}\n", report.mode.as_str()));
    output.push_str(&format!(
        "  Available sections: {}\n",
        report.available_sections.join(", ")
    ));
    if let Some(memory_id) = report.explain_memory_id.as_deref() {
        output.push_str(&format!("  Explain target: {memory_id}\n"));
    }
    if let Some(command) = report.explain_command.as_deref() {
        output.push_str(&format!("  Explain command: {command}\n"));
    }
    output.push('\n');

    for section in &report.sections {
        output.push_str(section.title);
        output.push('\n');
        output.push_str(&format!("  Section: {}\n", section.name));
        output.push_str(&format!("  Summary: {}\n", section.summary));
        output.push_str(&format!("  Why: {}\n", section.why_it_matters));
        output.push_str("  Next:\n");
        for command in &section.next_commands {
            output.push_str(&format!("    {command}\n"));
        }
        output.push('\n');
    }

    output
}

fn authorities_section() -> InsightsSection {
    authorities_section_from_scores(&HitsScores::default())
}

fn authorities_section_from_scores(scores: &HitsScores) -> InsightsSection {
    hits_section_from_inputs(
        HitsSectionSpec {
            name: "authorities",
            title: "Authority Memories",
            summary: "HITS authority scores for memories that ground claims from many hubs.",
            why_it_matters: "Authority memories help agents distinguish grounded evidence from navigational references.",
            interpretation: "authority",
            score_field: "authorityScore",
            next_commands: vec!["ee insights --section authorities --workspace . --json"],
        },
        hits_inputs_from_scores(&scores.authorities),
    )
}

fn bridges_section() -> InsightsSection {
    bridges_section_from_inputs(&[])
}

fn blind_spots_section() -> InsightsSection {
    blind_spots_section_from_inputs(&[])
}

fn blind_spots_section_from_inputs(inputs: &[BlindSpotInput]) -> InsightsSection {
    let mut inputs = inputs.to_vec();
    sort_blind_spot_inputs(&mut inputs);
    let items = inputs
        .into_iter()
        .enumerate()
        .map(|(index, input)| {
            let blind_spot_id = blind_spot_id(&input.symbol_id);
            serde_json::json!({
                "rank": index + 1,
                "blindSpotId": blind_spot_id,
                "path": input.path,
                "symbolId": input.symbol_id,
                "canonicalName": input.canonical_name,
                "symbolKind": input.symbol_kind,
                "coverageStatus": "uncovered",
                "coverageRatio": input.coverage_ratio,
                "coveredNodeCount": input.covered_node_count,
                "totalNodeCount": input.total_node_count,
                "sourceRange": {
                    "startLine": input.start_line,
                    "endLine": input.end_line,
                },
                "explanation": "No current memory file provenance, lexical path mention, or lexical symbol mention covers this code-graph node.",
                "evidence": {
                    "schema": BLIND_SPOT_SCHEMA_V1,
                    "signal": "code_graph_memory_coverage_complement",
                    "algorithm": "symbol_snapshot_minus_memory_file_or_lexical_mentions",
                    "snapshotHash": input.snapshot_hash,
                    "anchorTableRequired": false,
                    "coverageSources": [
                        BlindSpotCoverageSource::FileProvenance.as_str(),
                        BlindSpotCoverageSource::LexicalPathMention.as_str(),
                        BlindSpotCoverageSource::LexicalSymbolMention.as_str(),
                    ],
                },
            })
        })
        .collect();

    InsightsSection {
        name: "blindSpots",
        title: "Blind Spots",
        summary: "Code-graph nodes with no matching memory file provenance, path mention, or symbol mention.",
        why_it_matters: "Blind spots give agents a calibrated caution signal before they rely on memory for code areas the store does not cover.",
        items,
        next_commands: vec!["ee insights --section blindSpots --workspace . --json"],
    }
}

fn sort_blind_spot_inputs(inputs: &mut [BlindSpotInput]) {
    inputs.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.start_line.cmp(&right.start_line))
            .then_with(|| left.canonical_name.cmp(&right.canonical_name))
            .then_with(|| left.symbol_id.cmp(&right.symbol_id))
    });
}

fn blind_spot_id(symbol_id: &str) -> String {
    let digest = blake3::hash(symbol_id.as_bytes()).to_hex();
    format!("bs_{}", &digest[..16])
}

fn bridges_section_from_inputs(inputs: &[BridgeInsightInput]) -> InsightsSection {
    let items = inputs
        .iter()
        .enumerate()
        .map(|(index, input)| {
            serde_json::json!({
                "rank": index + 1,
                "memoryId": &input.memory_id,
                "articulationPoint": true,
                "clusterDisconnectionMagnitude": input.cluster_disconnection_magnitude,
                "componentCountBefore": input.component_count_before,
                "componentCountAfter": input.component_count_after,
                "bridgeEdgeCount": input.bridge_edge_count,
                "degree": input.degree,
                "interpretation": "articulation_point",
                "evidence": {
                    "schema": BRIDGE_INSIGHT_SCHEMA_V1,
                    "algorithm": "tarjan_articulation_points",
                    "bridgeAlgorithm": "tarjan_bridges",
                    "snapshotVersion": input.snapshot_version,
                },
            })
        })
        .collect();

    InsightsSection {
        name: "bridges",
        title: "Bridge Memories",
        summary: "Top articulation-point memories ranked by cluster-disconnection magnitude.",
        why_it_matters: "Bridge memories deserve careful decay and review because removing them can disconnect useful context.",
        items,
        next_commands: vec!["ee insights --section bridges --workspace . --json"],
    }
}

fn causal_bottlenecks_section() -> InsightsSection {
    causal_bottlenecks_section_from_reports(&[])
}

fn causal_bottlenecks_section_from_reports(reports: &[CausalBottleneckInput]) -> InsightsSection {
    let mut reports = reports
        .iter()
        .filter(|report| report.betweenness.is_finite() && report.betweenness > 0.0)
        .collect::<Vec<_>>();
    // `total_cmp` gives a total order on f64 even if NaN sneaks past
    // the upstream `is_finite()` filter at line 1667. Without an
    // in-comparator total order, `sort_by`'s output order is documented
    // as "unspecified" when any pair returns `Greater`/`Equal`/`Less`
    // inconsistently (i.e. when NaN collapses onto `Equal` via
    // `partial_cmp(...).unwrap_or(Equal)`). This sort feeds the
    // `causal_bottlenecks` insights section (`ee.insights.section.v1`),
    // which is a determinism-contract surface — same input snapshot →
    // byte-identical `rank` field. The `memory_id` tiebreaker below
    // would still stabilize a NaN-collapse, so the two orderings are
    // observationally equivalent today; this is defense-in-depth for
    // future callers that bypass the upstream filter. Mirrors
    // `top_positive_influencers` in `src/core/influence.rs` and
    // `load_bearing_memory_items` in `src/graph/bipartite_provenance.rs`.
    reports.sort_by(|left, right| {
        right
            .betweenness
            .total_cmp(&left.betweenness)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });

    let items = reports
        .into_iter()
        .enumerate()
        .map(|(index, report)| {
            serde_json::json!({
                "rank": index + 1,
                "memoryId": &report.memory_id,
                "betweenness": report.betweenness,
                "interpretation": "causal_bridge",
                "evidence": {
                    "schema": CAUSAL_BOTTLENECK_REPORT_SCHEMA_V1,
                    "algorithm": "betweenness_centrality_directed",
                    "snapshotVersion": report.snapshot_version,
                },
            })
        })
        .collect();

    InsightsSection {
        name: "causalBottlenecks",
        title: "Causal Bottlenecks",
        summary: "High-betweenness memories in causal-evidence subgraphs.",
        why_it_matters: "Causal bottlenecks show which facts carry the most explanatory load for failures and repairs.",
        items,
        next_commands: vec!["ee insights --section causalBottlenecks --workspace . --json"],
    }
}

fn comprehensive_rules_section() -> InsightsSection {
    placeholder_section(
        "comprehensiveRules",
        "Comprehensive Rules",
        "Rule memories with broad provenance coverage and high reuse potential.",
        "Comprehensive rules are candidates for promotion because they generalize across repeated work.",
        vec!["ee curate candidates --workspace . --json"],
    )
}

fn contradiction_clusters_section() -> InsightsSection {
    contradiction_clusters_section_from_clusters(&[])
}

fn contradiction_clusters_section_from_clusters(
    clusters: &[crate::graph::health::ContradictionCluster],
) -> InsightsSection {
    let items = clusters
        .iter()
        .filter_map(|cluster| serde_json::to_value(cluster).ok())
        .collect();

    InsightsSection {
        name: "contradictionClusters",
        title: "Contradiction Clusters",
        summary: "Louvain communities filtered to contradiction-heavy memory neighborhoods.",
        why_it_matters: "Contradiction clusters identify parts of the memory graph that need curation before agents rely on them.",
        items,
        next_commands: vec!["ee insights --section contradictionClusters --workspace . --json"],
    }
}

fn hubs_section() -> InsightsSection {
    hubs_section_from_scores(&HitsScores::default())
}

fn hubs_section_from_scores(scores: &HitsScores) -> InsightsSection {
    hits_section_from_inputs(
        HitsSectionSpec {
            name: "hubs",
            title: "Hub Memories",
            summary: "HITS hub scores for memories that point to many authoritative facts.",
            why_it_matters: "Hub memories are useful navigation anchors for agents assembling a task-specific context pack.",
            interpretation: "hub",
            score_field: "hubScore",
            next_commands: vec!["ee insights --section hubs --workspace . --json"],
        },
        hits_inputs_from_scores(&scores.hubs),
    )
}

fn hits_section_from_inputs(
    spec: HitsSectionSpec,
    mut inputs: Vec<HitsInsightInput>,
) -> InsightsSection {
    inputs.retain(|input| input.score.is_finite() && input.score > 0.0);
    // See `causal_bottlenecks_section_from_reports` for the `total_cmp`
    // rationale: defense-in-depth for the insights determinism contract
    // even though the upstream `retain(is_finite)` excludes NaN today.
    inputs.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.memory_id.cmp(&right.memory_id))
    });

    let items = inputs
        .into_iter()
        .enumerate()
        .map(|(index, input)| {
            let mut item = serde_json::json!({
                "rank": index + 1,
                "memoryId": input.memory_id,
                "interpretation": spec.interpretation,
                "evidence": {
                    "schema": HITS_REPORT_SCHEMA_V1,
                    "algorithm": "hits_centrality_directed",
                    "snapshotVersion": input.snapshot_version,
                },
            });
            if let JsonValue::Object(object) = &mut item {
                object.insert(spec.score_field.to_owned(), serde_json::json!(input.score));
            }
            item
        })
        .collect();

    InsightsSection {
        name: spec.name,
        title: spec.title,
        summary: spec.summary,
        why_it_matters: spec.why_it_matters,
        items,
        next_commands: spec.next_commands,
    }
}

fn k_core_section() -> InsightsSection {
    placeholder_section(
        "kCore",
        "K-Core",
        "Core-number membership for densely connected memory regions.",
        "K-core posture shows which memories sit in stable, mutually reinforcing graph neighborhoods.",
        vec!["ee insights --section kCore --workspace . --json"],
    )
}

fn k_truss_section() -> InsightsSection {
    placeholder_section(
        "kTruss",
        "K-Truss",
        "Triangle-supported structural health findings for support subgraphs.",
        "K-truss posture helps separate isolated support edges from stronger corroborating clusters.",
        vec!["ee insights --section kTruss --workspace . --json"],
    )
}

fn knowledge_gaps_section() -> InsightsSection {
    knowledge_gaps_section_from_inputs(&[])
}

fn knowledge_gaps_section_from_inputs(inputs: &[KnowledgeGapInput]) -> InsightsSection {
    let mut inputs = inputs.to_vec();
    sort_and_dedup_knowledge_gaps(&mut inputs);
    let items = inputs
        .into_iter()
        .enumerate()
        .map(|(index, input)| {
            let source_memory_ids = input.source_memory_ids;
            let gap_id = knowledge_gap_id(input.category, &source_memory_ids);
            let recommendation = serde_json::json!({
                "kind": "reflect_propose",
                "command": knowledge_gap_reflect_command(&source_memory_ids),
                "sourceMemoryIds": source_memory_ids.clone(),
            });
            serde_json::json!({
                "rank": index + 1,
                "gapId": gap_id,
                "category": input.category,
                "priority": input.priority,
                "confidence": input.confidence,
                "sourceMemoryIds": source_memory_ids,
                "metricEvidence": input.metric_evidence,
                "explanation": input.explanation,
                "recommendation": recommendation,
            })
        })
        .collect();

    InsightsSection {
        name: "knowledgeGaps",
        title: "Knowledge Gaps",
        summary: "Graph-derived gaps that need reflection or curation before agents rely on nearby memories.",
        why_it_matters: "Knowledge gaps turn weak graph evidence into deterministic reflection requests instead of silently trusting incomplete memories.",
        items,
        next_commands: vec!["ee insights --section knowledgeGaps --workspace . --json"],
    }
}

fn knowledge_gap_id(category: &str, source_memory_ids: &[String]) -> String {
    let mut seed = category.to_owned();
    for id in source_memory_ids {
        seed.push(':');
        seed.push_str(id);
    }
    let digest = blake3::hash(seed.as_bytes()).to_hex();
    format!("kg_{}", &digest[..16])
}

fn knowledge_gap_reflect_command(source_memory_ids: &[String]) -> String {
    let mut command = "ee --workspace . --json reflect propose --kind gaps --dry-run".to_owned();
    for memory_id in source_memory_ids {
        command.push_str(" --source-memory ");
        command.push_str(memory_id);
    }
    command
}

fn knowledge_gap_recommendations_from_items(items: &[JsonValue]) -> Vec<JsonValue> {
    items
        .iter()
        .filter_map(|item| {
            let id = item.get("gapId")?.as_str()?;
            let reason = item.get("explanation")?.as_str()?;
            let priority = item.get("priority").and_then(JsonValue::as_u64)?;
            let confidence = item
                .get("confidence")
                .and_then(JsonValue::as_f64)
                .unwrap_or(0.0);
            let suggested_query = item
                .get("recommendation")
                .and_then(|recommendation| recommendation.get("command"))
                .and_then(JsonValue::as_str)?;
            let recommendation_kind = item
                .get("recommendation")
                .and_then(|recommendation| recommendation.get("kind"))
                .and_then(JsonValue::as_str)?;
            Some(serde_json::json!({
                "id": id,
                "severity": knowledge_gap_recommendation_severity(priority, confidence),
                "reason": reason,
                "suggested_query": suggested_query,
                "recommendation_kind": recommendation_kind,
            }))
        })
        .collect()
}

fn knowledge_gap_recommendation_severity(priority: u64, confidence: f64) -> &'static str {
    if priority >= 90 && confidence >= 0.80 {
        "high"
    } else if priority >= 80 {
        "medium"
    } else if priority >= 70 {
        "warning"
    } else {
        "low"
    }
}

fn knowledge_skyline_section() -> InsightsSection {
    knowledge_skyline_section_from_report(None)
}

fn knowledge_skyline_section_from_report(report: Option<&KnowledgeSkyline>) -> InsightsSection {
    let items = report
        .filter(|skyline| skyline.node_count > 0)
        .map(|skyline| {
            serde_json::json!({
                "rank": 1,
                "interpretation": "portfolio_posture",
                "evidence": {
                    "schema": KNOWLEDGE_SKYLINE_SCHEMA_V1,
                    "algorithm": "onion_layers_louvain_k_truss",
                    "snapshotVersion": 0,
                },
                "skyline": skyline,
            })
        })
        .into_iter()
        .collect();

    InsightsSection {
        name: "knowledgeSkyline",
        title: "Knowledge Skyline",
        summary: "Composite posture across onion layer, community, age, trust, and graph health signals.",
        why_it_matters: "The skyline gives agents a portfolio-level view of memory quality before relying on a workspace.",
        items,
        next_commands: vec!["ee insights --section knowledgeSkyline --workspace . --json"],
    }
}

fn load_bearing_memories_section() -> InsightsSection {
    load_bearing_memories_section_from_items(&[])
}

fn load_bearing_memories_section_from_items(
    items: &[crate::graph::bipartite_provenance::LoadBearingMemoryItem],
) -> InsightsSection {
    let items = items
        .iter()
        .filter_map(|item| serde_json::to_value(item).ok())
        .collect();

    InsightsSection {
        name: "loadBearingMemories",
        title: "Load-Bearing Memories",
        summary: "Memories with high influence in rule-to-source provenance projections.",
        why_it_matters: "Load-bearing memories should be preserved or reviewed carefully because many rules depend on them.",
        items,
        next_commands: vec!["ee insights --section loadBearingMemories --workspace . --json"],
    }
}

fn proximity_hotspots_section() -> InsightsSection {
    proximity_hotspots_section_from_reports(&[])
}

fn proximity_hotspots_section_from_reports(reports: &[ProximityHotspotInput]) -> InsightsSection {
    let mut reports = reports
        .iter()
        .filter(|report| report.min_cut.is_some_and(f64::is_finite))
        .collect::<Vec<_>>();
    // `total_cmp` over `partial_cmp(...).unwrap_or(Equal)`: even though
    // the `is_some_and(f64::is_finite)` filter above guarantees both
    // values are `Some(finite)` (so `partial_cmp` always returns
    // `Some(Ordering)` and the `unwrap_or(Equal)` is unreachable today),
    // the determinism contract documented at AGENTS.md ("same DB +
    // indexes + config + query → byte-identical JSON output") extends
    // to every f64 sort along a render path. If a future caller path
    // bypasses the upstream filter — or a refactor moves it elsewhere —
    // a NaN that reaches this sort under `partial_cmp(...).unwrap_or(Equal)`
    // would collapse the ordering into intransitivity and silently
    // scramble the `rank` field on the `ee.insights.section.v1`
    // proximityHotspots surface. Defense-in-depth pattern shipped in
    // 4a067ecb (causalBottlenecks + hits sorts in this same file) and
    // 18f20375 (influence.rs `top_positive_influencers`); same fix
    // class as the recent `models/economy.rs::AggregateUtility` peer
    // change. After the filter, `unwrap_or(NEG_INFINITY)` is dead-code
    // for the None branch (and matches `Option::partial_cmp`'s
    // "None < Some" semantic if a None ever slipped through).
    reports.sort_by(|left, right| {
        let left_value = left.min_cut.unwrap_or(f64::NEG_INFINITY);
        let right_value = right.min_cut.unwrap_or(f64::NEG_INFINITY);
        left_value
            .total_cmp(&right_value)
            .then_with(|| left.memory_a.cmp(&right.memory_a))
            .then_with(|| left.memory_b.cmp(&right.memory_b))
    });

    let items = reports
        .into_iter()
        .enumerate()
        .map(|(index, report)| {
            serde_json::json!({
                "rank": index + 1,
                "memoryA": &report.memory_a,
                "memoryB": &report.memory_b,
                "minCut": report.min_cut,
                "interpretation": &report.interpretation,
                "treePath": &report.tree_path,
                "evidence": {
                    "schema": PROXIMITY_REPORT_SCHEMA_V1,
                    "algorithm": "gomory_hu_tree",
                    "snapshotVersion": report.snapshot_version,
                },
            })
        })
        .collect();

    InsightsSection {
        name: "proximityHotspots",
        title: "Proximity Hotspots",
        summary: "Memory pairs with small min-cut distance in Gomory-Hu proximity projections.",
        why_it_matters: "Proximity hotspots surface tightly coupled facts that should be packed, reviewed, or curated together.",
        items,
        next_commands: vec!["ee insights --section proximityHotspots --workspace . --json"],
    }
}

fn revision_frontiers_section() -> InsightsSection {
    placeholder_section(
        "revisionFrontiers",
        "Revision Frontiers",
        "Top recent revisions ranked by dominance-frontier size in logical memory revision DAGs.",
        "Revision frontiers help agents understand which edits may change downstream context behavior.",
        vec!["ee insights --section revisionFrontiers --workspace . --json"],
    )
}

fn top_memories_section() -> InsightsSection {
    top_memories_section_from_inputs(&[])
}

fn top_memories_section_from_inputs(inputs: &[TopMemoryInsightInput]) -> InsightsSection {
    let items = inputs
        .iter()
        .enumerate()
        .map(|(index, input)| {
            serde_json::json!({
                "rank": index + 1,
                "memoryId": &input.memory_id,
                "level": &input.level,
                "kind": &input.kind,
                "trustClass": &input.trust_class,
                "confidence": input.confidence,
                "utility": input.utility,
                "importance": input.importance,
                "pagerank": input.pagerank,
                "retrievalPosture": input.retrieval_posture,
                "linkDegree": input.link_degree,
                "incomingLinkCount": input.incoming_link_count,
                "outgoingLinkCount": input.outgoing_link_count,
                "createdAt": &input.created_at,
                "interpretation": "top_memory",
                "evidence": {
                    "schema": TOP_MEMORY_INSIGHT_SCHEMA_V1,
                    "algorithm": "pagerank_with_retrieval_posture_tiebreak",
                    "snapshotVersion": input.snapshot_version,
                },
            })
        })
        .collect();

    InsightsSection {
        name: "topMemories",
        title: "Top Memories",
        summary: "Top-ranked memories by cached graph centrality and retrieval posture.",
        why_it_matters: "Top memories provide an immediate overview of the facts most likely to shape agent behavior.",
        items,
        next_commands: vec!["ee insights --section topMemories --workspace . --json"],
    }
}

fn placeholder_section(
    name: &'static str,
    title: &'static str,
    summary: &'static str,
    why_it_matters: &'static str,
    next_commands: Vec<&'static str>,
) -> InsightsSection {
    InsightsSection {
        name,
        title,
        summary,
        why_it_matters,
        items: Vec::new(),
        next_commands,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{
        CreateGraphSnapshotInput, CreateMemoryInput, CreateMemoryLinkInput,
        CreateProceduralRuleInput, CreateWorkspaceInput, GraphSnapshotStatus, GraphSnapshotType,
        MemoryLinkRelation, MemoryLinkSource,
    };
    use chrono::TimeZone;
    use clap::Parser as _;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    type TestResult = Result<(), String>;

    fn section_names(report: &InsightsReport) -> Vec<&'static str> {
        report.sections.iter().map(|section| section.name).collect()
    }

    fn unique_insights_workspace(prefix: &str) -> Result<std::path::PathBuf, String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("clock moved backwards: {error}"))?
            .as_nanos();
        let workspace = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("ee-insights-feature-flags")
            .join(format!("{prefix}-{}-{now}", std::process::id()));
        fs::create_dir_all(workspace.join(".ee")).map_err(|error| error.to_string())?;
        Ok(workspace)
    }

    fn write_graph_feature_config(
        workspace: &std::path::Path,
        enabled: bool,
    ) -> Result<(), String> {
        let value = if enabled { "true" } else { "false" };
        fs::write(
            workspace.join(".ee").join("config.toml"),
            format!(
                "[graph.feature.causal_explain]\nenabled = {value}\n\
                 [graph.feature.revision_dominance]\nenabled = {value}\n\
                 [graph.feature.skyline]\nenabled = {value}\n\
                 [graph.feature.load_bearing]\nenabled = {value}\n\
                 [graph.feature.hits_profiles]\nenabled = {value}\n"
            ),
        )
        .map_err(|error| error.to_string())
    }

    fn seed_load_bearing_workspace(workspace: &std::path::Path) -> Result<String, String> {
        let database_path = workspace.join(".ee").join("ee.db");
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = crate::core::curate::stable_workspace_id(workspace);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.to_string_lossy().into_owned(),
                    name: Some("load-bearing insights".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        for (id, content) in [
            (
                "mem_loadbearinganchor000000001",
                "Load-bearing source memory for release rules.",
            ),
            (
                "mem_loadbearingsolo00000000001",
                "Solo source memory for one release rule.",
            ),
        ] {
            connection
                .insert_memory(
                    id,
                    &CreateMemoryInput {
                        workspace_id: workspace_id.clone(),
                        level: "semantic".to_owned(),
                        kind: "fact".to_owned(),
                        content: content.to_owned(),
                        workflow_id: None,
                        confidence: 0.9,
                        utility: 0.8,
                        importance: 0.7,
                        provenance_uri: None,
                        trust_class: "human_explicit".to_owned(),
                        trust_subclass: None,
                        tags: Vec::new(),
                        valid_from: Some("2026-05-20T00:00:00Z".to_owned()),
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;
        }

        for (rule_id, sources) in [
            (
                "rule_loadbearingalpha0000000000",
                vec!["mem_loadbearinganchor000000001".to_owned()],
            ),
            (
                "rule_loadbearingbeta00000000000",
                vec![
                    "mem_loadbearinganchor000000001".to_owned(),
                    "mem_loadbearingsolo00000000001".to_owned(),
                ],
            ),
        ] {
            connection
                .insert_procedural_rule(
                    rule_id,
                    &CreateProceduralRuleInput {
                        workspace_id: workspace_id.clone(),
                        content: format!("Rule {rule_id} cites source memories."),
                        confidence: 0.9,
                        utility: 0.8,
                        importance: 0.7,
                        trust_class: "human_explicit".to_owned(),
                        scope: "workspace".to_owned(),
                        scope_pattern: None,
                        maturity: "validated".to_owned(),
                        protected: false,
                        source_memory_ids: sources,
                        tags: Vec::new(),
                    },
                )
                .map_err(|error| error.to_string())?;
        }

        connection.close().map_err(|error| error.to_string())?;
        Ok("mem_loadbearinganchor000000001".to_owned())
    }

    fn seed_insights_graph_workspace(workspace: &std::path::Path) -> Result<(), String> {
        let database_path = workspace.join(".ee").join("ee.db");
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        let workspace_id = crate::core::curate::stable_workspace_id(workspace);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.to_string_lossy().into_owned(),
                    name: Some("real insights graph".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_graph_snapshot(
                "gsnap_insightsstale000000000000",
                &CreateGraphSnapshotInput {
                    workspace_id: workspace_id.clone(),
                    snapshot_version: 7,
                    schema_version: "ee.graph.snapshot.v1".to_owned(),
                    graph_type: GraphSnapshotType::MemoryLinks,
                    node_count: 0,
                    edge_count: 0,
                    metrics_json: r#"{"nodes":[],"edges":[]}"#.to_owned(),
                    content_hash: "blake3:stale-insights-snapshot".to_owned(),
                    source_generation: 0,
                    expires_at: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .update_graph_snapshot_status(
                "gsnap_insightsstale000000000000",
                GraphSnapshotStatus::Stale,
            )
            .map_err(|error| error.to_string())?;

        for (id, trust_class, content) in [
            (
                "mem_insightsbridgea00000000001",
                "human_explicit",
                "Bridge endpoint A.",
            ),
            (
                "mem_insightsbridgeb00000000001",
                "agent_validated",
                "Bridge articulation B.",
            ),
            (
                "mem_insightsbridgec00000000001",
                "agent_validated",
                "Bridge articulation C.",
            ),
            (
                "mem_insightsbridged00000000001",
                "human_explicit",
                "Bridge endpoint D.",
            ),
            (
                "mem_insightscontraa00000000001",
                "human_explicit",
                "Contradiction exemplar A.",
            ),
            (
                "mem_insightscontrab00000000001",
                "agent_validated",
                "Contradiction exemplar B.",
            ),
            (
                "mem_insightscontrac00000000001",
                "agent_validated",
                "Contradiction exemplar C.",
            ),
        ] {
            connection
                .insert_memory(
                    id,
                    &CreateMemoryInput {
                        workspace_id: workspace_id.clone(),
                        level: "semantic".to_owned(),
                        kind: "fact".to_owned(),
                        content: content.to_owned(),
                        workflow_id: None,
                        confidence: 0.9,
                        utility: 0.8,
                        importance: 0.7,
                        provenance_uri: None,
                        trust_class: trust_class.to_owned(),
                        trust_subclass: None,
                        tags: Vec::new(),
                        valid_from: Some("2026-05-20T00:00:00Z".to_owned()),
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;
        }

        for (id, source, target, relation) in [
            (
                "link_insightsbridge000000000001",
                "mem_insightsbridgea00000000001",
                "mem_insightsbridgeb00000000001",
                MemoryLinkRelation::Supports,
            ),
            (
                "link_insightsbridge000000000002",
                "mem_insightsbridgeb00000000001",
                "mem_insightsbridgec00000000001",
                MemoryLinkRelation::Supports,
            ),
            (
                "link_insightsbridge000000000003",
                "mem_insightsbridgec00000000001",
                "mem_insightsbridged00000000001",
                MemoryLinkRelation::Supports,
            ),
            (
                "link_insightscontra000000000001",
                "mem_insightscontraa00000000001",
                "mem_insightscontrab00000000001",
                MemoryLinkRelation::Contradicts,
            ),
            (
                "link_insightscontra000000000002",
                "mem_insightscontrab00000000001",
                "mem_insightscontrac00000000001",
                MemoryLinkRelation::Supersedes,
            ),
            (
                "link_insightscontra000000000003",
                "mem_insightscontraa00000000001",
                "mem_insightscontrac00000000001",
                MemoryLinkRelation::Contradicts,
            ),
        ] {
            connection
                .insert_memory_link(
                    id,
                    &CreateMemoryLinkInput {
                        src_memory_id: source.to_owned(),
                        dst_memory_id: target.to_owned(),
                        relation,
                        weight: 1.0,
                        confidence: 1.0,
                        directed: false,
                        evidence_count: 1,
                        last_reinforced_at: None,
                        source: MemoryLinkSource::Agent,
                        created_by: Some("insights-db-test".to_owned()),
                        metadata_json: None,
                    },
                )
                .map_err(|error| error.to_string())?;
        }

        connection.close().map_err(|error| error.to_string())
    }

    #[test]
    fn full_bundle_uses_deterministic_section_order() -> TestResult {
        let report = build_insights_report(&InsightsArgs {
            section: None,
            explain: None,
            limit: DEFAULT_SECTION_LIMIT,
            offset: 0,
            json_stream: false,
        })
        .map_err(|error| error.to_string())?;

        assert_eq!(report.schema, INSIGHTS_SCHEMA_V1);
        assert_eq!(report.mode, InsightsMode::FullBundle);
        assert_eq!(report.snapshot_version, 0);
        assert_eq!(report.generated_at, EMPTY_WORKSPACE_GENERATED_AT);
        assert_eq!(report.run_duration_ms, 0);
        assert_eq!(
            report.available_sections,
            vec![
                "authorities",
                "blindSpots",
                "bridges",
                "causalBottlenecks",
                "comprehensiveRules",
                "contradictionClusters",
                "hubs",
                "kCore",
                "kTruss",
                "knowledgeGaps",
                "knowledgeSkyline",
                "loadBearingMemories",
                "proximityHotspots",
                "revisionFrontiers",
                "topMemories"
            ]
        );
        assert_eq!(section_names(&report), report.available_sections);
        assert_eq!(report.selected_section, None);
        assert_eq!(report.explain_memory_id, None);
        assert_eq!(report.explain_command, None);
        assert_eq!(report.degraded_signals.len(), 2);
        assert_eq!(report.degraded_signals[0].code, "graph.workspace_empty");
        assert_eq!(report.degraded_signals[0].severity, "info");
        assert_eq!(
            report.degraded_signals[0].sources,
            vec!["insights".to_owned()]
        );
        assert_eq!(
            report.degraded_signals[1].code,
            INSIGHTS_SECTION_UNAVAILABLE_CODE
        );
        let mut expected_sources = PLACEHOLDER_BACKED_SECTIONS
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>();
        expected_sources.sort();
        let mut actual_sources = report.degraded_signals[1].sources.clone();
        actual_sources.sort();
        assert_eq!(actual_sources, expected_sources);
        for section in &report.sections {
            assert!(section.items.is_empty());
        }

        Ok(())
    }

    #[test]
    fn explain_mode_preserves_memory_target_and_full_context() -> TestResult {
        let report = build_insights_report(&InsightsArgs {
            section: None,
            explain: Some("mem_123".to_owned()),
            limit: DEFAULT_SECTION_LIMIT,
            offset: 0,
            json_stream: false,
        })
        .map_err(|error| error.to_string())?;

        assert_eq!(report.mode, InsightsMode::Explain);
        assert_eq!(report.explain_memory_id.as_deref(), Some("mem_123"));
        assert_eq!(
            report.explain_command.as_deref(),
            Some("ee why mem_123 --json")
        );
        assert_eq!(section_names(&report), report.available_sections);

        Ok(())
    }

    #[test]
    fn rendered_json_wraps_schema_aligned_data() -> TestResult {
        let report = build_insights_report(&InsightsArgs {
            section: Some("topMemories".to_owned()),
            explain: None,
            limit: DEFAULT_SECTION_LIMIT,
            offset: 0,
            json_stream: false,
        })
        .map_err(|error| error.to_string())?;
        let json: serde_json::Value = serde_json::from_str(&render_insights_json(&report))
            .map_err(|error| {
                format!("rendered insights JSON should parse as response envelope: {error}")
            })?;
        let data = &json["data"];

        assert_eq!(json["schema"], RESPONSE_SCHEMA_V2);
        assert_eq!(json["success"], true);
        assert_eq!(data["schema"], INSIGHTS_SCHEMA_V1);
        assert_eq!(data["command"], "insights");
        assert_eq!(data["mode"], "section");
        assert_eq!(data["snapshotVersion"], 0);
        assert_eq!(data["generatedAt"], EMPTY_WORKSPACE_GENERATED_AT);
        assert_eq!(data["runDurationMs"], 0);
        assert_eq!(data["selectedSection"], "topMemories");
        assert_eq!(data["pagination"]["limit"], DEFAULT_SECTION_LIMIT);
        assert_eq!(data["pagination"]["offset"], 0);
        assert_eq!(data["pagination"]["returned"], 0);
        assert_eq!(data["pagination"]["total"], 0);
        assert_eq!(data["degradedSignals"][0]["code"], "graph.workspace_empty");

        Ok(())
    }

    #[test]
    fn rendered_json_stream_emits_parseable_header_sections_footer() -> TestResult {
        let report = build_insights_report(&InsightsArgs {
            section: Some("topMemories".to_owned()),
            explain: None,
            limit: DEFAULT_SECTION_LIMIT,
            offset: 0,
            json_stream: false,
        })
        .map_err(|error| error.to_string())?;
        let rendered = render_insights_json_stream(&report);
        let lines = rendered.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), report.sections.len() + 2);

        let values = lines
            .iter()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .map_err(|error| format!("stream line should parse as JSON: {error}: {line}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(values[0]["schema"], INSIGHTS_JSON_STREAM_HEADER_SCHEMA_V1);
        assert_eq!(values[0]["kind"], "header");
        assert_eq!(values[0]["reportSchema"], INSIGHTS_SCHEMA_V1);
        assert_eq!(values[0]["snapshotVersion"], 0);
        assert_eq!(values[0]["generatedAt"], EMPTY_WORKSPACE_GENERATED_AT);
        assert_eq!(values[0]["sectionCount"], 1);

        assert_eq!(values[1]["schema"], INSIGHTS_JSON_STREAM_SECTION_SCHEMA_V1);
        assert_eq!(values[1]["kind"], "section");
        assert_eq!(values[1]["index"], 0);
        assert_eq!(values[1]["name"], "topMemories");
        assert_eq!(values[1]["section"]["name"], "topMemories");

        let footer = values
            .last()
            .ok_or_else(|| "stream footer should be present".to_owned())?;
        assert_eq!(footer["schema"], INSIGHTS_JSON_STREAM_FOOTER_SCHEMA_V1);
        assert_eq!(footer["kind"], "footer");
        assert_eq!(footer["degraded"][0]["code"], "graph.workspace_empty");
        assert_eq!(footer["runDurationMs"], 0);

        Ok(())
    }

    #[test]
    fn rendered_markdown_preserves_sections_items_and_degraded() -> TestResult {
        let mut report = build_insights_report(&InsightsArgs {
            section: Some("topMemories".to_owned()),
            explain: None,
            limit: DEFAULT_SECTION_LIMIT,
            offset: 0,
            json_stream: false,
        })
        .map_err(|error| error.to_string())?;
        report.sections = vec![InsightsSection {
            name: "topMemories",
            title: "Top Memories",
            summary: "Top-ranked memories by cached graph centrality and retrieval posture.",
            why_it_matters: "Top memories provide an immediate overview of the facts most likely to shape agent behavior.",
            items: vec![serde_json::json!({"memoryId": "mem_top"})],
            next_commands: vec!["ee insights --section topMemories --workspace . --json"],
        }];
        report.degraded_signals = vec![InsightsDegradedSignal {
            code: "graph.insights_fixture".to_owned(),
            severity: "warning".to_owned(),
            message: "fixture insights degradation".to_owned(),
            repair: Some("ee graph snapshot refresh --workspace . --json".to_owned()),
            sources: vec!["insights".to_owned(), "topMemories".to_owned()],
        }];

        let markdown = render_insights_markdown(&report);

        assert!(markdown.contains("# Insights"));
        assert!(markdown.contains("- Schema: `ee.insights.v1`"));
        assert!(markdown.contains("- Mode: `section`"));
        assert!(markdown.contains("- Selected section: `topMemories`"));
        assert!(markdown.contains("- Pagination: limit=10 offset=0 returned=0 total=0"));
        assert!(markdown.contains("### Top Memories"));
        assert!(markdown.contains(r#""memoryId":"mem_top""#));
        assert!(markdown.contains("`ee insights --section topMemories --workspace . --json`"));
        assert!(
            markdown
                .contains("- **warning** `graph.insights_fixture`: fixture insights degradation")
        );
        assert!(markdown.contains("  - Sources: insights, topMemories"));

        Ok(())
    }

    #[test]
    fn rendered_toon_decodes_to_canonical_json() -> TestResult {
        let report = build_insights_report(&InsightsArgs {
            section: Some("topMemories".to_owned()),
            explain: None,
            limit: DEFAULT_SECTION_LIMIT,
            offset: 0,
            json_stream: false,
        })
        .map_err(|error| error.to_string())?;
        let json = render_insights_json(&report);
        let toon = render_insights_toon(&report);

        let expected_json = serde_json::from_str::<serde_json::Value>(&json)
            .map_err(|error| format!("insights JSON should parse: {error}"))?;
        let expected = serde_json::Value::from(toon::JsonValue::from(expected_json));
        let decoded = toon::try_decode(&toon, None)
            .map_err(|error| format!("insights TOON should decode: {error}"))?;
        let actual = serde_json::Value::from(decoded);

        assert_eq!(actual, expected);
        Ok(())
    }

    #[test]
    fn section_lookup_accepts_camel_kebab_and_snake_case() -> TestResult {
        for section in [
            "causalBottlenecks",
            "causal-bottlenecks",
            "causal_bottlenecks",
        ] {
            let report = build_insights_report(&InsightsArgs {
                section: Some(section.to_owned()),
                explain: None,
                limit: DEFAULT_SECTION_LIMIT,
                offset: 0,
                json_stream: false,
            })
            .map_err(|error| error.to_string())?;

            assert_eq!(
                report.selected_section.as_deref(),
                Some("causalBottlenecks")
            );
            assert_eq!(section_names(&report), vec!["causalBottlenecks"]);
        }

        Ok(())
    }

    #[test]
    fn selected_graph_feature_sections_emit_disabled_signal() -> TestResult {
        let cases = [
            (
                "causalBottlenecks",
                "Causal bottleneck insights are disabled by graph.feature.causal_explain.enabled.",
                "ee config set graph.feature.causal_explain.enabled true",
            ),
            (
                "revisionFrontiers",
                "Revision frontier insights are disabled by graph.feature.revision_dominance.enabled.",
                "ee config set graph.feature.revision_dominance.enabled true",
            ),
            (
                "knowledgeSkyline",
                "Knowledge skyline insights are disabled by graph.feature.skyline.enabled.",
                "ee config set graph.feature.skyline.enabled true",
            ),
            (
                "loadBearingMemories",
                "Load-bearing memory insights are disabled by graph.feature.load_bearing.enabled.",
                "ee config set graph.feature.load_bearing.enabled true",
            ),
            (
                "hubs",
                "HITS profile insights are disabled by graph.feature.hits_profiles.enabled.",
                "ee config set graph.feature.hits_profiles.enabled true",
            ),
            (
                "authorities",
                "HITS profile insights are disabled by graph.feature.hits_profiles.enabled.",
                "ee config set graph.feature.hits_profiles.enabled true",
            ),
        ];

        for (section, message, repair) in cases {
            let workspace = unique_insights_workspace(section)?;
            write_graph_feature_config(&workspace, false)?;
            let report = build_insights_report_with_options(
                &InsightsArgs {
                    section: Some(section.to_owned()),
                    explain: None,
                    limit: DEFAULT_SECTION_LIMIT,
                    offset: 0,
                    json_stream: false,
                },
                InsightsBuildOptions {
                    workspace: Some(&workspace),
                },
            )
            .map_err(|error| error.to_string())?;

            assert_eq!(report.mode, InsightsMode::Section);
            assert_eq!(report.selected_section.as_deref(), Some(section));
            assert_eq!(section_names(&report), vec![section]);
            assert!(report.sections[0].items.is_empty());
            assert_eq!(report.degraded_signals.len(), 1);
            assert_eq!(report.degraded_signals[0].code, "graph_feature_disabled");
            assert_eq!(report.degraded_signals[0].severity, "medium");
            assert_eq!(report.degraded_signals[0].message, message);
            assert_eq!(report.degraded_signals[0].repair.as_deref(), Some(repair));
            assert_eq!(report.degraded_signals[0].sources, vec![section.to_owned()]);
        }

        Ok(())
    }

    #[test]
    fn load_bearing_section_reads_rule_provenance_projection() -> TestResult {
        let workspace = unique_insights_workspace("load-bearing")?;
        write_graph_feature_config(&workspace, true)?;
        let load_bearing_memory_id = seed_load_bearing_workspace(&workspace)?;

        let report = build_insights_report_with_options(
            &InsightsArgs {
                section: Some("loadBearingMemories".to_owned()),
                explain: None,
                limit: DEFAULT_SECTION_LIMIT,
                offset: 0,
                json_stream: false,
            },
            InsightsBuildOptions {
                workspace: Some(&workspace),
            },
        )
        .map_err(|error| error.to_string())?;

        assert_eq!(report.mode, InsightsMode::Section);
        assert_eq!(
            report.selected_section.as_deref(),
            Some("loadBearingMemories")
        );
        assert!(report.degraded_signals.is_empty());
        let item = report
            .sections
            .first()
            .and_then(|section| section.items.first())
            .ok_or_else(|| "load-bearing section should emit an item".to_owned())?;
        assert_eq!(
            item["memoryId"].as_str(),
            Some(load_bearing_memory_id.as_str())
        );
        assert_eq!(item["rank"].as_u64(), Some(1));
        assert_eq!(item["citingRuleCount"].as_u64(), Some(2));
        assert_eq!(item["interpretation"].as_str(), Some("load_bearing"));
        assert_eq!(
            item["evidence"]["algorithm"].as_str(),
            Some("bipartite_hits")
        );

        Ok(())
    }

    #[test]
    fn real_insight_sections_read_live_db_when_memory_link_snapshot_is_stale() -> TestResult {
        let workspace = unique_insights_workspace("real-insights")?;
        write_graph_feature_config(&workspace, true)?;
        seed_insights_graph_workspace(&workspace)?;

        for section_name in [
            "bridges",
            "contradictionClusters",
            "knowledgeSkyline",
            "topMemories",
        ] {
            let report = build_insights_report_with_options(
                &InsightsArgs {
                    section: Some(section_name.to_owned()),
                    explain: None,
                    limit: DEFAULT_SECTION_LIMIT,
                    offset: 0,
                    json_stream: false,
                },
                InsightsBuildOptions {
                    workspace: Some(&workspace),
                },
            )
            .map_err(|error| error.to_string())?;

            assert_eq!(report.mode, InsightsMode::Section);
            assert_eq!(report.selected_section.as_deref(), Some(section_name));
            assert!(
                report.degraded_signals.is_empty(),
                "{section_name} should use live DB rows without stale-snapshot degradation"
            );
            let item = report
                .sections
                .first()
                .and_then(|section| section.items.first())
                .ok_or_else(|| format!("{section_name} should emit live graph evidence"))?;
            match section_name {
                "bridges" => {
                    assert_eq!(item["articulationPoint"], true);
                    assert!(
                        item["clusterDisconnectionMagnitude"].as_u64().unwrap_or(0) > 0,
                        "bridge evidence should be ranked by disconnection magnitude"
                    );
                }
                "contradictionClusters" => {
                    assert_eq!(item["size"].as_u64(), Some(3));
                    assert_eq!(item["internalContradictions"].as_u64(), Some(3));
                    assert!(
                        item["exemplarMemoryIds"]
                            .as_array()
                            .is_some_and(|ids| ids.len() == 3),
                        "contradiction cluster should carry source memory ids"
                    );
                }
                "knowledgeSkyline" => {
                    assert_eq!(item["skyline"]["nodeCount"].as_u64(), Some(7));
                    assert!(
                        item["skyline"]["communities"]
                            .as_array()
                            .is_some_and(|communities| !communities.is_empty()),
                        "knowledge skyline should carry community provenance"
                    );
                }
                "topMemories" => {
                    assert_eq!(item["interpretation"].as_str(), Some("top_memory"));
                    assert!(
                        item["pagerank"].as_f64().is_some_and(|score| score > 0.0),
                        "topMemories should carry live PageRank evidence"
                    );
                    assert_eq!(
                        item["evidence"]["schema"].as_str(),
                        Some(TOP_MEMORY_INSIGHT_SCHEMA_V1)
                    );
                    assert_eq!(
                        item["evidence"]["algorithm"].as_str(),
                        Some("pagerank_with_retrieval_posture_tiebreak")
                    );
                }
                unexpected => return Err(format!("unexpected insights section {unexpected}")),
            }
        }

        Ok(())
    }

    #[test]
    fn selected_real_insight_sections_do_not_emit_placeholder_degradation() -> TestResult {
        for section in [
            "bridges",
            "contradictionClusters",
            "knowledgeSkyline",
            "topMemories",
        ] {
            let report = build_insights_report(&InsightsArgs {
                section: Some(section.to_owned()),
                explain: None,
                limit: DEFAULT_SECTION_LIMIT,
                offset: 0,
                json_stream: false,
            })
            .map_err(|error| error.to_string())?;

            assert_eq!(report.selected_section.as_deref(), Some(section));
            assert!(
                report
                    .degraded_signals
                    .iter()
                    .all(|signal| signal.code != INSIGHTS_SECTION_UNAVAILABLE_CODE),
                "{section} should no longer be placeholder-backed"
            );
        }

        Ok(())
    }

    #[test]
    fn bridge_section_uses_articulation_points_and_deterministic_ranking() -> TestResult {
        let links = vec![
            stored_memory_link_with_relation("link_bridge_1", "mem_a", "mem_b", "supports", None),
            stored_memory_link_with_relation("link_bridge_2", "mem_b", "mem_c", "supports", None),
            stored_memory_link_with_relation("link_bridge_3", "mem_c", "mem_d", "supports", None),
        ];

        let inputs = bridge_inputs_from_links(&links)
            .map_err(|error| format!("bridge input build failed: {error}"))?;
        let section = bridges_section_from_inputs(&inputs);

        assert_eq!(section.name, "bridges");
        assert_eq!(section.items.len(), 2);
        assert_eq!(section.items[0]["rank"], 1);
        assert_eq!(section.items[0]["memoryId"], "mem_b");
        assert_eq!(section.items[0]["articulationPoint"], true);
        assert_eq!(section.items[0]["clusterDisconnectionMagnitude"], 1);
        assert_eq!(section.items[0]["componentCountBefore"], 1);
        assert_eq!(section.items[0]["componentCountAfter"], 2);
        assert_eq!(section.items[0]["bridgeEdgeCount"], 2);
        assert_eq!(section.items[0]["degree"], 2);
        assert_eq!(
            section.items[0]["evidence"]["schema"],
            BRIDGE_INSIGHT_SCHEMA_V1
        );
        assert_eq!(
            section.items[0]["evidence"]["algorithm"],
            "tarjan_articulation_points"
        );
        assert_eq!(
            section.items[0]["evidence"]["bridgeAlgorithm"],
            "tarjan_bridges"
        );
        assert_eq!(section.items[1]["memoryId"], "mem_c");

        Ok(())
    }

    #[test]
    fn contradiction_clusters_section_uses_real_contradiction_graph() -> TestResult {
        let links = vec![
            stored_memory_link_with_relation(
                "link_contradiction_1",
                "mem_a",
                "mem_b",
                "contradicts",
                None,
            ),
            stored_memory_link_with_relation(
                "link_contradiction_2",
                "mem_b",
                "mem_c",
                "supersedes",
                None,
            ),
            stored_memory_link_with_relation(
                "link_contradiction_3",
                "mem_a",
                "mem_c",
                "resolves",
                None,
            ),
        ];

        let clusters = contradiction_clusters_from_links(&links)
            .map_err(|error| format!("contradiction cluster build failed: {error}"))?;
        let section = contradiction_clusters_section_from_clusters(&clusters);

        assert_eq!(section.name, "contradictionClusters");
        assert_eq!(section.items.len(), 1);
        assert_eq!(section.items[0]["size"], 3);
        assert_eq!(section.items[0]["internalContradictions"], 3);
        assert_eq!(section.items[0]["density"], 1.0);
        assert_eq!(section.items[0]["severity"], "incoherent");
        assert_eq!(section.items[0]["suggestedAction"], "curate_urgent");

        Ok(())
    }

    #[test]
    fn knowledge_gaps_use_graph_fixtures_and_reflection_recommendations() -> TestResult {
        let data = WorkspaceInsightsGraphData {
            memories: vec![
                stored_memory(
                    "mem_bridge_a",
                    "semantic",
                    "fact",
                    "Bridge endpoint A.",
                    0.9,
                ),
                stored_memory(
                    "mem_bridge_b",
                    "semantic",
                    "fact",
                    "Bridge articulation B.",
                    0.9,
                ),
                stored_memory(
                    "mem_bridge_c",
                    "semantic",
                    "fact",
                    "Bridge endpoint C.",
                    0.9,
                ),
                stored_memory(
                    "mem_contradiction_a",
                    "semantic",
                    "fact",
                    "Contradiction exemplar A.",
                    0.9,
                ),
                stored_memory(
                    "mem_contradiction_b",
                    "semantic",
                    "fact",
                    "Contradiction exemplar B.",
                    0.9,
                ),
                stored_memory(
                    "mem_contradiction_c",
                    "semantic",
                    "fact",
                    "Contradiction exemplar C.",
                    0.9,
                ),
                stored_memory(
                    "mem_harmful_outcome",
                    "episodic",
                    "failure",
                    "Harmful deployment outcome without a durable rule.",
                    0.8,
                ),
                stored_memory(
                    "mem_harm_neighbor",
                    "semantic",
                    "fact",
                    "Nearby evidence for the harmful incident.",
                    0.8,
                ),
                stored_memory(
                    "mem_causal_source",
                    "semantic",
                    "fact",
                    "Low-confidence causal source.",
                    0.6,
                ),
                stored_memory(
                    "mem_causal_target",
                    "semantic",
                    "fact",
                    "Low-confidence causal target.",
                    0.6,
                ),
            ],
            links: vec![
                stored_memory_link_with_evidence_and_confidence(
                    "link_bridge_1",
                    "mem_bridge_a",
                    "mem_bridge_b",
                    "supports",
                    1,
                    1.0,
                ),
                stored_memory_link_with_evidence_and_confidence(
                    "link_bridge_2",
                    "mem_bridge_b",
                    "mem_bridge_c",
                    "supports",
                    1,
                    1.0,
                ),
                stored_memory_link_with_relation(
                    "link_contra_1",
                    "mem_contradiction_a",
                    "mem_contradiction_b",
                    "contradicts",
                    None,
                ),
                stored_memory_link_with_relation(
                    "link_contra_2",
                    "mem_contradiction_b",
                    "mem_contradiction_c",
                    "contradicts",
                    None,
                ),
                stored_memory_link_with_relation(
                    "link_contra_3",
                    "mem_contradiction_a",
                    "mem_contradiction_c",
                    "contradicts",
                    None,
                ),
                stored_memory_link_with_relation(
                    "link_harm_neighbor",
                    "mem_harmful_outcome",
                    "mem_harm_neighbor",
                    "supports",
                    None,
                ),
                stored_memory_link_with_evidence_and_confidence(
                    "link_causal_low_confidence",
                    "mem_causal_source",
                    "mem_causal_target",
                    "supports",
                    4,
                    0.25,
                ),
            ],
        };

        let gaps = knowledge_gap_inputs_from_graph_data(&data)
            .map_err(|error| format!("knowledge gap input build failed: {error}"))?;
        let section = knowledge_gaps_section_from_inputs(&gaps);
        let section_json = serde_json::to_value(&section)
            .map_err(|error| format!("knowledgeGaps section should serialize: {error}"))?;
        let categories = section
            .items
            .iter()
            .map(|item| {
                item["category"]
                    .as_str()
                    .ok_or_else(|| "knowledge gap item missing category".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?;

        assert_eq!(section.name, "knowledgeGaps");
        assert_eq!(
            categories,
            vec![
                "thin_evidence_bridge",
                "unresolved_contradiction_cluster",
                "harmful_neighborhood_without_rule",
                "underdetermined_causal_chain",
            ]
        );
        assert_eq!(section_json["section"].as_str(), Some("knowledgeGaps"));
        let compact_recommendations = section_json["recommendations"]
            .as_array()
            .ok_or_else(|| "knowledgeGaps recommendations must be an array".to_owned())?;
        assert_eq!(compact_recommendations.len(), section.items.len());
        assert_eq!(
            compact_recommendations
                .iter()
                .map(|recommendation| recommendation["severity"].as_str().unwrap_or_default())
                .collect::<Vec<_>>(),
            vec!["high", "medium", "warning", "low"]
        );

        for (item, compact) in section.items.iter().zip(compact_recommendations) {
            assert!(item["rank"].as_u64().unwrap_or_default() > 0);
            assert_eq!(
                item["metricEvidence"]["schema"].as_str(),
                Some(KNOWLEDGE_GAP_SCHEMA_V1)
            );
            assert_eq!(
                item["recommendation"]["kind"].as_str(),
                Some("reflect_propose")
            );
            assert_eq!(compact["id"], item["gapId"]);
            assert_eq!(compact["reason"], item["explanation"]);
            assert_eq!(
                compact["recommendation_kind"].as_str(),
                Some("reflect_propose")
            );
            assert_eq!(
                compact["suggested_query"],
                item["recommendation"]["command"]
            );
            let source_ids = item["sourceMemoryIds"]
                .as_array()
                .ok_or_else(|| "sourceMemoryIds must be an array".to_owned())?
                .iter()
                .map(|id| {
                    id.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| "sourceMemoryIds entries must be strings".to_owned())
                })
                .collect::<Result<Vec<_>, _>>()?;
            assert!(!source_ids.is_empty());
            assert_eq!(
                item["recommendation"]["sourceMemoryIds"],
                item["sourceMemoryIds"]
            );

            let command = item["recommendation"]["command"]
                .as_str()
                .ok_or_else(|| "recommendation command must be a string".to_owned())?;
            for source_id in &source_ids {
                assert!(
                    command.contains(&format!("--source-memory {source_id}")),
                    "recommendation command should cite {source_id}: {command}"
                );
            }
            assert_reflect_propose_command(command, &source_ids)?;
        }

        Ok(())
    }

    #[test]
    fn knowledge_gaps_do_not_emit_fake_items_for_empty_or_healthy_graphs() -> TestResult {
        let empty = WorkspaceInsightsGraphData {
            memories: Vec::new(),
            links: Vec::new(),
        };
        let empty_gaps = knowledge_gap_inputs_from_graph_data(&empty)
            .map_err(|error| format!("empty knowledge gap build failed: {error}"))?;
        assert!(
            knowledge_gaps_section_from_inputs(&empty_gaps)
                .items
                .is_empty()
        );

        let healthy = WorkspaceInsightsGraphData {
            memories: vec![
                stored_memory("mem_healthy_a", "semantic", "fact", "Healthy A.", 0.9),
                stored_memory("mem_healthy_b", "semantic", "fact", "Healthy B.", 0.9),
                stored_memory("mem_healthy_c", "semantic", "fact", "Healthy C.", 0.9),
                stored_memory(
                    "mem_healthy_rule",
                    "procedural",
                    "rule",
                    "Durable rule with adjacent evidence.",
                    0.9,
                ),
            ],
            links: vec![
                stored_memory_link_with_evidence_and_confidence(
                    "link_healthy_1",
                    "mem_healthy_a",
                    "mem_healthy_b",
                    "supports",
                    3,
                    1.0,
                ),
                stored_memory_link_with_evidence_and_confidence(
                    "link_healthy_2",
                    "mem_healthy_b",
                    "mem_healthy_c",
                    "supports",
                    3,
                    1.0,
                ),
                stored_memory_link_with_evidence_and_confidence(
                    "link_healthy_3",
                    "mem_healthy_a",
                    "mem_healthy_c",
                    "supports",
                    3,
                    1.0,
                ),
                stored_memory_link_with_evidence_and_confidence(
                    "link_healthy_4",
                    "mem_healthy_b",
                    "mem_healthy_rule",
                    "supports",
                    3,
                    1.0,
                ),
            ],
        };
        let healthy_gaps = knowledge_gap_inputs_from_graph_data(&healthy)
            .map_err(|error| format!("healthy knowledge gap build failed: {error}"))?;
        assert!(
            knowledge_gaps_section_from_inputs(&healthy_gaps)
                .items
                .is_empty()
        );

        Ok(())
    }

    #[test]
    fn knowledge_gaps_are_deterministic_across_fixture_order() -> TestResult {
        let memories = vec![
            stored_memory("mem_order_a", "semantic", "fact", "Order A.", 0.9),
            stored_memory("mem_order_b", "semantic", "fact", "Order B.", 0.9),
            stored_memory("mem_order_c", "semantic", "fact", "Order C.", 0.9),
        ];
        let links = vec![
            stored_memory_link_with_relation(
                "link_order_1",
                "mem_order_a",
                "mem_order_b",
                "supports",
                None,
            ),
            stored_memory_link_with_relation(
                "link_order_2",
                "mem_order_b",
                "mem_order_c",
                "supports",
                None,
            ),
        ];
        let forward = WorkspaceInsightsGraphData {
            memories: memories.clone(),
            links: links.clone(),
        };
        let reverse = WorkspaceInsightsGraphData {
            memories: memories.into_iter().rev().collect(),
            links: links.into_iter().rev().collect(),
        };

        let forward_section = knowledge_gaps_section_from_inputs(
            &knowledge_gap_inputs_from_graph_data(&forward)
                .map_err(|error| format!("forward knowledge gap build failed: {error}"))?,
        );
        let reverse_section = knowledge_gaps_section_from_inputs(
            &knowledge_gap_inputs_from_graph_data(&reverse)
                .map_err(|error| format!("reverse knowledge gap build failed: {error}"))?,
        );

        assert_eq!(forward_section.items, reverse_section.items);
        Ok(())
    }

    #[test]
    fn blind_spots_use_symbol_graph_minus_memory_coverage_without_anchors() -> TestResult {
        let snapshot = crate::core::symbol_graph::extract_rust_symbol_snapshot_from_sources(&[
            crate::core::symbol_graph::RustSourceInput::new(
                "src/covered.rs",
                "pub fn covered_by_file() {}\n",
            ),
            crate::core::symbol_graph::RustSourceInput::new(
                "src/mixed.rs",
                "pub fn mentioned_symbol() {}\npub fn uncovered_symbol() {}\n",
            ),
        ]);
        let mut file_memory = stored_memory(
            "mem_file_coverage",
            "semantic",
            "fact",
            "A memory with direct file provenance.",
            0.9,
        );
        file_memory.provenance_uri = Some("file://src/covered.rs#L1".to_owned());
        let symbol_memory = stored_memory(
            "mem_symbol_coverage",
            "semantic",
            "fact",
            "The mentioned_symbol function carries the contract.",
            0.9,
        );

        let inputs =
            blind_spot_inputs_from_symbol_snapshot(&snapshot, &[file_memory, symbol_memory], None);
        let section = blind_spots_section_from_inputs(&inputs);
        let item = section
            .items
            .first()
            .ok_or_else(|| "blindSpots should include the uncovered symbol".to_owned())?;

        assert_eq!(section.name, "blindSpots");
        assert_eq!(section.items.len(), 1);
        assert_eq!(item["canonicalName"].as_str(), Some("uncovered_symbol"));
        assert_eq!(item["path"].as_str(), Some("src/mixed.rs"));
        assert_eq!(item["coverageStatus"].as_str(), Some("uncovered"));
        assert_eq!(item["coveredNodeCount"].as_u64(), Some(2));
        assert_eq!(item["totalNodeCount"].as_u64(), Some(3));
        assert_eq!(
            item["evidence"]["schema"].as_str(),
            Some(BLIND_SPOT_SCHEMA_V1)
        );
        assert_eq!(
            item["evidence"]["anchorTableRequired"].as_bool(),
            Some(false)
        );
        assert_eq!(
            item["evidence"]["algorithm"].as_str(),
            Some("symbol_snapshot_minus_memory_file_or_lexical_mentions")
        );
        Ok(())
    }

    #[test]
    fn blind_spots_are_deterministic_across_fixture_order() -> TestResult {
        let forward = crate::core::symbol_graph::extract_rust_symbol_snapshot_from_sources(&[
            crate::core::symbol_graph::RustSourceInput::new(
                "src/beta.rs",
                "pub fn beta_gap() {}\n",
            ),
            crate::core::symbol_graph::RustSourceInput::new(
                "src/alpha.rs",
                "pub fn alpha_gap() {}\n",
            ),
        ]);
        let reverse = crate::core::symbol_graph::extract_rust_symbol_snapshot_from_sources(&[
            crate::core::symbol_graph::RustSourceInput::new(
                "src/alpha.rs",
                "pub fn alpha_gap() {}\n",
            ),
            crate::core::symbol_graph::RustSourceInput::new(
                "src/beta.rs",
                "pub fn beta_gap() {}\n",
            ),
        ]);

        let forward_section = blind_spots_section_from_inputs(
            &blind_spot_inputs_from_symbol_snapshot(&forward, &[], None),
        );
        let reverse_section = blind_spots_section_from_inputs(
            &blind_spot_inputs_from_symbol_snapshot(&reverse, &[], None),
        );

        assert_eq!(forward_section.items, reverse_section.items);
        assert_eq!(
            forward_section
                .items
                .iter()
                .map(|item| item["canonicalName"].as_str().unwrap_or_default())
                .collect::<Vec<_>>(),
            vec!["alpha_gap", "beta_gap"]
        );
        Ok(())
    }

    #[test]
    fn top_memories_use_pagerank_posture_and_mesh_visibility() -> TestResult {
        let mut anchor = stored_memory(
            "mem_top_anchor",
            "procedural",
            "rule",
            "Central release rule.",
            0.95,
        );
        anchor.utility = 0.90;
        anchor.importance = 0.85;
        anchor.trust_class = "human_explicit".to_owned();
        let mut support = stored_memory(
            "mem_top_support",
            "semantic",
            "fact",
            "Supporting release fact.",
            0.80,
        );
        support.utility = 0.70;
        support.importance = 0.75;
        let private = stored_memory(
            "mem_top_private",
            "semantic",
            "fact",
            "Private memory behind hidden mesh metadata.",
            0.99,
        );
        let data = WorkspaceInsightsGraphData {
            memories: vec![
                support,
                private,
                anchor,
                stored_memory(
                    "mem_top_neighbor",
                    "episodic",
                    "decision",
                    "Neighbor decision.",
                    0.70,
                ),
            ],
            links: vec![
                stored_memory_link_with_relation(
                    "link_top_1",
                    "mem_top_support",
                    "mem_top_anchor",
                    "supports",
                    None,
                ),
                stored_memory_link_with_relation(
                    "link_top_2",
                    "mem_top_neighbor",
                    "mem_top_anchor",
                    "supports",
                    None,
                ),
                stored_memory_link(
                    "link_top_private",
                    "mem_top_anchor",
                    "mem_top_private",
                    Some(denied_mesh_link_metadata()),
                ),
            ],
        };
        let reverse = WorkspaceInsightsGraphData {
            memories: data.memories.iter().cloned().rev().collect(),
            links: data.links.iter().cloned().rev().collect(),
        };

        let section = top_memories_section_from_inputs(
            &top_memory_inputs_from_graph_data(&data)
                .map_err(|error| format!("top memory input build failed: {error}"))?,
        );
        let reverse_section = top_memories_section_from_inputs(
            &top_memory_inputs_from_graph_data(&reverse)
                .map_err(|error| format!("reverse top memory input build failed: {error}"))?,
        );

        assert_eq!(section.items, reverse_section.items);
        assert_eq!(section.name, "topMemories");
        assert!(
            section.items.len() >= 3,
            "top memories should include visible linked memories"
        );
        assert_eq!(section.items[0]["rank"].as_u64(), Some(1));
        assert_eq!(
            section.items[0]["memoryId"].as_str(),
            Some("mem_top_anchor")
        );
        assert_eq!(section.items[0]["level"].as_str(), Some("procedural"));
        assert_eq!(section.items[0]["kind"].as_str(), Some("rule"));
        assert_eq!(
            section.items[0]["trustClass"].as_str(),
            Some("human_explicit")
        );
        assert!(
            section.items[0]["pagerank"]
                .as_f64()
                .is_some_and(|score| score > 0.0)
        );
        assert!(
            section.items[0]["retrievalPosture"]
                .as_f64()
                .is_some_and(|score| score > 0.0)
        );
        assert_eq!(section.items[0]["linkDegree"].as_u64(), Some(2));
        assert_eq!(
            section.items[0]["evidence"]["schema"].as_str(),
            Some(TOP_MEMORY_INSIGHT_SCHEMA_V1)
        );
        assert_eq!(
            section.items[0]["evidence"]["algorithm"].as_str(),
            Some("pagerank_with_retrieval_posture_tiebreak")
        );
        assert!(
            section
                .items
                .iter()
                .all(|item| item["memoryId"].as_str() != Some("mem_top_private")),
            "mesh-hidden memories must not enter topMemories"
        );

        Ok(())
    }

    #[test]
    fn knowledge_skyline_section_serializes_existing_skyline_report() -> TestResult {
        let mut graph = Graph::new(CompatibilityMode::Strict);
        graph
            .add_edge("mem_a", "mem_b")
            .map_err(|error| error.to_string())?;
        graph
            .add_edge("mem_b", "mem_c")
            .map_err(|error| error.to_string())?;
        graph
            .add_edge("mem_a", "mem_c")
            .map_err(|error| error.to_string())?;
        let skyline = compute_knowledge_skyline(&KnowledgeSkylineInput {
            graph,
            memories: vec![
                skyline_memory("mem_a", "human_explicit", 1),
                skyline_memory("mem_b", "agent_validated", 2),
                skyline_memory("mem_c", "agent_validated", 3),
            ],
            ppr_scores: std::collections::BTreeMap::new(),
            as_of: Utc
                .with_ymd_and_hms(2026, 5, 4, 0, 0, 0)
                .single()
                .ok_or_else(|| "valid skyline as-of timestamp".to_owned())?,
        });

        let section = knowledge_skyline_section_from_report(Some(&skyline));

        assert_eq!(section.name, "knowledgeSkyline");
        assert_eq!(section.items.len(), 1);
        assert_eq!(section.items[0]["rank"], 1);
        assert_eq!(section.items[0]["interpretation"], "portfolio_posture");
        assert_eq!(
            section.items[0]["evidence"]["schema"],
            KNOWLEDGE_SKYLINE_SCHEMA_V1
        );
        assert_eq!(
            section.items[0]["skyline"]["schema"],
            KNOWLEDGE_SKYLINE_SCHEMA_V1
        );
        assert_eq!(section.items[0]["skyline"]["nodeCount"], 3);
        assert!(
            section.items[0]["skyline"]["communities"]
                .as_array()
                .is_some_and(|communities| !communities.is_empty())
        );

        Ok(())
    }

    #[test]
    fn knowledge_skyline_pagerank_respects_mesh_visibility() -> TestResult {
        let links = vec![
            stored_memory_link("visible", "mem_a", "mem_b", None),
            stored_memory_link(
                "denied",
                "mem_b",
                "mem_private",
                Some(denied_mesh_link_metadata()),
            ),
        ];

        let scores = pagerank_scores_for_skyline(&links)
            .map_err(|error| format!("skyline pagerank build failed: {error}"))?;

        assert!(scores.contains_key("mem_a"));
        assert!(scores.contains_key("mem_b"));
        assert!(!scores.contains_key("mem_private"));

        Ok(())
    }

    #[test]
    fn insights_degraded_signals_aggregate_same_code_sources() {
        let aggregated = aggregate_insights_degraded(vec![
            (
                "hubs",
                DegradationReport {
                    code: "graph_feature_disabled",
                    severity: "medium",
                    message: "HITS profile insights are disabled by graph.feature.hits_profiles.enabled.",
                    repair: "ee config set graph.feature.hits_profiles.enabled true",
                },
            ),
            (
                "authorities",
                DegradationReport {
                    code: "graph_feature_disabled",
                    severity: "medium",
                    message: "HITS profile insights are disabled by graph.feature.hits_profiles.enabled.",
                    repair: "ee config set graph.feature.hits_profiles.enabled true",
                },
            ),
        ]);

        assert_eq!(aggregated.len(), 1);
        assert_eq!(aggregated[0].code, "graph_feature_disabled");
        assert_eq!(
            aggregated[0].sources,
            vec!["authorities".to_owned(), "hubs".to_owned()]
        );
        assert_eq!(
            aggregated[0].repair.as_deref(),
            Some("ee config set graph.feature.hits_profiles.enabled true")
        );
    }

    #[test]
    fn section_pagination_clamps_limit_and_handles_empty_boundaries() -> TestResult {
        let report = build_insights_report(&InsightsArgs {
            section: Some("topMemories".to_owned()),
            explain: None,
            limit: 500,
            offset: 50,
            json_stream: false,
        })
        .map_err(|error| error.to_string())?;

        let pagination = report
            .pagination
            .ok_or_else(|| "section mode should include pagination".to_owned())?;
        assert_eq!(pagination.limit, MAX_SECTION_LIMIT);
        assert_eq!(pagination.offset, 50);
        assert_eq!(pagination.returned, 0);
        assert_eq!(pagination.total, 0);
        assert!(report.sections[0].items.is_empty());

        Ok(())
    }

    #[test]
    fn paginate_section_slices_items_with_offset_and_limit() -> TestResult {
        let section = InsightsSection {
            name: "topMemories",
            title: "Top Memories",
            summary: "fixture",
            why_it_matters: "fixture",
            items: vec![
                serde_json::json!({"id": "mem_1"}),
                serde_json::json!({"id": "mem_2"}),
                serde_json::json!({"id": "mem_3"}),
                serde_json::json!({"id": "mem_4"}),
            ],
            next_commands: vec![],
        };

        let page = paginate_section(section.clone(), 1, 2);
        assert_eq!(page.pagination.limit, 2);
        assert_eq!(page.pagination.offset, 1);
        assert_eq!(page.pagination.returned, 2);
        assert_eq!(page.pagination.total, 4);
        assert_eq!(
            page.section.items,
            vec![
                serde_json::json!({"id": "mem_2"}),
                serde_json::json!({"id": "mem_3"})
            ]
        );

        let empty_page = paginate_section(section, 10, 2);
        assert_eq!(empty_page.pagination.limit, 2);
        assert_eq!(empty_page.pagination.offset, 10);
        assert_eq!(empty_page.pagination.returned, 0);
        assert_eq!(empty_page.pagination.total, 4);
        assert!(empty_page.section.items.is_empty());

        Ok(())
    }

    #[test]
    fn proximity_hotspots_orders_pairs_by_min_cut_then_memory_ids() -> TestResult {
        let reports = vec![
            proximity_report("mem_b", "mem_c", Some(2.0), Some(vec!["mem_b", "mem_c"])),
            proximity_report(
                "mem_a",
                "mem_d",
                Some(0.5),
                Some(vec!["mem_a", "bridge", "mem_d"]),
            ),
            proximity_report("mem_a", "mem_c", Some(0.5), Some(vec!["mem_a", "mem_c"])),
            proximity_report("mem_x", "mem_y", None, None),
        ];

        let section = proximity_hotspots_section_from_reports(&reports);

        assert_eq!(section.name, "proximityHotspots");
        assert_eq!(section.items.len(), 3);
        assert_eq!(section.items[0]["rank"], 1);
        assert_eq!(section.items[0]["memoryA"], "mem_a");
        assert_eq!(section.items[0]["memoryB"], "mem_c");
        assert_eq!(section.items[0]["minCut"], 0.5);
        assert_eq!(
            section.items[0]["evidence"]["schema"],
            PROXIMITY_REPORT_SCHEMA_V1
        );
        assert_eq!(section.items[0]["evidence"]["algorithm"], "gomory_hu_tree");
        assert_eq!(section.items[1]["memoryA"], "mem_a");
        assert_eq!(section.items[1]["memoryB"], "mem_d");
        assert_eq!(section.items[2]["memoryA"], "mem_b");
        assert_eq!(section.items[2]["memoryB"], "mem_c");

        Ok(())
    }

    #[test]
    fn proximity_hotspots_ignore_denied_mesh_links() -> TestResult {
        let links = vec![
            stored_memory_link("link_allowed", "mem_a", "mem_b", None),
            stored_memory_link(
                "link_denied",
                "mem_b",
                "mem_c",
                Some(denied_mesh_link_metadata()),
            ),
        ];

        let reports = proximity_hotspot_reports_from_links(&links)
            .map_err(|error| format!("failed to build proximity reports: {error}"))?;

        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].memory_a, "mem_a");
        assert_eq!(reports[0].memory_b, "mem_b");
        assert_eq!(
            reports[0].tree_path.as_deref(),
            Some(&["mem_a".to_owned(), "mem_b".to_owned()][..])
        );

        Ok(())
    }

    #[test]
    fn causal_bottlenecks_order_by_betweenness_then_memory_id() -> TestResult {
        let reports = vec![
            causal_bottleneck("mem_b", 0.25),
            causal_bottleneck("mem_a", 0.75),
            causal_bottleneck("mem_c", 0.75),
            causal_bottleneck("mem_zero", 0.0),
            causal_bottleneck("mem_nan", f64::NAN),
        ];

        let section = causal_bottlenecks_section_from_reports(&reports);

        assert_eq!(section.name, "causalBottlenecks");
        assert_eq!(section.items.len(), 3);
        assert_eq!(section.items[0]["rank"], 1);
        assert_eq!(section.items[0]["memoryId"], "mem_a");
        assert_eq!(section.items[0]["betweenness"], 0.75);
        assert_eq!(
            section.items[0]["evidence"]["schema"],
            CAUSAL_BOTTLENECK_REPORT_SCHEMA_V1
        );
        assert_eq!(
            section.items[0]["evidence"]["algorithm"],
            "betweenness_centrality_directed"
        );
        assert_eq!(section.items[1]["memoryId"], "mem_c");
        assert_eq!(section.items[2]["memoryId"], "mem_b");

        Ok(())
    }

    #[test]
    fn causal_bottleneck_reports_preserve_centrality_scores() -> TestResult {
        let reports = causal_bottleneck_reports_from_scores(&[
            CentralityScore {
                node: "mem_bridge".to_owned(),
                score: 0.625,
            },
            CentralityScore {
                node: "mem_root".to_owned(),
                score: 0.125,
            },
        ]);

        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].memory_id, "mem_bridge");
        assert_eq!(reports[0].betweenness, 0.625);
        assert_eq!(reports[0].snapshot_version, 0);
        assert_eq!(reports[1].memory_id, "mem_root");
        assert_eq!(reports[1].betweenness, 0.125);
        assert_eq!(reports[1].snapshot_version, 0);

        Ok(())
    }

    #[test]
    fn causal_bottlenecks_empty_reports_keep_section_contract() -> TestResult {
        let section = causal_bottlenecks_section_from_reports(&[]);

        assert_eq!(section.name, "causalBottlenecks");
        assert_eq!(section.title, "Causal Bottlenecks");
        assert_eq!(
            section.summary,
            "High-betweenness memories in causal-evidence subgraphs."
        );
        assert!(section.items.is_empty());
        assert_eq!(
            section.next_commands,
            vec!["ee insights --section causalBottlenecks --workspace . --json"]
        );

        Ok(())
    }

    #[test]
    fn hubs_and_authorities_order_by_score_then_memory_id() -> TestResult {
        let scores = HitsScores {
            hubs: std::collections::BTreeMap::from([
                ("mem_b".to_owned(), 0.25),
                ("mem_a".to_owned(), 0.75),
                ("mem_c".to_owned(), 0.75),
                ("mem_zero".to_owned(), 0.0),
                ("mem_nan".to_owned(), f64::NAN),
            ]),
            authorities: std::collections::BTreeMap::from([
                ("mem_source".to_owned(), 0.1),
                ("mem_authority".to_owned(), 0.9),
            ]),
        };

        let hubs = hubs_section_from_scores(&scores);
        assert_eq!(hubs.name, "hubs");
        assert_eq!(hubs.items.len(), 3);
        assert_eq!(hubs.items[0]["rank"], 1);
        assert_eq!(hubs.items[0]["memoryId"], "mem_a");
        assert_eq!(hubs.items[0]["hubScore"], 0.75);
        assert_eq!(hubs.items[0]["interpretation"], "hub");
        assert_eq!(hubs.items[0]["evidence"]["schema"], HITS_REPORT_SCHEMA_V1);
        assert_eq!(
            hubs.items[0]["evidence"]["algorithm"],
            "hits_centrality_directed"
        );
        assert_eq!(hubs.items[1]["memoryId"], "mem_c");
        assert_eq!(hubs.items[2]["memoryId"], "mem_b");

        let authorities = authorities_section_from_scores(&scores);
        assert_eq!(authorities.name, "authorities");
        assert_eq!(authorities.items.len(), 2);
        assert_eq!(authorities.items[0]["rank"], 1);
        assert_eq!(authorities.items[0]["memoryId"], "mem_authority");
        assert_eq!(authorities.items[0]["authorityScore"], 0.9);
        assert_eq!(authorities.items[0]["interpretation"], "authority");
        assert_eq!(
            authorities.items[0]["evidence"]["schema"],
            HITS_REPORT_SCHEMA_V1
        );
        assert_eq!(authorities.items[1]["memoryId"], "mem_source");

        Ok(())
    }

    fn causal_bottleneck(memory_id: &str, betweenness: f64) -> CausalBottleneckInput {
        CausalBottleneckInput {
            memory_id: memory_id.to_owned(),
            betweenness,
            snapshot_version: 7,
        }
    }

    fn proximity_report(
        left: &str,
        right: &str,
        min_cut: Option<f64>,
        tree_path: Option<Vec<&str>>,
    ) -> ProximityHotspotInput {
        ProximityHotspotInput {
            memory_a: left.to_owned(),
            memory_b: right.to_owned(),
            snapshot_version: 42,
            min_cut,
            interpretation: min_cut
                .map(|cut| {
                    if cut < 1.0 {
                        "weak"
                    } else if cut < 3.0 {
                        "moderate"
                    } else {
                        "strong"
                    }
                })
                .unwrap_or("unavailable")
                .to_owned(),
            tree_path: tree_path.map(|nodes| nodes.into_iter().map(str::to_owned).collect()),
        }
    }

    fn stored_memory_link(
        id: &str,
        source: &str,
        target: &str,
        metadata_json: Option<String>,
    ) -> StoredMemoryLink {
        stored_memory_link_with_relation(id, source, target, "related", metadata_json)
    }

    fn stored_memory_link_with_relation(
        id: &str,
        source: &str,
        target: &str,
        relation: &str,
        metadata_json: Option<String>,
    ) -> StoredMemoryLink {
        stored_memory_link_with_evidence_and_confidence(id, source, target, relation, 1, 1.0)
            .with_metadata(metadata_json)
    }

    trait StoredMemoryLinkTestExt {
        fn with_metadata(self, metadata_json: Option<String>) -> Self;
    }

    impl StoredMemoryLinkTestExt for StoredMemoryLink {
        fn with_metadata(mut self, metadata_json: Option<String>) -> Self {
            self.metadata_json = metadata_json;
            self
        }
    }

    fn stored_memory_link_with_evidence_and_confidence(
        id: &str,
        source: &str,
        target: &str,
        relation: &str,
        evidence_count: u32,
        confidence: f32,
    ) -> StoredMemoryLink {
        StoredMemoryLink {
            id: id.to_owned(),
            src_memory_id: source.to_owned(),
            dst_memory_id: target.to_owned(),
            relation: relation.to_owned(),
            weight: 1.0,
            confidence,
            directed: false,
            evidence_count,
            last_reinforced_at: None,
            source: "agent".to_owned(),
            created_at: "2026-05-16T00:00:00Z".to_owned(),
            created_by: Some("insights-mesh-test".to_owned()),
            metadata_json: None,
        }
    }

    fn stored_memory(
        id: &str,
        level: &str,
        kind: &str,
        content: &str,
        confidence: f32,
    ) -> StoredMemory {
        StoredMemory {
            id: id.to_owned(),
            workspace_id: "wsp_insights_test".to_owned(),
            level: level.to_owned(),
            kind: kind.to_owned(),
            content: content.to_owned(),
            workflow_id: None,
            confidence,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "agent_validated".to_owned(),
            trust_subclass: None,
            provenance_chain_hash: None,
            provenance_chain_hash_version: "v1".to_owned(),
            provenance_verification_status: "unverified".to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: "2026-05-16T00:00:00Z".to_owned(),
            updated_at: "2026-05-16T00:00:00Z".to_owned(),
            tombstoned_at: None,
            valid_from: Some("2026-05-16T00:00:00Z".to_owned()),
            valid_to: None,
        }
    }

    fn assert_reflect_propose_command(command: &str, source_ids: &[String]) -> TestResult {
        let cli = crate::cli::Cli::try_parse_from(command.split_whitespace())
            .map_err(|error| format!("recommendation command should parse: {error}"))?;
        let Some(crate::cli::Command::Reflect(crate::cli::ReflectCommand::Propose(args))) =
            cli.command
        else {
            return Err(format!(
                "recommendation command should parse as reflect propose: {command}"
            ));
        };
        assert_eq!(cli.workspace.as_deref(), Some(std::path::Path::new(".")));
        assert!(cli.json);
        assert_eq!(args.kind.as_deref(), Some("gaps"));
        assert_eq!(args.source_memory.as_slice(), source_ids);
        assert!(args.source.is_empty());
        assert!(args.source_evidence_span.is_empty());
        assert!(args.dry_run);
        Ok(())
    }

    fn skyline_memory(memory_id: &str, trust_class: &str, day: u32) -> KnowledgeSkylineMemory {
        KnowledgeSkylineMemory {
            memory_id: memory_id.to_owned(),
            trust_class: trust_class.to_owned(),
            created_at: Utc
                .with_ymd_and_hms(2026, 5, day, 0, 0, 0)
                .single()
                .expect("valid skyline memory timestamp"),
        }
    }

    fn denied_mesh_link_metadata() -> String {
        serde_json::json!({
            "mesh": {
                "workspaceScopeDecision": "deny",
                "materialLane": "graphSignal",
                "cachedMaterialId": "mesh_insights_denied",
                "originWorkspaceId": "wsp_remote_private",
                "originWorkspaceLabel": "/Users/alice/private/repo",
                "producerPeerId": "peer_builder_one",
                "producerPeerLabel": "/Users/alice/private/peer-agent",
                "importDecisionId": "mesh_insights_decision_denied",
                "trustLane": "quarantined",
                "redactionPosture": "metadata_only"
            }
        })
        .to_string()
    }
}
