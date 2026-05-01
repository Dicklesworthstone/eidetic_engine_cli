//! Memory economy and attention budget types (EE-430).
//!
//! Treat agent attention as scarce: score utility, cost, false alarms,
//! maintenance debt, and tail-risk reserves before surfacing or demoting artifacts.

use std::{cmp::Reverse, fmt};

use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};

/// Schema identifier for attention cost records.
pub const ATTENTION_COST_SCHEMA_V1: &str = "ee.economy.attention_cost.v1";

/// Schema identifier for utility value records.
pub const UTILITY_VALUE_SCHEMA_V1: &str = "ee.economy.utility_value.v1";

/// Schema identifier for risk reserve records.
pub const RISK_RESERVE_SCHEMA_V1: &str = "ee.economy.risk_reserve.v1";

/// Schema identifier for maintenance debt records.
pub const MAINTENANCE_DEBT_SCHEMA_V1: &str = "ee.economy.maintenance_debt.v1";

/// Schema identifier for economy recommendation records.
pub const ECONOMY_RECOMMENDATION_SCHEMA_V1: &str = "ee.economy.recommendation.v1";

/// Schema identifier for economy report.
pub const ECONOMY_REPORT_SCHEMA_V1: &str = "ee.economy.report.v1";

/// Schema identifier for the economy schema catalog.
pub const ECONOMY_SCHEMA_CATALOG_V1: &str = "ee.economy.schemas.v1";

const JSON_SCHEMA_DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

fn rounded_metric(value: f64) -> f64 {
    if value.is_finite() {
        (value * 1000.0).round() / 1000.0
    } else {
        0.0
    }
}

// ============================================================================
// Utility Value
// ============================================================================

/// Utility value representing how helpful a memory has been.
///
/// Combines historical usage data with projected future value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UtilityValue {
    /// Raw utility score (0.0 to 1.0).
    pub score: f64,
    /// Number of times memory was retrieved.
    pub retrieval_count: u32,
    /// Number of times memory contributed to successful outcomes.
    pub success_count: u32,
    /// Number of times memory led to false alarms or wasted attention.
    pub false_alarm_count: u32,
    /// Projected future utility based on trends.
    pub projected_utility: f64,
    /// Confidence in the utility estimate.
    pub confidence: f64,
}

impl UtilityValue {
    /// Create a new utility value with initial score.
    #[must_use]
    pub fn new(score: f64) -> Self {
        Self {
            score: score.clamp(0.0, 1.0),
            retrieval_count: 0,
            success_count: 0,
            false_alarm_count: 0,
            projected_utility: score.clamp(0.0, 1.0),
            confidence: 0.5,
        }
    }

    /// Create a utility value from historical data.
    #[must_use]
    pub fn from_history(retrieval_count: u32, success_count: u32, false_alarm_count: u32) -> Self {
        let total = retrieval_count.max(1) as f64;
        let score = (success_count as f64 - false_alarm_count as f64 * 0.5) / total;
        let score = score.clamp(0.0, 1.0);
        let confidence = (total / 100.0).min(1.0);

        Self {
            score,
            retrieval_count,
            success_count,
            false_alarm_count,
            projected_utility: score,
            confidence,
        }
    }

    /// Calculate effective utility (score adjusted by confidence).
    #[must_use]
    pub fn effective(&self) -> f64 {
        self.score * self.confidence
    }

    /// Calculate false alarm rate.
    #[must_use]
    pub fn false_alarm_rate(&self) -> f64 {
        if self.retrieval_count == 0 {
            0.0
        } else {
            self.false_alarm_count as f64 / self.retrieval_count as f64
        }
    }

    /// Render as JSON.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": UTILITY_VALUE_SCHEMA_V1,
            "score": rounded_metric(self.score),
            "retrievalCount": self.retrieval_count,
            "successCount": self.success_count,
            "falseAlarmCount": self.false_alarm_count,
            "projectedUtility": rounded_metric(self.projected_utility),
            "confidence": rounded_metric(self.confidence),
            "effective": rounded_metric(self.effective()),
            "falseAlarmRate": rounded_metric(self.false_alarm_rate()),
        })
    }
}

impl Default for UtilityValue {
    fn default() -> Self {
        Self::new(0.5)
    }
}

// ============================================================================
// Attention Cost
// ============================================================================

/// Cost of surfacing a memory to an agent's attention.
///
/// Attention is scarce; every memory shown has an opportunity cost.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttentionCost {
    /// Token cost of including memory in context.
    pub token_cost: u32,
    /// Cognitive load factor (0.0 to 1.0, higher = harder to process).
    pub cognitive_load: f64,
    /// Relevance decay since last access.
    pub relevance_decay: f64,
    /// Context switching cost if memory is from different domain.
    pub context_switch_cost: f64,
    /// Priority displacement cost (what was bumped to show this).
    pub displacement_cost: f64,
}

impl AttentionCost {
    /// Create a new attention cost.
    #[must_use]
    pub fn new(token_cost: u32) -> Self {
        Self {
            token_cost,
            cognitive_load: 0.3,
            relevance_decay: 0.0,
            context_switch_cost: 0.0,
            displacement_cost: 0.0,
        }
    }

    /// Set cognitive load factor.
    #[must_use]
    pub fn with_cognitive_load(mut self, load: f64) -> Self {
        self.cognitive_load = load.clamp(0.0, 1.0);
        self
    }

    /// Set relevance decay.
    #[must_use]
    pub fn with_relevance_decay(mut self, decay: f64) -> Self {
        self.relevance_decay = decay.clamp(0.0, 1.0);
        self
    }

    /// Set context switch cost.
    #[must_use]
    pub fn with_context_switch(mut self, cost: f64) -> Self {
        self.context_switch_cost = cost.clamp(0.0, 1.0);
        self
    }

    /// Calculate total weighted cost.
    #[must_use]
    pub fn total_cost(&self) -> f64 {
        let base_cost = self.token_cost as f64 / 1000.0;
        let factors = 1.0
            + self.cognitive_load
            + self.relevance_decay
            + self.context_switch_cost
            + self.displacement_cost;
        base_cost * factors
    }

    /// Render as JSON.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": ATTENTION_COST_SCHEMA_V1,
            "tokenCost": self.token_cost,
            "cognitiveLoad": rounded_metric(self.cognitive_load),
            "relevanceDecay": rounded_metric(self.relevance_decay),
            "contextSwitchCost": rounded_metric(self.context_switch_cost),
            "displacementCost": rounded_metric(self.displacement_cost),
            "totalCost": rounded_metric(self.total_cost()),
        })
    }
}

impl Default for AttentionCost {
    fn default() -> Self {
        Self::new(100)
    }
}

// ============================================================================
// Risk Reserve
// ============================================================================

/// Budget reserved for unexpected situations and tail risks.
///
/// Ensures the system can respond to rare but critical events.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RiskReserve {
    /// Token budget reserved for emergencies.
    pub token_budget: u32,
    /// Memory slots reserved for critical information.
    pub memory_slots: u32,
    /// Current utilization (0.0 to 1.0).
    pub utilization: f64,
    /// Risk categories covered by this reserve.
    pub covered_risks: Vec<EconomyRiskCategory>,
    /// Minimum reserve level (triggers warning if below).
    pub min_level: f64,
    /// Maximum reserve level (excess can be released).
    pub max_level: f64,
}

impl RiskReserve {
    /// Create a new risk reserve with default settings.
    #[must_use]
    pub fn new(token_budget: u32, memory_slots: u32) -> Self {
        Self {
            token_budget,
            memory_slots,
            utilization: 0.0,
            covered_risks: vec![
                EconomyRiskCategory::SecurityIncident,
                EconomyRiskCategory::DataLoss,
            ],
            min_level: 0.2,
            max_level: 0.8,
        }
    }

    /// Check if reserve is below minimum.
    #[must_use]
    pub fn is_depleted(&self) -> bool {
        self.utilization > (1.0 - self.min_level)
    }

    /// Check if reserve has excess capacity.
    #[must_use]
    pub fn has_excess(&self) -> bool {
        self.utilization < (1.0 - self.max_level)
    }

    /// Calculate available token budget.
    #[must_use]
    pub fn available_tokens(&self) -> u32 {
        ((1.0 - self.utilization) * self.token_budget as f64) as u32
    }

    /// Calculate available memory slots.
    #[must_use]
    pub fn available_slots(&self) -> u32 {
        ((1.0 - self.utilization) * self.memory_slots as f64) as u32
    }

    /// Reserve capacity for a risk.
    pub fn reserve(&mut self, tokens: u32, slots: u32) -> bool {
        let token_use = tokens as f64 / self.token_budget.max(1) as f64;
        let slot_use = slots as f64 / self.memory_slots.max(1) as f64;
        let new_util = self.utilization + token_use.max(slot_use);

        if new_util <= 1.0 {
            self.utilization = new_util;
            true
        } else {
            false
        }
    }

    /// Release reserved capacity.
    pub fn release(&mut self, tokens: u32, slots: u32) {
        let token_release = tokens as f64 / self.token_budget.max(1) as f64;
        let slot_release = slots as f64 / self.memory_slots.max(1) as f64;
        self.utilization = (self.utilization - token_release.max(slot_release)).max(0.0);
    }

    /// Render as JSON.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": RISK_RESERVE_SCHEMA_V1,
            "tokenBudget": self.token_budget,
            "memorySlots": self.memory_slots,
            "utilization": rounded_metric(self.utilization),
            "minLevel": rounded_metric(self.min_level),
            "maxLevel": rounded_metric(self.max_level),
            "availableTokens": self.available_tokens(),
            "availableSlots": self.available_slots(),
            "coveredRisks": self.covered_risks.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
            "isDepleted": self.is_depleted(),
            "hasExcess": self.has_excess(),
        })
    }
}

impl Default for RiskReserve {
    fn default() -> Self {
        Self::new(2000, 10)
    }
}

/// Categories of risk that reserves can cover.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EconomyRiskCategory {
    /// Security incident response.
    SecurityIncident,
    /// Data loss or corruption.
    DataLoss,
    /// Service degradation.
    Degradation,
    /// Compliance violation.
    Compliance,
    /// Performance emergency.
    Performance,
    /// Unknown/other risks.
    Unknown,
}

impl EconomyRiskCategory {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SecurityIncident => "security_incident",
            Self::DataLoss => "data_loss",
            Self::Degradation => "degradation",
            Self::Compliance => "compliance",
            Self::Performance => "performance",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::SecurityIncident,
            Self::DataLoss,
            Self::Degradation,
            Self::Compliance,
            Self::Performance,
            Self::Unknown,
        ]
    }
}

impl fmt::Display for EconomyRiskCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ============================================================================
// Maintenance Debt
// ============================================================================

/// Accumulated maintenance needs for the memory system.
///
/// Tracks deferred work that degrades system quality over time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaintenanceDebt {
    /// Stale memories needing review.
    pub stale_memories: u32,
    /// Orphaned links needing cleanup.
    pub orphaned_links: u32,
    /// Pending consolidation candidates.
    pub pending_consolidations: u32,
    /// Index entries out of sync.
    pub index_drift: u32,
    /// Unvalidated procedural rules.
    pub unvalidated_rules: u32,
    /// Days since last full maintenance sweep.
    pub days_since_sweep: u32,
    /// Overall debt score (0.0 = healthy, 1.0 = critical).
    pub debt_score: f64,
}

impl MaintenanceDebt {
    /// Create a new maintenance debt record.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Calculate debt score from component metrics.
    pub fn recalculate_score(&mut self) {
        let stale_factor = (self.stale_memories as f64 / 100.0).min(1.0) * 0.25;
        let link_factor = (self.orphaned_links as f64 / 50.0).min(1.0) * 0.15;
        let consolidation_factor = (self.pending_consolidations as f64 / 20.0).min(1.0) * 0.15;
        let index_factor = (self.index_drift as f64 / 100.0).min(1.0) * 0.20;
        let rule_factor = (self.unvalidated_rules as f64 / 10.0).min(1.0) * 0.10;
        let time_factor = (self.days_since_sweep as f64 / 30.0).min(1.0) * 0.15;

        self.debt_score = (stale_factor
            + link_factor
            + consolidation_factor
            + index_factor
            + rule_factor
            + time_factor)
            .min(1.0);
    }

    /// Check if maintenance is urgently needed.
    #[must_use]
    pub fn is_urgent(&self) -> bool {
        self.debt_score > 0.7
    }

    /// Check if system is healthy.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.debt_score < 0.3
    }

    /// Get debt level as category.
    #[must_use]
    pub fn level(&self) -> DebtLevel {
        if self.debt_score < 0.3 {
            DebtLevel::Low
        } else if self.debt_score < 0.5 {
            DebtLevel::Moderate
        } else if self.debt_score < 0.7 {
            DebtLevel::High
        } else {
            DebtLevel::Critical
        }
    }

    /// Render as JSON.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": MAINTENANCE_DEBT_SCHEMA_V1,
            "staleMemories": self.stale_memories,
            "orphanedLinks": self.orphaned_links,
            "pendingConsolidations": self.pending_consolidations,
            "indexDrift": self.index_drift,
            "unvalidatedRules": self.unvalidated_rules,
            "daysSinceSweep": self.days_since_sweep,
            "debtScore": rounded_metric(self.debt_score),
            "level": self.level().as_str(),
            "isUrgent": self.is_urgent(),
            "isHealthy": self.is_healthy(),
        })
    }
}

impl Default for MaintenanceDebt {
    fn default() -> Self {
        Self {
            stale_memories: 0,
            orphaned_links: 0,
            pending_consolidations: 0,
            index_drift: 0,
            unvalidated_rules: 0,
            days_since_sweep: 0,
            debt_score: 0.0,
        }
    }
}

/// Debt severity level.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DebtLevel {
    Low,
    Moderate,
    High,
    Critical,
}

impl DebtLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Moderate => "moderate",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

impl fmt::Display for DebtLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ============================================================================
// Economy Recommendation
// ============================================================================

/// Recommendation for managing memory economy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EconomyRecommendation {
    /// Unique recommendation ID.
    pub id: String,
    /// Type of recommendation.
    pub recommendation_type: RecommendationType,
    /// Priority (0-100, higher = more urgent).
    pub priority: u8,
    /// Human-readable title.
    pub title: String,
    /// Detailed description.
    pub description: String,
    /// Expected impact on system health.
    pub expected_impact: Impact,
    /// Estimated effort to implement.
    pub effort: Effort,
    /// Whether action can be taken automatically.
    pub automatable: bool,
    /// Suggested CLI command.
    pub suggested_command: Option<String>,
}

impl EconomyRecommendation {
    /// Create a new recommendation.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        recommendation_type: RecommendationType,
        title: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            recommendation_type,
            priority: 50,
            title: title.into(),
            description: String::new(),
            expected_impact: Impact::Medium,
            effort: Effort::Medium,
            automatable: false,
            suggested_command: None,
        }
    }

    /// Set priority.
    #[must_use]
    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority.min(100);
        self
    }

    /// Set description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Set expected impact.
    #[must_use]
    pub fn with_impact(mut self, impact: Impact) -> Self {
        self.expected_impact = impact;
        self
    }

    /// Set effort level.
    #[must_use]
    pub fn with_effort(mut self, effort: Effort) -> Self {
        self.effort = effort;
        self
    }

    /// Mark as automatable with suggested command.
    #[must_use]
    pub fn automatable_with(mut self, command: impl Into<String>) -> Self {
        self.automatable = true;
        self.suggested_command = Some(command.into());
        self
    }

    /// Calculate priority score considering impact and effort.
    #[must_use]
    pub fn adjusted_priority(&self) -> u8 {
        let impact_factor = match self.expected_impact {
            Impact::Low => 0.7,
            Impact::Medium => 1.0,
            Impact::High => 1.3,
        };
        let effort_factor = match self.effort {
            Effort::Low => 1.2,
            Effort::Medium => 1.0,
            Effort::High => 0.8,
        };
        ((self.priority as f64 * impact_factor * effort_factor) as u8).min(100)
    }

    /// Render as JSON.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        let mut obj = json!({
            "schema": ECONOMY_RECOMMENDATION_SCHEMA_V1,
            "id": self.id,
            "type": self.recommendation_type.as_str(),
            "priority": self.priority,
            "adjustedPriority": self.adjusted_priority(),
            "title": self.title,
            "description": self.description,
            "expectedImpact": self.expected_impact.as_str(),
            "effort": self.effort.as_str(),
            "automatable": self.automatable,
        });

        if let Some(ref cmd) = self.suggested_command {
            obj["suggestedCommand"] = json!(cmd);
        }

        obj
    }
}

/// Type of economy recommendation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationType {
    /// Reduce maintenance debt.
    ReduceDebt,
    /// Optimize attention allocation.
    OptimizeAttention,
    /// Adjust risk reserves.
    AdjustReserves,
    /// Consolidate memories.
    Consolidate,
    /// Archive low-utility memories.
    Archive,
    /// Promote high-utility memories.
    Promote,
    /// Rebalance memory distribution.
    Rebalance,
    /// General improvement.
    Improve,
}

impl RecommendationType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReduceDebt => "reduce_debt",
            Self::OptimizeAttention => "optimize_attention",
            Self::AdjustReserves => "adjust_reserves",
            Self::Consolidate => "consolidate",
            Self::Archive => "archive",
            Self::Promote => "promote",
            Self::Rebalance => "rebalance",
            Self::Improve => "improve",
        }
    }

    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::ReduceDebt,
            Self::OptimizeAttention,
            Self::AdjustReserves,
            Self::Consolidate,
            Self::Archive,
            Self::Promote,
            Self::Rebalance,
            Self::Improve,
        ]
    }
}

impl fmt::Display for RecommendationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Expected impact level.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Impact {
    Low,
    Medium,
    High,
}

impl Impact {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

impl fmt::Display for Impact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Effort level for implementing recommendation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ============================================================================
// Economy Report
// ============================================================================

/// Comprehensive economy health report.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EconomyReport {
    /// Report timestamp.
    pub generated_at: String,
    /// Overall economy health score (0.0 to 1.0).
    pub health_score: f64,
    /// Current risk reserve status.
    pub risk_reserve: RiskReserve,
    /// Current maintenance debt status.
    pub maintenance_debt: MaintenanceDebt,
    /// Aggregate utility metrics.
    pub aggregate_utility: AggregateUtility,
    /// Active recommendations.
    pub recommendations: Vec<EconomyRecommendation>,
}

impl EconomyReport {
    /// Create a new economy report.
    #[must_use]
    pub fn new(generated_at: impl Into<String>) -> Self {
        Self {
            generated_at: generated_at.into(),
            health_score: 0.5,
            risk_reserve: RiskReserve::default(),
            maintenance_debt: MaintenanceDebt::default(),
            aggregate_utility: AggregateUtility::default(),
            recommendations: Vec::new(),
        }
    }

    /// Calculate overall health score.
    pub fn recalculate_health(&mut self) {
        let reserve_health = if self.risk_reserve.is_depleted() {
            0.3
        } else if self.risk_reserve.has_excess() {
            0.9
        } else {
            0.7
        };

        let debt_health = 1.0 - self.maintenance_debt.debt_score;
        let utility_health = self.aggregate_utility.mean_utility;

        self.health_score =
            (reserve_health * 0.3 + debt_health * 0.4 + utility_health * 0.3).clamp(0.0, 1.0);
    }

    /// Add a recommendation.
    pub fn add_recommendation(&mut self, rec: EconomyRecommendation) {
        self.recommendations.push(rec);
    }

    /// Sort recommendations by adjusted priority (highest first).
    pub fn sort_recommendations(&mut self) {
        self.recommendations
            .sort_by_key(|recommendation| Reverse(recommendation.adjusted_priority()));
    }

    /// Render as JSON.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": ECONOMY_REPORT_SCHEMA_V1,
            "generatedAt": self.generated_at,
            "healthScore": rounded_metric(self.health_score),
            "riskReserve": self.risk_reserve.data_json(),
            "maintenanceDebt": self.maintenance_debt.data_json(),
            "aggregateUtility": self.aggregate_utility.data_json(),
            "recommendationCount": self.recommendations.len(),
            "recommendations": self.recommendations.iter().map(|r| r.data_json()).collect::<Vec<_>>(),
        })
    }

    /// Render human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(1024);

        out.push_str("Memory Economy Report\n");
        out.push_str("=====================\n\n");

        out.push_str(&format!(
            "Overall Health: {:.1}%\n\n",
            self.health_score * 100.0
        ));

        out.push_str("Risk Reserve:\n");
        out.push_str(&format!(
            "  Tokens: {}/{} available\n",
            self.risk_reserve.available_tokens(),
            self.risk_reserve.token_budget
        ));
        out.push_str(&format!(
            "  Status: {}\n\n",
            if self.risk_reserve.is_depleted() {
                "DEPLETED"
            } else {
                "OK"
            }
        ));

        out.push_str("Maintenance Debt:\n");
        out.push_str(&format!(
            "  Level: {} ({:.1}%)\n",
            self.maintenance_debt.level(),
            self.maintenance_debt.debt_score * 100.0
        ));
        out.push_str(&format!(
            "  Stale memories: {}\n\n",
            self.maintenance_debt.stale_memories
        ));

        if !self.recommendations.is_empty() {
            out.push_str("Top Recommendations:\n");
            for (i, rec) in self.recommendations.iter().take(3).enumerate() {
                out.push_str(&format!(
                    "  {}. {} (priority: {})\n",
                    i + 1,
                    rec.title,
                    rec.adjusted_priority()
                ));
            }
        }

        out.push_str("\nNext:\n  ee economy report --json\n");
        out
    }
}

/// Aggregate utility metrics across all memories.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AggregateUtility {
    /// Total memories measured.
    pub total_memories: u32,
    /// Mean utility score.
    pub mean_utility: f64,
    /// Median utility score.
    pub median_utility: f64,
    /// Standard deviation.
    pub std_dev: f64,
    /// Memories below utility threshold.
    pub low_utility_count: u32,
    /// Memories above high-value threshold.
    pub high_utility_count: u32,
}

impl AggregateUtility {
    /// Create aggregate utility from a list of utility scores.
    #[must_use]
    pub fn from_scores(scores: &[f64]) -> Self {
        if scores.is_empty() {
            return Self::default();
        }

        let total = scores.len() as u32;
        let mean: f64 = scores.iter().sum::<f64>() / total as f64;

        let mut sorted = scores.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = sorted[sorted.len() / 2];

        let variance: f64 = scores.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / total as f64;
        let std_dev = variance.sqrt();

        let low_utility_count = scores.iter().filter(|&&s| s < 0.3).count() as u32;
        let high_utility_count = scores.iter().filter(|&&s| s > 0.7).count() as u32;

        Self {
            total_memories: total,
            mean_utility: mean,
            median_utility: median,
            std_dev,
            low_utility_count,
            high_utility_count,
        }
    }

    /// Render as JSON.
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "totalMemories": self.total_memories,
            "meanUtility": rounded_metric(self.mean_utility),
            "medianUtility": rounded_metric(self.median_utility),
            "stdDev": rounded_metric(self.std_dev),
            "lowUtilityCount": self.low_utility_count,
            "highUtilityCount": self.high_utility_count,
        })
    }
}

impl Default for AggregateUtility {
    fn default() -> Self {
        Self {
            total_memories: 0,
            mean_utility: 0.5,
            median_utility: 0.5,
            std_dev: 0.0,
            low_utility_count: 0,
            high_utility_count: 0,
        }
    }
}

// ============================================================================
// Schema Catalog
// ============================================================================

/// Field descriptor used by the economy schema catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EconomyFieldSchema {
    pub name: &'static str,
    pub type_name: &'static str,
    pub required: bool,
    pub description: &'static str,
}

impl EconomyFieldSchema {
    #[must_use]
    pub const fn new(
        name: &'static str,
        type_name: &'static str,
        required: bool,
        description: &'static str,
    ) -> Self {
        Self {
            name,
            type_name,
            required,
            description,
        }
    }
}

/// Stable JSON-schema-like catalog entry for economy records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EconomyObjectSchema {
    pub schema_name: &'static str,
    pub schema_uri: &'static str,
    pub kind: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub fields: &'static [EconomyFieldSchema],
}

impl EconomyObjectSchema {
    #[must_use]
    pub fn required_count(&self) -> usize {
        self.fields.iter().filter(|field| field.required).count()
    }
}

const UTILITY_VALUE_FIELDS: &[EconomyFieldSchema] = &[
    EconomyFieldSchema::new("schema", "string", true, "Schema identifier."),
    EconomyFieldSchema::new(
        "score",
        "number",
        true,
        "Raw utility score from 0.0 to 1.0.",
    ),
    EconomyFieldSchema::new(
        "retrievalCount",
        "integer",
        true,
        "Times the artifact was retrieved.",
    ),
    EconomyFieldSchema::new(
        "successCount",
        "integer",
        true,
        "Times retrieval contributed to a successful outcome.",
    ),
    EconomyFieldSchema::new(
        "falseAlarmCount",
        "integer",
        true,
        "Times retrieval wasted attention or produced a false alarm.",
    ),
    EconomyFieldSchema::new(
        "projectedUtility",
        "number",
        true,
        "Projected future utility based on trend evidence.",
    ),
    EconomyFieldSchema::new(
        "confidence",
        "number",
        true,
        "Confidence in the utility estimate from 0.0 to 1.0.",
    ),
    EconomyFieldSchema::new(
        "effective",
        "number",
        true,
        "Utility score adjusted by confidence.",
    ),
    EconomyFieldSchema::new(
        "falseAlarmRate",
        "number",
        true,
        "False alarms divided by retrieval count.",
    ),
];

const ATTENTION_COST_FIELDS: &[EconomyFieldSchema] = &[
    EconomyFieldSchema::new("schema", "string", true, "Schema identifier."),
    EconomyFieldSchema::new(
        "tokenCost",
        "integer",
        true,
        "Token cost of surfacing the artifact.",
    ),
    EconomyFieldSchema::new(
        "cognitiveLoad",
        "number",
        true,
        "Estimated cognitive load from 0.0 to 1.0.",
    ),
    EconomyFieldSchema::new(
        "relevanceDecay",
        "number",
        true,
        "Relevance decay since last use from 0.0 to 1.0.",
    ),
    EconomyFieldSchema::new(
        "contextSwitchCost",
        "number",
        true,
        "Cost of switching context to use the artifact.",
    ),
    EconomyFieldSchema::new(
        "displacementCost",
        "number",
        true,
        "Opportunity cost of displacing other artifacts.",
    ),
    EconomyFieldSchema::new(
        "totalCost",
        "number",
        true,
        "Weighted aggregate attention cost.",
    ),
];

const RISK_RESERVE_FIELDS: &[EconomyFieldSchema] = &[
    EconomyFieldSchema::new("schema", "string", true, "Schema identifier."),
    EconomyFieldSchema::new(
        "tokenBudget",
        "integer",
        true,
        "Token budget reserved for tail-risk coverage.",
    ),
    EconomyFieldSchema::new(
        "memorySlots",
        "integer",
        true,
        "Artifact slots reserved for critical fallback information.",
    ),
    EconomyFieldSchema::new(
        "utilization",
        "number",
        true,
        "Reserve utilization from 0.0 to 1.0.",
    ),
    EconomyFieldSchema::new(
        "coveredRisks",
        "array<string>",
        true,
        "Risk categories covered by the reserve.",
    ),
    EconomyFieldSchema::new(
        "minLevel",
        "number",
        true,
        "Minimum reserve level before depletion warnings.",
    ),
    EconomyFieldSchema::new(
        "maxLevel",
        "number",
        true,
        "Maximum reserve level before excess capacity can be released.",
    ),
    EconomyFieldSchema::new(
        "availableTokens",
        "integer",
        true,
        "Token budget still available in the reserve.",
    ),
    EconomyFieldSchema::new(
        "availableSlots",
        "integer",
        true,
        "Artifact slots still available in the reserve.",
    ),
    EconomyFieldSchema::new(
        "isDepleted",
        "boolean",
        true,
        "Whether reserve capacity is depleted.",
    ),
    EconomyFieldSchema::new(
        "hasExcess",
        "boolean",
        true,
        "Whether reserve capacity is above its target maximum.",
    ),
];

const MAINTENANCE_DEBT_FIELDS: &[EconomyFieldSchema] = &[
    EconomyFieldSchema::new("schema", "string", true, "Schema identifier."),
    EconomyFieldSchema::new(
        "staleMemories",
        "integer",
        true,
        "Stale memories needing review.",
    ),
    EconomyFieldSchema::new(
        "orphanedLinks",
        "integer",
        true,
        "Orphaned links needing cleanup.",
    ),
    EconomyFieldSchema::new(
        "pendingConsolidations",
        "integer",
        true,
        "Consolidation candidates waiting for review.",
    ),
    EconomyFieldSchema::new("indexDrift", "integer", true, "Index entries out of sync."),
    EconomyFieldSchema::new(
        "unvalidatedRules",
        "integer",
        true,
        "Procedural rules that still lack validation evidence.",
    ),
    EconomyFieldSchema::new(
        "daysSinceSweep",
        "integer",
        true,
        "Days since the last full maintenance sweep.",
    ),
    EconomyFieldSchema::new(
        "debtScore",
        "number",
        true,
        "Overall maintenance debt score from 0.0 to 1.0.",
    ),
    EconomyFieldSchema::new(
        "level",
        "string",
        true,
        "Categorical maintenance debt level.",
    ),
    EconomyFieldSchema::new(
        "isUrgent",
        "boolean",
        true,
        "Whether maintenance should be prioritized immediately.",
    ),
    EconomyFieldSchema::new(
        "isHealthy",
        "boolean",
        true,
        "Whether maintenance debt is within the healthy range.",
    ),
];

const ECONOMY_RECOMMENDATION_FIELDS: &[EconomyFieldSchema] = &[
    EconomyFieldSchema::new("schema", "string", true, "Schema identifier."),
    EconomyFieldSchema::new("id", "string", true, "Stable recommendation identifier."),
    EconomyFieldSchema::new("type", "string", true, "Recommendation type."),
    EconomyFieldSchema::new("priority", "integer", true, "Base priority from 0 to 100."),
    EconomyFieldSchema::new(
        "adjustedPriority",
        "integer",
        true,
        "Priority adjusted by expected impact and effort.",
    ),
    EconomyFieldSchema::new(
        "title",
        "string",
        true,
        "Human-readable recommendation title.",
    ),
    EconomyFieldSchema::new("description", "string", true, "Recommendation details."),
    EconomyFieldSchema::new("expectedImpact", "string", true, "Expected impact level."),
    EconomyFieldSchema::new("effort", "string", true, "Estimated implementation effort."),
    EconomyFieldSchema::new(
        "automatable",
        "boolean",
        true,
        "Whether action may be automated.",
    ),
    EconomyFieldSchema::new(
        "suggestedCommand",
        "string|null",
        false,
        "Suggested CLI command when automation is safe.",
    ),
];

const ECONOMY_REPORT_FIELDS: &[EconomyFieldSchema] = &[
    EconomyFieldSchema::new("schema", "string", true, "Schema identifier."),
    EconomyFieldSchema::new("generatedAt", "string", true, "RFC 3339 report timestamp."),
    EconomyFieldSchema::new(
        "healthScore",
        "number",
        true,
        "Overall economy health score.",
    ),
    EconomyFieldSchema::new(
        "riskReserve",
        "object",
        true,
        "Current risk reserve status.",
    ),
    EconomyFieldSchema::new(
        "maintenanceDebt",
        "object",
        true,
        "Current maintenance debt status.",
    ),
    EconomyFieldSchema::new(
        "aggregateUtility",
        "object",
        true,
        "Aggregate utility metrics across artifacts.",
    ),
    EconomyFieldSchema::new(
        "recommendationCount",
        "integer",
        true,
        "Number of active recommendations.",
    ),
    EconomyFieldSchema::new(
        "recommendations",
        "array<object>",
        true,
        "Active economy recommendations sorted by priority.",
    ),
];

#[must_use]
pub const fn economy_schemas() -> [EconomyObjectSchema; 6] {
    [
        EconomyObjectSchema {
            schema_name: UTILITY_VALUE_SCHEMA_V1,
            schema_uri: "urn:ee:schema:economy-utility-value:v1",
            kind: "utility_value",
            title: "UtilityValue",
            description: "Evidence-backed value estimate for surfacing an artifact.",
            fields: UTILITY_VALUE_FIELDS,
        },
        EconomyObjectSchema {
            schema_name: ATTENTION_COST_SCHEMA_V1,
            schema_uri: "urn:ee:schema:economy-attention-cost:v1",
            kind: "attention_cost",
            title: "AttentionCost",
            description: "Token and cognitive cost estimate for surfacing an artifact.",
            fields: ATTENTION_COST_FIELDS,
        },
        EconomyObjectSchema {
            schema_name: RISK_RESERVE_SCHEMA_V1,
            schema_uri: "urn:ee:schema:economy-risk-reserve:v1",
            kind: "risk_reserve",
            title: "RiskReserve",
            description: "Reserved attention budget for tail-risk coverage and fallback evidence.",
            fields: RISK_RESERVE_FIELDS,
        },
        EconomyObjectSchema {
            schema_name: MAINTENANCE_DEBT_SCHEMA_V1,
            schema_uri: "urn:ee:schema:economy-maintenance-debt:v1",
            kind: "maintenance_debt",
            title: "MaintenanceDebt",
            description: "Deferred memory maintenance work that degrades retrieval quality.",
            fields: MAINTENANCE_DEBT_FIELDS,
        },
        EconomyObjectSchema {
            schema_name: ECONOMY_RECOMMENDATION_SCHEMA_V1,
            schema_uri: "urn:ee:schema:economy-recommendation:v1",
            kind: "economy_recommendation",
            title: "EconomyRecommendation",
            description: "Recommended action to improve utility, reserves, or maintenance debt.",
            fields: ECONOMY_RECOMMENDATION_FIELDS,
        },
        EconomyObjectSchema {
            schema_name: ECONOMY_REPORT_SCHEMA_V1,
            schema_uri: "urn:ee:schema:economy-report:v1",
            kind: "economy_report",
            title: "EconomyReport",
            description: "Snapshot report over utility, attention cost, reserves, debt, and recommendations.",
            fields: ECONOMY_REPORT_FIELDS,
        },
    ]
}

#[must_use]
pub fn economy_schema_catalog_json() -> String {
    let schemas = economy_schemas();
    let mut output = String::from("{\n");
    output.push_str(&format!("  \"schema\": \"{ECONOMY_SCHEMA_CATALOG_V1}\",\n"));
    output.push_str("  \"schemas\": [\n");
    for (schema_index, schema) in schemas.iter().enumerate() {
        output.push_str("    {\n");
        output.push_str(&format!(
            "      \"$schema\": \"{JSON_SCHEMA_DRAFT_2020_12}\",\n"
        ));
        output.push_str("      \"$id\": ");
        push_json_string(&mut output, schema.schema_uri);
        output.push_str(",\n");
        output.push_str("      \"eeSchema\": ");
        push_json_string(&mut output, schema.schema_name);
        output.push_str(",\n");
        output.push_str("      \"kind\": ");
        push_json_string(&mut output, schema.kind);
        output.push_str(",\n");
        output.push_str("      \"title\": ");
        push_json_string(&mut output, schema.title);
        output.push_str(",\n");
        output.push_str("      \"description\": ");
        push_json_string(&mut output, schema.description);
        output.push_str(",\n");
        output.push_str("      \"type\": \"object\",\n");
        output.push_str("      \"required\": [\n");
        let mut emitted_required = 0;
        for field in schema.fields {
            if field.required {
                emitted_required += 1;
                output.push_str("        ");
                push_json_string(&mut output, field.name);
                if emitted_required == schema.required_count() {
                    output.push('\n');
                } else {
                    output.push_str(",\n");
                }
            }
        }
        output.push_str("      ],\n");
        output.push_str("      \"fields\": [\n");
        for (field_index, field) in schema.fields.iter().enumerate() {
            output.push_str("        {\"name\": ");
            push_json_string(&mut output, field.name);
            output.push_str(", \"type\": ");
            push_json_string(&mut output, field.type_name);
            output.push_str(", \"required\": ");
            output.push_str(if field.required { "true" } else { "false" });
            output.push_str(", \"description\": ");
            push_json_string(&mut output, field.description);
            if field_index + 1 == schema.fields.len() {
                output.push_str("}\n");
            } else {
                output.push_str("},\n");
            }
        }
        output.push_str("      ],\n");
        output.push_str("      \"additionalProperties\": false\n");
        if schema_index + 1 == schemas.len() {
            output.push_str("    }\n");
        } else {
            output.push_str("    },\n");
        }
    }
    output.push_str("  ]\n");
    output.push_str("}\n");
    output
}

fn push_json_string(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            other => output.push(other),
        }
    }
    output.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    const ECONOMY_SCHEMA_GOLDEN: &str =
        include_str!("../../tests/fixtures/golden/models/economy_schemas.json.golden");

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn ensure_json_number(value: &serde_json::Value, ctx: &str) -> TestResult {
        if value.is_number() {
            Ok(())
        } else {
            Err(format!("{ctx}: expected JSON number, got {value:?}"))
        }
    }

    #[test]
    fn economy_schema_constants_are_stable() -> TestResult {
        ensure(
            UTILITY_VALUE_SCHEMA_V1,
            "ee.economy.utility_value.v1",
            "utility",
        )?;
        ensure(
            ATTENTION_COST_SCHEMA_V1,
            "ee.economy.attention_cost.v1",
            "attention",
        )?;
        ensure(
            RISK_RESERVE_SCHEMA_V1,
            "ee.economy.risk_reserve.v1",
            "reserve",
        )?;
        ensure(
            MAINTENANCE_DEBT_SCHEMA_V1,
            "ee.economy.maintenance_debt.v1",
            "debt",
        )?;
        ensure(
            ECONOMY_RECOMMENDATION_SCHEMA_V1,
            "ee.economy.recommendation.v1",
            "recommendation",
        )?;
        ensure(ECONOMY_REPORT_SCHEMA_V1, "ee.economy.report.v1", "report")?;
        ensure(
            ECONOMY_SCHEMA_CATALOG_V1,
            "ee.economy.schemas.v1",
            "catalog",
        )
    }

    #[test]
    fn utility_value_from_history() {
        let util = UtilityValue::from_history(100, 80, 5);
        assert!(util.score > 0.7);
        assert!(util.confidence > 0.9);
        assert!(util.false_alarm_rate() < 0.1);
    }

    #[test]
    fn utility_value_effective() {
        let util = UtilityValue {
            score: 0.8,
            confidence: 0.5,
            ..UtilityValue::default()
        };
        assert!((util.effective() - 0.4).abs() < 0.01);
    }

    #[test]
    fn attention_cost_total() {
        let cost = AttentionCost::new(500)
            .with_cognitive_load(0.5)
            .with_relevance_decay(0.2);
        assert!(cost.total_cost() > 0.5);
    }

    #[test]
    fn risk_reserve_operations() {
        let mut reserve = RiskReserve::new(1000, 10);
        assert!(!reserve.is_depleted());
        assert!(reserve.has_excess());

        assert!(reserve.reserve(800, 8));
        assert!(!reserve.has_excess());

        reserve.release(400, 4);
        assert!(!reserve.is_depleted());
    }

    #[test]
    fn maintenance_debt_scoring() {
        let mut debt = MaintenanceDebt {
            stale_memories: 50,
            orphaned_links: 20,
            pending_consolidations: 10,
            index_drift: 30,
            unvalidated_rules: 5,
            days_since_sweep: 15,
            debt_score: 0.0,
        };
        debt.recalculate_score();
        assert!(debt.debt_score > 0.3);
        assert!(!debt.is_healthy());
    }

    #[test]
    fn debt_level_categorization() {
        let mut debt = MaintenanceDebt::new();
        debt.debt_score = 0.2;
        assert_eq!(debt.level(), DebtLevel::Low);

        debt.debt_score = 0.5;
        assert_eq!(debt.level(), DebtLevel::High);

        debt.debt_score = 0.8;
        assert_eq!(debt.level(), DebtLevel::Critical);
    }

    #[test]
    fn recommendation_adjusted_priority() {
        let rec = EconomyRecommendation::new("r1", RecommendationType::ReduceDebt, "Test")
            .with_priority(50)
            .with_impact(Impact::High)
            .with_effort(Effort::Low);

        assert!(rec.adjusted_priority() > 50);
    }

    #[test]
    fn recommendation_json_has_schema() {
        let rec = EconomyRecommendation::new("r2", RecommendationType::Archive, "Archive test");
        let json = rec.data_json();
        assert_eq!(json["schema"], ECONOMY_RECOMMENDATION_SCHEMA_V1);
    }

    #[test]
    fn utility_json_has_schema() {
        let json = UtilityValue::new(0.75).data_json();
        assert_eq!(json["schema"], UTILITY_VALUE_SCHEMA_V1);
    }

    #[test]
    fn data_json_numeric_metrics_are_json_numbers() -> TestResult {
        let utility = UtilityValue::from_history(3, 2, 1).data_json();
        ensure_json_number(&utility["score"], "utility score")?;
        ensure_json_number(&utility["projectedUtility"], "utility projected")?;
        ensure_json_number(&utility["confidence"], "utility confidence")?;
        ensure_json_number(&utility["effective"], "utility effective")?;
        ensure_json_number(&utility["falseAlarmRate"], "utility false alarm")?;

        let attention = AttentionCost::new(333)
            .with_cognitive_load(0.4567)
            .with_relevance_decay(0.2)
            .with_context_switch(0.1)
            .data_json();
        ensure_json_number(&attention["cognitiveLoad"], "attention cognitive load")?;
        ensure_json_number(&attention["totalCost"], "attention total cost")?;

        let mut reserve = RiskReserve::new(1000, 10);
        assert!(reserve.reserve(333, 3));
        let reserve = reserve.data_json();
        ensure_json_number(&reserve["utilization"], "reserve utilization")?;
        ensure_json_number(&reserve["minLevel"], "reserve min level")?;
        ensure_json_number(&reserve["maxLevel"], "reserve max level")?;

        let mut debt = MaintenanceDebt::new();
        debt.stale_memories = 3;
        debt.recalculate_score();
        let debt = debt.data_json();
        ensure_json_number(&debt["debtScore"], "debt score")?;

        let aggregate = AggregateUtility::from_scores(&[0.1, 0.2, 0.3]).data_json();
        ensure_json_number(&aggregate["meanUtility"], "aggregate mean")?;
        ensure_json_number(&aggregate["medianUtility"], "aggregate median")?;
        ensure_json_number(&aggregate["stdDev"], "aggregate std dev")?;

        let mut report = EconomyReport::new("2026-04-30T12:00:00Z");
        report.recalculate_health();
        let report = report.data_json();
        ensure_json_number(&report["healthScore"], "report health")
    }

    #[test]
    fn aggregate_utility_from_scores() {
        let scores = vec![0.2, 0.4, 0.5, 0.6, 0.8, 0.9];
        let agg = AggregateUtility::from_scores(&scores);

        assert_eq!(agg.total_memories, 6);
        assert!(agg.mean_utility > 0.5);
        assert_eq!(agg.low_utility_count, 1);
        assert_eq!(agg.high_utility_count, 2);
    }

    #[test]
    fn economy_report_health_calculation() {
        let mut report = EconomyReport::new("2026-04-30T12:00:00Z");
        report.maintenance_debt.debt_score = 0.3;
        report.aggregate_utility.mean_utility = 0.7;
        report.recalculate_health();

        assert!(report.health_score > 0.5);
    }

    #[test]
    fn economy_report_json_has_schema() {
        let report = EconomyReport::new("2026-04-30T12:00:00Z");
        let json = report.data_json();
        assert_eq!(json["schema"], ECONOMY_REPORT_SCHEMA_V1);
    }

    #[test]
    fn risk_category_all() {
        let all = EconomyRiskCategory::all();
        assert!(all.len() >= 5);
        assert!(all.contains(&EconomyRiskCategory::SecurityIncident));
    }

    #[test]
    fn recommendation_type_all() {
        let all = RecommendationType::all();
        assert!(all.len() >= 7);
        assert!(all.contains(&RecommendationType::ReduceDebt));
    }

    #[test]
    fn economy_schema_catalog_order_is_stable() -> TestResult {
        let schemas = economy_schemas();
        ensure(schemas.len(), 6, "schema count")?;
        ensure(schemas[0].schema_name, UTILITY_VALUE_SCHEMA_V1, "utility")?;
        ensure(
            schemas[1].schema_name,
            ATTENTION_COST_SCHEMA_V1,
            "attention cost",
        )?;
        ensure(schemas[2].schema_name, RISK_RESERVE_SCHEMA_V1, "reserve")?;
        ensure(schemas[3].schema_name, MAINTENANCE_DEBT_SCHEMA_V1, "debt")?;
        ensure(
            schemas[4].schema_name,
            ECONOMY_RECOMMENDATION_SCHEMA_V1,
            "recommendation",
        )?;
        ensure(schemas[5].schema_name, ECONOMY_REPORT_SCHEMA_V1, "report")
    }

    #[test]
    fn economy_schema_catalog_matches_golden_fixture() {
        assert_eq!(economy_schema_catalog_json(), ECONOMY_SCHEMA_GOLDEN);
    }

    #[test]
    fn economy_schema_catalog_is_valid_json() -> TestResult {
        let parsed: serde_json::Value = serde_json::from_str(ECONOMY_SCHEMA_GOLDEN)
            .map_err(|error| format!("economy schema golden must be valid JSON: {error}"))?;
        ensure(
            parsed.get("schema").and_then(serde_json::Value::as_str),
            Some(ECONOMY_SCHEMA_CATALOG_V1),
            "catalog schema",
        )?;
        let schemas = parsed
            .get("schemas")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "schemas must be an array".to_string())?;
        ensure(schemas.len(), 6, "catalog length")
    }
}
