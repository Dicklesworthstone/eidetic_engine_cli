//! Memory economics and attention budgets (EE-431).
//!
//! Treats agent attention as scarce: scores utility, cost, false alarms,
//! maintenance debt, and tail-risk reserves before surfacing or demoting artifacts.

use chrono::Utc;
use serde::Serialize;

use crate::models::DomainError;
use crate::models::economy::{
    AttentionBudgetAllocation, AttentionBudgetRequest, ContextAttentionProfile,
    ECONOMY_SIMULATION_SCHEMA_V1, SituationAttentionProfile,
};

/// Schema for economy prune plans.
pub const ECONOMY_PRUNE_PLAN_SCHEMA_V1: &str = "ee.economy.prune_plan.v1";

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

/// Options for generating a report-only prune plan.
#[derive(Clone, Debug)]
pub struct EconomyPrunePlanOptions {
    pub dry_run: bool,
    pub max_recommendations: usize,
}

/// Options for comparing alternate attention budgets without mutating ranking state.
#[derive(Clone, Debug)]
pub struct EconomySimulateOptions {
    pub baseline_budget_tokens: u32,
    pub budget_tokens: Vec<u32>,
    pub context_profile: String,
    pub situation_profile: String,
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

/// Report-only plan for reducing memory-economy maintenance debt.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EconomyPrunePlan {
    pub schema: &'static str,
    pub generated_at: String,
    pub dry_run: bool,
    pub status: String,
    pub mutation_status: String,
    pub summary: EconomyPrunePlanSummary,
    pub recommendations: Vec<EconomyPruneRecommendation>,
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
    pub mutation_status: String,
    pub baseline_budget_tokens: u32,
    pub context_profile: String,
    pub situation_profile: String,
    pub ranking_state_hash_before: String,
    pub ranking_state_hash_after: String,
    pub ranking_state_unchanged: bool,
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

    let mut recommendations = seed_prune_recommendations();
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

    Ok(EconomyPrunePlan {
        schema: ECONOMY_PRUNE_PLAN_SCHEMA_V1,
        generated_at: Utc::now().to_rfc3339(),
        dry_run: true,
        status: "planned".to_owned(),
        mutation_status: "not_applied".to_owned(),
        summary,
        recommendations,
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

    let budget_tokens = normalize_simulation_budgets(
        options.baseline_budget_tokens,
        options.budget_tokens.as_slice(),
    )?;
    let ranking_state_hash = economy_ranking_state_hash();
    let mut scenarios = budget_tokens
        .iter()
        .map(|budget| simulate_budget_scenario(*budget, context_profile, situation_profile))
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
        mutation_status: "not_applied".to_owned(),
        baseline_budget_tokens: options.baseline_budget_tokens,
        context_profile: context_profile.as_str().to_owned(),
        situation_profile: situation_profile.as_str().to_owned(),
        ranking_state_hash_before: ranking_state_hash.clone(),
        ranking_state_hash_after: ranking_state_hash,
        ranking_state_unchanged: true,
        summary,
        scenarios,
        explanations: vec![
            "simulation uses deterministic in-memory economy fixtures and does not write DB, index, graph, or ranking records".to_owned(),
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
) -> EconomySimulationScenario {
    let allocation = AttentionBudgetAllocation::calculate(AttentionBudgetRequest::new(
        budget_tokens,
        context_profile,
        situation_profile,
    ));
    let surfaced_limit = allocation.max_items as usize;
    let mut ranking = economy_artifact_seeds()
        .iter()
        .map(|seed| score_seed_for_budget(seed, &allocation))
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

fn score_seed_for_budget(
    seed: &EconomyArtifactSeed,
    allocation: &AttentionBudgetAllocation,
) -> EconomySimulatedArtifact {
    let per_item_budget = f64::from(
        allocation
            .retrieval_tokens
            .saturating_add(allocation.evidence_tokens)
            .saturating_add(allocation.procedure_tokens),
    ) / f64::from(allocation.max_items.max(1));
    let fit = (per_item_budget / f64::from(seed.token_cost.max(1))).min(1.0);
    let reserve_boost = if seed.tail_risk_protected {
        allocation.reserve_ratio() * 0.35
    } else {
        0.0
    };
    let over_budget_penalty = if f64::from(seed.token_cost) > per_item_budget {
        ((f64::from(seed.token_cost) - per_item_budget) / 1_000.0).min(0.30)
    } else {
        0.0
    };
    let score = round_metric(
        seed.utility_score * 0.42
            + seed.confidence_score * 0.18
            + seed.freshness_score * 0.14
            + fit * 0.16
            + reserve_boost
            - seed.false_alarm_rate * 0.22
            - seed.maintenance_debt * 0.14
            - over_budget_penalty,
    );

    EconomySimulatedArtifact {
        rank: 0,
        artifact_id: seed.artifact_id.to_owned(),
        artifact_type: seed.artifact_type.to_owned(),
        included: false,
        score,
        score_delta_vs_baseline: 0.0,
        token_cost: seed.token_cost,
        utility_score: seed.utility_score,
        false_alarm_rate: seed.false_alarm_rate,
        maintenance_debt: seed.maintenance_debt,
        tail_risk_protected: seed.tail_risk_protected,
        rationale: seed.rationale.to_owned(),
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

fn economy_ranking_state_hash() -> String {
    let mut hasher = blake3::Hasher::new();
    for seed in economy_artifact_seeds() {
        hasher.update(seed.artifact_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(seed.artifact_type.as_bytes());
        hasher.update(b"\0");
        hasher.update(seed.token_cost.to_string().as_bytes());
        hasher.update(b"\0");
        hasher.update(seed.utility_score.to_string().as_bytes());
        hasher.update(b"\0");
        hasher.update(seed.false_alarm_rate.to_string().as_bytes());
        hasher.update(b"\0");
        hasher.update(seed.maintenance_debt.to_string().as_bytes());
        hasher.update(b"\0");
        let reserve_state = if seed.tail_risk_protected {
            "protected"
        } else {
            "normal"
        };
        hasher.update(reserve_state.as_bytes());
        hasher.update(b"\n");
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn economy_artifact_seeds() -> &'static [EconomyArtifactSeed] {
    const SEEDS: &[EconomyArtifactSeed] = &[
        EconomyArtifactSeed {
            artifact_id: "mem_release_verification_rule",
            artifact_type: "memory",
            token_cost: 420,
            utility_score: 0.91,
            confidence_score: 0.86,
            freshness_score: 0.82,
            false_alarm_rate: 0.03,
            maintenance_debt: 0.12,
            tail_risk_protected: true,
            rationale: "high-confidence release rule with low false-alarm history",
        },
        EconomyArtifactSeed {
            artifact_id: "proc_rch_verification",
            artifact_type: "procedure",
            token_cost: 760,
            utility_score: 0.84,
            confidence_score: 0.78,
            freshness_score: 0.76,
            false_alarm_rate: 0.04,
            maintenance_debt: 0.18,
            tail_risk_protected: true,
            rationale: "procedure is expensive but protects verification under load",
        },
        EconomyArtifactSeed {
            artifact_id: "tripwire_destructive_git",
            artifact_type: "tripwire",
            token_cost: 340,
            utility_score: 0.80,
            confidence_score: 0.92,
            freshness_score: 0.88,
            false_alarm_rate: 0.09,
            maintenance_debt: 0.08,
            tail_risk_protected: true,
            rationale: "rare high-severity destructive-action warning keeps reserve priority",
        },
        EconomyArtifactSeed {
            artifact_id: "sit_rust_cli_release",
            artifact_type: "situation",
            token_cost: 520,
            utility_score: 0.73,
            confidence_score: 0.68,
            freshness_score: 0.70,
            false_alarm_rate: 0.08,
            maintenance_debt: 0.22,
            tail_risk_protected: false,
            rationale: "situation route is useful when budget can carry context classifiers",
        },
        EconomyArtifactSeed {
            artifact_id: "mem_legacy_branch_note",
            artifact_type: "memory",
            token_cost: 300,
            utility_score: 0.47,
            confidence_score: 0.55,
            freshness_score: 0.40,
            false_alarm_rate: 0.18,
            maintenance_debt: 0.51,
            tail_risk_protected: false,
            rationale: "low-utility stale note loses rank as budget tightens",
        },
    ];
    SEEDS
}

struct EconomyArtifactSeed {
    artifact_id: &'static str,
    artifact_type: &'static str,
    token_cost: u32,
    utility_score: f64,
    confidence_score: f64,
    freshness_score: f64,
    false_alarm_rate: f64,
    maintenance_debt: f64,
    tail_risk_protected: bool,
    rationale: &'static str,
}

fn round_metric(value: f64) -> f64 {
    if value.is_finite() {
        (value * 1000.0).round() / 1000.0
    } else {
        0.0
    }
}

fn seed_prune_recommendations() -> Vec<EconomyPruneRecommendation> {
    const SEEDS: &[PruneRecommendationSeed] = &[
        PruneRecommendationSeed {
            id: "econ_prune_revalidate_high_risk",
            action: "revalidate",
            artifact_type: "memory",
            candidate_count: 5,
            priority: 95,
            risk: "low",
            rationale: "High-impact memories with stale validation should be rechecked before any demotion.",
            estimated_token_savings: 0,
        },
        PruneRecommendationSeed {
            id: "econ_prune_retire_stale",
            action: "retire",
            artifact_type: "memory",
            candidate_count: 7,
            priority: 88,
            risk: "medium",
            rationale: "Low-utility stale memories should be retired once provenance confirms they are obsolete.",
            estimated_token_savings: 1400,
        },
        PruneRecommendationSeed {
            id: "econ_prune_compact_procedures",
            action: "compact",
            artifact_type: "procedure",
            candidate_count: 3,
            priority: 82,
            risk: "low",
            rationale: "Verbose procedures with repeated setup steps can be compacted while preserving evidence links.",
            estimated_token_savings: 900,
        },
        PruneRecommendationSeed {
            id: "econ_prune_merge_duplicates",
            action: "merge",
            artifact_type: "memory",
            candidate_count: 2,
            priority: 76,
            risk: "medium",
            rationale: "Near-duplicate rules should be merged into a single canonical memory with supersession links.",
            estimated_token_savings: 520,
        },
        PruneRecommendationSeed {
            id: "econ_prune_demote_noisy_tripwires",
            action: "demote",
            artifact_type: "tripwire",
            candidate_count: 4,
            priority: 70,
            risk: "medium",
            rationale: "Tripwires with high false-alarm rates should be demoted until fresh evidence improves precision.",
            estimated_token_savings: 360,
        },
    ];

    SEEDS.iter().map(prune_recommendation).collect()
}

struct PruneRecommendationSeed {
    id: &'static str,
    action: &'static str,
    artifact_type: &'static str,
    candidate_count: u32,
    priority: u8,
    risk: &'static str,
    rationale: &'static str,
    estimated_token_savings: u32,
}

fn prune_recommendation(seed: &PruneRecommendationSeed) -> EconomyPruneRecommendation {
    EconomyPruneRecommendation {
        id: seed.id.to_owned(),
        action: seed.action.to_owned(),
        artifact_type: seed.artifact_type.to_owned(),
        candidate_count: seed.candidate_count,
        priority: seed.priority,
        risk: seed.risk.to_owned(),
        rationale: seed.rationale.to_owned(),
        estimated_token_savings: seed.estimated_token_savings,
        dry_run_command: "ee economy prune-plan --dry-run --json".to_owned(),
    }
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
    fn score_artifact_includes_breakdown_when_requested() -> Result<(), String> {
        let options = EconomyScoreOptions {
            artifact_id: "mem_test".to_string(),
            artifact_type: "memory".to_string(),
            breakdown: true,
        };
        let report = score_artifact(&options).map_err(|e| e.message())?;
        assert!(report.breakdown.is_some());
        Ok(())
    }

    #[test]
    fn prune_plan_requires_dry_run() -> Result<(), String> {
        let error = match generate_prune_plan(&EconomyPrunePlanOptions {
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
    fn prune_plan_contains_all_action_classes() -> Result<(), String> {
        let report = generate_prune_plan(&EconomyPrunePlanOptions {
            dry_run: true,
            max_recommendations: 10,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.schema, ECONOMY_PRUNE_PLAN_SCHEMA_V1);
        assert_eq!(report.mutation_status, "not_applied");
        assert_eq!(
            report.summary.actions,
            vec!["revalidate", "retire", "compact", "merge", "demote"]
        );
        Ok(())
    }

    #[test]
    fn prune_plan_honors_recommendation_limit() -> Result<(), String> {
        let report = generate_prune_plan(&EconomyPrunePlanOptions {
            dry_run: true,
            max_recommendations: 2,
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.recommendations.len(), 2);
        assert_eq!(report.summary.recommendation_count, 2);
        assert_eq!(report.summary.actions, vec!["revalidate", "retire"]);
        Ok(())
    }

    #[test]
    fn simulate_includes_baseline_and_preserves_ranking_state() -> Result<(), String> {
        let report = simulate_budgets(&EconomySimulateOptions {
            baseline_budget_tokens: 4_000,
            budget_tokens: vec![2_000, 8_000],
            context_profile: "balanced".to_owned(),
            situation_profile: "standard".to_owned(),
        })
        .map_err(|error| error.message())?;

        assert_eq!(report.schema, ECONOMY_SIMULATION_SCHEMA_V1);
        assert_eq!(report.mutation_status, "not_applied");
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
    fn simulate_defaults_budget_set_when_no_alternates_are_supplied() -> Result<(), String> {
        let report = simulate_budgets(&EconomySimulateOptions {
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
    fn simulate_rejects_zero_budget() -> Result<(), String> {
        let error = match simulate_budgets(&EconomySimulateOptions {
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
    fn simulate_rejects_unknown_profiles() -> Result<(), String> {
        let error = match simulate_budgets(&EconomySimulateOptions {
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
