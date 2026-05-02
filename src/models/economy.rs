//! Memory economy and attention budget types (EE-430).
//!
//! Treat agent attention as scarce: score utility, cost, false alarms,
//! maintenance debt, and tail-risk reserves before surfacing or demoting artifacts.

use std::{cmp::Reverse, fmt};

use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};

/// Schema identifier for attention cost records.
pub const ATTENTION_COST_SCHEMA_V1: &str = "ee.economy.attention_cost.v1";

/// Schema identifier for attention budget calculations.
pub const ATTENTION_BUDGET_SCHEMA_V1: &str = "ee.economy.attention_budget.v1";

/// Schema identifier for utility value records.
pub const UTILITY_VALUE_SCHEMA_V1: &str = "ee.economy.utility_value.v1";

/// Schema identifier for risk reserve records.
pub const RISK_RESERVE_SCHEMA_V1: &str = "ee.economy.risk_reserve.v1";

/// Schema identifier for tail-risk reserve rule records.
pub const TAIL_RISK_RESERVE_RULE_SCHEMA_V1: &str = "ee.economy.tail_risk_reserve_rule.v1";

/// Schema identifier for maintenance debt records.
pub const MAINTENANCE_DEBT_SCHEMA_V1: &str = "ee.economy.maintenance_debt.v1";

/// Schema identifier for economy recommendation records.
pub const ECONOMY_RECOMMENDATION_SCHEMA_V1: &str = "ee.economy.recommendation.v1";

/// Schema identifier for economy report.
pub const ECONOMY_REPORT_SCHEMA_V1: &str = "ee.economy.report.v1";

/// Schema identifier for economy budget simulation reports.
pub const ECONOMY_SIMULATION_SCHEMA_V1: &str = "ee.economy.simulation.v1";

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
// Attention Budget Calculation
// ============================================================================

/// Context packing profile used for attention budget allocation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextAttentionProfile {
    Compact,
    Balanced,
    Thorough,
    Submodular,
    Broad,
}

impl ContextAttentionProfile {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Balanced => "balanced",
            Self::Thorough => "thorough",
            Self::Submodular => "submodular",
            Self::Broad => "broad",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "compact" => Some(Self::Compact),
            "balanced" => Some(Self::Balanced),
            "thorough" => Some(Self::Thorough),
            "submodular" => Some(Self::Submodular),
            "broad" => Some(Self::Broad),
            _ => None,
        }
    }

    const fn base(self) -> BudgetBasisPoints {
        match self {
            Self::Compact => BudgetBasisPoints::new(6_800, 1_400, 600, 800, 400, 8, 0.20),
            Self::Balanced => BudgetBasisPoints::new(5_900, 1_900, 900, 900, 400, 12, 0.30),
            Self::Thorough => BudgetBasisPoints::new(5_000, 2_300, 1_100, 1_200, 400, 18, 0.45),
            Self::Submodular => BudgetBasisPoints::new(5_300, 2_100, 800, 1_400, 400, 16, 0.35),
            Self::Broad => BudgetBasisPoints::new(4_400, 2_200, 900, 2_000, 500, 20, 0.55),
        }
    }
}

impl fmt::Display for ContextAttentionProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Situation routing profile used to adjust attention reserves.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SituationAttentionProfile {
    Minimal,
    Summary,
    Standard,
    Full,
}

impl SituationAttentionProfile {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Summary => "summary",
            Self::Standard => "standard",
            Self::Full => "full",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "minimal" => Some(Self::Minimal),
            "summary" => Some(Self::Summary),
            "standard" => Some(Self::Standard),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    const fn adjustment(self) -> BudgetBasisAdjustment {
        match self {
            Self::Minimal => BudgetBasisAdjustment::new(300, 0, 0, -200, -100, -4, -0.05),
            Self::Summary => BudgetBasisAdjustment::new(-300, 100, 0, 200, 0, -2, 0.00),
            Self::Standard => BudgetBasisAdjustment::new(-500, 0, 100, 400, 0, 0, 0.05),
            Self::Full => BudgetBasisAdjustment::new(-1_400, 300, 200, 900, 0, 4, 0.15),
        }
    }
}

impl fmt::Display for SituationAttentionProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BudgetBasisPoints {
    retrieval: i32,
    evidence: i32,
    procedure: i32,
    risk_reserve: i32,
    maintenance: i32,
    max_items: i32,
    cognitive_load_basis_points: u16,
}

impl BudgetBasisPoints {
    const fn new(
        retrieval: i32,
        evidence: i32,
        procedure: i32,
        risk_reserve: i32,
        maintenance: i32,
        max_items: i32,
        cognitive_load: f64,
    ) -> Self {
        Self {
            retrieval,
            evidence,
            procedure,
            risk_reserve,
            maintenance,
            max_items,
            cognitive_load_basis_points: (cognitive_load * 10_000.0) as u16,
        }
    }

    fn apply(self, adjustment: BudgetBasisAdjustment) -> Self {
        Self {
            retrieval: self.retrieval + adjustment.retrieval,
            evidence: self.evidence + adjustment.evidence,
            procedure: self.procedure + adjustment.procedure,
            risk_reserve: self.risk_reserve + adjustment.risk_reserve,
            maintenance: self.maintenance + adjustment.maintenance,
            max_items: (self.max_items + adjustment.max_items).max(1),
            cognitive_load_basis_points: ((self.cognitive_load_basis_points as i32)
                + adjustment.cognitive_load_basis_points)
                .clamp(0, 10_000) as u16,
        }
    }

    fn cognitive_load(self) -> f64 {
        f64::from(self.cognitive_load_basis_points) / 10_000.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BudgetBasisAdjustment {
    retrieval: i32,
    evidence: i32,
    procedure: i32,
    risk_reserve: i32,
    maintenance: i32,
    max_items: i32,
    cognitive_load_basis_points: i32,
}

impl BudgetBasisAdjustment {
    const fn new(
        retrieval: i32,
        evidence: i32,
        procedure: i32,
        risk_reserve: i32,
        maintenance: i32,
        max_items: i32,
        cognitive_load: f64,
    ) -> Self {
        Self {
            retrieval,
            evidence,
            procedure,
            risk_reserve,
            maintenance,
            max_items,
            cognitive_load_basis_points: (cognitive_load * 10_000.0) as i32,
        }
    }
}

/// Input shape for deterministic attention budget calculation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AttentionBudgetRequest {
    pub total_tokens: u32,
    pub context_profile: ContextAttentionProfile,
    pub situation_profile: SituationAttentionProfile,
}

impl AttentionBudgetRequest {
    #[must_use]
    pub const fn new(
        total_tokens: u32,
        context_profile: ContextAttentionProfile,
        situation_profile: SituationAttentionProfile,
    ) -> Self {
        Self {
            total_tokens,
            context_profile,
            situation_profile,
        }
    }
}

/// Deterministic allocation of context attention across retrieval, evidence, and reserves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttentionBudgetAllocation {
    pub total_tokens: u32,
    pub context_profile: ContextAttentionProfile,
    pub situation_profile: SituationAttentionProfile,
    pub retrieval_tokens: u32,
    pub evidence_tokens: u32,
    pub procedure_tokens: u32,
    pub risk_reserve_tokens: u32,
    pub maintenance_tokens: u32,
    pub max_items: u32,
    pub attention_cost: AttentionCost,
    pub reasons: Vec<String>,
}

impl AttentionBudgetAllocation {
    #[must_use]
    pub fn calculate(request: AttentionBudgetRequest) -> Self {
        let basis = request
            .context_profile
            .base()
            .apply(request.situation_profile.adjustment());

        let retrieval_tokens = budget_slice(request.total_tokens, basis.retrieval);
        let evidence_tokens = budget_slice(request.total_tokens, basis.evidence);
        let procedure_tokens = budget_slice(request.total_tokens, basis.procedure);
        let risk_reserve_tokens = budget_slice(request.total_tokens, basis.risk_reserve);
        let allocated = retrieval_tokens
            .saturating_add(evidence_tokens)
            .saturating_add(procedure_tokens)
            .saturating_add(risk_reserve_tokens);
        let maintenance_tokens = request.total_tokens.saturating_sub(allocated);
        let surfaced_tokens = retrieval_tokens
            .saturating_add(evidence_tokens)
            .saturating_add(procedure_tokens);
        let attention_cost = AttentionCost::new(surfaced_tokens)
            .with_cognitive_load(basis.cognitive_load())
            .with_context_switch(context_switch_for(request.situation_profile))
            .with_relevance_decay(relevance_decay_for(request.context_profile));

        Self {
            total_tokens: request.total_tokens,
            context_profile: request.context_profile,
            situation_profile: request.situation_profile,
            retrieval_tokens,
            evidence_tokens,
            procedure_tokens,
            risk_reserve_tokens,
            maintenance_tokens,
            max_items: basis.max_items as u32,
            attention_cost,
            reasons: budget_reasons(request.context_profile, request.situation_profile),
        }
    }

    #[must_use]
    pub const fn used_tokens(&self) -> u32 {
        self.retrieval_tokens
            + self.evidence_tokens
            + self.procedure_tokens
            + self.risk_reserve_tokens
            + self.maintenance_tokens
    }

    #[must_use]
    pub fn reserve_ratio(&self) -> f64 {
        if self.total_tokens == 0 {
            0.0
        } else {
            f64::from(self.risk_reserve_tokens) / f64::from(self.total_tokens)
        }
    }

    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": ATTENTION_BUDGET_SCHEMA_V1,
            "totalTokens": self.total_tokens,
            "usedTokens": self.used_tokens(),
            "contextProfile": self.context_profile.as_str(),
            "situationProfile": self.situation_profile.as_str(),
            "retrievalTokens": self.retrieval_tokens,
            "evidenceTokens": self.evidence_tokens,
            "procedureTokens": self.procedure_tokens,
            "riskReserveTokens": self.risk_reserve_tokens,
            "maintenanceTokens": self.maintenance_tokens,
            "reserveRatio": rounded_metric(self.reserve_ratio()),
            "maxItems": self.max_items,
            "attentionCost": self.attention_cost.data_json(),
            "reasons": self.reasons,
        })
    }
}

fn budget_slice(total_tokens: u32, basis_points: i32) -> u32 {
    let basis = u64::try_from(basis_points.max(0)).unwrap_or(0);
    ((u64::from(total_tokens) * basis) / 10_000) as u32
}

fn context_switch_for(profile: SituationAttentionProfile) -> f64 {
    match profile {
        SituationAttentionProfile::Minimal => 0.05,
        SituationAttentionProfile::Summary => 0.10,
        SituationAttentionProfile::Standard => 0.18,
        SituationAttentionProfile::Full => 0.30,
    }
}

fn relevance_decay_for(profile: ContextAttentionProfile) -> f64 {
    match profile {
        ContextAttentionProfile::Compact => 0.05,
        ContextAttentionProfile::Balanced | ContextAttentionProfile::Submodular => 0.10,
        ContextAttentionProfile::Thorough => 0.15,
        ContextAttentionProfile::Broad => 0.20,
    }
}

fn budget_reasons(
    context_profile: ContextAttentionProfile,
    situation_profile: SituationAttentionProfile,
) -> Vec<String> {
    vec![
        format!(
            "{} context profile sets the base retrieval/evidence/reserve split",
            context_profile.as_str()
        ),
        format!(
            "{} situation profile adjusts tail-risk reserve and item count",
            situation_profile.as_str()
        ),
    ]
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

    /// Check whether this reserve has enough capacity for a tail-risk rule.
    #[must_use]
    pub fn can_cover_tail_risk_rule(&self, rule: &TailRiskReserveRule) -> bool {
        rule.blocks_popularity_demotion()
            && self.available_tokens() >= rule.effective_reserve_tokens()
            && self.available_slots() >= rule.effective_reserve_slots()
            && self.covered_risks.contains(&rule.risk_category)
    }

    /// Reserve capacity for a tail-risk rule when popularity demotion is blocked.
    pub fn reserve_tail_risk_rule(&mut self, rule: &TailRiskReserveRule) -> bool {
        if !self.can_cover_tail_risk_rule(rule) {
            return false;
        }
        self.reserve(
            rule.effective_reserve_tokens(),
            rule.effective_reserve_slots(),
        )
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

/// Artifact kinds eligible for tail-risk reserve rules.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TailRiskArtifactKind {
    Warning,
    Procedure,
    Tripwire,
    Memory,
    Other,
}

impl TailRiskArtifactKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Procedure => "procedure",
            Self::Tripwire => "tripwire",
            Self::Memory => "memory",
            Self::Other => "other",
        }
    }

    #[must_use]
    pub const fn is_protectable(self) -> bool {
        matches!(self, Self::Warning | Self::Procedure)
    }
}

impl fmt::Display for TailRiskArtifactKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Severity of a rare tail-risk artifact.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TailRiskSeverity {
    Low,
    Medium,
    High,
    Critical,
}

impl TailRiskSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    #[must_use]
    pub const fn is_high_severity(self) -> bool {
        matches!(self, Self::High | Self::Critical)
    }
}

impl fmt::Display for TailRiskSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Deterministic action taken by a tail-risk reserve rule.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TailRiskDemotionAction {
    Protect,
    ManualReview,
    AllowPopularityDemotion,
}

impl TailRiskDemotionAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Protect => "protect",
            Self::ManualReview => "manual_review",
            Self::AllowPopularityDemotion => "allow_popularity_demotion",
        }
    }
}

impl fmt::Display for TailRiskDemotionAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Rule that prevents rare high-severity warnings/procedures from being demoted
/// solely because they are unpopular.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TailRiskReserveRule {
    pub rule_id: String,
    pub artifact_id: String,
    pub artifact_kind: TailRiskArtifactKind,
    pub severity: TailRiskSeverity,
    pub risk_category: EconomyRiskCategory,
    pub supporting_evidence_count: u32,
    pub historical_trigger_count: u32,
    pub retrieval_count: u32,
    pub false_alarm_count: u32,
    pub popularity_score: f64,
    pub utility_score: f64,
    pub reserve_tokens: u32,
    pub reserve_slots: u32,
}

impl TailRiskReserveRule {
    const RARE_POPULARITY_MAX: f64 = 0.20;
    const RARE_TRIGGER_MAX: u32 = 2;
    const FALSE_ALARM_REVIEW_MIN_COUNT: u32 = 3;
    const FALSE_ALARM_REVIEW_RATE: f64 = 0.60;

    #[must_use]
    pub fn new(
        rule_id: impl Into<String>,
        artifact_id: impl Into<String>,
        artifact_kind: TailRiskArtifactKind,
        severity: TailRiskSeverity,
        risk_category: EconomyRiskCategory,
    ) -> Self {
        Self {
            rule_id: rule_id.into(),
            artifact_id: artifact_id.into(),
            artifact_kind,
            severity,
            risk_category,
            supporting_evidence_count: 0,
            historical_trigger_count: 0,
            retrieval_count: 0,
            false_alarm_count: 0,
            popularity_score: 0.0,
            utility_score: 0.5,
            reserve_tokens: default_tail_risk_reserve_tokens(severity),
            reserve_slots: default_tail_risk_reserve_slots(severity),
        }
    }

    #[must_use]
    pub fn with_supporting_evidence(mut self, count: u32) -> Self {
        self.supporting_evidence_count = count;
        self
    }

    #[must_use]
    pub fn with_historical_triggers(mut self, count: u32) -> Self {
        self.historical_trigger_count = count;
        self
    }

    #[must_use]
    pub fn with_retrievals(mut self, count: u32) -> Self {
        self.retrieval_count = count;
        self
    }

    #[must_use]
    pub fn with_false_alarms(mut self, count: u32) -> Self {
        self.false_alarm_count = count;
        self
    }

    #[must_use]
    pub fn with_popularity_score(mut self, score: f64) -> Self {
        self.popularity_score = score.clamp(0.0, 1.0);
        self
    }

    #[must_use]
    pub fn with_utility_score(mut self, score: f64) -> Self {
        self.utility_score = score.clamp(0.0, 1.0);
        self
    }

    #[must_use]
    pub fn with_reserve(mut self, tokens: u32, slots: u32) -> Self {
        self.reserve_tokens = tokens;
        self.reserve_slots = slots;
        self
    }

    #[must_use]
    pub const fn is_protectable_artifact(&self) -> bool {
        self.artifact_kind.is_protectable()
    }

    #[must_use]
    pub const fn is_high_severity(&self) -> bool {
        self.severity.is_high_severity()
    }

    #[must_use]
    pub const fn has_tail_evidence(&self) -> bool {
        self.supporting_evidence_count > 0 || self.historical_trigger_count > 0
    }

    #[must_use]
    pub fn is_rare_signal(&self) -> bool {
        self.popularity_score <= Self::RARE_POPULARITY_MAX
            || self.historical_trigger_count <= Self::RARE_TRIGGER_MAX
    }

    #[must_use]
    pub fn false_alarm_rate(&self) -> f64 {
        if self.retrieval_count == 0 {
            0.0
        } else {
            self.false_alarm_count as f64 / self.retrieval_count as f64
        }
    }

    #[must_use]
    pub fn requires_manual_review(&self) -> bool {
        self.false_alarm_count >= Self::FALSE_ALARM_REVIEW_MIN_COUNT
            && self.false_alarm_rate() >= Self::FALSE_ALARM_REVIEW_RATE
            && self.is_protectable_artifact()
            && self.is_high_severity()
            && self.has_tail_evidence()
    }

    #[must_use]
    pub fn demotion_action(&self) -> TailRiskDemotionAction {
        if !self.is_protectable_artifact()
            || !self.is_high_severity()
            || !self.has_tail_evidence()
            || !self.is_rare_signal()
        {
            TailRiskDemotionAction::AllowPopularityDemotion
        } else if self.requires_manual_review() {
            TailRiskDemotionAction::ManualReview
        } else {
            TailRiskDemotionAction::Protect
        }
    }

    #[must_use]
    pub fn blocks_popularity_demotion(&self) -> bool {
        matches!(
            self.demotion_action(),
            TailRiskDemotionAction::Protect | TailRiskDemotionAction::ManualReview
        )
    }

    #[must_use]
    pub fn effective_reserve_tokens(&self) -> u32 {
        if self.blocks_popularity_demotion() {
            self.reserve_tokens
                .max(default_tail_risk_reserve_tokens(self.severity))
        } else {
            0
        }
    }

    #[must_use]
    pub fn effective_reserve_slots(&self) -> u32 {
        if self.blocks_popularity_demotion() {
            self.reserve_slots
                .max(default_tail_risk_reserve_slots(self.severity))
        } else {
            0
        }
    }

    #[must_use]
    pub fn reasons(&self) -> Vec<String> {
        let mut reasons = Vec::with_capacity(6);

        if self.is_protectable_artifact() {
            reasons.push(format!(
                "{} artifacts are eligible for tail-risk reserve protection",
                self.artifact_kind
            ));
        } else {
            reasons.push(format!(
                "{} artifacts are not protected by warning/procedure reserve rules",
                self.artifact_kind
            ));
        }

        if self.is_high_severity() {
            reasons.push(format!(
                "{} severity outranks popularity-only demotion",
                self.severity
            ));
        } else {
            reasons.push(format!(
                "{} severity does not qualify for tail-risk reserve protection",
                self.severity
            ));
        }

        if self.has_tail_evidence() {
            reasons.push(format!(
                "{} supporting evidence item(s) and {} historical trigger(s) justify treating the artifact as evidence-backed",
                self.supporting_evidence_count, self.historical_trigger_count
            ));
        } else {
            reasons.push(
                "no supporting evidence or historical trigger exists, so reserve protection is disabled"
                    .to_string(),
            );
        }

        if self.is_rare_signal() {
            reasons.push(format!(
                "popularity {:.3} or {} historical trigger(s) classify the artifact as rare",
                rounded_metric(self.popularity_score),
                self.historical_trigger_count
            ));
        } else {
            reasons.push(format!(
                "popularity {:.3} and {} historical trigger(s) do not classify the artifact as rare",
                rounded_metric(self.popularity_score),
                self.historical_trigger_count
            ));
        }

        if self.requires_manual_review() {
            reasons.push(format!(
                "false-alarm rate {:.3} requires manual review before any demotion",
                rounded_metric(self.false_alarm_rate())
            ));
        }

        reasons.push(match self.demotion_action() {
            TailRiskDemotionAction::Protect => {
                "popularity demotion is blocked and reserve capacity is retained".to_string()
            }
            TailRiskDemotionAction::ManualReview => {
                "popularity demotion is blocked until a reviewer evaluates the tail-risk evidence"
                    .to_string()
            }
            TailRiskDemotionAction::AllowPopularityDemotion => {
                "popularity demotion may proceed because tail-risk reserve criteria are not met"
                    .to_string()
            }
        });

        reasons
    }

    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": TAIL_RISK_RESERVE_RULE_SCHEMA_V1,
            "ruleId": self.rule_id,
            "artifactId": self.artifact_id,
            "artifactKind": self.artifact_kind.as_str(),
            "severity": self.severity.as_str(),
            "riskCategory": self.risk_category.as_str(),
            "supportingEvidenceCount": self.supporting_evidence_count,
            "historicalTriggerCount": self.historical_trigger_count,
            "retrievalCount": self.retrieval_count,
            "falseAlarmCount": self.false_alarm_count,
            "falseAlarmRate": rounded_metric(self.false_alarm_rate()),
            "popularityScore": rounded_metric(self.popularity_score),
            "utilityScore": rounded_metric(self.utility_score),
            "rareSignal": self.is_rare_signal(),
            "highSeverity": self.is_high_severity(),
            "protectableArtifact": self.is_protectable_artifact(),
            "demotionAction": self.demotion_action().as_str(),
            "blocksPopularityDemotion": self.blocks_popularity_demotion(),
            "requiresManualReview": self.requires_manual_review(),
            "reserveTokens": self.effective_reserve_tokens(),
            "reserveSlots": self.effective_reserve_slots(),
            "reasons": self.reasons(),
        })
    }
}

const fn default_tail_risk_reserve_tokens(severity: TailRiskSeverity) -> u32 {
    match severity {
        TailRiskSeverity::Low => 0,
        TailRiskSeverity::Medium => 128,
        TailRiskSeverity::High => 512,
        TailRiskSeverity::Critical => 1024,
    }
}

const fn default_tail_risk_reserve_slots(severity: TailRiskSeverity) -> u32 {
    match severity {
        TailRiskSeverity::Low | TailRiskSeverity::Medium => 0,
        TailRiskSeverity::High => 1,
        TailRiskSeverity::Critical => 2,
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

const ATTENTION_BUDGET_FIELDS: &[EconomyFieldSchema] = &[
    EconomyFieldSchema::new("schema", "string", true, "Schema identifier."),
    EconomyFieldSchema::new(
        "totalTokens",
        "integer",
        true,
        "Total attention token budget being allocated.",
    ),
    EconomyFieldSchema::new(
        "usedTokens",
        "integer",
        true,
        "Allocated tokens across all attention buckets.",
    ),
    EconomyFieldSchema::new(
        "contextProfile",
        "string",
        true,
        "Context packing profile used as the base allocation.",
    ),
    EconomyFieldSchema::new(
        "situationProfile",
        "string",
        true,
        "Situation routing profile used as the adjustment.",
    ),
    EconomyFieldSchema::new(
        "retrievalTokens",
        "integer",
        true,
        "Tokens reserved for retrieved memories and search results.",
    ),
    EconomyFieldSchema::new(
        "evidenceTokens",
        "integer",
        true,
        "Tokens reserved for provenance, proof, and supporting evidence.",
    ),
    EconomyFieldSchema::new(
        "procedureTokens",
        "integer",
        true,
        "Tokens reserved for procedural guidance.",
    ),
    EconomyFieldSchema::new(
        "riskReserveTokens",
        "integer",
        true,
        "Tokens held back for rare high-severity or low-confidence cases.",
    ),
    EconomyFieldSchema::new(
        "maintenanceTokens",
        "integer",
        true,
        "Tokens reserved for maintenance/degradation notices.",
    ),
    EconomyFieldSchema::new(
        "reserveRatio",
        "number",
        true,
        "Risk reserve as a fraction of total tokens.",
    ),
    EconomyFieldSchema::new(
        "maxItems",
        "integer",
        true,
        "Maximum surfaced item count for the allocation.",
    ),
    EconomyFieldSchema::new(
        "attentionCost",
        "object",
        true,
        "Derived attention cost for surfaced tokens.",
    ),
    EconomyFieldSchema::new(
        "reasons",
        "array<string>",
        true,
        "Deterministic explanation of the selected allocation.",
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

const TAIL_RISK_RESERVE_RULE_FIELDS: &[EconomyFieldSchema] = &[
    EconomyFieldSchema::new("schema", "string", true, "Schema identifier."),
    EconomyFieldSchema::new("ruleId", "string", true, "Stable reserve rule identifier."),
    EconomyFieldSchema::new(
        "artifactId",
        "string",
        true,
        "Artifact protected or released by the rule.",
    ),
    EconomyFieldSchema::new(
        "artifactKind",
        "string",
        true,
        "Artifact kind: warning, procedure, tripwire, memory, or other.",
    ),
    EconomyFieldSchema::new(
        "severity",
        "string",
        true,
        "Tail-risk severity assigned to the artifact.",
    ),
    EconomyFieldSchema::new("riskCategory", "string", true, "Risk category covered."),
    EconomyFieldSchema::new(
        "supportingEvidenceCount",
        "integer",
        true,
        "Evidence items that justify protection.",
    ),
    EconomyFieldSchema::new(
        "historicalTriggerCount",
        "integer",
        true,
        "Observed historical triggers for the tail risk.",
    ),
    EconomyFieldSchema::new(
        "retrievalCount",
        "integer",
        true,
        "Times the artifact was retrieved.",
    ),
    EconomyFieldSchema::new(
        "falseAlarmCount",
        "integer",
        true,
        "Times the artifact produced a false alarm.",
    ),
    EconomyFieldSchema::new(
        "falseAlarmRate",
        "number",
        true,
        "False alarms divided by retrieval count.",
    ),
    EconomyFieldSchema::new(
        "popularityScore",
        "number",
        true,
        "Observed popularity from 0.0 to 1.0.",
    ),
    EconomyFieldSchema::new(
        "utilityScore",
        "number",
        true,
        "Current utility score from 0.0 to 1.0.",
    ),
    EconomyFieldSchema::new(
        "rareSignal",
        "boolean",
        true,
        "Whether the artifact is rare enough for reserve protection.",
    ),
    EconomyFieldSchema::new(
        "highSeverity",
        "boolean",
        true,
        "Whether severity is high or critical.",
    ),
    EconomyFieldSchema::new(
        "protectableArtifact",
        "boolean",
        true,
        "Whether the artifact kind is a warning or procedure.",
    ),
    EconomyFieldSchema::new(
        "demotionAction",
        "string",
        true,
        "Deterministic demotion decision.",
    ),
    EconomyFieldSchema::new(
        "blocksPopularityDemotion",
        "boolean",
        true,
        "Whether popularity-only demotion is blocked.",
    ),
    EconomyFieldSchema::new(
        "requiresManualReview",
        "boolean",
        true,
        "Whether false-alarm pressure requires manual review.",
    ),
    EconomyFieldSchema::new(
        "reserveTokens",
        "integer",
        true,
        "Tokens retained in reserve while demotion is blocked.",
    ),
    EconomyFieldSchema::new(
        "reserveSlots",
        "integer",
        true,
        "Artifact slots retained in reserve while demotion is blocked.",
    ),
    EconomyFieldSchema::new(
        "reasons",
        "array<string>",
        true,
        "Deterministic explanation of the reserve decision.",
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

const ECONOMY_SIMULATION_FIELDS: &[EconomyFieldSchema] = &[
    EconomyFieldSchema::new("schema", "string", true, "Schema identifier."),
    EconomyFieldSchema::new(
        "generatedAt",
        "string",
        true,
        "RFC 3339 simulation timestamp.",
    ),
    EconomyFieldSchema::new(
        "dryRun",
        "boolean",
        true,
        "Always true for simulation output.",
    ),
    EconomyFieldSchema::new(
        "mutationStatus",
        "string",
        true,
        "Mutation state; simulation reports not_applied.",
    ),
    EconomyFieldSchema::new(
        "baselineBudgetTokens",
        "integer",
        true,
        "Baseline attention budget used for delta comparisons.",
    ),
    EconomyFieldSchema::new(
        "contextProfile",
        "string",
        true,
        "Context attention profile used by every scenario.",
    ),
    EconomyFieldSchema::new(
        "situationProfile",
        "string",
        true,
        "Situation attention profile used by every scenario.",
    ),
    EconomyFieldSchema::new(
        "rankingStateHashBefore",
        "string",
        true,
        "Hash of ranking inputs before simulation.",
    ),
    EconomyFieldSchema::new(
        "rankingStateHashAfter",
        "string",
        true,
        "Hash of ranking inputs after simulation.",
    ),
    EconomyFieldSchema::new(
        "rankingStateUnchanged",
        "boolean",
        true,
        "Whether before and after ranking hashes match.",
    ),
    EconomyFieldSchema::new(
        "summary",
        "object",
        true,
        "Best budget, baseline delta, and no-mutation evidence.",
    ),
    EconomyFieldSchema::new(
        "scenarios",
        "array<object>",
        true,
        "Budget scenarios with scores, allocations, and rankings.",
    ),
    EconomyFieldSchema::new(
        "explanations",
        "array<string>",
        true,
        "Deterministic explanation of simulation assumptions.",
    ),
];

#[must_use]
pub const fn economy_schemas() -> [EconomyObjectSchema; 9] {
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
            schema_name: ATTENTION_BUDGET_SCHEMA_V1,
            schema_uri: "urn:ee:schema:economy-attention-budget:v1",
            kind: "attention_budget",
            title: "AttentionBudgetAllocation",
            description: "Deterministic token allocation for context and situation attention profiles.",
            fields: ATTENTION_BUDGET_FIELDS,
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
            schema_name: TAIL_RISK_RESERVE_RULE_SCHEMA_V1,
            schema_uri: "urn:ee:schema:economy-tail-risk-reserve-rule:v1",
            kind: "tail_risk_reserve_rule",
            title: "TailRiskReserveRule",
            description: "Rule that protects rare high-severity warnings and procedures from popularity-only demotion.",
            fields: TAIL_RISK_RESERVE_RULE_FIELDS,
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
        EconomyObjectSchema {
            schema_name: ECONOMY_SIMULATION_SCHEMA_V1,
            schema_uri: "urn:ee:schema:economy-simulation:v1",
            kind: "economy_simulation",
            title: "EconomySimulationReport",
            description: "Report-only comparison of alternate attention budgets without changing ranking state.",
            fields: ECONOMY_SIMULATION_FIELDS,
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
            ATTENTION_BUDGET_SCHEMA_V1,
            "ee.economy.attention_budget.v1",
            "attention budget",
        )?;
        ensure(
            RISK_RESERVE_SCHEMA_V1,
            "ee.economy.risk_reserve.v1",
            "reserve",
        )?;
        ensure(
            TAIL_RISK_RESERVE_RULE_SCHEMA_V1,
            "ee.economy.tail_risk_reserve_rule.v1",
            "tail reserve rule",
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
            ECONOMY_SIMULATION_SCHEMA_V1,
            "ee.economy.simulation.v1",
            "simulation",
        )?;
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
    fn attention_budget_compact_minimal_is_deterministic() -> TestResult {
        let allocation = AttentionBudgetAllocation::calculate(AttentionBudgetRequest::new(
            4_000,
            ContextAttentionProfile::Compact,
            SituationAttentionProfile::Minimal,
        ));

        ensure(allocation.retrieval_tokens, 2_840, "retrieval")?;
        ensure(allocation.evidence_tokens, 560, "evidence")?;
        ensure(allocation.procedure_tokens, 240, "procedure")?;
        ensure(allocation.risk_reserve_tokens, 240, "reserve")?;
        ensure(allocation.maintenance_tokens, 120, "maintenance")?;
        ensure(allocation.used_tokens(), 4_000, "used tokens")?;
        ensure(allocation.max_items, 4, "max items")
    }

    #[test]
    fn attention_budget_thorough_full_preserves_tail_risk_reserve() -> TestResult {
        let allocation = AttentionBudgetAllocation::calculate(AttentionBudgetRequest::new(
            6_000,
            ContextAttentionProfile::Thorough,
            SituationAttentionProfile::Full,
        ));

        ensure(allocation.retrieval_tokens, 2_160, "retrieval")?;
        ensure(allocation.evidence_tokens, 1_560, "evidence")?;
        ensure(allocation.procedure_tokens, 780, "procedure")?;
        ensure(allocation.risk_reserve_tokens, 1_260, "reserve")?;
        ensure(allocation.maintenance_tokens, 240, "maintenance")?;
        ensure(allocation.reserve_ratio() > 0.20, true, "reserve ratio")?;
        ensure(allocation.max_items, 22, "max items")
    }

    #[test]
    fn attention_budget_profiles_parse_stable_names() -> TestResult {
        ensure(
            ContextAttentionProfile::parse("SUBMODULAR"),
            Some(ContextAttentionProfile::Submodular),
            "context parse",
        )?;
        ensure(
            ContextAttentionProfile::parse("missing"),
            None,
            "context invalid",
        )?;
        ensure(
            SituationAttentionProfile::parse("full"),
            Some(SituationAttentionProfile::Full),
            "situation parse",
        )?;
        ensure(
            SituationAttentionProfile::parse("unknown"),
            None,
            "situation invalid",
        )
    }

    #[test]
    fn attention_budget_json_has_schema_and_numbers() -> TestResult {
        let allocation = AttentionBudgetAllocation::calculate(AttentionBudgetRequest::new(
            1_000,
            ContextAttentionProfile::Broad,
            SituationAttentionProfile::Summary,
        ));
        let json = allocation.data_json();

        assert_eq!(json["schema"], ATTENTION_BUDGET_SCHEMA_V1);
        ensure_json_number(&json["reserveRatio"], "reserve ratio")?;
        ensure_json_number(&json["attentionCost"]["totalCost"], "attention total cost")?;
        ensure(json["reasons"].as_array().map(Vec::len), Some(2), "reasons")
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
    fn tail_risk_rule_protects_rare_critical_warning_from_popularity_demotion() -> TestResult {
        let rule = TailRiskReserveRule::new(
            "tail.rule.cleanup",
            "warning.cleanup.destructive",
            TailRiskArtifactKind::Warning,
            TailRiskSeverity::Critical,
            EconomyRiskCategory::DataLoss,
        )
        .with_supporting_evidence(2)
        .with_historical_triggers(1)
        .with_retrievals(1)
        .with_popularity_score(0.03)
        .with_utility_score(0.18);

        ensure(
            rule.demotion_action(),
            TailRiskDemotionAction::Protect,
            "demotion action",
        )?;
        ensure(
            rule.blocks_popularity_demotion(),
            true,
            "blocks popularity demotion",
        )?;
        ensure(rule.effective_reserve_tokens(), 1024, "reserve tokens")?;
        ensure(rule.effective_reserve_slots(), 2, "reserve slots")?;

        let json = rule.data_json();
        ensure(
            json["schema"].as_str(),
            Some(TAIL_RISK_RESERVE_RULE_SCHEMA_V1),
            "schema",
        )?;
        ensure(
            json["demotionAction"].as_str(),
            Some("protect"),
            "json action",
        )?;
        ensure_json_number(&json["falseAlarmRate"], "false alarm rate")?;
        ensure_json_number(&json["popularityScore"], "popularity score")
    }

    #[test]
    fn tail_risk_rule_reserves_capacity_for_rare_high_severity_procedure() -> TestResult {
        let rule = TailRiskReserveRule::new(
            "tail.rule.release",
            "procedure.release.verify",
            TailRiskArtifactKind::Procedure,
            TailRiskSeverity::High,
            EconomyRiskCategory::Degradation,
        )
        .with_supporting_evidence(1)
        .with_historical_triggers(2)
        .with_popularity_score(0.12)
        .with_reserve(700, 1);

        let mut reserve = RiskReserve::new(2_000, 4);
        reserve.covered_risks = vec![EconomyRiskCategory::Degradation];

        ensure(
            reserve.can_cover_tail_risk_rule(&rule),
            true,
            "reserve can cover",
        )?;
        ensure(
            reserve.reserve_tail_risk_rule(&rule),
            true,
            "reserve tail-risk rule",
        )?;
        ensure(reserve.available_tokens(), 1300, "remaining tokens")?;
        ensure(reserve.available_slots(), 2, "remaining slots")
    }

    #[test]
    fn tail_risk_rule_allows_low_severity_popularity_demotion() -> TestResult {
        let rule = TailRiskReserveRule::new(
            "tail.rule.low",
            "warning.low",
            TailRiskArtifactKind::Warning,
            TailRiskSeverity::Low,
            EconomyRiskCategory::Unknown,
        )
        .with_supporting_evidence(3)
        .with_historical_triggers(1)
        .with_popularity_score(0.01);

        ensure(
            rule.demotion_action(),
            TailRiskDemotionAction::AllowPopularityDemotion,
            "low severity action",
        )?;
        ensure(
            rule.blocks_popularity_demotion(),
            false,
            "low severity block",
        )?;
        ensure(rule.effective_reserve_tokens(), 0, "low reserve tokens")
    }

    #[test]
    fn tail_risk_rule_blocks_demotion_but_requires_review_on_false_alarm_pressure() -> TestResult {
        let rule = TailRiskReserveRule::new(
            "tail.rule.false_alarm",
            "warning.security.rotate_keys",
            TailRiskArtifactKind::Warning,
            TailRiskSeverity::High,
            EconomyRiskCategory::SecurityIncident,
        )
        .with_supporting_evidence(1)
        .with_historical_triggers(1)
        .with_retrievals(5)
        .with_false_alarms(4)
        .with_popularity_score(0.05);

        ensure(
            rule.demotion_action(),
            TailRiskDemotionAction::ManualReview,
            "manual review action",
        )?;
        ensure(
            rule.blocks_popularity_demotion(),
            true,
            "manual review blocks",
        )?;
        ensure(rule.requires_manual_review(), true, "requires review")?;
        ensure_json_number(&rule.data_json()["falseAlarmRate"], "false alarm rate")
    }

    #[test]
    fn tail_risk_rule_requires_evidence_before_protection() -> TestResult {
        let rule = TailRiskReserveRule::new(
            "tail.rule.no_evidence",
            "procedure.unproven",
            TailRiskArtifactKind::Procedure,
            TailRiskSeverity::Critical,
            EconomyRiskCategory::Compliance,
        )
        .with_popularity_score(0.02);

        ensure(rule.has_tail_evidence(), false, "evidence requirement")?;
        ensure(
            rule.demotion_action(),
            TailRiskDemotionAction::AllowPopularityDemotion,
            "no evidence action",
        )?;
        ensure(
            rule.blocks_popularity_demotion(),
            false,
            "no evidence block",
        )
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

        let rule = TailRiskReserveRule::new(
            "tail.rule.numeric",
            "warning.numeric",
            TailRiskArtifactKind::Warning,
            TailRiskSeverity::High,
            EconomyRiskCategory::SecurityIncident,
        )
        .with_supporting_evidence(1)
        .with_retrievals(4)
        .with_false_alarms(1)
        .with_popularity_score(0.1234)
        .with_utility_score(0.4567)
        .data_json();
        ensure_json_number(&rule["falseAlarmRate"], "rule false alarm rate")?;
        ensure_json_number(&rule["popularityScore"], "rule popularity")?;
        ensure_json_number(&rule["utilityScore"], "rule utility")?;

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
        ensure(schemas.len(), 9, "schema count")?;
        ensure(schemas[0].schema_name, UTILITY_VALUE_SCHEMA_V1, "utility")?;
        ensure(
            schemas[1].schema_name,
            ATTENTION_COST_SCHEMA_V1,
            "attention cost",
        )?;
        ensure(
            schemas[2].schema_name,
            ATTENTION_BUDGET_SCHEMA_V1,
            "attention budget",
        )?;
        ensure(schemas[3].schema_name, RISK_RESERVE_SCHEMA_V1, "reserve")?;
        ensure(
            schemas[4].schema_name,
            TAIL_RISK_RESERVE_RULE_SCHEMA_V1,
            "tail risk reserve rule",
        )?;
        ensure(schemas[5].schema_name, MAINTENANCE_DEBT_SCHEMA_V1, "debt")?;
        ensure(
            schemas[6].schema_name,
            ECONOMY_RECOMMENDATION_SCHEMA_V1,
            "recommendation",
        )?;
        ensure(schemas[7].schema_name, ECONOMY_REPORT_SCHEMA_V1, "report")?;
        ensure(
            schemas[8].schema_name,
            ECONOMY_SIMULATION_SCHEMA_V1,
            "simulation",
        )
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
        ensure(schemas.len(), 9, "catalog length")
    }
}
