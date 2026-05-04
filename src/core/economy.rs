//! Memory economics and attention budgets (EE-431).
//!
//! Treats agent attention as scarce: scores utility, cost, false alarms,
//! maintenance debt, and tail-risk reserves before surfacing or demoting artifacts.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::db::{DbConnection, StoredFeedbackEvent, StoredMemory};
use crate::models::DomainError;
use crate::models::WorkspaceId;
use crate::models::economy::{
    AttentionBudgetAllocation, AttentionBudgetRequest, ContextAttentionProfile,
    ECONOMY_REPORT_SCHEMA_V1, ECONOMY_SIMULATION_SCHEMA_V1, SituationAttentionProfile,
};

/// Schema for economy prune plans.
pub const ECONOMY_PRUNE_PLAN_SCHEMA_V1: &str = "ee.economy.prune_plan.v1";

/// Options for generating an economy report.
#[derive(Clone, Debug)]
pub struct EconomyReportOptions {
    pub workspace_path: PathBuf,
    pub database_path: PathBuf,
    pub artifact_type: Option<String>,
    pub min_utility: Option<f64>,
    pub include_debt: bool,
    pub include_reserves: bool,
}

/// Options for scoring a single artifact.
#[derive(Clone, Debug)]
pub struct EconomyScoreOptions {
    pub workspace_path: PathBuf,
    pub database_path: PathBuf,
    pub artifact_id: String,
    pub artifact_type: String,
    pub breakdown: bool,
}

/// Options for generating a report-only prune plan.
#[derive(Clone, Debug)]
pub struct EconomyPrunePlanOptions {
    pub workspace_path: PathBuf,
    pub database_path: PathBuf,
    pub dry_run: bool,
    pub max_recommendations: usize,
}

/// Options for comparing alternate attention budgets without mutating ranking state.
#[derive(Clone, Debug)]
pub struct EconomySimulateOptions {
    pub workspace_path: PathBuf,
    pub database_path: PathBuf,
    pub baseline_budget_tokens: u32,
    pub budget_tokens: Vec<u32>,
    pub context_profile: String,
    pub situation_profile: String,
}

/// Economy report covering all artifact types.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EconomyReport {
    pub schema: &'static str,
    pub generated_at: String,
    pub status: String,
    pub mutation_status: String,
    pub workspace_id: String,
    pub database_path: String,
    pub total_artifacts: u32,
    pub artifact_breakdown: Vec<ArtifactTypeStats>,
    pub overall_utility_score: f64,
    pub attention_budget_used: f64,
    pub attention_budget_total: f64,
    pub scored_artifact_ids: Vec<String>,
    pub formula_components: Vec<String>,
    pub degraded: Vec<EconomyDegradation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maintenance_debt: Option<MaintenanceDebt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail_risk_reserves: Option<TailRiskReserves>,
}

/// Stable degraded-state metadata for conservative economy abstention.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EconomyDegradation {
    pub code: String,
    pub severity: String,
    pub message: String,
    pub repair: String,
}

/// Statistics for a single artifact type.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactTypeStats {
    pub artifact_type: String,
    pub count: u32,
    pub avg_utility: f64,
    pub total_cost: f64,
    pub false_alarm_rate: f64,
}

/// Maintenance debt analysis.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaintenanceDebt {
    pub stale_artifacts: u32,
    pub consolidation_candidates: u32,
    pub tombstone_pending: u32,
    pub estimated_cleanup_tokens: u32,
}

/// Tail-risk reserves analysis.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TailRiskReserves {
    pub critical_memories: u32,
    pub fallback_procedures: u32,
    pub degradation_coverage: f64,
}

/// Score report for a single artifact.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EconomyScoreReport {
    pub artifact_id: String,
    pub artifact_type: String,
    pub status: String,
    pub mutation_status: String,
    pub overall_score: f64,
    pub utility_score: f64,
    pub cost_score: f64,
    pub freshness_score: f64,
    pub confidence_score: f64,
    pub false_alarm_rate: f64,
    pub maintenance_debt: f64,
    pub tail_risk_protected: bool,
    pub degraded: Vec<EconomyDegradation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breakdown: Option<ScoreBreakdown>,
}

/// Detailed breakdown of score components.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreBreakdown {
    pub retrieval_frequency: u32,
    pub last_accessed_days_ago: u32,
    pub citation_count: u32,
    pub confidence_delta: f64,
    pub decay_factor: f64,
    pub formula: String,
}

/// Report-only plan for reducing memory-economy maintenance debt.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EconomyPrunePlan {
    pub schema: &'static str,
    pub generated_at: String,
    pub dry_run: bool,
    pub read_only: bool,
    pub status: String,
    pub mutation_status: String,
    pub summary: EconomyPrunePlanSummary,
    pub recommendations: Vec<EconomyPruneRecommendation>,
    pub degraded: Vec<EconomyDegradation>,
}

/// Aggregate summary for an economy prune plan.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EconomyPrunePlanSummary {
    pub recommendation_count: usize,
    pub total_candidates: u32,
    pub estimated_token_savings: u32,
    pub actions: Vec<String>,
}

/// A single report-only prune recommendation.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EconomyPruneRecommendation {
    pub id: String,
    pub action: String,
    pub artifact_type: String,
    pub candidate_count: u32,
    pub priority: u8,
    pub risk: String,
    pub rationale: String,
    pub estimated_token_savings: u32,
    pub dry_run_command: String,
}

/// Report-only comparison of alternate attention budgets.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EconomySimulationReport {
    pub schema: &'static str,
    pub generated_at: String,
    pub dry_run: bool,
    pub read_only: bool,
    pub status: String,
    pub mutation_status: String,
    pub baseline_budget_tokens: u32,
    pub context_profile: String,
    pub situation_profile: String,
    pub ranking_state_hash_before: String,
    pub ranking_state_hash_after: String,
    pub ranking_state_unchanged: bool,
    pub scored_artifact_ids: Vec<String>,
    pub degraded: Vec<EconomyDegradation>,
    pub summary: EconomySimulationSummary,
    pub scenarios: Vec<EconomySimulationScenario>,
    pub explanations: Vec<String>,
}

/// Aggregate simulation outcome across all compared budgets.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EconomySimulationSummary {
    pub scenario_count: usize,
    pub best_budget_tokens: u32,
    pub recommended_budget_tokens: u32,
    pub baseline_score: f64,
    pub best_score: f64,
    pub score_delta_vs_baseline: f64,
    pub baseline_rank: usize,
    pub stable_top_artifact: bool,
    pub no_mutation_evidence: Vec<String>,
}

/// One simulated budget scenario.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EconomySimulationScenario {
    pub budget_tokens: u32,
    pub score: f64,
    pub score_delta_vs_baseline: f64,
    pub surfaced_count: usize,
    pub estimated_utility: f64,
    pub estimated_attention_cost: f64,
    pub false_alarm_cost: f64,
    pub maintenance_debt_cost: f64,
    pub budget: EconomySimulationBudget,
    pub ranking: Vec<EconomySimulatedArtifact>,
}

/// Stable subset of attention-budget allocation used by the simulation output.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EconomySimulationBudget {
    pub total_tokens: u32,
    pub retrieval_tokens: u32,
    pub evidence_tokens: u32,
    pub procedure_tokens: u32,
    pub risk_reserve_tokens: u32,
    pub maintenance_tokens: u32,
    pub reserve_ratio: f64,
    pub max_items: u32,
    pub reasons: Vec<String>,
}

/// Simulated artifact ranking under a single budget.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EconomySimulatedArtifact {
    pub rank: usize,
    pub artifact_id: String,
    pub artifact_type: String,
    pub included: bool,
    pub score: f64,
    pub score_delta_vs_baseline: f64,
    pub token_cost: u32,
    pub utility_score: f64,
    pub false_alarm_rate: f64,
    pub maintenance_debt: f64,
    pub tail_risk_protected: bool,
    pub rationale: String,
}

/// Generate an economy report across persisted memory artifacts.
pub fn generate_economy_report(
    options: &EconomyReportOptions,
) -> Result<EconomyReport, DomainError> {
    let metrics = load_memory_economy_metrics(&options.workspace_path, &options.database_path)?;
    let mut artifacts = metrics.active_artifacts.clone();

    if let Some(ref artifact_type) = options.artifact_type {
        if artifact_type != "memory" {
            artifacts.clear();
        }
    }

    if let Some(min_util) = options.min_utility {
        artifacts.retain(|artifact| artifact.utility_score >= min_util);
    }

    let total = u32::try_from(artifacts.len()).unwrap_or(u32::MAX);
    let artifact_breakdown = if artifacts.is_empty() {
        Vec::new()
    } else {
        vec![ArtifactTypeStats {
            artifact_type: "memory".to_owned(),
            count: total,
            avg_utility: round_metric(average_by(&artifacts, |artifact| artifact.utility_score)),
            total_cost: round_metric(
                artifacts
                    .iter()
                    .map(|artifact| f64::from(artifact.token_cost))
                    .sum::<f64>(),
            ),
            false_alarm_rate: round_metric(average_by(&artifacts, |artifact| {
                artifact.false_alarm_rate
            })),
        }]
    };
    let degraded = report_degradations(&artifacts, &metrics, options.artifact_type.as_deref());
    let status = economy_status(&artifacts, &degraded);
    let attention_budget_used = round_metric(
        artifacts
            .iter()
            .map(|artifact| f64::from(artifact.token_cost))
            .sum::<f64>(),
    );

    Ok(EconomyReport {
        schema: ECONOMY_REPORT_SCHEMA_V1,
        generated_at: Utc::now().to_rfc3339(),
        status,
        mutation_status: "read_only_no_mutation".to_owned(),
        workspace_id: metrics.workspace_id.clone(),
        database_path: options.database_path.display().to_string(),
        total_artifacts: total,
        artifact_breakdown,
        overall_utility_score: round_metric(average_by(&artifacts, |artifact| {
            artifact.utility_score
        })),
        attention_budget_used,
        attention_budget_total: 4_000.0,
        scored_artifact_ids: artifacts
            .iter()
            .map(|artifact| artifact.artifact_id.clone())
            .collect(),
        formula_components: economy_formula_components(),
        degraded,
        maintenance_debt: options
            .include_debt
            .then(|| maintenance_debt_from_metrics(&metrics)),
        tail_risk_reserves: options
            .include_reserves
            .then(|| tail_risk_reserves_from_metrics(&metrics)),
    })
}

/// Score a single persisted artifact for economic value.
pub fn score_artifact(options: &EconomyScoreOptions) -> Result<EconomyScoreReport, DomainError> {
    if options.artifact_type != "memory" {
        return Err(DomainError::UnsatisfiedDegradedMode {
            message: format!(
                "economy scoring for `{}` artifacts is unavailable until persisted rows exist for that artifact type",
                options.artifact_type
            ),
            repair: Some("ee economy report --json".to_owned()),
        });
    }

    let metrics = load_memory_economy_metrics(&options.workspace_path, &options.database_path)?;
    let artifact = metrics
        .active_artifacts
        .iter()
        .find(|artifact| artifact.artifact_id == options.artifact_id)
        .ok_or_else(|| DomainError::NotFound {
            resource: "memory economy artifact".to_owned(),
            id: options.artifact_id.clone(),
            repair: Some("ee memory list --json".to_owned()),
        })?;

    Ok(EconomyScoreReport {
        artifact_id: artifact.artifact_id.clone(),
        artifact_type: artifact.artifact_type.clone(),
        status: if artifact.is_sparse() {
            "review".to_owned()
        } else {
            "available".to_owned()
        },
        mutation_status: "read_only_no_mutation".to_owned(),
        overall_score: artifact.score,
        utility_score: artifact.utility_score,
        cost_score: artifact.cost_score,
        freshness_score: artifact.freshness_score,
        confidence_score: artifact.confidence_score,
        false_alarm_rate: artifact.false_alarm_rate,
        maintenance_debt: artifact.maintenance_debt,
        tail_risk_protected: artifact.tail_risk_protected,
        degraded: artifact_degradations(artifact),
        breakdown: options.breakdown.then(|| ScoreBreakdown {
            retrieval_frequency: artifact.retrieval_frequency,
            last_accessed_days_ago: artifact.last_accessed_days_ago,
            citation_count: artifact.citation_count,
            confidence_delta: artifact.confidence_delta,
            decay_factor: artifact.decay_factor,
            formula: ECONOMY_SCORE_FORMULA.to_owned(),
        }),
    })
}

/// Generate a report-only prune plan for economy maintenance.
pub fn generate_prune_plan(
    options: &EconomyPrunePlanOptions,
) -> Result<EconomyPrunePlan, DomainError> {
    if !options.dry_run {
        return Err(DomainError::PolicyDenied {
            message: "economy prune-plan is report-only in this slice; pass --dry-run to confirm no mutation".to_owned(),
            repair: Some("ee economy prune-plan --dry-run --json".to_owned()),
        });
    }

    if options.max_recommendations == 0 {
        return Err(DomainError::Usage {
            message: "max recommendations must be greater than zero".to_owned(),
            repair: Some(
                "ee economy prune-plan --dry-run --max-recommendations 5 --json".to_owned(),
            ),
        });
    }

    let metrics = load_memory_economy_metrics(&options.workspace_path, &options.database_path)?;
    let mut recommendations = prune_recommendations_from_metrics(&metrics.active_artifacts);
    recommendations.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.id.cmp(&right.id))
    });
    recommendations.truncate(options.max_recommendations);

    let summary = EconomyPrunePlanSummary {
        recommendation_count: recommendations.len(),
        total_candidates: recommendations
            .iter()
            .map(|recommendation| recommendation.candidate_count)
            .sum(),
        estimated_token_savings: recommendations
            .iter()
            .map(|recommendation| recommendation.estimated_token_savings)
            .sum(),
        actions: recommendations
            .iter()
            .map(|recommendation| recommendation.action.clone())
            .collect(),
    };
    let degraded = if recommendations.is_empty() {
        vec![economy_degradation(
            "economy_no_prune_candidates",
            "low",
            "No persisted memory rows currently meet report-only prune criteria.",
            "ee economy report --include-debt --include-reserves --json",
        )]
    } else {
        sparse_degradations(&metrics.active_artifacts)
    };

    Ok(EconomyPrunePlan {
        schema: ECONOMY_PRUNE_PLAN_SCHEMA_V1,
        generated_at: Utc::now().to_rfc3339(),
        dry_run: true,
        read_only: true,
        status: economy_status(&metrics.active_artifacts, &degraded),
        mutation_status: "read_only_no_mutation".to_owned(),
        summary,
        recommendations,
        degraded,
    })
}

/// Compare alternate attention budgets without changing ranking state.
pub fn simulate_budgets(
    options: &EconomySimulateOptions,
) -> Result<EconomySimulationReport, DomainError> {
    if options.baseline_budget_tokens == 0 {
        return Err(DomainError::Usage {
            message: "baseline budget must be greater than zero".to_owned(),
            repair: Some(
                "ee economy simulate --baseline-budget 4000 --budget 2000 --budget 8000 --json"
                    .to_owned(),
            ),
        });
    }

    let context_profile =
        ContextAttentionProfile::parse(&options.context_profile).ok_or_else(|| {
            DomainError::Usage {
                message: format!(
                    "unknown context profile '{}'",
                    options.context_profile.trim()
                ),
                repair: Some(
                    "use one of: compact, balanced, thorough, submodular, broad".to_owned(),
                ),
            }
        })?;
    let situation_profile = SituationAttentionProfile::parse(&options.situation_profile)
        .ok_or_else(|| DomainError::Usage {
            message: format!(
                "unknown situation profile '{}'",
                options.situation_profile.trim()
            ),
            repair: Some("use one of: minimal, summary, standard, full".to_owned()),
        })?;

    let metrics = load_memory_economy_metrics(&options.workspace_path, &options.database_path)?;
    let budget_tokens = normalize_simulation_budgets(
        options.baseline_budget_tokens,
        options.budget_tokens.as_slice(),
    )?;
    let ranking_state_hash = economy_ranking_state_hash(&metrics.active_artifacts);
    let mut scenarios = budget_tokens
        .iter()
        .map(|budget| {
            simulate_budget_scenario(
                *budget,
                context_profile,
                situation_profile,
                &metrics.active_artifacts,
            )
        })
        .collect::<Vec<_>>();

    let baseline_score = scenarios
        .iter()
        .find(|scenario| scenario.budget_tokens == options.baseline_budget_tokens)
        .map_or(0.0, |scenario| scenario.score);
    let baseline_artifact_scores = scenarios
        .iter()
        .find(|scenario| scenario.budget_tokens == options.baseline_budget_tokens)
        .map(|scenario| {
            scenario
                .ranking
                .iter()
                .map(|artifact| (artifact.artifact_id.clone(), artifact.score))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    for scenario in &mut scenarios {
        scenario.score_delta_vs_baseline = round_metric(scenario.score - baseline_score);
        for artifact in &mut scenario.ranking {
            artifact.score_delta_vs_baseline = round_metric(
                artifact.score
                    - baseline_artifact_score(&baseline_artifact_scores, &artifact.artifact_id),
            );
        }
    }

    let summary = simulation_summary(&scenarios, options.baseline_budget_tokens, baseline_score);

    Ok(EconomySimulationReport {
        schema: ECONOMY_SIMULATION_SCHEMA_V1,
        generated_at: Utc::now().to_rfc3339(),
        dry_run: true,
        read_only: true,
        status: economy_status(&metrics.active_artifacts, &sparse_degradations(&metrics.active_artifacts)),
        mutation_status: "not_applied".to_owned(),
        baseline_budget_tokens: options.baseline_budget_tokens,
        context_profile: context_profile.as_str().to_owned(),
        situation_profile: situation_profile.as_str().to_owned(),
        ranking_state_hash_before: ranking_state_hash.clone(),
        ranking_state_hash_after: ranking_state_hash,
        ranking_state_unchanged: true,
        scored_artifact_ids: metrics
            .active_artifacts
            .iter()
            .map(|artifact| artifact.artifact_id.clone())
            .collect(),
        degraded: sparse_degradations(&metrics.active_artifacts),
        summary,
        scenarios,
        explanations: vec![
            "simulation uses persisted memory rows and feedback events without writing DB, index, graph, or ranking records".to_owned(),
            "rankingStateHashBefore and rankingStateHashAfter are identical when no durable ranking state changed".to_owned(),
            "scenario scores include utility, attention cost, false-alarm cost, maintenance debt, and tail-risk reserve coverage".to_owned(),
        ],
    })
}

fn normalize_simulation_budgets(
    baseline_budget_tokens: u32,
    budget_tokens: &[u32],
) -> Result<Vec<u32>, DomainError> {
    let mut budgets = if budget_tokens.is_empty() {
        vec![2_000, baseline_budget_tokens, 8_000]
    } else {
        let mut budgets = budget_tokens.to_vec();
        budgets.push(baseline_budget_tokens);
        budgets
    };

    if budgets.contains(&0) {
        return Err(DomainError::Usage {
            message: "simulation budgets must be greater than zero".to_owned(),
            repair: Some("ee economy simulate --budget 2000 --budget 4000 --json".to_owned()),
        });
    }

    budgets.sort_unstable();
    budgets.dedup();
    Ok(budgets)
}

fn simulate_budget_scenario(
    budget_tokens: u32,
    context_profile: ContextAttentionProfile,
    situation_profile: SituationAttentionProfile,
    artifacts: &[EconomyArtifactMetric],
) -> EconomySimulationScenario {
    let allocation = AttentionBudgetAllocation::calculate(AttentionBudgetRequest::new(
        budget_tokens,
        context_profile,
        situation_profile,
    ));
    let surfaced_limit = allocation.max_items as usize;
    let mut ranking = artifacts
        .iter()
        .map(|artifact| score_artifact_for_budget(artifact, &allocation))
        .collect::<Vec<_>>();

    ranking.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.artifact_id.cmp(&right.artifact_id))
    });

    for (index, artifact) in ranking.iter_mut().enumerate() {
        artifact.rank = index + 1;
        artifact.included = index < surfaced_limit;
    }

    let included = ranking
        .iter()
        .filter(|artifact| artifact.included)
        .collect::<Vec<_>>();
    let surfaced_count = included.len();
    let denominator = surfaced_count.max(1) as f64;
    let estimated_utility = round_metric(
        included
            .iter()
            .map(|artifact| artifact.utility_score)
            .sum::<f64>()
            / denominator,
    );
    let false_alarm_cost = round_metric(
        included
            .iter()
            .map(|artifact| artifact.false_alarm_rate)
            .sum::<f64>()
            / denominator,
    );
    let maintenance_debt_cost = round_metric(
        included
            .iter()
            .map(|artifact| artifact.maintenance_debt)
            .sum::<f64>()
            / denominator,
    );
    let estimated_attention_cost = round_metric(allocation.attention_cost.total_cost());
    let score = round_metric(
        estimated_utility - false_alarm_cost.mul_add(0.25, maintenance_debt_cost * 0.20)
            + allocation.reserve_ratio() * 0.12
            - estimated_attention_cost.min(12.0) * 0.01,
    );

    EconomySimulationScenario {
        budget_tokens,
        score,
        score_delta_vs_baseline: 0.0,
        surfaced_count,
        estimated_utility,
        estimated_attention_cost,
        false_alarm_cost,
        maintenance_debt_cost,
        budget: simulation_budget(&allocation),
        ranking,
    }
}

fn score_artifact_for_budget(
    artifact: &EconomyArtifactMetric,
    allocation: &AttentionBudgetAllocation,
) -> EconomySimulatedArtifact {
    let per_item_budget = f64::from(
        allocation
            .retrieval_tokens
            .saturating_add(allocation.evidence_tokens)
            .saturating_add(allocation.procedure_tokens),
    ) / f64::from(allocation.max_items.max(1));
    let fit = (per_item_budget / f64::from(artifact.token_cost.max(1))).min(1.0);
    let reserve_boost = if artifact.tail_risk_protected {
        allocation.reserve_ratio() * 0.35
    } else {
        0.0
    };
    let over_budget_penalty = if f64::from(artifact.token_cost) > per_item_budget {
        ((f64::from(artifact.token_cost) - per_item_budget) / 1_000.0).min(0.30)
    } else {
        0.0
    };
    let score = round_metric(
        artifact.utility_score * 0.42
            + artifact.confidence_score * 0.18
            + artifact.freshness_score * 0.14
            + fit * 0.16
            + reserve_boost
            - artifact.false_alarm_rate * 0.22
            - artifact.maintenance_debt * 0.14
            - over_budget_penalty,
    );

    EconomySimulatedArtifact {
        rank: 0,
        artifact_id: artifact.artifact_id.clone(),
        artifact_type: artifact.artifact_type.clone(),
        included: false,
        score,
        score_delta_vs_baseline: 0.0,
        token_cost: artifact.token_cost,
        utility_score: artifact.utility_score,
        false_alarm_rate: artifact.false_alarm_rate,
        maintenance_debt: artifact.maintenance_debt,
        tail_risk_protected: artifact.tail_risk_protected,
        rationale: artifact.rationale.clone(),
    }
}

fn simulation_budget(allocation: &AttentionBudgetAllocation) -> EconomySimulationBudget {
    EconomySimulationBudget {
        total_tokens: allocation.total_tokens,
        retrieval_tokens: allocation.retrieval_tokens,
        evidence_tokens: allocation.evidence_tokens,
        procedure_tokens: allocation.procedure_tokens,
        risk_reserve_tokens: allocation.risk_reserve_tokens,
        maintenance_tokens: allocation.maintenance_tokens,
        reserve_ratio: round_metric(allocation.reserve_ratio()),
        max_items: allocation.max_items,
        reasons: allocation.reasons.clone(),
    }
}

fn simulation_summary(
    scenarios: &[EconomySimulationScenario],
    baseline_budget_tokens: u32,
    baseline_score: f64,
) -> EconomySimulationSummary {
    let best = scenarios.iter().max_by(|left, right| {
        left.score
            .total_cmp(&right.score)
            .then_with(|| right.budget_tokens.cmp(&left.budget_tokens))
    });
    let best_budget_tokens = best.map_or(baseline_budget_tokens, |scenario| scenario.budget_tokens);
    let best_score = best.map_or(baseline_score, |scenario| scenario.score);
    let baseline_rank = 1 + scenarios
        .iter()
        .filter(|scenario| {
            scenario.score > baseline_score + f64::EPSILON
                || ((scenario.score - baseline_score).abs() <= f64::EPSILON
                    && scenario.budget_tokens < baseline_budget_tokens)
        })
        .count();
    let baseline_top = scenarios
        .iter()
        .find(|scenario| scenario.budget_tokens == baseline_budget_tokens)
        .and_then(|scenario| scenario.ranking.first())
        .map(|artifact| artifact.artifact_id.as_str());
    let stable_top_artifact = baseline_top.is_some_and(|artifact_id| {
        scenarios.iter().all(|scenario| {
            scenario
                .ranking
                .first()
                .is_some_and(|artifact| artifact.artifact_id.as_str() == artifact_id)
        })
    });

    EconomySimulationSummary {
        scenario_count: scenarios.len(),
        best_budget_tokens,
        recommended_budget_tokens: best_budget_tokens,
        baseline_score: round_metric(baseline_score),
        best_score: round_metric(best_score),
        score_delta_vs_baseline: round_metric(best_score - baseline_score),
        baseline_rank,
        stable_top_artifact,
        no_mutation_evidence: vec![
            "command is report-only and opens no persistence handle".to_owned(),
            "ranking state hash is computed from immutable fixture inputs".to_owned(),
            "before and after ranking state hashes match".to_owned(),
        ],
    }
}

fn baseline_artifact_score(scores: &[(String, f64)], artifact_id: &str) -> f64 {
    scores
        .iter()
        .find(|(candidate_id, _)| candidate_id.as_str() == artifact_id)
        .map_or(0.0, |(_, score)| *score)
}

fn economy_ranking_state_hash(artifacts: &[EconomyArtifactMetric]) -> String {
    let mut hasher = blake3::Hasher::new();
    for artifact in artifacts {
        hasher.update(artifact.artifact_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(artifact.artifact_type.as_bytes());
        hasher.update(b"\0");
        hasher.update(artifact.token_cost.to_string().as_bytes());
        hasher.update(b"\0");
        hasher.update(artifact.utility_score.to_string().as_bytes());
        hasher.update(b"\0");
        hasher.update(artifact.false_alarm_rate.to_string().as_bytes());
        hasher.update(b"\0");
        hasher.update(artifact.maintenance_debt.to_string().as_bytes());
        hasher.update(b"\0");
        let reserve_state = if artifact.tail_risk_protected {
            "protected"
        } else {
            "normal"
        };
        hasher.update(reserve_state.as_bytes());
        hasher.update(b"\n");
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

#[derive(Clone, Debug)]
struct EconomyWorkspaceMetrics {
    workspace_id: String,
    active_artifacts: Vec<EconomyArtifactMetric>,
    tombstoned_count: u32,
}

#[derive(Clone, Debug)]
struct EconomyArtifactMetric {
    artifact_id: String,
    artifact_type: String,
    score: f64,
    token_cost: u32,
    utility_score: f64,
    cost_score: f64,
    confidence_score: f64,
    freshness_score: f64,
    false_alarm_rate: f64,
    maintenance_debt: f64,
    tail_risk_protected: bool,
    retrieval_frequency: u32,
    last_accessed_days_ago: u32,
    citation_count: u32,
    confidence_delta: f64,
    decay_factor: f64,
    rationale: String,
}

impl EconomyArtifactMetric {
    fn is_sparse(&self) -> bool {
        self.retrieval_frequency < 2
    }

    fn is_stale(&self) -> bool {
        self.last_accessed_days_ago >= 90
    }
}

fn round_metric(value: f64) -> f64 {
    if value.is_finite() {
        (value * 1000.0).round() / 1000.0
    } else {
        0.0
    }
}

const ECONOMY_SCORE_FORMULA: &str = "utility*0.40 + confidence*0.20 + freshness*0.15 + cost*0.10 + tail_risk_reserve*0.10 - false_alarm_rate*0.25 - maintenance_debt*0.15";

fn economy_formula_components() -> Vec<String> {
    vec![
        "utility_score".to_owned(),
        "confidence_score".to_owned(),
        "freshness_score".to_owned(),
        "cost_score".to_owned(),
        "tail_risk_reserve".to_owned(),
        "false_alarm_rate".to_owned(),
        "maintenance_debt".to_owned(),
    ]
}

fn load_memory_economy_metrics(
    workspace_path: &Path,
    database_path: &Path,
) -> Result<EconomyWorkspaceMetrics, DomainError> {
    let workspace_path = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let workspace_id = stable_workspace_id(&workspace_path);
    let connection = DbConnection::open_file(database_path).map_err(|error| {
        DomainError::UnsatisfiedDegradedMode {
            message: format!(
                "Memory economy metrics are unavailable because the database could not be opened: {error}"
            ),
            repair: Some("ee init --workspace .".to_owned()),
        }
    })?;
    let active_memories = connection
        .list_memories(&workspace_id, None, false)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to read memory rows for economy metrics: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    let all_memories = connection
        .list_memories(&workspace_id, None, true)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to read tombstoned memory rows for economy metrics: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    let tombstoned_count = u32::try_from(
        all_memories
            .iter()
            .filter(|memory| memory.tombstoned_at.is_some())
            .count(),
    )
    .unwrap_or(u32::MAX);
    let now = Utc::now();
    let mut active_artifacts = active_memories
        .iter()
        .map(|memory| {
            let tags =
                connection
                    .get_memory_tags(&memory.id)
                    .map_err(|error| DomainError::Storage {
                        message: format!("Failed to read memory tags for economy metrics: {error}"),
                        repair: Some("ee doctor --json".to_owned()),
                    })?;
            let feedback = connection
                .list_feedback_events_for_target("memory", &memory.id)
                .map_err(|error| DomainError::Storage {
                    message: format!("Failed to read memory feedback for economy metrics: {error}"),
                    repair: Some("ee doctor --json".to_owned()),
                })?;
            Ok(memory_metric(memory, &tags, &feedback, now))
        })
        .collect::<Result<Vec<_>, DomainError>>()?;
    active_artifacts.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));

    Ok(EconomyWorkspaceMetrics {
        workspace_id,
        active_artifacts,
        tombstoned_count,
    })
}

fn memory_metric(
    memory: &StoredMemory,
    tags: &[String],
    feedback: &[StoredFeedbackEvent],
    now: DateTime<Utc>,
) -> EconomyArtifactMetric {
    let last_accessed_days_ago = last_accessed_days_ago(memory, feedback, now);
    let retrieval_frequency = u32::try_from(feedback.len()).unwrap_or(u32::MAX);
    let harmful_count = feedback
        .iter()
        .filter(|event| is_harmful_signal(&event.signal))
        .count();
    let stale_count = feedback
        .iter()
        .filter(|event| is_stale_signal(&event.signal))
        .count();
    let helpful_count = feedback
        .iter()
        .filter(|event| is_helpful_signal(&event.signal))
        .count();
    let false_alarm_rate = if retrieval_frequency == 0 {
        0.0
    } else {
        harmful_count as f64 / f64::from(retrieval_frequency)
    };
    let confidence_delta = round_metric(
        helpful_count as f64 * 0.03 - harmful_count as f64 * 0.05 - stale_count as f64 * 0.02,
    );
    let age_penalty = (f64::from(last_accessed_days_ago) / 180.0).min(0.80);
    let decay_factor = round_metric(1.0 - age_penalty);
    let freshness_score = decay_factor;
    let token_cost = estimate_token_cost(&memory.content);
    let cost_score = round_metric(1.0 - (f64::from(token_cost) / 4_000.0).min(0.95));
    let maintenance_debt = round_metric(
        (f64::from(last_accessed_days_ago) / 365.0).min(0.7)
            + false_alarm_rate * 0.2
            + if memory.tombstoned_at.is_some() {
                0.3
            } else {
                0.0
            },
    )
    .min(1.0);
    let tail_risk_protected = tags.iter().any(|tag| is_tail_risk_tag(tag));
    let reserve_score = if tail_risk_protected { 1.0 } else { 0.0 };
    let utility_score =
        round_metric((f64::from(memory.utility) + confidence_delta).clamp(0.0, 1.0));
    let confidence_score = round_metric(f64::from(memory.confidence).clamp(0.0, 1.0));
    let score = round_metric(
        utility_score * 0.40
            + confidence_score * 0.20
            + freshness_score * 0.15
            + cost_score * 0.10
            + reserve_score * 0.10
            - false_alarm_rate * 0.25
            - maintenance_debt * 0.15,
    )
    .clamp(0.0, 1.0);
    let citation_count = u32::try_from(
        usize::from(memory.provenance_uri.is_some())
            + feedback
                .iter()
                .filter(|event| event.evidence_json.is_some())
                .count(),
    )
    .unwrap_or(u32::MAX);

    EconomyArtifactMetric {
        artifact_id: memory.id.clone(),
        artifact_type: "memory".to_owned(),
        token_cost,
        utility_score,
        cost_score,
        confidence_score,
        freshness_score,
        false_alarm_rate: round_metric(false_alarm_rate),
        maintenance_debt,
        tail_risk_protected,
        retrieval_frequency,
        last_accessed_days_ago,
        citation_count,
        confidence_delta,
        decay_factor,
        rationale: if tail_risk_protected {
            "explicit tail-risk metadata keeps this memory in reserve review".to_owned()
        } else if harmful_count > helpful_count {
            "harmful feedback exceeds helpful feedback; review before promotion".to_owned()
        } else if retrieval_frequency == 0 {
            "no feedback evidence yet; score is conservative".to_owned()
        } else {
            "score derived from stored memory row plus feedback events".to_owned()
        },
        score,
    }
}

fn last_accessed_days_ago(
    memory: &StoredMemory,
    feedback: &[StoredFeedbackEvent],
    now: DateTime<Utc>,
) -> u32 {
    let latest_feedback = feedback
        .iter()
        .filter_map(|event| parse_rfc3339(&event.created_at))
        .max();
    let latest_memory = parse_rfc3339(&memory.updated_at)
        .or_else(|| parse_rfc3339(&memory.created_at))
        .unwrap_or(now);
    let latest = latest_feedback.unwrap_or(latest_memory);
    u32::try_from(now.signed_duration_since(latest).num_days().max(0)).unwrap_or(u32::MAX)
}

fn parse_rfc3339(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|datetime| datetime.with_timezone(&Utc))
}

fn estimate_token_cost(content: &str) -> u32 {
    let estimated = content.split_whitespace().count().max(1) * 4;
    u32::try_from(estimated).unwrap_or(u32::MAX)
}

fn is_helpful_signal(signal: &str) -> bool {
    matches!(signal, "helpful" | "confirmation" | "positive")
}

fn is_harmful_signal(signal: &str) -> bool {
    matches!(
        signal,
        "harmful" | "contradiction" | "inaccurate" | "negative"
    )
}

fn is_stale_signal(signal: &str) -> bool {
    matches!(signal, "stale" | "outdated")
}

fn is_tail_risk_tag(tag: &str) -> bool {
    matches!(
        tag,
        "tail-risk" | "tail_risk" | "risk-reserve" | "safety-critical" | "protected"
    )
}

fn average_by(artifacts: &[EconomyArtifactMetric], f: fn(&EconomyArtifactMetric) -> f64) -> f64 {
    if artifacts.is_empty() {
        0.0
    } else {
        artifacts.iter().map(f).sum::<f64>() / artifacts.len() as f64
    }
}

fn report_degradations(
    artifacts: &[EconomyArtifactMetric],
    metrics: &EconomyWorkspaceMetrics,
    artifact_type: Option<&str>,
) -> Vec<EconomyDegradation> {
    if artifact_type.is_some_and(|artifact_type| artifact_type != "memory") {
        return vec![economy_degradation(
            "economy_artifact_type_unavailable",
            "medium",
            "Only persisted memory rows are currently available for economy metrics.",
            "ee economy report --artifact-type memory --json",
        )];
    }
    if metrics.active_artifacts.is_empty() {
        return vec![economy_degradation(
            "economy_metrics_empty",
            "low",
            "No active memory rows exist in the workspace, so economy scoring abstains.",
            "ee remember --workspace . --level procedural --kind rule \"...\" --json",
        )];
    }
    sparse_degradations(artifacts)
}

fn sparse_degradations(artifacts: &[EconomyArtifactMetric]) -> Vec<EconomyDegradation> {
    if artifacts.is_empty() {
        return vec![economy_degradation(
            "economy_metrics_empty",
            "low",
            "No active persisted artifacts are available for economy scoring.",
            "ee remember --workspace . --level procedural --kind rule \"...\" --json",
        )];
    }
    if artifacts.iter().all(EconomyArtifactMetric::is_sparse) {
        vec![economy_degradation(
            "economy_evidence_sparse",
            "medium",
            "Persisted memory rows exist, but feedback evidence is too sparse for aggressive recommendations.",
            "ee outcome <memory-id> --signal helpful --json",
        )]
    } else {
        Vec::new()
    }
}

fn artifact_degradations(artifact: &EconomyArtifactMetric) -> Vec<EconomyDegradation> {
    if artifact.is_sparse() {
        vec![economy_degradation(
            "economy_evidence_sparse",
            "medium",
            "This artifact has sparse feedback evidence; treat the score as review guidance.",
            "ee outcome <memory-id> --signal helpful --json",
        )]
    } else {
        Vec::new()
    }
}

fn economy_degradation(
    code: &str,
    severity: &str,
    message: &str,
    repair: &str,
) -> EconomyDegradation {
    EconomyDegradation {
        code: code.to_owned(),
        severity: severity.to_owned(),
        message: message.to_owned(),
        repair: repair.to_owned(),
    }
}

fn economy_status(artifacts: &[EconomyArtifactMetric], degraded: &[EconomyDegradation]) -> String {
    if artifacts.is_empty() {
        "abstain".to_owned()
    } else if !degraded.is_empty() {
        "review".to_owned()
    } else {
        "available".to_owned()
    }
}

fn maintenance_debt_from_metrics(metrics: &EconomyWorkspaceMetrics) -> MaintenanceDebt {
    let stale_artifacts = u32::try_from(
        metrics
            .active_artifacts
            .iter()
            .filter(|artifact| artifact.is_stale())
            .count(),
    )
    .unwrap_or(u32::MAX);
    let consolidation_candidates = u32::try_from(
        metrics
            .active_artifacts
            .iter()
            .filter(|artifact| artifact.maintenance_debt >= 0.45 && !artifact.tail_risk_protected)
            .count(),
    )
    .unwrap_or(u32::MAX);

    MaintenanceDebt {
        stale_artifacts,
        consolidation_candidates,
        tombstone_pending: metrics.tombstoned_count,
        estimated_cleanup_tokens: metrics
            .active_artifacts
            .iter()
            .filter(|artifact| artifact.maintenance_debt >= 0.45 && !artifact.tail_risk_protected)
            .map(|artifact| artifact.token_cost)
            .sum(),
    }
}

fn tail_risk_reserves_from_metrics(metrics: &EconomyWorkspaceMetrics) -> TailRiskReserves {
    let critical_memories = u32::try_from(
        metrics
            .active_artifacts
            .iter()
            .filter(|artifact| artifact.tail_risk_protected)
            .count(),
    )
    .unwrap_or(u32::MAX);
    let fallback_procedures = 0;
    let degradation_coverage = if metrics.active_artifacts.is_empty() {
        0.0
    } else {
        round_metric(f64::from(critical_memories) / metrics.active_artifacts.len() as f64)
    };

    TailRiskReserves {
        critical_memories,
        fallback_procedures,
        degradation_coverage,
    }
}

fn prune_recommendations_from_metrics(
    artifacts: &[EconomyArtifactMetric],
) -> Vec<EconomyPruneRecommendation> {
    artifacts
        .iter()
        .filter_map(prune_recommendation_for_artifact)
        .collect()
}

fn prune_recommendation_for_artifact(
    artifact: &EconomyArtifactMetric,
) -> Option<EconomyPruneRecommendation> {
    if artifact.tail_risk_protected {
        return (artifact.is_stale() || artifact.is_sparse()).then(|| EconomyPruneRecommendation {
            id: format!("econ_prune_reserve_{}", artifact.artifact_id),
            action: "reserve_review".to_owned(),
            artifact_type: artifact.artifact_type.clone(),
            candidate_count: 1,
            priority: 99,
            risk: "low".to_owned(),
            rationale:
                "Explicit tail-risk metadata prevents demotion; schedule human review instead."
                    .to_owned(),
            estimated_token_savings: 0,
            dry_run_command: "ee economy prune-plan --dry-run --json".to_owned(),
        });
    }

    if artifact.false_alarm_rate >= 0.4 {
        Some(EconomyPruneRecommendation {
            id: format!("econ_prune_review_false_alarms_{}", artifact.artifact_id),
            action: "review_false_alarms".to_owned(),
            artifact_type: artifact.artifact_type.clone(),
            candidate_count: 1,
            priority: 92,
            risk: "medium".to_owned(),
            rationale: "Stored harmful feedback is high enough to require review before this memory is surfaced aggressively.".to_owned(),
            estimated_token_savings: artifact.token_cost,
            dry_run_command: "ee economy prune-plan --dry-run --json".to_owned(),
        })
    } else if artifact.is_stale() && artifact.utility_score < 0.45 {
        Some(EconomyPruneRecommendation {
            id: format!("econ_prune_retire_stale_{}", artifact.artifact_id),
            action: "retire_review".to_owned(),
            artifact_type: artifact.artifact_type.clone(),
            candidate_count: 1,
            priority: 84,
            risk: "medium".to_owned(),
            rationale:
                "Persisted memory is stale and low utility; retire only after provenance review."
                    .to_owned(),
            estimated_token_savings: artifact.token_cost,
            dry_run_command: "ee economy prune-plan --dry-run --json".to_owned(),
        })
    } else if artifact.is_stale() || artifact.confidence_score < 0.45 {
        Some(EconomyPruneRecommendation {
            id: format!("econ_prune_revalidate_{}", artifact.artifact_id),
            action: "revalidate".to_owned(),
            artifact_type: artifact.artifact_type.clone(),
            candidate_count: 1,
            priority: 76,
            risk: "low".to_owned(),
            rationale: "Persisted memory needs revalidation before any compaction or demotion."
                .to_owned(),
            estimated_token_savings: 0,
            dry_run_command: "ee economy prune-plan --dry-run --json".to_owned(),
        })
    } else {
        None
    }
}

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{CreateFeedbackEventInput, CreateMemoryInput, CreateWorkspaceInput};

    type TestResult = Result<(), String>;

    struct EconomyFixture {
        _temp: tempfile::TempDir,
        workspace: PathBuf,
        database: PathBuf,
        workspace_id: String,
    }

    fn fixture() -> Result<EconomyFixture, String> {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(workspace.join(".ee")).map_err(|error| error.to_string())?;
        let workspace = workspace
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let database = workspace.join(".ee").join("ee.db");
        let workspace_id = stable_workspace_id(&workspace);
        let connection = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("workspace".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;

        Ok(EconomyFixture {
            _temp: temp,
            workspace,
            database,
            workspace_id,
        })
    }

    fn connection(fixture: &EconomyFixture) -> Result<DbConnection, String> {
        DbConnection::open_file(&fixture.database).map_err(|error| error.to_string())
    }

    fn add_memory(
        fixture: &EconomyFixture,
        id: &str,
        content: &str,
        confidence: f32,
        utility: f32,
        tags: &[&str],
    ) -> TestResult {
        let connection = connection(fixture)?;
        connection
            .insert_memory(
                id,
                &CreateMemoryInput {
                    workspace_id: fixture.workspace_id.clone(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: content.to_owned(),
                    confidence,
                    utility,
                    importance: 0.5,
                    provenance_uri: Some("file://AGENTS.md#L1".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: Some("economy-test".to_owned()),
                    tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())
    }

    fn add_feedback(
        fixture: &EconomyFixture,
        id: &str,
        target_id: &str,
        signal: &str,
    ) -> TestResult {
        let connection = connection(fixture)?;
        connection
            .insert_feedback_event(
                id,
                &CreateFeedbackEventInput {
                    workspace_id: fixture.workspace_id.clone(),
                    target_type: "memory".to_owned(),
                    target_id: target_id.to_owned(),
                    signal: signal.to_owned(),
                    weight: 1.0,
                    source_type: "outcome_observed".to_owned(),
                    source_id: Some(format!("src_{id}")),
                    reason: Some("economy test feedback".to_owned()),
                    evidence_json: Some("{\"schema\":\"fixture.economy.feedback.v1\"}".to_owned()),
                    session_id: Some("session_economy".to_owned()),
                },
            )
            .map_err(|error| error.to_string())
    }

    fn make_stale(fixture: &EconomyFixture, id: &str) -> TestResult {
        let connection = connection(fixture)?;
        connection
            .execute_raw(&format!(
                "UPDATE memories SET updated_at = '2025-01-01T00:00:00Z' WHERE id = '{id}'"
            ))
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn report_options(fixture: &EconomyFixture) -> EconomyReportOptions {
        EconomyReportOptions {
            workspace_path: fixture.workspace.clone(),
            database_path: fixture.database.clone(),
            artifact_type: None,
            min_utility: None,
            include_debt: true,
            include_reserves: true,
        }
    }

    fn score_options(fixture: &EconomyFixture, artifact_id: &str) -> EconomyScoreOptions {
        EconomyScoreOptions {
            workspace_path: fixture.workspace.clone(),
            database_path: fixture.database.clone(),
            artifact_id: artifact_id.to_owned(),
            artifact_type: "memory".to_owned(),
            breakdown: true,
        }
    }

    #[test]
    fn economy_report_empty_db_abstains_without_seed_artifacts() -> TestResult {
        let fixture = fixture()?;
        let report =
            generate_economy_report(&report_options(&fixture)).map_err(|error| error.message())?;

        assert_eq!(report.schema, ECONOMY_REPORT_SCHEMA_V1);
        assert_eq!(report.status, "abstain");
        assert_eq!(report.total_artifacts, 0);
        assert!(report.scored_artifact_ids.is_empty());
        assert_eq!(report.degraded[0].code, "economy_metrics_empty");
        Ok(())
    }

    #[test]
    fn economy_report_sparse_evidence_reviews_real_rows() -> TestResult {
        let fixture = fixture()?;
        add_memory(
            &fixture,
            "mem_00000000000000000000000001",
            "Run cargo fmt before release.",
            0.8,
            0.7,
            &[],
        )?;
        let report =
            generate_economy_report(&report_options(&fixture)).map_err(|error| error.message())?;

        assert_eq!(report.status, "review");
        assert_eq!(
            report.scored_artifact_ids,
            vec!["mem_00000000000000000000000001"]
        );
        assert_eq!(report.artifact_breakdown[0].artifact_type, "memory");
        assert_eq!(report.degraded[0].code, "economy_evidence_sparse");
        Ok(())
    }

    #[test]
    fn economy_score_high_false_alarm_uses_feedback_counts() -> TestResult {
        let fixture = fixture()?;
        let memory_id = "mem_00000000000000000000000002";
        add_memory(
            &fixture,
            memory_id,
            "Warn before risky release steps.",
            0.8,
            0.7,
            &[],
        )?;
        add_feedback(
            &fixture,
            "fb_00000000000000000000000001",
            memory_id,
            "harmful",
        )?;
        add_feedback(
            &fixture,
            "fb_00000000000000000000000002",
            memory_id,
            "harmful",
        )?;
        add_feedback(
            &fixture,
            "fb_00000000000000000000000003",
            memory_id,
            "helpful",
        )?;

        let report =
            score_artifact(&score_options(&fixture, memory_id)).map_err(|error| error.message())?;

        assert_eq!(report.artifact_id, memory_id);
        assert!(report.false_alarm_rate > 0.6);
        assert_eq!(
            report.breakdown.as_ref().map(|b| b.retrieval_frequency),
            Some(3)
        );
        assert_eq!(report.mutation_status, "read_only_no_mutation");
        Ok(())
    }

    #[test]
    fn prune_plan_reviews_stale_artifact_from_db() -> TestResult {
        let fixture = fixture()?;
        let memory_id = "mem_00000000000000000000000003";
        add_memory(
            &fixture,
            memory_id,
            "Old low utility branch note.",
            0.5,
            0.2,
            &[],
        )?;
        make_stale(&fixture, memory_id)?;

        let report = generate_prune_plan(&EconomyPrunePlanOptions {
            workspace_path: fixture.workspace.clone(),
            database_path: fixture.database.clone(),
            dry_run: true,
            max_recommendations: 10,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.mutation_status, "read_only_no_mutation");
        assert_eq!(report.recommendations.len(), 1);
        assert_eq!(report.recommendations[0].action, "retire_review");
        assert!(report.recommendations[0].id.contains(memory_id));
        Ok(())
    }

    #[test]
    fn tail_risk_protected_memory_is_reserved_not_retired() -> TestResult {
        let fixture = fixture()?;
        let memory_id = "mem_00000000000000000000000004";
        add_memory(
            &fixture,
            memory_id,
            "Never delete release verification safeguards.",
            0.9,
            0.2,
            &["tail-risk"],
        )?;
        make_stale(&fixture, memory_id)?;

        let score = score_artifact(&score_options(&fixture, memory_id)).map_err(|e| e.message())?;
        assert!(score.tail_risk_protected);

        let plan = generate_prune_plan(&EconomyPrunePlanOptions {
            workspace_path: fixture.workspace.clone(),
            database_path: fixture.database.clone(),
            dry_run: true,
            max_recommendations: 10,
        })
        .map_err(|error| error.message())?;

        assert_eq!(plan.recommendations[0].action, "reserve_review");
        assert_eq!(plan.recommendations[0].estimated_token_savings, 0);
        assert!(!plan.recommendations[0].action.contains("retire"));
        Ok(())
    }

    #[test]
    fn report_ordering_is_deterministic_by_memory_id() -> TestResult {
        let fixture = fixture()?;
        add_memory(
            &fixture,
            "mem_00000000000000000000000009",
            "Later ID.",
            0.7,
            0.7,
            &[],
        )?;
        add_memory(
            &fixture,
            "mem_00000000000000000000000008",
            "Earlier ID.",
            0.7,
            0.7,
            &[],
        )?;

        let report =
            generate_economy_report(&report_options(&fixture)).map_err(|error| error.message())?;

        assert_eq!(
            report.scored_artifact_ids,
            vec![
                "mem_00000000000000000000000008",
                "mem_00000000000000000000000009"
            ]
        );
        Ok(())
    }

    #[test]
    fn economy_reports_do_not_mutate_storage() -> TestResult {
        let fixture = fixture()?;
        let memory_id = "mem_00000000000000000000000005";
        add_memory(
            &fixture,
            memory_id,
            "Keep reports read-only.",
            0.7,
            0.7,
            &[],
        )?;
        let before = connection(&fixture)?
            .list_memories(&fixture.workspace_id, None, true)
            .map_err(|error| error.to_string())?
            .len();

        let _ =
            generate_economy_report(&report_options(&fixture)).map_err(|error| error.message())?;
        let _ = generate_prune_plan(&EconomyPrunePlanOptions {
            workspace_path: fixture.workspace.clone(),
            database_path: fixture.database.clone(),
            dry_run: true,
            max_recommendations: 10,
        })
        .map_err(|error| error.message())?;
        let _ = simulate_budgets(&EconomySimulateOptions {
            workspace_path: fixture.workspace.clone(),
            database_path: fixture.database.clone(),
            baseline_budget_tokens: 4_000,
            budget_tokens: vec![2_000],
            context_profile: "balanced".to_owned(),
            situation_profile: "standard".to_owned(),
        })
        .map_err(|error| error.message())?;

        let after = connection(&fixture)?
            .list_memories(&fixture.workspace_id, None, true)
            .map_err(|error| error.to_string())?
            .len();
        assert_eq!(before, after);
        Ok(())
    }

    #[test]
    fn prune_plan_requires_dry_run() -> TestResult {
        let fixture = fixture()?;
        let error = match generate_prune_plan(&EconomyPrunePlanOptions {
            workspace_path: fixture.workspace,
            database_path: fixture.database,
            dry_run: false,
            max_recommendations: 5,
        }) {
            Ok(_) => return Err("non-dry-run prune plan unexpectedly succeeded".to_owned()),
            Err(error) => error,
        };

        assert_eq!(error.code(), "policy_denied");
        Ok(())
    }

    #[test]
    fn prune_plan_honors_recommendation_limit() -> TestResult {
        let fixture = fixture()?;
        for index in 0..3 {
            let memory_id = format!("mem_0000000000000000000000001{index}");
            add_memory(&fixture, &memory_id, "Noisy memory.", 0.6, 0.6, &[])?;
            add_feedback(
                &fixture,
                &format!("fb_0000000000000000000000001{index}"),
                &memory_id,
                "harmful",
            )?;
            add_feedback(
                &fixture,
                &format!("fb_0000000000000000000000002{index}"),
                &memory_id,
                "harmful",
            )?;
        }
        let report = generate_prune_plan(&EconomyPrunePlanOptions {
            workspace_path: fixture.workspace,
            database_path: fixture.database,
            dry_run: true,
            max_recommendations: 2,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.recommendations.len(), 2);
        assert_eq!(report.summary.recommendation_count, 2);
        Ok(())
    }

    #[test]
    fn simulate_includes_baseline_and_preserves_ranking_state() -> TestResult {
        let fixture = fixture()?;
        add_memory(
            &fixture,
            "mem_00000000000000000000000006",
            "Useful release memory.",
            0.8,
            0.8,
            &[],
        )?;
        let report = simulate_budgets(&EconomySimulateOptions {
            workspace_path: fixture.workspace,
            database_path: fixture.database,
            baseline_budget_tokens: 4_000,
            budget_tokens: vec![2_000, 8_000],
            context_profile: "balanced".to_owned(),
            situation_profile: "standard".to_owned(),
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.schema, ECONOMY_SIMULATION_SCHEMA_V1);
        assert_eq!(report.mutation_status, "not_applied");
        assert!(report.read_only);
        assert_eq!(
            report.ranking_state_hash_before,
            report.ranking_state_hash_after
        );
        assert!(report.ranking_state_unchanged);
        assert_eq!(report.scenarios.len(), 3);
        assert_eq!(
            report
                .scenarios
                .iter()
                .map(|scenario| scenario.budget_tokens)
                .collect::<Vec<_>>(),
            vec![2_000, 4_000, 8_000]
        );
        Ok(())
    }

    #[test]
    fn simulate_defaults_budget_set_when_no_alternates_are_supplied() -> TestResult {
        let fixture = fixture()?;
        let report = simulate_budgets(&EconomySimulateOptions {
            workspace_path: fixture.workspace,
            database_path: fixture.database,
            baseline_budget_tokens: 4_000,
            budget_tokens: Vec::new(),
            context_profile: "compact".to_owned(),
            situation_profile: "summary".to_owned(),
        })
        .map_err(|error| error.message())?;

        assert_eq!(
            report
                .scenarios
                .iter()
                .map(|scenario| scenario.budget_tokens)
                .collect::<Vec<_>>(),
            vec![2_000, 4_000, 8_000]
        );
        assert_eq!(report.context_profile, "compact");
        assert_eq!(report.situation_profile, "summary");
        Ok(())
    }

    #[test]
    fn simulate_rejects_zero_budget() -> TestResult {
        let fixture = fixture()?;
        let error = match simulate_budgets(&EconomySimulateOptions {
            workspace_path: fixture.workspace,
            database_path: fixture.database,
            baseline_budget_tokens: 4_000,
            budget_tokens: vec![0],
            context_profile: "balanced".to_owned(),
            situation_profile: "standard".to_owned(),
        }) {
            Ok(_) => return Err("zero budget simulation unexpectedly succeeded".to_owned()),
            Err(error) => error,
        };

        assert_eq!(error.code(), "usage");
        Ok(())
    }

    #[test]
    fn simulate_rejects_unknown_profiles() -> TestResult {
        let fixture = fixture()?;
        let error = match simulate_budgets(&EconomySimulateOptions {
            workspace_path: fixture.workspace,
            database_path: fixture.database,
            baseline_budget_tokens: 4_000,
            budget_tokens: vec![2_000],
            context_profile: "unknown".to_owned(),
            situation_profile: "standard".to_owned(),
        }) {
            Ok(_) => return Err("unknown profile simulation unexpectedly succeeded".to_owned()),
            Err(error) => error,
        };

        assert_eq!(error.code(), "usage");
        Ok(())
    }
}
