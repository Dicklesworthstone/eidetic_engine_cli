use clap::Args;
use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::models::{DomainError, RESPONSE_SCHEMA_V1};

pub const INSIGHTS_SCHEMA_V1: &str = "ee.insights.v1";
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
    pub code: &'static str,
    pub severity: &'static str,
    pub message: &'static str,
    pub repair: Option<&'static str>,
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

pub fn build_insights_report(args: &InsightsArgs) -> Result<InsightsReport, DomainError> {
    let registry = section_registry();
    let available_sections: Vec<&'static str> = registry.iter().map(|(_, name, _)| *name).collect();
    let mode = if args.explain.is_some() {
        InsightsMode::Explain
    } else if args.section.is_some() {
        InsightsMode::Section
    } else {
        InsightsMode::FullBundle
    };

    let (selected_section, sections, pagination) = if let Some(section) = args.section.as_deref() {
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
        let section = paginate_section(builder(), args.offset, args.limit);
        (
            Some((*display_name).to_owned()),
            vec![section.section],
            Some(section.pagination),
        )
    } else {
        (
            None,
            registry.iter().map(|(_, _, builder)| builder()).collect(),
            None,
        )
    };

    let explain_memory_id = args.explain.clone();
    let explain_command = explain_memory_id
        .as_ref()
        .map(|memory_id| format!("ee why {memory_id} --json"));

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
        degraded_signals: vec![InsightsDegradedSignal {
            code: "graph.workspace_empty",
            severity: "info",
            message: "No graph memories are available for insights yet.",
            repair: Some("run: ee remember --workspace . \"<memory>\" --json"),
        }],
    })
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
    placeholder_section(
        "authorities",
        "Authority Memories",
        "HITS authority scores for memories that ground claims from many hubs.",
        "Authority memories help agents distinguish grounded evidence from navigational references.",
        vec!["ee insights --section authorities --workspace . --json"],
    )
}

fn bridges_section() -> InsightsSection {
    placeholder_section(
        "bridges",
        "Bridge Memories",
        "Articulation points and bridge-like memories that connect otherwise separate knowledge regions.",
        "Bridge memories deserve careful decay and review because removing them can disconnect useful context.",
        vec!["ee insights --section bridges --workspace . --json"],
    )
}

fn causal_bottlenecks_section() -> InsightsSection {
    placeholder_section(
        "causalBottlenecks",
        "Causal Bottlenecks",
        "High-betweenness memories in causal-evidence subgraphs.",
        "Causal bottlenecks show which facts carry the most explanatory load for failures and repairs.",
        vec!["ee insights --section causalBottlenecks --workspace . --json"],
    )
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
    placeholder_section(
        "hubs",
        "Hub Memories",
        "HITS hub scores for memories that point to many authoritative facts.",
        "Hub memories are useful navigation anchors for agents assembling a task-specific context pack.",
        vec!["ee insights --section hubs --workspace . --json"],
    )
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
    placeholder_section(
        "proximityHotspots",
        "Proximity Hotspots",
        "Memory pairs with small min-cut distance in Gomory-Hu proximity projections.",
        "Proximity hotspots surface tightly coupled facts that should be packed, reviewed, or curated together.",
        vec!["ee insights --section proximityHotspots --workspace . --json"],
    )
}

fn revision_frontiers_section() -> InsightsSection {
    placeholder_section(
        "revisionFrontiers",
        "Revision Frontiers",
        "Dominance-frontier findings for logical memory revision DAGs.",
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

    type TestResult = Result<(), String>;

    fn section_names(report: &InsightsReport) -> Vec<&'static str> {
        report.sections.iter().map(|section| section.name).collect()
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
}
