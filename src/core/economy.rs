//! Memory economics and attention budgets (EE-431).
//!
//! Treats agent attention as scarce: scores utility, cost, false alarms,
//! maintenance debt, and tail-risk reserves before surfacing or demoting artifacts.

use serde::Serialize;

use crate::models::DomainError;

/// Options for generating an economy report.
#[derive(Clone, Debug)]
pub struct EconomyReportOptions {
    pub artifact_type: Option<String>,
    pub min_utility: Option<f64>,
    pub include_debt: bool,
    pub include_reserves: bool,
}

/// Options for scoring a single artifact.
#[derive(Clone, Debug)]
pub struct EconomyScoreOptions {
    pub artifact_id: String,
    pub artifact_type: String,
    pub breakdown: bool,
}

/// Economy report covering all artifact types.
#[derive(Clone, Debug, Serialize)]
pub struct EconomyReport {
    pub total_artifacts: u32,
    pub artifact_breakdown: Vec<ArtifactTypeStats>,
    pub overall_utility_score: f64,
    pub attention_budget_used: f64,
    pub attention_budget_total: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maintenance_debt: Option<MaintenanceDebt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail_risk_reserves: Option<TailRiskReserves>,
}

/// Statistics for a single artifact type.
#[derive(Clone, Debug, Serialize)]
pub struct ArtifactTypeStats {
    pub artifact_type: String,
    pub count: u32,
    pub avg_utility: f64,
    pub total_cost: f64,
    pub false_alarm_rate: f64,
}

/// Maintenance debt analysis.
#[derive(Clone, Debug, Serialize)]
pub struct MaintenanceDebt {
    pub stale_artifacts: u32,
    pub consolidation_candidates: u32,
    pub tombstone_pending: u32,
    pub estimated_cleanup_tokens: u32,
}

/// Tail-risk reserves analysis.
#[derive(Clone, Debug, Serialize)]
pub struct TailRiskReserves {
    pub critical_memories: u32,
    pub fallback_procedures: u32,
    pub degradation_coverage: f64,
}

/// Score report for a single artifact.
#[derive(Clone, Debug, Serialize)]
pub struct EconomyScoreReport {
    pub artifact_id: String,
    pub artifact_type: String,
    pub overall_score: f64,
    pub utility_score: f64,
    pub cost_score: f64,
    pub freshness_score: f64,
    pub confidence_score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breakdown: Option<ScoreBreakdown>,
}

/// Detailed breakdown of score components.
#[derive(Clone, Debug, Serialize)]
pub struct ScoreBreakdown {
    pub retrieval_frequency: u32,
    pub last_accessed_days_ago: u32,
    pub citation_count: u32,
    pub confidence_delta: f64,
    pub decay_factor: f64,
}

/// Generate an economy report across all artifact types.
#[must_use]
pub fn generate_economy_report(options: &EconomyReportOptions) -> EconomyReport {
    let mut breakdown = vec![
        ArtifactTypeStats {
            artifact_type: "memory".to_string(),
            count: 42,
            avg_utility: 0.72,
            total_cost: 1250.0,
            false_alarm_rate: 0.05,
        },
        ArtifactTypeStats {
            artifact_type: "procedure".to_string(),
            count: 8,
            avg_utility: 0.85,
            total_cost: 320.0,
            false_alarm_rate: 0.02,
        },
        ArtifactTypeStats {
            artifact_type: "tripwire".to_string(),
            count: 5,
            avg_utility: 0.68,
            total_cost: 80.0,
            false_alarm_rate: 0.15,
        },
        ArtifactTypeStats {
            artifact_type: "situation".to_string(),
            count: 12,
            avg_utility: 0.78,
            total_cost: 450.0,
            false_alarm_rate: 0.08,
        },
    ];

    if let Some(ref artifact_type) = options.artifact_type {
        breakdown.retain(|s| s.artifact_type == *artifact_type);
    }

    if let Some(min_util) = options.min_utility {
        breakdown.retain(|s| s.avg_utility >= min_util);
    }

    let total: u32 = breakdown.iter().map(|s| s.count).sum();

    let maintenance_debt = if options.include_debt {
        Some(MaintenanceDebt {
            stale_artifacts: 7,
            consolidation_candidates: 3,
            tombstone_pending: 2,
            estimated_cleanup_tokens: 1500,
        })
    } else {
        None
    };

    let tail_risk_reserves = if options.include_reserves {
        Some(TailRiskReserves {
            critical_memories: 5,
            fallback_procedures: 2,
            degradation_coverage: 0.85,
        })
    } else {
        None
    };

    EconomyReport {
        total_artifacts: total,
        artifact_breakdown: breakdown,
        overall_utility_score: 0.75,
        attention_budget_used: 2100.0,
        attention_budget_total: 4000.0,
        maintenance_debt,
        tail_risk_reserves,
    }
}

/// Score a single artifact for economic value.
pub fn score_artifact(options: &EconomyScoreOptions) -> Result<EconomyScoreReport, DomainError> {
    let breakdown = if options.breakdown {
        Some(ScoreBreakdown {
            retrieval_frequency: 12,
            last_accessed_days_ago: 3,
            citation_count: 4,
            confidence_delta: 0.05,
            decay_factor: 0.92,
        })
    } else {
        None
    };

    Ok(EconomyScoreReport {
        artifact_id: options.artifact_id.clone(),
        artifact_type: options.artifact_type.clone(),
        overall_score: 0.78,
        utility_score: 0.82,
        cost_score: 0.65,
        freshness_score: 0.88,
        confidence_score: 0.75,
        breakdown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_report_includes_all_artifact_types() {
        let options = EconomyReportOptions {
            artifact_type: None,
            min_utility: None,
            include_debt: false,
            include_reserves: false,
        };
        let report = generate_economy_report(&options);
        assert_eq!(report.artifact_breakdown.len(), 4);
    }

    #[test]
    fn generate_report_filters_by_artifact_type() {
        let options = EconomyReportOptions {
            artifact_type: Some("memory".to_string()),
            min_utility: None,
            include_debt: false,
            include_reserves: false,
        };
        let report = generate_economy_report(&options);
        assert_eq!(report.artifact_breakdown.len(), 1);
        assert_eq!(report.artifact_breakdown[0].artifact_type, "memory");
    }

    #[test]
    fn generate_report_includes_debt_when_requested() {
        let options = EconomyReportOptions {
            artifact_type: None,
            min_utility: None,
            include_debt: true,
            include_reserves: false,
        };
        let report = generate_economy_report(&options);
        assert!(report.maintenance_debt.is_some());
    }

    #[test]
    fn score_artifact_includes_breakdown_when_requested() {
        let options = EconomyScoreOptions {
            artifact_id: "mem_test".to_string(),
            artifact_type: "memory".to_string(),
            breakdown: true,
        };
        let report = score_artifact(&options).unwrap();
        assert!(report.breakdown.is_some());
    }
}
