//! Regret ledger scoring for counterfactual memory analysis (EE-383).
//!
//! Scores memory decisions based on counterfactual analysis of task episodes.
//! Regret measures the impact of memory retrieval decisions on task outcomes:
//!
//! * **Missed**: Relevant memory existed but wasn't retrieved
//! * **Stale**: Outdated memory was used and caused issues
//! * **Noisy**: Irrelevant memory was retrieved and distracted
//! * **Harmful**: Actively wrong memory led to bad outcome
//!
//! Regret scores feed into curation candidates for memory improvement.

use crate::models::episode::{
    CounterfactualRun, EpisodeOutcome, REGRET_ENTRY_SCHEMA_V1, RegretCategory, RegretEntry,
};

/// Schema version for regret scoring output.
pub const REGRET_SCORING_SCHEMA_V1: &str = "ee.regret_scoring.v1";

/// Default weights for regret category scoring.
pub const DEFAULT_MISSED_WEIGHT: f64 = 1.0;
pub const DEFAULT_STALE_WEIGHT: f64 = 0.8;
pub const DEFAULT_NOISY_WEIGHT: f64 = 0.3;
pub const DEFAULT_HARMFUL_WEIGHT: f64 = 1.5;

/// Minimum confidence threshold for regret entry creation.
pub const MIN_REGRET_CONFIDENCE: f64 = 0.5;

/// Minimum regret score to consider for curation.
pub const MIN_ACTIONABLE_REGRET: f64 = 0.4;

/// Configuration for regret scoring.
#[derive(Clone, Debug, PartialEq)]
pub struct RegretScoringConfig {
    /// Weight for missed memory regret.
    pub missed_weight: f64,
    /// Weight for stale memory regret.
    pub stale_weight: f64,
    /// Weight for noisy memory regret.
    pub noisy_weight: f64,
    /// Weight for harmful memory regret.
    pub harmful_weight: f64,
    /// Minimum confidence to create regret entry.
    pub min_confidence: f64,
    /// Minimum regret score to be actionable.
    pub min_actionable: f64,
}

impl Default for RegretScoringConfig {
    fn default() -> Self {
        Self {
            missed_weight: DEFAULT_MISSED_WEIGHT,
            stale_weight: DEFAULT_STALE_WEIGHT,
            noisy_weight: DEFAULT_NOISY_WEIGHT,
            harmful_weight: DEFAULT_HARMFUL_WEIGHT,
            min_confidence: MIN_REGRET_CONFIDENCE,
            min_actionable: MIN_ACTIONABLE_REGRET,
        }
    }
}

impl RegretScoringConfig {
    /// Create a new config with custom weights.
    #[must_use]
    pub fn new(
        missed_weight: f64,
        stale_weight: f64,
        noisy_weight: f64,
        harmful_weight: f64,
    ) -> Self {
        Self {
            missed_weight,
            stale_weight,
            noisy_weight,
            harmful_weight,
            ..Default::default()
        }
    }

    /// Set minimum confidence threshold.
    #[must_use]
    pub fn with_min_confidence(mut self, min: f64) -> Self {
        self.min_confidence = min;
        self
    }

    /// Set minimum actionable regret threshold.
    #[must_use]
    pub fn with_min_actionable(mut self, min: f64) -> Self {
        self.min_actionable = min;
        self
    }

    /// Get weight for a regret category.
    #[must_use]
    pub fn weight_for(&self, category: RegretCategory) -> f64 {
        match category {
            RegretCategory::MissingKnowledge => self.missed_weight,
            RegretCategory::StaleInformation => self.stale_weight,
            RegretCategory::RetrievalFailure => self.missed_weight,
            RegretCategory::UnderutilizedMemory => self.noisy_weight,
            RegretCategory::Misinformation => self.harmful_weight,
            RegretCategory::Other => self.noisy_weight,
        }
    }
}

/// Input for scoring a counterfactual run.
#[derive(Clone, Debug)]
pub struct RegretScoringInput {
    /// ID for the generated regret entry.
    pub entry_id: String,
    /// Episode that experienced potential regret.
    pub episode_id: String,
    /// Counterfactual run being scored.
    pub counterfactual_run: CounterfactualRun,
    /// Original episode outcome.
    pub original_outcome: EpisodeOutcome,
    /// Timestamp for the regret entry.
    pub timestamp: String,
}

/// Output from regret scoring.
#[derive(Clone, Debug, PartialEq)]
pub struct RegretScoringOutput {
    /// Generated regret entry, if regret was detected.
    pub entry: Option<RegretEntry>,
    /// Raw regret score before thresholds.
    pub raw_score: f64,
    /// Weighted score after category adjustment.
    pub weighted_score: f64,
    /// Detected regret category.
    pub category: RegretCategory,
    /// Whether this regret is actionable.
    pub is_actionable: bool,
    /// Explanation of scoring decision.
    pub explanation: String,
}

/// Score a counterfactual run to determine regret.
///
/// Compares the original outcome with the hypothetical outcome
/// to calculate regret scores. Higher scores indicate interventions
/// that would have significantly improved the outcome.
#[must_use]
pub fn score_counterfactual(
    input: &RegretScoringInput,
    config: &RegretScoringConfig,
) -> RegretScoringOutput {
    let cfr = &input.counterfactual_run;

    // Determine regret category from intervention types
    let category = categorize_interventions(cfr);

    // Calculate base regret from outcome improvement
    let raw_score = calculate_outcome_improvement(input.original_outcome, cfr.hypothetical_outcome);

    // Apply category weight
    let category_weight = config.weight_for(category);
    let weighted_score = raw_score * category_weight * cfr.confidence;

    // Check thresholds
    let meets_confidence = cfr.confidence >= config.min_confidence;
    let is_actionable = weighted_score >= config.min_actionable && meets_confidence;

    // Build explanation
    let explanation = build_explanation(
        input.original_outcome,
        cfr.hypothetical_outcome,
        category,
        raw_score,
        weighted_score,
        cfr.confidence,
        is_actionable,
    );

    // Create entry if actionable
    let entry = if is_actionable {
        let intervention_id = cfr
            .intervention_ids
            .first()
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());

        Some(RegretEntry {
            schema: REGRET_ENTRY_SCHEMA_V1,
            id: input.entry_id.clone(),
            episode_id: input.episode_id.clone(),
            counterfactual_run_id: cfr.id.clone(),
            intervention_id,
            regret_score: weighted_score,
            confidence: cfr.confidence,
            category,
            promoted: false,
            promoted_memory_id: None,
            created_at: input.timestamp.clone(),
        })
    } else {
        None
    };

    RegretScoringOutput {
        entry,
        raw_score,
        weighted_score,
        category,
        is_actionable,
        explanation,
    }
}

/// Categorize regret based on intervention types.
fn categorize_interventions(cfr: &CounterfactualRun) -> RegretCategory {
    // In a real implementation, we'd look at the actual interventions.
    // For now, infer from the analysis text or use default.
    if let Some(ref analysis) = cfr.analysis {
        let lower = analysis.to_lowercase();
        if lower.contains("missing") || lower.contains("not retrieved") {
            return RegretCategory::MissingKnowledge;
        }
        if lower.contains("stale") || lower.contains("outdated") {
            return RegretCategory::StaleInformation;
        }
        if lower.contains("wrong") || lower.contains("incorrect") || lower.contains("harmful") {
            return RegretCategory::Misinformation;
        }
        if lower.contains("noisy") || lower.contains("irrelevant") || lower.contains("distract") {
            return RegretCategory::UnderutilizedMemory;
        }
        if lower.contains("retrieval") || lower.contains("search") {
            return RegretCategory::RetrievalFailure;
        }
    }
    RegretCategory::Other
}

/// Calculate outcome improvement score.
///
/// Returns a score from 0.0 to 1.0 indicating how much the
/// hypothetical outcome improves over the original.
fn calculate_outcome_improvement(original: EpisodeOutcome, hypothetical: EpisodeOutcome) -> f64 {
    let original_value = outcome_value(original);
    let hypothetical_value = outcome_value(hypothetical);

    // Improvement is positive delta, clamped to [0, 1]
    let delta = hypothetical_value - original_value;
    delta.clamp(0.0, 1.0)
}

/// Assign numeric value to outcomes for comparison.
fn outcome_value(outcome: EpisodeOutcome) -> f64 {
    match outcome {
        EpisodeOutcome::Success => 1.0,
        EpisodeOutcome::Unknown => 0.5,
        EpisodeOutcome::Cancelled => 0.3,
        EpisodeOutcome::Timeout => 0.2,
        EpisodeOutcome::Failure => 0.0,
    }
}

/// Build human-readable explanation of scoring.
fn build_explanation(
    original: EpisodeOutcome,
    hypothetical: EpisodeOutcome,
    category: RegretCategory,
    raw_score: f64,
    weighted_score: f64,
    confidence: f64,
    is_actionable: bool,
) -> String {
    let action_status = if is_actionable {
        "actionable"
    } else {
        "not actionable"
    };
    format!(
        "Outcome: {} -> {} (improvement: {:.2}). Category: {}. \
         Weighted score: {:.3} (confidence: {:.2}). Status: {}.",
        original.as_str(),
        hypothetical.as_str(),
        raw_score,
        category.as_str(),
        weighted_score,
        confidence,
        action_status,
    )
}

/// Score multiple counterfactual runs and aggregate.
#[must_use]
pub fn score_counterfactuals(
    inputs: &[RegretScoringInput],
    config: &RegretScoringConfig,
) -> Vec<RegretScoringOutput> {
    inputs
        .iter()
        .map(|input| score_counterfactual(input, config))
        .collect()
}

/// Filter actionable regret entries from scoring outputs.
#[must_use]
pub fn filter_actionable(outputs: &[RegretScoringOutput]) -> Vec<&RegretEntry> {
    outputs.iter().filter_map(|o| o.entry.as_ref()).collect()
}

/// Aggregate regret statistics from multiple entries.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RegretStatistics {
    /// Total entries analyzed.
    pub total_analyzed: u32,
    /// Actionable entries.
    pub actionable_count: u32,
    /// Average regret score.
    pub average_score: f64,
    /// Maximum regret score.
    pub max_score: f64,
    /// Count by category.
    pub by_category: Vec<(RegretCategory, u32)>,
}

impl RegretStatistics {
    /// Compute statistics from scoring outputs.
    #[must_use]
    pub fn from_outputs(outputs: &[RegretScoringOutput]) -> Self {
        if outputs.is_empty() {
            return Self::default();
        }

        let total_analyzed = outputs.len() as u32;
        let actionable_count = outputs.iter().filter(|o| o.is_actionable).count() as u32;

        let sum_score: f64 = outputs.iter().map(|o| o.weighted_score).sum();
        let average_score = sum_score / outputs.len() as f64;

        let max_score = outputs
            .iter()
            .map(|o| o.weighted_score)
            .fold(0.0_f64, f64::max);

        // Count by category
        let mut category_counts = std::collections::HashMap::new();
        for output in outputs {
            *category_counts.entry(output.category).or_insert(0u32) += 1;
        }
        let by_category: Vec<_> = category_counts.into_iter().collect();

        Self {
            total_analyzed,
            actionable_count,
            average_score,
            max_score,
            by_category,
        }
    }
}

/// Suggest curation action based on regret entry.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SuggestedCurationAction {
    /// Add a new memory based on intervention.
    AddMemory,
    /// Promote confidence of underutilized memory.
    PromoteConfidence,
    /// Deprecate stale or harmful memory.
    DeprecateMemory,
    /// Supersede with corrected information.
    SupersedeMemory,
    /// Improve retrieval scoring.
    TuneRetrieval,
    /// No action recommended.
    None,
}

impl SuggestedCurationAction {
    /// Stable string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AddMemory => "add_memory",
            Self::PromoteConfidence => "promote_confidence",
            Self::DeprecateMemory => "deprecate_memory",
            Self::SupersedeMemory => "supersede_memory",
            Self::TuneRetrieval => "tune_retrieval",
            Self::None => "none",
        }
    }
}

/// Suggest curation action for a regret entry.
#[must_use]
pub fn suggest_curation(entry: &RegretEntry) -> SuggestedCurationAction {
    match entry.category {
        RegretCategory::MissingKnowledge => SuggestedCurationAction::AddMemory,
        RegretCategory::StaleInformation => SuggestedCurationAction::SupersedeMemory,
        RegretCategory::RetrievalFailure => SuggestedCurationAction::TuneRetrieval,
        RegretCategory::UnderutilizedMemory => SuggestedCurationAction::PromoteConfidence,
        RegretCategory::Misinformation => SuggestedCurationAction::DeprecateMemory,
        RegretCategory::Other => {
            if entry.regret_score >= 0.7 {
                SuggestedCurationAction::AddMemory
            } else {
                SuggestedCurationAction::None
            }
        }
    }
}

/// Schema version for counterfactual curation candidate output.
pub const COUNTERFACTUAL_CANDIDATE_SCHEMA_V1: &str = "ee.counterfactual_candidate.v1";

/// Counterfactual candidate generation mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CounterfactualMode {
    /// Generate candidates in dry-run mode (no persistence).
    DryRun,
    /// Generate and persist candidates.
    Persist,
}

impl CounterfactualMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DryRun => "dry_run",
            Self::Persist => "persist",
        }
    }

    #[must_use]
    pub const fn is_dry_run(self) -> bool {
        matches!(self, Self::DryRun)
    }
}

/// A curation candidate generated from counterfactual analysis.
#[derive(Clone, Debug)]
pub struct CounterfactualCandidate {
    /// Schema version.
    pub schema: &'static str,
    /// Source regret entry ID.
    pub regret_entry_id: String,
    /// Episode that was analyzed.
    pub episode_id: String,
    /// Workspace ID for the candidate.
    pub workspace_id: String,
    /// Target memory ID (if known).
    pub target_memory_id: Option<String>,
    /// Suggested candidate type.
    pub candidate_type: super::CandidateType,
    /// Suggested curation action.
    pub suggested_action: SuggestedCurationAction,
    /// Reason for the curation.
    pub reason: String,
    /// Confidence score.
    pub confidence: f64,
    /// Regret score from original analysis.
    pub regret_score: f64,
    /// Regret category.
    pub category: RegretCategory,
    /// Dry-run flag.
    pub dry_run: bool,
}

impl CounterfactualCandidate {
    #[must_use]
    pub fn is_actionable(&self) -> bool {
        !matches!(self.suggested_action, SuggestedCurationAction::None)
    }
}

/// Generate a curation candidate from a regret entry.
#[must_use]
pub fn generate_candidate_from_regret(
    entry: &RegretEntry,
    workspace_id: &str,
    mode: CounterfactualMode,
) -> Option<CounterfactualCandidate> {
    let suggested_action = suggest_curation(entry);
    if matches!(suggested_action, SuggestedCurationAction::None) {
        return None;
    }

    let candidate_type = action_to_candidate_type(suggested_action);
    let reason = build_candidate_reason(entry, suggested_action);

    Some(CounterfactualCandidate {
        schema: COUNTERFACTUAL_CANDIDATE_SCHEMA_V1,
        regret_entry_id: entry.id.clone(),
        episode_id: entry.episode_id.clone(),
        workspace_id: workspace_id.to_string(),
        target_memory_id: entry.promoted_memory_id.clone(),
        candidate_type,
        suggested_action,
        reason,
        confidence: entry.confidence,
        regret_score: entry.regret_score,
        category: entry.category,
        dry_run: mode.is_dry_run(),
    })
}

/// Generate curation candidates from multiple regret entries.
#[must_use]
pub fn generate_candidates_from_counterfactuals(
    entries: &[RegretEntry],
    workspace_id: &str,
    mode: CounterfactualMode,
) -> Vec<CounterfactualCandidate> {
    entries
        .iter()
        .filter_map(|e| generate_candidate_from_regret(e, workspace_id, mode))
        .collect()
}

/// Report for counterfactual candidate generation.
#[derive(Clone, Debug, Default)]
pub struct CounterfactualCandidateReport {
    /// Total regret entries analyzed.
    pub entries_analyzed: u32,
    /// Candidates generated.
    pub candidates_generated: u32,
    /// Candidates by type.
    pub by_type: Vec<(super::CandidateType, u32)>,
    /// Candidates by action.
    pub by_action: Vec<(SuggestedCurationAction, u32)>,
    /// Mode used.
    pub mode: Option<CounterfactualMode>,
}

impl CounterfactualCandidateReport {
    /// Build a report from generated candidates.
    #[must_use]
    pub fn from_candidates(
        entries_analyzed: usize,
        candidates: &[CounterfactualCandidate],
        mode: CounterfactualMode,
    ) -> Self {
        let mut type_counts = std::collections::HashMap::new();
        let mut action_counts = std::collections::HashMap::new();

        for c in candidates {
            *type_counts.entry(c.candidate_type).or_insert(0u32) += 1;
            *action_counts.entry(c.suggested_action).or_insert(0u32) += 1;
        }

        Self {
            entries_analyzed: entries_analyzed as u32,
            candidates_generated: candidates.len() as u32,
            by_type: type_counts.into_iter().collect(),
            by_action: action_counts.into_iter().collect(),
            mode: Some(mode),
        }
    }
}

fn action_to_candidate_type(action: SuggestedCurationAction) -> super::CandidateType {
    match action {
        SuggestedCurationAction::AddMemory => super::CandidateType::Consolidate,
        SuggestedCurationAction::PromoteConfidence => super::CandidateType::Promote,
        SuggestedCurationAction::DeprecateMemory => super::CandidateType::Deprecate,
        SuggestedCurationAction::SupersedeMemory => super::CandidateType::Supersede,
        SuggestedCurationAction::TuneRetrieval => super::CandidateType::Promote,
        SuggestedCurationAction::None => super::CandidateType::Promote,
    }
}

fn build_candidate_reason(entry: &RegretEntry, action: SuggestedCurationAction) -> String {
    format!(
        "Counterfactual analysis ({}) suggests {} for episode {}. Regret score: {:.3}, confidence: {:.2}.",
        entry.category.as_str(),
        action.as_str(),
        entry.episode_id,
        entry.regret_score,
        entry.confidence,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::episode::CounterfactualMethod;

    fn sample_cfr(
        hypothetical: EpisodeOutcome,
        confidence: f64,
        analysis: Option<&str>,
    ) -> CounterfactualRun {
        let mut cfr = CounterfactualRun::new(
            "cfr_test_001",
            "ep_test_001",
            hypothetical,
            confidence,
            CounterfactualMethod::HeuristicEstimate,
            "2026-04-30T12:00:00Z",
        );
        cfr.add_intervention("int_test_001");
        if let Some(a) = analysis {
            cfr.analysis = Some(a.to_string());
        }
        cfr
    }

    fn sample_input(original: EpisodeOutcome, cfr: CounterfactualRun) -> RegretScoringInput {
        RegretScoringInput {
            entry_id: "reg_test_001".to_string(),
            episode_id: "ep_test_001".to_string(),
            counterfactual_run: cfr,
            original_outcome: original,
            timestamp: "2026-04-30T12:00:00Z".to_string(),
        }
    }

    #[test]
    fn default_config_values() {
        let config = RegretScoringConfig::default();
        assert!((config.missed_weight - 1.0).abs() < f64::EPSILON);
        assert!((config.stale_weight - 0.8).abs() < f64::EPSILON);
        assert!((config.noisy_weight - 0.3).abs() < f64::EPSILON);
        assert!((config.harmful_weight - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn config_weight_for_category() {
        let config = RegretScoringConfig::default();
        assert!((config.weight_for(RegretCategory::MissingKnowledge) - 1.0).abs() < f64::EPSILON);
        assert!((config.weight_for(RegretCategory::StaleInformation) - 0.8).abs() < f64::EPSILON);
        assert!((config.weight_for(RegretCategory::Misinformation) - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn outcome_improvement_failure_to_success() {
        let improvement =
            calculate_outcome_improvement(EpisodeOutcome::Failure, EpisodeOutcome::Success);
        assert!((improvement - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn outcome_improvement_success_to_failure_is_zero() {
        let improvement =
            calculate_outcome_improvement(EpisodeOutcome::Success, EpisodeOutcome::Failure);
        assert!(improvement.abs() < f64::EPSILON);
    }

    #[test]
    fn outcome_improvement_failure_to_unknown() {
        let improvement =
            calculate_outcome_improvement(EpisodeOutcome::Failure, EpisodeOutcome::Unknown);
        assert!((improvement - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn categorize_missing_knowledge() {
        let cfr = sample_cfr(
            EpisodeOutcome::Success,
            0.8,
            Some("The missing knowledge about API format would have helped"),
        );
        let category = categorize_interventions(&cfr);
        assert_eq!(category, RegretCategory::MissingKnowledge);
    }

    #[test]
    fn categorize_stale_information() {
        let cfr = sample_cfr(
            EpisodeOutcome::Success,
            0.8,
            Some("Using stale outdated version info caused the failure"),
        );
        let category = categorize_interventions(&cfr);
        assert_eq!(category, RegretCategory::StaleInformation);
    }

    #[test]
    fn categorize_misinformation() {
        let cfr = sample_cfr(
            EpisodeOutcome::Success,
            0.8,
            Some("The wrong incorrect information was harmful"),
        );
        let category = categorize_interventions(&cfr);
        assert_eq!(category, RegretCategory::Misinformation);
    }

    #[test]
    fn score_actionable_regret() {
        let config = RegretScoringConfig::default();
        let cfr = sample_cfr(
            EpisodeOutcome::Success,
            0.9,
            Some("Missing knowledge about the API"),
        );
        let input = sample_input(EpisodeOutcome::Failure, cfr);

        let output = score_counterfactual(&input, &config);

        assert!(output.is_actionable);
        assert!(output.entry.is_some());
        assert!((output.raw_score - 1.0).abs() < f64::EPSILON);
        assert!(output.weighted_score > 0.4);
    }

    #[test]
    fn score_low_confidence_not_actionable() {
        let config = RegretScoringConfig::default();
        let cfr = sample_cfr(EpisodeOutcome::Success, 0.3, None);
        let input = sample_input(EpisodeOutcome::Failure, cfr);

        let output = score_counterfactual(&input, &config);

        assert!(!output.is_actionable);
        assert!(output.entry.is_none());
    }

    #[test]
    fn score_no_improvement_not_actionable() {
        let config = RegretScoringConfig::default();
        let cfr = sample_cfr(EpisodeOutcome::Failure, 0.9, None);
        let input = sample_input(EpisodeOutcome::Failure, cfr);

        let output = score_counterfactual(&input, &config);

        assert!(!output.is_actionable);
        assert!(output.raw_score.abs() < f64::EPSILON);
    }

    #[test]
    fn filter_actionable_entries() {
        let config = RegretScoringConfig::default();

        let cfr1 = sample_cfr(EpisodeOutcome::Success, 0.9, Some("Missing knowledge"));
        let cfr2 = sample_cfr(EpisodeOutcome::Failure, 0.9, None);

        let inputs = vec![
            sample_input(EpisodeOutcome::Failure, cfr1),
            sample_input(EpisodeOutcome::Failure, cfr2),
        ];

        let outputs = score_counterfactuals(&inputs, &config);
        let actionable = filter_actionable(&outputs);

        assert_eq!(actionable.len(), 1);
    }

    #[test]
    fn statistics_from_outputs() {
        let config = RegretScoringConfig::default();

        let cfr1 = sample_cfr(EpisodeOutcome::Success, 0.9, Some("Missing knowledge"));
        let cfr2 = sample_cfr(EpisodeOutcome::Unknown, 0.7, Some("Stale outdated info"));

        let inputs = vec![
            sample_input(EpisodeOutcome::Failure, cfr1),
            sample_input(EpisodeOutcome::Failure, cfr2),
        ];

        let outputs = score_counterfactuals(&inputs, &config);
        let stats = RegretStatistics::from_outputs(&outputs);

        assert_eq!(stats.total_analyzed, 2);
        assert!(stats.actionable_count >= 1);
        assert!(stats.average_score > 0.0);
    }

    #[test]
    fn suggest_curation_for_missing() {
        let entry = RegretEntry::new(
            "reg_001",
            "ep_001",
            "cfr_001",
            "int_001",
            0.8,
            0.9,
            RegretCategory::MissingKnowledge,
            "2026-04-30T12:00:00Z",
        );

        let action = suggest_curation(&entry);
        assert_eq!(action, SuggestedCurationAction::AddMemory);
    }

    #[test]
    fn suggest_curation_for_stale() {
        let entry = RegretEntry::new(
            "reg_002",
            "ep_001",
            "cfr_001",
            "int_001",
            0.7,
            0.85,
            RegretCategory::StaleInformation,
            "2026-04-30T12:00:00Z",
        );

        let action = suggest_curation(&entry);
        assert_eq!(action, SuggestedCurationAction::SupersedeMemory);
    }

    #[test]
    fn suggest_curation_for_harmful() {
        let entry = RegretEntry::new(
            "reg_003",
            "ep_001",
            "cfr_001",
            "int_001",
            0.9,
            0.95,
            RegretCategory::Misinformation,
            "2026-04-30T12:00:00Z",
        );

        let action = suggest_curation(&entry);
        assert_eq!(action, SuggestedCurationAction::DeprecateMemory);
    }

    #[test]
    fn suggested_action_strings_are_stable() {
        assert_eq!(SuggestedCurationAction::AddMemory.as_str(), "add_memory");
        assert_eq!(
            SuggestedCurationAction::PromoteConfidence.as_str(),
            "promote_confidence"
        );
        assert_eq!(
            SuggestedCurationAction::DeprecateMemory.as_str(),
            "deprecate_memory"
        );
        assert_eq!(
            SuggestedCurationAction::SupersedeMemory.as_str(),
            "supersede_memory"
        );
        assert_eq!(
            SuggestedCurationAction::TuneRetrieval.as_str(),
            "tune_retrieval"
        );
        assert_eq!(SuggestedCurationAction::None.as_str(), "none");
    }

    #[test]
    fn counterfactual_mode_dry_run() {
        assert!(CounterfactualMode::DryRun.is_dry_run());
        assert!(!CounterfactualMode::Persist.is_dry_run());
        assert_eq!(CounterfactualMode::DryRun.as_str(), "dry_run");
        assert_eq!(CounterfactualMode::Persist.as_str(), "persist");
    }

    #[test]
    fn generate_candidate_from_actionable_regret() {
        let entry = RegretEntry::new(
            "reg_004",
            "ep_test_001",
            "cfr_test_001",
            "int_test_001",
            0.8,
            0.9,
            RegretCategory::MissingKnowledge,
            "2026-04-30T12:00:00Z",
        );

        let candidate =
            generate_candidate_from_regret(&entry, "ws_test", CounterfactualMode::DryRun);

        assert!(candidate.is_some());
        let c = candidate.unwrap();
        assert_eq!(c.regret_entry_id, "reg_004");
        assert_eq!(c.workspace_id, "ws_test");
        assert!(c.dry_run);
        assert!(c.is_actionable());
        assert_eq!(c.suggested_action, SuggestedCurationAction::AddMemory);
        assert_eq!(c.candidate_type, super::super::CandidateType::Consolidate);
    }

    #[test]
    fn generate_candidate_from_non_actionable_regret() {
        let entry = RegretEntry::new(
            "reg_005",
            "ep_test_002",
            "cfr_test_002",
            "int_test_002",
            0.2,
            0.3,
            RegretCategory::Other,
            "2026-04-30T12:00:00Z",
        );

        let candidate =
            generate_candidate_from_regret(&entry, "ws_test", CounterfactualMode::DryRun);
        assert!(candidate.is_none());
    }

    #[test]
    fn generate_candidates_from_multiple_entries() {
        let entries = vec![
            RegretEntry::new(
                "reg_006",
                "ep_001",
                "cfr_001",
                "int_001",
                0.8,
                0.9,
                RegretCategory::MissingKnowledge,
                "2026-04-30T12:00:00Z",
            ),
            RegretEntry::new(
                "reg_007",
                "ep_002",
                "cfr_002",
                "int_002",
                0.7,
                0.85,
                RegretCategory::StaleInformation,
                "2026-04-30T12:00:00Z",
            ),
            RegretEntry::new(
                "reg_008",
                "ep_003",
                "cfr_003",
                "int_003",
                0.1,
                0.2,
                RegretCategory::Other,
                "2026-04-30T12:00:00Z",
            ),
        ];

        let candidates = generate_candidates_from_counterfactuals(
            &entries,
            "ws_test",
            CounterfactualMode::DryRun,
        );
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn counterfactual_candidate_report_from_candidates() {
        let entries = vec![
            RegretEntry::new(
                "reg_009",
                "ep_001",
                "cfr_001",
                "int_001",
                0.8,
                0.9,
                RegretCategory::MissingKnowledge,
                "2026-04-30T12:00:00Z",
            ),
            RegretEntry::new(
                "reg_010",
                "ep_002",
                "cfr_002",
                "int_002",
                0.9,
                0.95,
                RegretCategory::Misinformation,
                "2026-04-30T12:00:00Z",
            ),
        ];

        let candidates = generate_candidates_from_counterfactuals(
            &entries,
            "ws_test",
            CounterfactualMode::DryRun,
        );
        let report = CounterfactualCandidateReport::from_candidates(
            entries.len(),
            &candidates,
            CounterfactualMode::DryRun,
        );

        assert_eq!(report.entries_analyzed, 2);
        assert_eq!(report.candidates_generated, 2);
        assert_eq!(report.mode, Some(CounterfactualMode::DryRun));
    }

    #[test]
    fn candidate_reason_format() {
        let entry = RegretEntry::new(
            "reg_011",
            "ep_reason_test",
            "cfr_001",
            "int_001",
            0.75,
            0.88,
            RegretCategory::StaleInformation,
            "2026-04-30T12:00:00Z",
        );

        let candidate =
            generate_candidate_from_regret(&entry, "ws_test", CounterfactualMode::DryRun).unwrap();

        assert!(candidate.reason.contains("stale_information"));
        assert!(candidate.reason.contains("supersede_memory"));
        assert!(candidate.reason.contains("ep_reason_test"));
    }
}
