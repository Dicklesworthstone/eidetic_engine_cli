use std::path::{Path, PathBuf};

use clap::Args;
use fnx_algorithms::CentralityScore;
use fnx_classes::{AttrMap, Graph};
use fnx_runtime::CgseValue;
use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::config::{
    GRAPH_FEATURE_CAUSAL_EXPLAIN_ENABLED_KEY, GRAPH_FEATURE_HITS_PROFILES_ENABLED_KEY,
    GRAPH_FEATURE_LOAD_BEARING_ENABLED_KEY, GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY,
    GRAPH_FEATURE_SKYLINE_ENABLED_KEY,
};
use crate::core::config_surface::{ConfigSurfaceOptions, get_config};
use crate::core::degraded_aggregation::{AggregatedDegradation, aggregate_degraded};
use crate::core::status::DegradationReport;
use crate::db::{DbConnection, StoredMemoryLink};
use crate::graph::gomory_hu::{
    GOMORY_HU_WEIGHT_ATTR, PROXIMITY_SCHEMA_V1, build_gomory_hu_tree, query_proximity,
};
use crate::graph::hits::{HITS_REPORT_SCHEMA_V1, HitsScores, compute_hits_report};
use crate::models::{DomainError, RESPONSE_SCHEMA_V1};

pub const INSIGHTS_SCHEMA_V1: &str = "ee.insights.v1";
const PROXIMITY_REPORT_SCHEMA_V1: &str = PROXIMITY_SCHEMA_V1;
const CAUSAL_BOTTLENECK_REPORT_SCHEMA_V1: &str = "ee.graph.causal_evidence_projection.v1";
const DEFAULT_SECTION_LIMIT: usize = 10;
const MAX_SECTION_LIMIT: usize = 100;
const EMPTY_WORKSPACE_GENERATED_AT: &str = "1970-01-01T00:00:00Z";

type SectionBuilder = fn() -> InsightsSection;
type SectionRegistryEntry = (&'static str, &'static str, SectionBuilder);

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct InsightsArgs {
    /// Emit only one insight section by name.
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InsightsSection {
    pub name: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
    pub why_it_matters: &'static str,
    pub items: Vec<JsonValue>,
    pub next_commands: Vec<&'static str>,
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

#[derive(Clone, Debug, PartialEq)]
struct HitsInsightInput {
    memory_id: String,
    score: f64,
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
                repair: Some(format!("ee insights --section <{available}>")),
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
        degraded_signals_for_sections(&sections)
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

    Ok(BuiltSection {
        section: build_registry_section(display_name, builder, workspace)?,
        degraded_signal: None,
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
        "causalBottlenecks" => {
            let reports = load_causal_bottleneck_reports(workspace)?;
            Ok(causal_bottlenecks_section_from_reports(&reports))
        }
        "hubs" => {
            let scores = load_hits_scores(workspace)?;
            Ok(hubs_section_from_scores(&scores))
        }
        "proximityHotspots" => {
            let reports = load_proximity_hotspot_reports(workspace)?;
            Ok(proximity_hotspots_section_from_reports(&reports))
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

fn degraded_signals_for_sections(sections: &[InsightsSection]) -> Vec<InsightsDegradedInput> {
    if sections.iter().any(|section| !section.items.is_empty()) {
        Vec::new()
    } else {
        vec![(
            "insights",
            DegradationReport {
                code: "graph.workspace_empty",
                severity: "info",
                message: "No graph memories are available for insights yet.",
                repair: "run: ee remember --workspace . \"<memory>\" --json",
            },
        )]
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
    serde_json::json!({
        "schema": RESPONSE_SCHEMA_V1,
        "success": true,
        "data": report,
    })
    .to_string()
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
    placeholder_section(
        "bridges",
        "Bridge Memories",
        "Top articulation-point memories ranked by cluster-disconnection-magnitude.",
        "Bridge memories deserve careful decay and review because removing them can disconnect useful context.",
        vec!["ee insights --section bridges --workspace . --json"],
    )
}

fn causal_bottlenecks_section() -> InsightsSection {
    causal_bottlenecks_section_from_reports(&[])
}

fn causal_bottlenecks_section_from_reports(reports: &[CausalBottleneckInput]) -> InsightsSection {
    let mut reports = reports
        .iter()
        .filter(|report| report.betweenness.is_finite() && report.betweenness > 0.0)
        .collect::<Vec<_>>();
    reports.sort_by(|left, right| {
        right
            .betweenness
            .partial_cmp(&left.betweenness)
            .unwrap_or(std::cmp::Ordering::Equal)
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
    placeholder_section(
        "contradictionClusters",
        "Contradiction Clusters",
        "Louvain communities filtered to contradiction-heavy memory neighborhoods.",
        "Contradiction clusters identify parts of the memory graph that need curation before agents rely on them.",
        vec!["ee insights --section contradictionClusters --workspace . --json"],
    )
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
    inputs.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
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

fn knowledge_skyline_section() -> InsightsSection {
    placeholder_section(
        "knowledgeSkyline",
        "Knowledge Skyline",
        "Composite posture across onion layer, community, age, trust, and graph health signals.",
        "The skyline gives agents a portfolio-level view of memory quality before relying on a workspace.",
        vec!["ee insights --section knowledgeSkyline --workspace . --json"],
    )
}

fn load_bearing_memories_section() -> InsightsSection {
    placeholder_section(
        "loadBearingMemories",
        "Load-Bearing Memories",
        "Memories with high influence in rule-to-source provenance projections.",
        "Load-bearing memories should be preserved or reviewed carefully because many rules depend on them.",
        vec!["ee insights --section loadBearingMemories --workspace . --json"],
    )
}

fn proximity_hotspots_section() -> InsightsSection {
    proximity_hotspots_section_from_reports(&[])
}

fn proximity_hotspots_section_from_reports(reports: &[ProximityHotspotInput]) -> InsightsSection {
    let mut reports = reports
        .iter()
        .filter(|report| report.min_cut.is_some_and(f64::is_finite))
        .collect::<Vec<_>>();
    reports.sort_by(|left, right| {
        left.min_cut
            .partial_cmp(&right.min_cut)
            .unwrap_or(std::cmp::Ordering::Equal)
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
    placeholder_section(
        "topMemories",
        "Top Memories",
        "Top-ranked memories by cached graph centrality and retrieval posture.",
        "Top memories provide an immediate overview of the facts most likely to shape agent behavior.",
        vec!["ee insights --section topMemories --workspace . --json"],
    )
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

    #[test]
    fn full_bundle_uses_deterministic_section_order() -> TestResult {
        let report = build_insights_report(&InsightsArgs {
            section: None,
            explain: None,
            limit: DEFAULT_SECTION_LIMIT,
            offset: 0,
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
                "bridges",
                "causalBottlenecks",
                "comprehensiveRules",
                "contradictionClusters",
                "hubs",
                "kCore",
                "kTruss",
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
        assert_eq!(report.degraded_signals.len(), 1);
        assert_eq!(report.degraded_signals[0].code, "graph.workspace_empty");
        assert_eq!(report.degraded_signals[0].severity, "info");
        assert_eq!(
            report.degraded_signals[0].sources,
            vec!["insights".to_owned()]
        );
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
        })
        .map_err(|error| error.to_string())?;
        let json: serde_json::Value = serde_json::from_str(&render_insights_json(&report))
            .map_err(|error| {
                format!("rendered insights JSON should parse as response envelope: {error}")
            })?;
        let data = &json["data"];

        assert_eq!(json["schema"], RESPONSE_SCHEMA_V1);
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
        StoredMemoryLink {
            id: id.to_owned(),
            src_memory_id: source.to_owned(),
            dst_memory_id: target.to_owned(),
            relation: "related".to_owned(),
            weight: 1.0,
            confidence: 1.0,
            directed: false,
            evidence_count: 1,
            last_reinforced_at: None,
            source: "agent".to_owned(),
            created_at: "2026-05-16T00:00:00Z".to_owned(),
            created_by: Some("insights-mesh-test".to_owned()),
            metadata_json,
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
