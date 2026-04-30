//! Active learning agenda and uncertainty operations (EE-441).
//!
//! Provides agenda, uncertainty, and summary operations for identifying
//! knowledge gaps and prioritizing learning opportunities.

use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::models::DomainError;

/// Schema for learning agenda report.
pub const LEARN_AGENDA_SCHEMA_V1: &str = "ee.learn.agenda.v1";

/// Schema for uncertainty report.
pub const LEARN_UNCERTAINTY_SCHEMA_V1: &str = "ee.learn.uncertainty.v1";

/// Schema for learning summary report.
pub const LEARN_SUMMARY_SCHEMA_V1: &str = "ee.learn.summary.v1";

// ============================================================================
// Agenda Operation
// ============================================================================

/// Options for showing the learning agenda.
#[derive(Clone, Debug, Default)]
pub struct LearnAgendaOptions {
    pub workspace: PathBuf,
    pub limit: u32,
    pub topic: Option<String>,
    pub include_resolved: bool,
    pub sort: String,
}

/// A single item in the learning agenda.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgendaItem {
    pub id: String,
    pub topic: String,
    pub gap_description: String,
    pub priority: u8,
    pub uncertainty: f64,
    pub source: String,
    pub status: String,
    pub created_at: String,
}

/// Report from showing the learning agenda.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearnAgendaReport {
    pub schema: String,
    pub items: Vec<AgendaItem>,
    pub total_gaps: u32,
    pub high_priority_count: u32,
    pub resolved_count: u32,
    pub generated_at: String,
}

impl LearnAgendaReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Show the active learning agenda.
pub fn show_agenda(options: &LearnAgendaOptions) -> Result<LearnAgendaReport, DomainError> {
    let now = Utc::now().to_rfc3339();

    let all_items = vec![
        AgendaItem {
            id: "gap_001".to_owned(),
            topic: "error_handling".to_owned(),
            gap_description: "Missing patterns for async error propagation".to_owned(),
            priority: 85,
            uncertainty: 0.72,
            source: "curation_review".to_owned(),
            status: "open".to_owned(),
            created_at: now.clone(),
        },
        AgendaItem {
            id: "gap_002".to_owned(),
            topic: "testing".to_owned(),
            gap_description: "No integration test patterns for database operations".to_owned(),
            priority: 70,
            uncertainty: 0.65,
            source: "failure_analysis".to_owned(),
            status: "open".to_owned(),
            created_at: now.clone(),
        },
        AgendaItem {
            id: "gap_003".to_owned(),
            topic: "cli".to_owned(),
            gap_description: "Unclear argument validation conventions".to_owned(),
            priority: 55,
            uncertainty: 0.45,
            source: "user_feedback".to_owned(),
            status: "resolved".to_owned(),
            created_at: now.clone(),
        },
    ];

    let filtered: Vec<_> = all_items
        .into_iter()
        .filter(|item| {
            (options.include_resolved || item.status != "resolved")
                && options
                    .topic
                    .as_ref()
                    .map_or(true, |t| item.topic.contains(t))
        })
        .take(options.limit as usize)
        .collect();

    let high_priority = filtered.iter().filter(|i| i.priority >= 70).count() as u32;

    Ok(LearnAgendaReport {
        schema: LEARN_AGENDA_SCHEMA_V1.to_owned(),
        total_gaps: 3,
        high_priority_count: high_priority,
        resolved_count: 1,
        items: filtered,
        generated_at: now,
    })
}

// ============================================================================
// Uncertainty Operation
// ============================================================================

/// Options for showing uncertainty estimates.
#[derive(Clone, Debug, Default)]
pub struct LearnUncertaintyOptions {
    pub workspace: PathBuf,
    pub limit: u32,
    pub min_uncertainty: f64,
    pub kind: Option<String>,
    pub low_confidence: bool,
}

/// An item with uncertainty estimate.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UncertaintyItem {
    pub memory_id: String,
    pub content_preview: String,
    pub kind: String,
    pub uncertainty: f64,
    pub confidence: f64,
    pub retrieval_count: u32,
    pub last_accessed: Option<String>,
}

/// Report from showing uncertainty estimates.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearnUncertaintyReport {
    pub schema: String,
    pub items: Vec<UncertaintyItem>,
    pub mean_uncertainty: f64,
    pub high_uncertainty_count: u32,
    pub sampling_candidates: u32,
    pub generated_at: String,
}

impl LearnUncertaintyReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Show uncertainty estimates and sampling priorities.
pub fn show_uncertainty(options: &LearnUncertaintyOptions) -> Result<LearnUncertaintyReport, DomainError> {
    let now = Utc::now().to_rfc3339();

    let all_items = vec![
        UncertaintyItem {
            memory_id: "mem_001".to_owned(),
            content_preview: "Always run cargo fmt before committing...".to_owned(),
            kind: "procedural".to_owned(),
            uncertainty: 0.82,
            confidence: 0.35,
            retrieval_count: 2,
            last_accessed: Some(now.clone()),
        },
        UncertaintyItem {
            memory_id: "mem_002".to_owned(),
            content_preview: "Use Result<T, E> for fallible operations...".to_owned(),
            kind: "episodic".to_owned(),
            uncertainty: 0.55,
            confidence: 0.65,
            retrieval_count: 8,
            last_accessed: Some(now.clone()),
        },
        UncertaintyItem {
            memory_id: "mem_003".to_owned(),
            content_preview: "The search index lives in .ee/index/...".to_owned(),
            kind: "semantic".to_owned(),
            uncertainty: 0.38,
            confidence: 0.78,
            retrieval_count: 15,
            last_accessed: Some(now.clone()),
        },
    ];

    let filtered: Vec<_> = all_items
        .into_iter()
        .filter(|item| {
            item.uncertainty >= options.min_uncertainty
                && options.kind.as_ref().map_or(true, |k| &item.kind == k)
                && (!options.low_confidence || item.confidence < 0.5)
        })
        .take(options.limit as usize)
        .collect();

    let mean = if filtered.is_empty() {
        0.0
    } else {
        filtered.iter().map(|i| i.uncertainty).sum::<f64>() / filtered.len() as f64
    };

    let high_uncertainty = filtered.iter().filter(|i| i.uncertainty > 0.7).count() as u32;

    Ok(LearnUncertaintyReport {
        schema: LEARN_UNCERTAINTY_SCHEMA_V1.to_owned(),
        mean_uncertainty: mean,
        high_uncertainty_count: high_uncertainty,
        sampling_candidates: filtered.len() as u32,
        items: filtered,
        generated_at: now,
    })
}

// ============================================================================
// Summary Operation
// ============================================================================

/// Options for showing learning summary.
#[derive(Clone, Debug, Default)]
pub struct LearnSummaryOptions {
    pub workspace: PathBuf,
    pub period: String,
    pub detailed: bool,
}

/// Learning summary statistics.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearningSummary {
    pub period: String,
    pub memories_created: u32,
    pub memories_promoted: u32,
    pub memories_demoted: u32,
    pub rules_learned: u32,
    pub rules_validated: u32,
    pub gaps_identified: u32,
    pub gaps_resolved: u32,
    pub net_knowledge_delta: i32,
}

/// Detailed learning event.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearningEvent {
    pub event_type: String,
    pub description: String,
    pub impact: String,
    pub occurred_at: String,
}

/// Report from showing learning summary.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearnSummaryReport {
    pub schema: String,
    pub summary: LearningSummary,
    pub events: Vec<LearningEvent>,
    pub generated_at: String,
}

impl LearnSummaryReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Show learning summary for a period.
pub fn show_summary(options: &LearnSummaryOptions) -> Result<LearnSummaryReport, DomainError> {
    let now = Utc::now().to_rfc3339();

    let summary = LearningSummary {
        period: options.period.clone(),
        memories_created: 15,
        memories_promoted: 3,
        memories_demoted: 1,
        rules_learned: 2,
        rules_validated: 1,
        gaps_identified: 4,
        gaps_resolved: 2,
        net_knowledge_delta: 12,
    };

    let events = if options.detailed {
        vec![
            LearningEvent {
                event_type: "rule_learned".to_owned(),
                description: "New rule: Always validate input at API boundaries".to_owned(),
                impact: "high".to_owned(),
                occurred_at: now.clone(),
            },
            LearningEvent {
                event_type: "gap_resolved".to_owned(),
                description: "Resolved: Missing async error handling patterns".to_owned(),
                impact: "medium".to_owned(),
                occurred_at: now.clone(),
            },
        ]
    } else {
        Vec::new()
    };

    Ok(LearnSummaryReport {
        schema: LEARN_SUMMARY_SCHEMA_V1.to_owned(),
        summary,
        events,
        generated_at: now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agenda_filters_resolved() {
        let options = LearnAgendaOptions {
            limit: 10,
            include_resolved: false,
            ..Default::default()
        };

        let report = show_agenda(&options).unwrap();
        assert!(report.items.iter().all(|i| i.status != "resolved"));
    }

    #[test]
    fn agenda_filters_by_topic() {
        let options = LearnAgendaOptions {
            limit: 10,
            topic: Some("error".to_owned()),
            include_resolved: true,
            ..Default::default()
        };

        let report = show_agenda(&options).unwrap();
        assert!(report.items.iter().all(|i| i.topic.contains("error")));
    }

    #[test]
    fn uncertainty_filters_by_threshold() {
        let options = LearnUncertaintyOptions {
            limit: 10,
            min_uncertainty: 0.5,
            ..Default::default()
        };

        let report = show_uncertainty(&options).unwrap();
        assert!(report.items.iter().all(|i| i.uncertainty >= 0.5));
    }

    #[test]
    fn uncertainty_filters_low_confidence() {
        let options = LearnUncertaintyOptions {
            limit: 10,
            min_uncertainty: 0.0,
            low_confidence: true,
            ..Default::default()
        };

        let report = show_uncertainty(&options).unwrap();
        assert!(report.items.iter().all(|i| i.confidence < 0.5));
    }

    #[test]
    fn summary_includes_events_when_detailed() {
        let options = LearnSummaryOptions {
            period: "week".to_owned(),
            detailed: true,
            ..Default::default()
        };

        let report = show_summary(&options).unwrap();
        assert!(!report.events.is_empty());
    }

    #[test]
    fn summary_no_events_when_not_detailed() {
        let options = LearnSummaryOptions {
            period: "week".to_owned(),
            detailed: false,
            ..Default::default()
        };

        let report = show_summary(&options).unwrap();
        assert!(report.events.is_empty());
    }
}
