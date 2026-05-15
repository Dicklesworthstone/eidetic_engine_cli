use std::collections::BTreeMap;

use clap::Args;
use serde::Serialize;

use crate::models::{DomainError, RESPONSE_SCHEMA_V1};

pub const INSIGHTS_SCHEMA_V1: &str = "ee.insights.v1";

type SectionBuilder = fn() -> InsightsSection;

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct InsightsArgs {
    /// Emit only one insight section by name.
    #[arg(long, value_name = "NAME")]
    pub section: Option<String>,

    /// Frame the insights bundle around a memory explanation target.
    #[arg(long, value_name = "MEMORY_ID", conflicts_with = "section")]
    pub explain: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InsightsReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub mode: InsightsMode,
    pub snapshot_version: &'static str,
    pub generated_at: Option<String>,
    pub run_duration_ms: u64,
    pub selected_section: Option<String>,
    pub explain_memory_id: Option<String>,
    pub explain_command: Option<String>,
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
    pub next_commands: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InsightsDegradedSignal {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: &'static str,
    pub repair: Option<&'static str>,
}

fn section_registry() -> BTreeMap<&'static str, SectionBuilder> {
    let mut registry = BTreeMap::new();
    registry.insert("coordination", coordination_section as SectionBuilder);
    registry.insert("graph", graph_section as SectionBuilder);
    registry.insert("retrieval", retrieval_section as SectionBuilder);
    registry.insert("verification", verification_section as SectionBuilder);
    registry
}

pub fn build_insights_report(args: &InsightsArgs) -> Result<InsightsReport, DomainError> {
    let registry = section_registry();
    let available_sections: Vec<&'static str> = registry.keys().copied().collect();
    let mode = if args.explain.is_some() {
        InsightsMode::Explain
    } else if args.section.is_some() {
        InsightsMode::Section
    } else {
        InsightsMode::FullBundle
    };

    let (selected_section, sections) = if let Some(section) = args.section.as_deref() {
        let normalized = section.trim().to_ascii_lowercase();
        let Some(builder) = registry.get(normalized.as_str()) else {
            let available = available_sections.join(", ");
            return Err(DomainError::Usage {
                message: format!(
                    "Unknown insights section `{section}`. Available sections: {available}."
                ),
                repair: Some(format!("ee insights --section <{available}>")),
            });
        };
        (Some(normalized), vec![builder()])
    } else {
        (None, registry.values().map(|builder| builder()).collect())
    };

    let explain_memory_id = args.explain.clone();
    let explain_command = explain_memory_id
        .as_ref()
        .map(|memory_id| format!("ee why {memory_id} --json"));

    Ok(InsightsReport {
        schema: INSIGHTS_SCHEMA_V1,
        command: "insights",
        mode,
        snapshot_version: "scaffold",
        generated_at: None,
        run_duration_ms: 0,
        selected_section,
        explain_memory_id,
        explain_command,
        available_sections,
        sections,
        degraded_signals: Vec::new(),
    })
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

fn coordination_section() -> InsightsSection {
    InsightsSection {
        name: "coordination",
        title: "Coordination Posture",
        summary: "Read-only swarm coordination, active work, and edit-surface awareness.",
        why_it_matters: "Crowded agent repos need an early signal before claiming work or touching files.",
        next_commands: vec![
            "ee swarm brief --workspace . --json",
            "br ready --json",
            "bv --robot-triage",
        ],
    }
}

fn graph_section() -> InsightsSection {
    InsightsSection {
        name: "graph",
        title: "Graph Readiness",
        summary: "Graph snapshots, centrality refreshes, and graph-derived retrieval feature posture.",
        why_it_matters: "Graph insights are derived assets; stale or missing graph state must not block core retrieval.",
        next_commands: vec![
            "ee diag graph --workspace . --json",
            "ee graph export --workspace . --json",
            "ee graph centrality-refresh --workspace . --dry-run --json",
        ],
    }
}

fn retrieval_section() -> InsightsSection {
    InsightsSection {
        name: "retrieval",
        title: "Retrieval And Packing",
        summary: "Search, context packing, provenance, and score-explanation surfaces.",
        why_it_matters: "The core agent workflow depends on explainable retrieval before any higher-level insights matter.",
        next_commands: vec![
            "ee search \"<query>\" --workspace . --json",
            "ee context \"<task>\" --workspace . --max-tokens 4000 --json",
            "ee why <memory-id> --workspace . --json",
        ],
    }
}

fn verification_section() -> InsightsSection {
    InsightsSection {
        name: "verification",
        title: "Verification Closure",
        summary: "Evidence capture, closure guidance, and readiness gates for safe handoff.",
        why_it_matters: "Agents need concrete verification evidence rather than relying on intent or proxy success signals.",
        next_commands: vec![
            "ee verification closure-guidance --workspace . --json",
            "cargo fmt --check",
            "cargo clippy --all-targets -- -D warnings",
        ],
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
        })
        .map_err(|error| error.to_string())?;

        assert_eq!(report.schema, INSIGHTS_SCHEMA_V1);
        assert_eq!(report.mode, InsightsMode::FullBundle);
        assert_eq!(report.snapshot_version, "scaffold");
        assert_eq!(report.generated_at, None);
        assert_eq!(report.run_duration_ms, 0);
        assert_eq!(
            report.available_sections,
            vec!["coordination", "graph", "retrieval", "verification"]
        );
        assert_eq!(section_names(&report), report.available_sections);
        assert_eq!(report.selected_section, None);
        assert_eq!(report.explain_memory_id, None);
        assert_eq!(report.explain_command, None);
        assert!(report.degraded_signals.is_empty());

        Ok(())
    }

    #[test]
    fn explain_mode_preserves_memory_target_and_full_context() -> TestResult {
        let report = build_insights_report(&InsightsArgs {
            section: None,
            explain: Some("mem_123".to_owned()),
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
            section: Some("graph".to_owned()),
            explain: None,
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
        assert_eq!(data["snapshotVersion"], "scaffold");
        assert!(data["generatedAt"].is_null());
        assert_eq!(data["runDurationMs"], 0);
        assert_eq!(data["selectedSection"], "graph");
        assert!(
            data["degradedSignals"]
                .as_array()
                .is_some_and(Vec::is_empty)
        );

        Ok(())
    }
}
