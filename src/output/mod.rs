use std::env;
use std::io::IsTerminal;

use crate::core::agent_detect::{AgentInventoryReport, InstalledAgentDetectionReport};
use crate::core::capabilities::CapabilitiesReport;
use crate::core::check::CheckReport;
use crate::core::curate::{
    CurateApplyReport, CurateCandidatesReport, CurateDispositionReport, CurateReviewReport,
    CurateValidateReport,
};
use crate::core::doctor::{
    DependencyBlockedFeature, DependencyContractEntry, DependencyDiagnosticsReport,
    DependencyFeatureProfile, DependencyOptionalFeatureProfile, DependencySource, DoctorReport,
    FixPlan, FrankenDependencyHealth, FrankenHealthReport, IntegrityCanaryReport,
    IntegrityDiagnosticCheck, IntegrityDiagnosticDegradation, IntegrityDiagnosticsReport,
};
use crate::core::health::HealthReport;
use crate::core::memory::{
    MemoryDetails, MemoryHistoryReport, MemoryListReport, MemoryShowReport, memory_validity,
};
use crate::core::outcome::{OutcomeQuarantineListReport, OutcomeQuarantineReviewReport};
use crate::core::quarantine::{QuarantineEntry, QuarantineReport};
use crate::core::rule::{RuleAddReport, RuleListReport, RuleProtectReport, RuleShowReport};
use crate::core::status::StatusReport;
use crate::core::why::WhyReport;
use crate::core::{VERSION_PROVENANCE_SCHEMA_V1, VersionReport};
use crate::eval::{EvaluationReport, EvaluationStatus, ScenarioValidationResult};
use crate::models::decision::{DecisionPlane, DecisionPlaneMetadata, DecisionRecord};
use crate::models::{
    DomainError, ERROR_SCHEMA_V1, InstallCheckReport, InstallPlanReport, RESPONSE_SCHEMA_V1,
};
use crate::pack::{
    ContextResponse, PackAdvisoryBanner, PackAdvisoryNote, PackDraftItem, PackItemProvenance,
    PackOmissionMetrics, PackQualityMetrics, PackRejectedFrontierItem, PackSectionMetric,
    PackSelectedItem, PackSelectionCertificate, PackSelectionStep, RenderedPackProvenance,
};

pub mod jsonl_export;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Renderer {
    #[default]
    Human,
    Json,
    Toon,
    Jsonl,
    Compact,
    Hook,
    Markdown,
}

impl Renderer {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Json => "json",
            Self::Toon => "toon",
            Self::Jsonl => "jsonl",
            Self::Compact => "compact",
            Self::Hook => "hook",
            Self::Markdown => "markdown",
        }
    }

    #[must_use]
    pub const fn is_machine_readable(self) -> bool {
        matches!(self, Self::Json | Self::Jsonl | Self::Compact | Self::Hook)
    }
}

/// Field profile controls the verbosity of JSON output.
///
/// - `Minimal`: IDs, status, version only — bare minimum for scripting
/// - `Summary`: + top-level metrics and counts
/// - `Standard`: + arrays with items, but without verbose details
/// - `Full`: everything including provenance, why, debug info
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FieldProfile {
    Minimal,
    Summary,
    #[default]
    Standard,
    Full,
}

impl FieldProfile {
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
    pub const fn include_arrays(self) -> bool {
        matches!(self, Self::Standard | Self::Full)
    }

    #[must_use]
    pub const fn include_summary_metrics(self) -> bool {
        !matches!(self, Self::Minimal)
    }

    #[must_use]
    pub const fn include_verbose_details(self) -> bool {
        matches!(self, Self::Full)
    }

    #[must_use]
    pub const fn include_provenance(self) -> bool {
        matches!(self, Self::Full)
    }
}

/// Cards output profile (EE-341).
///
/// Controls which cards are included in structured output:
/// - `None`: No cards in output (minimal response)
/// - `Summary`: One-line card summaries only
/// - `Math`: Include mathematical artifacts and certificates
/// - `Full`: All cards with full provenance and explanations
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CardsProfile {
    None,
    Summary,
    #[default]
    Math,
    Full,
}

impl CardsProfile {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Summary => "summary",
            Self::Math => "math",
            Self::Full => "full",
        }
    }

    #[must_use]
    pub const fn include_cards(self) -> bool {
        !matches!(self, Self::None)
    }

    #[must_use]
    pub const fn include_math(self) -> bool {
        matches!(self, Self::Math | Self::Full)
    }

    #[must_use]
    pub const fn include_provenance(self) -> bool {
        matches!(self, Self::Full)
    }
}

/// Schema identifier for cards output.
pub const CARDS_SCHEMA_V1: &str = "ee.cards.v1";

/// MCP protocol version advertised by the optional stdio adapter.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Schema identifier for the MCP manifest data payload.
pub const MCP_MANIFEST_SCHEMA_V1: &str = "ee.mcp.manifest.v1";

/// A single card in structured output.
#[derive(Clone, Debug)]
pub struct Card {
    pub id: String,
    pub kind: CardKind,
    pub title: String,
    pub summary: Option<String>,
    pub math: Option<CardMath>,
    pub provenance: Option<String>,
}

impl Card {
    #[must_use]
    pub fn new(id: impl Into<String>, kind: CardKind, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind,
            title: title.into(),
            summary: None,
            math: None,
            provenance: None,
        }
    }

    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    pub fn with_math(mut self, math: CardMath) -> Self {
        self.math = Some(math);
        self
    }

    pub fn with_provenance(mut self, provenance: impl Into<String>) -> Self {
        self.provenance = Some(provenance.into());
        self
    }

    #[must_use]
    pub fn to_json(&self, profile: CardsProfile) -> String {
        let mut b = JsonBuilder::with_capacity(256);
        b.field_str("id", &self.id);
        b.field_str("kind", self.kind.as_str());
        b.field_str("title", &self.title);
        if profile.include_cards() {
            if let Some(ref summary) = self.summary {
                b.field_str("summary", summary);
            }
        }
        if profile.include_math() {
            if let Some(ref math) = self.math {
                b.field_raw("math", &math.to_json());
            }
        }
        if profile.include_provenance() {
            if let Some(ref prov) = self.provenance {
                b.field_str("provenance", prov);
            }
        }
        b.finish()
    }
}

/// Kind of card.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CardKind {
    Certificate,
    Artifact,
    Audit,
    Risk,
    Lifecycle,
    Recommendation,
}

impl CardKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Certificate => "certificate",
            Self::Artifact => "artifact",
            Self::Audit => "audit",
            Self::Risk => "risk",
            Self::Lifecycle => "lifecycle",
            Self::Recommendation => "recommendation",
        }
    }
}

/// Mathematical content in a card.
#[derive(Clone, Debug)]
pub struct CardMath {
    pub formula: Option<String>,
    pub value: Option<f64>,
    pub confidence: Option<f64>,
    pub unit: Option<String>,
    pub substituted_values: Option<String>,
    pub intuition: Option<String>,
    pub assumptions: Vec<String>,
    pub decision_change: Option<String>,
}

impl CardMath {
    #[must_use]
    pub fn new() -> Self {
        Self {
            formula: None,
            value: None,
            confidence: None,
            unit: None,
            substituted_values: None,
            intuition: None,
            assumptions: Vec::new(),
            decision_change: None,
        }
    }

    pub fn with_value(mut self, value: f64) -> Self {
        self.value = Some(value);
        self
    }

    pub fn with_confidence(mut self, confidence: f64) -> Self {
        self.confidence = Some(confidence);
        self
    }

    pub fn with_formula(mut self, formula: impl Into<String>) -> Self {
        self.formula = Some(formula.into());
        self
    }

    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    pub fn with_substituted_values(mut self, values: impl Into<String>) -> Self {
        self.substituted_values = Some(values.into());
        self
    }

    pub fn with_intuition(mut self, intuition: impl Into<String>) -> Self {
        self.intuition = Some(intuition.into());
        self
    }

    pub fn with_assumption(mut self, assumption: impl Into<String>) -> Self {
        self.assumptions.push(assumption.into());
        self
    }

    pub fn with_decision_change(mut self, decision_change: impl Into<String>) -> Self {
        self.decision_change = Some(decision_change.into());
        self
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        let mut b = JsonBuilder::new();
        if let Some(ref formula) = self.formula {
            b.field_str("formula", formula);
        }
        if let Some(value) = self.value {
            b.field_raw("value", &format!("{:.6}", value));
        }
        if let Some(confidence) = self.confidence {
            b.field_raw("confidence", &format!("{:.4}", confidence));
        }
        if let Some(ref unit) = self.unit {
            b.field_str("unit", unit);
        }
        if let Some(ref values) = self.substituted_values {
            b.field_str("substitutedValues", values);
        }
        if let Some(ref intuition) = self.intuition {
            b.field_str("intuition", intuition);
        }
        if !self.assumptions.is_empty() {
            b.field_raw(
                "assumptions",
                &string_array_json(self.assumptions.iter().map(String::as_str)),
            );
        }
        if let Some(ref decision_change) = self.decision_change {
            b.field_str("decisionChange", decision_change);
        }
        b.finish()
    }
}

impl Default for CardMath {
    fn default() -> Self {
        Self::new()
    }
}

/// Create a selection score math card showing the weighted combination formula.
#[must_use]
pub fn selection_score_card(confidence: f32, utility: f32, importance: f32, score: f32) -> Card {
    let formula = "score = α·confidence + β·utility + γ·importance";
    let computation = format!(
        "score = 0.40×{confidence:.2} + 0.35×{utility:.2} + 0.25×{importance:.2} = {score:.3}"
    );
    let math = CardMath::new()
        .with_formula(formula)
        .with_value(score as f64)
        .with_confidence(confidence as f64)
        .with_substituted_values(computation.clone())
        .with_intuition("Higher confidence, utility, and importance raise the selected score.")
        .with_assumption("Weights are fixed for this certificate profile.")
        .with_decision_change(
            "A lower confidence or utility value could move this item below the cutoff.",
        );
    Card::new(
        "card_selection_score",
        CardKind::Certificate,
        "Selection Score Computation",
    )
    .with_summary(computation)
    .with_math(math)
}

/// Create a relevance score math card showing semantic/lexical fusion.
#[must_use]
pub fn relevance_score_card(
    semantic: f32,
    lexical: f32,
    fused: f32,
    rank: u32,
    rrf_k: u32,
) -> Card {
    let rrf_formula = format!("RRF(d) = Σ(1 / (k + rank(d))), k={rrf_k}");
    let summary =
        format!("Rank {rank}: semantic={semantic:.3}, lexical={lexical:.3} → fused={fused:.4}");
    let math = CardMath::new()
        .with_formula(rrf_formula)
        .with_value(fused as f64)
        .with_substituted_values(summary.clone())
        .with_intuition("Documents ranked well by either retriever gain fused relevance.")
        .with_assumption("Semantic and lexical ranks are stable for the same index generation.")
        .with_decision_change("A worse semantic or lexical rank would lower fused relevance.");
    Card::new(
        "card_relevance_score",
        CardKind::Certificate,
        "Relevance Score (RRF Fusion)",
    )
    .with_summary(summary)
    .with_math(math)
}

/// Create a utility decay math card showing temporal decay computation.
#[must_use]
pub fn utility_decay_card(
    base_utility: f32,
    age_days: u32,
    decay_rate: f32,
    current_utility: f32,
) -> Card {
    let formula = "utility(t) = base · exp(-λ·t)";
    let computation = format!(
        "utility = {base_utility:.3} × exp(-{decay_rate:.4} × {age_days}) = {current_utility:.3}"
    );
    let math = CardMath::new()
        .with_formula(formula)
        .with_value(current_utility as f64)
        .with_unit("utility units".to_string())
        .with_substituted_values(computation.clone())
        .with_intuition("Older memories lose utility unless their base value is high enough.")
        .with_assumption("The decay rate is fixed for the active policy.")
        .with_decision_change("A lower age or decay rate would preserve more utility.");
    Card::new(
        "card_utility_decay",
        CardKind::Certificate,
        "Utility Temporal Decay",
    )
    .with_summary(computation)
    .with_math(math)
}

/// Create a trust score math card showing weighted trust class contribution.
#[must_use]
pub fn trust_score_card(
    trust_class: &str,
    trust_weight: f32,
    confidence: f32,
    combined: f32,
) -> Card {
    let formula = "trust = class_weight × confidence";
    let computation =
        format!("trust({trust_class}) = {trust_weight:.2} × {confidence:.2} = {combined:.3}");
    let math = CardMath::new()
        .with_formula(formula)
        .with_value(combined as f64)
        .with_confidence(confidence as f64)
        .with_substituted_values(computation.clone())
        .with_intuition("Higher-trust sources contribute more strongly to selection.")
        .with_assumption("Trust class weights are policy-controlled and deterministic.")
        .with_decision_change("A lower trust class weight could demote this memory.");
    Card::new(
        "card_trust_score",
        CardKind::Certificate,
        "Trust Score Computation",
    )
    .with_summary(computation)
    .with_math(math)
}

/// Create a pack budget math card showing token budget utilization.
#[must_use]
pub fn pack_budget_card(
    used_tokens: u32,
    max_tokens: u32,
    item_count: u32,
    omitted_count: u32,
) -> Card {
    let utilization = (used_tokens as f64 / max_tokens as f64) * 100.0;
    let formula = "utilization = used_tokens / max_tokens";
    let summary = format!(
        "{used_tokens}/{max_tokens} tokens ({utilization:.1}%), \
         {item_count} items packed, {omitted_count} omitted"
    );
    let math = CardMath::new()
        .with_formula(formula)
        .with_value(utilization)
        .with_unit("%".to_string())
        .with_substituted_values(summary.clone())
        .with_intuition("Higher utilization means less room remains for additional memories.")
        .with_assumption("Token estimates use the active deterministic estimator.")
        .with_decision_change("A larger max token budget could admit omitted items.");
    Card::new("card_pack_budget", CardKind::Audit, "Pack Token Budget")
        .with_summary(summary)
        .with_math(math)
}

/// Create a diversity penalty math card showing MMR-style diversity score.
#[must_use]
pub fn diversity_penalty_card(
    base_score: f32,
    diversity_penalty: f32,
    final_score: f32,
    similar_items: u32,
) -> Card {
    let formula = "final = base - λ·max_sim(selected)";
    let computation = format!(
        "final = {base_score:.3} - {diversity_penalty:.3} = {final_score:.3} \
         ({similar_items} similar items penalized)"
    );
    let math = CardMath::new()
        .with_formula(formula)
        .with_value(final_score as f64)
        .with_substituted_values(computation.clone())
        .with_intuition("Redundant memories lose score so the pack covers more distinct evidence.")
        .with_assumption("Similarity is computed against already selected items.")
        .with_decision_change("Less overlap with selected memories would reduce the penalty.");
    Card::new(
        "card_diversity_penalty",
        CardKind::Certificate,
        "Diversity Penalty (MMR)",
    )
    .with_summary(computation)
    .with_math(math)
}

// ============================================================================
// EE-374: Graveyard recommendation cards
// ============================================================================

/// Priority level for graveyard recommendations.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum GraveyardPriority {
    Low,
    Medium,
    High,
    Critical,
}

impl GraveyardPriority {
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
    pub const fn all() -> [Self; 4] {
        [Self::Low, Self::Medium, Self::High, Self::Critical]
    }
}

/// Type of graveyard recommendation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraveyardRecommendationType {
    /// Claim has not been verified recently.
    StaleClaim,
    /// Claim has no associated demo.
    MissingDemo,
    /// Recent verification attempt failed.
    FailedVerification,
    /// Claim is a candidate for uplift/promotion.
    UpliftCandidate,
    /// Demo output has drifted from expected.
    OutputDrift,
    /// Claim depends on deprecated feature.
    DeprecatedDependency,
}

impl GraveyardRecommendationType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StaleClaim => "stale_claim",
            Self::MissingDemo => "missing_demo",
            Self::FailedVerification => "failed_verification",
            Self::UpliftCandidate => "uplift_candidate",
            Self::OutputDrift => "output_drift",
            Self::DeprecatedDependency => "deprecated_dependency",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 6] {
        [
            Self::StaleClaim,
            Self::MissingDemo,
            Self::FailedVerification,
            Self::UpliftCandidate,
            Self::OutputDrift,
            Self::DeprecatedDependency,
        ]
    }

    #[must_use]
    pub const fn default_priority(self) -> GraveyardPriority {
        match self {
            Self::StaleClaim => GraveyardPriority::Medium,
            Self::MissingDemo => GraveyardPriority::High,
            Self::FailedVerification => GraveyardPriority::Critical,
            Self::UpliftCandidate => GraveyardPriority::Low,
            Self::OutputDrift => GraveyardPriority::High,
            Self::DeprecatedDependency => GraveyardPriority::Medium,
        }
    }
}

/// Create a stale claim recommendation card.
#[must_use]
pub fn graveyard_stale_claim_card(
    claim_id: &str,
    days_since_verification: u32,
    stale_threshold_days: u32,
) -> Card {
    let summary = format!(
        "Claim {} has not been verified in {} days (threshold: {} days). \
         Run `ee claim verify {}` to update verification status.",
        claim_id, days_since_verification, stale_threshold_days, claim_id
    );
    Card::new(
        format!("card_graveyard_stale_{}", claim_id),
        CardKind::Recommendation,
        "Stale Claim Verification",
    )
    .with_summary(summary)
}

/// Create a missing demo recommendation card.
#[must_use]
pub fn graveyard_missing_demo_card(claim_id: &str, claim_title: &str) -> Card {
    let summary = format!(
        "Claim '{}' ({}) has no associated demo. \
         Add a demo to demo.yaml with claim_id: {} to make this claim executable.",
        claim_title, claim_id, claim_id
    );
    Card::new(
        format!("card_graveyard_missing_demo_{}", claim_id),
        CardKind::Recommendation,
        "Missing Demo for Claim",
    )
    .with_summary(summary)
}

/// Create a failed verification recommendation card.
#[must_use]
pub fn graveyard_failed_verification_card(
    claim_id: &str,
    failure_reason: &str,
    last_attempt: &str,
) -> Card {
    let summary = format!(
        "Claim {} verification failed on {}: {}. \
         Review and fix the underlying issue, then re-run verification.",
        claim_id, last_attempt, failure_reason
    );
    Card::new(
        format!("card_graveyard_failed_{}", claim_id),
        CardKind::Recommendation,
        "Failed Verification",
    )
    .with_summary(summary)
}

/// Create an uplift candidate recommendation card.
#[must_use]
pub fn graveyard_uplift_candidate_card(
    claim_id: &str,
    consecutive_passes: u32,
    confidence_score: f32,
) -> Card {
    let summary = format!(
        "Claim {} has passed verification {} consecutive times with {:.1}% confidence. \
         Consider promoting to 'verified' status.",
        claim_id,
        consecutive_passes,
        confidence_score * 100.0
    );
    Card::new(
        format!("card_graveyard_uplift_{}", claim_id),
        CardKind::Recommendation,
        "Uplift Candidate",
    )
    .with_summary(summary)
    .with_math(
        CardMath::new()
            .with_formula("confidence = consecutive_pass_signal")
            .with_value(confidence_score as f64)
            .with_unit("confidence")
            .with_substituted_values(format!(
                "consecutive_passes={consecutive_passes}, confidence={confidence_score:.3}"
            ))
            .with_intuition("Repeated successful verification raises promotion confidence.")
            .with_assumption("Recent verification attempts are comparable.")
            .with_decision_change("A new failed verification would block promotion."),
    )
}

/// Create an output drift recommendation card.
#[must_use]
pub fn graveyard_output_drift_card(
    demo_id: &str,
    expected_hash: &str,
    actual_hash: &str,
    drift_percentage: f32,
) -> Card {
    let summary = format!(
        "Demo {} output has drifted {:.1}% from expected. \
         Expected hash: {}..., actual: {}... \
         Update expected values or investigate regression.",
        demo_id,
        drift_percentage * 100.0,
        &expected_hash[..8.min(expected_hash.len())],
        &actual_hash[..8.min(actual_hash.len())]
    );
    Card::new(
        format!("card_graveyard_drift_{}", demo_id),
        CardKind::Recommendation,
        "Output Drift Detected",
    )
    .with_summary(summary)
    .with_math(
        CardMath::new()
            .with_formula("drift = changed_output_bytes / expected_output_bytes")
            .with_value(drift_percentage as f64)
            .with_unit("drift")
            .with_substituted_values(format!("drift={drift_percentage:.3}"))
            .with_intuition("Large output drift can indicate a changed contract.")
            .with_assumption("Expected and actual hashes refer to the same demo.")
            .with_decision_change(
                "Refreshing the expected artifact after review would clear drift.",
            ),
    )
}

/// Create a deprecated dependency recommendation card.
#[must_use]
pub fn graveyard_deprecated_dependency_card(
    claim_id: &str,
    deprecated_feature: &str,
    replacement: Option<&str>,
) -> Card {
    let summary = if let Some(repl) = replacement {
        format!(
            "Claim {} depends on deprecated feature '{}'. \
             Migrate to '{}' before the feature is removed.",
            claim_id, deprecated_feature, repl
        )
    } else {
        format!(
            "Claim {} depends on deprecated feature '{}'. \
             Review and remove this dependency.",
            claim_id, deprecated_feature
        )
    };
    Card::new(
        format!("card_graveyard_deprecated_{}", claim_id),
        CardKind::Recommendation,
        "Deprecated Dependency",
    )
    .with_summary(summary)
}

/// Render a cards array for JSON output.
#[must_use]
pub fn render_cards_json(cards: &[Card], profile: CardsProfile) -> String {
    if !profile.include_cards() || cards.is_empty() {
        return "[]".to_string();
    }
    let mut result = String::from("[");
    for (i, card) in cards.iter().enumerate() {
        if i > 0 {
            result.push(',');
        }
        result.push_str(&card.to_json(profile));
    }
    result.push(']');
    result
}

#[derive(Clone, Copy, Debug)]
pub struct OutputContext {
    pub renderer: Renderer,
    pub field_profile: FieldProfile,
    pub is_tty: bool,
    pub color_enabled: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct OutputEnvironment {
    ee_json: Option<String>,
    ee_output_format: Option<String>,
    ee_format: Option<String>,
    toon_default_format: Option<String>,
    ee_agent_mode: Option<String>,
    ee_hook_mode: Option<String>,
    no_color: Option<String>,
    ee_no_color: Option<String>,
    force_color: Option<String>,
}

impl OutputEnvironment {
    fn from_process_env() -> Self {
        Self {
            ee_json: env::var("EE_JSON").ok(),
            ee_output_format: env::var("EE_OUTPUT_FORMAT").ok(),
            ee_format: env::var("EE_FORMAT").ok(),
            toon_default_format: env::var("TOON_DEFAULT_FORMAT").ok(),
            ee_agent_mode: env::var("EE_AGENT_MODE").ok(),
            ee_hook_mode: env::var("EE_HOOK_MODE").ok(),
            no_color: env::var("NO_COLOR").ok(),
            ee_no_color: env::var("EE_NO_COLOR").ok(),
            force_color: env::var("FORCE_COLOR").ok(),
        }
    }
}

fn env_flag_truthy(value: Option<&str>) -> bool {
    value.is_some_and(|raw| {
        let trimmed = raw.trim();
        !(trimmed.is_empty()
            || trimmed == "0"
            || trimmed.eq_ignore_ascii_case("false")
            || trimmed.eq_ignore_ascii_case("no")
            || trimmed.eq_ignore_ascii_case("off"))
    })
}

fn renderer_from_env_value(value: &str) -> Option<Renderer> {
    match value.trim().to_ascii_lowercase().as_str() {
        "human" => Some(Renderer::Human),
        "json" => Some(Renderer::Json),
        "toon" => Some(Renderer::Toon),
        "jsonl" => Some(Renderer::Jsonl),
        "compact" => Some(Renderer::Compact),
        "hook" => Some(Renderer::Hook),
        "markdown" | "md" => Some(Renderer::Markdown),
        _ => None,
    }
}

impl OutputContext {
    #[must_use]
    pub fn detect() -> Self {
        Self::detect_with_hints(false, false, None)
    }

    #[must_use]
    pub fn detect_with_hints(
        json_flag: bool,
        robot_flag: bool,
        format_override: Option<Renderer>,
    ) -> Self {
        let is_tty = std::io::stdout().is_terminal();
        Self::detect_with_environment(
            json_flag,
            robot_flag,
            format_override,
            is_tty,
            &OutputEnvironment::from_process_env(),
        )
    }

    fn detect_with_environment(
        json_flag: bool,
        robot_flag: bool,
        format_override: Option<Renderer>,
        is_tty: bool,
        environment: &OutputEnvironment,
    ) -> Self {
        let no_color = environment.no_color.is_some() || environment.ee_no_color.is_some();
        let force_color = env_flag_truthy(environment.force_color.as_deref());
        let renderer = if let Some(r) = format_override {
            r
        } else if json_flag
            || robot_flag
            || env_flag_truthy(environment.ee_json.as_deref())
            || env_flag_truthy(environment.ee_agent_mode.as_deref())
        {
            Renderer::Json
        } else if env_flag_truthy(environment.ee_hook_mode.as_deref()) {
            Renderer::Hook
        } else if let Some(renderer) = environment
            .ee_output_format
            .as_deref()
            .and_then(renderer_from_env_value)
        {
            renderer
        } else if let Some(renderer) = environment
            .ee_format
            .as_deref()
            .and_then(renderer_from_env_value)
        {
            renderer
        } else if let Some(renderer) = environment
            .toon_default_format
            .as_deref()
            .and_then(renderer_from_env_value)
        {
            renderer
        } else {
            Renderer::Human
        };

        let color_enabled = (is_tty || force_color) && !no_color && !renderer.is_machine_readable();

        Self {
            renderer,
            field_profile: FieldProfile::Standard,
            is_tty,
            color_enabled,
        }
    }

    #[must_use]
    pub fn with_field_profile(mut self, profile: FieldProfile) -> Self {
        self.field_profile = profile;
        self
    }

    #[must_use]
    pub const fn is_machine_output(&self) -> bool {
        self.renderer.is_machine_readable()
    }
}

/// Severity level for degradation notices in the ee.response.v1 envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DegradationSeverity {
    Low,
    Medium,
    High,
}

impl DegradationSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// A single degradation notice in the ee.response.v1 envelope.
///
/// Degradation notices tell consumers that the response is valid but
/// incomplete or limited in some way. The repair field suggests how to
/// resolve the degradation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Degradation {
    pub code: String,
    pub severity: DegradationSeverity,
    pub message: String,
    pub repair: String,
}

impl Degradation {
    #[must_use]
    pub fn new(
        code: impl Into<String>,
        severity: DegradationSeverity,
        message: impl Into<String>,
        repair: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            severity,
            message: message.into(),
            repair: repair.into(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        let mut b = JsonBuilder::new();
        b.field_str("code", &self.code);
        b.field_str("severity", self.severity.as_str());
        b.field_str("message", &self.message);
        b.field_str("repair", &self.repair);
        b.finish()
    }
}

pub struct JsonBuilder {
    buffer: String,
    first: bool,
}

impl JsonBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: String::from("{"),
            first: true,
        }
    }

    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let mut buffer = String::with_capacity(capacity);
        buffer.push('{');
        Self {
            buffer,
            first: true,
        }
    }

    pub fn field_str(&mut self, key: &str, value: &str) -> &mut Self {
        self.separator();
        self.buffer.push('"');
        self.buffer.push_str(key);
        self.buffer.push_str("\":\"");
        self.buffer.push_str(&escape_json_string(value));
        self.buffer.push('"');
        self
    }

    pub fn field_raw(&mut self, key: &str, raw_json: &str) -> &mut Self {
        self.separator();
        self.buffer.push('"');
        self.buffer.push_str(key);
        self.buffer.push_str("\":");
        self.buffer.push_str(raw_json);
        self
    }

    pub fn field_bool(&mut self, key: &str, value: bool) -> &mut Self {
        self.field_raw(key, if value { "true" } else { "false" })
    }

    pub fn field_u32(&mut self, key: &str, value: u32) -> &mut Self {
        self.separator();
        self.buffer.push('"');
        self.buffer.push_str(key);
        self.buffer.push_str("\":");
        self.buffer.push_str(&value.to_string());
        self
    }

    pub fn field_object<F>(&mut self, key: &str, build: F) -> &mut Self
    where
        F: FnOnce(&mut JsonBuilder),
    {
        let mut nested = JsonBuilder::new();
        build(&mut nested);
        let nested_json = nested.finish();
        self.field_raw(key, &nested_json)
    }

    pub fn field_array_of_objects<T, F>(&mut self, key: &str, items: &[T], build: F) -> &mut Self
    where
        F: Fn(&mut JsonBuilder, &T),
    {
        self.separator();
        self.buffer.push('"');
        self.buffer.push_str(key);
        self.buffer.push_str("\":[");
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.buffer.push(',');
            }
            let mut nested = JsonBuilder::new();
            build(&mut nested, item);
            self.buffer.push_str(&nested.finish());
        }
        self.buffer.push(']');
        self
    }

    pub fn field_array_of_strings(&mut self, key: &str, items: &[String]) -> &mut Self {
        self.separator();
        self.buffer.push('"');
        self.buffer.push_str(key);
        self.buffer.push_str("\":[");
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.buffer.push(',');
            }
            self.buffer.push('"');
            self.buffer.push_str(&escape_json_string(item));
            self.buffer.push('"');
        }
        self.buffer.push(']');
        self
    }

    pub fn field_array_of_strs(&mut self, key: &str, items: &[&str]) -> &mut Self {
        self.separator();
        self.buffer.push('"');
        self.buffer.push_str(key);
        self.buffer.push_str("\":[");
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.buffer.push(',');
            }
            self.buffer.push('"');
            self.buffer.push_str(&escape_json_string(item));
            self.buffer.push('"');
        }
        self.buffer.push(']');
        self
    }

    fn separator(&mut self) {
        if self.first {
            self.first = false;
        } else {
            self.buffer.push(',');
        }
    }

    #[must_use]
    pub fn finish(mut self) -> String {
        self.buffer.push('}');
        self.buffer
    }
}

impl Default for JsonBuilder {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ResponseEnvelope {
    builder: JsonBuilder,
}

impl ResponseEnvelope {
    #[must_use]
    pub fn success() -> Self {
        let mut builder = JsonBuilder::with_capacity(256);
        builder.field_str("schema", RESPONSE_SCHEMA_V1);
        builder.field_bool("success", true);
        Self { builder }
    }

    #[must_use]
    pub fn failure() -> Self {
        let mut builder = JsonBuilder::with_capacity(256);
        builder.field_str("schema", RESPONSE_SCHEMA_V1);
        builder.field_bool("success", false);
        Self { builder }
    }

    pub fn data<F>(mut self, build: F) -> Self
    where
        F: FnOnce(&mut JsonBuilder),
    {
        self.builder.field_object("data", build);
        self
    }

    pub fn data_raw(mut self, raw_json: &str) -> Self {
        self.builder.field_raw("data", raw_json);
        self
    }

    pub fn degraded_array<T, F>(mut self, items: &[T], build: F) -> Self
    where
        F: Fn(&mut JsonBuilder, &T),
    {
        self.builder
            .field_array_of_objects("degraded", items, build);
        self
    }

    #[must_use]
    pub fn finish(self) -> String {
        self.builder.finish()
    }
}

// ============================================================================
// Output Size Diagnostics (EE-335)
//
// Compare JSON and TOON output sizes to help understand token savings.
// Tokens are estimated using a simple heuristic (words * 4/3).
// ============================================================================

/// Schema identifier for output size diagnostic.
pub const OUTPUT_SIZE_DIAGNOSTIC_SCHEMA_V1: &str = "ee.output_size_diagnostic.v1";

/// Output size diagnostic comparing JSON and TOON representations.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputSizeDiagnostic {
    pub json_bytes: usize,
    pub toon_bytes: usize,
    pub json_estimated_tokens: usize,
    pub toon_estimated_tokens: usize,
    pub byte_savings: i64,
    pub token_savings: i64,
    pub compression_ratio: f64,
}

impl OutputSizeDiagnostic {
    /// Compute size diagnostic from JSON string.
    #[must_use]
    pub fn from_json(json: &str) -> Self {
        let toon = render_toon_from_json(json);
        Self::from_pair(json, &toon)
    }

    /// Compute size diagnostic from JSON and TOON pair.
    #[must_use]
    pub fn from_pair(json: &str, toon: &str) -> Self {
        let json_bytes = json.len();
        let toon_bytes = toon.len();
        let json_estimated_tokens = estimate_tokens(json);
        let toon_estimated_tokens = estimate_tokens(toon);

        let byte_savings = json_bytes as i64 - toon_bytes as i64;
        let token_savings = json_estimated_tokens as i64 - toon_estimated_tokens as i64;
        let compression_ratio = if json_bytes > 0 {
            toon_bytes as f64 / json_bytes as f64
        } else {
            1.0
        };

        Self {
            json_bytes,
            toon_bytes,
            json_estimated_tokens,
            toon_estimated_tokens,
            byte_savings,
            token_savings,
            compression_ratio,
        }
    }

    /// Render as JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut b = JsonBuilder::with_capacity(256);
        b.field_str("schema", OUTPUT_SIZE_DIAGNOSTIC_SCHEMA_V1);
        b.field_object("json", |j| {
            j.field_raw("bytes", &self.json_bytes.to_string());
            j.field_raw("estimatedTokens", &self.json_estimated_tokens.to_string());
        });
        b.field_object("toon", |t| {
            t.field_raw("bytes", &self.toon_bytes.to_string());
            t.field_raw("estimatedTokens", &self.toon_estimated_tokens.to_string());
        });
        b.field_object("savings", |s| {
            s.field_raw("bytes", &self.byte_savings.to_string());
            s.field_raw("tokens", &self.token_savings.to_string());
            s.field_raw(
                "compressionRatio",
                &format!("{:.3}", self.compression_ratio),
            );
        });
        b.finish()
    }

    /// Render as human-readable text.
    #[must_use]
    pub fn to_human(&self) -> String {
        let savings_pct = if self.json_bytes > 0 {
            (1.0 - self.compression_ratio) * 100.0
        } else {
            0.0
        };

        format!(
            "Output Size Diagnostic\n\
             ─────────────────────────────────────\n\
             JSON:  {:>8} bytes  {:>6} tokens\n\
             TOON:  {:>8} bytes  {:>6} tokens\n\
             ─────────────────────────────────────\n\
             Savings: {:>+6} bytes  {:>+5} tokens ({:.1}%)\n",
            self.json_bytes,
            self.json_estimated_tokens,
            self.toon_bytes,
            self.toon_estimated_tokens,
            self.byte_savings,
            self.token_savings,
            savings_pct,
        )
    }
}

/// Estimate token count using a simple heuristic.
fn estimate_tokens(text: &str) -> usize {
    text.split_whitespace().count().saturating_mul(4) / 3
}

/// Compute size diagnostics for representative payloads.
#[must_use]
pub fn compute_representative_size_diagnostics() -> Vec<(&'static str, OutputSizeDiagnostic)> {
    use crate::core::health::HealthReport;
    use crate::core::status::StatusReport;

    let mut diagnostics = Vec::new();

    // Status report
    let status = StatusReport::gather();
    let status_json = render_status_json(&status);
    diagnostics.push(("status", OutputSizeDiagnostic::from_json(&status_json)));

    // Health report
    let health = HealthReport::gather();
    let health_json = render_health_json(&health);
    diagnostics.push(("health", OutputSizeDiagnostic::from_json(&health_json)));

    diagnostics
}

/// Render a context response as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_context_response_json(response: &ContextResponse) -> String {
    let mut b = JsonBuilder::with_capacity(2048);
    b.field_str("schema", response.schema);
    b.field_bool("success", response.success);
    b.field_object("data", |d| {
        d.field_str("command", response.data.command);
        d.field_object("request", |request| {
            request.field_str("query", &response.data.request.query);
            request.field_str("profile", response.data.request.profile.as_str());
            request.field_u32("maxTokens", response.data.request.budget.max_tokens());
            request.field_u32("candidatePool", response.data.request.candidate_pool);
            let sections = string_array_json(
                response
                    .data
                    .request
                    .sections
                    .iter()
                    .map(|section| section.as_str()),
            );
            request.field_raw("sections", &sections);
        });
        d.field_object("pack", |pack| {
            pack.field_str("query", &response.data.pack.query);
            match &response.data.pack.hash {
                Some(hash) => pack.field_str("hash", hash),
                None => pack.field_raw("hash", "null"),
            };
            pack.field_object("budget", |budget| {
                budget.field_u32("maxTokens", response.data.pack.budget.max_tokens());
                budget.field_u32("usedTokens", response.data.pack.used_tokens);
            });
            let advisory_banner = response.data.advisory_banner();
            pack.field_object("advisoryBanner", |banner| {
                build_pack_advisory_banner(banner, &advisory_banner);
            });
            let quality_metrics = response.data.pack.quality_metrics();
            pack.field_object("quality", |quality| {
                build_pack_quality_metrics(quality, &quality_metrics);
            });
            pack.field_object("selectionCertificate", |certificate| {
                build_pack_selection_certificate(
                    certificate,
                    &response.data.pack.selection_certificate,
                );
            });
            pack.field_array_of_objects("items", &response.data.pack.items, |obj, item| {
                obj.field_u32("rank", item.rank);
                obj.field_str("memoryId", &item.memory_id.to_string());
                obj.field_str("section", item.section.as_str());
                obj.field_str("content", &item.content);
                obj.field_u32("estimatedTokens", item.estimated_tokens);
                obj.field_object("scores", |scores| {
                    scores.field_raw("relevance", &score_json(item.relevance.into_inner()));
                    scores.field_raw("utility", &score_json(item.utility.into_inner()));
                });
                obj.field_object("trust", |trust| {
                    trust.field_str("class", item.trust.class.as_str());
                    match item.trust.subclass.as_deref() {
                        Some(subclass) => trust.field_str("subclass", subclass),
                        None => trust.field_raw("subclass", "null"),
                    };
                    trust.field_str("posture", item.trust.posture().as_str());
                });
                let provenance = item.rendered_provenance();
                obj.field_array_of_objects("provenance", &provenance, build_rendered_provenance);
                obj.field_str("why", &item.why);
                if let Some(diversity_key) = &item.diversity_key {
                    obj.field_str("diversityKey", diversity_key);
                }
            });
            pack.field_array_of_objects("omitted", &response.data.pack.omitted, |obj, omission| {
                obj.field_str("memoryId", &omission.memory_id.to_string());
                obj.field_u32("estimatedTokens", omission.estimated_tokens);
                obj.field_str("reason", omission.reason.as_str());
            });
            let footer = response.data.pack.provenance_footer();
            pack.field_object("provenanceFooter", |obj| {
                obj.field_raw("memoryCount", &footer.memory_count.to_string());
                obj.field_raw("sourceCount", &footer.source_count.to_string());
                obj.field_raw(
                    "schemes",
                    &string_array_json(footer.schemes.iter().copied()),
                );
                obj.field_array_of_objects("entries", &footer.entries, build_item_provenance);
            });
        });
        d.field_array_of_objects("degraded", &response.data.degraded, |obj, degraded| {
            obj.field_str("code", &degraded.code);
            obj.field_str("severity", degraded.severity.as_str());
            obj.field_str("message", &degraded.message);
            if let Some(repair) = &degraded.repair {
                obj.field_str("repair", repair);
            }
        });
    });
    b.finish()
}

/// Render a context response as human-readable text.
#[must_use]
pub fn render_context_response_human(response: &ContextResponse) -> String {
    let mut output = String::new();
    output.push_str(&format!(
        "ee context \"{}\"\n\n",
        response.data.request.query
    ));
    output.push_str(&format!(
        "Profile: {} | Budget: {}/{} tokens\n\n",
        response.data.request.profile.as_str(),
        response.data.pack.used_tokens,
        response.data.pack.budget.max_tokens()
    ));

    let advisory_banner = response.data.advisory_banner();
    output.push_str(&format!(
        "Advisory: {} — {}\n\n",
        advisory_banner.status.as_str(),
        advisory_banner.summary
    ));
    if !advisory_banner.notes.is_empty() {
        output.push_str("Advisory notes:\n");
        for note in &advisory_banner.notes {
            output.push_str(&format!(
                "  [{}] {}: {}\n",
                note.severity.as_str(),
                note.code,
                note.message
            ));
        }
        output.push('\n');
    }

    if response.data.pack.items.is_empty() {
        output.push_str("No items in pack.\n");
    } else {
        output.push_str("Items:\n");
        for item in &response.data.pack.items {
            output.push_str(&format!(
                "  {}. [{}] {} ({}t)\n",
                item.rank,
                item.section.as_str(),
                item.memory_id,
                item.estimated_tokens
            ));
        }
    }

    if !response.data.degraded.is_empty() {
        output.push_str("\nDegraded:\n");
        for d in &response.data.degraded {
            output.push_str(&format!("  [{}] {}\n", d.severity.as_str(), d.message));
            if let Some(repair) = &d.repair {
                output.push_str(&format!("    Next: {repair}\n"));
            }
        }
    }

    output.push_str("\nNext:\n  ee context --json \"<query>\"\n");
    output
}

/// Render a context response as TOON.
#[must_use]
pub fn render_context_response_toon(response: &ContextResponse) -> String {
    render_toon_from_json(&render_context_response_json(response))
}

/// Render a context response as Markdown.
///
/// Produces a structured Markdown document suitable for direct inclusion
/// in agent context windows or documentation. Sections are organized by
/// pack section, with provenance and why explanations preserved.
#[must_use]
pub fn render_context_response_markdown(response: &ContextResponse) -> String {
    use std::collections::BTreeMap;

    let mut output = String::new();

    output.push_str(&format!(
        "# Context Pack: {}\n\n",
        response.data.request.query
    ));

    output.push_str(&format!(
        "**Profile:** {} | **Budget:** {}/{} tokens\n\n",
        response.data.request.profile.as_str(),
        response.data.pack.used_tokens,
        response.data.pack.budget.max_tokens()
    ));

    let advisory_banner = response.data.advisory_banner();
    output.push_str("## Advisory Memory Banner\n\n");
    output.push_str(&format!(
        "**Status:** `{}`\n\n",
        advisory_banner.status.as_str()
    ));
    output.push_str(&advisory_banner.summary);
    output.push_str("\n\n");
    if !advisory_banner.notes.is_empty() {
        for note in &advisory_banner.notes {
            output.push_str(&format!(
                "- **{}** `[{}]` {} Action: `{}`\n",
                note.severity.as_str(),
                note.code,
                note.message,
                note.action
            ));
        }
        output.push('\n');
    }

    if response.data.pack.items.is_empty() {
        output.push_str("*No items in pack.*\n\n");
    } else {
        let mut by_section: BTreeMap<&str, Vec<&PackDraftItem>> = BTreeMap::new();
        for item in &response.data.pack.items {
            by_section
                .entry(item.section.as_str())
                .or_default()
                .push(item);
        }

        for (section, items) in by_section {
            output.push_str(&format!("## {}\n\n", section_display_name(section)));
            for item in items {
                output.push_str(&format!(
                    "### {}. {} ({} tokens)\n\n",
                    item.rank, item.memory_id, item.estimated_tokens
                ));

                if !item.content.is_empty() {
                    output.push_str("```\n");
                    output.push_str(&item.content);
                    if !item.content.ends_with('\n') {
                        output.push('\n');
                    }
                    output.push_str("```\n\n");
                }

                if !item.why.is_empty() {
                    output.push_str(&format!("**Why:** {}\n\n", item.why));
                }

                output.push_str(&format!(
                    "**Trust:** `{}` / `{}`\n\n",
                    item.trust.class.as_str(),
                    item.trust.posture().as_str()
                ));

                if !item.provenance.is_empty() {
                    output.push_str("**Provenance:**\n");
                    for prov in item.rendered_provenance() {
                        output.push_str(&format!("- {} ({})\n", prov.uri, prov.scheme));
                    }
                    output.push('\n');
                }
            }
        }
    }

    if !response.data.pack.omitted.is_empty() {
        output.push_str("## Omitted\n\n");
        for omission in &response.data.pack.omitted {
            output.push_str(&format!(
                "- {} ({} tokens) — {}\n",
                omission.memory_id, omission.estimated_tokens, omission.reason
            ));
        }
        output.push('\n');
    }

    if !response.data.degraded.is_empty() {
        output.push_str("## Degradations\n\n");
        for d in &response.data.degraded {
            output.push_str(&format!("- **[{}]** {}\n", d.severity.as_str(), d.message));
            if let Some(repair) = &d.repair {
                output.push_str(&format!("  - *Repair:* `{}`\n", repair));
            }
        }
        output.push('\n');
    }

    output.push_str("---\n\n");
    output.push_str(&format!(
        "*Generated by `ee context \"{}\" --format markdown`*\n",
        response.data.request.query
    ));

    output
}

fn section_display_name(section: &str) -> &str {
    match section {
        "core" => "Core",
        "supporting" => "Supporting",
        "procedural" => "Procedural",
        "background" => "Background",
        "example" => "Example",
        other => other,
    }
}

fn build_pack_advisory_banner(obj: &mut JsonBuilder, banner: &PackAdvisoryBanner) {
    obj.field_str("status", banner.status.as_str());
    obj.field_str("summary", &banner.summary);
    obj.field_raw(
        "authoritativeCount",
        &banner.authoritative_count.to_string(),
    );
    obj.field_raw("advisoryCount", &banner.advisory_count.to_string());
    obj.field_raw("legacyCount", &banner.legacy_count.to_string());
    obj.field_raw("degradationCount", &banner.degradation_count.to_string());
    obj.field_array_of_objects("notes", &banner.notes, build_pack_advisory_note);
}

fn build_pack_advisory_note(obj: &mut JsonBuilder, note: &PackAdvisoryNote) {
    obj.field_str("code", note.code);
    obj.field_str("severity", note.severity.as_str());
    obj.field_str("message", &note.message);
    obj.field_raw(
        "memoryIds",
        &string_array_json(note.memory_ids.iter().map(String::as_str)),
    );
    obj.field_str("action", note.action);
}

fn build_pack_quality_metrics(obj: &mut JsonBuilder, metrics: &PackQualityMetrics) {
    obj.field_raw("itemCount", &metrics.item_count.to_string());
    obj.field_raw("omittedCount", &metrics.omitted_count.to_string());
    obj.field_u32("usedTokens", metrics.used_tokens);
    obj.field_u32("maxTokens", metrics.max_tokens);
    obj.field_raw("budgetUtilization", &score_json(metrics.budget_utilization));
    obj.field_raw("averageRelevance", &score_json(metrics.average_relevance));
    obj.field_raw("averageUtility", &score_json(metrics.average_utility));
    obj.field_raw(
        "provenanceSourceCount",
        &metrics.provenance_source_count.to_string(),
    );
    obj.field_raw(
        "provenanceSourcesPerItem",
        &score_json(metrics.provenance_sources_per_item),
    );
    obj.field_bool("provenanceComplete", metrics.provenance_complete);
    obj.field_array_of_objects("sections", &metrics.sections, build_pack_section_metric);
    obj.field_object("omissions", |omissions| {
        build_pack_omission_metrics(omissions, &metrics.omissions);
    });
}

fn build_pack_section_metric(obj: &mut JsonBuilder, metric: &PackSectionMetric) {
    obj.field_str("section", metric.section.as_str());
    obj.field_raw("itemCount", &metric.item_count.to_string());
    obj.field_u32("usedTokens", metric.used_tokens);
}

fn build_pack_omission_metrics(obj: &mut JsonBuilder, metrics: &PackOmissionMetrics) {
    obj.field_raw(
        "tokenBudgetExceeded",
        &metrics.token_budget_exceeded.to_string(),
    );
    obj.field_raw(
        "redundantCandidates",
        &metrics.redundant_candidates.to_string(),
    );
}

fn build_pack_selection_certificate(obj: &mut JsonBuilder, certificate: &PackSelectionCertificate) {
    if let Some(certificate_id) = &certificate.certificate_id {
        obj.field_str("certificateId", certificate_id);
    }
    obj.field_str("profile", certificate.profile.as_str());
    obj.field_str("objective", certificate.objective.as_str());
    obj.field_str("algorithm", certificate.algorithm);
    obj.field_str("guarantee", certificate.guarantee);
    obj.field_str("guaranteeStatus", certificate.guarantee_status.as_str());
    obj.field_object("guaranteeEvidence", |guarantee| {
        guarantee.field_str("status", certificate.guarantee_status.as_str());
        match &certificate.certificate_id {
            Some(certificate_id) => guarantee.field_str("certificateId", certificate_id),
            None => guarantee.field_raw("certificateId", "null"),
        };
        guarantee.field_bool("identityValid", certificate.has_valid_guarantee_identity());
        guarantee.field_str("summary", certificate.guarantee);
    });
    obj.field_raw("candidateCount", &certificate.candidate_count.to_string());
    obj.field_raw("selectedCount", &certificate.selected_count.to_string());
    obj.field_raw("omittedCount", &certificate.omitted_count.to_string());
    obj.field_u32("budgetLimit", certificate.budget_limit);
    obj.field_u32("budgetUsed", certificate.budget_used);
    obj.field_raw(
        "totalObjectiveValue",
        &score_json(certificate.total_objective_value),
    );
    obj.field_bool("monotone", certificate.monotone);
    obj.field_bool("submodular", certificate.submodular);
    obj.field_array_of_objects(
        "selectedItems",
        &certificate.selected_items,
        build_pack_selected_item,
    );
    obj.field_array_of_objects(
        "rejectedFrontier",
        &certificate.rejected_frontier,
        build_pack_rejected_frontier_item,
    );
    obj.field_array_of_objects("steps", &certificate.steps, build_pack_selection_step);
}

fn build_pack_selected_item(obj: &mut JsonBuilder, item: &PackSelectedItem) {
    obj.field_u32("rank", item.rank);
    obj.field_str("memoryId", &item.memory_id.to_string());
    obj.field_u32("tokenCost", item.token_cost);
    obj.field_bool("feasible", item.feasible);
}

fn build_pack_rejected_frontier_item(obj: &mut JsonBuilder, item: &PackRejectedFrontierItem) {
    obj.field_str("memoryId", &item.memory_id.to_string());
    obj.field_u32("tokenCost", item.token_cost);
    obj.field_str("reason", item.reason.as_str());
    obj.field_bool("feasible", item.feasible);
}

fn build_pack_selection_step(obj: &mut JsonBuilder, step: &PackSelectionStep) {
    obj.field_u32("rank", step.rank);
    obj.field_str("memoryId", &step.memory_id.to_string());
    obj.field_raw("marginalGain", &score_json(step.marginal_gain));
    obj.field_raw("objectiveValue", &score_json(step.objective_value));
    obj.field_u32("tokenCost", step.token_cost);
    obj.field_bool("feasible", step.feasible);
    obj.field_raw(
        "coveredFeatures",
        &string_array_json(step.covered_features.iter()),
    );
}

fn build_rendered_provenance(obj: &mut JsonBuilder, source: &RenderedPackProvenance) {
    obj.field_str("uri", &source.uri);
    obj.field_str("scheme", source.scheme);
    obj.field_str("label", &source.label);
    if let Some(locator) = &source.locator {
        obj.field_str("locator", locator);
    }
    obj.field_str("note", &source.note);
}

fn build_item_provenance(obj: &mut JsonBuilder, entry: &PackItemProvenance) {
    obj.field_u32("rank", entry.rank);
    obj.field_str("memoryId", &entry.memory_id.to_string());
    obj.field_u32("sourceIndex", entry.source_index);
    obj.field_object("source", |source| {
        build_rendered_provenance(source, &entry.source);
    });
}

fn score_json(score: f32) -> String {
    format!("{score:.6}")
}

fn string_array_json<I, S>(values: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut output = String::from("[");
    for (index, value) in values.into_iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push('"');
        output.push_str(&escape_json_string(value.as_ref()));
        output.push('"');
    }
    output.push(']');
    output
}

/// Render a status report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_status_json(report: &StatusReport) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "status");
        d.field_str("version", report.version);
        if let Some(workspace) = report.workspace.as_ref() {
            render_workspace_status_json(d, workspace);
        }
        d.field_object("capabilities", |c| {
            c.field_str("runtime", report.capabilities.runtime.as_str());
            c.field_str("storage", report.capabilities.storage.as_str());
            c.field_str("search", report.capabilities.search.as_str());
            c.field_str(
                "agentDetection",
                report.capabilities.agent_detection.as_str(),
            );
        });
        d.field_object("runtime", |r| {
            r.field_str("engine", report.runtime.engine);
            r.field_str("profile", report.runtime.profile);
            r.field_raw("workerThreads", &report.runtime.worker_threads.to_string());
            r.field_str("asyncBoundary", report.runtime.async_boundary);
        });
        render_memory_health_json(d, &report.memory_health);
        render_curation_health_json(d, &report.curation_health);
        render_feedback_health_json(d, &report.feedback_health);
        render_derived_assets_json(d, &report.derived_assets, true);
        render_agent_inventory_json(d, "agentInventory", &report.agent_inventory, false);
        d.field_array_of_objects("degraded", &report.degradations, |obj, deg| {
            obj.field_str("code", deg.code);
            obj.field_str("severity", deg.severity);
            obj.field_str("message", deg.message);
            obj.field_str("repair", deg.repair);
        });
    });
    b.finish()
}

fn render_workspace_status_json(
    parent: &mut JsonBuilder,
    workspace: &crate::core::status::WorkspaceStatusReport,
) {
    parent.field_object("workspace", |w| {
        w.field_str("source", workspace.source.as_str());
        w.field_str("root", &workspace.root.to_string_lossy());
        w.field_str("configDir", &workspace.config_dir.to_string_lossy());
        w.field_bool("markerPresent", workspace.marker_present);
        w.field_str("canonicalRoot", &workspace.canonical_root.to_string_lossy());
        w.field_str("fingerprint", &workspace.fingerprint);
        w.field_str("scopeKind", &workspace.scope_kind);
        if let Some(repository_root) = workspace.repository_root.as_ref() {
            w.field_str("repositoryRoot", &repository_root.to_string_lossy());
        } else {
            w.field_raw("repositoryRoot", "null");
        }
        if let Some(repository_fingerprint) = workspace.repository_fingerprint.as_ref() {
            w.field_str("repositoryFingerprint", repository_fingerprint);
        } else {
            w.field_raw("repositoryFingerprint", "null");
        }
        if let Some(subproject_path) = workspace.subproject_path.as_ref() {
            w.field_str("subprojectPath", &subproject_path.to_string_lossy());
        } else {
            w.field_raw("subprojectPath", "null");
        }
        w.field_array_of_objects("diagnostics", &workspace.diagnostics, |obj, diagnostic| {
            obj.field_str("code", diagnostic.code);
            obj.field_str("severity", diagnostic.severity.as_str());
            obj.field_str("message", &diagnostic.message);
            obj.field_str("repair", &diagnostic.repair);
            if let Some(source) = diagnostic.selected_source {
                obj.field_str("selectedSource", source.as_str());
            }
            if let Some(root) = diagnostic.selected_root.as_ref() {
                obj.field_str("selectedRoot", &root.to_string_lossy());
            }
            if let Some(source) = diagnostic.conflicting_source {
                obj.field_str("conflictingSource", source.as_str());
            }
            if let Some(root) = diagnostic.conflicting_root.as_ref() {
                obj.field_str("conflictingRoot", &root.to_string_lossy());
            }
            if !diagnostic.marker_roots.is_empty() {
                let marker_roots = diagnostic
                    .marker_roots
                    .iter()
                    .map(|root| root.to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                obj.field_raw(
                    "markerRoots",
                    &string_array_json(marker_roots.iter().map(String::as_str)),
                );
            }
        });
    });
}

fn render_memory_health_json(
    parent: &mut JsonBuilder,
    health: &crate::core::status::MemoryHealthReport,
) {
    parent.field_object("memoryHealth", |h| {
        h.field_str("status", health.status.as_str());
        h.field_u32("totalCount", health.total_count);
        h.field_u32("activeCount", health.active_count);
        h.field_u32("tombstonedCount", health.tombstoned_count);
        h.field_u32("staleCount", health.stale_count);
        field_optional_score(h, "healthScore", health.health_score);
        match health.average_confidence {
            Some(c) => h.field_raw("averageConfidence", &format!("{c:.2}")),
            None => h.field_raw("averageConfidence", "null"),
        };
        match health.provenance_coverage {
            Some(c) => h.field_raw("provenanceCoverage", &format!("{c:.2}")),
            None => h.field_raw("provenanceCoverage", "null"),
        };
        h.field_object("scoreComponents", |components| {
            if let Some(score) = health.score_components {
                field_optional_score(components, "activeRatio", Some(score.active_ratio));
                field_optional_score(components, "freshnessScore", Some(score.freshness_score));
                field_optional_score(components, "confidenceScore", Some(score.confidence_score));
                field_optional_score(components, "provenanceScore", Some(score.provenance_score));
                field_optional_score(
                    components,
                    "tombstonePenalty",
                    Some(score.tombstone_penalty),
                );
            } else {
                field_optional_score(components, "activeRatio", None);
                field_optional_score(components, "freshnessScore", None);
                field_optional_score(components, "confidenceScore", None);
                field_optional_score(components, "provenanceScore", None);
                field_optional_score(components, "tombstonePenalty", None);
            }
        });
    });
}

fn render_curation_health_json(
    parent: &mut JsonBuilder,
    health: &crate::core::status::CurationHealthReport,
) {
    parent.field_object("curationHealth", |h| {
        h.field_str("status", health.status.as_str());
        h.field_u32("totalCount", health.total_count);
        h.field_u32("pendingCount", health.pending_count);
        h.field_u32("acceptedCount", health.accepted_count);
        h.field_u32("snoozedCount", health.snoozed_count);
        h.field_u32("rejectedCount", health.rejected_count);
        h.field_u32("dueCount", health.due_count);
        h.field_u32("promptCount", health.prompt_count);
        h.field_u32("escalationCount", health.escalation_count);
        h.field_u32("blockedCount", health.blocked_count);
        h.field_u32("policyCount", health.policy_count);
        h.field_u32("autoPromoteEnabledCount", health.auto_promote_enabled_count);
        match health.oldest_pending_age_days {
            Some(days) => h.field_raw("oldestPendingAgeDays", &days.to_string()),
            None => h.field_raw("oldestPendingAgeDays", "null"),
        };
        match health.mean_review_latency_days {
            Some(days) => h.field_raw("meanReviewLatencyDays", &days.to_string()),
            None => h.field_raw("meanReviewLatencyDays", "null"),
        };
        match health.next_scheduled_at.as_deref() {
            Some(next) => h.field_str("nextScheduledAt", next),
            None => h.field_raw("nextScheduledAt", "null"),
        };
    });
}

fn render_feedback_health_json(
    parent: &mut JsonBuilder,
    health: &crate::core::status::FeedbackHealthReport,
) {
    parent.field_object("feedbackHealth", |h| {
        h.field_str("status", health.status.as_str());
        h.field_u32(
            "harmfulPerSourcePerHour",
            health.harmful_per_source_per_hour,
        );
        h.field_u32(
            "harmfulBurstWindowSeconds",
            health.harmful_burst_window_seconds,
        );
        h.field_array_of_objects(
            "perSourceHarmfulCounts",
            &health.per_source_harmful_counts,
            |obj, source| {
                obj.field_str("sourceId", &source.source_id);
                obj.field_u32("harmfulCount", source.harmful_count);
            },
        );
        h.field_u32("quarantineQueueDepth", health.quarantine_queue_depth);
        h.field_u32("protectedRuleCount", health.protected_rule_count);
        match health.last_inversion_event.as_deref() {
            Some(event) => h.field_str("lastInversionEvent", event),
            None => h.field_raw("lastInversionEvent", "null"),
        };
        h.field_str("nextDeterministicAction", &health.next_deterministic_action);
    });
}

fn field_optional_score(builder: &mut JsonBuilder, key: &str, score: Option<f32>) {
    match score {
        Some(score) => builder.field_raw(key, &format!("{score:.2}")),
        None => builder.field_raw(key, "null"),
    };
}

fn render_derived_assets_json(
    parent: &mut JsonBuilder,
    assets: &[crate::core::status::DerivedAssetReport],
    include_repair: bool,
) {
    parent.field_array_of_objects("derivedAssets", assets, |obj, asset| {
        obj.field_str("name", asset.name);
        obj.field_str("status", asset.status.as_str());
        match asset.source_high_watermark {
            Some(value) => obj.field_raw("sourceHighWatermark", &value.to_string()),
            None => obj.field_raw("sourceHighWatermark", "null"),
        };
        match asset.asset_high_watermark {
            Some(value) => obj.field_raw("assetHighWatermark", &value.to_string()),
            None => obj.field_raw("assetHighWatermark", "null"),
        };
        match asset.high_watermark_lag {
            Some(value) => obj.field_raw("highWatermarkLag", &value.to_string()),
            None => obj.field_raw("highWatermarkLag", "null"),
        };
        obj.field_str("path", asset.path);
        if include_repair && let Some(repair) = asset.repair {
            obj.field_str("repair", repair);
        }
    });
}

fn render_agent_inventory_json(
    parent: &mut JsonBuilder,
    field_name: &str,
    inventory: &AgentInventoryReport,
    include_agents: bool,
) {
    parent.field_object(field_name, |agent| {
        agent.field_str("schema", inventory.schema);
        agent.field_str("status", inventory.status.as_str());
        agent.field_u32("formatVersion", inventory.format_version);
        agent.field_object("summary", |summary| {
            summary.field_u32("detectedCount", inventory.summary.detected_count as u32);
            summary.field_u32("totalCount", inventory.summary.total_count as u32);
        });
        agent.field_str("inspectionCommand", inventory.inspection_command);
        if include_agents {
            agent.field_array_of_objects(
                "installedAgents",
                &inventory.installed_agents,
                |obj, item| {
                    obj.field_str("slug", &item.slug);
                    obj.field_bool("detected", item.detected);
                    obj.field_raw("evidence", &strings_to_json_array(&item.evidence));
                    obj.field_raw("rootPaths", &strings_to_json_array(&item.root_paths));
                },
            );
        }
        agent.field_array_of_objects("degraded", &inventory.degraded, |obj, degraded| {
            obj.field_str("code", &degraded.code);
            obj.field_str("severity", degraded.severity);
            obj.field_str("message", &degraded.message);
            obj.field_str("repair", degraded.repair);
        });
    });
}

/// Render a status report as JSON with optional timing metadata.
///
/// When `timing` is provided, adds a `meta` object with timing fields.
#[must_use]
pub fn render_status_json_with_meta(
    report: &StatusReport,
    timing: Option<&crate::models::DiagnosticTiming>,
) -> String {
    let mut b = JsonBuilder::with_capacity(1024);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "status");
        d.field_str("version", report.version);
        if let Some(workspace) = report.workspace.as_ref() {
            render_workspace_status_json(d, workspace);
        }
        d.field_object("capabilities", |c| {
            c.field_str("runtime", report.capabilities.runtime.as_str());
            c.field_str("storage", report.capabilities.storage.as_str());
            c.field_str("search", report.capabilities.search.as_str());
            c.field_str(
                "agentDetection",
                report.capabilities.agent_detection.as_str(),
            );
        });
        d.field_object("runtime", |r| {
            r.field_str("engine", report.runtime.engine);
            r.field_str("profile", report.runtime.profile);
            r.field_raw("workerThreads", &report.runtime.worker_threads.to_string());
            r.field_str("asyncBoundary", report.runtime.async_boundary);
        });
        render_memory_health_json(d, &report.memory_health);
        render_derived_assets_json(d, &report.derived_assets, true);
        render_agent_inventory_json(d, "agentInventory", &report.agent_inventory, false);
        d.field_array_of_objects("degraded", &report.degradations, |obj, deg| {
            obj.field_str("code", deg.code);
            obj.field_str("severity", deg.severity);
            obj.field_str("message", deg.message);
            obj.field_str("repair", deg.repair);
        });
    });
    if let Some(t) = timing {
        b.field_object("meta", |m| {
            m.field_object("timing", |tm| {
                tm.field_raw("elapsedMs", &format!("{:.3}", t.elapsed_ms));
                if !t.phases.is_empty() {
                    tm.field_array_of_objects("phases", &t.phases, |obj, phase| {
                        obj.field_str("name", phase.name);
                        obj.field_raw("durationMs", &format!("{:.3}", phase.duration_ms));
                    });
                }
            });
        });
    }
    b.finish()
}

/// Render a status report as human-readable text.
#[must_use]
pub fn render_status_human(report: &StatusReport) -> String {
    let workspace_line = report
        .workspace
        .as_ref()
        .map_or_else(String::new, |workspace| {
            let diagnostics = workspace.diagnostics.len();
            format!(
                "workspace: {} ({}; {} diagnostic{})\n",
                workspace.root.display(),
                workspace.source.as_str(),
                diagnostics,
                if diagnostics == 1 { "" } else { "s" }
            )
        });
    format!(
        "ee status\n\n{}storage: {}\nsearch: {}\nagent detection: {}\nruntime: {} ({} {})\n\nNext:\n  ee status --json\n",
        workspace_line,
        report.capabilities.storage.as_str(),
        report.capabilities.search.as_str(),
        report.capabilities.agent_detection.as_str(),
        report.capabilities.runtime.as_str(),
        report.runtime.engine,
        report.runtime.profile
    )
}

/// Render a status report as TOON (Terse Object Output Notation).
#[must_use]
pub fn render_status_toon(report: &StatusReport) -> String {
    render_status_toon_filtered(report, FieldProfile::Standard)
}

/// Render a status report as TOON with field filtering.
#[must_use]
pub fn render_status_toon_filtered(report: &StatusReport, profile: FieldProfile) -> String {
    render_toon_from_json(&render_status_json_filtered(report, profile))
}

/// Render a doctor report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_doctor_json(report: &DoctorReport) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.overall_healthy);
    b.field_object("data", |d| {
        d.field_str("command", "doctor");
        d.field_str("version", report.version);
        d.field_bool("healthy", report.overall_healthy);
        d.field_array_of_objects("checks", &report.checks, |obj, check| {
            obj.field_str("name", check.name);
            obj.field_str("severity", check.severity.as_str());
            obj.field_str("message", &check.message);
            if let Some(code) = check.error_code {
                obj.field_str("errorCode", code.id);
            }
            if let Some(repair) = check.repair {
                obj.field_str("repair", repair);
            }
        });
    });
    b.finish()
}

/// Render a doctor report as human-readable text.
#[must_use]
pub fn render_doctor_human(report: &DoctorReport) -> String {
    let mut output = String::from("ee doctor\n\n");

    for check in &report.checks {
        let icon = match check.severity {
            crate::core::doctor::CheckSeverity::Ok => "✓",
            crate::core::doctor::CheckSeverity::Warning => "⚠",
            crate::core::doctor::CheckSeverity::Error => "✗",
        };
        output.push_str(&format!("{} {}: {}\n", icon, check.name, check.message));
        if let Some(repair) = check.repair {
            output.push_str(&format!("  repair: {}\n", repair));
        }
    }

    if report.overall_healthy {
        output.push_str("\nAll checks passed.\n");
    } else {
        output.push_str("\nSome checks failed. Run suggested repairs to fix issues.\n");
    }

    output
}

/// Render a doctor report as TOON.
#[must_use]
pub fn render_doctor_toon(report: &DoctorReport) -> String {
    render_toon_from_json(&render_doctor_json(report))
}

/// Render a doctor report as a deterministic Mermaid diagram.
#[must_use]
pub fn render_doctor_mermaid(report: &DoctorReport) -> String {
    let mut output = String::from("flowchart TD\n");
    let summary = if report.overall_healthy {
        "ee doctor: healthy"
    } else {
        "ee doctor: needs repair"
    };
    output.push_str(&format!(
        "  doctor[\"{}\"]\n",
        escape_mermaid_label(summary)
    ));

    for (index, check) in report.checks.iter().enumerate() {
        let node_id = format!("check{}", index + 1);
        let label = format!("{}: {}", check.name, check.severity.as_str());
        output.push_str(&format!(
            "  {}[\"{}\"]\n",
            node_id,
            escape_mermaid_label(&label)
        ));
        output.push_str(&format!("  doctor --> {}\n", node_id));

        if let Some(repair) = check.repair {
            let repair_id = format!("repair{}", index + 1);
            output.push_str(&format!(
                "  {}[\"{}\"]\n",
                repair_id,
                escape_mermaid_label(repair)
            ));
            output.push_str(&format!("  {} -. repair .-> {}\n", node_id, repair_id));
        }
    }

    output
}

/// Render a why report as a deterministic Mermaid diagram.
#[must_use]
pub fn render_why_mermaid(report: &WhyReport) -> String {
    let mut output = String::from("flowchart TD\n");
    output.push_str(&format!(
        "  memory[\"memory: {}\"]\n",
        escape_mermaid_label(&report.memory_id)
    ));

    if let Some(storage) = &report.storage {
        let label = format!("storage: {} / {}", storage.origin, storage.trust_class);
        output.push_str(&format!(
            "  storage[\"{}\"]\n",
            escape_mermaid_label(&label)
        ));
        output.push_str("  memory --> storage\n");
    }

    if let Some(retrieval) = &report.retrieval {
        let label = format!(
            "retrieval: {} {} confidence {:.2}",
            retrieval.level, retrieval.kind, retrieval.confidence
        );
        output.push_str(&format!(
            "  retrieval[\"{}\"]\n",
            escape_mermaid_label(&label)
        ));
        if report.storage.is_some() {
            output.push_str("  storage --> retrieval\n");
        } else {
            output.push_str("  memory --> retrieval\n");
        }
    }

    if let Some(selection) = &report.selection {
        let label = format!(
            "selection: score {:.2}, active {}",
            selection.selection_score, selection.is_active
        );
        output.push_str(&format!(
            "  selection[\"{}\"]\n",
            escape_mermaid_label(&label)
        ));
        if report.retrieval.is_some() {
            output.push_str("  retrieval --> selection\n");
        } else {
            output.push_str("  memory --> selection\n");
        }

        if let Some(pack) = &selection.latest_pack_selection {
            let pack_label = format!("pack: {} rank {}", pack.pack_id, pack.rank);
            output.push_str(&format!(
                "  pack[\"{}\"]\n",
                escape_mermaid_label(&pack_label)
            ));
            output.push_str("  selection --> pack\n");
        }
    }

    for (index, link) in report.links.iter().enumerate() {
        let node_id = format!("link{}", index + 1);
        let label = format!(
            "{} {} {}",
            link.direction, link.relation, link.linked_memory_id
        );
        output.push_str(&format!(
            "  {}[\"{}\"]\n",
            node_id,
            escape_mermaid_label(&label)
        ));
        output.push_str(&format!("  memory --> {}\n", node_id));
    }

    for (index, trace) in report.rationale_traces.iter().enumerate() {
        let node_id = format!("rationale{}", index + 1);
        let label = format!(
            "rationale: {} {} {}",
            trace.kind, trace.posture, trace.summary
        );
        output.push_str(&format!(
            "  {}[\"{}\"]\n",
            node_id,
            escape_mermaid_label(&label)
        ));
        output.push_str(&format!("  memory --> {}\n", node_id));
    }

    for (index, degraded) in report.degraded.iter().enumerate() {
        let node_id = format!("degraded{}", index + 1);
        let label = format!("degraded: {} ({})", degraded.code, degraded.severity);
        output.push_str(&format!(
            "  {}[\"{}\"]\n",
            node_id,
            escape_mermaid_label(&label)
        ));
        output.push_str(&format!("  memory -.-> {}\n", node_id));
    }

    output
}

/// Render a fix plan as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_fix_plan_json(plan: &FixPlan) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "doctor");
        d.field_str("mode", "fix-plan");
        d.field_str("version", plan.version);
        d.field_raw("totalIssues", &plan.total_issues.to_string());
        d.field_raw("fixableIssues", &plan.fixable_issues.to_string());
        d.field_array_of_objects("steps", &plan.steps, |obj, step| {
            obj.field_raw("order", &step.order.to_string());
            obj.field_str("subsystem", step.subsystem);
            obj.field_str("severity", step.severity.as_str());
            obj.field_str("issue", &step.issue);
            if let Some(code) = step.error_code {
                obj.field_str("errorCode", code.id);
            }
            obj.field_str("command", step.command);
        });
        d.field_object("cassImportGuidance", |guidance| {
            guidance.field_str("status", plan.cass_import_guidance.status.as_str());
            guidance.field_raw(
                "detectedAgentCount",
                &plan.cass_import_guidance.detected_agent_count.to_string(),
            );
            guidance.field_raw(
                "detectedRootCount",
                &plan.cass_import_guidance.detected_root_count.to_string(),
            );
            guidance.field_str("message", &plan.cass_import_guidance.message);
            guidance.field_array_of_objects(
                "roots",
                &plan.cass_import_guidance.roots,
                |obj, root| {
                    obj.field_str("connector", &root.connector);
                    obj.field_str("rootPath", &root.root_path);
                    obj.field_str("guidance", &root.guidance);
                },
            );
            guidance.field_array_of_strings(
                "suggestedCommands",
                &plan.cass_import_guidance.suggested_commands,
            );
        });
        let mut all_suggested: Vec<String> = plan
            .steps
            .iter()
            .map(|step| step.command.to_string())
            .collect();
        all_suggested.extend(plan.cass_import_guidance.suggested_commands.iter().cloned());
        d.field_array_of_strings("suggestedCommands", &all_suggested);
    });
    b.finish()
}

/// Render a fix plan as human-readable text.
#[must_use]
pub fn render_fix_plan_human(plan: &FixPlan) -> String {
    let mut output = String::from("ee doctor --fix-plan\n\n");

    if plan.is_empty() {
        output.push_str("No issues to fix. All subsystems are healthy.\n");
    } else {
        output.push_str(&format!(
            "Found {} issue(s), {} fixable:\n\n",
            plan.total_issues, plan.fixable_issues
        ));

        for step in &plan.steps {
            output.push_str(&format!(
                "{}. [{}] {}\n   Issue: {}\n   Fix:   {}\n\n",
                step.order,
                step.subsystem,
                step.severity.as_str().to_uppercase(),
                step.issue,
                step.command
            ));
        }

        if plan.fixable_issues > 0 {
            output.push_str("Run commands in order to resolve issues.\n");
        }
    }

    output.push_str("\nCASS import guidance:\n");
    output.push_str(&format!(
        "  Status: {}\n",
        plan.cass_import_guidance.status.as_str()
    ));
    output.push_str(&format!(
        "  Message: {}\n",
        plan.cass_import_guidance.message
    ));
    if !plan.cass_import_guidance.roots.is_empty() {
        output.push_str("  Detected roots:\n");
        for root in &plan.cass_import_guidance.roots {
            output.push_str(&format!("    - {}: {}\n", root.connector, root.root_path));
        }
    }
    if !plan.cass_import_guidance.suggested_commands.is_empty() {
        output.push_str("  Suggested commands:\n");
        for command in &plan.cass_import_guidance.suggested_commands {
            output.push_str(&format!("    {command}\n"));
        }
    }

    output
}

/// Render a fix plan as TOON.
#[must_use]
pub fn render_fix_plan_toon(plan: &FixPlan) -> String {
    render_toon_from_json(&render_fix_plan_json(plan))
}

/// Render dependency diagnostics as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_dependency_diagnostics_json(report: &DependencyDiagnosticsReport) -> String {
    let mut b = JsonBuilder::with_capacity(4096);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.summary.forbidden_default_hit_count == 0);
    b.field_object("data", |d| {
        d.field_str("command", "diag dependencies");
        d.field_str("version", report.version);
        d.field_str("schema", report.schema);
        d.field_raw("matrixRevision", &report.matrix_revision.to_string());
        d.field_object("source", |source| {
            source.field_str("bead", report.source_bead);
            source.field_str("planItem", report.source_plan_item);
        });
        d.field_str("defaultFeatureProfile", report.default_feature_profile);
        render_dependency_diagnostics_summary(d, report);
        d.field_array_of_strs("forbiddenCrates", report.forbidden_crates);
        d.field_array_of_objects("entries", report.entries, render_dependency_contract_entry);
        d.field_object("driftPolicy", |policy| {
            policy.field_str(
                "cargoUpdateDryRun",
                report.drift_policy.cargo_update_dry_run,
            );
            policy.field_array_of_strs("failConditions", report.drift_policy.fail_conditions);
            policy.field_str(
                "runtimeDiagnosticOwner",
                report.drift_policy.runtime_diagnostic_owner,
            );
        });
    });
    b.finish()
}

/// Render dependency diagnostics as human-readable text.
#[must_use]
pub fn render_dependency_diagnostics_human(report: &DependencyDiagnosticsReport) -> String {
    let mut output = String::from("ee diag dependencies\n\n");
    output.push_str(&format!(
        "matrix: revision {} ({}/{})\n",
        report.matrix_revision, report.source_bead, report.source_plan_item
    ));
    output.push_str(&format!(
        "dependencies: {} total, {} default-enabled, {} forbidden default hits\n",
        report.summary.total_dependencies,
        report.summary.default_enabled_count,
        report.summary.forbidden_default_hit_count
    ));
    output.push_str(&format!(
        "blocked feature gates: {}\n\n",
        report.summary.blocked_feature_count
    ));

    for entry in report.entries {
        output.push_str(&format!(
            "- {} [{}] {} via {}\n",
            entry.name, entry.owning_surface, entry.status, entry.diagnostic_command
        ));
    }

    output
}

/// Render dependency diagnostics as TOON.
#[must_use]
pub fn render_dependency_diagnostics_toon(report: &DependencyDiagnosticsReport) -> String {
    render_toon_from_json(&render_dependency_diagnostics_json(report))
}

/// Render integrity diagnostics as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_integrity_diagnostics_json(report: &IntegrityDiagnosticsReport) -> String {
    let mut b = JsonBuilder::with_capacity(2048);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.success());
    b.field_object("data", |d| {
        d.field_str("command", "diag integrity");
        d.field_str("version", report.version);
        d.field_str("schema", report.schema);
        d.field_str("status", report.status.as_str());
        d.field_str("workspaceId", &report.workspace_id);
        d.field_str("databasePath", &report.database_path.to_string_lossy());
        d.field_u32("sampleSize", report.sample_size);
        d.field_array_of_objects("checks", &report.checks, build_integrity_check);
        d.field_object("provenanceSample", |sample| {
            match report.provenance_sample.as_ref() {
                Some(provenance) => build_provenance_sample(sample, provenance),
                None => {
                    let empty_records: &[crate::db::ProvenanceVerificationRecord] = &[];
                    sample.field_str("workspaceId", &report.workspace_id);
                    sample.field_u32("requestedSampleSize", report.sample_size);
                    sample.field_u32("checkedCount", 0);
                    sample.field_u32("verifiedCount", 0);
                    sample.field_u32("missingCount", 0);
                    sample.field_u32("mismatchCount", 0);
                    sample.field_array_of_objects(
                        "records",
                        empty_records,
                        build_provenance_record,
                    );
                }
            }
        });
        d.field_object("canary", |canary| {
            build_integrity_canary(canary, &report.canary)
        });
        d.field_array_of_objects("degraded", &report.degraded, build_integrity_degradation);
    });
    b.finish()
}

fn build_integrity_check(obj: &mut JsonBuilder, check: &IntegrityDiagnosticCheck) {
    obj.field_str("name", check.name);
    obj.field_str("severity", check.severity.as_str());
    obj.field_str("message", &check.message);
    field_optional_str(obj, "repair", check.repair);
}

fn build_provenance_sample(
    obj: &mut JsonBuilder,
    report: &crate::db::ProvenanceSampleVerificationReport,
) {
    obj.field_str("workspaceId", &report.workspace_id);
    obj.field_u32("requestedSampleSize", report.requested_sample_size);
    obj.field_u32("checkedCount", report.checked_count);
    obj.field_u32("verifiedCount", report.verified_count);
    obj.field_u32("missingCount", report.missing_count);
    obj.field_u32("mismatchCount", report.mismatch_count);
    obj.field_array_of_objects("records", &report.records, build_provenance_record);
}

fn build_provenance_record(
    obj: &mut JsonBuilder,
    record: &crate::db::ProvenanceVerificationRecord,
) {
    obj.field_str("memoryId", &record.memory_id);
    field_optional_str(obj, "storedHash", record.stored_hash.as_deref());
    obj.field_str("expectedHash", &record.expected_hash);
    obj.field_str("status", &record.status);
    obj.field_str("verifiedAt", &record.verified_at);
    obj.field_str("note", &record.note);
}

fn build_integrity_canary(obj: &mut JsonBuilder, canary: &IntegrityCanaryReport) {
    obj.field_bool("requested", canary.requested);
    obj.field_bool("dryRun", canary.dry_run);
    obj.field_str("memoryId", canary.memory_id);
    obj.field_str("status", canary.status.as_str());
    obj.field_str("message", &canary.message);
    field_optional_str(obj, "repair", canary.repair);
}

fn build_integrity_degradation(obj: &mut JsonBuilder, degraded: &IntegrityDiagnosticDegradation) {
    obj.field_str("code", degraded.code);
    obj.field_str("severity", degraded.severity);
    obj.field_str("message", &degraded.message);
    field_optional_str(obj, "repair", degraded.repair);
}

/// Render integrity diagnostics as human-readable text.
#[must_use]
pub fn render_integrity_diagnostics_human(report: &IntegrityDiagnosticsReport) -> String {
    let mut output = format!(
        "ee diag integrity (v{})\n\nStatus: {}\nDatabase: {}\n\n",
        report.version,
        report.status.as_str(),
        report.database_path.display()
    );

    output.push_str("Checks:\n");
    for check in &report.checks {
        output.push_str(&format!(
            "  [{}] {}: {}\n",
            check.severity.as_str(),
            check.name,
            check.message
        ));
    }

    output.push_str(&format!(
        "\nCanary: {} ({})\n",
        report.canary.status.as_str(),
        report.canary.memory_id
    ));

    if let Some(sample) = report.provenance_sample.as_ref() {
        output.push_str(&format!(
            "Provenance sample: {} checked, {} verified, {} missing, {} mismatched\n",
            sample.checked_count,
            sample.verified_count,
            sample.missing_count,
            sample.mismatch_count
        ));
    }

    output.push_str("\nNext:\n  ee diag integrity --json\n");
    output
}

/// Render integrity diagnostics as TOON.
#[must_use]
pub fn render_integrity_diagnostics_toon(report: &IntegrityDiagnosticsReport) -> String {
    render_toon_from_json(&render_integrity_diagnostics_json(report))
}

/// Render franken-stack doctor health as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_franken_health_json(report: &FrankenHealthReport) -> String {
    let mut b = JsonBuilder::with_capacity(4096);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.healthy);
    b.field_object("data", |d| {
        d.field_str("command", "doctor");
        d.field_str("mode", "franken-health");
        d.field_str("version", report.version);
        d.field_str("schema", report.schema);
        d.field_bool("healthy", report.healthy);
        d.field_object("summary", |summary| {
            summary.field_raw(
                "totalDependencies",
                &report.summary.total_dependencies.to_string(),
            );
            summary.field_raw("readyCount", &report.summary.ready_count.to_string());
            summary.field_raw(
                "featureGatedCount",
                &report.summary.feature_gated_count.to_string(),
            );
            summary.field_raw(
                "notLinkedCount",
                &report.summary.not_linked_count.to_string(),
            );
            summary.field_raw(
                "defaultEnabledCount",
                &report.summary.default_enabled_count.to_string(),
            );
            summary.field_raw(
                "localSourceCount",
                &report.summary.local_source_count.to_string(),
            );
            summary.field_raw(
                "forbiddenDefaultHitCount",
                &report.summary.forbidden_default_hit_count.to_string(),
            );
            summary.field_raw(
                "blockedFeatureCount",
                &report.summary.blocked_feature_count.to_string(),
            );
        });
        d.field_array_of_objects("dependencies", &report.dependencies, |obj, dependency| {
            render_franken_dependency_health(obj, dependency);
        });
    });
    b.finish()
}

/// Render franken-stack doctor health as human-readable text.
#[must_use]
pub fn render_franken_health_human(report: &FrankenHealthReport) -> String {
    let mut output = String::from("ee doctor --franken-health\n\n");
    output.push_str(&format!(
        "healthy: {}\nready: {}/{}\nfeature-gated: {}\nblocked feature gates: {}\n\n",
        report.healthy,
        report.summary.ready_count,
        report.summary.total_dependencies,
        report.summary.feature_gated_count,
        report.summary.blocked_feature_count
    ));

    for dependency in &report.dependencies {
        output.push_str(&format!(
            "- {} [{}]: {} ({})\n",
            dependency.name, dependency.owning_surface, dependency.readiness, dependency.status
        ));
    }

    output
}

/// Render franken-stack doctor health as TOON.
#[must_use]
pub fn render_franken_health_toon(report: &FrankenHealthReport) -> String {
    render_toon_from_json(&render_franken_health_json(report))
}

fn render_dependency_diagnostics_summary(
    d: &mut JsonBuilder,
    report: &DependencyDiagnosticsReport,
) {
    d.field_object("summary", |summary| {
        summary.field_raw(
            "totalDependencies",
            &report.summary.total_dependencies.to_string(),
        );
        summary.field_raw(
            "acceptedDefaultCount",
            &report.summary.accepted_default_count.to_string(),
        );
        summary.field_raw(
            "acceptedExternalCount",
            &report.summary.accepted_external_count.to_string(),
        );
        summary.field_raw(
            "optionalFeatureGatedCount",
            &report.summary.optional_feature_gated_count.to_string(),
        );
        summary.field_raw(
            "plannedNotLinkedCount",
            &report.summary.planned_not_linked_count.to_string(),
        );
        summary.field_raw(
            "defaultEnabledCount",
            &report.summary.default_enabled_count.to_string(),
        );
        summary.field_raw(
            "forbiddenDefaultHitCount",
            &report.summary.forbidden_default_hit_count.to_string(),
        );
        summary.field_raw(
            "blockedFeatureCount",
            &report.summary.blocked_feature_count.to_string(),
        );
    });
}

fn render_dependency_contract_entry(obj: &mut JsonBuilder, entry: &DependencyContractEntry) {
    obj.field_str("name", entry.name);
    obj.field_str("kind", entry.kind);
    obj.field_str("owningSurface", entry.owning_surface);
    obj.field_str("status", entry.status);
    obj.field_str("readiness", entry.readiness());
    obj.field_bool("enabledByDefault", entry.enabled_by_default);
    render_dependency_source(obj, "source", &entry.source);
    render_dependency_feature_profile(obj, "defaultFeatureProfile", &entry.default_feature_profile);
    obj.field_array_of_objects(
        "optionalFeatureProfiles",
        entry.optional_feature_profiles,
        render_optional_feature_profile,
    );
    obj.field_array_of_objects(
        "blockedFeatures",
        entry.blocked_features,
        render_blocked_feature,
    );
    obj.field_array_of_strs(
        "forbiddenTransitiveDependencies",
        entry.forbidden_transitive_dependencies,
    );
    obj.field_str("minimumSmokeTest", entry.minimum_smoke_test);
    obj.field_str("degradationCode", entry.degradation_code);
    obj.field_array_of_strs("statusFields", entry.status_fields);
    obj.field_str("diagnosticCommand", entry.diagnostic_command);
    obj.field_str("releasePinDecision", entry.release_pin_decision);
}

fn render_franken_dependency_health(obj: &mut JsonBuilder, dependency: &FrankenDependencyHealth) {
    obj.field_str("name", dependency.name);
    obj.field_str("owningSurface", dependency.owning_surface);
    obj.field_str("status", dependency.status);
    obj.field_str("readiness", dependency.readiness);
    obj.field_bool("enabledByDefault", dependency.enabled_by_default);
    render_dependency_source(obj, "source", &dependency.source);
    render_dependency_feature_profile(
        obj,
        "defaultFeatureProfile",
        &dependency.default_feature_profile,
    );
    obj.field_array_of_objects(
        "blockedFeatures",
        dependency.blocked_features,
        render_blocked_feature,
    );
    obj.field_array_of_strs(
        "forbiddenTransitiveDependencies",
        dependency.forbidden_transitive_dependencies,
    );
    obj.field_str("degradationCode", dependency.degradation_code);
    obj.field_str("diagnosticCommand", dependency.diagnostic_command);
    obj.field_str("minimumSmokeTest", dependency.minimum_smoke_test);
    obj.field_str("releasePinDecision", dependency.release_pin_decision);
}

fn render_dependency_source(obj: &mut JsonBuilder, key: &str, source: &DependencySource) {
    obj.field_object(key, |source_json| {
        source_json.field_str("kind", source.kind);
        source_json.field_str("version", source.version);
        source_json.field_str("path", source.path);
    });
}

fn render_dependency_feature_profile(
    obj: &mut JsonBuilder,
    key: &str,
    profile: &DependencyFeatureProfile,
) {
    obj.field_object(key, |profile_json| {
        profile_json.field_bool("defaultFeatures", profile.default_features);
        profile_json.field_array_of_strs("features", profile.features);
    });
}

fn render_optional_feature_profile(
    obj: &mut JsonBuilder,
    profile: &DependencyOptionalFeatureProfile,
) {
    obj.field_str("name", profile.name);
    obj.field_array_of_strs("features", profile.features);
    obj.field_str("status", profile.status);
}

fn render_blocked_feature(obj: &mut JsonBuilder, feature: &DependencyBlockedFeature) {
    obj.field_str("name", feature.name);
    obj.field_array_of_strs("forbiddenCrates", feature.forbidden_crates);
    obj.field_str("action", feature.action);
}

/// Render a quarantine report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_quarantine_json(report: &QuarantineReport) -> String {
    let mut b = JsonBuilder::with_capacity(1024);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "diag quarantine");
        d.field_str("version", report.version);
        d.field_object("summary", |s| {
            s.field_raw(
                "quarantinedCount",
                &report.summary.quarantined_count.to_string(),
            );
            s.field_raw("atRiskCount", &report.summary.at_risk_count.to_string());
            s.field_raw("blockedCount", &report.summary.blocked_count.to_string());
            s.field_raw("totalSources", &report.summary.total_sources.to_string());
            s.field_raw("healthyCount", &report.summary.healthy_count.to_string());
        });
        d.field_array_of_objects(
            "quarantinedSources",
            &report.quarantined_sources,
            build_quarantine_entry,
        );
        d.field_array_of_objects(
            "atRiskSources",
            &report.at_risk_sources,
            build_quarantine_entry,
        );
        d.field_array_of_objects(
            "blockedSources",
            &report.blocked_sources,
            build_quarantine_entry,
        );
    });
    b.finish()
}

fn build_quarantine_entry(obj: &mut JsonBuilder, entry: &QuarantineEntry) {
    obj.field_str("sourceId", &entry.source_id);
    obj.field_str("advisory", entry.advisory.as_str());
    obj.field_raw("effectiveTrust", &format!("{:.4}", entry.effective_trust));
    obj.field_raw("decayFactor", &format!("{:.4}", entry.decay_factor));
    obj.field_raw("negativeRate", &format!("{:.4}", entry.negative_rate));
    obj.field_raw("negativeCount", &entry.negative_count.to_string());
    obj.field_raw("totalImports", &entry.total_imports.to_string());
    obj.field_str("message", &entry.message);
    obj.field_bool("permitsImport", entry.permits_import);
    obj.field_bool("requiresValidation", entry.requires_validation);
}

/// Render a quarantine report as human-readable text.
#[must_use]
pub fn render_quarantine_human(report: &QuarantineReport) -> String {
    let mut output = format!("ee diag quarantine (v{})\n\n", report.version);

    if !report.has_issues() {
        output.push_str("No sources require attention.\n");
        output.push_str(&format!(
            "Tracked: {} sources, {} healthy\n\n",
            report.summary.total_sources, report.summary.healthy_count
        ));
        output.push_str("Next:\n  ee diag quarantine --json\n");
        return output;
    }

    output.push_str(&format!(
        "Summary: {} quarantined, {} at risk, {} blocked\n\n",
        report.summary.quarantined_count,
        report.summary.at_risk_count,
        report.summary.blocked_count
    ));

    if !report.blocked_sources.is_empty() {
        output.push_str("Blocked Sources:\n");
        for entry in &report.blocked_sources {
            output.push_str(&format!(
                "  ✗ {} (trust {:.2})\n    {}\n",
                entry.source_id, entry.effective_trust, entry.message
            ));
        }
        output.push('\n');
    }

    if !report.quarantined_sources.is_empty() {
        output.push_str("Quarantined Sources:\n");
        for entry in &report.quarantined_sources {
            output.push_str(&format!(
                "  ⚠ {} (trust {:.2}, decay {:.2})\n    {}\n",
                entry.source_id, entry.effective_trust, entry.decay_factor, entry.message
            ));
        }
        output.push('\n');
    }

    if !report.at_risk_sources.is_empty() {
        output.push_str("At-Risk Sources:\n");
        for entry in &report.at_risk_sources {
            output.push_str(&format!(
                "  ◐ {} (trust {:.2})\n    {}\n",
                entry.source_id, entry.effective_trust, entry.message
            ));
        }
        output.push('\n');
    }

    output.push_str("Next:\n  ee diag quarantine --json\n  ee import cass --dry-run --json\n");
    output
}

/// Render a quarantine report as TOON.
#[must_use]
pub fn render_quarantine_toon(report: &QuarantineReport) -> String {
    render_toon_from_json(&render_quarantine_json(report))
}

// ============================================================================
// EE-243: Graph Diagnostic Output
// ============================================================================

/// Render a graph diagnostic report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_graph_diag_json(readiness: &crate::graph::GraphModuleReadiness) -> String {
    use crate::models::CapabilityStatus;

    let capabilities: Vec<serde_json::Value> = readiness
        .capabilities()
        .iter()
        .map(|cap| {
            serde_json::json!({
                "name": cap.name().as_str(),
                "surface": cap.surface().as_str(),
                "status": match cap.status() {
                    CapabilityStatus::Ready => "ready",
                    CapabilityStatus::Pending => "pending",
                    CapabilityStatus::Degraded => "degraded",
                    CapabilityStatus::Unimplemented => "unimplemented",
                },
                "repair": cap.repair(),
            })
        })
        .collect();

    let status = match readiness.status() {
        CapabilityStatus::Ready => "ready",
        CapabilityStatus::Pending => "pending",
        CapabilityStatus::Degraded => "degraded",
        CapabilityStatus::Unimplemented => "unimplemented",
    };

    serde_json::json!({
        "schema": RESPONSE_SCHEMA_V1,
        "success": readiness.status() == CapabilityStatus::Ready,
        "data": {
            "command": "diag graph",
            "subsystem": readiness.subsystem(),
            "contract": readiness.contract(),
            "graphEngine": readiness.graph_engine(),
            "status": status,
            "capabilityCount": capabilities.len(),
            "readyCount": readiness.capabilities().iter().filter(|c| c.status() == CapabilityStatus::Ready).count(),
            "pendingCount": readiness.capabilities().iter().filter(|c| c.status() == CapabilityStatus::Pending).count(),
            "capabilities": capabilities,
        }
    })
    .to_string()
}

/// Render a graph diagnostic report as human-readable text.
#[must_use]
pub fn render_graph_diag_human(readiness: &crate::graph::GraphModuleReadiness) -> String {
    use crate::models::CapabilityStatus;

    let mut output = String::new();
    output.push_str("Graph Module Diagnostics\n\n");

    let status_str = match readiness.status() {
        CapabilityStatus::Ready => "ready",
        CapabilityStatus::Pending => "pending",
        CapabilityStatus::Degraded => "degraded",
        CapabilityStatus::Unimplemented => "unimplemented",
    };
    output.push_str(&format!("Status: {status_str}\n"));
    output.push_str(&format!("Contract: {}\n", readiness.contract()));
    output.push_str(&format!("Engine: {}\n\n", readiness.graph_engine()));

    output.push_str("Capabilities:\n");
    for cap in readiness.capabilities() {
        let status = match cap.status() {
            CapabilityStatus::Ready => "[ready]",
            CapabilityStatus::Pending => "[pending]",
            CapabilityStatus::Degraded => "[degraded]",
            CapabilityStatus::Unimplemented => "[unimplemented]",
        };
        output.push_str(&format!(
            "  {} {} ({})\n",
            status,
            cap.name().as_str(),
            cap.surface().as_str()
        ));
        if cap.status() != CapabilityStatus::Ready {
            output.push_str(&format!("    Next: {}\n", cap.repair()));
        }
    }

    output
}

/// Render a graph diagnostic report as TOON.
#[must_use]
pub fn render_graph_diag_toon(readiness: &crate::graph::GraphModuleReadiness) -> String {
    render_toon_from_json(&render_graph_diag_json(readiness))
}

/// Render a streams diagnostic report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_streams_json(report: &crate::core::streams::StreamsReport) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.is_healthy());
    b.field_object("data", |d| {
        d.field_str("command", "diag streams");
        d.field_str("version", report.version);
        d.field_bool("stdoutIsolated", report.stdout_isolated);
        d.field_bool("stderrReceivedProbe", report.stderr_received_probe);
        d.field_str("stderrProbeMessage", &report.stderr_probe_message);
        d.field_bool("healthy", report.is_healthy());
    });
    b.finish()
}

/// Render a streams diagnostic report as human-readable text.
#[must_use]
pub fn render_streams_human(report: &crate::core::streams::StreamsReport) -> String {
    let mut output = format!("ee diag streams (v{})\n\n", report.version);

    if report.is_healthy() {
        output.push_str("Stream separation: OK\n\n");
        output.push_str("  stdout: isolated for machine data\n");
        output.push_str("  stderr: received diagnostic probe\n\n");
        output.push_str("This confirms that stdout contains only machine-readable data\n");
        output
            .push_str("and stderr receives diagnostics, as required for agent-native operation.\n");
    } else {
        output.push_str("Stream separation: FAILED\n\n");
        if !report.stdout_isolated {
            output.push_str("  ✗ stdout is not isolated\n");
        }
        if !report.stderr_received_probe {
            output.push_str("  ✗ stderr did not receive probe\n");
        }
        output.push_str("\nNext:\n  Check for stderr redirection or write failures.\n");
    }

    output
}

/// Render a streams diagnostic report as TOON.
#[must_use]
pub fn render_streams_toon(report: &crate::core::streams::StreamsReport) -> String {
    render_toon_from_json(&render_streams_json(report))
}

/// Render a check report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_check_json(report: &CheckReport) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.posture.is_usable());
    b.field_object("data", |d| {
        d.field_str("command", "check");
        d.field_str("version", report.version);
        d.field_str("posture", report.posture.as_str());
        d.field_bool("workspaceInitialized", report.workspace_initialized);
        d.field_bool("databaseReady", report.database_ready);
        d.field_bool("searchReady", report.search_ready);
        d.field_bool("runtimeReady", report.runtime_ready);
        d.field_array_of_objects(
            "suggestedActions",
            &report.suggested_actions,
            |obj, action| {
                obj.field_raw("priority", &action.priority.to_string());
                obj.field_str("command", action.command);
                obj.field_str("reason", action.reason);
            },
        );
    });
    b.finish()
}

/// Render a check report as human-readable text.
#[must_use]
pub fn render_check_human(report: &CheckReport) -> String {
    let mut output = format!("ee check\n\nposture: {}\n\n", report.posture.as_str());

    output.push_str(&format!(
        "workspace: {}\ndatabase: {}\nsearch: {}\nruntime: {}\n",
        if report.workspace_initialized {
            "initialized"
        } else {
            "not initialized"
        },
        if report.database_ready {
            "ready"
        } else {
            "not ready"
        },
        if report.search_ready {
            "ready"
        } else {
            "not ready"
        },
        if report.runtime_ready {
            "ready"
        } else {
            "not ready"
        },
    ));

    if !report.suggested_actions.is_empty() {
        output.push_str("\nNext:\n");
        for action in &report.suggested_actions {
            output.push_str(&format!("  {} — {}\n", action.command, action.reason));
        }
    }

    output
}

/// Render a check report as TOON.
#[must_use]
pub fn render_check_toon(report: &CheckReport) -> String {
    render_toon_from_json(&render_check_json(report))
}

/// Render a health report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_health_json(report: &HealthReport) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.verdict.is_healthy());
    b.field_object("data", |d| {
        d.field_str("command", "health");
        d.field_str("version", report.version);
        d.field_str("verdict", report.verdict.as_str());
        d.field_object("subsystems", |s| {
            s.field_bool("runtime", report.runtime_ok);
            s.field_bool("storage", report.storage_ok);
            s.field_bool("search", report.search_ok);
        });
        d.field_object("summary", |s| {
            s.field_raw("issueCount", &report.issue_count().to_string());
            s.field_raw("highSeverity", &report.high_severity_count().to_string());
            s.field_raw(
                "mediumSeverity",
                &report.medium_severity_count().to_string(),
            );
        });
        d.field_array_of_objects("issues", &report.issues, |obj, issue| {
            obj.field_str("subsystem", issue.subsystem);
            obj.field_str("code", issue.code);
            obj.field_str("severity", issue.severity);
            obj.field_str("message", issue.message);
        });
    });
    b.finish()
}

/// Render a health report as human-readable text.
#[must_use]
pub fn render_health_human(report: &HealthReport) -> String {
    let mut output = format!(
        "ee health (v{})\n\nVerdict: {}\n\n",
        report.version,
        report.verdict.as_str().to_uppercase()
    );

    output.push_str("Subsystems:\n");
    output.push_str(&format!(
        "  runtime: {}\n  storage: {}\n  search: {}\n",
        if report.runtime_ok { "ok" } else { "not ok" },
        if report.storage_ok { "ok" } else { "not ok" },
        if report.search_ok { "ok" } else { "not ok" },
    ));

    if !report.issues.is_empty() {
        output.push_str(&format!("\nIssues ({}):\n", report.issue_count()));
        for issue in &report.issues {
            output.push_str(&format!(
                "  [{}] {} — {}\n",
                issue.severity, issue.subsystem, issue.message
            ));
        }
    }

    output.push_str("\nNext:\n  ee health --json\n  ee doctor\n");
    output
}

/// Render a health report as TOON.
#[must_use]
pub fn render_health_toon(report: &HealthReport) -> String {
    render_toon_from_json(&render_health_json(report))
}

/// Render a memory show report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_memory_show_json(report: &MemoryShowReport) -> String {
    let mut b = JsonBuilder::with_capacity(1024);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.found && report.error.is_none());
    b.field_object("data", |d| {
        d.field_str("command", "memory show");
        d.field_str("version", report.version);
        d.field_bool("found", report.found);
        d.field_bool("is_tombstoned", report.is_tombstoned);

        if let Some(ref details) = report.memory {
            d.field_object("memory", |m| {
                render_memory_fields(m, details);
            });
        }

        if let Some(ref err) = report.error {
            d.field_str("error", err);
        }
    });
    b.finish()
}

/// Render memory fields into a JSON builder.
fn render_memory_fields(b: &mut JsonBuilder, details: &MemoryDetails) {
    let mem = &details.memory;
    b.field_str("id", &mem.id);
    b.field_str("workspace_id", &mem.workspace_id);
    b.field_str("level", &mem.level);
    b.field_str("kind", &mem.kind);
    b.field_str("content", &mem.content);
    b.field_raw("confidence", &format!("{:.4}", mem.confidence));
    b.field_raw("utility", &format!("{:.4}", mem.utility));
    b.field_raw("importance", &format!("{:.4}", mem.importance));
    if let Some(ref uri) = mem.provenance_uri {
        b.field_str("provenance_uri", uri);
    }
    b.field_str("trust_class", &mem.trust_class);
    if let Some(ref sub) = mem.trust_subclass {
        b.field_str("trust_subclass", sub);
    }
    b.field_str("created_at", &mem.created_at);
    b.field_str("updated_at", &mem.updated_at);
    if let Some(ref ts) = mem.tombstoned_at {
        b.field_str("tombstoned_at", ts);
    }
    let validity = memory_validity(&mem.valid_from, &mem.valid_to);
    field_optional_str(b, "valid_from", validity.valid_from.as_deref());
    field_optional_str(b, "valid_to", validity.valid_to.as_deref());
    b.field_str("validity_status", &validity.status);
    b.field_str("validity_window_kind", &validity.window_kind);
    b.field_array_of_objects("tags", &details.tags, |obj, tag| {
        obj.field_str("name", tag);
    });
}

/// Render a memory show report as human-readable text.
#[must_use]
pub fn render_memory_show_human(report: &MemoryShowReport) -> String {
    if let Some(ref err) = report.error {
        return format!("error: {err}\n");
    }

    if !report.found {
        return "Memory not found.\n".to_string();
    }

    let details = match &report.memory {
        Some(d) => d,
        None => return "Memory not found.\n".to_string(),
    };

    let mem = &details.memory;
    let mut output = format!("Memory: {}\n\n", mem.id);
    output.push_str(&format!("  Level: {}\n", mem.level));
    output.push_str(&format!("  Kind: {}\n", mem.kind));
    output.push_str(&format!("  Content:\n    {}\n", mem.content));
    output.push_str(&format!(
        "  Scores: confidence={:.2}, utility={:.2}, importance={:.2}\n",
        mem.confidence, mem.utility, mem.importance
    ));
    output.push_str(&format!("  Trust: {}", mem.trust_class));
    if let Some(ref sub) = mem.trust_subclass {
        output.push_str(&format!(" ({})", sub));
    }
    output.push('\n');
    if let Some(ref uri) = mem.provenance_uri {
        output.push_str(&format!("  Provenance: {}\n", uri));
    }
    output.push_str(&format!("  Created: {}\n", mem.created_at));
    output.push_str(&format!("  Updated: {}\n", mem.updated_at));
    if let Some(ref ts) = mem.tombstoned_at {
        output.push_str(&format!("  Tombstoned: {}\n", ts));
    }
    let validity = memory_validity(&mem.valid_from, &mem.valid_to);
    output.push_str(&format!(
        "  Validity: {} ({})\n",
        validity.status, validity.window_kind
    ));
    if let Some(ref ts) = validity.valid_from {
        output.push_str(&format!("    From: {ts}\n"));
    }
    if let Some(ref ts) = validity.valid_to {
        output.push_str(&format!("    To: {ts}\n"));
    }
    if !details.tags.is_empty() {
        output.push_str(&format!("  Tags: {}\n", details.tags.join(", ")));
    }
    output
}

/// Render a memory show report as TOON.
#[must_use]
pub fn render_memory_show_toon(report: &MemoryShowReport) -> String {
    render_toon_from_json(&render_memory_show_json(report))
}

/// Render a memory list report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_memory_list_json(report: &MemoryListReport) -> String {
    let mut b = JsonBuilder::with_capacity(2048);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.error.is_none());
    b.field_object("data", |d| {
        d.field_str("command", "memory list");
        d.field_str("version", report.version);
        d.field_u32("total_count", report.total_count);
        d.field_bool("truncated", report.truncated);

        d.field_object("filter", |f| {
            if let Some(ref level) = report.filter.level {
                f.field_str("level", level);
            }
            if let Some(ref tag) = report.filter.tag {
                f.field_str("tag", tag);
            }
            f.field_bool("include_tombstoned", report.filter.include_tombstoned);
        });

        d.field_array_of_objects("memories", &report.memories, |obj, m| {
            obj.field_str("id", &m.id);
            obj.field_str("level", &m.level);
            obj.field_str("kind", &m.kind);
            obj.field_str("content_preview", &m.content_preview);
            obj.field_raw("confidence", &format!("{:.4}", m.confidence));
            if let Some(ref uri) = m.provenance_uri {
                obj.field_str("provenance_uri", uri);
            }
            obj.field_bool("is_tombstoned", m.is_tombstoned);
            field_optional_str(obj, "valid_from", m.valid_from.as_deref());
            field_optional_str(obj, "valid_to", m.valid_to.as_deref());
            obj.field_str("validity_status", &m.validity_status);
            obj.field_str("validity_window_kind", &m.validity_window_kind);
            obj.field_str("created_at", &m.created_at);
        });

        if let Some(ref err) = report.error {
            d.field_str("error", err);
        }
    });
    b.finish()
}

/// Render a memory list report as human-readable text.
#[must_use]
pub fn render_memory_list_human(report: &MemoryListReport) -> String {
    if let Some(ref err) = report.error {
        return format!("error: {err}\n");
    }

    let mut output = format!("Memories ({} total", report.total_count);
    if report.truncated {
        output.push_str(", showing first batch");
    }
    output.push_str(")\n\n");

    if report.memories.is_empty() {
        output.push_str("  No memories found.\n");
        return output;
    }

    for m in &report.memories {
        output.push_str(&format!("  {} [{}] {}\n", m.id, m.level, m.kind));
        output.push_str(&format!("    {}\n", m.content_preview));
        output.push_str(&format!(
            "    confidence={:.2}, created={}, validity={} ({})\n",
            m.confidence, m.created_at, m.validity_status, m.validity_window_kind
        ));
        if m.is_tombstoned {
            output.push_str("    [TOMBSTONED]\n");
        }
        output.push('\n');
    }

    output.push_str("Next:\n  ee memory show <ID>\n");
    output
}

/// Render a memory list report as TOON.
#[must_use]
pub fn render_memory_list_toon(report: &MemoryListReport) -> String {
    render_toon_from_json(&render_memory_list_json(report))
}

/// Render a memory history report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_memory_history_json(report: &MemoryHistoryReport) -> String {
    let mut b = JsonBuilder::with_capacity(2048);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.error.is_none());
    b.field_object("data", |d| {
        d.field_str("command", "memory history");
        d.field_str("version", report.version);
        d.field_str("memory_id", &report.memory_id);
        d.field_bool("memory_exists", report.memory_exists);
        d.field_bool("is_tombstoned", report.is_tombstoned);
        d.field_u32("total_count", report.total_count);
        d.field_bool("truncated", report.truncated);

        d.field_array_of_objects("entries", &report.entries, |obj, e| {
            obj.field_str("audit_id", &e.audit_id);
            obj.field_str("timestamp", &e.timestamp);
            if let Some(ref actor) = e.actor {
                obj.field_str("actor", actor);
            }
            obj.field_str("action", &e.action);
            if let Some(ref details) = e.details {
                obj.field_raw("details", details);
            }
        });

        if let Some(ref err) = report.error {
            d.field_str("error", err);
        }
    });
    b.finish()
}

/// Render a memory history report as human-readable text.
#[must_use]
pub fn render_memory_history_human(report: &MemoryHistoryReport) -> String {
    if let Some(ref err) = report.error {
        return format!("error: {err}\n");
    }

    if !report.memory_exists {
        return format!("Memory not found: {}\n", report.memory_id);
    }

    let mut output = format!(
        "History for {} ({} entries",
        report.memory_id, report.total_count
    );
    if report.truncated {
        output.push_str(", showing first batch");
    }
    output.push_str(")\n");

    if report.is_tombstoned {
        output.push_str("  [TOMBSTONED]\n");
    }
    output.push('\n');

    if report.entries.is_empty() {
        output.push_str("  No history entries found.\n");
        return output;
    }

    for e in &report.entries {
        output.push_str(&format!("  {} [{}]\n", e.timestamp, e.action));
        if let Some(ref actor) = e.actor {
            output.push_str(&format!("    actor: {actor}\n"));
        }
        if let Some(ref details) = e.details {
            output.push_str(&format!("    details: {details}\n"));
        }
        output.push_str(&format!("    audit_id: {}\n\n", e.audit_id));
    }

    output
}

/// Render a memory history report as TOON.
#[must_use]
pub fn render_memory_history_toon(report: &MemoryHistoryReport) -> String {
    render_toon_from_json(&render_memory_history_json(report))
}

/// Render a procedural rule add report as JSON (`ee.response.v1` envelope).
#[must_use]
pub fn render_rule_add_json(report: &RuleAddReport) -> String {
    ResponseEnvelope::success()
        .data_raw(&report.data_json())
        .finish()
}

/// Render a procedural rule add report as human-readable text.
#[must_use]
pub fn render_rule_add_human(report: &RuleAddReport) -> String {
    report.human_summary()
}

/// Render a procedural rule add report as TOON.
#[must_use]
pub fn render_rule_add_toon(report: &RuleAddReport) -> String {
    render_toon_from_json(&render_rule_add_json(report))
}

/// Render a procedural rule list report as JSON (`ee.response.v1` envelope).
#[must_use]
pub fn render_rule_list_json(report: &RuleListReport) -> String {
    ResponseEnvelope::success()
        .data_raw(&report.data_json())
        .finish()
}

/// Render a procedural rule list report as human-readable text.
#[must_use]
pub fn render_rule_list_human(report: &RuleListReport) -> String {
    report.human_summary()
}

/// Render a procedural rule list report as TOON.
#[must_use]
pub fn render_rule_list_toon(report: &RuleListReport) -> String {
    render_toon_from_json(&render_rule_list_json(report))
}

/// Render a procedural rule show report as JSON (`ee.response.v1` envelope).
#[must_use]
pub fn render_rule_show_json(report: &RuleShowReport) -> String {
    ResponseEnvelope::success()
        .data_raw(&report.data_json())
        .finish()
}

/// Render a procedural rule show report as human-readable text.
#[must_use]
pub fn render_rule_show_human(report: &RuleShowReport) -> String {
    report.human_summary()
}

/// Render a procedural rule show report as TOON.
#[must_use]
pub fn render_rule_show_toon(report: &RuleShowReport) -> String {
    render_toon_from_json(&render_rule_show_json(report))
}

/// Render a procedural rule protection report as JSON (`ee.response.v1` envelope).
#[must_use]
pub fn render_rule_protect_json(report: &RuleProtectReport) -> String {
    ResponseEnvelope::success()
        .data_raw(&report.data_json())
        .finish()
}

/// Render a procedural rule protection report as human-readable text.
#[must_use]
pub fn render_rule_protect_human(report: &RuleProtectReport) -> String {
    report.human_summary()
}

/// Render a procedural rule protection report as TOON.
#[must_use]
pub fn render_rule_protect_toon(report: &RuleProtectReport) -> String {
    render_toon_from_json(&render_rule_protect_json(report))
}

/// Render a feedback quarantine list report as JSON (`ee.response.v1` envelope).
#[must_use]
pub fn render_outcome_quarantine_list_json(report: &OutcomeQuarantineListReport) -> String {
    ResponseEnvelope::success()
        .data_raw(&report.data_json())
        .finish()
}

/// Render a feedback quarantine list report as human-readable text.
#[must_use]
pub fn render_outcome_quarantine_list_human(report: &OutcomeQuarantineListReport) -> String {
    report.human_summary()
}

/// Render a feedback quarantine list report as TOON.
#[must_use]
pub fn render_outcome_quarantine_list_toon(report: &OutcomeQuarantineListReport) -> String {
    render_toon_from_json(&render_outcome_quarantine_list_json(report))
}

/// Render a feedback quarantine review report as JSON (`ee.response.v1` envelope).
#[must_use]
pub fn render_outcome_quarantine_review_json(report: &OutcomeQuarantineReviewReport) -> String {
    ResponseEnvelope::success()
        .data_raw(&report.data_json())
        .finish()
}

/// Render a feedback quarantine review report as human-readable text.
#[must_use]
pub fn render_outcome_quarantine_review_human(report: &OutcomeQuarantineReviewReport) -> String {
    report.human_summary()
}

/// Render a feedback quarantine review report as TOON.
#[must_use]
pub fn render_outcome_quarantine_review_toon(report: &OutcomeQuarantineReviewReport) -> String {
    render_toon_from_json(&render_outcome_quarantine_review_json(report))
}

/// Render a curation candidate list report as JSON (`ee.response.v1` envelope).
#[must_use]
pub fn render_curate_candidates_json(report: &CurateCandidatesReport) -> String {
    ResponseEnvelope::success()
        .data_raw(&report.data_json())
        .finish()
}

/// Render a curation candidate list report as human-readable text.
#[must_use]
pub fn render_curate_candidates_human(report: &CurateCandidatesReport) -> String {
    report.human_summary()
}

/// Render a curation candidate list report as TOON.
#[must_use]
pub fn render_curate_candidates_toon(report: &CurateCandidatesReport) -> String {
    render_toon_from_json(&render_curate_candidates_json(report))
}

/// Render a curation validation report as JSON (`ee.response.v1` envelope).
#[must_use]
pub fn render_curate_validate_json(report: &CurateValidateReport) -> String {
    ResponseEnvelope::success()
        .data_raw(&report.data_json())
        .finish()
}

/// Render a curation validation report as human-readable text.
#[must_use]
pub fn render_curate_validate_human(report: &CurateValidateReport) -> String {
    report.human_summary()
}

/// Render a curation validation report as TOON.
#[must_use]
pub fn render_curate_validate_toon(report: &CurateValidateReport) -> String {
    render_toon_from_json(&render_curate_validate_json(report))
}

/// Render a curation apply report as JSON (`ee.response.v1` envelope).
#[must_use]
pub fn render_curate_apply_json(report: &CurateApplyReport) -> String {
    ResponseEnvelope::success()
        .data_raw(&report.data_json())
        .finish()
}

/// Render a curation apply report as human-readable text.
#[must_use]
pub fn render_curate_apply_human(report: &CurateApplyReport) -> String {
    report.human_summary()
}

/// Render a curation apply report as TOON.
#[must_use]
pub fn render_curate_apply_toon(report: &CurateApplyReport) -> String {
    render_toon_from_json(&render_curate_apply_json(report))
}

/// Render a curation review lifecycle report as JSON (`ee.response.v1` envelope).
#[must_use]
pub fn render_curate_review_json(report: &CurateReviewReport) -> String {
    ResponseEnvelope::success()
        .data_raw(&report.data_json())
        .finish()
}

/// Render a curation review lifecycle report as human-readable text.
#[must_use]
pub fn render_curate_review_human(report: &CurateReviewReport) -> String {
    report.human_summary()
}

/// Render a curation review lifecycle report as TOON.
#[must_use]
pub fn render_curate_review_toon(report: &CurateReviewReport) -> String {
    render_toon_from_json(&render_curate_review_json(report))
}

/// Render a curation TTL disposition report as JSON (`ee.response.v1` envelope).
#[must_use]
pub fn render_curate_disposition_json(report: &CurateDispositionReport) -> String {
    ResponseEnvelope::success()
        .data_raw(&report.data_json())
        .finish()
}

/// Render a curation TTL disposition report as human-readable text.
#[must_use]
pub fn render_curate_disposition_human(report: &CurateDispositionReport) -> String {
    report.human_summary()
}

/// Render a curation TTL disposition report as TOON.
#[must_use]
pub fn render_curate_disposition_toon(report: &CurateDispositionReport) -> String {
    render_toon_from_json(&render_curate_disposition_json(report))
}

/// Render binary version and build provenance as JSON (`ee.response.v1` envelope).
#[must_use]
pub fn render_version_json(report: &VersionReport) -> String {
    let mut b = JsonBuilder::with_capacity(2048);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "version");
        d.field_str("schema", VERSION_PROVENANCE_SCHEMA_V1);
        d.field_str("package", report.build.package);
        d.field_str("version", report.build.version);
        d.field_str("releaseChannel", report.build.release_channel);
        d.field_object("source", |source| {
            field_optional_str(source, "gitCommit", report.build.git_commit);
            field_optional_str(source, "gitTag", report.build.git_tag);
            field_optional_bool(source, "gitDirty", report.build.git_dirty);
            source.field_str("state", source_state(report));
        });
        d.field_object("build", |build| {
            build.field_str("profile", report.build.build_profile);
            build.field_str("targetTriple", report.build.target_triple);
            build.field_str("targetArch", report.build.target_arch);
            build.field_str("targetOs", report.build.target_os);
            build.field_str("timestampPolicy", report.build.build_timestamp_policy);
            build.field_raw("timestamp", "null");
        });
        d.field_array_of_objects("features", &report.features, |obj, feature| {
            obj.field_str("name", feature.name);
            obj.field_bool("enabled", feature.enabled);
        });
        d.field_array_of_objects("schemas", &report.schemas, |obj, schema| {
            obj.field_str("name", schema.name);
            obj.field_str("schema", schema.schema);
        });
        d.field_object("database", |db| {
            db.field_object("supportedMigrationRange", |range| {
                range.field_u32("min", report.build.min_db_migration);
                range.field_u32("max", report.build.max_db_migration);
            });
            db.field_str("compatibility", "unknown_without_workspace");
        });
        d.field_object("provenance", |provenance| {
            provenance.field_bool("available", report.provenance_available());
            provenance.field_array_of_objects("degraded", &report.degradations, |obj, deg| {
                obj.field_str("code", deg.code);
                obj.field_str("severity", deg.severity);
                obj.field_str("message", deg.message);
                obj.field_str("repair", deg.repair);
            });
        });
    });
    b.finish()
}

/// Render binary version and build provenance as TOON.
#[must_use]
pub fn render_version_toon(report: &VersionReport) -> String {
    render_toon_from_json(&render_version_json(report))
}

/// Render `ee install check` as JSON (`ee.response.v1` envelope).
#[must_use]
pub fn render_install_check_json(report: &InstallCheckReport) -> String {
    let raw = serde_json::to_string(report).unwrap_or_else(|_| "{}".to_owned());
    ResponseEnvelope::success().data_raw(&raw).finish()
}

#[must_use]
pub fn render_install_check_human(report: &InstallCheckReport) -> String {
    let mut output = format!(
        "ee install check ({})\n\nStatus: {}\nTarget: {}\nInstall path: {}\n",
        report.version,
        report.status().as_str(),
        report.target.target_triple,
        report.target.install_path
    );
    output.push_str(&format!("PATH: {}\n", report.path.status.as_str()));
    for finding in &report.findings {
        output.push_str(&format!(
            "- {}: {} Next: {}\n",
            finding.code, finding.message, finding.next_action
        ));
    }
    output
}

#[must_use]
pub fn render_install_check_toon(report: &InstallCheckReport) -> String {
    render_toon_from_json(&render_install_check_json(report))
}

/// Render install/update dry-run plans as JSON (`ee.response.v1` envelope).
#[must_use]
pub fn render_install_plan_json(report: &InstallPlanReport) -> String {
    let raw = serde_json::to_string(report).unwrap_or_else(|_| "{}".to_owned());
    ResponseEnvelope::success().data_raw(&raw).finish()
}

#[must_use]
pub fn render_install_plan_human(report: &InstallPlanReport) -> String {
    let mut output = format!(
        "ee {} plan ({})\n\nStatus: {}\nTarget: {}\nInstall path: {}\n",
        report.operation.as_str(),
        report.version,
        report.status.as_str(),
        report.target.target_triple,
        report.target.install_path
    );
    if let Some(version) = &report.target_version {
        output.push_str(&format!("Target version: {version}\n"));
    }
    if let Some(artifact) = &report.artifact {
        output.push_str(&format!("Artifact: {}\n", artifact.file_name));
    }
    for finding in &report.findings {
        output.push_str(&format!(
            "- {}: {} Next: {}\n",
            finding.code, finding.message, finding.next_action
        ));
    }
    output
}

#[must_use]
pub fn render_install_plan_toon(report: &InstallPlanReport) -> String {
    render_toon_from_json(&render_install_plan_json(report))
}

fn field_optional_str(builder: &mut JsonBuilder, key: &str, value: Option<&str>) {
    match value {
        Some(value) => builder.field_str(key, value),
        None => builder.field_raw(key, "null"),
    };
}

fn field_optional_bool(builder: &mut JsonBuilder, key: &str, value: Option<bool>) {
    match value {
        Some(value) => builder.field_bool(key, value),
        None => builder.field_raw(key, "null"),
    };
}

fn source_state(report: &VersionReport) -> &'static str {
    match report.build.git_dirty {
        Some(true) => "dirty",
        Some(false) => "clean",
        None if report.build.git_commit.is_some() || report.build.git_tag.is_some() => "unknown",
        None => "unavailable",
    }
}

/// Render a capabilities report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_capabilities_json(report: &CapabilitiesReport) -> String {
    let mut b = JsonBuilder::with_capacity(1024);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "capabilities");
        d.field_str("version", report.version);
        d.field_array_of_objects("subsystems", &report.subsystems, |obj, sub| {
            obj.field_str("name", sub.name);
            obj.field_str("status", sub.status.as_str());
            obj.field_str("description", sub.description);
        });
        d.field_array_of_objects("features", &report.features, |obj, feat| {
            obj.field_str("name", feat.name);
            obj.field_bool("enabled", feat.enabled);
            obj.field_str("description", feat.description);
        });
        d.field_array_of_objects("commands", &report.commands, |obj, cmd| {
            obj.field_str("name", cmd.name);
            obj.field_bool("available", cmd.available);
            obj.field_str("description", cmd.description);
        });
        write_capabilities_output_metadata(d, report, true);
        d.field_object("summary", |s| {
            s.field_raw(
                "readySubsystems",
                &report.ready_subsystem_count().to_string(),
            );
            s.field_raw("totalSubsystems", &report.subsystems.len().to_string());
            s.field_raw(
                "enabledFeatures",
                &report.enabled_feature_count().to_string(),
            );
            s.field_raw("totalFeatures", &report.features.len().to_string());
            s.field_raw(
                "availableCommands",
                &report.available_command_count().to_string(),
            );
            s.field_raw("totalCommands", &report.commands.len().to_string());
        });
    });
    b.finish()
}

fn write_capabilities_output_metadata(
    builder: &mut JsonBuilder,
    report: &CapabilitiesReport,
    include_size_diagnostics: bool,
) {
    builder.field_object("output", |output| {
        output.field_array_of_objects("formats", &report.output_formats, |obj, format| {
            obj.field_str("name", format.name);
            obj.field_bool("available", format.available);
            obj.field_bool("machineReadable", format.machine_readable);
            obj.field_str("description", format.description);
        });
        output.field_object("toon", |toon| {
            toon.field_bool("available", report.toon.available);
            toon.field_str("canonicalSourceFormat", report.toon.canonical_source_format);
            toon.field_object("dependency", |dependency| {
                dependency.field_str("crate", report.toon.dependency.crate_name);
                dependency.field_str("package", report.toon.dependency.package);
                dependency.field_str("version", report.toon.dependency.version);
                dependency.field_str("sourceKind", report.toon.dependency.source_kind);
                dependency.field_str("path", report.toon.dependency.path);
                dependency.field_bool("defaultFeatures", report.toon.dependency.default_features);
            });
            toon.field_array_of_strs(
                "supportedOutputProfiles",
                &report.toon.supported_output_profiles,
            );
            toon.field_str("defaultFormatEnv", report.toon.default_format_env);
            toon.field_array_of_strs("errorCodes", &report.toon.error_codes);
        });
        if include_size_diagnostics {
            let diagnostics = compute_representative_size_diagnostics();
            output.field_array_of_objects("sizeDiagnostics", &diagnostics, |obj, item| {
                let (command, diagnostic) = item;
                obj.field_str("command", command);
                obj.field_raw("diagnostic", &diagnostic.to_json());
            });
        }
    });
}

/// Render a capabilities report as human-readable text.
#[must_use]
pub fn render_capabilities_human(report: &CapabilitiesReport) -> String {
    let mut output = format!("ee capabilities (v{})\n\n", report.version);

    output.push_str("Subsystems:\n");
    for sub in &report.subsystems {
        let icon = match sub.status {
            crate::models::CapabilityStatus::Ready => "✓",
            crate::models::CapabilityStatus::Pending => "◐",
            crate::models::CapabilityStatus::Degraded => "⚠",
            crate::models::CapabilityStatus::Unimplemented => "○",
        };
        output.push_str(&format!("  {} {} — {}\n", icon, sub.name, sub.description));
    }

    output.push_str("\nFeatures:\n");
    for feat in &report.features {
        let icon = if feat.enabled { "✓" } else { "○" };
        output.push_str(&format!(
            "  {} {} — {}\n",
            icon, feat.name, feat.description
        ));
    }

    output.push_str("\nCommands:\n");
    for cmd in &report.commands {
        let icon = if cmd.available { "✓" } else { "○" };
        output.push_str(&format!("  {} {} — {}\n", icon, cmd.name, cmd.description));
    }

    output.push_str(&format!(
        "\nSummary: {}/{} subsystems ready, {}/{} features enabled, {}/{} commands available\n",
        report.ready_subsystem_count(),
        report.subsystems.len(),
        report.enabled_feature_count(),
        report.features.len(),
        report.available_command_count(),
        report.commands.len()
    ));

    output.push_str("\nNext:\n  ee capabilities --json\n");
    output
}

/// Render a capabilities report as TOON.
#[must_use]
pub fn render_capabilities_toon(report: &CapabilitiesReport) -> String {
    render_toon_from_json(&render_capabilities_json(report))
}

/// Render evaluation run result as JSON (ee.response.v1 envelope).
///
/// This stub version is used when no report is available.
#[must_use]
pub fn render_eval_run_json(scenario_id: Option<&str>, include_science_metrics: bool) -> String {
    render_eval_report_json(
        &build_eval_run_stub_report(include_science_metrics),
        scenario_id,
    )
}

fn build_eval_run_stub_report(include_science_metrics: bool) -> EvaluationReport {
    let mut report = EvaluationReport::new();
    if include_science_metrics {
        report.attach_science_metrics();
    }
    report
}

fn render_eval_science_metrics_json(
    obj: &mut JsonBuilder,
    metrics: &crate::eval::EvaluationScienceMetricsReport,
) {
    obj.field_str("schema", metrics.schema);
    obj.field_str("status", metrics.status.as_str());
    obj.field_bool("available", metrics.available);
    field_optional_str(obj, "degradationCode", metrics.degradation_code);
    obj.field_raw(
        "scenariosEvaluated",
        &metrics.scenarios_evaluated.to_string(),
    );
    obj.field_str("positiveLabel", metrics.positive_label);
    match metrics.precision {
        Some(value) => obj.field_raw("precision", &format!("{value:.6}")),
        None => obj.field_raw("precision", "null"),
    };
    match metrics.recall {
        Some(value) => obj.field_raw("recall", &format!("{value:.6}")),
        None => obj.field_raw("recall", "null"),
    };
    match metrics.f1_score {
        Some(value) => obj.field_raw("f1Score", &format!("{value:.6}")),
        None => obj.field_raw("f1Score", "null"),
    };
}

/// Render evaluation report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_eval_report_json(report: &EvaluationReport, scenario_id: Option<&str>) -> String {
    let mut b = JsonBuilder::with_capacity(1024);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.status.is_success());
    b.field_object("data", |d| {
        d.field_str("command", "eval run");
        if let Some(id) = scenario_id {
            d.field_str("scenarioId", id);
        }
        d.field_str("status", report.status.as_str());
        d.field_raw("scenariosRun", &report.scenarios_run.to_string());
        d.field_raw("scenariosPassed", &report.scenarios_passed.to_string());
        d.field_raw("scenariosFailed", &report.scenarios_failed.to_string());
        d.field_raw("elapsedMs", &format!("{:.2}", report.elapsed_ms));
        if let Some(ref dir) = report.fixture_dir {
            d.field_str("fixtureDir", dir);
        }
        if report.status == EvaluationStatus::NoScenarios {
            d.field_str(
                "message",
                "No evaluation scenarios configured. Add fixtures to tests/fixtures/eval/.",
            );
        }
        if let Some(ref metrics) = report.science_metrics {
            d.field_object("scienceMetrics", |science| {
                render_eval_science_metrics_json(science, metrics);
            });
        }
        d.field_array_of_objects("results", &report.results, render_scenario_result_json);
    });
    b.finish()
}

fn render_scenario_result_json(obj: &mut JsonBuilder, result: &ScenarioValidationResult) {
    obj.field_str("scenarioId", &result.scenario_id);
    obj.field_bool("passed", result.passed);
    obj.field_raw("stepsPassed", &result.steps_passed.to_string());
    obj.field_raw("stepsTotal", &result.steps_total.to_string());
    obj.field_array_of_objects("failures", &result.failures, |f, failure| {
        f.field_raw("step", &failure.step.to_string());
        f.field_str("kind", failure.kind.as_str());
        f.field_str("message", &failure.message);
    });
}

/// Render evaluation run result as human-readable text.
///
/// This stub version is used when no report is available.
#[must_use]
pub fn render_eval_run_human(scenario_id: Option<&str>, include_science_metrics: bool) -> String {
    render_eval_report_human(
        &build_eval_run_stub_report(include_science_metrics),
        scenario_id,
    )
}

/// Render evaluation report as human-readable text.
#[must_use]
pub fn render_eval_report_human(report: &EvaluationReport, scenario_id: Option<&str>) -> String {
    let mut output = String::from("ee eval run\n\n");

    if let Some(id) = scenario_id {
        output.push_str(&format!("Scenario: {id}\n\n"));
    }

    let status_display = match report.status {
        EvaluationStatus::NoScenarios => "no scenarios available",
        EvaluationStatus::AllPassed => "all passed",
        EvaluationStatus::SomeFailed => "some failed",
        EvaluationStatus::AllFailed => "all failed",
    };
    output.push_str(&format!("Status: {status_display}\n"));
    output.push_str(&format!(
        "Results: {} run, {} passed, {} failed\n",
        report.scenarios_run, report.scenarios_passed, report.scenarios_failed
    ));
    output.push_str(&format!("Elapsed: {:.1}ms\n", report.elapsed_ms));

    if let Some(ref dir) = report.fixture_dir {
        output.push_str(&format!("Fixtures: {dir}\n"));
    }

    if report.status == EvaluationStatus::NoScenarios {
        output.push_str("\nNo evaluation scenarios configured.\n");
        output.push_str("Add fixtures to tests/fixtures/eval/ to define scenarios.\n");
    } else {
        output.push('\n');
        for result in &report.results {
            let icon = if result.passed { "[PASS]" } else { "[FAIL]" };
            output.push_str(&format!(
                "{icon} {}: {}/{} steps\n",
                result.scenario_id, result.steps_passed, result.steps_total
            ));
            for failure in &result.failures {
                output.push_str(&format!(
                    "  - Step {}: {} - {}\n",
                    failure.step,
                    failure.kind.as_str(),
                    failure.message
                ));
            }
        }
    }

    if let Some(metrics) = report.science_metrics.as_ref() {
        output.push_str("\nScience metrics:\n");
        output.push_str(&format!("  Status: {}\n", metrics.status.as_str()));
        output.push_str(&format!("  Available: {}\n", metrics.available));
        output.push_str(&format!(
            "  Scenarios evaluated: {}\n",
            metrics.scenarios_evaluated
        ));
        if let Some(code) = metrics.degradation_code {
            output.push_str(&format!("  Degradation: {code}\n"));
        }
        output.push_str(&format!("  Positive label: {}\n", metrics.positive_label));
        match metrics.precision {
            Some(value) => output.push_str(&format!("  Precision: {value:.3}\n")),
            None => output.push_str("  Precision: n/a\n"),
        }
        match metrics.recall {
            Some(value) => output.push_str(&format!("  Recall: {value:.3}\n")),
            None => output.push_str("  Recall: n/a\n"),
        }
        match metrics.f1_score {
            Some(value) => output.push_str(&format!("  F1: {value:.3}\n")),
            None => output.push_str("  F1: n/a\n"),
        }
    }

    output
}

/// Render evaluation run result as TOON.
///
/// This stub version is used when no report is available.
#[must_use]
pub fn render_eval_run_toon(scenario_id: Option<&str>, include_science_metrics: bool) -> String {
    render_eval_report_toon(
        &build_eval_run_stub_report(include_science_metrics),
        scenario_id,
    )
}

/// Render evaluation report as TOON.
#[must_use]
pub fn render_eval_report_toon(report: &EvaluationReport, scenario_id: Option<&str>) -> String {
    render_toon_from_json(&render_eval_report_json(report, scenario_id))
}

/// Render evaluation scenario list as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_eval_list_json() -> String {
    let mut b = JsonBuilder::with_capacity(256);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "eval list");
        d.field_raw("scenarios", "[]");
        d.field_str(
            "message",
            "No evaluation scenarios configured. Add fixtures to tests/fixtures/eval/.",
        );
    });
    b.finish()
}

/// Render evaluation scenario list as human-readable text.
#[must_use]
pub fn render_eval_list_human() -> String {
    let mut output = String::from("ee eval list\n\n");
    output.push_str("No evaluation scenarios configured.\n");
    output.push_str("Add fixtures to tests/fixtures/eval/ to define scenarios.\n");
    output
}

/// Render evaluation scenario list as TOON.
#[must_use]
pub fn render_eval_list_toon() -> String {
    render_toon_from_json(&render_eval_list_json())
}

/// Public schema entry for the schema registry.
#[derive(Clone, Debug)]
pub struct SchemaEntry {
    pub id: &'static str,
    pub version: &'static str,
    pub description: &'static str,
    pub category: &'static str,
}

/// All public schemas exposed by ee.
pub const fn public_schemas() -> &'static [SchemaEntry] {
    &[
        SchemaEntry {
            id: "ee.response.v1",
            version: "1",
            description: "Success response envelope for all ee commands",
            category: "envelope",
        },
        SchemaEntry {
            id: "ee.error.v1",
            version: "1",
            description: "Error response envelope with code, message, and repair",
            category: "envelope",
        },
        SchemaEntry {
            id: MCP_MANIFEST_SCHEMA_V1,
            version: "1",
            description: "MCP adapter manifest generated from ee's public command and schema registries",
            category: "adapter",
        },
        SchemaEntry {
            id: "ee.certificate.v1",
            version: "1",
            description: "Certificate schemas for pack, curation, tail-risk, privacy-budget, and lifecycle",
            category: "domain",
        },
        SchemaEntry {
            id: "ee.executable_id_schemas.v1",
            version: "1",
            description: "Executable claim/evidence/policy/trace/demo ID schemas",
            category: "id",
        },
        SchemaEntry {
            id: "ee.procedure.schemas.v1",
            version: "1",
            description: "Procedure, verification, export, and render-only skill capsule schemas",
            category: "domain",
        },
        SchemaEntry {
            id: "ee.economy.schemas.v1",
            version: "1",
            description: "Utility, attention-cost, reserve, debt, recommendation, report, and simulation schemas",
            category: "domain",
        },
        SchemaEntry {
            id: "ee.learning.schemas.v1",
            version: "1",
            description: "Learning question, uncertainty, experiment, observation, and outcome schemas",
            category: "domain",
        },
        SchemaEntry {
            id: "ee.rule.add.v1",
            version: "1",
            description: "Procedural rule creation response data",
            category: "domain",
        },
        SchemaEntry {
            id: "ee.rule.list.v1",
            version: "1",
            description: "Procedural rule list response data",
            category: "domain",
        },
        SchemaEntry {
            id: "ee.rule.show.v1",
            version: "1",
            description: "Procedural rule detail response data",
            category: "domain",
        },
        SchemaEntry {
            id: "ee.causal.schemas.v1",
            version: "1",
            description: "Causal exposure, decision trace, uplift, confounder, and promotion-plan schemas",
            category: "domain",
        },
    ]
}

/// Render the schema list as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_schema_list_json() -> String {
    let schemas = public_schemas();
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "schema list");
        d.field_array_of_objects("schemas", schemas, |obj, entry| {
            obj.field_str("id", entry.id);
            obj.field_str("version", entry.version);
            obj.field_str("description", entry.description);
            obj.field_str("category", entry.category);
        });
    });
    b.finish()
}

/// Render the schema list as human-readable text.
#[must_use]
pub fn render_schema_list_human() -> String {
    let schemas = public_schemas();
    let mut output = String::from("ee schema list\n\nAvailable schemas:\n\n");
    for entry in schemas {
        output.push_str(&format!("  {} (v{})\n", entry.id, entry.version));
        output.push_str(&format!("    {}\n\n", entry.description));
    }
    output.push_str(
        "Use `ee schema export <SCHEMA_ID>` to export a schema's JSON Schema definition.\n",
    );
    output
}

/// Render the schema list as TOON.
#[must_use]
pub fn render_schema_list_toon() -> String {
    render_toon_from_json(&render_schema_list_json())
}

/// Render a schema export as JSON (full JSON Schema definition).
#[must_use]
pub fn render_schema_export_json(schema_id: Option<&str>) -> String {
    match schema_id {
        Some(id) => render_single_schema_export(id),
        None => render_all_schemas_export(),
    }
}

fn render_single_schema_export(schema_id: &str) -> String {
    match schema_id {
        "ee.response.v1" => response_schema_definition(),
        "ee.error.v1" => error_schema_definition(),
        MCP_MANIFEST_SCHEMA_V1 => mcp_manifest_schema_definition(),
        "ee.certificate.v1" => certificate_schema_definition(),
        "ee.executable_id_schemas.v1" => crate::models::executable_id_schema_catalog_json(),
        "ee.procedure.schemas.v1" => crate::models::procedure_schema_catalog_json(),
        "ee.economy.schemas.v1" => crate::models::economy_schema_catalog_json(),
        "ee.learning.schemas.v1" => crate::models::learning_schema_catalog_json(),
        "ee.causal.schemas.v1" => crate::models::causal_schema_catalog_json(),
        _ => {
            let mut b = JsonBuilder::with_capacity(256);
            b.field_str("schema", ERROR_SCHEMA_V1);
            b.field_object("error", |e| {
                e.field_str("code", "schema_not_found");
                e.field_str("message", &format!("Schema '{}' not found", schema_id));
                e.field_str("repair", "ee schema list");
            });
            b.finish()
        }
    }
}

fn render_all_schemas_export() -> String {
    let mut b = JsonBuilder::with_capacity(3072);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "schema export");
        d.field_raw(
            "schemas",
            &format!(
                "[{},{},{},{},{},{},{},{},{}]",
                response_schema_definition(),
                error_schema_definition(),
                mcp_manifest_schema_definition(),
                certificate_schema_definition(),
                crate::models::executable_id_schema_catalog_json(),
                crate::models::procedure_schema_catalog_json(),
                crate::models::economy_schema_catalog_json(),
                crate::models::learning_schema_catalog_json(),
                crate::models::causal_schema_catalog_json()
            ),
        );
    });
    b.finish()
}

fn response_schema_definition() -> String {
    r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","$id":"ee.response.v1","type":"object","required":["schema","success","data"],"properties":{"schema":{"const":"ee.response.v1"},"success":{"type":"boolean"},"data":{"type":"object"},"degraded":{"type":"array","items":{"type":"object","required":["code","severity","message","repair"],"properties":{"code":{"type":"string"},"severity":{"type":"string","enum":["low","medium","high"]},"message":{"type":"string"},"repair":{"type":"string"}}}}}}"#.to_string()
}

fn error_schema_definition() -> String {
    r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","$id":"ee.error.v1","type":"object","required":["schema","error"],"properties":{"schema":{"const":"ee.error.v1"},"error":{"type":"object","required":["code","message"],"properties":{"code":{"type":"string"},"message":{"type":"string"},"repair":{"type":"string"}}}}}"#.to_string()
}

fn mcp_manifest_schema_definition() -> String {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": MCP_MANIFEST_SCHEMA_V1,
        "type": "object",
        "required": [
            "command",
            "schema",
            "version",
            "protocolVersion",
            "adapter",
            "capabilities",
            "tools",
            "schemas",
            "degraded"
        ],
        "properties": {
            "command": { "const": "mcp manifest" },
            "schema": { "const": MCP_MANIFEST_SCHEMA_V1 },
            "version": { "type": "string" },
            "protocolVersion": { "type": "string" },
            "adapter": { "type": "object" },
            "capabilities": { "type": "object" },
            "tools": { "type": "array", "items": { "type": "object" } },
            "schemas": { "type": "array", "items": { "type": "object" } },
            "degraded": { "type": "array", "items": { "type": "object" } }
        }
    })
    .to_string()
}

fn certificate_schema_definition() -> String {
    r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","$id":"ee.certificate.v1","type":"object","required":["kind","status"],"properties":{"kind":{"type":"string","enum":["pack","curation","tail_risk","privacy_budget","lifecycle"]},"status":{"type":"string","enum":["pending","active","revoked","expired"]}}}"#.to_string()
}

/// Render a schema export as human-readable text.
#[must_use]
pub fn render_schema_export_human(schema_id: Option<&str>) -> String {
    let json = render_schema_export_json(schema_id);
    if json.contains("\"error\"") {
        String::from("error: Schema not found\n\nRun `ee schema list` to see available schemas.\n")
    } else {
        format!("ee schema export\n\n{}\n", json)
    }
}

/// Render a schema export as TOON.
#[must_use]
pub fn render_schema_export_toon(schema_id: Option<&str>) -> String {
    render_toon_from_json(&render_schema_export_json(schema_id))
}

struct McpManifestDegradation {
    code: &'static str,
    severity: &'static str,
    message: &'static str,
    repair: &'static str,
}

const MCP_FEATURE_DISABLED_DEGRADATION: McpManifestDegradation = McpManifestDegradation {
    code: "mcp_feature_disabled",
    severity: "low",
    message: "The MCP stdio adapter feature is not enabled in this build; the command/schema manifest is still available.",
    repair: "Build or install ee with the mcp feature enabled.",
};

/// Render the MCP adapter manifest as JSON.
#[must_use]
pub fn render_mcp_manifest_json() -> String {
    let mut b = JsonBuilder::with_capacity(8192);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "mcp manifest");
        d.field_str("schema", MCP_MANIFEST_SCHEMA_V1);
        d.field_str("version", env!("CARGO_PKG_VERSION"));
        d.field_str("protocolVersion", MCP_PROTOCOL_VERSION);
        d.field_object("adapter", |adapter| {
            adapter.field_str("name", "ee");
            adapter.field_str("transport", "stdio");
            adapter.field_str("feature", "mcp");
            adapter.field_bool("featureEnabled", cfg!(feature = "mcp"));
            adapter.field_str("runtime", "asupersync");
            adapter.field_str("businessLogic", "cli_core_services");
        });
        d.field_object("capabilities", |capabilities| {
            capabilities.field_bool("tools", true);
            capabilities.field_bool("resources", cfg!(feature = "mcp"));
            capabilities.field_bool("prompts", cfg!(feature = "mcp"));
            capabilities.field_bool("experimental", false);
        });
        d.field_object("registry", |registry| {
            registry.field_str("commandSource", "COMMAND_MANIFEST");
            registry.field_str("schemaSource", "public_schemas");
            registry.field_raw("commandCount", &COMMAND_MANIFEST.len().to_string());
            registry.field_raw("schemaCount", &public_schemas().len().to_string());
        });
        d.field_array_of_objects("tools", COMMAND_MANIFEST, render_mcp_tool_manifest_entry);
        d.field_array_of_objects("schemas", public_schemas(), render_public_schema_entry);
        if cfg!(feature = "mcp") {
            d.field_raw("degraded", "[]");
        } else {
            d.field_array_of_objects(
                "degraded",
                &[MCP_FEATURE_DISABLED_DEGRADATION],
                render_mcp_manifest_degradation,
            );
        }
    });
    b.finish()
}

fn render_mcp_tool_manifest_entry(obj: &mut JsonBuilder, cmd: &CommandEntry) {
    let tool_name = format!("ee_{}", cmd.name.replace('-', "_"));
    obj.field_str("name", &tool_name);
    obj.field_str("command", cmd.name);
    obj.field_str("description", cmd.description);
    obj.field_bool("available", cmd.available);
    obj.field_str("source", "public_command_manifest");
    obj.field_str("responseEnvelope", RESPONSE_SCHEMA_V1);
    obj.field_str("errorEnvelope", ERROR_SCHEMA_V1);
    obj.field_array_of_objects("subcommands", cmd.subcommands, |sub, sc| {
        sub.field_str("name", sc.name);
        sub.field_str("description", sc.description);
    });
    obj.field_array_of_objects("args", cmd.args, |arg, a| {
        arg.field_str("name", a.name);
        arg.field_str("description", a.description);
        arg.field_bool("required", a.required);
        if let Some(default) = a.default {
            arg.field_str("default", default);
        }
    });
    obj.field_object("inputSchema", |schema| {
        schema.field_str("type", "object");
        schema.field_object("properties", |properties| {
            properties.field_object("workspace", |workspace| {
                workspace.field_str("type", "string");
                workspace.field_str(
                    "description",
                    "Workspace path, equivalent to the CLI --workspace option.",
                );
            });
            properties.field_object("args", |args| {
                args.field_str("type", "array");
                args.field_str("description", "Command-specific CLI arguments in order.");
                args.field_object("items", |items| {
                    items.field_str("type", "string");
                });
            });
            properties.field_object("json", |json| {
                json.field_str("type", "boolean");
                json.field_str("description", "Request the stable JSON response envelope.");
            });
        });
        schema.field_raw("required", "[]");
    });
}

fn render_public_schema_entry(obj: &mut JsonBuilder, schema: &SchemaEntry) {
    obj.field_str("id", schema.id);
    obj.field_str("version", schema.version);
    obj.field_str("description", schema.description);
    obj.field_str("category", schema.category);
}

fn render_mcp_manifest_degradation(obj: &mut JsonBuilder, degraded: &McpManifestDegradation) {
    obj.field_str("code", degraded.code);
    obj.field_str("severity", degraded.severity);
    obj.field_str("message", degraded.message);
    obj.field_str("repair", degraded.repair);
}

/// Render the MCP adapter manifest as human-readable text.
#[must_use]
pub fn render_mcp_manifest_human() -> String {
    let feature_status = if cfg!(feature = "mcp") {
        "enabled"
    } else {
        "disabled"
    };
    let mut output = String::from("ee mcp manifest\n\n");
    output.push_str(&format!("Protocol: {MCP_PROTOCOL_VERSION}\n"));
    output.push_str(&format!("Feature: mcp ({feature_status})\n"));
    output.push_str(&format!("Tools: {}\n", COMMAND_MANIFEST.len()));
    output.push_str(&format!("Schemas: {}\n", public_schemas().len()));
    if !cfg!(feature = "mcp") {
        output.push_str("\nDegraded: mcp_feature_disabled\n");
        output.push_str("Build or install ee with the mcp feature enabled.\n");
    }
    output.push_str("\nUse `ee mcp manifest --json` for the machine-readable manifest.\n");
    output
}

/// Render the MCP adapter manifest as TOON.
#[must_use]
pub fn render_mcp_manifest_toon() -> String {
    render_toon_from_json(&render_mcp_manifest_json())
}

pub fn render_toon_from_json(json: &str) -> String {
    toon::json_to_toon(json).unwrap_or_else(|error| {
        let message = escape_toon_quoted_string(&format!("TOON encoding failed: {error}"));
        format!(
            "schema: {ERROR_SCHEMA_V1}\nerror:\n  code: toon_encoding_failed\n  message: \"{message}\"\n"
        )
    })
}

fn escape_toon_quoted_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// Legacy placeholder for backwards compatibility during transition.
#[must_use]
pub fn status_response_json() -> String {
    render_status_json(&StatusReport::gather())
}

/// Legacy placeholder for backwards compatibility during transition.
#[must_use]
pub fn human_status() -> String {
    render_status_human(&StatusReport::gather())
}

#[must_use]
pub fn help_text() -> &'static str {
    "ee - durable memory substrate for coding agents\n\nUsage:\n  ee status [--json]\n  ee --version\n  ee --help\n"
}

#[must_use]
pub fn schema_json() -> String {
    format!(
        "{{\"schema\":\"{}\",\"success\":true,\"data\":{{\"command\":\"schema\",\"schemas\":{{\"response\":\"{}\",\"error\":\"{}\"}}}}}}",
        RESPONSE_SCHEMA_V1, RESPONSE_SCHEMA_V1, ERROR_SCHEMA_V1
    )
}

#[must_use]
pub fn help_json() -> String {
    let mut b = JsonBuilder::with_capacity(4096);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "help");
        d.field_str("binary", "ee");
        d.field_str("version", env!("CARGO_PKG_VERSION"));
        d.field_str("usage", "ee [OPTIONS] [COMMAND]");
        d.field_str(
            "description",
            "Durable, local-first, explainable memory for coding agents.",
        );

        d.field_array_of_objects("globalOptions", GLOBAL_OPTIONS, |obj, opt| {
            obj.field_str("name", opt.name);
            obj.field_str("short", opt.short);
            obj.field_str("description", opt.description);
            obj.field_str("type", opt.opt_type);
        });

        d.field_array_of_objects("commands", COMMAND_MANIFEST, |obj, cmd| {
            obj.field_str("name", cmd.name);
            obj.field_str("description", cmd.description);
            obj.field_bool("available", cmd.available);
            if !cmd.subcommands.is_empty() {
                obj.field_array_of_objects("subcommands", cmd.subcommands, |sub, sc| {
                    sub.field_str("name", sc.name);
                    sub.field_str("description", sc.description);
                });
            }
            if !cmd.args.is_empty() {
                obj.field_array_of_objects("args", cmd.args, |arg, a| {
                    arg.field_str("name", a.name);
                    arg.field_str("description", a.description);
                    arg.field_bool("required", a.required);
                    if let Some(def) = a.default {
                        arg.field_str("default", def);
                    }
                });
            }
        });
    });
    b.finish()
}

struct GlobalOption {
    name: &'static str,
    short: &'static str,
    description: &'static str,
    opt_type: &'static str,
}

const GLOBAL_OPTIONS: &[GlobalOption] = &[
    GlobalOption {
        name: "--json",
        short: "-j",
        description: "Emit JSON output",
        opt_type: "flag",
    },
    GlobalOption {
        name: "--workspace",
        short: "",
        description: "Workspace root to operate on",
        opt_type: "path",
    },
    GlobalOption {
        name: "--no-color",
        short: "",
        description: "Disable colored diagnostics",
        opt_type: "flag",
    },
    GlobalOption {
        name: "--robot",
        short: "",
        description: "Use agent-oriented output defaults",
        opt_type: "flag",
    },
    GlobalOption {
        name: "--format",
        short: "",
        description: "Select output renderer (human|json|toon|jsonl|compact|hook)",
        opt_type: "enum",
    },
    GlobalOption {
        name: "--fields",
        short: "",
        description: "Control output verbosity (minimal|summary|standard|full)",
        opt_type: "enum",
    },
    GlobalOption {
        name: "--schema",
        short: "",
        description: "Print JSON schema for response envelope",
        opt_type: "flag",
    },
    GlobalOption {
        name: "--help-json",
        short: "",
        description: "Print JSON-formatted help",
        opt_type: "flag",
    },
    GlobalOption {
        name: "--agent-docs",
        short: "",
        description: "Print agent-oriented documentation",
        opt_type: "flag",
    },
    GlobalOption {
        name: "--meta",
        short: "",
        description: "Include additional metadata in response",
        opt_type: "flag",
    },
    GlobalOption {
        name: "--shadow",
        short: "",
        description: "Shadow mode for decision plane tracking (off|compare|record)",
        opt_type: "enum",
    },
    GlobalOption {
        name: "--policy",
        short: "",
        description: "Policy ID to use for decision plane operations",
        opt_type: "string",
    },
];

struct CommandArg {
    name: &'static str,
    description: &'static str,
    required: bool,
    default: Option<&'static str>,
}

struct SubcommandEntry {
    name: &'static str,
    description: &'static str,
}

struct CommandEntry {
    name: &'static str,
    description: &'static str,
    available: bool,
    subcommands: &'static [SubcommandEntry],
    args: &'static [CommandArg],
}

const COMMAND_MANIFEST: &[CommandEntry] = &[
    CommandEntry {
        name: "agent-docs",
        description: "Agent-oriented documentation for ee commands, contracts, and usage",
        available: true,
        subcommands: &[],
        args: &[CommandArg {
            name: "TOPIC",
            description: "Documentation topic (guide, commands, contracts, schemas, paths, env, exit-codes, fields, errors, formats, examples)",
            required: false,
            default: None,
        }],
    },
    CommandEntry {
        name: "analyze",
        description: "Analyze subsystem readiness and diagnostic posture",
        available: true,
        subcommands: &[SubcommandEntry {
            name: "science-status",
            description: "Report science analytics availability and degraded posture",
        }],
        args: &[],
    },
    CommandEntry {
        name: "capabilities",
        description: "Report feature availability, commands, and subsystem status",
        available: true,
        subcommands: &[],
        args: &[],
    },
    CommandEntry {
        name: "check",
        description: "Quick posture summary: ready, degraded, or needs attention",
        available: true,
        subcommands: &[],
        args: &[],
    },
    CommandEntry {
        name: "diag",
        description: "Run diagnostic commands for trust, quarantine, and streams",
        available: true,
        subcommands: &[SubcommandEntry {
            name: "quarantine",
            description: "Report quarantine status for import sources",
        }],
        args: &[],
    },
    CommandEntry {
        name: "doctor",
        description: "Run health checks on workspace and subsystems",
        available: true,
        subcommands: &[],
        args: &[CommandArg {
            name: "--fix-plan",
            description: "Output structured repair plan",
            required: false,
            default: None,
        }],
    },
    CommandEntry {
        name: "eval",
        description: "Run evaluation scenarios against fixtures",
        available: true,
        subcommands: &[
            SubcommandEntry {
                name: "run",
                description: "Run one or more evaluation scenarios",
            },
            SubcommandEntry {
                name: "list",
                description: "List available evaluation scenarios",
            },
        ],
        args: &[],
    },
    CommandEntry {
        name: "health",
        description: "Quick health check with overall verdict",
        available: true,
        subcommands: &[],
        args: &[],
    },
    CommandEntry {
        name: "help",
        description: "Print command help",
        available: true,
        subcommands: &[],
        args: &[],
    },
    CommandEntry {
        name: "import",
        description: "Import memories and evidence from external sources",
        available: true,
        subcommands: &[SubcommandEntry {
            name: "cass",
            description: "Import from coding_agent_session_search",
        }],
        args: &[],
    },
    CommandEntry {
        name: "index",
        description: "Manage search indexes",
        available: true,
        subcommands: &[
            SubcommandEntry {
                name: "rebuild",
                description: "Rebuild the search index",
            },
            SubcommandEntry {
                name: "status",
                description: "Inspect index health and generation",
            },
        ],
        args: &[],
    },
    CommandEntry {
        name: "mcp",
        description: "Inspect the optional MCP adapter manifest",
        available: true,
        subcommands: &[SubcommandEntry {
            name: "manifest",
            description: "Print the MCP tool and schema manifest",
        }],
        args: &[],
    },
    CommandEntry {
        name: "remember",
        description: "Store a new memory",
        available: true,
        subcommands: &[],
        args: &[
            CommandArg {
                name: "CONTENT",
                description: "Memory content to store",
                required: true,
                default: None,
            },
            CommandArg {
                name: "--level",
                description: "Memory level",
                required: false,
                default: Some("episodic"),
            },
            CommandArg {
                name: "--kind",
                description: "Memory kind",
                required: false,
                default: Some("fact"),
            },
            CommandArg {
                name: "--tags",
                description: "Tags (comma-separated)",
                required: false,
                default: None,
            },
            CommandArg {
                name: "--confidence",
                description: "Confidence score (0.0-1.0)",
                required: false,
                default: Some("0.8"),
            },
            CommandArg {
                name: "--source",
                description: "Source provenance URI",
                required: false,
                default: None,
            },
            CommandArg {
                name: "--dry-run",
                description: "Perform dry run without storing",
                required: false,
                default: None,
            },
        ],
    },
    CommandEntry {
        name: "rule",
        description: "Direct procedural rule management",
        available: true,
        subcommands: &[
            SubcommandEntry {
                name: "add",
                description: "Add a procedural rule with lifecycle and evidence metadata",
            },
            SubcommandEntry {
                name: "list",
                description: "List procedural rules with stable filters",
            },
            SubcommandEntry {
                name: "show",
                description: "Show one procedural rule with evidence and lifecycle metadata",
            },
        ],
        args: &[],
    },
    CommandEntry {
        name: "schema",
        description: "List or export public response schemas",
        available: true,
        subcommands: &[
            SubcommandEntry {
                name: "list",
                description: "List all available public schemas",
            },
            SubcommandEntry {
                name: "export",
                description: "Export schema JSON definition",
            },
        ],
        args: &[],
    },
    CommandEntry {
        name: "search",
        description: "Search indexed memories and sessions",
        available: true,
        subcommands: &[],
        args: &[
            CommandArg {
                name: "QUERY",
                description: "Query string to search for",
                required: true,
                default: None,
            },
            CommandArg {
                name: "--limit",
                description: "Maximum results",
                required: false,
                default: Some("10"),
            },
            CommandArg {
                name: "--database",
                description: "Database path",
                required: false,
                default: None,
            },
            CommandArg {
                name: "--index-dir",
                description: "Index directory path",
                required: false,
                default: None,
            },
        ],
    },
    CommandEntry {
        name: "status",
        description: "Report workspace and subsystem readiness",
        available: true,
        subcommands: &[],
        args: &[],
    },
    CommandEntry {
        name: "version",
        description: "Print the ee version",
        available: true,
        subcommands: &[],
        args: &[],
    },
];

#[must_use]
pub fn render_introspect_json() -> String {
    let mut b = JsonBuilder::with_capacity(8192);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "introspect");
        d.field_str("version", env!("CARGO_PKG_VERSION"));

        d.field_object("commands", |c| {
            for cmd in COMMAND_MANIFEST {
                c.field_object(cmd.name, |obj| {
                    obj.field_str("description", cmd.description);
                    obj.field_bool("available", cmd.available);
                    if !cmd.subcommands.is_empty() {
                        obj.field_array_of_objects("subcommands", cmd.subcommands, |sub, sc| {
                            sub.field_str("name", sc.name);
                            sub.field_str("description", sc.description);
                        });
                    }
                    if !cmd.args.is_empty() {
                        obj.field_raw("argCount", &cmd.args.len().to_string());
                    }
                });
            }
        });

        d.field_object("schemas", |s| {
            for schema in public_schemas() {
                s.field_object(schema.id, |obj| {
                    obj.field_str("version", schema.version);
                    obj.field_str("description", schema.description);
                    obj.field_str("category", schema.category);
                });
            }
        });

        d.field_object("errorCodes", |e| {
            for code in ERROR_CODES {
                e.field_object(code.code, |obj| {
                    obj.field_str("message", code.message);
                    obj.field_str("repair", code.repair);
                    obj.field_str("category", code.category);
                });
            }
        });

        d.field_object("globalOptions", |g| {
            for opt in GLOBAL_OPTIONS {
                g.field_object(opt.name, |obj| {
                    if !opt.short.is_empty() {
                        obj.field_str("short", opt.short);
                    }
                    obj.field_str("description", opt.description);
                    obj.field_str("type", opt.opt_type);
                });
            }
        });
    });
    b.finish()
}

#[must_use]
pub fn render_introspect_human() -> String {
    let mut output = format!("ee introspect (v{})\n\n", env!("CARGO_PKG_VERSION"));

    output.push_str("Commands:\n");
    for cmd in COMMAND_MANIFEST {
        let status = if cmd.available { "✓" } else { "○" };
        output.push_str(&format!(
            "  {} {} — {}\n",
            status, cmd.name, cmd.description
        ));
    }

    output.push_str("\nSchemas:\n");
    for schema in public_schemas() {
        output.push_str(&format!(
            "  {} (v{}) — {}\n",
            schema.id, schema.version, schema.description
        ));
    }

    output.push_str("\nError Codes:\n");
    for code in ERROR_CODES {
        output.push_str(&format!("  {} — {}\n", code.code, code.message));
    }

    output.push_str("\nNext:\n  ee introspect --json\n");
    output
}

#[must_use]
pub fn render_introspect_toon() -> String {
    render_toon_from_json(&render_introspect_json())
}

struct ErrorCodeEntry {
    code: &'static str,
    message: &'static str,
    repair: &'static str,
    category: &'static str,
}

const ERROR_CODES: &[ErrorCodeEntry] = &[
    ErrorCodeEntry {
        code: "usage",
        message: "Invalid command usage",
        repair: "ee --help",
        category: "cli",
    },
    ErrorCodeEntry {
        code: "config",
        message: "Configuration error",
        repair: "ee doctor",
        category: "config",
    },
    ErrorCodeEntry {
        code: "storage",
        message: "Storage operation failed",
        repair: "ee doctor --fix-plan",
        category: "storage",
    },
    ErrorCodeEntry {
        code: "search_index",
        message: "Search index error",
        repair: "ee index rebuild",
        category: "search",
    },
    ErrorCodeEntry {
        code: "import",
        message: "Import operation failed",
        repair: "ee import cass --dry-run",
        category: "import",
    },
    ErrorCodeEntry {
        code: "degraded",
        message: "Required capability is degraded",
        repair: "ee status --json",
        category: "degraded",
    },
    ErrorCodeEntry {
        code: "policy",
        message: "Operation denied by policy",
        repair: "ee capabilities --json",
        category: "policy",
    },
    ErrorCodeEntry {
        code: "migration",
        message: "Migration required",
        repair: "ee doctor --fix-plan",
        category: "storage",
    },
];

#[must_use]
pub fn agent_docs() -> String {
    let report = crate::core::agent_docs::AgentDocsReport::gather(None);
    render_agent_docs_json(&report)
}

fn strings_to_json_array(strings: &[String]) -> String {
    let mut arr = String::from("[");
    for (i, s) in strings.iter().enumerate() {
        if i > 0 {
            arr.push(',');
        }
        arr.push('"');
        arr.push_str(&s.replace('\\', "\\\\").replace('"', "\\\""));
        arr.push('"');
    }
    arr.push(']');
    arr
}

#[must_use]
pub fn render_agent_detect_json(report: &InstalledAgentDetectionReport) -> String {
    let mut b = JsonBuilder::with_capacity(2048);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "agent detect");
        d.field_u32("formatVersion", report.format_version);
        d.field_str("generatedAt", &report.generated_at);
        d.field_object("summary", |s| {
            s.field_u32("detectedCount", report.summary.detected_count as u32);
            s.field_u32("totalCount", report.summary.total_count as u32);
        });
        d.field_array_of_objects("installedAgents", &report.installed_agents, |obj, agent| {
            obj.field_str("slug", &agent.slug);
            obj.field_bool("detected", agent.detected);
            obj.field_raw("evidence", &strings_to_json_array(&agent.evidence));
            obj.field_raw("rootPaths", &strings_to_json_array(&agent.root_paths));
        });
    });
    b.finish()
}

#[must_use]
pub fn render_agent_detect_human(report: &InstalledAgentDetectionReport) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str("Agent Detection Report\n");
    out.push_str("======================\n\n");
    out.push_str(&format!(
        "Detected {} of {} known agent(s)\n\n",
        report.summary.detected_count, report.summary.total_count
    ));

    for agent in &report.installed_agents {
        let status = if agent.detected {
            "[detected]"
        } else {
            "[missing]"
        };
        out.push_str(&format!("{} {}\n", agent.slug, status));
        for path in &agent.root_paths {
            out.push_str(&format!("  - {}\n", path));
        }
    }
    out
}

#[must_use]
pub fn render_agent_detect_toon(report: &InstalledAgentDetectionReport) -> String {
    render_toon_from_json(&render_agent_detect_json(report))
}

#[must_use]
pub fn render_agent_status_json(report: &AgentInventoryReport) -> String {
    let mut b = JsonBuilder::with_capacity(2048);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool(
        "success",
        report.status != crate::core::agent_detect::AgentInventoryStatus::Unavailable,
    );
    b.field_object("data", |d| {
        d.field_str("command", "agent status");
        d.field_str("version", env!("CARGO_PKG_VERSION"));
        render_agent_inventory_json(d, "inventory", report, true);
    });
    b.finish()
}

#[must_use]
pub fn render_agent_status_human(report: &AgentInventoryReport) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str("Agent Inventory\n");
    out.push_str("===============\n\n");
    out.push_str(&format!(
        "Status: {}\nDetected: {} of {} known connector(s)\n\n",
        report.status.as_str(),
        report.summary.detected_count,
        report.summary.total_count
    ));

    for agent in &report.installed_agents {
        let state = if agent.detected {
            "detected"
        } else {
            "missing"
        };
        out.push_str(&format!("{} [{}]\n", agent.slug, state));
        for path in &agent.root_paths {
            out.push_str(&format!("  - {}\n", path));
        }
    }

    if !report.degraded.is_empty() {
        out.push_str("\nDegraded:\n");
        for degraded in &report.degraded {
            out.push_str(&format!("  - {}: {}\n", degraded.code, degraded.message));
        }
    }

    out
}

#[must_use]
pub fn render_agent_status_toon(report: &AgentInventoryReport) -> String {
    render_toon_from_json(&render_agent_status_json(report))
}

use crate::core::agent_docs::{
    AGENT_DOC_RECIPES, AgentDocsReport, AgentDocsTopic, CONTRACTS, DEFAULT_PATHS, ENV_VARS,
    EXAMPLES, EXIT_CODES, FIELD_LEVELS, GUIDE_SECTIONS, OUTPUT_FORMATS,
};

#[must_use]
pub fn render_agent_docs_json(report: &AgentDocsReport) -> String {
    let mut b = JsonBuilder::with_capacity(8192);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "agent-docs");
        d.field_str("version", report.version);

        if let Some(topic) = report.topic {
            d.field_str("topic", topic.as_str());
            render_agent_docs_topic_json(d, topic);
        } else {
            d.field_str("topic", "overview");
            d.field_str(
                "description",
                "Durable, local-first, explainable memory for coding agents.",
            );
            d.field_str(
                "primaryWorkflow",
                "ee context \"<task>\" --workspace . --max-tokens 4000 --json",
            );
            d.field_array_of_strs(
                "coreCommands",
                &["init", "remember", "search", "context", "why", "status"],
            );
            d.field_str("recipeCatalogCommand", "ee agent-docs recipes --json");
            d.field_raw("recipeCount", &AGENT_DOC_RECIPES.len().to_string());
            d.field_array_of_strs(
                "jqExamples",
                &[
                    ".data.topics[] | {name, description}",
                    ".data.recipes[] | {id, command, jq}",
                ],
            );
            d.field_array_of_objects("topics", AgentDocsTopic::all(), |obj, topic| {
                obj.field_str("name", topic.as_str());
                obj.field_str("description", topic.description());
            });
        }
    });
    b.finish()
}

fn render_agent_docs_topic_json(d: &mut JsonBuilder, topic: AgentDocsTopic) {
    match topic {
        AgentDocsTopic::Guide => {
            d.field_array_of_objects("sections", GUIDE_SECTIONS, |obj, section| {
                obj.field_str("title", section.title);
                obj.field_str("content", section.content);
            });
        }
        AgentDocsTopic::Commands => {
            d.field_array_of_objects("commands", COMMAND_MANIFEST, |obj, cmd| {
                obj.field_str("name", cmd.name);
                obj.field_str("description", cmd.description);
                obj.field_bool("available", cmd.available);
                if !cmd.subcommands.is_empty() {
                    obj.field_array_of_objects("subcommands", cmd.subcommands, |sub, sc| {
                        sub.field_str("name", sc.name);
                        sub.field_str("description", sc.description);
                    });
                }
                if !cmd.args.is_empty() {
                    obj.field_array_of_objects("args", cmd.args, |arg, a| {
                        arg.field_str("name", a.name);
                        arg.field_str("description", a.description);
                        arg.field_bool("required", a.required);
                        if let Some(def) = a.default {
                            arg.field_str("default", def);
                        }
                    });
                }
            });
        }
        AgentDocsTopic::Contracts => {
            d.field_array_of_objects("contracts", CONTRACTS, |obj, contract| {
                obj.field_str("name", contract.name);
                obj.field_str("schema", contract.schema);
                obj.field_str("description", contract.description);
                obj.field_str("stability", contract.stability);
            });
        }
        AgentDocsTopic::Schemas => {
            let schemas = public_schemas();
            d.field_array_of_objects("schemas", schemas, |obj, schema| {
                obj.field_str("id", schema.id);
                obj.field_str("version", schema.version);
                obj.field_str("description", schema.description);
                obj.field_str("category", schema.category);
            });
        }
        AgentDocsTopic::Paths => {
            d.field_array_of_objects("paths", DEFAULT_PATHS, |obj, path| {
                obj.field_str("name", path.name);
                obj.field_str("default", path.default);
                obj.field_str("description", path.description);
                if let Some(env) = path.env_override {
                    obj.field_str("envOverride", env);
                }
            });
        }
        AgentDocsTopic::Env => {
            d.field_array_of_objects("envVars", ENV_VARS, |obj, var| {
                obj.field_str("name", var.name);
                obj.field_str("description", var.description);
                obj.field_str("category", var.category);
                if let Some(def) = var.default {
                    obj.field_str("default", def);
                }
            });
        }
        AgentDocsTopic::ExitCodes => {
            d.field_array_of_objects("exitCodes", EXIT_CODES, |obj, code| {
                obj.field_raw("code", &code.code.to_string());
                obj.field_str("name", code.name);
                obj.field_str("description", code.description);
            });
        }
        AgentDocsTopic::Fields => {
            d.field_array_of_objects("fieldLevels", FIELD_LEVELS, |obj, level| {
                obj.field_str("name", level.name);
                obj.field_str("flag", level.flag);
                obj.field_str("includes", level.includes);
                obj.field_str("useCase", level.use_case);
            });
        }
        AgentDocsTopic::Errors => {
            d.field_array_of_objects("errorCodes", ERROR_CODES, |obj, code| {
                obj.field_str("code", code.code);
                obj.field_str("message", code.message);
                obj.field_str("repair", code.repair);
                obj.field_str("category", code.category);
            });
        }
        AgentDocsTopic::Formats => {
            d.field_array_of_objects("formats", OUTPUT_FORMATS, |obj, fmt| {
                obj.field_str("name", fmt.name);
                obj.field_str("flag", fmt.flag);
                obj.field_str("description", fmt.description);
                obj.field_bool("machineReadable", fmt.machine_readable);
            });
        }
        AgentDocsTopic::Examples => {
            d.field_array_of_objects("examples", EXAMPLES, |obj, example| {
                obj.field_str("title", example.title);
                obj.field_str("description", example.description);
                obj.field_str("command", example.command);
                obj.field_str("category", example.category);
            });
        }
        AgentDocsTopic::Recipes => {
            d.field_array_of_objects("recipes", AGENT_DOC_RECIPES, |obj, recipe| {
                obj.field_str("id", recipe.id);
                obj.field_str("title", recipe.title);
                obj.field_str("description", recipe.description);
                obj.field_str("category", recipe.category);
                obj.field_str("command", recipe.command);
                obj.field_str("jq", recipe.jq);
                obj.field_str("successCheck", recipe.success_check);
                obj.field_array_of_objects(
                    "failureBranches",
                    recipe.failure_branches,
                    |b, branch| {
                        b.field_str("condition", branch.condition);
                        b.field_str("jq", branch.jq);
                        b.field_str("nextAction", branch.next_action);
                    },
                );
            });
        }
    }
}

#[must_use]
pub fn render_agent_docs_human(report: &AgentDocsReport) -> String {
    let mut output = String::with_capacity(2048);
    output.push_str("ee agent-docs");
    if let Some(topic) = report.topic {
        output.push(' ');
        output.push_str(topic.as_str());
    }
    output.push('\n');
    output.push_str(&"-".repeat(40));
    output.push('\n');

    if let Some(topic) = report.topic {
        render_agent_docs_topic_human(&mut output, topic);
    } else {
        output.push_str("\nDurable, local-first, explainable memory for coding agents.\n\n");
        output.push_str(
            "Primary workflow:\n  ee context \"<task>\" --workspace . --max-tokens 4000 --json\n\n",
        );
        output.push_str("Recipe catalog:\n  ee agent-docs recipes --json\n\n");
        output.push_str("Available topics:\n");
        for t in AgentDocsTopic::all() {
            output.push_str(&format!("  {:12} {}\n", t.as_str(), t.description()));
        }
        output.push_str("\nRun `ee agent-docs <topic>` for details.\n");
    }

    output
}

fn render_agent_docs_topic_human(output: &mut String, topic: AgentDocsTopic) {
    match topic {
        AgentDocsTopic::Guide => {
            for section in GUIDE_SECTIONS {
                output.push_str(&format!("\n{}:\n  {}\n", section.title, section.content));
            }
        }
        AgentDocsTopic::Commands => {
            output.push_str("\nAvailable commands:\n");
            for cmd in COMMAND_MANIFEST {
                let status = if cmd.available {
                    ""
                } else {
                    " (unimplemented)"
                };
                output.push_str(&format!(
                    "  {:16} {}{}\n",
                    cmd.name, cmd.description, status
                ));
                for sub in cmd.subcommands {
                    output.push_str(&format!("    {:14} {}\n", sub.name, sub.description));
                }
            }
        }
        AgentDocsTopic::Contracts => {
            output.push_str("\nStable output contracts:\n");
            for contract in CONTRACTS {
                output.push_str(&format!(
                    "  {:12} {} ({})\n    {}\n",
                    contract.name, contract.schema, contract.stability, contract.description
                ));
            }
        }
        AgentDocsTopic::Schemas => {
            output.push_str("\nPublic schemas:\n");
            for schema in public_schemas() {
                output.push_str(&format!(
                    "  {:30} v{} [{}]\n    {}\n",
                    schema.id, schema.version, schema.category, schema.description
                ));
            }
        }
        AgentDocsTopic::Paths => {
            output.push_str("\nDefault paths:\n");
            for path in DEFAULT_PATHS {
                output.push_str(&format!("  {:14} {}\n", path.name, path.default));
                output.push_str(&format!("    {}\n", path.description));
                if let Some(env) = path.env_override {
                    output.push_str(&format!("    Override: {}\n", env));
                }
            }
        }
        AgentDocsTopic::Env => {
            output.push_str("\nEnvironment variables:\n");
            for var in ENV_VARS {
                let def = var
                    .default
                    .map_or(String::new(), |d| format!(" (default: {})", d));
                output.push_str(&format!(
                    "  {:20}{}\n    {}\n",
                    var.name, def, var.description
                ));
            }
        }
        AgentDocsTopic::ExitCodes => {
            output.push_str("\nExit codes:\n");
            for code in EXIT_CODES {
                output.push_str(&format!(
                    "  {:3} {:16} {}\n",
                    code.code, code.name, code.description
                ));
            }
        }
        AgentDocsTopic::Fields => {
            output.push_str("\nField profile levels:\n");
            for level in FIELD_LEVELS {
                output.push_str(&format!("  {:10} {}\n", level.name, level.flag));
                output.push_str(&format!("    Includes: {}\n", level.includes));
                output.push_str(&format!("    Use case: {}\n", level.use_case));
            }
        }
        AgentDocsTopic::Errors => {
            output.push_str("\nError codes:\n");
            for code in ERROR_CODES {
                output.push_str(&format!("  {:16} [{}]\n", code.code, code.category));
                output.push_str(&format!("    {}\n", code.message));
                output.push_str(&format!("    Repair: {}\n", code.repair));
            }
        }
        AgentDocsTopic::Formats => {
            output.push_str("\nOutput formats:\n");
            for fmt in OUTPUT_FORMATS {
                let machine = if fmt.machine_readable {
                    " [machine]"
                } else {
                    ""
                };
                output.push_str(&format!("  {:10}{}\n", fmt.name, machine));
                output.push_str(&format!("    Flag: {}\n", fmt.flag));
                output.push_str(&format!("    {}\n", fmt.description));
            }
        }
        AgentDocsTopic::Examples => {
            output.push_str("\nCommon examples:\n");
            for example in EXAMPLES {
                output.push_str(&format!("\n  {} [{}]\n", example.title, example.category));
                output.push_str(&format!("    {}\n", example.description));
                output.push_str(&format!("    $ {}\n", example.command));
            }
        }
        AgentDocsTopic::Recipes => {
            output.push_str("\nMachine-readable recipes:\n");
            for recipe in AGENT_DOC_RECIPES {
                output.push_str(&format!("\n  {} [{}]\n", recipe.id, recipe.category));
                output.push_str(&format!("    {}\n", recipe.description));
                output.push_str(&format!("    $ {}\n", recipe.command));
                output.push_str(&format!("    jq: {}\n", recipe.jq));
                output.push_str("    Failure branches:\n");
                for branch in recipe.failure_branches {
                    output.push_str(&format!("      - {}\n", branch.condition));
                    output.push_str(&format!("        jq: {}\n", branch.jq));
                    output.push_str(&format!("        next: {}\n", branch.next_action));
                }
            }
        }
    }
}

#[must_use]
pub fn render_agent_docs_toon(report: &AgentDocsReport) -> String {
    render_toon_from_json(&render_agent_docs_json(report))
}

#[must_use]
pub fn error_response_json(error: &DomainError) -> String {
    let code = error.code();
    let message = escape_json_string(&error.message());
    match error.repair() {
        Some(repair) => {
            let repair = escape_json_string(repair);
            format!(
                "{{\"schema\":\"{schema}\",\"error\":{{\"code\":\"{code}\",\"message\":\"{message}\",\"repair\":\"{repair}\"}}}}",
                schema = ERROR_SCHEMA_V1
            )
        }
        None => {
            format!(
                "{{\"schema\":\"{schema}\",\"error\":{{\"code\":\"{code}\",\"message\":\"{message}\"}}}}",
                schema = ERROR_SCHEMA_V1
            )
        }
    }
}

#[must_use]
pub fn error_response_toon(error: &DomainError) -> String {
    render_toon_from_json(&error_response_json(error))
}

fn escape_mermaid_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace(['\n', '\r'], " ")
}

pub fn escape_json_string(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c.is_control() => {
                result.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => result.push(c),
        }
    }
    result
}

// ============================================================================
// Field Profile Filtered Renderers (EE-037)
//
// These functions respect the `FieldProfile` setting to control output
// verbosity. Each level progressively includes more fields:
// - minimal: command, version, status only
// - summary: + top-level metrics and summary counts
// - standard: + arrays with items (default behavior)
// - full: + verbose details like provenance, why, debug info
// ============================================================================

/// Render a status report as JSON with field filtering.
#[must_use]
pub fn render_status_json_filtered(report: &StatusReport, profile: FieldProfile) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_str("fields", profile.as_str());
    b.field_object("data", |d| {
        d.field_str("command", "status");
        d.field_str("version", report.version);
        if let Some(workspace) = report.workspace.as_ref() {
            render_workspace_status_json(d, workspace);
        }

        if profile.include_summary_metrics() {
            d.field_object("capabilities", |c| {
                c.field_str("runtime", report.capabilities.runtime.as_str());
                c.field_str("storage", report.capabilities.storage.as_str());
                c.field_str("search", report.capabilities.search.as_str());
                c.field_str(
                    "agentDetection",
                    report.capabilities.agent_detection.as_str(),
                );
            });
        }

        if profile.include_arrays() {
            d.field_object("runtime", |r| {
                r.field_str("engine", report.runtime.engine);
                r.field_str("profile", report.runtime.profile);
                r.field_raw("workerThreads", &report.runtime.worker_threads.to_string());
                r.field_str("asyncBoundary", report.runtime.async_boundary);
            });
            render_memory_health_json(d, &report.memory_health);
            render_curation_health_json(d, &report.curation_health);
            render_feedback_health_json(d, &report.feedback_health);
            render_derived_assets_json(
                d,
                &report.derived_assets,
                profile.include_verbose_details(),
            );
            render_agent_inventory_json(
                d,
                "agentInventory",
                &report.agent_inventory,
                profile.include_verbose_details(),
            );
            d.field_array_of_objects("degraded", &report.degradations, |obj, deg| {
                obj.field_str("code", deg.code);
                obj.field_str("severity", deg.severity);
                obj.field_str("message", deg.message);
                if profile.include_verbose_details() {
                    obj.field_str("repair", deg.repair);
                }
            });
        }
    });
    b.finish()
}

/// Render a capabilities report as JSON with field filtering.
#[must_use]
pub fn render_capabilities_json_filtered(
    report: &CapabilitiesReport,
    profile: FieldProfile,
) -> String {
    let mut b = JsonBuilder::with_capacity(1024);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_str("fields", profile.as_str());
    b.field_object("data", |d| {
        d.field_str("command", "capabilities");
        d.field_str("version", report.version);

        if profile.include_arrays() {
            d.field_array_of_objects("subsystems", &report.subsystems, |obj, sub| {
                obj.field_str("name", sub.name);
                obj.field_str("status", sub.status.as_str());
                if profile.include_verbose_details() {
                    obj.field_str("description", sub.description);
                }
            });
            d.field_array_of_objects("features", &report.features, |obj, feat| {
                obj.field_str("name", feat.name);
                obj.field_bool("enabled", feat.enabled);
                if profile.include_verbose_details() {
                    obj.field_str("description", feat.description);
                }
            });
            d.field_array_of_objects("commands", &report.commands, |obj, cmd| {
                obj.field_str("name", cmd.name);
                obj.field_bool("available", cmd.available);
                if profile.include_verbose_details() {
                    obj.field_str("description", cmd.description);
                }
            });
            write_capabilities_output_metadata(d, report, true);
        }

        if profile.include_summary_metrics() {
            d.field_object("summary", |s| {
                s.field_raw(
                    "readySubsystems",
                    &report.ready_subsystem_count().to_string(),
                );
                s.field_raw("totalSubsystems", &report.subsystems.len().to_string());
                s.field_raw(
                    "enabledFeatures",
                    &report.enabled_feature_count().to_string(),
                );
                s.field_raw("totalFeatures", &report.features.len().to_string());
                s.field_raw(
                    "availableCommands",
                    &report.available_command_count().to_string(),
                );
                s.field_raw("totalCommands", &report.commands.len().to_string());
            });
        }
    });
    b.finish()
}

/// Render a doctor report as JSON with field filtering.
#[must_use]
pub fn render_doctor_json_filtered(report: &DoctorReport, profile: FieldProfile) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.overall_healthy);
    b.field_str("fields", profile.as_str());
    b.field_object("data", |d| {
        d.field_str("command", "doctor");
        d.field_str("version", report.version);
        d.field_bool("healthy", report.overall_healthy);

        if profile.include_arrays() {
            d.field_array_of_objects("checks", &report.checks, |obj, check| {
                obj.field_str("name", check.name);
                obj.field_str("severity", check.severity.as_str());
                if profile.include_summary_metrics() {
                    obj.field_str("message", &check.message);
                }
                if profile.include_verbose_details() {
                    if let Some(code) = check.error_code {
                        obj.field_str("errorCode", code.id);
                    }
                    if let Some(repair) = check.repair {
                        obj.field_str("repair", repair);
                    }
                }
            });
        }
    });
    b.finish()
}

/// Render a health report as JSON with field filtering.
#[must_use]
pub fn render_health_json_filtered(report: &HealthReport, profile: FieldProfile) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.verdict.is_healthy());
    b.field_str("fields", profile.as_str());
    b.field_object("data", |d| {
        d.field_str("command", "health");
        d.field_str("version", report.version);
        d.field_str("verdict", report.verdict.as_str());

        if profile.include_summary_metrics() {
            d.field_object("subsystems", |s| {
                s.field_bool("runtime", report.runtime_ok);
                s.field_bool("storage", report.storage_ok);
                s.field_bool("search", report.search_ok);
            });
            d.field_object("summary", |s| {
                s.field_raw("issueCount", &report.issue_count().to_string());
                s.field_raw("highSeverity", &report.high_severity_count().to_string());
                s.field_raw(
                    "mediumSeverity",
                    &report.medium_severity_count().to_string(),
                );
            });
        }

        if profile.include_arrays() {
            d.field_array_of_objects("issues", &report.issues, |obj, issue| {
                obj.field_str("subsystem", issue.subsystem);
                obj.field_str("code", issue.code);
                obj.field_str("severity", issue.severity);
                if profile.include_verbose_details() {
                    obj.field_str("message", issue.message);
                }
            });
        }
    });
    b.finish()
}

/// Render a check report as JSON with field filtering.
#[must_use]
pub fn render_check_json_filtered(report: &CheckReport, profile: FieldProfile) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.posture.is_usable());
    b.field_str("fields", profile.as_str());
    b.field_object("data", |d| {
        d.field_str("command", "check");
        d.field_str("version", report.version);
        d.field_str("posture", report.posture.as_str());

        if profile.include_summary_metrics() {
            d.field_bool("workspaceInitialized", report.workspace_initialized);
            d.field_bool("databaseReady", report.database_ready);
            d.field_bool("searchReady", report.search_ready);
            d.field_bool("runtimeReady", report.runtime_ready);
        }

        if profile.include_arrays() {
            d.field_array_of_objects(
                "suggestedActions",
                &report.suggested_actions,
                |obj, action| {
                    obj.field_raw("priority", &action.priority.to_string());
                    obj.field_str("command", action.command);
                    if profile.include_verbose_details() {
                        obj.field_str("reason", action.reason);
                    }
                },
            );
        }
    });
    b.finish()
}

/// Render a quarantine report as JSON with field filtering.
#[must_use]
pub fn render_quarantine_json_filtered(report: &QuarantineReport, profile: FieldProfile) -> String {
    let mut b = JsonBuilder::with_capacity(1024);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_str("fields", profile.as_str());
    b.field_object("data", |d| {
        d.field_str("command", "diag quarantine");
        d.field_str("version", report.version);

        if profile.include_summary_metrics() {
            d.field_object("summary", |s| {
                s.field_raw(
                    "quarantinedCount",
                    &report.summary.quarantined_count.to_string(),
                );
                s.field_raw("atRiskCount", &report.summary.at_risk_count.to_string());
                s.field_raw("blockedCount", &report.summary.blocked_count.to_string());
                s.field_raw("totalSources", &report.summary.total_sources.to_string());
                s.field_raw("healthyCount", &report.summary.healthy_count.to_string());
            });
        }

        if profile.include_arrays() {
            let build_entry = |obj: &mut JsonBuilder, entry: &QuarantineEntry| {
                obj.field_str("sourceId", &entry.source_id);
                obj.field_str("advisory", entry.advisory.as_str());
                obj.field_raw("effectiveTrust", &format!("{:.4}", entry.effective_trust));
                if profile.include_verbose_details() {
                    obj.field_raw("decayFactor", &format!("{:.4}", entry.decay_factor));
                    obj.field_raw("negativeRate", &format!("{:.4}", entry.negative_rate));
                    obj.field_raw("negativeCount", &entry.negative_count.to_string());
                    obj.field_raw("totalImports", &entry.total_imports.to_string());
                    obj.field_str("message", &entry.message);
                    obj.field_bool("permitsImport", entry.permits_import);
                    obj.field_bool("requiresValidation", entry.requires_validation);
                }
            };
            d.field_array_of_objects(
                "quarantinedSources",
                &report.quarantined_sources,
                build_entry,
            );
            d.field_array_of_objects("atRiskSources", &report.at_risk_sources, build_entry);
            d.field_array_of_objects("blockedSources", &report.blocked_sources, build_entry);
        }
    });
    b.finish()
}

// ============================================================================
// EE-342: Certificate output renderers
// ============================================================================

use crate::core::certificate::{
    CERTIFICATE_LIST_SCHEMA_V1, CERTIFICATE_SHOW_SCHEMA_V1, CERTIFICATE_VERIFY_SCHEMA_V1,
    CertificateListReport, CertificateShowReport, CertificateVerifyReport,
};

#[must_use]
pub fn render_certificate_list_json(report: &CertificateListReport) -> String {
    let mut b = JsonBuilder::with_capacity(1024);
    b.field_str("schema", CERTIFICATE_LIST_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "certificate list");
        d.field_u32("totalCount", report.total_count);
        d.field_u32("usableCount", report.usable_count);
        d.field_u32("expiredCount", report.expired_count);
        d.field_array_of_strings(
            "kindsPresent",
            &report
                .kinds_present
                .iter()
                .map(|k| k.as_str().to_owned())
                .collect::<Vec<_>>(),
        );
        d.field_array_of_objects("certificates", &report.certificates, |d, cert| {
            d.field_str("id", &cert.id);
            d.field_str("kind", cert.kind.as_str());
            d.field_str("status", cert.status.as_str());
            d.field_str("issuedAt", &cert.issued_at);
            d.field_str("workspaceId", &cert.workspace_id);
            d.field_bool("isUsable", cert.is_usable);
        });
    });
    b.finish()
}

#[must_use]
pub fn render_certificate_list_human(report: &CertificateListReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Certificates: {} total, {} usable, {} expired\n\n",
        report.total_count, report.usable_count, report.expired_count
    ));
    if report.certificates.is_empty() {
        out.push_str("No certificates found.\n");
    } else {
        for cert in &report.certificates {
            let status_marker = if cert.is_usable { "✓" } else { "✗" };
            out.push_str(&format!(
                "  {} {} [{}] {}\n",
                status_marker,
                cert.id,
                cert.kind.as_str(),
                cert.status.as_str()
            ));
        }
    }
    out
}

#[must_use]
pub fn render_certificate_list_toon(report: &CertificateListReport) -> String {
    render_toon_from_json(&render_certificate_list_json(report))
}

#[must_use]
pub fn render_certificate_show_json(report: &CertificateShowReport) -> String {
    let mut b = JsonBuilder::with_capacity(1024);
    b.field_str("schema", CERTIFICATE_SHOW_SCHEMA_V1);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "certificate show");
        d.field_str("verificationStatus", report.verification_status.as_str());
        d.field_str("payloadSummary", &report.payload_summary);
        d.field_object("certificate", |d| {
            d.field_str("id", &report.certificate.id);
            d.field_str("kind", report.certificate.kind.as_str());
            d.field_str("status", report.certificate.status.as_str());
            d.field_str("workspaceId", &report.certificate.workspace_id);
            d.field_str("issuedAt", &report.certificate.issued_at);
            if let Some(ref expires) = report.certificate.expires_at {
                d.field_str("expiresAt", expires);
            }
            d.field_str("payloadHash", &report.certificate.payload_hash);
            d.field_bool("isUsable", report.certificate.is_usable());
        });
    });
    b.finish()
}

#[must_use]
pub fn render_certificate_show_human(report: &CertificateShowReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("Certificate: {}\n", report.certificate.id));
    out.push_str(&format!("  Kind: {}\n", report.certificate.kind.as_str()));
    out.push_str(&format!(
        "  Status: {}\n",
        report.certificate.status.as_str()
    ));
    out.push_str(&format!(
        "  Workspace: {}\n",
        report.certificate.workspace_id
    ));
    out.push_str(&format!("  Issued: {}\n", report.certificate.issued_at));
    if let Some(ref expires) = report.certificate.expires_at {
        out.push_str(&format!("  Expires: {}\n", expires));
    }
    out.push_str(&format!(
        "  Payload Hash: {}\n",
        report.certificate.payload_hash
    ));
    out.push_str(&format!(
        "  Verification: {}\n",
        report.verification_status.as_str()
    ));
    out.push_str(&format!("  Summary: {}\n", report.payload_summary));
    out
}

#[must_use]
pub fn render_certificate_show_toon(report: &CertificateShowReport) -> String {
    render_toon_from_json(&render_certificate_show_json(report))
}

#[must_use]
pub fn render_certificate_verify_json(report: &CertificateVerifyReport) -> String {
    let mut b = JsonBuilder::with_capacity(512);
    b.field_str("schema", CERTIFICATE_VERIFY_SCHEMA_V1);
    b.field_bool("success", report.is_valid());
    b.field_object("data", |d| {
        d.field_str("command", "certificate verify");
        d.field_str("certificateId", &report.certificate_id);
        d.field_str("result", report.result.as_str());
        d.field_str("checkedAt", &report.checked_at);
        d.field_bool("hashVerified", report.hash_verified);
        d.field_bool("payloadHashFresh", report.payload_hash_fresh);
        d.field_bool("schemaVersionValid", report.schema_version_valid);
        d.field_bool("assumptionsValid", report.assumptions_valid);
        d.field_bool("statusValid", report.status_valid);
        d.field_bool("expiryValid", report.expiry_valid);
        d.field_raw(
            "failureCodes",
            &string_array_json(report.failure_codes.iter().map(String::as_str)),
        );
        d.field_str("message", &report.message);
    });
    b.finish()
}

#[must_use]
pub fn render_certificate_verify_human(report: &CertificateVerifyReport) -> String {
    let mut out = String::new();
    let status = if report.is_valid() {
        "PASSED"
    } else {
        "FAILED"
    };
    out.push_str(&format!("Certificate Verification: {}\n\n", status));
    out.push_str(&format!("  Certificate: {}\n", report.certificate_id));
    out.push_str(&format!("  Result: {}\n", report.result.as_str()));
    out.push_str(&format!("  Checked: {}\n", report.checked_at));
    out.push_str(&format!(
        "  Hash Verified: {}\n",
        if report.hash_verified { "yes" } else { "no" }
    ));
    out.push_str(&format!(
        "  Payload Hash Fresh: {}\n",
        if report.payload_hash_fresh {
            "yes"
        } else {
            "no"
        }
    ));
    out.push_str(&format!(
        "  Schema Version Valid: {}\n",
        if report.schema_version_valid {
            "yes"
        } else {
            "no"
        }
    ));
    out.push_str(&format!(
        "  Assumptions Valid: {}\n",
        if report.assumptions_valid {
            "yes"
        } else {
            "no"
        }
    ));
    out.push_str(&format!(
        "  Status Valid: {}\n",
        if report.status_valid { "yes" } else { "no" }
    ));
    out.push_str(&format!(
        "  Expiry Valid: {}\n",
        if report.expiry_valid { "yes" } else { "no" }
    ));
    out.push_str(&format!("  Message: {}\n", report.message));
    out
}

#[must_use]
pub fn render_certificate_verify_toon(report: &CertificateVerifyReport) -> String {
    render_toon_from_json(&render_certificate_verify_json(report))
}

// ============================================================================
// EE-362: Claim verification output renderers
// ============================================================================

use crate::core::claims::{ClaimListReport, ClaimShowReport, ClaimVerifyReport};

#[must_use]
pub fn render_claim_list_json(report: &ClaimListReport) -> String {
    let mut b = JsonBuilder::with_capacity(1024);
    b.field_str("schema", report.schema);
    b.field_bool("success", true);
    b.field_object("data", |d| {
        d.field_str("command", "claim list");
        d.field_str("claimsFile", &report.claims_file);
        d.field_bool("claimsFileExists", report.claims_file_exists);
        d.field_u32("totalCount", report.total_count as u32);
        d.field_u32("filteredCount", report.filtered_count as u32);
        if let Some(ref s) = report.filter_status {
            d.field_str("filterStatus", s);
        }
        if let Some(ref f) = report.filter_frequency {
            d.field_str("filterFrequency", f);
        }
        if let Some(ref t) = report.filter_tag {
            d.field_str("filterTag", t);
        }
        d.field_array_of_objects("claims", &report.claims, |d, claim| {
            d.field_str("id", &claim.id);
            d.field_str("title", &claim.title);
            d.field_str("status", claim.status.as_str());
            d.field_str("frequency", claim.frequency.as_str());
            d.field_u32("evidenceCount", claim.evidence_count as u32);
            d.field_u32("demoCount", claim.demo_count as u32);
        });
    });
    b.finish()
}

#[must_use]
pub fn render_claim_list_human(report: &ClaimListReport) -> String {
    let mut out = String::new();
    if !report.claims_file_exists {
        out.push_str("No claims.yaml found at ");
        out.push_str(&report.claims_file);
        out.push_str("\n\nTo create a claims file, add claims.yaml to your workspace root.\n");
        return out;
    }
    out.push_str(&format!(
        "Claims: {} total, {} after filters\n\n",
        report.total_count, report.filtered_count
    ));
    if report.claims.is_empty() {
        out.push_str("No claims match the specified filters.\n");
    } else {
        for claim in &report.claims {
            out.push_str(&format!(
                "  {} [{}] {}\n",
                claim.id,
                claim.status.as_str(),
                claim.title
            ));
        }
    }
    out
}

#[must_use]
pub fn render_claim_list_toon(report: &ClaimListReport) -> String {
    render_toon_from_json(&render_claim_list_json(report))
}

#[must_use]
pub fn render_claim_show_json(report: &ClaimShowReport) -> String {
    let mut b = JsonBuilder::with_capacity(1024);
    b.field_str("schema", report.schema);
    b.field_bool("success", report.found);
    b.field_object("data", |d| {
        d.field_str("command", "claim show");
        d.field_str("claimId", &report.claim_id);
        d.field_bool("found", report.found);
        if let Some(ref claim) = report.claim {
            d.field_object("claim", |d| {
                d.field_str("id", &claim.id);
                d.field_str("title", &claim.title);
                d.field_str("description", &claim.description);
                d.field_str("status", claim.status.as_str());
                d.field_str("frequency", claim.frequency.as_str());
                if let Some(ref pid) = claim.policy_id {
                    d.field_str("policyId", pid);
                }
            });
        }
        if report.include_manifest {
            if let Some(ref manifest) = report.manifest {
                d.field_object("manifest", |d| {
                    d.field_str("claimId", &manifest.claim_id);
                    d.field_u32("artifactCount", manifest.artifact_count as u32);
                    d.field_str("verificationStatus", manifest.verification_status.as_str());
                    if let Some(ref t) = manifest.last_verified_at {
                        d.field_str("lastVerifiedAt", t);
                    }
                    if let Some(ref t) = manifest.last_trace_id {
                        d.field_str("lastTraceId", t);
                    }
                });
            }
        }
    });
    b.finish()
}

#[must_use]
pub fn render_claim_show_human(report: &ClaimShowReport) -> String {
    let mut out = String::new();
    if !report.found {
        out.push_str(&format!("Claim not found: {}\n", report.claim_id));
        return out;
    }
    if let Some(ref claim) = report.claim {
        out.push_str(&format!("Claim: {}\n", claim.id));
        out.push_str(&format!("  Title: {}\n", claim.title));
        out.push_str(&format!("  Status: {}\n", claim.status.as_str()));
        out.push_str(&format!("  Frequency: {}\n", claim.frequency.as_str()));
        out.push_str(&format!("  Description: {}\n", claim.description));
    }
    out
}

#[must_use]
pub fn render_claim_show_toon(report: &ClaimShowReport) -> String {
    render_toon_from_json(&render_claim_show_json(report))
}

#[must_use]
pub fn render_claim_verify_json(report: &ClaimVerifyReport) -> String {
    let mut b = JsonBuilder::with_capacity(2048);
    b.field_str("schema", report.schema);
    b.field_bool("success", report.failed_count == 0);
    b.field_object("data", |d| {
        d.field_str("command", "claim verify");
        d.field_str("claimId", &report.claim_id);
        d.field_bool("verifyAll", report.verify_all);
        d.field_str("claimsFile", &report.claims_file);
        d.field_str("artifactsDir", &report.artifacts_dir);
        d.field_u32("totalClaims", report.total_claims as u32);
        d.field_u32("verifiedCount", report.verified_count as u32);
        d.field_u32("failedCount", report.failed_count as u32);
        d.field_u32("skippedCount", report.skipped_count as u32);
        d.field_bool("failFast", report.fail_fast);
        d.field_array_of_objects("results", &report.results, |d, result| {
            d.field_str("claimId", &result.claim_id);
            d.field_str("status", result.status.as_str());
            d.field_u32("artifactsChecked", result.artifacts_checked as u32);
            d.field_u32("artifactsPassed", result.artifacts_passed as u32);
            d.field_u32("artifactsFailed", result.artifacts_failed as u32);
            if !result.errors.is_empty() {
                let errors = result.errors.iter().map(String::as_str).collect::<Vec<_>>();
                d.field_array_of_strs("errors", &errors);
            }
        });
    });
    b.finish()
}

#[must_use]
pub fn render_claim_verify_human(report: &ClaimVerifyReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Verification: {} verified, {} failed, {} skipped\n\n",
        report.verified_count, report.failed_count, report.skipped_count
    ));
    if report.results.is_empty() {
        out.push_str("No claims to verify.\n");
    } else {
        for result in &report.results {
            let icon = match result.status {
                crate::models::ManifestVerificationStatus::Passing => "[PASS]",
                crate::models::ManifestVerificationStatus::Failing => "[FAIL]",
                _ => "[----]",
            };
            out.push_str(&format!(
                "  {} {} ({}/{} artifacts)\n",
                icon, result.claim_id, result.artifacts_passed, result.artifacts_checked
            ));
        }
    }
    out
}

#[must_use]
pub fn render_claim_verify_toon(report: &ClaimVerifyReport) -> String {
    render_toon_from_json(&render_claim_verify_json(report))
}

// ============================================================================
// EE-DIAG-001: Support Bundle Rendering
// ============================================================================

use crate::core::support_bundle::{BundleReport, InspectReport};

#[must_use]
pub fn render_support_bundle_json(report: &BundleReport) -> String {
    let raw = serde_json::to_string(report).unwrap_or_default();
    ResponseEnvelope::success().data_raw(&raw).finish()
}

#[must_use]
pub fn render_support_bundle_human(report: &BundleReport) -> String {
    let mut out = String::new();

    let mode_str = if report.dry_run { "DRY RUN" } else { "CREATED" };

    out.push_str(&format!("Support Bundle [{mode_str}]\n"));

    if let Some(ref path) = report.output_path {
        out.push_str(&format!("Output: {}\n", path.display()));
    }

    out.push_str(&format!(
        "Redaction: {}\n",
        if report.redaction_applied {
            "enabled"
        } else {
            "disabled"
        }
    ));

    out.push_str(&format!("Size: {} bytes\n\n", report.total_size_bytes));

    out.push_str("Files:\n");
    for file in &report.files_collected {
        out.push_str(&format!("  - {file}\n"));
    }

    out
}

#[must_use]
pub fn render_support_bundle_toon(report: &BundleReport) -> String {
    render_toon_from_json(&render_support_bundle_json(report))
}

#[must_use]
pub fn render_support_inspect_json(report: &InspectReport) -> String {
    let raw = serde_json::to_string(report).unwrap_or_default();
    ResponseEnvelope::success().data_raw(&raw).finish()
}

#[must_use]
pub fn render_support_inspect_human(report: &InspectReport) -> String {
    let mut out = String::new();

    out.push_str("Support Bundle Inspection\n");
    out.push_str(&format!("Path: {}\n", report.bundle_path.display()));
    out.push_str(&format!("Files: {}\n", report.files_found.len()));
    out.push_str(&format!("Total Size: {} bytes\n", report.total_size_bytes));

    if let Some(verified) = report.hash_verified {
        out.push_str(&format!(
            "Hash Verification: {}\n",
            if verified { "passed" } else { "FAILED" }
        ));
    }

    if let Some(ref version) = report.version_info {
        out.push_str(&format!("Version: {version}\n"));
    }

    out
}

#[must_use]
pub fn render_support_inspect_toon(report: &InspectReport) -> String {
    render_toon_from_json(&render_support_inspect_json(report))
}

// ============================================================================
// EE-431: Memory Economics Rendering
// ============================================================================

use crate::core::economy::{
    EconomyPrunePlan, EconomyReport, EconomyScoreReport, EconomySimulationReport,
};

#[must_use]
pub fn render_economy_report_json(report: &EconomyReport) -> String {
    let raw = serde_json::to_string(report).unwrap_or_default();
    ResponseEnvelope::success().data_raw(&raw).finish()
}

#[must_use]
pub fn render_economy_report_human(report: &EconomyReport) -> String {
    let mut out = String::new();

    out.push_str("Economy Report\n");
    out.push_str(&format!("Total Artifacts: {}\n", report.total_artifacts));
    out.push_str(&format!(
        "Overall Utility: {:.2}\n",
        report.overall_utility_score
    ));
    out.push_str(&format!(
        "Attention Budget: {:.0}/{:.0} ({:.1}%)\n\n",
        report.attention_budget_used,
        report.attention_budget_total,
        (report.attention_budget_used / report.attention_budget_total) * 100.0
    ));

    out.push_str("Artifact Breakdown:\n");
    for stats in &report.artifact_breakdown {
        out.push_str(&format!(
            "  {}: {} items, avg utility {:.2}, cost {:.0}, false alarm {:.1}%\n",
            stats.artifact_type,
            stats.count,
            stats.avg_utility,
            stats.total_cost,
            stats.false_alarm_rate * 100.0
        ));
    }

    if let Some(ref debt) = report.maintenance_debt {
        out.push_str(&format!(
            "\nMaintenance Debt: {} stale, {} consolidation candidates, {} pending tombstone\n",
            debt.stale_artifacts, debt.consolidation_candidates, debt.tombstone_pending
        ));
    }

    if let Some(ref reserves) = report.tail_risk_reserves {
        out.push_str(&format!(
            "\nTail-Risk Reserves: {} critical memories, {} fallback procedures, {:.1}% degradation coverage\n",
            reserves.critical_memories, reserves.fallback_procedures, reserves.degradation_coverage * 100.0
        ));
    }

    out
}

#[must_use]
pub fn render_economy_report_toon(report: &EconomyReport) -> String {
    render_toon_from_json(&render_economy_report_json(report))
}

#[must_use]
pub fn render_economy_score_json(report: &EconomyScoreReport) -> String {
    let raw = serde_json::to_string(report).unwrap_or_default();
    ResponseEnvelope::success().data_raw(&raw).finish()
}

#[must_use]
pub fn render_economy_score_human(report: &EconomyScoreReport) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "Economy Score: {} ({})\n",
        report.artifact_id, report.artifact_type
    ));
    out.push_str(&format!("Overall: {:.2}\n", report.overall_score));
    out.push_str(&format!("  Utility:    {:.2}\n", report.utility_score));
    out.push_str(&format!("  Cost:       {:.2}\n", report.cost_score));
    out.push_str(&format!("  Freshness:  {:.2}\n", report.freshness_score));
    out.push_str(&format!("  Confidence: {:.2}\n", report.confidence_score));

    if let Some(ref breakdown) = report.breakdown {
        out.push_str("\nBreakdown:\n");
        out.push_str(&format!(
            "  Retrieval frequency: {}\n",
            breakdown.retrieval_frequency
        ));
        out.push_str(&format!(
            "  Last accessed: {} days ago\n",
            breakdown.last_accessed_days_ago
        ));
        out.push_str(&format!("  Citation count: {}\n", breakdown.citation_count));
        out.push_str(&format!(
            "  Confidence delta: {:.3}\n",
            breakdown.confidence_delta
        ));
        out.push_str(&format!("  Decay factor: {:.3}\n", breakdown.decay_factor));
    }

    out
}

#[must_use]
pub fn render_economy_score_toon(report: &EconomyScoreReport) -> String {
    render_toon_from_json(&render_economy_score_json(report))
}

#[must_use]
pub fn render_economy_simulation_json(report: &EconomySimulationReport) -> String {
    let raw = serde_json::to_string(report).unwrap_or_default();
    ResponseEnvelope::success().data_raw(&raw).finish()
}

#[must_use]
pub fn render_economy_simulation_human(report: &EconomySimulationReport) -> String {
    let mut out = String::new();

    out.push_str("Economy Simulation\n");
    out.push_str(&format!("Mutation: {}\n", report.mutation_status));
    out.push_str(&format!(
        "Ranking State Unchanged: {}\n",
        report.ranking_state_unchanged
    ));
    out.push_str(&format!(
        "Baseline Budget: {} tokens\n",
        report.baseline_budget_tokens
    ));
    out.push_str(&format!(
        "Recommended Budget: {} tokens\n",
        report.summary.recommended_budget_tokens
    ));
    out.push_str(&format!(
        "Best Score: {:.3} ({:+.3} vs baseline)\n\n",
        report.summary.best_score, report.summary.score_delta_vs_baseline
    ));

    for scenario in &report.scenarios {
        out.push_str(&format!(
            "- {} tokens: score {:.3}, surfaced {}, reserve {} tokens\n",
            scenario.budget_tokens,
            scenario.score,
            scenario.surfaced_count,
            scenario.budget.risk_reserve_tokens
        ));
    }

    out
}

#[must_use]
pub fn render_economy_simulation_toon(report: &EconomySimulationReport) -> String {
    render_toon_from_json(&render_economy_simulation_json(report))
}

#[must_use]
pub fn render_economy_prune_plan_json(report: &EconomyPrunePlan) -> String {
    let raw = serde_json::to_string(report).unwrap_or_default();
    ResponseEnvelope::success().data_raw(&raw).finish()
}

#[must_use]
pub fn render_economy_prune_plan_human(report: &EconomyPrunePlan) -> String {
    let mut out = String::new();

    out.push_str("Economy Prune Plan\n");
    out.push_str(&format!("Status: {}\n", report.status));
    out.push_str(&format!("Dry Run: {}\n", report.dry_run));
    out.push_str(&format!("Mutation: {}\n", report.mutation_status));
    out.push_str(&format!(
        "Recommendations: {}\n",
        report.summary.recommendation_count
    ));
    out.push_str(&format!(
        "Estimated Token Savings: {}\n\n",
        report.summary.estimated_token_savings
    ));

    for recommendation in &report.recommendations {
        out.push_str(&format!(
            "- {} {} {} item(s): {} [priority {}, risk {}]\n",
            recommendation.action,
            recommendation.candidate_count,
            recommendation.artifact_type,
            recommendation.rationale,
            recommendation.priority,
            recommendation.risk
        ));
    }

    out
}

#[must_use]
pub fn render_economy_prune_plan_toon(report: &EconomyPrunePlan) -> String {
    render_toon_from_json(&render_economy_prune_plan_json(report))
}

/// Schema identifier for shadow-run reports.
pub const SHADOW_RUN_SCHEMA_V1: &str = "ee.shadow_run.v1";

/// A single shadow-vs-incumbent comparison for decision plane tracking.
#[derive(Clone, Debug)]
pub struct ShadowRunComparison {
    /// Which decision plane this comparison belongs to.
    pub plane: DecisionPlane,
    /// Tracking metadata (policy, decision, trace IDs).
    pub metadata: DecisionPlaneMetadata,
    /// When the decision was made.
    pub decided_at: String,
    /// The shadow policy's outcome.
    pub shadow_outcome: String,
    /// The incumbent policy's outcome.
    pub incumbent_outcome: String,
    /// Whether the outcomes differ.
    pub diverged: bool,
    /// Confidence score from the shadow decision.
    pub confidence: Option<f64>,
    /// Explanation for the shadow decision.
    pub reason: Option<String>,
}

impl ShadowRunComparison {
    #[must_use]
    pub fn from_record(record: &DecisionRecord) -> Option<Self> {
        if !record.shadow {
            return None;
        }
        Some(Self {
            plane: record.plane,
            metadata: record.metadata.clone(),
            decided_at: record.decided_at.clone(),
            shadow_outcome: record.outcome.clone(),
            incumbent_outcome: record.incumbent_outcome.clone().unwrap_or_default(),
            diverged: record.incumbent_outcome.as_deref() != Some(&record.outcome),
            confidence: record.confidence,
            reason: record.reason.clone(),
        })
    }
}

/// Summary metrics for a shadow-run report.
#[derive(Clone, Debug, Default)]
pub struct ShadowRunSummary {
    /// Total number of shadow decisions.
    pub total: u32,
    /// Number that diverged from incumbent.
    pub diverged: u32,
    /// Number that matched incumbent.
    pub matched: u32,
    /// Average confidence of shadow decisions (if available).
    pub avg_confidence: Option<f64>,
}

/// Report for shadow-run comparisons.
#[derive(Clone, Debug)]
pub struct ShadowRunReport {
    /// Schema identifier.
    pub schema: String,
    /// Command that generated this report.
    pub command: String,
    /// Policy ID being shadow-tested.
    pub shadow_policy: String,
    /// Incumbent policy for comparison.
    pub incumbent_policy: String,
    /// Individual comparisons.
    pub comparisons: Vec<ShadowRunComparison>,
    /// Summary metrics.
    pub summary: ShadowRunSummary,
}

impl ShadowRunReport {
    #[must_use]
    pub fn new(shadow_policy: impl Into<String>, incumbent_policy: impl Into<String>) -> Self {
        Self {
            schema: SHADOW_RUN_SCHEMA_V1.to_owned(),
            command: "shadow-run".to_owned(),
            shadow_policy: shadow_policy.into(),
            incumbent_policy: incumbent_policy.into(),
            comparisons: Vec::new(),
            summary: ShadowRunSummary::default(),
        }
    }

    #[must_use]
    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.command = command.into();
        self
    }

    pub fn add_comparison(&mut self, comparison: ShadowRunComparison) {
        if comparison.diverged {
            self.summary.diverged += 1;
        } else {
            self.summary.matched += 1;
        }
        self.summary.total += 1;
        self.comparisons.push(comparison);
    }

    pub fn add_from_record(&mut self, record: &DecisionRecord) {
        if let Some(comparison) = ShadowRunComparison::from_record(record) {
            self.add_comparison(comparison);
        }
    }

    pub fn compute_avg_confidence(&mut self) {
        let confidences: Vec<f64> = self
            .comparisons
            .iter()
            .filter_map(|c| c.confidence)
            .collect();
        if !confidences.is_empty() {
            let sum: f64 = confidences.iter().sum();
            self.summary.avg_confidence = Some(sum / confidences.len() as f64);
        }
    }

    #[must_use]
    pub fn divergence_rate(&self) -> f64 {
        if self.summary.total == 0 {
            0.0
        } else {
            f64::from(self.summary.diverged) / f64::from(self.summary.total)
        }
    }
}

/// Render a shadow-run report as JSON.
#[must_use]
pub fn render_shadow_run_json(report: &ShadowRunReport) -> String {
    let mut b = JsonBuilder::with_capacity(2048);
    b.field_str("schema", &report.schema);
    b.field_str("command", &report.command);
    b.field_object("policies", |p| {
        p.field_str("shadow", &report.shadow_policy);
        p.field_str("incumbent", &report.incumbent_policy);
    });
    b.field_object("summary", |s| {
        s.field_u32("total", report.summary.total);
        s.field_u32("diverged", report.summary.diverged);
        s.field_u32("matched", report.summary.matched);
        let rate = format!("{:.4}", report.divergence_rate());
        s.field_raw("divergenceRate", &rate);
        if let Some(avg) = report.summary.avg_confidence {
            let avg_str = format!("{:.4}", avg);
            s.field_raw("avgConfidence", &avg_str);
        }
    });
    b.field_array_of_objects("comparisons", &report.comparisons, |obj, c| {
        obj.field_str("plane", c.plane.as_str());
        obj.field_str("decidedAt", &c.decided_at);
        obj.field_str("shadowOutcome", &c.shadow_outcome);
        obj.field_str("incumbentOutcome", &c.incumbent_outcome);
        obj.field_bool("diverged", c.diverged);
        if let Some(conf) = c.confidence {
            let conf_str = format!("{:.4}", conf);
            obj.field_raw("confidence", &conf_str);
        }
        if let Some(reason) = &c.reason {
            obj.field_str("reason", reason);
        }
        if let Some(policy_id) = &c.metadata.policy_id {
            obj.field_str("policyId", policy_id);
        }
        if let Some(decision_id) = &c.metadata.decision_id {
            obj.field_str("decisionId", decision_id);
        }
        if let Some(trace_id) = &c.metadata.trace_id {
            obj.field_str("traceId", trace_id);
        }
    });
    b.finish()
}

/// Render a shadow-run report as human-readable text.
#[must_use]
pub fn render_shadow_run_human(report: &ShadowRunReport) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str("Shadow-Run Comparison Report\n");
    out.push_str("============================\n\n");
    out.push_str(&format!("Shadow policy:    {}\n", report.shadow_policy));
    out.push_str(&format!(
        "Incumbent policy: {}\n\n",
        report.incumbent_policy
    ));
    out.push_str("Summary:\n");
    out.push_str(&format!("  Total decisions:  {}\n", report.summary.total));
    out.push_str(&format!(
        "  Diverged:         {}\n",
        report.summary.diverged
    ));
    out.push_str(&format!("  Matched:          {}\n", report.summary.matched));
    out.push_str(&format!(
        "  Divergence rate:  {:.1}%\n",
        report.divergence_rate() * 100.0
    ));
    if let Some(avg) = report.summary.avg_confidence {
        out.push_str(&format!("  Avg confidence:   {:.2}\n", avg));
    }

    if !report.comparisons.is_empty() {
        out.push_str("\nComparisons:\n");
        for (i, c) in report.comparisons.iter().enumerate() {
            let status = if c.diverged { "DIVERGED" } else { "MATCHED" };
            out.push_str(&format!(
                "\n  {}. [{}] {} ({})\n",
                i + 1,
                status,
                c.plane,
                c.decided_at
            ));
            out.push_str(&format!("     Shadow:    {}\n", c.shadow_outcome));
            out.push_str(&format!("     Incumbent: {}\n", c.incumbent_outcome));
            if let Some(reason) = &c.reason {
                out.push_str(&format!("     Reason:    {}\n", reason));
            }
            if let Some(conf) = c.confidence {
                out.push_str(&format!("     Confidence: {:.2}\n", conf));
            }
        }
    }

    out.push_str("\nNext:\n");
    out.push_str("  ee shadow-run --policy <id> --json\n");
    out
}

/// Render a shadow-run report as TOON.
#[must_use]
pub fn render_shadow_run_toon(report: &ShadowRunReport) -> String {
    render_toon_from_json(&render_shadow_run_json(report))
}

// ============================================================================
// EE-382: Lab (Counterfactual Memory) Rendering
// ============================================================================

use crate::core::lab::{CaptureReport, CounterfactualReport, ReplayReport};

/// Render a lab capture report as JSON.
#[must_use]
pub fn render_lab_capture_json(report: &CaptureReport) -> String {
    let json = serde_json::json!({
        "schema": crate::models::RESPONSE_SCHEMA_V1,
        "success": true,
        "data": {
            "episode_id": report.episode_id,
            "workspace": report.workspace,
            "task_input": report.task_input,
            "packHash": report.pack_hash,
            "policyIds": report.policy_ids,
            "outcomeRef": report.outcome_ref,
            "repositoryFingerprint": report.repository_fingerprint,
            "evidenceIds": report.evidence_ids,
            "redactionStatus": report.redaction_status,
            "redactionClasses": report.redaction_classes,
            "episodeHash": report.episode_hash,
            "stored": report.stored,
            "memories_captured": report.memories_captured,
            "actions_captured": report.actions_captured,
            "dry_run": report.dry_run,
            "captured_at": report.captured_at,
        }
    });
    json.to_string()
}

/// Render a lab capture report as human-readable text.
#[must_use]
pub fn render_lab_capture_human(report: &CaptureReport) -> String {
    let mut lines = Vec::new();
    if report.dry_run {
        lines.push("Lab capture (dry run):".to_string());
    } else {
        lines.push("Lab capture:".to_string());
    }
    lines.push(format!("  Episode ID: {}", report.episode_id));
    if !report.task_input.is_empty() {
        lines.push(format!("  Task input: {}", report.task_input));
    }
    lines.push(format!("  Memories: {}", report.memories_captured));
    lines.push(format!("  Actions: {}", report.actions_captured));
    lines.push(format!("  Captured at: {}", report.captured_at));
    lines.join("\n")
}

/// Render a lab capture report as TOON.
#[must_use]
pub fn render_lab_capture_toon(report: &CaptureReport) -> String {
    format!(
        "LAB_CAPTURE|{}|{}|{}|{}",
        report.episode_id,
        report.memories_captured,
        report.actions_captured,
        if report.dry_run {
            "dry_run"
        } else {
            "captured"
        }
    )
}

/// Render a lab replay report as JSON.
#[must_use]
pub fn render_lab_replay_json(report: &ReplayReport) -> String {
    let json = serde_json::json!({
        "schema": crate::models::RESPONSE_SCHEMA_V1,
        "success": true,
        "data": {
            "episode_id": report.episode_id,
            "replay_id": report.replay_id,
            "status": report.status.as_str(),
            "frozenInputs": report.frozen_inputs,
            "replayEvidenceAvailable": report.replay_evidence_available,
            "missingFrozenInputs": report.missing_frozen_inputs,
            "mutableCurrentStateAccess": report.mutable_current_state_access,
            "episodeHashVerified": report.episode_hash_verified,
            "dry_run": report.dry_run,
            "warnings": report.warnings,
            "replayed_at": report.replayed_at,
        }
    });
    json.to_string()
}

/// Render a lab replay report as human-readable text.
#[must_use]
pub fn render_lab_replay_human(report: &ReplayReport) -> String {
    let mut lines = Vec::new();
    if report.dry_run {
        lines.push("Lab replay (dry run):".to_string());
    } else {
        lines.push("Lab replay:".to_string());
    }
    lines.push(format!("  Episode ID: {}", report.episode_id));
    lines.push(format!("  Replay ID: {}", report.replay_id));
    lines.push(format!("  Status: {}", report.status.as_str()));
    lines.push(format!(
        "  Replay evidence available: {}",
        report.replay_evidence_available
    ));
    if !report.missing_frozen_inputs.is_empty() {
        lines.push(format!(
            "  Missing frozen inputs: {}",
            report.missing_frozen_inputs.join(", ")
        ));
    }
    lines.push(format!("  Replayed at: {}", report.replayed_at));
    lines.join("\n")
}

/// Render a lab replay report as TOON.
#[must_use]
pub fn render_lab_replay_toon(report: &ReplayReport) -> String {
    format!(
        "LAB_REPLAY|{}|{}|{}",
        report.episode_id,
        report.status.as_str(),
        if report.dry_run {
            "dry_run"
        } else {
            "replayed"
        }
    )
}

/// Render a lab counterfactual report as JSON.
#[must_use]
pub fn render_lab_counterfactual_json(report: &CounterfactualReport) -> String {
    let json = serde_json::json!({
        "schema": crate::models::RESPONSE_SCHEMA_V1,
        "success": true,
        "data": {
            "run_id": report.run_id,
            "episode_id": report.episode_id,
            "status": report.status.as_str(),
            "observedPackHash": report.observed_pack_hash,
            "counterfactualPackHash": report.counterfactual_pack_hash,
            "changedItems": report.changed_items,
            "confidenceState": report.confidence_state,
            "assumptions": report.assumptions,
            "degradationCodes": report.degradation_codes,
            "nextAction": report.next_action,
            "durableMutation": report.durable_mutation,
            "curationCandidates": report.curation_candidates,
            "claimStatus": report.claim_status,
            "replayEvidenceAvailable": report.replay_evidence_available,
            "behaviorClaims": report.behavior_claims,
            "interventions_applied": report.interventions_applied,
            "hypothesisRecords": report.hypothesis_records.len(),
            "hypothesisKinds": report.hypothesis_records.iter().map(|record| &record.hypothesis_kind).collect::<Vec<_>>(),
            "dry_run": report.dry_run,
            "analyzed_at": report.analyzed_at,
        }
    });
    json.to_string()
}

/// Render a lab counterfactual report as human-readable text.
#[must_use]
pub fn render_lab_counterfactual_human(report: &CounterfactualReport) -> String {
    let mut lines = Vec::new();
    if report.dry_run {
        lines.push("Lab counterfactual (dry run):".to_string());
    } else {
        lines.push("Lab counterfactual:".to_string());
    }
    lines.push(format!("  Run ID: {}", report.run_id));
    lines.push(format!("  Episode ID: {}", report.episode_id));
    lines.push(format!("  Status: {}", report.status.as_str()));
    lines.push(format!("  Interventions: {}", report.interventions_applied));
    lines.push(format!(
        "  Behavior claims: {}",
        report.behavior_claims.len()
    ));
    if !report.hypothesis_records.is_empty() {
        lines.push(format!(
            "  Hypothesis records: {}",
            report.hypothesis_records.len()
        ));
        for record in &report.hypothesis_records {
            lines.push(format!("    - {}: {}", record.id, record.explanation));
        }
    }
    lines.push(format!("  Analyzed at: {}", report.analyzed_at));
    lines.join("\n")
}

/// Render a lab counterfactual report as TOON.
#[must_use]
pub fn render_lab_counterfactual_toon(report: &CounterfactualReport) -> String {
    format!(
        "LAB_COUNTERFACTUAL|{}|{}|{}|{}|{}",
        report.run_id,
        report.episode_id,
        report.status.as_str(),
        report.interventions_applied,
        if report.dry_run {
            "dry_run"
        } else {
            "executed"
        }
    )
}

// ============================================================================
// EE-391: Preflight Rendering
// ============================================================================

use crate::core::preflight::{CloseReport, RunReport, ShowReport};

/// Render a preflight run report as JSON.
#[must_use]
pub fn render_preflight_run_json(report: &RunReport) -> String {
    let json = serde_json::json!({
        "schema": crate::models::RESPONSE_SCHEMA_V1,
        "success": true,
        "data": {
            "run_id": report.run_id,
            "task_input": report.task_input,
            "status": report.status,
            "risk_level": report.risk_level,
            "cleared": report.cleared,
            "block_reason": report.block_reason,
            "risk_brief_id": report.risk_brief_id,
            "top_risks": report.top_risks,
            "ask_now_prompts": report.ask_now_prompts,
            "must_verify_checks": report.must_verify_checks,
            "evidence_ids": report.evidence_ids,
            "next_action": report.next_action,
            "risks_identified": report.risks_identified,
            "tripwires_set": report.tripwires_set,
            "tripwires": report.tripwires,
            "degraded": report.degraded,
            "dry_run": report.dry_run,
            "started_at": report.started_at,
            "completed_at": report.completed_at,
        }
    });
    json.to_string()
}

/// Render a preflight run report as human-readable text.
#[must_use]
pub fn render_preflight_run_human(report: &RunReport) -> String {
    let mut lines = Vec::new();
    if report.dry_run {
        lines.push("Preflight run (dry run):".to_string());
    } else {
        lines.push("Preflight run:".to_string());
    }
    lines.push(format!("  Run ID: {}", report.run_id));
    lines.push(format!("  Task: {}", report.task_input));
    lines.push(format!("  Status: {}", report.status));
    lines.push(format!("  Risk level: {}", report.risk_level));
    lines.push(format!("  Cleared: {}", report.cleared));
    if let Some(ref reason) = report.block_reason {
        lines.push(format!("  Block reason: {}", reason));
    }
    lines.push(format!("  Started at: {}", report.started_at));
    lines.join("\n")
}

/// Render a preflight run report as TOON.
#[must_use]
pub fn render_preflight_run_toon(report: &RunReport) -> String {
    format!(
        "PREFLIGHT_RUN|{}|{}|{}|{}",
        report.run_id,
        report.risk_level,
        if report.cleared { "cleared" } else { "blocked" },
        if report.dry_run {
            "dry_run"
        } else {
            "executed"
        }
    )
}

/// Render a preflight show report as JSON.
#[must_use]
pub fn render_preflight_show_json(report: &ShowReport) -> String {
    let json = serde_json::json!({
        "schema": crate::models::RESPONSE_SCHEMA_V1,
        "success": true,
        "data": {
            "run": report.run,
            "brief": report.brief,
            "tripwires": report.tripwires,
            "degraded": report.degraded,
        }
    });
    json.to_string()
}

/// Render a preflight show report as human-readable text.
#[must_use]
pub fn render_preflight_show_human(report: &ShowReport) -> String {
    let mut lines = Vec::new();
    lines.push("Preflight run details:".to_string());
    lines.push(format!("  ID: {}", report.run.id));
    lines.push(format!("  Task: {}", report.run.task_input));
    lines.push(format!("  Status: {}", report.run.status));
    lines.push(format!("  Risk level: {}", report.run.risk_level));
    lines.push(format!("  Cleared: {}", report.run.cleared));
    if let Some(ref reason) = report.run.block_reason {
        lines.push(format!("  Block reason: {}", reason));
    }
    if let Some(ref brief) = report.brief {
        lines.push("  Risk brief:".to_string());
        lines.push(format!("    ID: {}", brief.id));
        lines.push(format!("    Level: {}", brief.risk_level));
        if let Some(ref summary) = brief.summary {
            lines.push(format!("    Summary: {}", summary));
        }
    }
    if !report.tripwires.is_empty() {
        lines.push(format!("  Tripwires: {}", report.tripwires.len()));
    }
    lines.join("\n")
}

/// Render a preflight show report as TOON.
#[must_use]
pub fn render_preflight_show_toon(report: &ShowReport) -> String {
    format!(
        "PREFLIGHT_SHOW|{}|{}|{}",
        report.run.id, report.run.risk_level, report.run.status
    )
}

/// Render a preflight close report as JSON.
#[must_use]
pub fn render_preflight_close_json(report: &CloseReport) -> String {
    let json = serde_json::json!({
        "schema": crate::models::RESPONSE_SCHEMA_V1,
        "success": true,
        "data": {
            "run_id": report.run_id,
            "previous_status": report.previous_status,
            "new_status": report.new_status,
            "cleared": report.cleared,
            "reason": report.reason,
            "task_outcome": report.task_outcome,
            "feedback": report.feedback,
            "dry_run": report.dry_run,
            "closed_at": report.closed_at,
        }
    });
    json.to_string()
}

/// Render a preflight close report as human-readable text.
#[must_use]
pub fn render_preflight_close_human(report: &CloseReport) -> String {
    let mut lines = Vec::new();
    if report.dry_run {
        lines.push("Preflight close (dry run):".to_string());
    } else {
        lines.push("Preflight closed:".to_string());
    }
    lines.push(format!("  Run ID: {}", report.run_id));
    lines.push(format!("  Previous status: {}", report.previous_status));
    lines.push(format!("  New status: {}", report.new_status));
    lines.push(format!("  Cleared: {}", report.cleared));
    if let Some(ref outcome) = report.task_outcome {
        lines.push(format!("  Task outcome: {}", outcome));
    }
    if let Some(ref reason) = report.reason {
        lines.push(format!("  Reason: {}", reason));
    }
    if let Some(ref feedback) = report.feedback {
        lines.push(format!("  Feedback: {}", feedback.signal));
        lines.push(format!(
            "  Score effect: utility {:+.2}, confidence {:+.2}, false alarms +{}",
            feedback.score_effect.utility_delta,
            feedback.score_effect.confidence_delta,
            feedback.score_effect.false_alarm_delta
        ));
    }
    lines.push(format!("  Closed at: {}", report.closed_at));
    lines.join("\n")
}

/// Render a preflight close report as TOON.
#[must_use]
pub fn render_preflight_close_toon(report: &CloseReport) -> String {
    format!(
        "PREFLIGHT_CLOSE|{}|{}|{}",
        report.run_id,
        report.new_status,
        if report.dry_run { "dry_run" } else { "closed" }
    )
}

// ============================================================================
// EE-411: Procedure Output Rendering
// ============================================================================

use crate::core::procedure::{
    ProcedureDriftReport, ProcedureExportReport, ProcedureListReport, ProcedurePromoteReport,
    ProcedureProposeReport, ProcedureShowReport,
};

/// Render a procedure propose report as JSON.
#[must_use]
pub fn render_procedure_propose_json(report: &ProcedureProposeReport) -> String {
    serde_json::json!({
        "schema": report.schema,
        "success": true,
        "procedureId": report.procedure_id,
        "title": report.title,
        "summary": report.summary,
        "status": report.status,
        "sourceRunCount": report.source_run_count,
        "evidenceCount": report.evidence_count,
        "dryRun": report.dry_run,
        "createdAt": report.created_at,
    })
    .to_string()
}

/// Render a procedure propose report as human-readable text.
#[must_use]
pub fn render_procedure_propose_human(report: &ProcedureProposeReport) -> String {
    let mut out = String::with_capacity(512);
    out.push_str(&format!("Procedure Proposed: {}\n\n", report.procedure_id));
    out.push_str(&format!("Title: {}\n", report.title));
    out.push_str(&format!("Summary: {}\n", report.summary));
    out.push_str(&format!("Status: {}\n", report.status));
    out.push_str(&format!("Source runs: {}\n", report.source_run_count));
    out.push_str(&format!("Evidence items: {}\n", report.evidence_count));
    if report.dry_run {
        out.push_str("\n[dry-run: no changes made]\n");
    }
    out.push_str("\nNext:\n  ee procedure show ");
    out.push_str(&report.procedure_id);
    out.push_str(" --json\n");
    out
}

/// Render a procedure propose report as TOON.
#[must_use]
pub fn render_procedure_propose_toon(report: &ProcedureProposeReport) -> String {
    format!(
        "PROCEDURE_PROPOSE|{}|{}|{}",
        report.procedure_id,
        report.status,
        if report.dry_run { "dry_run" } else { "created" }
    )
}

/// Render a procedure show report as JSON.
#[must_use]
pub fn render_procedure_show_json(report: &ProcedureShowReport) -> String {
    serde_json::to_string(report).unwrap_or_default()
}

/// Render a procedure show report as human-readable text.
#[must_use]
pub fn render_procedure_show_human(report: &ProcedureShowReport) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str(&format!("Procedure: {}\n", report.procedure.procedure_id));
    out.push_str(&format!("Title: {}\n", report.procedure.title));
    out.push_str(&format!("Status: {}\n", report.procedure.status));
    out.push_str(&format!("Steps: {}\n\n", report.procedure.step_count));
    out.push_str(&format!("Summary: {}\n\n", report.procedure.summary));

    if !report.steps.is_empty() {
        out.push_str("Steps:\n");
        for step in &report.steps {
            out.push_str(&format!(
                "  {}. {} {}\n",
                step.sequence,
                step.title,
                if step.required { "" } else { "(optional)" }
            ));
            out.push_str(&format!("     {}\n", step.instruction));
            if let Some(ref hint) = step.command_hint {
                out.push_str(&format!("     Command: {}\n", hint));
            }
        }
    }

    if let Some(ref v) = report.verification {
        out.push_str(&format!("\nVerification: {}\n", v.status));
    }

    out.push_str("\nNext:\n  ee procedure export ");
    out.push_str(&report.procedure.procedure_id);
    out.push_str(" --export-format markdown\n");
    out
}

/// Render a procedure show report as TOON.
#[must_use]
pub fn render_procedure_show_toon(report: &ProcedureShowReport) -> String {
    format!(
        "PROCEDURE_SHOW|{}|{}|steps={}",
        report.procedure.procedure_id, report.procedure.status, report.procedure.step_count
    )
}

/// Render a procedure list report as JSON.
#[must_use]
pub fn render_procedure_list_json(report: &ProcedureListReport) -> String {
    serde_json::to_string(report).unwrap_or_default()
}

/// Render a procedure list report as human-readable text.
#[must_use]
pub fn render_procedure_list_human(report: &ProcedureListReport) -> String {
    let mut out = String::with_capacity(512);
    out.push_str(&format!(
        "Procedures: {} of {} shown\n\n",
        report.filtered_count, report.total_count
    ));

    if report.procedures.is_empty() {
        out.push_str("No procedures found.\n");
    } else {
        for p in &report.procedures {
            out.push_str(&format!(
                "  {} [{}] {} ({} steps)\n",
                p.procedure_id, p.status, p.title, p.step_count
            ));
        }
    }

    out.push_str("\nNext:\n  ee procedure show <id> --json\n");
    out
}

/// Render a procedure list report as TOON.
#[must_use]
pub fn render_procedure_list_toon(report: &ProcedureListReport) -> String {
    format!(
        "PROCEDURE_LIST|total={}|shown={}",
        report.total_count, report.filtered_count
    )
}

/// Render a procedure export report as JSON.
#[must_use]
pub fn render_procedure_export_json(report: &ProcedureExportReport) -> String {
    serde_json::json!({
        "schema": RESPONSE_SCHEMA_V1,
        "success": true,
        "data": {
            "schema": report.schema,
            "command": "procedure export",
            "exportId": report.export_id,
            "procedureId": report.procedure_id,
            "format": report.format,
            "artifactKind": report.artifact_kind,
            "outputPath": report.output_path,
            "content": report.content,
            "contentLength": report.content_length,
            "contentHash": report.content_hash,
            "includesEvidence": report.includes_evidence,
            "redactionStatus": report.redaction_status,
            "installMode": report.install_mode,
            "warnings": report.warnings,
            "exportedAt": report.exported_at,
        }
    })
    .to_string()
}

/// Render a procedure export report as human-readable text.
#[must_use]
pub fn render_procedure_export_human(report: &ProcedureExportReport) -> String {
    if report.output_path.is_none() {
        return report.content.clone();
    }

    let mut out = String::with_capacity(256);
    out.push_str(&format!("Exported: {}\n", report.procedure_id));
    out.push_str(&format!("Format: {}\n", report.format));
    out.push_str(&format!("Artifact: {}\n", report.artifact_kind));
    out.push_str(&format!("Size: {} bytes\n", report.content_length));
    out.push_str(&format!("Hash: {}\n", report.content_hash));
    if let Some(ref path) = report.output_path {
        out.push_str(&format!("Output: {}\n", path));
    }
    out
}

/// Render a procedure export report as TOON.
#[must_use]
pub fn render_procedure_export_toon(report: &ProcedureExportReport) -> String {
    format!(
        "PROCEDURE_EXPORT|id={}|format={}|kind={}|bytes={}|hash={}",
        report.procedure_id,
        report.format,
        report.artifact_kind,
        report.content_length,
        report.content_hash
    )
}

/// Render a procedure promotion dry-run report as JSON.
#[must_use]
pub fn render_procedure_promote_json(report: &ProcedurePromoteReport) -> String {
    serde_json::json!({
        "schema": RESPONSE_SCHEMA_V1,
        "success": true,
        "data": {
            "schema": report.schema,
            "command": "procedure promote",
            "promotionId": report.promotion_id,
            "procedureId": report.procedure_id,
            "dryRun": report.dry_run,
            "status": report.status,
            "fromStatus": report.from_status,
            "toStatus": report.to_status,
            "curation": report.curation,
            "audit": report.audit,
            "verification": report.verification,
            "plannedEffects": report.planned_effects,
            "warnings": report.warnings,
            "nextActions": report.next_actions,
            "generatedAt": report.generated_at,
        }
    })
    .to_string()
}

/// Render a procedure promotion dry-run report as human-readable text.
#[must_use]
pub fn render_procedure_promote_human(report: &ProcedurePromoteReport) -> String {
    let mut out = String::with_capacity(768);
    out.push_str("Procedure Promotion [DRY RUN]\n\n");
    out.push_str(&format!("Procedure: {}\n", report.procedure_id));
    out.push_str(&format!(
        "Status: {} -> {} ({})\n",
        report.from_status, report.to_status, report.status
    ));
    out.push_str(&format!(
        "Curation candidate: {}\n",
        report.curation.candidate_id
    ));
    out.push_str(&format!("Audit operation: {}\n", report.audit.operation_id));
    out.push_str(&format!(
        "Verification: {} passed, {} failed, confidence {:.1}%\n",
        report.verification.pass_count,
        report.verification.fail_count,
        report.verification.confidence * 100.0
    ));

    if !report.planned_effects.is_empty() {
        out.push_str("\nPlanned effects:\n");
        for effect in &report.planned_effects {
            out.push_str(&format!(
                "  - {} {} {} (would write: {}, applied: {})\n",
                effect.surface,
                effect.operation,
                effect.target_id,
                effect.would_write,
                effect.applied
            ));
        }
    }

    if !report.warnings.is_empty() {
        out.push_str("\nWarnings:\n");
        for warning in &report.warnings {
            out.push_str(&format!("  - {warning}\n"));
        }
    }

    if !report.next_actions.is_empty() {
        out.push_str("\nNext:\n");
        for action in &report.next_actions {
            out.push_str(&format!("  {action}\n"));
        }
    }

    out
}

/// Render a procedure-promotion curation plan as a deterministic Mermaid diagram.
#[must_use]
pub fn render_procedure_promote_mermaid(report: &ProcedurePromoteReport) -> String {
    let mut output = String::from("flowchart TD\n");
    output.push_str(&format!(
        "  procedure[\"procedure: {}\"]\n",
        escape_mermaid_label(&report.procedure_id)
    ));
    let curation_label = format!(
        "curation: {} {}",
        report.curation.candidate_id, report.curation.candidate_type
    );
    output.push_str(&format!(
        "  curation[\"{}\"]\n",
        escape_mermaid_label(&curation_label)
    ));
    output.push_str("  procedure --> curation\n");

    let verification_label = format!(
        "verification: {} pass {} fail {}",
        report.verification.status, report.verification.pass_count, report.verification.fail_count
    );
    output.push_str(&format!(
        "  verification[\"{}\"]\n",
        escape_mermaid_label(&verification_label)
    ));
    output.push_str("  curation --> verification\n");

    let audit_label = format!("audit: {}", report.audit.operation_id);
    output.push_str(&format!(
        "  audit[\"{}\"]\n",
        escape_mermaid_label(&audit_label)
    ));
    output.push_str("  curation --> audit\n");

    for (index, effect) in report.planned_effects.iter().enumerate() {
        let node_id = format!("effect{}", index + 1);
        let label = format!(
            "{}: {} {}",
            effect.surface, effect.operation, effect.target_id
        );
        output.push_str(&format!(
            "  {}[\"{}\"]\n",
            node_id,
            escape_mermaid_label(&label)
        ));
        output.push_str(&format!("  curation --> {}\n", node_id));
    }

    output
}

/// Render a procedure promotion dry-run report as TOON.
#[must_use]
pub fn render_procedure_promote_toon(report: &ProcedurePromoteReport) -> String {
    format!(
        "PROCEDURE_PROMOTE|id={}|status={}|dry_run={}|effects={}|warnings={}",
        report.procedure_id,
        report.status,
        report.dry_run,
        report.planned_effects.len(),
        report.warnings.len()
    )
}

/// Render a procedure drift report as JSON.
#[must_use]
pub fn render_procedure_drift_json(report: &ProcedureDriftReport) -> String {
    serde_json::json!({
        "schema": RESPONSE_SCHEMA_V1,
        "success": true,
        "data": {
            "schema": report.schema,
            "command": "procedure drift",
            "procedureId": report.procedure_id,
            "status": report.status,
            "driftDetected": report.drift_detected,
            "checkedAt": report.checked_at,
            "stalenessThresholdDays": report.staleness_threshold_days,
            "dryRun": report.dry_run,
            "mutation": report.mutation,
            "counts": report.counts,
            "signals": report.signals,
            "nextActions": report.next_actions,
        }
    })
    .to_string()
}

/// Render a procedure drift report as human-readable text.
#[must_use]
pub fn render_procedure_drift_human(report: &ProcedureDriftReport) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str("Procedure Drift [DRY RUN]\n\n");
    out.push_str(&format!("Procedure: {}\n", report.procedure_id));
    out.push_str(&format!("Status: {}\n", report.status));
    out.push_str(&format!("Signals: {}\n", report.counts.total));
    out.push_str(&format!("Checked: {}\n", report.checked_at));

    if !report.signals.is_empty() {
        out.push_str("\nSignals:\n");
        for signal in &report.signals {
            out.push_str(&format!(
                "  - {} [{}] {}: {}\n",
                signal.kind, signal.severity, signal.source_id, signal.summary
            ));
            out.push_str(&format!("    Next: {}\n", signal.recommended_action));
        }
    }

    if !report.next_actions.is_empty() {
        out.push_str("\nNext:\n");
        for action in &report.next_actions {
            out.push_str(&format!("  {action}\n"));
        }
    }
    out
}

/// Render a procedure drift report as TOON.
#[must_use]
pub fn render_procedure_drift_toon(report: &ProcedureDriftReport) -> String {
    format!(
        "PROCEDURE_DRIFT|id={}|status={}|signals={}|high={}|medium={}|applied={}",
        report.procedure_id,
        report.status,
        report.counts.total,
        report.counts.high,
        report.counts.medium,
        report.mutation.applied
    )
}

// ============================================================================
// EE-441: Learn Output Rendering
// ============================================================================

use crate::core::learn::{
    LearnAgendaReport, LearnCloseReport, LearnExperimentProposalReport, LearnExperimentRunReport,
    LearnObserveReport, LearnSummaryReport, LearnUncertaintyReport,
};

/// Render a learn agenda report as JSON.
#[must_use]
pub fn render_learn_agenda_json(report: &LearnAgendaReport) -> String {
    serde_json::json!({
        "schema": report.schema,
        "success": true,
        "totalGaps": report.total_gaps,
        "highPriorityCount": report.high_priority_count,
        "resolvedCount": report.resolved_count,
        "items": report.items,
        "generatedAt": report.generated_at,
    })
    .to_string()
}

/// Render a learn agenda report as human-readable text.
#[must_use]
pub fn render_learn_agenda_human(report: &LearnAgendaReport) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str("Learning Agenda\n\n");
    out.push_str(&format!(
        "Total gaps: {} ({} high priority, {} resolved)\n\n",
        report.total_gaps, report.high_priority_count, report.resolved_count
    ));

    for item in &report.items {
        out.push_str(&format!(
            "[{}] {} (priority: {}, uncertainty: {:.2})\n",
            item.id, item.topic, item.priority, item.uncertainty
        ));
        out.push_str(&format!("    {}\n", item.gap_description));
        out.push_str(&format!(
            "    Status: {} | Source: {}\n\n",
            item.status, item.source
        ));
    }

    out.push_str("Next:\n  ee learn uncertainty --json\n");
    out
}

/// Render a learn agenda report as TOON.
#[must_use]
pub fn render_learn_agenda_toon(report: &LearnAgendaReport) -> String {
    format!(
        "LEARN_AGENDA|total={}|high={}|resolved={}",
        report.total_gaps, report.high_priority_count, report.resolved_count
    )
}

/// Render a learn uncertainty report as JSON.
#[must_use]
pub fn render_learn_uncertainty_json(report: &LearnUncertaintyReport) -> String {
    serde_json::json!({
        "schema": report.schema,
        "success": true,
        "meanUncertainty": report.mean_uncertainty,
        "highUncertaintyCount": report.high_uncertainty_count,
        "samplingCandidates": report.sampling_candidates,
        "items": report.items,
        "generatedAt": report.generated_at,
    })
    .to_string()
}

/// Render a learn uncertainty report as human-readable text.
#[must_use]
pub fn render_learn_uncertainty_human(report: &LearnUncertaintyReport) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str("Uncertainty Estimates\n\n");
    out.push_str(&format!(
        "Mean uncertainty: {:.2} ({} high, {} candidates)\n\n",
        report.mean_uncertainty, report.high_uncertainty_count, report.sampling_candidates
    ));

    for item in &report.items {
        out.push_str(&format!(
            "[{}] {} (uncertainty: {:.2}, confidence: {:.2})\n",
            item.memory_id, item.kind, item.uncertainty, item.confidence
        ));
        out.push_str(&format!("    {}\n", item.content_preview));
        out.push_str(&format!(
            "    Retrieval count: {}\n\n",
            item.retrieval_count
        ));
    }

    out.push_str("Next:\n  ee learn summary --json\n");
    out
}

/// Render a learn uncertainty report as TOON.
#[must_use]
pub fn render_learn_uncertainty_toon(report: &LearnUncertaintyReport) -> String {
    format!(
        "LEARN_UNCERTAINTY|mean={:.2}|high={}|candidates={}",
        report.mean_uncertainty, report.high_uncertainty_count, report.sampling_candidates
    )
}

/// Render a learn summary report as JSON.
#[must_use]
pub fn render_learn_summary_json(report: &LearnSummaryReport) -> String {
    serde_json::json!({
        "schema": report.schema,
        "success": true,
        "summary": report.summary,
        "events": report.events,
        "generatedAt": report.generated_at,
    })
    .to_string()
}

/// Render a learn summary report as human-readable text.
#[must_use]
pub fn render_learn_summary_human(report: &LearnSummaryReport) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str(&format!("Learning Summary ({})\n\n", report.summary.period));
    out.push_str(&format!(
        "Memories created: {}\n",
        report.summary.memories_created
    ));
    out.push_str(&format!(
        "Memories promoted: {}\n",
        report.summary.memories_promoted
    ));
    out.push_str(&format!(
        "Memories demoted: {}\n",
        report.summary.memories_demoted
    ));
    out.push_str(&format!(
        "Rules learned: {}\n",
        report.summary.rules_learned
    ));
    out.push_str(&format!(
        "Rules validated: {}\n",
        report.summary.rules_validated
    ));
    out.push_str(&format!(
        "Gaps identified: {}\n",
        report.summary.gaps_identified
    ));
    out.push_str(&format!(
        "Gaps resolved: {}\n",
        report.summary.gaps_resolved
    ));
    out.push_str(&format!(
        "Net knowledge delta: {:+}\n\n",
        report.summary.net_knowledge_delta
    ));

    if !report.events.is_empty() {
        out.push_str("Recent Events:\n");
        for event in &report.events {
            out.push_str(&format!(
                "  [{}] {} ({})\n",
                event.event_type, event.description, event.impact
            ));
        }
    }

    out.push_str("\nNext:\n  ee learn agenda --json\n");
    out
}

/// Render a learn summary report as TOON.
#[must_use]
pub fn render_learn_summary_toon(report: &LearnSummaryReport) -> String {
    format!(
        "LEARN_SUMMARY|{}|delta={:+}|events={}",
        report.summary.period,
        report.summary.net_knowledge_delta,
        report.events.len()
    )
}

/// Render a learn experiment proposal report as JSON.
#[must_use]
pub fn render_learn_experiment_proposal_json(report: &LearnExperimentProposalReport) -> String {
    serde_json::json!({
        "schema": report.schema,
        "success": true,
        "totalCandidates": report.total_candidates,
        "returned": report.returned,
        "minExpectedValue": report.min_expected_value,
        "maxAttentionTokens": report.max_attention_tokens,
        "maxRuntimeSeconds": report.max_runtime_seconds,
        "proposals": report.proposals,
        "generatedAt": report.generated_at,
    })
    .to_string()
}

/// Render a learn experiment proposal report as human-readable text.
#[must_use]
pub fn render_learn_experiment_proposal_human(report: &LearnExperimentProposalReport) -> String {
    let mut out = String::with_capacity(1536);
    out.push_str("Learning Experiment Proposals\n\n");
    out.push_str(&format!(
        "Returned {} of {} candidates (min expected value: {:.2})\n\n",
        report.returned, report.total_candidates, report.min_expected_value
    ));

    for proposal in &report.proposals {
        out.push_str(&format!(
            "[{}] {} (expected value: {:.2})\n",
            proposal.experiment_id, proposal.title, proposal.expected_value
        ));
        out.push_str(&format!("    Topic: {}\n", proposal.topic));
        out.push_str(&format!("    Hypothesis: {}\n", proposal.hypothesis));
        out.push_str(&format!(
            "    Budget: {} tokens, {}s runtime ({})\n",
            proposal.budget.attention_tokens,
            proposal.budget.max_runtime_seconds,
            proposal.budget.budget_class
        ));
        out.push_str(&format!(
            "    Safety: {} | dry-run-first: {} | review-required: {}\n",
            proposal.safety.boundary,
            proposal.safety.dry_run_first,
            proposal.safety.review_required
        ));
        out.push_str(&format!(
            "    Decision impact: {} -> {}\n",
            proposal.decision_impact.current_decision, proposal.decision_impact.possible_change
        ));
        out.push_str(&format!("    Next: {}\n\n", proposal.next_command));
    }

    out.push_str("Next:\n  ee learn experiment run --dry-run --json\n");
    out
}

/// Render a learn experiment proposal report as TOON.
#[must_use]
pub fn render_learn_experiment_proposal_toon(report: &LearnExperimentProposalReport) -> String {
    format!(
        "LEARN_EXPERIMENT_PROPOSAL|returned={}|candidates={}|min_ev={:.2}",
        report.returned, report.total_candidates, report.min_expected_value
    )
}

/// Render a learn experiment run rehearsal as JSON.
#[must_use]
pub fn render_learn_experiment_run_json(report: &LearnExperimentRunReport) -> String {
    report.data_json().to_string()
}

/// Render a learn experiment run rehearsal as human-readable text.
#[must_use]
pub fn render_learn_experiment_run_human(report: &LearnExperimentRunReport) -> String {
    report.human_summary()
}

/// Render a learn experiment run rehearsal as TOON.
#[must_use]
pub fn render_learn_experiment_run_toon(report: &LearnExperimentRunReport) -> String {
    report.toon_summary()
}

/// Render a learn observation report as JSON.
#[must_use]
pub fn render_learn_observe_json(report: &LearnObserveReport) -> String {
    report.data_json().to_string()
}

/// Render a learn observation report as human-readable text.
#[must_use]
pub fn render_learn_observe_human(report: &LearnObserveReport) -> String {
    report.human_summary()
}

/// Render a learn observation report as TOON.
#[must_use]
pub fn render_learn_observe_toon(report: &LearnObserveReport) -> String {
    report.toon_summary()
}

/// Render a learn closure report as JSON.
#[must_use]
pub fn render_learn_close_json(report: &LearnCloseReport) -> String {
    report.data_json().to_string()
}

/// Render a learn closure report as human-readable text.
#[must_use]
pub fn render_learn_close_human(report: &LearnCloseReport) -> String {
    report.human_summary()
}

/// Render a learn closure report as TOON.
#[must_use]
pub fn render_learn_close_toon(report: &LearnCloseReport) -> String {
    report.toon_summary()
}

// ============================================================================
// EE-AUDIT-001: Audit Output Rendering
// ============================================================================

use crate::core::audit::{
    AuditDiffReport, AuditShowReport, AuditTimelineReport, AuditVerifyReport,
};
use crate::core::handoff::{
    CreateReport as HandoffCreateReport, InspectReport as HandoffInspectReport,
    PreviewReport as HandoffPreviewReport, ResumeReport as HandoffResumeReport,
};

/// Render an audit timeline report as JSON.
#[must_use]
pub fn render_audit_timeline_json(report: &AuditTimelineReport) -> String {
    serde_json::json!({
        "schema": report.schema,
        "success": true,
        "entries": report.entries,
        "pagination": report.pagination,
        "generatedAt": report.generated_at,
    })
    .to_string()
}

/// Render an audit timeline report as human-readable text.
#[must_use]
pub fn render_audit_timeline_human(report: &AuditTimelineReport) -> String {
    let mut out = String::with_capacity(2048);
    out.push_str("Audit Timeline\n\n");
    out.push_str(&format!(
        "Showing {} of {} operations\n\n",
        report.pagination.returned_count, report.pagination.total_count
    ));

    for entry in &report.entries {
        out.push_str(&format!(
            "[{}] {} ({})\n",
            entry.operation_id, entry.command_path, entry.outcome
        ));
        out.push_str(&format!(
            "    Effect: {} | Dry-run: {}\n",
            entry.effect_class, entry.dry_run
        ));
        if !entry.changed_surfaces.is_empty() {
            out.push_str(&format!(
                "    Changed: {}\n",
                entry.changed_surfaces.join(", ")
            ));
        }
        out.push('\n');
    }

    out.push_str("Next:\n  ee audit show <operation-id> --json\n");
    out
}

/// Render an audit timeline report as TOON.
#[must_use]
pub fn render_audit_timeline_toon(report: &AuditTimelineReport) -> String {
    format!(
        "AUDIT_TIMELINE|count={}|total={}|has_more={}",
        report.pagination.returned_count, report.pagination.total_count, report.pagination.has_more
    )
}

/// Render an audit show report as JSON.
#[must_use]
pub fn render_audit_show_json(report: &AuditShowReport) -> String {
    serde_json::json!({
        "schema": report.schema,
        "success": true,
        "operation": report.operation,
        "nextCommands": report.next_commands,
        "generatedAt": report.generated_at,
    })
    .to_string()
}

/// Render an audit show report as human-readable text.
#[must_use]
pub fn render_audit_show_human(report: &AuditShowReport) -> String {
    let op = &report.operation;
    let mut out = String::with_capacity(1024);
    out.push_str(&format!("Operation: {}\n\n", op.operation_id));
    out.push_str(&format!("Command: {}\n", op.command_path));
    out.push_str(&format!("Outcome: {}\n", op.outcome));
    out.push_str(&format!(
        "Effect: {} (expected: {})\n",
        op.observed_effect, op.expected_effect
    ));
    out.push_str(&format!(
        "Match: {}\n",
        if op.effect_match { "yes" } else { "MISMATCH" }
    ));
    out.push_str(&format!("Dry-run: {}\n", op.dry_run));
    out.push_str(&format!("Transaction: {}\n", op.transaction_status));
    out.push_str(&format!("Hash chain valid: {}\n\n", op.hash_chain_valid));

    if !op.changed_surfaces.is_empty() {
        out.push_str("Changed Surfaces:\n");
        for surface in &op.changed_surfaces {
            out.push_str(&format!(
                "  {} ({}) - {} rows\n",
                surface.surface_name,
                surface.surface_type,
                surface.rows_affected.unwrap_or(0)
            ));
        }
    }

    out.push_str(&format!(
        "\nRedaction: {} ({} fields redacted)\n",
        op.redaction_summary.posture, op.redaction_summary.fields_redacted
    ));

    out.push_str("\nNext:\n");
    for cmd in &report.next_commands {
        out.push_str(&format!("  {}\n", cmd));
    }
    out
}

/// Render an audit show report as TOON.
#[must_use]
pub fn render_audit_show_toon(report: &AuditShowReport) -> String {
    format!(
        "AUDIT_SHOW|{}|{}|{}|match={}",
        report.operation.operation_id,
        report.operation.command_path,
        report.operation.outcome,
        report.operation.effect_match
    )
}

/// Render an audit diff report as JSON.
#[must_use]
pub fn render_audit_diff_json(report: &AuditDiffReport) -> String {
    serde_json::json!({
        "schema": report.schema,
        "success": true,
        "operationId": report.operation_id,
        "deltas": report.deltas,
        "allMatch": report.all_match,
        "generatedAt": report.generated_at,
    })
    .to_string()
}

/// Render an audit diff report as human-readable text.
#[must_use]
pub fn render_audit_diff_human(report: &AuditDiffReport) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str(&format!("Operation Diff: {}\n\n", report.operation_id));
    out.push_str(&format!(
        "All match: {}\n\n",
        if report.all_match { "yes" } else { "NO" }
    ));

    for delta in &report.deltas {
        out.push_str(&format!(
            "[{}] {} -> {}\n",
            delta.surface_name, delta.declared_change, delta.observed_change
        ));
        out.push_str(&format!("    Status: {}\n", delta.match_status));
        if let (Some(before), Some(after)) = (delta.row_count_before, delta.row_count_after) {
            out.push_str(&format!("    Rows: {} -> {}\n", before, after));
        }
        out.push('\n');
    }

    out.push_str("Next:\n  ee audit verify --json\n");
    out
}

/// Render an audit diff report as TOON.
#[must_use]
pub fn render_audit_diff_toon(report: &AuditDiffReport) -> String {
    format!(
        "AUDIT_DIFF|{}|deltas={}|all_match={}",
        report.operation_id,
        report.deltas.len(),
        report.all_match
    )
}

/// Render an audit verify report as JSON.
#[must_use]
pub fn render_audit_verify_json(report: &AuditVerifyReport) -> String {
    serde_json::json!({
        "schema": report.schema,
        "success": report.overall_valid,
        "summary": report.summary,
        "issues": report.issues,
        "overallValid": report.overall_valid,
        "nextActions": report.next_actions,
        "generatedAt": report.generated_at,
    })
    .to_string()
}

/// Render an audit verify report as human-readable text.
#[must_use]
pub fn render_audit_verify_human(report: &AuditVerifyReport) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str("Audit Verification\n\n");
    out.push_str(&format!(
        "Overall: {}\n\n",
        if report.overall_valid {
            "VALID"
        } else {
            "ISSUES FOUND"
        }
    ));

    out.push_str(&format!(
        "Operations checked: {}\n",
        report.summary.operations_checked
    ));
    out.push_str(&format!(
        "Hash chain valid: {}\n",
        report.summary.hash_chain_valid
    ));
    out.push_str(&format!(
        "Missing records: {}\n",
        report.summary.missing_records
    ));
    out.push_str(&format!(
        "Malformed entries: {}\n",
        report.summary.malformed_entries
    ));
    out.push_str(&format!(
        "Effect mismatches: {}\n",
        report.summary.effect_mismatches
    ));
    out.push_str(&format!(
        "Redaction failures: {}\n",
        report.summary.redaction_failures
    ));

    if !report.issues.is_empty() {
        out.push_str("\nIssues:\n");
        for issue in &report.issues {
            out.push_str(&format!(
                "  [{}] {}: {}\n",
                issue.severity, issue.code, issue.message
            ));
            out.push_str(&format!("    Action: {}\n", issue.next_action));
        }
    }

    out.push_str("\nNext:\n");
    for action in &report.next_actions {
        out.push_str(&format!("  {}\n", action));
    }
    out
}

/// Render an audit verify report as TOON.
#[must_use]
pub fn render_audit_verify_toon(report: &AuditVerifyReport) -> String {
    format!(
        "AUDIT_VERIFY|ops={}|valid={}|issues={}",
        report.summary.operations_checked,
        report.overall_valid,
        report.issues.len()
    )
}

// ============================================================================
// Handoff Rendering
// ============================================================================

/// Render a handoff preview report as JSON.
#[must_use]
pub fn render_handoff_preview_json(report: &HandoffPreviewReport) -> String {
    serde_json::json!({
        "schema": report.schema,
        "workspace": report.workspace,
        "profile": report.profile,
        "planned_sections": report.planned_sections,
        "omitted_sections": report.omitted_sections,
        "evidence_ids": report.evidence_ids,
        "token_estimate": report.token_estimate,
        "byte_estimate": report.byte_estimate,
        "redaction_posture": report.redaction_posture,
        "degradations": report.degradations,
        "sufficient_for_resume": report.sufficient_for_resume,
        "generated_at": report.generated_at
    })
    .to_string()
}

/// Render a handoff preview report as human-readable text.
#[must_use]
pub fn render_handoff_preview_human(report: &HandoffPreviewReport) -> String {
    let mut lines = Vec::new();
    lines.push(format!("Handoff Preview ({})", report.profile));
    lines.push(format!("Workspace: {}", report.workspace.display()));
    lines.push(String::new());

    lines.push("Planned Sections:".to_owned());
    for section in &report.planned_sections {
        lines.push(format!(
            "  - {} ({}, ~{} tokens)",
            section.title, section.confidence, section.token_estimate
        ));
    }

    if !report.omitted_sections.is_empty() {
        lines.push(String::new());
        lines.push("Omitted Sections:".to_owned());
        for omission in &report.omitted_sections {
            lines.push(format!("  - {}: {}", omission.id, omission.reason));
        }
    }

    lines.push(String::new());
    lines.push(format!(
        "Estimates: ~{} tokens, ~{} bytes",
        report.token_estimate, report.byte_estimate
    ));
    lines.push(format!(
        "Sufficient for resume: {}",
        if report.sufficient_for_resume {
            "yes"
        } else {
            "no"
        }
    ));

    if !report.degradations.is_empty() {
        lines.push(String::new());
        lines.push("Degradations:".to_owned());
        for deg in &report.degradations {
            lines.push(format!("  - [{}] {}", deg.code, deg.message));
        }
    }

    lines.join("\n") + "\n"
}

/// Render a handoff preview report as TOON.
#[must_use]
pub fn render_handoff_preview_toon(report: &HandoffPreviewReport) -> String {
    format!(
        "HANDOFF_PREVIEW|profile={}|sections={}|tokens={}|sufficient={}",
        report.profile,
        report.planned_sections.len(),
        report.token_estimate,
        report.sufficient_for_resume
    )
}

/// Render a handoff create report as JSON.
#[must_use]
pub fn render_handoff_create_json(report: &HandoffCreateReport) -> String {
    serde_json::json!({
        "schema": report.schema,
        "capsule_id": report.capsule_id,
        "workspace": report.workspace,
        "output_path": report.output_path,
        "profile": report.profile,
        "sections_included": report.sections_included,
        "evidence_count": report.evidence_count,
        "token_count": report.token_count,
        "byte_count": report.byte_count,
        "content_hash": report.content_hash,
        "redaction_summary": report.redaction_summary,
        "dry_run": report.dry_run,
        "created_at": report.created_at
    })
    .to_string()
}

/// Render a handoff create report as human-readable text.
#[must_use]
pub fn render_handoff_create_human(report: &HandoffCreateReport) -> String {
    let mut lines = Vec::new();

    if report.dry_run {
        lines.push("Handoff Capsule (dry run)".to_owned());
    } else {
        lines.push("Handoff Capsule Created".to_owned());
    }

    lines.push(format!("ID: {}", report.capsule_id));
    lines.push(format!("Output: {}", report.output_path.display()));
    lines.push(format!("Profile: {}", report.profile));
    lines.push(String::new());
    lines.push(format!("Sections: {}", report.sections_included));
    lines.push(format!("Evidence items: {}", report.evidence_count));
    lines.push(format!("Tokens: {}", report.token_count));
    lines.push(format!("Bytes: {}", report.byte_count));
    lines.push(format!("Content hash: {}", report.content_hash));

    lines.join("\n") + "\n"
}

/// Render a handoff create report as TOON.
#[must_use]
pub fn render_handoff_create_toon(report: &HandoffCreateReport) -> String {
    format!(
        "HANDOFF_CREATE|id={}|profile={}|sections={}|tokens={}|dry_run={}",
        report.capsule_id,
        report.profile,
        report.sections_included,
        report.token_count,
        report.dry_run
    )
}

/// Render a handoff inspect report as JSON.
#[must_use]
pub fn render_handoff_inspect_json(report: &HandoffInspectReport) -> String {
    serde_json::json!({
        "schema": report.schema,
        "path": report.path,
        "capsule_id": report.capsule_id,
        "capsule_schema": report.capsule_schema,
        "validation_status": report.validation_status,
        "workspace_id": report.workspace_id,
        "repository_fingerprint": report.repository_fingerprint,
        "profile": report.profile,
        "section_count": report.section_count,
        "evidence_count": report.evidence_count,
        "hash_valid": report.hash_valid,
        "hash_expected": report.hash_expected,
        "hash_actual": report.hash_actual,
        "stale_evidence": report.stale_evidence,
        "missing_evidence": report.missing_evidence,
        "redaction_status": report.redaction_status,
        "compatible_versions": report.compatible_versions,
        "warnings": report.warnings,
        "inspected_at": report.inspected_at
    })
    .to_string()
}

/// Render a handoff inspect report as human-readable text.
#[must_use]
pub fn render_handoff_inspect_human(report: &HandoffInspectReport) -> String {
    let mut lines = Vec::new();

    lines.push(format!("Capsule Inspection: {}", report.path.display()));
    lines.push(format!("Status: {}", report.validation_status));
    lines.push(String::new());

    lines.push(format!("Capsule ID: {}", report.capsule_id));
    lines.push(format!("Schema: {}", report.capsule_schema));
    lines.push(format!("Profile: {}", report.profile));

    if let Some(ref ws) = report.workspace_id {
        lines.push(format!("Workspace: {ws}"));
    }

    lines.push(String::new());
    lines.push(format!("Sections: {}", report.section_count));
    lines.push(format!("Evidence items: {}", report.evidence_count));
    lines.push(format!(
        "Hash valid: {}",
        if report.hash_valid { "yes" } else { "no" }
    ));

    if !report.warnings.is_empty() {
        lines.push(String::new());
        lines.push("Warnings:".to_owned());
        for warning in &report.warnings {
            lines.push(format!("  - {warning}"));
        }
    }

    lines.join("\n") + "\n"
}

/// Render a handoff inspect report as TOON.
#[must_use]
pub fn render_handoff_inspect_toon(report: &HandoffInspectReport) -> String {
    format!(
        "HANDOFF_INSPECT|id={}|status={}|sections={}|hash_valid={}",
        report.capsule_id, report.validation_status, report.section_count, report.hash_valid
    )
}

/// Render a handoff resume report as JSON.
#[must_use]
pub fn render_handoff_resume_json(report: &HandoffResumeReport) -> String {
    serde_json::json!({
        "schema": report.schema,
        "capsule_id": report.capsule_id,
        "capsule_path": report.capsule_path,
        "workspace": report.workspace,
        "current_objective": report.current_objective,
        "status_summary": report.status_summary,
        "next_actions": report.next_actions,
        "blockers": report.blockers,
        "do_not_repeat": report.do_not_repeat,
        "recent_decisions": report.recent_decisions,
        "recent_outcomes": report.recent_outcomes,
        "selected_memories": report.selected_memories,
        "artifact_pointers": report.artifact_pointers,
        "degradations": report.degradations,
        "resumed_at": report.resumed_at
    })
    .to_string()
}

/// Render a handoff resume report as human-readable text.
#[must_use]
pub fn render_handoff_resume_human(report: &HandoffResumeReport) -> String {
    let mut lines = Vec::new();

    lines.push("Session Resume".to_owned());
    lines.push(format!("From capsule: {}", report.capsule_id));
    lines.push(String::new());

    if let Some(ref obj) = report.current_objective {
        lines.push("Current Objective:".to_owned());
        lines.push(format!("  {obj}"));
        lines.push(String::new());
    }

    if !report.next_actions.is_empty() {
        lines.push("Next Actions:".to_owned());
        for action in &report.next_actions {
            lines.push(format!("  {}. {}", action.priority, action.description));
            if let Some(ref cmd) = action.suggested_command {
                lines.push(format!("     Command: {cmd}"));
            }
        }
        lines.push(String::new());
    }

    if !report.blockers.is_empty() {
        lines.push("Blockers:".to_owned());
        for blocker in &report.blockers {
            let hard = if blocker.hard { " [HARD]" } else { "" };
            lines.push(format!("  - {}{hard}", blocker.description));
        }
        lines.push(String::new());
    }

    if !report.do_not_repeat.is_empty() {
        lines.push("Do Not Repeat:".to_owned());
        for dnr in &report.do_not_repeat {
            lines.push(format!("  - {}: {}", dnr.pattern, dnr.reason));
        }
        lines.push(String::new());
    }

    if !report.degradations.is_empty() {
        lines.push("Degradations:".to_owned());
        for deg in &report.degradations {
            lines.push(format!("  - [{}] {}", deg.code, deg.message));
        }
    }

    lines.join("\n") + "\n"
}

/// Render a handoff resume report as TOON.
#[must_use]
pub fn render_handoff_resume_toon(report: &HandoffResumeReport) -> String {
    format!(
        "HANDOFF_RESUME|id={}|actions={}|blockers={}|dnr={}",
        report.capsule_id,
        report.next_actions.len(),
        report.blockers.len(),
        report.do_not_repeat.len()
    )
}

// ============================================================================
// EE-363: Claim Diagnostics Output
// ============================================================================

/// Render a claims diagnostic report as JSON (ee.response.v1 envelope).
#[must_use]
pub fn render_diag_claims_json(report: &crate::core::claims::DiagClaimsReport) -> String {
    let mut b = JsonBuilder::with_capacity(1024);
    b.field_str("schema", RESPONSE_SCHEMA_V1);
    b.field_bool("success", report.health_status == "healthy");
    b.field_object("data", |d| {
        d.field_str("command", "diag claims");
        d.field_str("reportSchema", report.schema);
        d.field_str("claimsFile", &report.claims_file);
        d.field_bool("claimsFileExists", report.claims_file_exists);
        d.field_raw(
            "stalenessThresholdDays",
            &report.staleness_threshold_days.to_string(),
        );
        d.field_str("healthStatus", report.health_status);
        d.field_object("counts", |c| {
            c.field_raw("total", &report.counts.total.to_string());
            c.field_raw("verified", &report.counts.verified.to_string());
            c.field_raw("unverified", &report.counts.unverified.to_string());
            c.field_raw("stale", &report.counts.stale.to_string());
            c.field_raw("regressed", &report.counts.regressed.to_string());
            c.field_raw("unknown", &report.counts.unknown.to_string());
        });
        d.field_array_of_objects("entries", &report.entries, |obj, entry| {
            obj.field_str("id", &entry.id);
            obj.field_str("title", &entry.title);
            obj.field_str("posture", entry.posture.as_str());
            obj.field_str("severity", entry.posture.severity());
            if let Some(ref verified_at) = entry.last_verified_at {
                obj.field_str("lastVerifiedAt", verified_at);
            }
            if let Some(days) = entry.staleness_days {
                obj.field_raw("stalenessDays", &days.to_string());
            }
            obj.field_raw("evidenceCount", &entry.evidence_count.to_string());
            obj.field_raw("demoCount", &entry.demo_count.to_string());
            obj.field_str("frequency", entry.frequency.as_str());
        });
        d.field_array_of_strings("repairActions", &report.repair_actions);
    });
    b.finish()
}

/// Render a claims diagnostic report as human-readable text.
#[must_use]
pub fn render_diag_claims_human(report: &crate::core::claims::DiagClaimsReport) -> String {
    let mut output = String::with_capacity(1024);

    output.push_str("ee diag claims\n\n");

    if !report.claims_file_exists {
        output.push_str(&format!(
            "Claims file not found: {}\n\n",
            report.claims_file
        ));
        output.push_str("Next:\n");
        for action in &report.repair_actions {
            output.push_str(&format!("  {}\n", action));
        }
        return output;
    }

    output.push_str(&format!("Claims file: {}\n", report.claims_file));
    output.push_str(&format!("Health: {}\n", report.health_status));
    output.push_str(&format!(
        "Staleness threshold: {} days\n\n",
        report.staleness_threshold_days
    ));

    output.push_str("Summary:\n");
    output.push_str(&format!("  Total:      {}\n", report.counts.total));
    output.push_str(&format!("  Verified:   {}\n", report.counts.verified));
    output.push_str(&format!("  Unverified: {}\n", report.counts.unverified));
    output.push_str(&format!("  Stale:      {}\n", report.counts.stale));
    output.push_str(&format!("  Regressed:  {}\n", report.counts.regressed));

    if !report.entries.is_empty() {
        output.push_str("\nClaims requiring attention:\n");
        for entry in &report.entries {
            let severity_marker = match entry.posture.severity() {
                "error" => "✗",
                "warning" => "⚠",
                _ => "·",
            };
            output.push_str(&format!(
                "  {} [{}] {} — {}\n",
                severity_marker,
                entry.posture.as_str(),
                entry.id,
                entry.title
            ));
        }
    }

    if !report.repair_actions.is_empty() {
        output.push_str("\nNext:\n");
        for action in &report.repair_actions {
            output.push_str(&format!("  {}\n", action));
        }
    }

    output
}

/// Render a claims diagnostic report as TOON.
#[must_use]
pub fn render_diag_claims_toon(report: &crate::core::claims::DiagClaimsReport) -> String {
    render_toon_from_json(&render_diag_claims_json(report))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use uuid::Uuid;

    use super::{
        Degradation, DegradationSeverity, FieldProfile, JsonBuilder, OutputContext,
        OutputEnvironment, Renderer, ResponseEnvelope, SHADOW_RUN_SCHEMA_V1, ShadowRunComparison,
        ShadowRunReport, error_response_json, escape_json_string, help_text, human_status,
        render_agent_docs_json, render_agent_docs_toon, render_context_response_json,
        render_context_response_toon, render_doctor_json, render_doctor_toon, render_health_json,
        render_health_toon, render_learn_experiment_proposal_human,
        render_learn_experiment_proposal_json, render_learn_experiment_proposal_toon,
        render_shadow_run_human, render_shadow_run_json, render_shadow_run_toon,
        render_status_json, render_status_json_filtered, render_status_toon, render_version_json,
        status_response_json,
    };
    use crate::core::agent_docs::AgentDocsReport;
    use crate::core::doctor::DoctorReport;
    use crate::core::health::HealthReport;
    use crate::core::learn::{
        ExperimentBudget, ExperimentDecisionImpact, ExperimentProposal, ExperimentSafetyPlan,
        LEARN_EXPERIMENT_PROPOSAL_SCHEMA_V1, LearnExperimentProposalReport,
    };
    use crate::core::status::StatusReport;
    use crate::core::{
        BUILD_TIMESTAMP_POLICY, BuildFeature, BuildInfo, BuildProvenanceDegradation,
        SupportedSchema, VERSION_PROVENANCE_SCHEMA_V1, VersionReport,
    };
    use crate::models::decision::{DecisionPlane, DecisionPlaneMetadata, DecisionRecord};
    use crate::models::{
        DomainError, ERROR_SCHEMA_V1, MemoryId, ProvenanceUri, RESPONSE_SCHEMA_V1, TrustClass,
        UnitScore,
    };
    use crate::pack::{
        ContextRequest, ContextResponse, PackCandidate, PackCandidateInput, PackProvenance,
        PackSection, PackTrustSignal, TokenBudget, assemble_draft,
    };

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
        ensure(
            haystack.contains(needle),
            format!("{context}: expected output to contain {needle:?}, got {haystack:?}"),
        )
    }

    fn ensure_starts_with(haystack: &str, prefix: &str, context: &str) -> TestResult {
        ensure(
            haystack.starts_with(prefix),
            format!("{context}: expected output to start with {prefix:?}, got {haystack:?}"),
        )
    }

    fn memory_id(seed: u128) -> MemoryId {
        MemoryId::from_uuid(Uuid::from_u128(seed))
    }

    fn score(value: f32) -> Result<UnitScore, String> {
        UnitScore::parse(value).map_err(|error| format!("test score rejected: {error:?}"))
    }

    fn pack_provenance(uri: &str) -> Result<PackProvenance, String> {
        let uri = ProvenanceUri::from_str(uri)
            .map_err(|error| format!("test provenance URI rejected: {error:?}"))?;
        PackProvenance::new(uri, "source evidence")
            .map_err(|error| format!("test provenance rejected: {error:?}"))
    }

    fn output_context_from_env(environment: OutputEnvironment) -> OutputContext {
        OutputContext::detect_with_environment(false, false, None, false, &environment)
    }

    fn context_response_fixture() -> Result<ContextResponse, String> {
        let request = ContextRequest::from_query("prepare release")
            .map_err(|error| format!("request rejected: {error:?}"))?;
        let budget =
            TokenBudget::new(100).map_err(|error| format!("budget rejected: {error:?}"))?;
        let candidate = PackCandidate::new(PackCandidateInput {
            memory_id: memory_id(42),
            section: PackSection::ProceduralRules,
            content: "Run cargo fmt --check before release.".to_string(),
            estimated_tokens: 10,
            relevance: score(0.8)?,
            utility: score(0.6)?,
            provenance: vec![pack_provenance("file://AGENTS.md#L42")?],
            why: "selected because release checks match the task".to_string(),
        })
        .map(|candidate| {
            candidate.with_trust_signal(PackTrustSignal::new(
                TrustClass::HumanExplicit,
                Some("project-rule".to_string()),
            ))
        })
        .map_err(|error| format!("candidate rejected: {error:?}"))?;
        let draft = assemble_draft(&request.query, budget, vec![candidate])
            .map_err(|error| format!("draft rejected: {error:?}"))?;
        ContextResponse::new(request, draft, Vec::new())
            .map_err(|error| format!("response rejected: {error:?}"))
    }

    fn learn_experiment_proposal_fixture() -> LearnExperimentProposalReport {
        LearnExperimentProposalReport {
            schema: LEARN_EXPERIMENT_PROPOSAL_SCHEMA_V1.to_string(),
            total_candidates: 3,
            returned: 1,
            min_expected_value: 0.3,
            max_attention_tokens: 800,
            max_runtime_seconds: 180,
            generated_at: "2026-01-02T03:04:05Z".to_string(),
            proposals: vec![ExperimentProposal {
                experiment_id: "exp_renderer_fixture".to_string(),
                question_id: "gap_renderer_fixture".to_string(),
                title: "Render experiment proposal contract".to_string(),
                hypothesis: "A fixed renderer fixture preserves proposal JSON, human, and TOON output without invoking unavailable learn records.".to_string(),
                status: "proposed".to_string(),
                topic: "renderer_contract".to_string(),
                expected_value: 0.539,
                uncertainty_reduction: 0.32,
                confidence: 0.48,
                budget: ExperimentBudget {
                    attention_tokens: 800,
                    max_runtime_seconds: 180,
                    dry_run_required: true,
                    budget_class: "medium".to_string(),
                },
                safety: ExperimentSafetyPlan {
                    boundary: "human_review".to_string(),
                    dry_run_first: true,
                    mutation_allowed: false,
                    review_required: true,
                    stop_conditions: vec![
                        "Stop after renderer output is validated.".to_string(),
                        "Stop before any durable memory mutation.".to_string(),
                    ],
                    denied_reasons: Vec::new(),
                },
                decision_impact: ExperimentDecisionImpact {
                    decision_id: "decision_renderer_fixture".to_string(),
                    target_artifact_ids: vec!["mem_renderer_fixture".to_string()],
                    current_decision: "Keep proposal renderer output stable.".to_string(),
                    possible_change: "Update only renderer contracts when the public shape changes."
                        .to_string(),
                    impact_score: 0.85,
                },
                evidence_ids: vec!["gap_renderer_fixture".to_string()],
                next_command: "ee learn experiment run --dry-run --id exp_renderer_fixture --json"
                    .to_string(),
            }],
        }
    }

    #[test]
    fn learn_experiment_proposal_json_exposes_ev_budget_safety_and_decision() -> TestResult {
        let report = learn_experiment_proposal_fixture();
        let json = render_learn_experiment_proposal_json(&report);
        let value: serde_json::Value =
            serde_json::from_str(&json).map_err(|error| error.to_string())?;

        ensure_contains(&json, "\"expectedValue\"", "expected value field")?;
        ensure_contains(&json, "\"budget\"", "budget field")?;
        ensure_contains(&json, "\"safety\"", "safety field")?;
        ensure_contains(&json, "\"decisionImpact\"", "decision impact field")?;
        ensure(
            value.get("success").and_then(serde_json::Value::as_bool) == Some(true),
            "proposal JSON must be successful",
        )?;
        ensure(
            value
                .get("proposals")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| !items.is_empty()),
            "proposal JSON must include proposals",
        )
    }

    #[test]
    fn learn_experiment_proposal_human_and_toon_are_stable() -> TestResult {
        let report = learn_experiment_proposal_fixture();

        let human = render_learn_experiment_proposal_human(&report);
        ensure_contains(&human, "Learning Experiment Proposals", "human title")?;
        ensure_contains(&human, "Decision impact:", "human decision impact")?;
        ensure_contains(&human, "ee learn experiment run --dry-run", "human next")?;

        let toon = render_learn_experiment_proposal_toon(&report);
        ensure_starts_with(
            &toon,
            "LEARN_EXPERIMENT_PROPOSAL|returned=1|candidates=3",
            "toon prefix",
        )
    }

    fn version_report_fixture(
        git_commit: Option<&'static str>,
        git_tag: Option<&'static str>,
        git_dirty: Option<bool>,
        target_triple: &'static str,
        build_profile: &'static str,
        release_channel: &'static str,
        degradations: Vec<BuildProvenanceDegradation>,
    ) -> VersionReport {
        VersionReport {
            build: BuildInfo {
                package: "ee",
                version: "9.9.9",
                git_commit,
                git_tag,
                git_dirty,
                target_triple,
                target_arch: "x86_64",
                target_os: "linux",
                build_profile,
                release_channel,
                build_timestamp_policy: BUILD_TIMESTAMP_POLICY,
                min_db_migration: 1,
                max_db_migration: 14,
            },
            features: vec![
                BuildFeature::new("fts5", true),
                BuildFeature::new("json", true),
                BuildFeature::new("mcp", false),
                BuildFeature::new("serve", false),
            ],
            schemas: vec![
                SupportedSchema::new("response", RESPONSE_SCHEMA_V1),
                SupportedSchema::new("error", ERROR_SCHEMA_V1),
                SupportedSchema::new("version_provenance", VERSION_PROVENANCE_SCHEMA_V1),
            ],
            degradations,
        }
    }

    #[test]
    fn version_json_release_clean_fixture_has_full_provenance() -> TestResult {
        let report = version_report_fixture(
            Some("abcdef123456"),
            Some("v9.9.9"),
            Some(false),
            "x86_64-unknown-linux-gnu",
            "release",
            "stable",
            Vec::new(),
        );
        let json = render_version_json(&report);

        ensure_starts_with(&json, "{\"schema\":\"ee.response.v1\"", "response schema")?;
        ensure_contains(&json, "\"command\":\"version\"", "command")?;
        ensure_contains(
            &json,
            "\"schema\":\"ee.version.provenance.v1\"",
            "version schema",
        )?;
        ensure_contains(&json, "\"releaseChannel\":\"stable\"", "release channel")?;
        ensure_contains(&json, "\"gitCommit\":\"abcdef123456\"", "git commit")?;
        ensure_contains(&json, "\"gitTag\":\"v9.9.9\"", "git tag")?;
        ensure_contains(&json, "\"gitDirty\":false", "clean source")?;
        ensure_contains(&json, "\"state\":\"clean\"", "clean state")?;
        ensure_contains(&json, "\"profile\":\"release\"", "release profile")?;
        ensure_contains(
            &json,
            "\"targetTriple\":\"x86_64-unknown-linux-gnu\"",
            "target triple",
        )?;
        ensure_contains(
            &json,
            "\"timestampPolicy\":\"omitted_for_reproducibility\",\"timestamp\":null",
            "timestamp policy",
        )?;
        ensure_contains(&json, "\"available\":true", "available provenance")
    }

    #[test]
    fn version_json_dirty_fixture_reports_dirty_source() -> TestResult {
        let report = version_report_fixture(
            Some("abcdef123456"),
            Some("v9.9.9-dirty"),
            Some(true),
            "x86_64-unknown-linux-gnu",
            "debug",
            "dev",
            Vec::new(),
        );
        let json = render_version_json(&report);

        ensure_contains(&json, "\"gitDirty\":true", "dirty flag")?;
        ensure_contains(&json, "\"state\":\"dirty\"", "dirty state")?;
        ensure_contains(&json, "\"releaseChannel\":\"dev\"", "dev channel")
    }

    #[test]
    fn version_json_missing_metadata_fixture_reports_degradations() -> TestResult {
        let report = version_report_fixture(
            None,
            None,
            None,
            "unknown",
            "debug",
            "dev",
            vec![
                BuildProvenanceDegradation::new(
                    "git_metadata_unavailable",
                    "low",
                    "Git source metadata was not provided by the build.",
                    "Build with VERGEN_GIT_SHA, VERGEN_GIT_DESCRIBE, and VERGEN_GIT_DIRTY set.",
                ),
                BuildProvenanceDegradation::new(
                    "target_triple_unavailable",
                    "low",
                    "Target triple was not provided by the build.",
                    "Build with EE_BUILD_TARGET set to the target triple.",
                ),
            ],
        );
        let json = render_version_json(&report);

        ensure_contains(&json, "\"gitCommit\":null", "missing commit")?;
        ensure_contains(&json, "\"gitTag\":null", "missing tag")?;
        ensure_contains(&json, "\"gitDirty\":null", "missing dirty flag")?;
        ensure_contains(&json, "\"state\":\"unavailable\"", "unavailable state")?;
        ensure_contains(&json, "\"available\":false", "degraded provenance")?;
        ensure_contains(
            &json,
            "\"code\":\"git_metadata_unavailable\"",
            "git degradation",
        )?;
        ensure_contains(
            &json,
            "\"code\":\"target_triple_unavailable\"",
            "target degradation",
        )
    }

    #[test]
    fn version_json_feature_and_db_contracts_are_stable() -> TestResult {
        let report = version_report_fixture(
            Some("abcdef123456"),
            Some("v9.9.9"),
            Some(false),
            "x86_64-unknown-linux-gnu",
            "release",
            "stable",
            Vec::new(),
        );
        let json = render_version_json(&report);

        ensure_contains(
            &json,
            "\"features\":[{\"name\":\"fts5\",\"enabled\":true},{\"name\":\"json\",\"enabled\":true},{\"name\":\"mcp\",\"enabled\":false},{\"name\":\"serve\",\"enabled\":false}]",
            "feature order and adapter flags",
        )?;
        ensure_contains(
            &json,
            "\"supportedMigrationRange\":{\"min\":1,\"max\":14}",
            "database range",
        )?;
        ensure_contains(
            &json,
            "\"compatibility\":\"unknown_without_workspace\"",
            "workspace compatibility state",
        )?;
        let assignment_needle = concat!("TOKEN", "=");
        ensure(
            !json.contains("/tmp/")
                && !json.contains(assignment_needle)
                && !json.contains("ubuntu"),
            "version JSON must not leak build paths, usernames, or assignment-like secrets",
        )
    }

    #[test]
    fn status_json_has_stable_schema_and_degradation_codes() -> TestResult {
        let json = status_response_json();
        ensure_starts_with(&json, "{\"schema\":\"ee.response.v1\"", "status schema")?;
        ensure_contains(&json, "\"success\":true", "status success flag")?;
        ensure_contains(&json, "\"runtime\":\"ready\"", "status runtime capability")?;
        ensure_contains(&json, "\"engine\":\"asupersync\"", "status runtime engine")?;
        ensure_contains(
            &json,
            "\"profile\":\"current_thread\"",
            "status runtime profile",
        )?;
        ensure_contains(
            &json,
            "\"storage_not_implemented\"",
            "status storage degradation",
        )?;
        ensure_contains(
            &json,
            "\"search_not_implemented\"",
            "status search degradation",
        )?;
        ensure_contains(&json, "\"derivedAssets\":[", "derived assets")?;
        ensure_contains(&json, "\"name\":\"search_index\"", "search index asset")?;
        ensure_contains(
            &json,
            "\"assetHighWatermark\":null",
            "asset watermark field",
        )
    }

    #[test]
    fn human_status_is_not_json() -> TestResult {
        let status = human_status();
        ensure_starts_with(&status, "ee status", "human status heading")?;
        ensure(!status.starts_with('{'), "human status must not be JSON")
    }

    #[test]
    fn help_mentions_supported_skeleton_commands() -> TestResult {
        let help = help_text();
        ensure_contains(help, "ee status [--json]", "help status command")?;
        ensure_contains(help, "ee --version", "help version command")
    }

    #[test]
    fn error_json_has_stable_schema_and_code() -> TestResult {
        let error = DomainError::Usage {
            message: "unrecognized subcommand 'foo'".to_string(),
            repair: Some("ee --help".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "error schema")?;
        ensure_contains(&json, "\"code\":\"usage\"", "error code")?;
        ensure_contains(
            &json,
            "\"message\":\"unrecognized subcommand 'foo'\"",
            "error message",
        )?;
        ensure_contains(&json, "\"repair\":\"ee --help\"", "error repair")
    }

    #[test]
    fn error_json_without_repair_omits_field() -> TestResult {
        let error = DomainError::Storage {
            message: "Database locked".to_string(),
            repair: None,
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "error schema")?;
        ensure_contains(&json, "\"code\":\"storage\"", "error code")?;
        ensure(!json.contains("repair"), "repair field should be absent")
    }

    #[test]
    fn escape_json_handles_special_chars() -> TestResult {
        let escaped = escape_json_string("line1\nline2\ttab\"quote\\backslash");
        ensure_contains(&escaped, "\\n", "newline escape")?;
        ensure_contains(&escaped, "\\t", "tab escape")?;
        ensure_contains(&escaped, "\\\"", "quote escape")?;
        ensure_contains(&escaped, "\\\\", "backslash escape")
    }

    #[test]
    fn json_builder_constructs_simple_object() -> TestResult {
        let mut b = JsonBuilder::new();
        b.field_str("name", "test");
        b.field_bool("active", true);
        b.field_u32("count", 42);
        let json = b.finish();
        ensure_contains(&json, "\"name\":\"test\"", "string field")?;
        ensure_contains(&json, "\"active\":true", "bool field")?;
        ensure_contains(&json, "\"count\":42", "u32 field")?;
        ensure(
            json.starts_with('{') && json.ends_with('}'),
            "valid JSON object",
        )
    }

    #[test]
    fn json_builder_escapes_string_values() -> TestResult {
        let mut b = JsonBuilder::new();
        b.field_str("message", "line1\nline2");
        let json = b.finish();
        ensure_contains(&json, "\"message\":\"line1\\nline2\"", "escaped newline")
    }

    #[test]
    fn json_builder_supports_nested_objects() -> TestResult {
        let mut b = JsonBuilder::new();
        b.field_str("schema", "test.v1");
        b.field_object("data", |obj| {
            obj.field_str("inner", "value");
        });
        let json = b.finish();
        ensure_contains(&json, "\"schema\":\"test.v1\"", "outer field")?;
        ensure_contains(&json, "\"data\":{\"inner\":\"value\"}", "nested object")
    }

    #[test]
    fn json_builder_supports_array_of_objects() -> TestResult {
        let items = vec![("a", 1u32), ("b", 2u32)];
        let mut b = JsonBuilder::new();
        b.field_array_of_objects("items", &items, |obj, (name, val)| {
            obj.field_str("name", name);
            obj.field_u32("value", *val);
        });
        let json = b.finish();
        ensure_contains(&json, "\"items\":[", "array start")?;
        ensure_contains(&json, "{\"name\":\"a\",\"value\":1}", "first item")?;
        ensure_contains(&json, "{\"name\":\"b\",\"value\":2}", "second item")
    }

    #[test]
    fn json_builder_raw_field_allows_prebuilt_json() -> TestResult {
        let mut b = JsonBuilder::new();
        b.field_raw("config", "[1,2,3]");
        let json = b.finish();
        ensure_contains(&json, "\"config\":[1,2,3]", "raw json array")
    }

    #[test]
    fn renderer_wire_names_are_stable() -> TestResult {
        ensure_equal(&Renderer::Human.as_str(), &"human", "human")?;
        ensure_equal(&Renderer::Json.as_str(), &"json", "json")?;
        ensure_equal(&Renderer::Toon.as_str(), &"toon", "toon")?;
        ensure_equal(&Renderer::Jsonl.as_str(), &"jsonl", "jsonl")?;
        ensure_equal(&Renderer::Compact.as_str(), &"compact", "compact")?;
        ensure_equal(&Renderer::Hook.as_str(), &"hook", "hook")
    }

    fn ensure_equal<T: std::fmt::Debug + PartialEq>(
        actual: &T,
        expected: &T,
        ctx: &str,
    ) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn renderer_machine_readable_classification() -> TestResult {
        ensure(
            !Renderer::Human.is_machine_readable(),
            "human is not machine",
        )?;
        ensure(!Renderer::Toon.is_machine_readable(), "toon is not machine")?;
        ensure(Renderer::Json.is_machine_readable(), "json is machine")?;
        ensure(Renderer::Jsonl.is_machine_readable(), "jsonl is machine")?;
        ensure(
            Renderer::Compact.is_machine_readable(),
            "compact is machine",
        )?;
        ensure(Renderer::Hook.is_machine_readable(), "hook is machine")
    }

    #[test]
    fn output_context_json_flag_forces_json() -> TestResult {
        let ctx = OutputContext::detect_with_hints(true, false, None);
        ensure_equal(&ctx.renderer, &Renderer::Json, "json flag")
    }

    #[test]
    fn output_context_robot_flag_forces_json() -> TestResult {
        let ctx = OutputContext::detect_with_hints(false, true, None);
        ensure_equal(&ctx.renderer, &Renderer::Json, "robot flag")
    }

    #[test]
    fn output_context_format_override_takes_precedence() -> TestResult {
        let ctx = OutputContext::detect_with_hints(true, true, Some(Renderer::Toon));
        ensure_equal(&ctx.renderer, &Renderer::Toon, "format override")
    }

    #[test]
    fn output_context_ee_json_forces_json_over_toon_default() -> TestResult {
        let ctx = output_context_from_env(OutputEnvironment {
            ee_json: Some("1".to_string()),
            toon_default_format: Some("toon".to_string()),
            ..OutputEnvironment::default()
        });
        ensure_equal(&ctx.renderer, &Renderer::Json, "EE_JSON precedence")
    }

    #[test]
    fn output_context_agent_mode_forces_json_over_output_format() -> TestResult {
        let ctx = output_context_from_env(OutputEnvironment {
            ee_agent_mode: Some("true".to_string()),
            ee_output_format: Some("toon".to_string()),
            ..OutputEnvironment::default()
        });
        ensure_equal(&ctx.renderer, &Renderer::Json, "EE_AGENT_MODE precedence")
    }

    #[test]
    fn output_context_hook_mode_precedes_toon_default() -> TestResult {
        let ctx = output_context_from_env(OutputEnvironment {
            ee_hook_mode: Some("yes".to_string()),
            toon_default_format: Some("toon".to_string()),
            ..OutputEnvironment::default()
        });
        ensure_equal(&ctx.renderer, &Renderer::Hook, "EE_HOOK_MODE precedence")
    }

    #[test]
    fn output_context_ee_output_format_precedes_toon_default() -> TestResult {
        let ctx = output_context_from_env(OutputEnvironment {
            ee_output_format: Some("jsonl".to_string()),
            toon_default_format: Some("toon".to_string()),
            ..OutputEnvironment::default()
        });
        ensure_equal(
            &ctx.renderer,
            &Renderer::Jsonl,
            "EE_OUTPUT_FORMAT precedence",
        )
    }

    #[test]
    fn output_context_legacy_ee_format_precedes_toon_default() -> TestResult {
        let ctx = output_context_from_env(OutputEnvironment {
            ee_format: Some("compact".to_string()),
            toon_default_format: Some("toon".to_string()),
            ..OutputEnvironment::default()
        });
        ensure_equal(&ctx.renderer, &Renderer::Compact, "EE_FORMAT precedence")
    }

    #[test]
    fn output_context_toon_default_format_applies_as_fallback() -> TestResult {
        let ctx = output_context_from_env(OutputEnvironment {
            toon_default_format: Some("toon".to_string()),
            ..OutputEnvironment::default()
        });
        ensure_equal(
            &ctx.renderer,
            &Renderer::Toon,
            "TOON_DEFAULT_FORMAT fallback",
        )
    }

    // EE-336: TOON_DEFAULT_FORMAT precedence tests proving explicit machine
    // output flags always override the fallback environment variable.

    #[test]
    fn toon_default_format_json_flag_forces_json() -> TestResult {
        let ctx = OutputContext::detect_with_environment(
            true,
            false,
            None,
            false,
            &OutputEnvironment {
                toon_default_format: Some("toon".to_string()),
                ..OutputEnvironment::default()
            },
        );
        ensure_equal(
            &ctx.renderer,
            &Renderer::Json,
            "--json flag must override TOON_DEFAULT_FORMAT",
        )
    }

    #[test]
    fn toon_default_format_robot_flag_forces_json() -> TestResult {
        let ctx = OutputContext::detect_with_environment(
            false,
            true,
            None,
            false,
            &OutputEnvironment {
                toon_default_format: Some("toon".to_string()),
                ..OutputEnvironment::default()
            },
        );
        ensure_equal(
            &ctx.renderer,
            &Renderer::Json,
            "--robot flag must override TOON_DEFAULT_FORMAT",
        )
    }

    #[test]
    fn toon_default_format_format_json_override_forces_json() -> TestResult {
        let ctx = OutputContext::detect_with_environment(
            false,
            false,
            Some(Renderer::Json),
            false,
            &OutputEnvironment {
                toon_default_format: Some("toon".to_string()),
                ..OutputEnvironment::default()
            },
        );
        ensure_equal(
            &ctx.renderer,
            &Renderer::Json,
            "--format json must override TOON_DEFAULT_FORMAT",
        )
    }

    #[test]
    fn toon_default_format_hook_mode_stays_hook() -> TestResult {
        let ctx = OutputContext::detect_with_environment(
            false,
            false,
            None,
            false,
            &OutputEnvironment {
                ee_hook_mode: Some("1".to_string()),
                toon_default_format: Some("toon".to_string()),
                ..OutputEnvironment::default()
            },
        );
        ensure_equal(
            &ctx.renderer,
            &Renderer::Hook,
            "EE_HOOK_MODE must override TOON_DEFAULT_FORMAT",
        )
    }

    #[test]
    fn toon_default_format_mcp_agent_mode_stays_json() -> TestResult {
        let ctx = OutputContext::detect_with_environment(
            false,
            false,
            None,
            false,
            &OutputEnvironment {
                ee_agent_mode: Some("1".to_string()),
                toon_default_format: Some("toon".to_string()),
                ..OutputEnvironment::default()
            },
        );
        ensure_equal(
            &ctx.renderer,
            &Renderer::Json,
            "EE_AGENT_MODE (MCP) must override TOON_DEFAULT_FORMAT",
        )
    }

    #[test]
    fn output_context_falsey_env_flags_do_not_force_machine_output() -> TestResult {
        let ctx = output_context_from_env(OutputEnvironment {
            ee_json: Some("0".to_string()),
            ee_agent_mode: Some("false".to_string()),
            ee_hook_mode: Some("off".to_string()),
            ..OutputEnvironment::default()
        });
        ensure_equal(&ctx.renderer, &Renderer::Human, "falsey env flags")
    }

    #[test]
    fn output_context_no_color_wins_over_force_color() -> TestResult {
        let ctx = OutputContext::detect_with_environment(
            false,
            false,
            None,
            false,
            &OutputEnvironment {
                no_color: Some("".to_string()),
                force_color: Some("1".to_string()),
                ..OutputEnvironment::default()
            },
        );
        ensure(!ctx.color_enabled, "NO_COLOR must disable color")
    }

    #[test]
    fn output_context_force_color_enables_human_color_without_tty() -> TestResult {
        let ctx = OutputContext::detect_with_environment(
            false,
            false,
            None,
            false,
            &OutputEnvironment {
                force_color: Some("1".to_string()),
                ..OutputEnvironment::default()
            },
        );
        ensure(ctx.color_enabled, "FORCE_COLOR enables human color")
    }

    #[test]
    fn output_context_force_color_does_not_color_machine_output() -> TestResult {
        let ctx = OutputContext::detect_with_environment(
            false,
            false,
            None,
            false,
            &OutputEnvironment {
                ee_output_format: Some("json".to_string()),
                force_color: Some("1".to_string()),
                ..OutputEnvironment::default()
            },
        );
        ensure_equal(&ctx.renderer, &Renderer::Json, "machine renderer")?;
        ensure(!ctx.color_enabled, "machine output stays uncolored")
    }

    #[test]
    fn response_envelope_success_has_stable_schema() -> TestResult {
        let json = ResponseEnvelope::success()
            .data(|d| {
                d.field_str("command", "test");
            })
            .finish();
        ensure_starts_with(&json, "{\"schema\":\"ee.response.v1\"", "schema")?;
        ensure_contains(&json, "\"success\":true", "success flag")?;
        ensure_contains(&json, "\"data\":{\"command\":\"test\"}", "data object")
    }

    #[test]
    fn response_envelope_failure_has_success_false() -> TestResult {
        let json = ResponseEnvelope::failure()
            .data_raw("{\"error\":\"something\"}")
            .finish();
        ensure_contains(&json, "\"success\":false", "failure flag")?;
        ensure_contains(&json, "\"data\":{\"error\":\"something\"}", "data raw")
    }

    #[test]
    fn response_envelope_degraded_array() -> TestResult {
        let degradations = vec![("code1", "message1")];
        let json = ResponseEnvelope::success()
            .data(|d| {
                d.field_str("status", "ok");
            })
            .degraded_array(&degradations, |obj, (code, msg)| {
                obj.field_str("code", code);
                obj.field_str("message", msg);
            })
            .finish();
        ensure_contains(&json, "\"degraded\":[{", "degraded array start")?;
        ensure_contains(&json, "\"code\":\"code1\"", "degradation code")
    }

    #[test]
    fn context_response_json_renders_provenance() -> TestResult {
        let response = context_response_fixture()?;
        let json = render_context_response_json(&response);

        ensure_starts_with(&json, "{\"schema\":\"ee.response.v1\"", "schema")?;
        ensure_contains(&json, "\"command\":\"context\"", "command")?;
        ensure_contains(
            &json,
            "\"provenance\":[{\"uri\":\"file://AGENTS.md#L42\",\"scheme\":\"file\",\"label\":\"AGENTS.md:L42\",\"locator\":\"L42\",\"note\":\"source evidence\"}]",
            "item provenance",
        )?;
        ensure_contains(
            &json,
            "\"provenanceFooter\":{\"memoryCount\":1,\"sourceCount\":1,\"schemes\":[\"file\"],\"entries\":[",
            "provenance footer",
        )?;
        ensure_contains(
            &json,
            "\"advisoryBanner\":{\"status\":\"clear\"",
            "advisory banner",
        )?;
        ensure_contains(
            &json,
            "\"trust\":{\"class\":\"human_explicit\",\"subclass\":\"project-rule\",\"posture\":\"authoritative\"}",
            "item trust posture",
        )?;
        ensure_contains(&json, "\"relevance\":0.800000", "stable relevance")
    }

    #[test]
    fn context_response_json_renders_pack_quality() -> TestResult {
        let response = context_response_fixture()?;
        let json = render_context_response_json(&response);

        ensure_contains(
            &json,
            "\"quality\":{\"itemCount\":1,\"omittedCount\":0,\"usedTokens\":10,\"maxTokens\":100,\"budgetUtilization\":0.100000",
            "quality metric header",
        )?;
        ensure_contains(
            &json,
            "\"averageRelevance\":0.800000,\"averageUtility\":0.600000",
            "quality score averages",
        )?;
        ensure_contains(
            &json,
            "\"provenanceSourceCount\":1,\"provenanceSourcesPerItem\":1.000000,\"provenanceComplete\":true",
            "quality provenance density",
        )?;
        ensure_contains(
            &json,
            "\"sections\":[{\"section\":\"procedural_rules\",\"itemCount\":1,\"usedTokens\":10},{\"section\":\"decisions\",\"itemCount\":0,\"usedTokens\":0}",
            "quality section metrics",
        )?;
        ensure_contains(
            &json,
            "\"omissions\":{\"tokenBudgetExceeded\":0,\"redundantCandidates\":0}",
            "quality omission metrics",
        )
    }

    #[test]
    fn degradation_severity_strings_are_stable() -> TestResult {
        ensure_equal(&DegradationSeverity::Low.as_str(), &"low", "low")?;
        ensure_equal(&DegradationSeverity::Medium.as_str(), &"medium", "medium")?;
        ensure_equal(&DegradationSeverity::High.as_str(), &"high", "high")
    }

    #[test]
    fn degradation_to_json_has_stable_structure() -> TestResult {
        let d = Degradation::new(
            "storage_stale",
            DegradationSeverity::Medium,
            "Storage index is stale.",
            "ee index rebuild",
        );
        let json = d.to_json();
        ensure_contains(&json, "\"code\":\"storage_stale\"", "code field")?;
        ensure_contains(&json, "\"severity\":\"medium\"", "severity field")?;
        ensure_contains(
            &json,
            "\"message\":\"Storage index is stale.\"",
            "message field",
        )?;
        ensure_contains(&json, "\"repair\":\"ee index rebuild\"", "repair field")
    }

    // ========================================================================
    // Error JSON Schema Tests (EE-015)
    //
    // These tests verify the ee.error.v1 JSON schema contract for all
    // DomainError variants. Each error type must produce valid JSON with:
    // - schema: "ee.error.v1"
    // - error.code: stable string matching the error variant
    // - error.message: human-readable description
    // - error.repair: optional remediation command (present when provided)
    // ========================================================================

    #[test]
    fn error_schema_usage_has_stable_structure() -> TestResult {
        let error = DomainError::Usage {
            message: "Unknown command 'xyz'.".to_string(),
            repair: Some("ee --help".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"usage\"", "code")?;
        ensure_contains(&json, "\"message\":\"Unknown command 'xyz'.\"", "message")?;
        ensure_contains(&json, "\"repair\":\"ee --help\"", "repair")
    }

    #[test]
    fn error_schema_configuration_has_stable_structure() -> TestResult {
        let error = DomainError::Configuration {
            message: "Invalid config file format.".to_string(),
            repair: Some("ee doctor --fix-plan --json".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"configuration\"", "code")?;
        ensure_contains(
            &json,
            "\"message\":\"Invalid config file format.\"",
            "message",
        )?;
        ensure_contains(
            &json,
            "\"repair\":\"ee doctor --fix-plan --json\"",
            "repair",
        )
    }

    #[test]
    fn error_schema_storage_has_stable_structure() -> TestResult {
        let error = DomainError::Storage {
            message: "Database file corrupted.".to_string(),
            repair: Some("ee doctor --fix-plan --json".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"storage\"", "code")?;
        ensure_contains(&json, "\"message\":\"Database file corrupted.\"", "message")?;
        ensure_contains(
            &json,
            "\"repair\":\"ee doctor --fix-plan --json\"",
            "repair",
        )
    }

    #[test]
    fn error_schema_search_index_has_stable_structure() -> TestResult {
        let error = DomainError::SearchIndex {
            message: "Index is stale (generation 9, database generation 12).".to_string(),
            repair: Some("ee index rebuild".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"search_index\"", "code")?;
        ensure_contains(&json, "generation 9", "message contains details")?;
        ensure_contains(&json, "\"repair\":\"ee index rebuild\"", "repair")
    }

    #[test]
    fn error_schema_import_has_stable_structure() -> TestResult {
        let error = DomainError::Import {
            message: "CASS session file not found.".to_string(),
            repair: Some("ee import cass --dry-run".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"import\"", "code")?;
        ensure_contains(
            &json,
            "\"message\":\"CASS session file not found.\"",
            "message",
        )?;
        ensure_contains(&json, "\"repair\":\"ee import cass --dry-run\"", "repair")
    }

    #[test]
    fn error_schema_unsatisfied_degraded_mode_has_stable_structure() -> TestResult {
        let error = DomainError::UnsatisfiedDegradedMode {
            message: "Semantic search unimplemented and --require-semantic was set.".to_string(),
            repair: Some("ee index reembed --dry-run".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"unsatisfied_degraded_mode\"", "code")?;
        ensure_contains(&json, "--require-semantic", "message contains flag")?;
        ensure_contains(&json, "\"repair\":\"ee index reembed --dry-run\"", "repair")
    }

    #[test]
    fn error_schema_policy_denied_has_stable_structure() -> TestResult {
        let error = DomainError::PolicyDenied {
            message: "Redaction policy prevents this operation.".to_string(),
            repair: Some("ee doctor --fix-plan --json".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"policy_denied\"", "code")?;
        ensure_contains(
            &json,
            "\"message\":\"Redaction policy prevents this operation.\"",
            "message",
        )?;
        ensure_contains(
            &json,
            "\"repair\":\"ee doctor --fix-plan --json\"",
            "repair",
        )
    }

    #[test]
    fn error_schema_migration_required_has_stable_structure() -> TestResult {
        let error = DomainError::MigrationRequired {
            message: "Database schema version 3 requires migration to version 5.".to_string(),
            repair: Some("ee init --workspace .".to_string()),
        };
        let json = error_response_json(&error);
        ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", "schema")?;
        ensure_contains(&json, "\"code\":\"migration_required\"", "code")?;
        ensure_contains(&json, "version 3", "message contains version")?;
        ensure_contains(&json, "\"repair\":\"ee init --workspace .\"", "repair")
    }

    #[test]
    fn error_schema_all_codes_are_covered() -> TestResult {
        // Ensure we have tests for all 8 error types
        let codes = [
            "usage",
            "configuration",
            "storage",
            "search_index",
            "import",
            "unsatisfied_degraded_mode",
            "policy_denied",
            "migration_required",
        ];

        // Verify each code produces valid JSON
        let errors = [
            DomainError::Usage {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::Configuration {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::Storage {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::SearchIndex {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::Import {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::UnsatisfiedDegradedMode {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::PolicyDenied {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::MigrationRequired {
                message: "test".to_string(),
                repair: None,
            },
        ];

        for (error, expected_code) in errors.iter().zip(codes.iter()) {
            let json = error_response_json(error);
            ensure_starts_with(&json, "{\"schema\":\"ee.error.v1\"", expected_code)?;
            ensure_contains(
                &json,
                &format!("\"code\":\"{expected_code}\""),
                expected_code,
            )?;
        }
        Ok(())
    }

    #[test]
    fn error_schema_without_repair_omits_field() -> TestResult {
        // Verify that when repair is None, the field is absent (not null)
        for error in [
            DomainError::Usage {
                message: "test".to_string(),
                repair: None,
            },
            DomainError::Storage {
                message: "test".to_string(),
                repair: None,
            },
        ] {
            let json = error_response_json(&error);
            ensure(
                !json.contains("repair"),
                format!("{}: repair field should be absent when None", error.code()),
            )?;
        }
        Ok(())
    }

    // ========================================================================
    // TOON Output Tests (EE-036)
    //
    // TOON is rendered from the canonical JSON envelope through /dp/toon_rust.
    // These tests prove the public renderer is valid TOON and semantically
    // equivalent to the JSON status output.
    // ========================================================================

    #[test]
    fn toon_status_has_required_fields() -> TestResult {
        let report = StatusReport::gather();
        let toon = render_status_toon(&report);
        ensure_contains(&toon, "schema: ee.response.v1", "toon schema")?;
        ensure_contains(&toon, "success: true", "toon success")?;
        ensure_contains(&toon, "command: status", "toon command")?;
        ensure_contains(&toon, "capabilities:", "toon capabilities section")?;
        ensure_contains(&toon, "runtime:", "toon runtime section")?;
        ensure_contains(&toon, "derivedAssets", "toon derived assets section")?;
        ensure_contains(&toon, "engine: asupersync", "toon engine")
    }

    #[test]
    fn toon_status_has_degradation_details() -> TestResult {
        let report = StatusReport::gather();
        let toon = render_status_toon(&report);
        ensure_contains(
            &toon,
            "degraded[3]{code,severity,message}:",
            "degradation section",
        )?;
        ensure_contains(&toon, "storage_not_implemented", "storage degradation code")?;
        ensure_contains(&toon, "search_not_implemented", "search degradation code")?;
        ensure_contains(
            &toon,
            "memory_health_unavailable",
            "memory health degradation code",
        )
    }

    #[test]
    fn json_toon_parity_status_decodes_to_same_json() -> TestResult {
        let report = StatusReport::gather();
        let json = render_status_json_filtered(&report, FieldProfile::Standard);
        let toon = render_status_toon(&report);

        let expected_json = serde_json::from_str::<serde_json::Value>(&json)
            .map_err(|error| format!("status JSON should parse: {error}"))?;
        let expected = serde_json::Value::from(toon::JsonValue::from(expected_json));
        let decoded = toon::try_decode(&toon, None)
            .map_err(|error| format!("status TOON should decode: {error}"))?;
        let actual = serde_json::Value::from(decoded);

        ensure_equal(&actual, &expected, "decoded TOON matches status JSON")
    }

    #[test]
    fn json_toon_parity_health_decodes_to_same_json() -> TestResult {
        let report = HealthReport::gather();
        let json = render_health_json(&report);
        let toon = render_health_toon(&report);

        let expected_json = serde_json::from_str::<serde_json::Value>(&json)
            .map_err(|error| format!("health JSON should parse: {error}"))?;
        let expected = serde_json::Value::from(toon::JsonValue::from(expected_json));
        let decoded = toon::try_decode(&toon, None)
            .map_err(|error| format!("health TOON should decode: {error}"))?;
        let actual = serde_json::Value::from(decoded);

        ensure_equal(&actual, &expected, "decoded TOON matches health JSON")
    }

    #[test]
    fn json_toon_parity_doctor_decodes_to_same_json() -> TestResult {
        let report = DoctorReport::gather();
        let json = render_doctor_json(&report);
        let toon = render_doctor_toon(&report);

        let expected_json = serde_json::from_str::<serde_json::Value>(&json)
            .map_err(|error| format!("doctor JSON should parse: {error}"))?;
        let expected = serde_json::Value::from(toon::JsonValue::from(expected_json));
        let decoded = toon::try_decode(&toon, None)
            .map_err(|error| format!("doctor TOON should decode: {error}"))?;
        let actual = serde_json::Value::from(decoded);

        ensure_equal(&actual, &expected, "decoded TOON matches doctor JSON")
    }

    #[test]
    fn json_toon_parity_agent_docs_decodes_to_same_json() -> TestResult {
        let report = AgentDocsReport::gather(None);
        let json = render_agent_docs_json(&report);
        let toon = render_agent_docs_toon(&report);

        let expected_json = serde_json::from_str::<serde_json::Value>(&json)
            .map_err(|error| format!("agent-docs JSON should parse: {error}"))?;
        let expected = serde_json::Value::from(toon::JsonValue::from(expected_json));
        let decoded = toon::try_decode(&toon, None)
            .map_err(|error| format!("agent-docs TOON should decode: {error}"))?;
        let actual = serde_json::Value::from(decoded);

        ensure_equal(&actual, &expected, "decoded TOON matches agent-docs JSON")
    }

    #[test]
    fn json_toon_parity_context_decodes_to_same_json() -> TestResult {
        let response = context_response_fixture()?;
        let json = render_context_response_json(&response);
        let toon = render_context_response_toon(&response);

        let expected_json = serde_json::from_str::<serde_json::Value>(&json)
            .map_err(|error| format!("context JSON should parse: {error}"))?;
        let expected = serde_json::Value::from(toon::JsonValue::from(expected_json));
        let decoded = toon::try_decode(&toon, None)
            .map_err(|error| format!("context TOON should decode: {error}"))?;
        let actual = serde_json::Value::from(decoded);

        ensure_equal(&actual, &expected, "decoded TOON matches context JSON")
    }

    #[test]
    fn invalid_json_to_toon_returns_stable_error() -> TestResult {
        let toon = super::render_toon_from_json("{not valid json");
        ensure_contains(&toon, "schema: ee.error.v1", "error schema")?;
        ensure_contains(&toon, "code: toon_encoding_failed", "error code")
    }

    const TOON_STATUS_GOLDEN: &str = include_str!("../../tests/fixtures/golden/toon/status.golden");

    #[test]
    fn toon_status_matches_golden() -> TestResult {
        let report = StatusReport::gather();
        let actual = render_status_toon(&report);

        // Normalize both for comparison (trim trailing whitespace)
        let actual_lines: Vec<&str> = actual.lines().collect();
        let golden_lines: Vec<&str> = TOON_STATUS_GOLDEN.lines().collect();

        if actual_lines.len() != golden_lines.len() {
            return Err(format!(
                "line count mismatch: actual={} golden={}",
                actual_lines.len(),
                golden_lines.len()
            ));
        }

        for (i, (actual_line, golden_line)) in
            actual_lines.iter().zip(golden_lines.iter()).enumerate()
        {
            if actual_line.trim_end() != golden_line.trim_end() {
                return Err(format!(
                    "line {} mismatch:\n  actual:  {:?}\n  golden:  {:?}",
                    i + 1,
                    actual_line,
                    golden_line
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn golden_error_fixtures_are_valid_json() -> TestResult {
        let fixtures = [
            include_str!("../../tests/fixtures/golden/error/usage.golden"),
            include_str!("../../tests/fixtures/golden/error/configuration.golden"),
            include_str!("../../tests/fixtures/golden/error/storage.golden"),
            include_str!("../../tests/fixtures/golden/error/search_index.golden"),
            include_str!("../../tests/fixtures/golden/error/import.golden"),
            include_str!("../../tests/fixtures/golden/error/policy_denied.golden"),
            include_str!("../../tests/fixtures/golden/error/migration_required.golden"),
            include_str!("../../tests/fixtures/golden/error/unsatisfied_degraded_mode.golden"),
            include_str!("../../tests/fixtures/golden/error/no_repair.golden"),
        ];

        for (i, fixture) in fixtures.iter().enumerate() {
            let value: serde_json::Value = serde_json::from_str(fixture)
                .map_err(|e| format!("error fixture {} is not valid JSON: {e}", i))?;
            if value.get("schema") != Some(&serde_json::Value::String("ee.error.v1".to_string())) {
                return Err(format!("error fixture {} missing schema", i));
            }
        }
        Ok(())
    }

    #[test]
    fn golden_status_fixtures_are_valid_json() -> TestResult {
        let fixtures = [
            include_str!("../../tests/fixtures/golden/status/status_healthy.golden"),
            include_str!("../../tests/fixtures/golden/status/status_degraded.golden"),
        ];

        for (i, fixture) in fixtures.iter().enumerate() {
            let value: serde_json::Value = serde_json::from_str(fixture)
                .map_err(|e| format!("status fixture {} is not valid JSON: {e}", i))?;
            if value.get("schema") != Some(&serde_json::Value::String("ee.response.v1".to_string()))
            {
                return Err(format!("status fixture {} missing schema", i));
            }
        }
        Ok(())
    }

    #[test]
    fn golden_version_fixture_is_valid_json() -> TestResult {
        let fixture = include_str!("../../tests/fixtures/golden/version/version.golden");
        let value: serde_json::Value = serde_json::from_str(fixture)
            .map_err(|e| format!("version fixture is not valid JSON: {e}"))?;
        if value.get("schema") != Some(&serde_json::Value::String("ee.response.v1".to_string())) {
            return Err("version fixture missing schema".to_string());
        }
        Ok(())
    }

    #[test]
    fn golden_human_fixtures_have_expected_structure() -> TestResult {
        let error_fixture =
            include_str!("../../tests/fixtures/golden/human/error_with_repair.golden");
        ensure_starts_with(error_fixture, "error:", "human error starts with 'error:'")?;
        ensure_contains(error_fixture, "Next:", "human error has Next section")?;

        let success_fixture =
            include_str!("../../tests/fixtures/golden/human/success_with_summary.golden");
        ensure_contains(success_fixture, "Next:", "human success has Next section")?;
        ensure(
            !success_fixture.starts_with('{'),
            "human output is not JSON",
        )
    }

    // ========================================================================
    // Field Profile Tests (EE-037)
    //
    // These tests verify the --fields filtering behavior for JSON output.
    // Each profile level progressively includes more fields.
    // ========================================================================

    #[test]
    fn field_profile_as_str_is_stable() -> TestResult {
        use super::FieldProfile;
        ensure_equal(&FieldProfile::Minimal.as_str(), &"minimal", "minimal")?;
        ensure_equal(&FieldProfile::Summary.as_str(), &"summary", "summary")?;
        ensure_equal(&FieldProfile::Standard.as_str(), &"standard", "standard")?;
        ensure_equal(&FieldProfile::Full.as_str(), &"full", "full")
    }

    #[test]
    fn field_profile_inclusion_rules() -> TestResult {
        use super::FieldProfile;

        // Minimal: no arrays, no summary metrics, no verbose
        ensure(!FieldProfile::Minimal.include_arrays(), "minimal no arrays")?;
        ensure(
            !FieldProfile::Minimal.include_summary_metrics(),
            "minimal no summary",
        )?;
        ensure(
            !FieldProfile::Minimal.include_verbose_details(),
            "minimal no verbose",
        )?;

        // Summary: no arrays, has summary metrics, no verbose
        ensure(!FieldProfile::Summary.include_arrays(), "summary no arrays")?;
        ensure(
            FieldProfile::Summary.include_summary_metrics(),
            "summary has summary",
        )?;
        ensure(
            !FieldProfile::Summary.include_verbose_details(),
            "summary no verbose",
        )?;

        // Standard: has arrays, has summary metrics, no verbose
        ensure(
            FieldProfile::Standard.include_arrays(),
            "standard has arrays",
        )?;
        ensure(
            FieldProfile::Standard.include_summary_metrics(),
            "standard has summary",
        )?;
        ensure(
            !FieldProfile::Standard.include_verbose_details(),
            "standard no verbose",
        )?;

        // Full: has everything
        ensure(FieldProfile::Full.include_arrays(), "full has arrays")?;
        ensure(
            FieldProfile::Full.include_summary_metrics(),
            "full has summary",
        )?;
        ensure(
            FieldProfile::Full.include_verbose_details(),
            "full has verbose",
        )
    }

    #[test]
    fn render_status_json_filtered_minimal_has_only_essentials() -> TestResult {
        use super::{FieldProfile, render_status_json_filtered};
        let report = StatusReport::gather();
        let json = render_status_json_filtered(&report, FieldProfile::Minimal);

        ensure_contains(&json, "\"schema\":\"ee.response.v1\"", "schema")?;
        ensure_contains(&json, "\"success\":true", "success")?;
        ensure_contains(&json, "\"fields\":\"minimal\"", "fields indicator")?;
        ensure_contains(&json, "\"command\":\"status\"", "command")?;
        ensure_contains(&json, "\"version\":", "version")?;
        // Minimal should NOT have capabilities, runtime, or degraded
        ensure(!json.contains("\"capabilities\":"), "no capabilities")?;
        ensure(!json.contains("\"runtime\":"), "no runtime")?;
        ensure(!json.contains("\"degraded\":"), "no degraded")
    }

    #[test]
    fn render_status_json_filtered_summary_adds_capabilities() -> TestResult {
        use super::{FieldProfile, render_status_json_filtered};
        let report = StatusReport::gather();
        let json = render_status_json_filtered(&report, FieldProfile::Summary);

        ensure_contains(&json, "\"fields\":\"summary\"", "fields indicator")?;
        ensure_contains(&json, "\"capabilities\":", "has capabilities")?;
        // Summary should NOT have runtime or degraded arrays
        let value = serde_json::from_str::<serde_json::Value>(&json)
            .map_err(|error| format!("status summary JSON parses: {error}"))?;
        let data = value
            .get("data")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| "status summary has data object".to_string())?;
        ensure(!data.contains_key("runtime"), "no runtime object")?;
        ensure(!data.contains_key("degraded"), "no degraded array")
    }

    #[test]
    fn render_status_json_filtered_standard_adds_arrays() -> TestResult {
        use super::{FieldProfile, render_status_json_filtered};
        let report = StatusReport::gather();
        let json = render_status_json_filtered(&report, FieldProfile::Standard);

        ensure_contains(&json, "\"fields\":\"standard\"", "fields indicator")?;
        ensure_contains(&json, "\"capabilities\":", "has capabilities")?;
        ensure_contains(&json, "\"runtime\":", "has runtime")?;
        ensure_contains(&json, "\"degraded\":", "has degraded")?;
        // Standard should NOT have repair in degraded items
        ensure(!json.contains("\"repair\":"), "no repair in degraded")
    }

    #[test]
    fn render_status_json_filtered_full_includes_verbose() -> TestResult {
        use super::{FieldProfile, render_status_json_filtered};
        let report = StatusReport::gather();
        let json = render_status_json_filtered(&report, FieldProfile::Full);

        ensure_contains(&json, "\"fields\":\"full\"", "fields indicator")?;
        ensure_contains(&json, "\"capabilities\":", "has capabilities")?;
        ensure_contains(&json, "\"runtime\":", "has runtime")?;
        ensure_contains(&json, "\"degraded\":", "has degraded")?;
        ensure_contains(&json, "\"repair\":", "has repair in degraded")
    }

    #[test]
    fn render_capabilities_json_filtered_minimal_only_essentials() -> TestResult {
        use super::{FieldProfile, render_capabilities_json_filtered};
        use crate::core::capabilities::CapabilitiesReport;
        let report = CapabilitiesReport::gather();
        let json = render_capabilities_json_filtered(&report, FieldProfile::Minimal);

        ensure_contains(&json, "\"command\":\"capabilities\"", "command")?;
        ensure_contains(&json, "\"version\":", "version")?;
        ensure_contains(&json, "\"fields\":\"minimal\"", "fields")?;
        // Minimal: no arrays, no summary
        ensure(!json.contains("\"subsystems\":"), "no subsystems")?;
        ensure(!json.contains("\"features\":"), "no features")?;
        ensure(!json.contains("\"commands\":"), "no commands")?;
        ensure(!json.contains("\"summary\":"), "no summary")
    }

    #[test]
    fn render_capabilities_json_filtered_full_has_descriptions() -> TestResult {
        use super::{FieldProfile, render_capabilities_json_filtered};
        use crate::core::capabilities::CapabilitiesReport;
        let report = CapabilitiesReport::gather();
        let json = render_capabilities_json_filtered(&report, FieldProfile::Full);

        ensure_contains(&json, "\"subsystems\":", "has subsystems")?;
        ensure_contains(&json, "\"description\":", "has descriptions")?;
        ensure_contains(&json, "\"summary\":", "has summary")
    }

    // ========================================================================
    // Evaluation Report Renderer Tests (EE-255)
    // ========================================================================

    #[test]
    fn render_eval_report_json_empty_report() -> TestResult {
        use super::render_eval_report_json;
        use crate::eval::EvaluationReport;

        let report = EvaluationReport::new();
        let json = render_eval_report_json(&report, None);

        ensure_contains(&json, "\"schema\":\"ee.response.v1\"", "schema")?;
        ensure_contains(&json, "\"success\":true", "success")?;
        ensure_contains(&json, "\"command\":\"eval run\"", "command")?;
        ensure_contains(&json, "\"status\":\"no_scenarios\"", "status")?;
        ensure_contains(&json, "\"scenariosRun\":0", "scenariosRun")?;
        ensure_contains(&json, "\"results\":[]", "empty results")
    }

    #[test]
    fn render_eval_report_json_with_scenario_id() -> TestResult {
        use super::render_eval_report_json;
        use crate::eval::EvaluationReport;

        let report = EvaluationReport::new();
        let json = render_eval_report_json(&report, Some("test_scenario"));

        ensure_contains(&json, "\"scenarioId\":\"test_scenario\"", "scenarioId")
    }

    #[test]
    fn render_eval_run_json_with_science_includes_metrics() -> TestResult {
        use super::render_eval_report_json;
        use crate::eval::{EVAL_SCIENCE_METRICS_SCHEMA_V1, EvaluationReport};
        use crate::science::status;

        let mut report = EvaluationReport::new();
        report.attach_science_metrics();
        let json = render_eval_report_json(&report, None);
        ensure_contains(&json, "\"scienceMetrics\":", "science metrics block")?;
        ensure_contains(
            &json,
            &format!("\"schema\":\"{EVAL_SCIENCE_METRICS_SCHEMA_V1}\""),
            "science schema",
        )?;
        ensure_contains(
            &json,
            &format!("\"status\":\"{}\"", status().as_str()),
            "science status",
        )
    }

    #[test]
    fn render_eval_run_human_with_science_includes_metrics_section() -> TestResult {
        use super::render_eval_report_human;
        use crate::eval::EvaluationReport;

        let mut report = EvaluationReport::new();
        report.attach_science_metrics();
        let human = render_eval_report_human(&report, None);
        ensure_contains(&human, "Science metrics:", "science header")?;
        ensure_contains(&human, "Scenarios evaluated:", "scenario count")
    }

    #[test]
    fn render_eval_report_json_all_passed() -> TestResult {
        use super::render_eval_report_json;
        use crate::eval::{EvaluationReport, EvaluationStatus, ScenarioValidationResult};

        let mut report = EvaluationReport::new();
        report.add_result(ScenarioValidationResult {
            scenario_id: "scenario_1".to_string(),
            passed: true,
            steps_passed: 3,
            steps_total: 3,
            failures: vec![],
        });
        report.add_result(ScenarioValidationResult {
            scenario_id: "scenario_2".to_string(),
            passed: true,
            steps_passed: 2,
            steps_total: 2,
            failures: vec![],
        });
        report.finalize();

        ensure_equal(&report.status, &EvaluationStatus::AllPassed, "status")?;

        let json = render_eval_report_json(&report, None);
        ensure_contains(&json, "\"success\":true", "success")?;
        ensure_contains(&json, "\"status\":\"all_passed\"", "status")?;
        ensure_contains(&json, "\"scenariosRun\":2", "scenariosRun")?;
        ensure_contains(&json, "\"scenariosPassed\":2", "scenariosPassed")?;
        ensure_contains(&json, "\"scenariosFailed\":0", "scenariosFailed")?;
        ensure_contains(&json, "\"scenarioId\":\"scenario_1\"", "result 1")?;
        ensure_contains(&json, "\"scenarioId\":\"scenario_2\"", "result 2")
    }

    #[test]
    fn render_eval_report_json_some_failed() -> TestResult {
        use super::render_eval_report_json;
        use crate::eval::{
            EvaluationReport, EvaluationStatus, ScenarioValidationResult, ValidationFailure,
            ValidationFailureKind,
        };

        let mut report = EvaluationReport::new();
        report.add_result(ScenarioValidationResult {
            scenario_id: "passing".to_string(),
            passed: true,
            steps_passed: 2,
            steps_total: 2,
            failures: vec![],
        });
        report.add_result(ScenarioValidationResult {
            scenario_id: "failing".to_string(),
            passed: false,
            steps_passed: 1,
            steps_total: 2,
            failures: vec![ValidationFailure {
                step: 2,
                kind: ValidationFailureKind::GoldenMismatch,
                message: "Output differs from golden".to_string(),
            }],
        });
        report.finalize();

        ensure_equal(&report.status, &EvaluationStatus::SomeFailed, "status")?;

        let json = render_eval_report_json(&report, None);
        ensure_contains(&json, "\"success\":false", "not success")?;
        ensure_contains(&json, "\"status\":\"some_failed\"", "status")?;
        ensure_contains(&json, "\"scenariosPassed\":1", "scenariosPassed")?;
        ensure_contains(&json, "\"scenariosFailed\":1", "scenariosFailed")?;
        ensure_contains(&json, "\"kind\":\"golden_mismatch\"", "failure kind")?;
        ensure_contains(
            &json,
            "\"message\":\"Output differs from golden\"",
            "failure msg",
        )
    }

    #[test]
    fn render_eval_report_human_empty_report() -> TestResult {
        use super::render_eval_report_human;
        use crate::eval::EvaluationReport;

        let report = EvaluationReport::new();
        let human = render_eval_report_human(&report, None);

        ensure_contains(&human, "ee eval run", "header")?;
        ensure_contains(&human, "Status: no scenarios available", "status")?;
        ensure_contains(&human, "Results: 0 run, 0 passed, 0 failed", "results")?;
        ensure_contains(&human, "No evaluation scenarios configured", "message")
    }

    #[test]
    fn render_eval_report_human_with_results() -> TestResult {
        use super::render_eval_report_human;
        use crate::eval::{
            EvaluationReport, ScenarioValidationResult, ValidationFailure, ValidationFailureKind,
        };

        let mut report = EvaluationReport::new();
        report.add_result(ScenarioValidationResult {
            scenario_id: "test_scenario".to_string(),
            passed: false,
            steps_passed: 2,
            steps_total: 3,
            failures: vec![ValidationFailure {
                step: 3,
                kind: ValidationFailureKind::ExitCodeMismatch,
                message: "Expected 0, got 1".to_string(),
            }],
        });
        report.finalize();

        let human = render_eval_report_human(&report, None);

        ensure_contains(&human, "[FAIL] test_scenario: 2/3 steps", "scenario result")?;
        ensure_contains(&human, "Step 3: exit_code_mismatch", "failure step")?;
        ensure_contains(&human, "Expected 0, got 1", "failure message")
    }

    #[test]
    fn render_eval_report_toon_produces_valid_toon() -> TestResult {
        use super::render_eval_report_toon;
        use crate::eval::{EvaluationReport, ScenarioValidationResult};

        let mut report = EvaluationReport::new();
        report.add_result(ScenarioValidationResult {
            scenario_id: "test".to_string(),
            passed: true,
            steps_passed: 1,
            steps_total: 1,
            failures: vec![],
        });
        report.finalize();

        let toon = render_eval_report_toon(&report, None);

        ensure_contains(&toon, "ee.response.v1", "schema")?;
        ensure_contains(&toon, "all_passed", "status")?;
        ensure_contains(&toon, "test", "scenario id")
    }

    #[test]
    fn evaluation_status_strings_are_stable() -> TestResult {
        use crate::eval::EvaluationStatus;

        ensure_equal(
            &EvaluationStatus::NoScenarios.as_str(),
            &"no_scenarios",
            "no_scenarios",
        )?;
        ensure_equal(
            &EvaluationStatus::AllPassed.as_str(),
            &"all_passed",
            "all_passed",
        )?;
        ensure_equal(
            &EvaluationStatus::SomeFailed.as_str(),
            &"some_failed",
            "some_failed",
        )?;
        ensure_equal(
            &EvaluationStatus::AllFailed.as_str(),
            &"all_failed",
            "all_failed",
        )
    }

    #[test]
    fn evaluation_status_is_success() -> TestResult {
        use crate::eval::EvaluationStatus;

        ensure_equal(
            &EvaluationStatus::NoScenarios.is_success(),
            &true,
            "no_scenarios is success",
        )?;
        ensure_equal(
            &EvaluationStatus::AllPassed.is_success(),
            &true,
            "all_passed is success",
        )?;
        ensure_equal(
            &EvaluationStatus::SomeFailed.is_success(),
            &false,
            "some_failed not success",
        )?;
        ensure_equal(
            &EvaluationStatus::AllFailed.is_success(),
            &false,
            "all_failed not success",
        )
    }

    #[test]
    fn render_eval_report_with_elapsed_and_fixture_dir() -> TestResult {
        use super::render_eval_report_json;
        use crate::eval::EvaluationReport;

        let report = EvaluationReport::new()
            .with_elapsed_ms(42.5)
            .with_fixture_dir("tests/fixtures/eval/");
        let json = render_eval_report_json(&report, None);

        ensure_contains(&json, "\"elapsedMs\":42.50", "elapsedMs")?;
        ensure_contains(
            &json,
            "\"fixtureDir\":\"tests/fixtures/eval/\"",
            "fixtureDir",
        )
    }

    #[test]
    fn shadow_run_report_new_has_correct_schema() -> TestResult {
        let report = ShadowRunReport::new("exp-policy", "default");
        ensure_equal(&report.schema, &SHADOW_RUN_SCHEMA_V1.to_owned(), "schema")
    }

    #[test]
    fn shadow_run_report_add_comparison_updates_summary() -> TestResult {
        let mut report = ShadowRunReport::new("exp-policy", "default");
        report.add_comparison(ShadowRunComparison {
            plane: DecisionPlane::Ranking,
            metadata: DecisionPlaneMetadata::empty(),
            decided_at: "2026-04-30T12:00:00Z".to_owned(),
            shadow_outcome: "rank-3".to_owned(),
            incumbent_outcome: "rank-1".to_owned(),
            diverged: true,
            confidence: Some(0.85),
            reason: None,
        });
        report.add_comparison(ShadowRunComparison {
            plane: DecisionPlane::Packing,
            metadata: DecisionPlaneMetadata::empty(),
            decided_at: "2026-04-30T12:01:00Z".to_owned(),
            shadow_outcome: "include".to_owned(),
            incumbent_outcome: "include".to_owned(),
            diverged: false,
            confidence: Some(0.95),
            reason: None,
        });

        ensure_equal(&report.summary.total, &2, "total")?;
        ensure_equal(&report.summary.diverged, &1, "diverged")?;
        ensure_equal(&report.summary.matched, &1, "matched")
    }

    #[test]
    fn shadow_run_divergence_rate_empty_is_zero() -> TestResult {
        let report = ShadowRunReport::new("exp", "default");
        let rate = report.divergence_rate();
        ensure(
            (rate - 0.0).abs() < 0.0001,
            format!("expected 0.0, got {rate}"),
        )
    }

    #[test]
    fn shadow_run_divergence_rate_computed_correctly() -> TestResult {
        let mut report = ShadowRunReport::new("exp", "default");
        report.add_comparison(ShadowRunComparison {
            plane: DecisionPlane::Curation,
            metadata: DecisionPlaneMetadata::empty(),
            decided_at: "2026-04-30T12:00:00Z".to_owned(),
            shadow_outcome: "archive".to_owned(),
            incumbent_outcome: "keep".to_owned(),
            diverged: true,
            confidence: None,
            reason: None,
        });
        report.add_comparison(ShadowRunComparison {
            plane: DecisionPlane::Curation,
            metadata: DecisionPlaneMetadata::empty(),
            decided_at: "2026-04-30T12:01:00Z".to_owned(),
            shadow_outcome: "keep".to_owned(),
            incumbent_outcome: "keep".to_owned(),
            diverged: false,
            confidence: None,
            reason: None,
        });

        let rate = report.divergence_rate();
        ensure(
            (rate - 0.5).abs() < 0.0001,
            format!("expected 0.5, got {rate}"),
        )
    }

    #[test]
    fn shadow_run_from_record_only_shadow_records() -> TestResult {
        let non_shadow = DecisionRecord::builder()
            .plane(DecisionPlane::Ranking)
            .shadow(false)
            .build();
        ensure(
            ShadowRunComparison::from_record(&non_shadow).is_none(),
            "non-shadow record should return None",
        )?;

        let shadow = DecisionRecord::builder()
            .plane(DecisionPlane::Ranking)
            .shadow(true)
            .outcome("rank-2")
            .incumbent_outcome("rank-1")
            .build();
        let comparison = ShadowRunComparison::from_record(&shadow);
        ensure(comparison.is_some(), "shadow record should return Some")
    }

    #[test]
    fn render_shadow_run_json_contains_schema_and_policies() -> TestResult {
        let report = ShadowRunReport::new("exp-ranker", "default-ranker");
        let json = render_shadow_run_json(&report);

        ensure_contains(&json, "\"schema\":\"ee.shadow_run.v1\"", "schema")?;
        ensure_contains(&json, "\"shadow\":\"exp-ranker\"", "shadow policy")?;
        ensure_contains(
            &json,
            "\"incumbent\":\"default-ranker\"",
            "incumbent policy",
        )
    }

    #[test]
    fn render_shadow_run_json_contains_summary() -> TestResult {
        let mut report = ShadowRunReport::new("exp", "default");
        report.add_comparison(ShadowRunComparison {
            plane: DecisionPlane::CacheAdmission,
            metadata: DecisionPlaneMetadata::empty(),
            decided_at: "2026-04-30T12:00:00Z".to_owned(),
            shadow_outcome: "admit".to_owned(),
            incumbent_outcome: "evict".to_owned(),
            diverged: true,
            confidence: Some(0.9),
            reason: Some("high reuse".to_owned()),
        });
        let json = render_shadow_run_json(&report);

        ensure_contains(&json, "\"total\":1", "total")?;
        ensure_contains(&json, "\"diverged\":1", "diverged")?;
        ensure_contains(&json, "\"matched\":0", "matched")?;
        ensure_contains(&json, "\"divergenceRate\":1.0", "divergenceRate")
    }

    #[test]
    fn render_shadow_run_json_contains_comparison_fields() -> TestResult {
        let mut report = ShadowRunReport::new("exp", "default");
        report.add_comparison(ShadowRunComparison {
            plane: DecisionPlane::RepairOrder,
            metadata: DecisionPlaneMetadata::full("exp", "dec-001", "trace-abc"),
            decided_at: "2026-04-30T12:00:00Z".to_owned(),
            shadow_outcome: "priority-high".to_owned(),
            incumbent_outcome: "priority-low".to_owned(),
            diverged: true,
            confidence: Some(0.75),
            reason: Some("critical path".to_owned()),
        });
        let json = render_shadow_run_json(&report);

        ensure_contains(&json, "\"plane\":\"repair_order\"", "plane")?;
        ensure_contains(
            &json,
            "\"shadowOutcome\":\"priority-high\"",
            "shadowOutcome",
        )?;
        ensure_contains(
            &json,
            "\"incumbentOutcome\":\"priority-low\"",
            "incumbentOutcome",
        )?;
        ensure_contains(&json, "\"diverged\":true", "diverged")?;
        ensure_contains(&json, "\"reason\":\"critical path\"", "reason")?;
        ensure_contains(&json, "\"decisionId\":\"dec-001\"", "decisionId")?;
        ensure_contains(&json, "\"traceId\":\"trace-abc\"", "traceId")
    }

    #[test]
    fn render_shadow_run_human_contains_header_and_summary() -> TestResult {
        let mut report = ShadowRunReport::new("exp-policy", "incumbent-policy");
        report.add_comparison(ShadowRunComparison {
            plane: DecisionPlane::Packing,
            metadata: DecisionPlaneMetadata::empty(),
            decided_at: "2026-04-30T12:00:00Z".to_owned(),
            shadow_outcome: "include".to_owned(),
            incumbent_outcome: "exclude".to_owned(),
            diverged: true,
            confidence: None,
            reason: None,
        });
        let human = render_shadow_run_human(&report);

        ensure_contains(&human, "Shadow-Run Comparison Report", "header")?;
        ensure_contains(&human, "Shadow policy:    exp-policy", "shadow policy")?;
        ensure_contains(
            &human,
            "Incumbent policy: incumbent-policy",
            "incumbent policy",
        )?;
        ensure_contains(&human, "Total decisions:  1", "total")?;
        ensure_contains(&human, "Diverged:         1", "diverged")?;
        ensure_contains(&human, "Divergence rate:  100.0%", "rate")
    }

    #[test]
    fn render_shadow_run_human_contains_comparison_details() -> TestResult {
        let mut report = ShadowRunReport::new("exp", "default");
        report.add_comparison(ShadowRunComparison {
            plane: DecisionPlane::Ranking,
            metadata: DecisionPlaneMetadata::empty(),
            decided_at: "2026-04-30T12:00:00Z".to_owned(),
            shadow_outcome: "rank-5".to_owned(),
            incumbent_outcome: "rank-2".to_owned(),
            diverged: true,
            confidence: Some(0.88),
            reason: Some("recency boost".to_owned()),
        });
        let human = render_shadow_run_human(&report);

        ensure_contains(&human, "[DIVERGED]", "status")?;
        ensure_contains(&human, "ranking", "plane")?;
        ensure_contains(&human, "Shadow:    rank-5", "shadow outcome")?;
        ensure_contains(&human, "Incumbent: rank-2", "incumbent outcome")?;
        ensure_contains(&human, "Reason:    recency boost", "reason")?;
        ensure_contains(&human, "Confidence: 0.88", "confidence")
    }

    #[test]
    fn render_shadow_run_toon_is_valid_toon() -> TestResult {
        let report = ShadowRunReport::new("exp", "default");
        let toon = render_shadow_run_toon(&report);

        ensure_starts_with(&toon, "schema: ee.shadow_run.v1", "toon schema")
    }

    #[test]
    fn shadow_run_compute_avg_confidence() -> TestResult {
        let mut report = ShadowRunReport::new("exp", "default");
        report.add_comparison(ShadowRunComparison {
            plane: DecisionPlane::Packing,
            metadata: DecisionPlaneMetadata::empty(),
            decided_at: "2026-04-30T12:00:00Z".to_owned(),
            shadow_outcome: "include".to_owned(),
            incumbent_outcome: "include".to_owned(),
            diverged: false,
            confidence: Some(0.8),
            reason: None,
        });
        report.add_comparison(ShadowRunComparison {
            plane: DecisionPlane::Packing,
            metadata: DecisionPlaneMetadata::empty(),
            decided_at: "2026-04-30T12:01:00Z".to_owned(),
            shadow_outcome: "exclude".to_owned(),
            incumbent_outcome: "include".to_owned(),
            diverged: true,
            confidence: Some(0.6),
            reason: None,
        });
        report.compute_avg_confidence();

        let avg = report
            .summary
            .avg_confidence
            .ok_or_else(|| "expected avg confidence".to_owned())?;
        ensure(
            (avg - 0.7).abs() < 0.0001,
            format!("expected 0.7, got {avg}"),
        )
    }

    // ========================================================================
    // Output Size Diagnostic Tests (EE-335)
    // ========================================================================

    #[test]
    fn size_diagnostic_from_json_computes_all_fields() -> TestResult {
        use super::OutputSizeDiagnostic;

        let json = r#"{"schema":"ee.response.v1","success":true,"data":{"command":"status"}}"#;
        let diagnostic = OutputSizeDiagnostic::from_json(json);

        ensure(diagnostic.json_bytes > 0, "json_bytes should be positive")?;
        ensure(diagnostic.toon_bytes > 0, "toon_bytes should be positive")?;
        ensure(
            diagnostic.json_estimated_tokens > 0,
            "json tokens should be positive",
        )?;
        ensure(
            diagnostic.toon_estimated_tokens > 0,
            "toon tokens should be positive",
        )?;
        ensure(
            diagnostic.compression_ratio > 0.0 && diagnostic.compression_ratio <= 2.0,
            format!(
                "compression ratio should be reasonable, got {}",
                diagnostic.compression_ratio
            ),
        )
    }

    #[test]
    fn size_diagnostic_json_output_has_required_schema() -> TestResult {
        use super::{OUTPUT_SIZE_DIAGNOSTIC_SCHEMA_V1, OutputSizeDiagnostic};

        let json = r#"{"schema":"ee.response.v1","success":true,"data":{"command":"test"}}"#;
        let diagnostic = OutputSizeDiagnostic::from_json(json);
        let output = diagnostic.to_json();

        ensure_contains(&output, OUTPUT_SIZE_DIAGNOSTIC_SCHEMA_V1, "schema field")?;
        ensure_contains(&output, "\"json\":", "json section")?;
        ensure_contains(&output, "\"toon\":", "toon section")?;
        ensure_contains(&output, "\"savings\":", "savings section")?;
        ensure_contains(&output, "\"bytes\":", "bytes field")?;
        ensure_contains(&output, "\"estimatedTokens\":", "estimatedTokens field")?;
        ensure_contains(&output, "\"compressionRatio\":", "compressionRatio field")
    }

    #[test]
    fn size_diagnostic_human_output_has_structure() -> TestResult {
        use super::OutputSizeDiagnostic;

        let json = r#"{"schema":"ee.response.v1","success":true,"data":{"command":"test"}}"#;
        let diagnostic = OutputSizeDiagnostic::from_json(json);
        let output = diagnostic.to_human();

        ensure_contains(&output, "Output Size Diagnostic", "title")?;
        ensure_contains(&output, "JSON:", "JSON label")?;
        ensure_contains(&output, "TOON:", "TOON label")?;
        ensure_contains(&output, "Savings:", "savings label")?;
        ensure_contains(&output, "bytes", "bytes unit")?;
        ensure_contains(&output, "tokens", "tokens unit")
    }

    #[test]
    fn size_diagnostic_status_report_shows_toon_savings() -> TestResult {
        use super::OutputSizeDiagnostic;

        let report = StatusReport::gather();
        let json = render_status_json(&report);
        let diagnostic = OutputSizeDiagnostic::from_json(&json);

        // TOON should typically be smaller than JSON for structured data
        // But we only assert both are computed, not the relationship
        ensure(diagnostic.json_bytes > 0, "json_bytes computed")?;
        ensure(diagnostic.toon_bytes > 0, "toon_bytes computed")
    }

    #[test]
    fn size_diagnostic_health_report_shows_toon_savings() -> TestResult {
        use super::OutputSizeDiagnostic;

        let report = HealthReport::gather();
        let json = render_health_json(&report);
        let diagnostic = OutputSizeDiagnostic::from_json(&json);

        ensure(diagnostic.json_bytes > 0, "json_bytes computed")?;
        ensure(diagnostic.toon_bytes > 0, "toon_bytes computed")
    }

    #[test]
    fn size_diagnostic_empty_json_handles_gracefully() -> TestResult {
        use super::OutputSizeDiagnostic;

        let diagnostic = OutputSizeDiagnostic::from_json("{}");

        ensure(diagnostic.json_bytes == 2, "empty json is 2 bytes")?;
        ensure(
            diagnostic.compression_ratio >= 0.0,
            "compression ratio should be non-negative",
        )
    }

    #[test]
    fn representative_diagnostics_returns_multiple_reports() -> TestResult {
        use super::compute_representative_size_diagnostics;

        let diagnostics = compute_representative_size_diagnostics();

        ensure(
            diagnostics.len() >= 2,
            format!("expected at least 2 diagnostics, got {}", diagnostics.len()),
        )?;

        for (name, diag) in &diagnostics {
            ensure(
                diag.json_bytes > 0,
                format!("{name}: json_bytes should be positive"),
            )?;
            ensure(
                diag.toon_bytes > 0,
                format!("{name}: toon_bytes should be positive"),
            )?;
        }
        Ok(())
    }

    // ========================================================================
    // Cards Output Tests (EE-341)
    // ========================================================================

    use super::{
        Card, CardKind, CardMath, CardsProfile, GraveyardPriority, GraveyardRecommendationType,
        diversity_penalty_card, graveyard_deprecated_dependency_card,
        graveyard_failed_verification_card, graveyard_missing_demo_card,
        graveyard_output_drift_card, graveyard_stale_claim_card, graveyard_uplift_candidate_card,
        pack_budget_card, relevance_score_card, render_cards_json, selection_score_card,
        trust_score_card, utility_decay_card,
    };

    #[test]
    fn cards_profile_none_excludes_all_cards() -> TestResult {
        let profile = CardsProfile::None;
        ensure(!profile.include_cards(), "none excludes cards")?;
        ensure(!profile.include_math(), "none excludes math")?;
        ensure(!profile.include_provenance(), "none excludes provenance")
    }

    #[test]
    fn cards_profile_summary_includes_cards_only() -> TestResult {
        let profile = CardsProfile::Summary;
        ensure(profile.include_cards(), "summary includes cards")?;
        ensure(!profile.include_math(), "summary excludes math")?;
        ensure(!profile.include_provenance(), "summary excludes provenance")
    }

    #[test]
    fn cards_profile_math_includes_math() -> TestResult {
        let profile = CardsProfile::Math;
        ensure(profile.include_cards(), "math includes cards")?;
        ensure(profile.include_math(), "math includes math")?;
        ensure(!profile.include_provenance(), "math excludes provenance")
    }

    #[test]
    fn cards_profile_full_includes_everything() -> TestResult {
        let profile = CardsProfile::Full;
        ensure(profile.include_cards(), "full includes cards")?;
        ensure(profile.include_math(), "full includes math")?;
        ensure(profile.include_provenance(), "full includes provenance")
    }

    #[test]
    fn card_to_json_respects_profile_none() -> TestResult {
        let card = Card::new("card_001", CardKind::Certificate, "Test Card")
            .with_summary("A test summary")
            .with_math(CardMath::new().with_value(0.95))
            .with_provenance("file://test.rs#L42");

        let json = card.to_json(CardsProfile::None);
        ensure_contains(&json, "\"id\":\"card_001\"", "id always present")?;
        ensure_contains(&json, "\"kind\":\"certificate\"", "kind always present")?;
        ensure_contains(&json, "\"title\":\"Test Card\"", "title always present")?;
        // Summary excluded in None profile
        ensure(
            !json.contains("summary"),
            "summary should be excluded in None profile",
        )
    }

    #[test]
    fn card_to_json_respects_profile_summary() -> TestResult {
        let card = Card::new("card_002", CardKind::Risk, "Risk Card")
            .with_summary("Risk summary")
            .with_math(CardMath::new().with_value(0.75).with_confidence(0.9));

        let json = card.to_json(CardsProfile::Summary);
        ensure_contains(&json, "\"summary\":\"Risk summary\"", "summary included")?;
        ensure(
            !json.contains("math"),
            "math should be excluded in Summary profile",
        )
    }

    #[test]
    fn card_to_json_respects_profile_math() -> TestResult {
        let card = Card::new("card_003", CardKind::Artifact, "Math Card")
            .with_summary("Summary here")
            .with_math(
                CardMath::new()
                    .with_value(0.85)
                    .with_formula("f(x) = x^2")
                    .with_unit("score"),
            )
            .with_provenance("file://math.rs");

        let json = card.to_json(CardsProfile::Math);
        ensure_contains(&json, "\"summary\":", "summary included")?;
        ensure_contains(&json, "\"math\":", "math included")?;
        ensure_contains(&json, "\"formula\":\"f(x) = x^2\"", "formula in math")?;
        ensure(
            !json.contains("provenance"),
            "provenance should be excluded in Math profile",
        )
    }

    #[test]
    fn card_to_json_respects_profile_full() -> TestResult {
        let card = Card::new("card_004", CardKind::Lifecycle, "Full Card")
            .with_summary("Full summary")
            .with_math(CardMath::new().with_confidence(0.99))
            .with_provenance("file://full.rs#L100");

        let json = card.to_json(CardsProfile::Full);
        ensure_contains(&json, "\"summary\":", "summary included")?;
        ensure_contains(&json, "\"math\":", "math included")?;
        ensure_contains(
            &json,
            "\"provenance\":\"file://full.rs#L100\"",
            "provenance included",
        )
    }

    #[test]
    fn render_cards_json_returns_empty_array_for_none_profile() -> TestResult {
        let cards = vec![Card::new("c1", CardKind::Certificate, "Card 1")];
        let json = render_cards_json(&cards, CardsProfile::None);
        ensure(json == "[]", format!("expected [], got {json}"))
    }

    #[test]
    fn render_cards_json_returns_empty_array_for_empty_list() -> TestResult {
        let cards: Vec<Card> = vec![];
        let json = render_cards_json(&cards, CardsProfile::Full);
        ensure(json == "[]", format!("expected [], got {json}"))
    }

    #[test]
    fn render_cards_json_formats_array_correctly() -> TestResult {
        let cards = vec![
            Card::new("c1", CardKind::Certificate, "Card 1"),
            Card::new("c2", CardKind::Risk, "Card 2"),
        ];
        let json = render_cards_json(&cards, CardsProfile::Summary);

        ensure_contains(&json, "[{", "starts with array")?;
        ensure_contains(&json, "}]", "ends with array")?;
        ensure_contains(&json, "},{", "cards separated by comma")
    }

    #[test]
    fn card_kind_as_str_covers_all_variants() -> TestResult {
        ensure(
            CardKind::Certificate.as_str() == "certificate",
            "certificate",
        )?;
        ensure(CardKind::Artifact.as_str() == "artifact", "artifact")?;
        ensure(CardKind::Audit.as_str() == "audit", "audit")?;
        ensure(CardKind::Risk.as_str() == "risk", "risk")?;
        ensure(CardKind::Lifecycle.as_str() == "lifecycle", "lifecycle")
    }

    #[test]
    fn card_math_to_json_includes_all_fields() -> TestResult {
        let math = CardMath::new()
            .with_value(0.123456)
            .with_confidence(0.9999)
            .with_formula("E = mc^2")
            .with_unit("joules");
        let json = math.to_json();

        ensure_contains(&json, "\"formula\":\"E = mc^2\"", "formula")?;
        ensure_contains(&json, "\"value\":0.123456", "value")?;
        ensure_contains(&json, "\"confidence\":0.9999", "confidence")?;
        ensure_contains(&json, "\"unit\":\"joules\"", "unit")
    }

    #[test]
    fn selection_score_card_computes_weighted_combination() -> TestResult {
        let card = selection_score_card(0.9, 0.8, 0.7, 0.835);
        ensure(card.id == "card_selection_score", "card id matches")?;
        ensure_equal(&card.kind, &CardKind::Certificate, "card kind")?;
        ensure(card.summary.is_some(), "summary present")?;
        ensure(card.math.is_some(), "math present")?;
        let Some(math) = card.math else {
            return Err("math present".to_string());
        };
        ensure(math.formula.is_some(), "formula present")?;
        let Some(formula) = math.formula.as_ref() else {
            return Err("formula present".to_string());
        };
        ensure_contains(formula, "score =", "formula has expected form")
    }

    #[test]
    fn relevance_score_card_shows_rrf_fusion() -> TestResult {
        let card = relevance_score_card(0.95, 0.8, 0.875, 3, 60);
        ensure(card.id == "card_relevance_score", "card id matches")?;
        ensure(card.summary.is_some(), "summary present")?;
        let Some(summary) = card.summary else {
            return Err("summary present".to_string());
        };
        ensure_contains(&summary, "Rank 3", "shows rank")?;
        ensure_contains(&summary, "semantic=0.950", "shows semantic score")
    }

    #[test]
    fn utility_decay_card_shows_exponential_decay() -> TestResult {
        let card = utility_decay_card(0.9, 30, 0.01, 0.67);
        ensure(card.id == "card_utility_decay", "card id matches")?;
        ensure(card.math.is_some(), "math present")?;
        let Some(math) = card.math else {
            return Err("math present".to_string());
        };
        let Some(formula) = math.formula.as_ref() else {
            return Err("formula present".to_string());
        };
        ensure_contains(formula, "exp(", "formula shows exponential decay")
    }

    #[test]
    fn trust_score_card_shows_weighted_computation() -> TestResult {
        let card = trust_score_card("human_explicit", 1.0, 0.95, 0.95);
        ensure(card.id == "card_trust_score", "card id matches")?;
        ensure(card.summary.is_some(), "summary present")?;
        let Some(summary) = card.summary.as_ref() else {
            return Err("summary present".to_string());
        };
        ensure_contains(summary, "human_explicit", "shows trust class")
    }

    #[test]
    fn pack_budget_card_shows_utilization() -> TestResult {
        let card = pack_budget_card(3500, 4000, 12, 3);
        ensure(card.id == "card_pack_budget", "card id matches")?;
        ensure_equal(&card.kind, &CardKind::Audit, "card kind is audit")?;
        ensure(card.summary.is_some(), "summary present")?;
        let Some(summary) = card.summary else {
            return Err("summary present".to_string());
        };
        ensure_contains(&summary, "3500/4000", "shows token usage")?;
        ensure_contains(&summary, "12 items", "shows item count")?;
        ensure_contains(&summary, "3 omitted", "shows omitted count")
    }

    #[test]
    fn diversity_penalty_card_shows_mmr_computation() -> TestResult {
        let card = diversity_penalty_card(0.95, 0.15, 0.80, 2);
        ensure(card.id == "card_diversity_penalty", "card id matches")?;
        ensure(card.math.is_some(), "math present")?;
        let Some(math) = card.math else {
            return Err("math present".to_string());
        };
        let Some(formula) = math.formula.as_ref() else {
            return Err("formula present".to_string());
        };
        ensure_contains(formula, "max_sim", "formula references similarity")
    }

    // ====================================================================
    // EE-374: Graveyard recommendation card tests
    // ====================================================================

    #[test]
    fn graveyard_priority_ordering() {
        assert!(GraveyardPriority::Low < GraveyardPriority::Medium);
        assert!(GraveyardPriority::Medium < GraveyardPriority::High);
        assert!(GraveyardPriority::High < GraveyardPriority::Critical);
    }

    #[test]
    fn graveyard_priority_strings_stable() {
        assert_eq!(GraveyardPriority::Low.as_str(), "low");
        assert_eq!(GraveyardPriority::Medium.as_str(), "medium");
        assert_eq!(GraveyardPriority::High.as_str(), "high");
        assert_eq!(GraveyardPriority::Critical.as_str(), "critical");
    }

    #[test]
    fn graveyard_recommendation_type_strings_stable() {
        assert_eq!(
            GraveyardRecommendationType::StaleClaim.as_str(),
            "stale_claim"
        );
        assert_eq!(
            GraveyardRecommendationType::MissingDemo.as_str(),
            "missing_demo"
        );
        assert_eq!(
            GraveyardRecommendationType::FailedVerification.as_str(),
            "failed_verification"
        );
        assert_eq!(
            GraveyardRecommendationType::UpliftCandidate.as_str(),
            "uplift_candidate"
        );
        assert_eq!(
            GraveyardRecommendationType::OutputDrift.as_str(),
            "output_drift"
        );
        assert_eq!(
            GraveyardRecommendationType::DeprecatedDependency.as_str(),
            "deprecated_dependency"
        );
    }

    #[test]
    fn graveyard_recommendation_default_priorities() {
        assert_eq!(
            GraveyardRecommendationType::StaleClaim.default_priority(),
            GraveyardPriority::Medium
        );
        assert_eq!(
            GraveyardRecommendationType::MissingDemo.default_priority(),
            GraveyardPriority::High
        );
        assert_eq!(
            GraveyardRecommendationType::FailedVerification.default_priority(),
            GraveyardPriority::Critical
        );
        assert_eq!(
            GraveyardRecommendationType::UpliftCandidate.default_priority(),
            GraveyardPriority::Low
        );
    }

    #[test]
    fn graveyard_stale_claim_card_fixture() -> TestResult {
        let card = graveyard_stale_claim_card("claim_test_001", 45, 30);
        ensure(
            card.id.contains("graveyard_stale"),
            "card id contains graveyard_stale",
        )?;
        ensure_equal(
            &card.kind,
            &CardKind::Recommendation,
            "card kind is recommendation",
        )?;
        ensure(card.summary.is_some(), "summary present")?;
        let Some(summary) = card.summary else {
            return Err("summary present".to_string());
        };
        ensure_contains(&summary, "45 days", "shows days since verification")?;
        ensure_contains(&summary, "claim_test_001", "includes claim id")
    }

    #[test]
    fn graveyard_missing_demo_card_fixture() -> TestResult {
        let card = graveyard_missing_demo_card("claim_test_002", "Test Claim Title");
        ensure(
            card.id.contains("missing_demo"),
            "card id contains missing_demo",
        )?;
        ensure_equal(
            &card.kind,
            &CardKind::Recommendation,
            "card kind is recommendation",
        )?;
        ensure(card.summary.is_some(), "summary present")?;
        let Some(summary) = card.summary else {
            return Err("summary present".to_string());
        };
        ensure_contains(&summary, "Test Claim Title", "includes claim title")?;
        ensure_contains(&summary, "demo.yaml", "mentions demo.yaml")
    }

    #[test]
    fn graveyard_failed_verification_card_fixture() -> TestResult {
        let card =
            graveyard_failed_verification_card("claim_test_003", "Exit code 1", "2026-04-30");
        ensure(
            card.id.contains("graveyard_failed"),
            "card id contains graveyard_failed",
        )?;
        ensure_equal(
            &card.kind,
            &CardKind::Recommendation,
            "card kind is recommendation",
        )?;
        ensure(card.summary.is_some(), "summary present")?;
        let Some(summary) = card.summary else {
            return Err("summary present".to_string());
        };
        ensure_contains(&summary, "Exit code 1", "includes failure reason")?;
        ensure_contains(&summary, "2026-04-30", "includes last attempt date")
    }

    #[test]
    fn graveyard_uplift_candidate_card_fixture() -> TestResult {
        let card = graveyard_uplift_candidate_card("claim_test_004", 5, 0.95);
        ensure(
            card.id.contains("graveyard_uplift"),
            "card id contains graveyard_uplift",
        )?;
        ensure_equal(
            &card.kind,
            &CardKind::Recommendation,
            "card kind is recommendation",
        )?;
        ensure(card.summary.is_some(), "summary present")?;
        let Some(summary) = card.summary else {
            return Err("summary present".to_string());
        };
        ensure_contains(&summary, "5 consecutive", "shows consecutive passes")?;
        ensure_contains(&summary, "95.0%", "shows confidence percentage")?;
        ensure(card.math.is_some(), "math present for uplift card")
    }

    #[test]
    fn graveyard_output_drift_card_fixture() -> TestResult {
        let card =
            graveyard_output_drift_card("demo_test_001", "abc123def456", "xyz789uvw012", 0.15);
        ensure(
            card.id.contains("graveyard_drift"),
            "card id contains graveyard_drift",
        )?;
        ensure_equal(
            &card.kind,
            &CardKind::Recommendation,
            "card kind is recommendation",
        )?;
        ensure(card.summary.is_some(), "summary present")?;
        let Some(summary) = card.summary else {
            return Err("summary present".to_string());
        };
        ensure_contains(&summary, "15.0%", "shows drift percentage")?;
        ensure_contains(&summary, "abc123de", "shows truncated expected hash")?;
        ensure(card.math.is_some(), "math present for drift card")
    }

    #[test]
    fn graveyard_deprecated_dependency_card_fixture() -> TestResult {
        let card = graveyard_deprecated_dependency_card(
            "claim_test_005",
            "old_feature_v1",
            Some("new_feature_v2"),
        );
        ensure(
            card.id.contains("graveyard_deprecated"),
            "card id contains graveyard_deprecated",
        )?;
        ensure_equal(
            &card.kind,
            &CardKind::Recommendation,
            "card kind is recommendation",
        )?;
        ensure(card.summary.is_some(), "summary present")?;
        let Some(summary) = card.summary else {
            return Err("summary present".to_string());
        };
        ensure_contains(&summary, "old_feature_v1", "shows deprecated feature")?;
        ensure_contains(&summary, "new_feature_v2", "shows replacement")
    }

    #[test]
    fn graveyard_deprecated_dependency_card_no_replacement() -> TestResult {
        let card = graveyard_deprecated_dependency_card("claim_test_006", "legacy_api", None);
        ensure(card.summary.is_some(), "summary present")?;
        let Some(summary) = card.summary else {
            return Err("summary present".to_string());
        };
        ensure_contains(&summary, "legacy_api", "shows deprecated feature")?;
        ensure_contains(&summary, "remove", "suggests removal when no replacement")
    }

    #[test]
    fn card_kind_recommendation_stable() {
        assert_eq!(CardKind::Recommendation.as_str(), "recommendation");
    }
}
