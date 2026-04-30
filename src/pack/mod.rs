use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fmt;

use crate::models::{MemoryId, ProvenanceUri, RESPONSE_SCHEMA_V1, UnitScore};

pub const SUBSYSTEM: &str = "pack";
pub const CONTEXT_COMMAND: &str = "context";
pub const DEFAULT_CONTEXT_MAX_TOKENS: u32 = 4_000;
pub const DEFAULT_CANDIDATE_POOL: u32 = 64;
pub const DEFAULT_MMR_RELEVANCE_WEIGHT: f32 = 0.75;

/// Conservative characters-per-token ratio for heuristic estimation.
/// Uses 3.5 instead of 4.0 to bias toward overestimation.
pub const DEFAULT_CHARS_PER_TOKEN: f32 = 3.5;

/// Token estimation strategy (EE-143).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum TokenEstimationStrategy {
    /// Use character count divided by chars-per-token ratio.
    /// Conservative and fast, suitable for most use cases.
    #[default]
    CharacterHeuristic,
    /// Count whitespace-separated words, multiplied by 1.3.
    /// More accurate for code but slower.
    WordHeuristic,
}

impl TokenEstimationStrategy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CharacterHeuristic => "character_heuristic",
            Self::WordHeuristic => "word_heuristic",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 2] {
        [Self::CharacterHeuristic, Self::WordHeuristic]
    }
}

impl fmt::Display for TokenEstimationStrategy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Estimate the number of tokens in the given text.
///
/// This is a deterministic heuristic that intentionally overestimates
/// to ensure packs fit within budgets. The default strategy uses a
/// character-based ratio; pass a custom strategy for different behavior.
///
/// Returns at least 1 for any non-empty trimmed input.
#[must_use]
pub fn estimate_tokens(content: &str, strategy: TokenEstimationStrategy) -> u32 {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return 0;
    }

    match strategy {
        TokenEstimationStrategy::CharacterHeuristic => {
            let char_count = trimmed.chars().count();
            // Divide by chars-per-token, round up for conservatism.
            let estimate = (char_count as f32 / DEFAULT_CHARS_PER_TOKEN).ceil();
            (estimate as u32).max(1)
        }
        TokenEstimationStrategy::WordHeuristic => {
            let word_count = trimmed.split_whitespace().count();
            // Multiply by 1.3 to account for punctuation and subword tokens.
            let estimate = (word_count as f32 * 1.3).ceil();
            (estimate as u32).max(1)
        }
    }
}

/// Estimate tokens using the default character heuristic strategy.
#[must_use]
pub fn estimate_tokens_default(content: &str) -> u32 {
    estimate_tokens(content, TokenEstimationStrategy::default())
}

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ContextPackProfile {
    Compact,
    Balanced,
    Thorough,
}

impl ContextPackProfile {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Balanced => "balanced",
            Self::Thorough => "thorough",
        }
    }
}

impl fmt::Display for ContextPackProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PackSection {
    ProceduralRules,
    Decisions,
    Failures,
    Evidence,
    Artifacts,
}

impl PackSection {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProceduralRules => "procedural_rules",
            Self::Decisions => "decisions",
            Self::Failures => "failures",
            Self::Evidence => "evidence",
            Self::Artifacts => "artifacts",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::ProceduralRules,
            Self::Decisions,
            Self::Failures,
            Self::Evidence,
            Self::Artifacts,
        ]
    }
}

impl fmt::Display for PackSection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Token quota for a single section (EE-144).
///
/// Quotas define soft limits on how many tokens a section can use.
/// When a section exceeds its max, remaining candidates are omitted
/// even if the overall budget has room.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SectionQuota {
    /// Minimum tokens to reserve for this section (0 = no minimum).
    pub min_tokens: u32,
    /// Maximum tokens this section can use (0 = unlimited).
    pub max_tokens: u32,
}

impl SectionQuota {
    /// Create a quota with explicit min and max.
    #[must_use]
    pub const fn new(min_tokens: u32, max_tokens: u32) -> Self {
        Self {
            min_tokens,
            max_tokens,
        }
    }

    /// Create an unlimited quota (no constraints).
    #[must_use]
    pub const fn unlimited() -> Self {
        Self {
            min_tokens: 0,
            max_tokens: 0,
        }
    }

    /// Create a quota with only a maximum.
    #[must_use]
    pub const fn capped(max_tokens: u32) -> Self {
        Self {
            min_tokens: 0,
            max_tokens,
        }
    }

    /// True if this quota has no constraints.
    #[must_use]
    pub const fn is_unlimited(self) -> bool {
        self.min_tokens == 0 && self.max_tokens == 0
    }

    /// Check if a token count exceeds this quota's maximum.
    #[must_use]
    pub const fn exceeds_max(self, tokens: u32) -> bool {
        self.max_tokens > 0 && tokens > self.max_tokens
    }

    /// Calculate remaining tokens allowed by this quota.
    #[must_use]
    pub const fn remaining(self, used: u32) -> u32 {
        if self.max_tokens == 0 {
            u32::MAX
        } else {
            self.max_tokens.saturating_sub(used)
        }
    }
}

impl Default for SectionQuota {
    fn default() -> Self {
        Self::unlimited()
    }
}

/// Section quotas for context packing (EE-144).
///
/// Quotas control token allocation across sections, ensuring diversity
/// in the final pack. Each section can have independent min/max limits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SectionQuotas {
    quotas: [SectionQuota; 5],
}

impl SectionQuotas {
    /// Create quotas with explicit values for each section.
    #[must_use]
    pub const fn new(
        procedural_rules: SectionQuota,
        decisions: SectionQuota,
        failures: SectionQuota,
        evidence: SectionQuota,
        artifacts: SectionQuota,
    ) -> Self {
        Self {
            quotas: [procedural_rules, decisions, failures, evidence, artifacts],
        }
    }

    /// Create quotas where all sections are unlimited.
    #[must_use]
    pub const fn unlimited() -> Self {
        Self {
            quotas: [SectionQuota::unlimited(); 5],
        }
    }

    /// Create balanced quotas based on total budget and profile.
    ///
    /// Balanced profile allocates roughly:
    /// - ProceduralRules: 30%
    /// - Decisions: 20%
    /// - Failures: 20%
    /// - Evidence: 20%
    /// - Artifacts: 10%
    #[must_use]
    pub fn balanced(total_budget: u32) -> Self {
        let procedural = (total_budget as f32 * 0.30).ceil() as u32;
        let decisions = (total_budget as f32 * 0.20).ceil() as u32;
        let failures = (total_budget as f32 * 0.20).ceil() as u32;
        let evidence = (total_budget as f32 * 0.20).ceil() as u32;
        let artifacts = (total_budget as f32 * 0.10).ceil() as u32;

        Self::new(
            SectionQuota::capped(procedural),
            SectionQuota::capped(decisions),
            SectionQuota::capped(failures),
            SectionQuota::capped(evidence),
            SectionQuota::capped(artifacts),
        )
    }

    /// Create compact quotas that prioritize procedural rules.
    ///
    /// Compact profile allocates:
    /// - ProceduralRules: 50%
    /// - Decisions: 15%
    /// - Failures: 20%
    /// - Evidence: 10%
    /// - Artifacts: 5%
    #[must_use]
    pub fn compact(total_budget: u32) -> Self {
        let procedural = (total_budget as f32 * 0.50).ceil() as u32;
        let decisions = (total_budget as f32 * 0.15).ceil() as u32;
        let failures = (total_budget as f32 * 0.20).ceil() as u32;
        let evidence = (total_budget as f32 * 0.10).ceil() as u32;
        let artifacts = (total_budget as f32 * 0.05).ceil() as u32;

        Self::new(
            SectionQuota::capped(procedural),
            SectionQuota::capped(decisions),
            SectionQuota::capped(failures),
            SectionQuota::capped(evidence),
            SectionQuota::capped(artifacts),
        )
    }

    /// Create thorough quotas with more even distribution.
    ///
    /// Thorough profile allocates:
    /// - ProceduralRules: 20%
    /// - Decisions: 20%
    /// - Failures: 20%
    /// - Evidence: 25%
    /// - Artifacts: 15%
    #[must_use]
    pub fn thorough(total_budget: u32) -> Self {
        let procedural = (total_budget as f32 * 0.20).ceil() as u32;
        let decisions = (total_budget as f32 * 0.20).ceil() as u32;
        let failures = (total_budget as f32 * 0.20).ceil() as u32;
        let evidence = (total_budget as f32 * 0.25).ceil() as u32;
        let artifacts = (total_budget as f32 * 0.15).ceil() as u32;

        Self::new(
            SectionQuota::capped(procedural),
            SectionQuota::capped(decisions),
            SectionQuota::capped(failures),
            SectionQuota::capped(evidence),
            SectionQuota::capped(artifacts),
        )
    }

    /// Get quotas based on profile and budget.
    #[must_use]
    pub fn for_profile(profile: ContextPackProfile, total_budget: u32) -> Self {
        match profile {
            ContextPackProfile::Compact => Self::compact(total_budget),
            ContextPackProfile::Balanced => Self::balanced(total_budget),
            ContextPackProfile::Thorough => Self::thorough(total_budget),
        }
    }

    /// Get the quota for a specific section.
    #[must_use]
    pub const fn get(&self, section: PackSection) -> SectionQuota {
        self.quotas[section as usize]
    }

    /// Check if a section has room for more tokens.
    #[must_use]
    pub const fn has_room(&self, section: PackSection, used: u32, candidate_tokens: u32) -> bool {
        let quota = self.get(section);
        if quota.max_tokens == 0 {
            return true;
        }
        match used.checked_add(candidate_tokens) {
            Some(total) => total <= quota.max_tokens,
            None => false,
        }
    }

    /// Get remaining tokens for a section.
    #[must_use]
    pub const fn remaining(&self, section: PackSection, used: u32) -> u32 {
        self.get(section).remaining(used)
    }
}

impl Default for SectionQuotas {
    fn default() -> Self {
        Self::unlimited()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextRequestInput {
    pub query: String,
    pub profile: Option<ContextPackProfile>,
    pub max_tokens: Option<u32>,
    pub candidate_pool: Option<u32>,
    pub sections: Vec<PackSection>,
}

impl ContextRequestInput {
    #[must_use]
    pub fn for_query(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            profile: None,
            max_tokens: None,
            candidate_pool: None,
            sections: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextRequest {
    pub query: String,
    pub profile: ContextPackProfile,
    pub budget: TokenBudget,
    pub candidate_pool: u32,
    pub sections: Vec<PackSection>,
}

impl ContextRequest {
    /// Build a validated context-pack request with stable defaults.
    ///
    /// # Errors
    ///
    /// Returns [`PackValidationError::EmptyQuery`] when the query is
    /// empty, [`PackValidationError::ZeroTokenBudget`] when
    /// `max_tokens` is zero, or
    /// [`PackValidationError::ZeroCandidatePool`] when the candidate
    /// pool is zero.
    pub fn new(input: ContextRequestInput) -> Result<Self, PackValidationError> {
        let query = trim_required(input.query, PackValidationError::EmptyQuery)?;
        let budget = match input.max_tokens {
            Some(max_tokens) => TokenBudget::new(max_tokens)?,
            None => TokenBudget::default_context(),
        };
        let candidate_pool = input.candidate_pool.unwrap_or(DEFAULT_CANDIDATE_POOL);
        if candidate_pool == 0 {
            return Err(PackValidationError::ZeroCandidatePool);
        }
        let sections = if input.sections.is_empty() {
            PackSection::all().to_vec()
        } else {
            input.sections
        };

        Ok(Self {
            query,
            profile: input.profile.unwrap_or(ContextPackProfile::Balanced),
            budget,
            candidate_pool,
            sections,
        })
    }

    pub fn from_query(query: impl Into<String>) -> Result<Self, PackValidationError> {
        Self::new(ContextRequestInput::for_query(query))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TokenBudget {
    max_tokens: u32,
}

impl TokenBudget {
    /// Construct a non-zero token budget.
    ///
    /// # Errors
    ///
    /// Returns [`PackValidationError::ZeroTokenBudget`] when `max_tokens`
    /// is zero.
    pub const fn new(max_tokens: u32) -> Result<Self, PackValidationError> {
        if max_tokens == 0 {
            return Err(PackValidationError::ZeroTokenBudget);
        }
        Ok(Self { max_tokens })
    }

    #[must_use]
    pub const fn default_context() -> Self {
        Self {
            max_tokens: DEFAULT_CONTEXT_MAX_TOKENS,
        }
    }

    #[must_use]
    pub const fn max_tokens(self) -> u32 {
        self.max_tokens
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackProvenance {
    pub uri: ProvenanceUri,
    pub note: String,
}

impl PackProvenance {
    /// Create a provenance entry with a short human-readable note.
    ///
    /// # Errors
    ///
    /// Returns [`PackValidationError::EmptyProvenanceNote`] when `note`
    /// is empty after trimming.
    pub fn new(uri: ProvenanceUri, note: impl Into<String>) -> Result<Self, PackValidationError> {
        let note = trim_required(
            note.into(),
            PackValidationError::EmptyProvenanceNote { uri: uri.clone() },
        )?;
        Ok(Self { uri, note })
    }

    /// Render this source reference into the stable shape used by pack
    /// outputs.
    #[must_use]
    pub fn rendered(&self) -> RenderedPackProvenance {
        RenderedPackProvenance::from(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedPackProvenance {
    pub uri: String,
    pub scheme: &'static str,
    pub label: String,
    pub locator: Option<String>,
    pub note: String,
}

impl From<&PackProvenance> for RenderedPackProvenance {
    fn from(provenance: &PackProvenance) -> Self {
        let scheme = provenance.uri.scheme();
        let (label, locator) = rendered_provenance_label(&provenance.uri);
        Self {
            uri: provenance.uri.to_string(),
            scheme,
            label,
            locator,
            note: provenance.note.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackItemProvenance {
    pub rank: u32,
    pub memory_id: MemoryId,
    pub source_index: u32,
    pub source: RenderedPackProvenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackProvenanceFooter {
    pub memory_count: usize,
    pub source_count: usize,
    pub schemes: Vec<&'static str>,
    pub entries: Vec<PackItemProvenance>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PackCandidate {
    pub memory_id: MemoryId,
    pub section: PackSection,
    pub content: String,
    pub estimated_tokens: u32,
    pub relevance: UnitScore,
    pub utility: UnitScore,
    pub provenance: Vec<PackProvenance>,
    pub why: String,
    pub diversity_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PackCandidateInput {
    pub memory_id: MemoryId,
    pub section: PackSection,
    pub content: String,
    pub estimated_tokens: u32,
    pub relevance: UnitScore,
    pub utility: UnitScore,
    pub provenance: Vec<PackProvenance>,
    pub why: String,
}

impl PackCandidate {
    /// Build a validated candidate for context packing.
    ///
    /// # Errors
    ///
    /// Returns a [`PackValidationError`] when the candidate lacks
    /// content, token estimates, provenance, or a selection explanation.
    pub fn new(input: PackCandidateInput) -> Result<Self, PackValidationError> {
        let PackCandidateInput {
            memory_id,
            section,
            content,
            estimated_tokens,
            relevance,
            utility,
            provenance,
            why,
        } = input;
        let content = trim_required(
            content,
            PackValidationError::EmptyCandidateContent { memory_id },
        )?;
        if estimated_tokens == 0 {
            return Err(PackValidationError::ZeroCandidateTokens { memory_id });
        }
        if provenance.is_empty() {
            return Err(PackValidationError::MissingProvenance { memory_id });
        }
        let why = trim_required(why, PackValidationError::MissingWhy { memory_id })?;
        Ok(Self {
            memory_id,
            section,
            content,
            estimated_tokens,
            relevance,
            utility,
            provenance,
            why,
            diversity_key: None,
        })
    }

    #[must_use]
    pub fn with_diversity_key(mut self, diversity_key: impl Into<String>) -> Self {
        let value = diversity_key.into();
        if !value.trim().is_empty() {
            self.diversity_key = Some(value.trim().to_string());
        }
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PackDraft {
    pub query: String,
    pub budget: TokenBudget,
    pub used_tokens: u32,
    pub items: Vec<PackDraftItem>,
    pub omitted: Vec<PackOmission>,
}

impl PackDraft {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[must_use]
    pub fn quality_metrics(&self) -> PackQualityMetrics {
        let item_count = self.items.len();
        let omitted_count = self.omitted.len();
        let provenance_source_count = self
            .items
            .iter()
            .map(|item| item.provenance.len())
            .sum::<usize>();

        let mut relevance_sum = 0.0_f32;
        let mut utility_sum = 0.0_f32;
        for item in &self.items {
            relevance_sum += item.relevance.into_inner();
            utility_sum += item.utility.into_inner();
        }

        let mut token_budget_exceeded = 0_usize;
        let mut redundant_candidates = 0_usize;
        for omission in &self.omitted {
            match omission.reason {
                PackOmissionReason::TokenBudgetExceeded => {
                    token_budget_exceeded = token_budget_exceeded.saturating_add(1);
                }
                PackOmissionReason::RedundantCandidate => {
                    redundant_candidates = redundant_candidates.saturating_add(1);
                }
            }
        }

        PackQualityMetrics {
            item_count,
            omitted_count,
            used_tokens: self.used_tokens,
            max_tokens: self.budget.max_tokens(),
            budget_utilization: token_ratio(self.used_tokens, self.budget.max_tokens()),
            average_relevance: average_metric(relevance_sum, item_count),
            average_utility: average_metric(utility_sum, item_count),
            provenance_source_count,
            provenance_sources_per_item: count_ratio(provenance_source_count, item_count),
            provenance_complete: self.items.iter().all(|item| !item.provenance.is_empty()),
            sections: PackSection::all()
                .into_iter()
                .map(|section| self.section_quality_metric(section))
                .collect(),
            omissions: PackOmissionMetrics {
                token_budget_exceeded,
                redundant_candidates,
            },
        }
    }

    #[must_use]
    pub fn provenance_footer(&self) -> PackProvenanceFooter {
        let mut memory_ids = BTreeSet::new();
        let mut schemes = BTreeSet::new();
        let mut entries = Vec::new();

        for item in &self.items {
            memory_ids.insert(item.memory_id.to_string());
            for (index, provenance) in item.provenance.iter().enumerate() {
                let source = provenance.rendered();
                schemes.insert(source.scheme);
                entries.push(PackItemProvenance {
                    rank: item.rank,
                    memory_id: item.memory_id,
                    source_index: source_index(index),
                    source,
                });
            }
        }

        PackProvenanceFooter {
            memory_count: memory_ids.len(),
            source_count: entries.len(),
            schemes: schemes.into_iter().collect(),
            entries,
        }
    }

    fn section_quality_metric(&self, section: PackSection) -> PackSectionMetric {
        let mut item_count = 0_usize;
        let mut used_tokens = 0_u32;
        for item in &self.items {
            if item.section == section {
                item_count = item_count.saturating_add(1);
                used_tokens = used_tokens.saturating_add(item.estimated_tokens);
            }
        }

        PackSectionMetric {
            section,
            item_count,
            used_tokens,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PackQualityMetrics {
    pub item_count: usize,
    pub omitted_count: usize,
    pub used_tokens: u32,
    pub max_tokens: u32,
    pub budget_utilization: f32,
    pub average_relevance: f32,
    pub average_utility: f32,
    pub provenance_source_count: usize,
    pub provenance_sources_per_item: f32,
    pub provenance_complete: bool,
    pub sections: Vec<PackSectionMetric>,
    pub omissions: PackOmissionMetrics,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackSectionMetric {
    pub section: PackSection,
    pub item_count: usize,
    pub used_tokens: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackOmissionMetrics {
    pub token_budget_exceeded: usize,
    pub redundant_candidates: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextResponse {
    pub schema: &'static str,
    pub success: bool,
    pub data: ContextResponseData,
}

impl ContextResponse {
    /// Build a stable successful `ee context` response.
    ///
    /// # Errors
    ///
    /// Returns [`PackValidationError::ContextResponseQueryMismatch`] if
    /// the request query and draft query differ. A response must carry
    /// exactly the request that produced the pack so later `ee why`
    /// explanations can trust the provenance chain.
    pub fn new(
        request: ContextRequest,
        pack: PackDraft,
        degraded: Vec<ContextResponseDegradation>,
    ) -> Result<Self, PackValidationError> {
        if request.query != pack.query {
            return Err(PackValidationError::ContextResponseQueryMismatch {
                request_query: request.query,
                draft_query: pack.query,
            });
        }
        Ok(Self {
            schema: RESPONSE_SCHEMA_V1,
            success: true,
            data: ContextResponseData {
                command: CONTEXT_COMMAND,
                request,
                pack,
                degraded,
            },
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextResponseData {
    pub command: &'static str,
    pub request: ContextRequest,
    pub pack: PackDraft,
    pub degraded: Vec<ContextResponseDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextResponseDegradation {
    pub code: String,
    pub severity: ContextResponseSeverity,
    pub message: String,
    pub repair: Option<String>,
}

impl ContextResponseDegradation {
    /// Build a validated degradation entry for a context response.
    ///
    /// # Errors
    ///
    /// Returns a [`PackValidationError`] when `code` or `message` is
    /// empty after trimming.
    pub fn new(
        code: impl Into<String>,
        severity: ContextResponseSeverity,
        message: impl Into<String>,
        repair: Option<String>,
    ) -> Result<Self, PackValidationError> {
        let code = trim_required(code.into(), PackValidationError::EmptyDegradationCode)?;
        let message = trim_required(
            message.into(),
            PackValidationError::EmptyDegradationMessage { code: code.clone() },
        )?;
        Ok(Self {
            code,
            severity,
            message,
            repair: repair
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextResponseSeverity {
    Low,
    Medium,
    High,
}

impl ContextResponseSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

impl fmt::Display for ContextResponseSeverity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PackDraftItem {
    pub rank: u32,
    pub memory_id: MemoryId,
    pub section: PackSection,
    pub content: String,
    pub estimated_tokens: u32,
    pub relevance: UnitScore,
    pub utility: UnitScore,
    pub provenance: Vec<PackProvenance>,
    pub why: String,
    pub diversity_key: Option<String>,
}

impl PackDraftItem {
    #[must_use]
    pub fn rendered_provenance(&self) -> Vec<RenderedPackProvenance> {
        self.provenance
            .iter()
            .map(PackProvenance::rendered)
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackOmission {
    pub memory_id: MemoryId,
    pub estimated_tokens: u32,
    pub reason: PackOmissionReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackOmissionReason {
    TokenBudgetExceeded,
    RedundantCandidate,
}

impl PackOmissionReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TokenBudgetExceeded => "token_budget_exceeded",
            Self::RedundantCandidate => "redundant_candidate",
        }
    }
}

impl fmt::Display for PackOmissionReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn rendered_provenance_label(uri: &ProvenanceUri) -> (String, Option<String>) {
    match uri {
        ProvenanceUri::CassSession { session, span } => {
            let locator = span.map(line_span_locator);
            let label = match locator.as_deref() {
                Some(locator) => format!("cass-session {session}#{locator}"),
                None => format!("cass-session {session}"),
            };
            (label, locator)
        }
        ProvenanceUri::File { path, span } => {
            let locator = span.map(line_span_locator);
            let label = match locator.as_deref() {
                Some(locator) => format!("{path}:{locator}"),
                None => path.clone(),
            };
            (label, locator)
        }
        ProvenanceUri::EeMemory(id) => (format!("memory {id}"), None),
        ProvenanceUri::Web { url } => (url.clone(), None),
        ProvenanceUri::AgentMail { thread, message } => {
            let locator = message.clone();
            let label = match message {
                Some(message) => format!("agent-mail {thread}/{message}"),
                None => format!("agent-mail {thread}"),
            };
            (label, locator)
        }
    }
}

fn line_span_locator(span: crate::models::LineSpan) -> String {
    match span.end {
        Some(end) if end != span.start => format!("L{}-{}", span.start, end),
        _ => format!("L{}", span.start),
    }
}

fn source_index(index: usize) -> u32 {
    u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX)
}

fn token_ratio(numerator: u32, denominator: u32) -> f32 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f32 / denominator as f32
    }
}

fn count_ratio(numerator: usize, denominator: usize) -> f32 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f32 / denominator as f32
    }
}

fn average_metric(sum: f32, count: usize) -> f32 {
    if count == 0 { 0.0 } else { sum / count as f32 }
}

/// Assemble a deterministic context-pack draft from validated candidates.
///
/// Selection uses deterministic MMR-style redundancy control: the first
/// item follows the stable relevance/utility order, then later candidates
/// are penalized when they overlap selected memories by memory id, explicit
/// diversity key, or exact normalized content. Redundant candidates are
/// omitted even when the token budget has room.
///
/// # Errors
///
/// Returns [`PackValidationError::EmptyQuery`] if `query` is empty after
/// trimming.
pub fn assemble_draft(
    query: impl Into<String>,
    budget: TokenBudget,
    candidates: impl IntoIterator<Item = PackCandidate>,
) -> Result<PackDraft, PackValidationError> {
    let query = trim_required(query.into(), PackValidationError::EmptyQuery)?;
    let mut candidates: Vec<PackCandidate> = candidates.into_iter().collect();
    candidates.sort_by(compare_candidates);

    let mut used_tokens = 0_u32;
    let mut next_rank = 1_u32;
    let mut selected_signatures = Vec::new();
    let mut items = Vec::new();
    let mut omitted = Vec::new();

    while !candidates.is_empty() {
        let candidate_index = select_next_candidate_index(&candidates, &selected_signatures);
        let candidate = candidates.remove(candidate_index);
        if is_redundant(&candidate, &selected_signatures) {
            omitted.push(PackOmission {
                memory_id: candidate.memory_id,
                estimated_tokens: candidate.estimated_tokens,
                reason: PackOmissionReason::RedundantCandidate,
            });
            continue;
        }

        match used_tokens.checked_add(candidate.estimated_tokens) {
            Some(total) if total <= budget.max_tokens() => {
                let rank = next_rank;
                next_rank = next_rank
                    .checked_add(1)
                    .ok_or(PackValidationError::CandidateRankOverflow)?;
                used_tokens = total;
                selected_signatures.push(CandidateSignature::from(&candidate));
                items.push(PackDraftItem {
                    rank,
                    memory_id: candidate.memory_id,
                    section: candidate.section,
                    content: candidate.content,
                    estimated_tokens: candidate.estimated_tokens,
                    relevance: candidate.relevance,
                    utility: candidate.utility,
                    provenance: candidate.provenance,
                    why: candidate.why,
                    diversity_key: candidate.diversity_key,
                });
            }
            _ => omitted.push(PackOmission {
                memory_id: candidate.memory_id,
                estimated_tokens: candidate.estimated_tokens,
                reason: PackOmissionReason::TokenBudgetExceeded,
            }),
        }
    }

    Ok(PackDraft {
        query,
        budget,
        used_tokens,
        items,
        omitted,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CandidateSignature {
    memory_id: MemoryId,
    diversity_key: Option<String>,
    normalized_content: String,
}

impl From<&PackCandidate> for CandidateSignature {
    fn from(candidate: &PackCandidate) -> Self {
        Self {
            memory_id: candidate.memory_id,
            diversity_key: candidate.diversity_key.clone(),
            normalized_content: normalize_redundancy_content(&candidate.content),
        }
    }
}

fn select_next_candidate_index(
    candidates: &[PackCandidate],
    selected: &[CandidateSignature],
) -> usize {
    let mut best_index = 0_usize;
    for (candidate_index, candidate) in candidates.iter().enumerate().skip(1) {
        let best = &candidates[best_index];
        if compare_candidates_with_redundancy(candidate, best, selected) == Ordering::Less {
            best_index = candidate_index;
        }
    }
    best_index
}

fn compare_candidates_with_redundancy(
    left: &PackCandidate,
    right: &PackCandidate,
    selected: &[CandidateSignature],
) -> Ordering {
    let left_score = redundancy_adjusted_score(left, selected);
    let right_score = redundancy_adjusted_score(right, selected);
    right_score
        .total_cmp(&left_score)
        .then_with(|| compare_candidates(left, right))
}

fn redundancy_adjusted_score(candidate: &PackCandidate, selected: &[CandidateSignature]) -> f32 {
    let relevance_score = candidate.relevance.into_inner();
    let max_similarity = max_selected_similarity(candidate, selected);
    (DEFAULT_MMR_RELEVANCE_WEIGHT * relevance_score)
        - ((1.0 - DEFAULT_MMR_RELEVANCE_WEIGHT) * max_similarity)
}

fn is_redundant(candidate: &PackCandidate, selected: &[CandidateSignature]) -> bool {
    max_selected_similarity(candidate, selected) >= 1.0
}

fn max_selected_similarity(candidate: &PackCandidate, selected: &[CandidateSignature]) -> f32 {
    selected
        .iter()
        .map(|signature| candidate_similarity(candidate, signature))
        .fold(0.0_f32, f32::max)
}

fn candidate_similarity(candidate: &PackCandidate, selected: &CandidateSignature) -> f32 {
    if candidate.memory_id == selected.memory_id {
        return 1.0;
    }

    if let Some(diversity_key) = &candidate.diversity_key
        && selected.diversity_key.as_ref() == Some(diversity_key)
    {
        return 1.0;
    }

    if normalize_redundancy_content(&candidate.content) == selected.normalized_content {
        return 1.0;
    }

    0.0
}

fn normalize_redundancy_content(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn compare_candidates(left: &PackCandidate, right: &PackCandidate) -> Ordering {
    right
        .relevance
        .into_inner()
        .total_cmp(&left.relevance.into_inner())
        .then_with(|| {
            right
                .utility
                .into_inner()
                .total_cmp(&left.utility.into_inner())
        })
        .then_with(|| left.section.cmp(&right.section))
        .then_with(|| left.memory_id.to_string().cmp(&right.memory_id.to_string()))
}

fn trim_required(value: String, error: PackValidationError) -> Result<String, PackValidationError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(error);
    }
    Ok(trimmed.to_string())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackValidationError {
    EmptyQuery,
    ZeroTokenBudget,
    ZeroCandidatePool,
    EmptyCandidateContent {
        memory_id: MemoryId,
    },
    ZeroCandidateTokens {
        memory_id: MemoryId,
    },
    MissingProvenance {
        memory_id: MemoryId,
    },
    EmptyProvenanceNote {
        uri: ProvenanceUri,
    },
    MissingWhy {
        memory_id: MemoryId,
    },
    CandidateRankOverflow,
    ContextResponseQueryMismatch {
        request_query: String,
        draft_query: String,
    },
    EmptyDegradationCode,
    EmptyDegradationMessage {
        code: String,
    },
}

impl fmt::Display for PackValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyQuery => formatter.write_str("context query must not be empty"),
            Self::ZeroTokenBudget => formatter.write_str("context token budget must be non-zero"),
            Self::ZeroCandidatePool => {
                formatter.write_str("context candidate pool must be non-zero")
            }
            Self::EmptyCandidateContent { memory_id } => {
                write!(formatter, "pack candidate `{memory_id}` has empty content")
            }
            Self::ZeroCandidateTokens { memory_id } => {
                write!(
                    formatter,
                    "pack candidate `{memory_id}` has zero estimated tokens"
                )
            }
            Self::MissingProvenance { memory_id } => {
                write!(formatter, "pack candidate `{memory_id}` has no provenance")
            }
            Self::EmptyProvenanceNote { uri } => {
                write!(formatter, "pack provenance `{uri}` has an empty note")
            }
            Self::MissingWhy { memory_id } => {
                write!(
                    formatter,
                    "pack candidate `{memory_id}` is missing a why explanation"
                )
            }
            Self::CandidateRankOverflow => {
                formatter.write_str("context pack contains too many ranked candidates")
            }
            Self::ContextResponseQueryMismatch {
                request_query,
                draft_query,
            } => write!(
                formatter,
                "context response request query `{request_query}` does not match pack query `{draft_query}`"
            ),
            Self::EmptyDegradationCode => {
                formatter.write_str("context response degradation code must not be empty")
            }
            Self::EmptyDegradationMessage { code } => write!(
                formatter,
                "context response degradation `{code}` message must not be empty"
            ),
        }
    }
}

impl std::error::Error for PackValidationError {}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use uuid::Uuid;

    use super::{
        CONTEXT_COMMAND, ContextPackProfile, ContextRequest, ContextRequestInput, ContextResponse,
        ContextResponseDegradation, ContextResponseSeverity, DEFAULT_CHARS_PER_TOKEN,
        PackCandidate, PackCandidateInput, PackOmissionReason, PackProvenance, PackSection,
        PackValidationError, SectionQuota, SectionQuotas, TokenBudget, TokenEstimationStrategy,
        assemble_draft, estimate_tokens, estimate_tokens_default, subsystem_name,
    };
    use crate::models::{MemoryId, ProvenanceUri, UnitScore};

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
    where
        T: std::fmt::Debug + PartialEq,
    {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn ensure_close(actual: f32, expected: f32, context: &str) -> TestResult {
        if (actual - expected).abs() <= 0.000_001 {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn memory_id(seed: u128) -> MemoryId {
        MemoryId::from_uuid(Uuid::from_u128(seed))
    }

    fn score(value: f32) -> Result<UnitScore, String> {
        UnitScore::parse(value).map_err(|error| format!("test score rejected: {error:?}"))
    }

    fn provenance(path: &str) -> Result<PackProvenance, String> {
        let uri = ProvenanceUri::from_str(path)
            .map_err(|error| format!("test provenance URI rejected: {error:?}"))?;
        PackProvenance::new(uri, "source evidence")
            .map_err(|error| format!("test provenance note rejected: {error:?}"))
    }

    fn candidate_input(
        memory_id: MemoryId,
        section: PackSection,
        content: impl Into<String>,
        estimated_tokens: u32,
        provenance: Vec<PackProvenance>,
        why: impl Into<String>,
    ) -> Result<PackCandidateInput, String> {
        Ok(PackCandidateInput {
            memory_id,
            section,
            content: content.into(),
            estimated_tokens,
            relevance: score(0.8)?,
            utility: score(0.5)?,
            provenance,
            why: why.into(),
        })
    }

    fn candidate(
        seed: u128,
        relevance: f32,
        utility: f32,
        tokens: u32,
    ) -> Result<PackCandidate, String> {
        candidate_with_content(seed, relevance, utility, tokens, format!("memory {seed}"))
    }

    fn candidate_with_content(
        seed: u128,
        relevance: f32,
        utility: f32,
        tokens: u32,
        content: impl Into<String>,
    ) -> Result<PackCandidate, String> {
        PackCandidate::new(PackCandidateInput {
            memory_id: memory_id(seed),
            section: PackSection::ProceduralRules,
            content: content.into(),
            estimated_tokens: tokens,
            relevance: score(relevance)?,
            utility: score(utility)?,
            provenance: vec![provenance("file://AGENTS.md#L1")?],
            why: format!("selected because memory {seed} matches the task"),
        })
        .map_err(|error| format!("test candidate rejected: {error:?}"))
    }

    fn candidate_in_section(
        seed: u128,
        section: PackSection,
        relevance: f32,
        utility: f32,
        tokens: u32,
        content: impl Into<String>,
    ) -> Result<PackCandidate, String> {
        PackCandidate::new(PackCandidateInput {
            memory_id: memory_id(seed),
            section,
            content: content.into(),
            estimated_tokens: tokens,
            relevance: score(relevance)?,
            utility: score(utility)?,
            provenance: vec![provenance("file://AGENTS.md#L1")?],
            why: format!("selected because memory {seed} matches the task"),
        })
        .map_err(|error| format!("test candidate rejected: {error:?}"))
    }

    #[test]
    fn subsystem_name_is_stable() -> TestResult {
        ensure_equal(&subsystem_name(), &"pack", "subsystem name")
    }

    #[test]
    fn default_chars_per_token_is_conservative() -> TestResult {
        ensure(
            DEFAULT_CHARS_PER_TOKEN < 4.0,
            "default ratio should be below 4.0 for conservative estimation",
        )?;
        ensure(
            DEFAULT_CHARS_PER_TOKEN > 2.0,
            "default ratio should be above 2.0 to avoid extreme overestimation",
        )
    }

    #[test]
    fn token_estimation_strategy_strings_are_stable() -> TestResult {
        ensure_equal(
            &TokenEstimationStrategy::CharacterHeuristic.as_str(),
            &"character_heuristic",
            "character heuristic strategy",
        )?;
        ensure_equal(
            &TokenEstimationStrategy::WordHeuristic.as_str(),
            &"word_heuristic",
            "word heuristic strategy",
        )?;
        ensure_equal(
            &TokenEstimationStrategy::all().len(),
            &2,
            "all strategies count",
        )?;
        ensure_equal(
            &TokenEstimationStrategy::default(),
            &TokenEstimationStrategy::CharacterHeuristic,
            "default strategy",
        )
    }

    #[test]
    fn estimate_tokens_returns_zero_for_empty_input() -> TestResult {
        ensure_equal(
            &estimate_tokens("", TokenEstimationStrategy::CharacterHeuristic),
            &0,
            "empty string",
        )?;
        ensure_equal(
            &estimate_tokens("   ", TokenEstimationStrategy::CharacterHeuristic),
            &0,
            "whitespace only",
        )?;
        ensure_equal(
            &estimate_tokens("\n\t", TokenEstimationStrategy::WordHeuristic),
            &0,
            "whitespace with word heuristic",
        )
    }

    #[test]
    fn estimate_tokens_returns_at_least_one_for_non_empty() -> TestResult {
        ensure(
            estimate_tokens("x", TokenEstimationStrategy::CharacterHeuristic) >= 1,
            "single char should estimate at least 1 token",
        )?;
        ensure(
            estimate_tokens("word", TokenEstimationStrategy::WordHeuristic) >= 1,
            "single word should estimate at least 1 token",
        )
    }

    #[test]
    fn estimate_tokens_character_heuristic_is_deterministic() -> TestResult {
        let content = "This is a test string for token estimation.";
        let first = estimate_tokens(content, TokenEstimationStrategy::CharacterHeuristic);
        let second = estimate_tokens(content, TokenEstimationStrategy::CharacterHeuristic);
        ensure_equal(&first, &second, "deterministic estimation")
    }

    #[test]
    fn estimate_tokens_character_heuristic_scales_with_length() -> TestResult {
        let short = estimate_tokens("hello", TokenEstimationStrategy::CharacterHeuristic);
        let long = estimate_tokens(
            "hello world this is a much longer string",
            TokenEstimationStrategy::CharacterHeuristic,
        );
        ensure(long > short, "longer content should estimate more tokens")
    }

    #[test]
    fn estimate_tokens_word_heuristic_counts_words() -> TestResult {
        let one_word = estimate_tokens("hello", TokenEstimationStrategy::WordHeuristic);
        let five_words = estimate_tokens(
            "one two three four five",
            TokenEstimationStrategy::WordHeuristic,
        );
        ensure(
            five_words > one_word,
            "more words should estimate more tokens",
        )?;
        ensure(
            five_words >= 5,
            "five words should estimate at least 5 tokens (with 1.3 multiplier)",
        )
    }

    #[test]
    fn estimate_tokens_default_uses_character_heuristic() -> TestResult {
        let content = "test content";
        let default_result = estimate_tokens_default(content);
        let explicit_result = estimate_tokens(content, TokenEstimationStrategy::CharacterHeuristic);
        ensure_equal(
            &default_result,
            &explicit_result,
            "default matches character heuristic",
        )
    }

    #[test]
    fn estimate_tokens_trims_input_before_counting() -> TestResult {
        let clean = estimate_tokens("hello world", TokenEstimationStrategy::CharacterHeuristic);
        let padded = estimate_tokens(
            "  hello world  ",
            TokenEstimationStrategy::CharacterHeuristic,
        );
        ensure_equal(&clean, &padded, "trimmed content should match")
    }

    #[test]
    fn section_quota_unlimited_has_no_constraints() -> TestResult {
        let unlimited = SectionQuota::unlimited();
        ensure(
            unlimited.is_unlimited(),
            "unlimited should report as unlimited",
        )?;
        ensure(
            !unlimited.exceeds_max(1_000_000),
            "unlimited should not exceed max",
        )?;
        ensure_equal(
            &unlimited.remaining(1000),
            &u32::MAX,
            "unlimited remaining should be u32::MAX",
        )
    }

    #[test]
    fn section_quota_capped_enforces_maximum() -> TestResult {
        let capped = SectionQuota::capped(100);
        ensure(!capped.is_unlimited(), "capped should not be unlimited")?;
        ensure(!capped.exceeds_max(100), "100 should not exceed max of 100")?;
        ensure(capped.exceeds_max(101), "101 should exceed max of 100")?;
        ensure_equal(
            &capped.remaining(50),
            &50,
            "remaining after using 50 of 100",
        )?;
        ensure_equal(&capped.remaining(100), &0, "remaining after using all")?;
        ensure_equal(&capped.remaining(150), &0, "remaining when over quota")
    }

    #[test]
    fn section_quota_new_accepts_min_and_max() -> TestResult {
        let quota = SectionQuota::new(10, 100);
        ensure_equal(&quota.min_tokens, &10, "min tokens")?;
        ensure_equal(&quota.max_tokens, &100, "max tokens")
    }

    #[test]
    fn section_quotas_unlimited_allows_everything() -> TestResult {
        let quotas = SectionQuotas::unlimited();
        for section in PackSection::all() {
            ensure(
                quotas.get(section).is_unlimited(),
                format!("{section} should be unlimited"),
            )?;
            ensure(
                quotas.has_room(section, 10000, 10000),
                format!("{section} should have room"),
            )?;
        }
        Ok(())
    }

    #[test]
    fn section_quotas_balanced_allocates_percentages() -> TestResult {
        let quotas = SectionQuotas::balanced(1000);

        let procedural = quotas.get(PackSection::ProceduralRules);
        ensure(
            procedural.max_tokens >= 290 && procedural.max_tokens <= 310,
            format!(
                "procedural_rules should be ~30% (got {})",
                procedural.max_tokens
            ),
        )?;

        let decisions = quotas.get(PackSection::Decisions);
        ensure(
            decisions.max_tokens >= 190 && decisions.max_tokens <= 210,
            format!("decisions should be ~20% (got {})", decisions.max_tokens),
        )?;

        let artifacts = quotas.get(PackSection::Artifacts);
        ensure(
            artifacts.max_tokens >= 90 && artifacts.max_tokens <= 110,
            format!("artifacts should be ~10% (got {})", artifacts.max_tokens),
        )
    }

    #[test]
    fn section_quotas_compact_prioritizes_procedural_rules() -> TestResult {
        let quotas = SectionQuotas::compact(1000);

        let procedural = quotas.get(PackSection::ProceduralRules);
        ensure(
            procedural.max_tokens >= 490 && procedural.max_tokens <= 510,
            format!(
                "procedural_rules should be ~50% in compact (got {})",
                procedural.max_tokens
            ),
        )
    }

    #[test]
    fn section_quotas_thorough_is_more_even() -> TestResult {
        let quotas = SectionQuotas::thorough(1000);

        let procedural = quotas.get(PackSection::ProceduralRules);
        let evidence = quotas.get(PackSection::Evidence);

        ensure(
            procedural.max_tokens >= 190 && procedural.max_tokens <= 210,
            format!(
                "procedural_rules should be ~20% in thorough (got {})",
                procedural.max_tokens
            ),
        )?;
        ensure(
            evidence.max_tokens >= 240 && evidence.max_tokens <= 260,
            format!(
                "evidence should be ~25% in thorough (got {})",
                evidence.max_tokens
            ),
        )
    }

    #[test]
    fn section_quotas_for_profile_dispatches_correctly() -> TestResult {
        let compact = SectionQuotas::for_profile(ContextPackProfile::Compact, 1000);
        let balanced = SectionQuotas::for_profile(ContextPackProfile::Balanced, 1000);
        let thorough = SectionQuotas::for_profile(ContextPackProfile::Thorough, 1000);

        ensure(
            compact.get(PackSection::ProceduralRules).max_tokens
                > balanced.get(PackSection::ProceduralRules).max_tokens,
            "compact should give more to procedural_rules than balanced",
        )?;
        ensure(
            thorough.get(PackSection::Evidence).max_tokens
                > balanced.get(PackSection::Evidence).max_tokens,
            "thorough should give more to evidence than balanced",
        )
    }

    #[test]
    fn section_quotas_has_room_checks_capacity() -> TestResult {
        let quotas = SectionQuotas::balanced(100);
        let section = PackSection::ProceduralRules;
        let max = quotas.get(section).max_tokens;

        ensure(
            quotas.has_room(section, 0, max),
            "should have room for max tokens when unused",
        )?;
        ensure(
            !quotas.has_room(section, 0, max + 1),
            "should not have room for more than max",
        )?;
        ensure(
            quotas.has_room(section, max - 10, 10),
            "should have room for exactly remaining",
        )?;
        ensure(
            !quotas.has_room(section, max - 10, 11),
            "should not have room when would exceed",
        )
    }

    #[test]
    fn section_quotas_remaining_tracks_usage() -> TestResult {
        let quotas = SectionQuotas::balanced(100);
        let section = PackSection::ProceduralRules;
        let max = quotas.get(section).max_tokens;

        ensure_equal(
            &quotas.remaining(section, 0),
            &max,
            "remaining when unused equals max",
        )?;
        ensure_equal(
            &quotas.remaining(section, max),
            &0,
            "remaining when fully used is 0",
        )?;
        ensure_equal(
            &quotas.remaining(section, max + 10),
            &0,
            "remaining when over quota is 0",
        )
    }

    #[test]
    fn profile_and_section_wire_names_are_stable() -> TestResult {
        ensure_equal(
            &ContextPackProfile::Compact.as_str(),
            &"compact",
            "compact profile",
        )?;
        ensure_equal(
            &ContextPackProfile::Balanced.to_string().as_str(),
            &"balanced",
            "balanced profile display",
        )?;
        ensure_equal(
            &PackSection::all().map(PackSection::as_str),
            &[
                "procedural_rules",
                "decisions",
                "failures",
                "evidence",
                "artifacts",
            ],
            "section order",
        )
    }

    #[test]
    fn token_budget_rejects_zero_and_keeps_default_stable() -> TestResult {
        let zero = TokenBudget::new(0);
        ensure(
            matches!(zero, Err(PackValidationError::ZeroTokenBudget)),
            "zero token budget must be rejected",
        )?;
        ensure_equal(
            &TokenBudget::default_context().max_tokens(),
            &4_000,
            "default context budget",
        )
    }

    #[test]
    fn context_request_defaults_are_stable() -> TestResult {
        let request = ContextRequest::from_query(" prepare release ")
            .map_err(|error| format!("request rejected: {error:?}"))?;

        ensure_equal(&request.query.as_str(), &"prepare release", "trimmed query")?;
        ensure_equal(
            &request.profile,
            &ContextPackProfile::Balanced,
            "default profile",
        )?;
        ensure_equal(&request.budget.max_tokens(), &4_000, "default max tokens")?;
        ensure_equal(&request.candidate_pool, &64, "default candidate pool")?;
        ensure_equal(
            &request.sections,
            &PackSection::all().to_vec(),
            "default sections",
        )
    }

    #[test]
    fn context_request_accepts_explicit_profile_budget_pool_and_sections() -> TestResult {
        let request = ContextRequest::new(ContextRequestInput {
            query: "fix release workflow".to_string(),
            profile: Some(ContextPackProfile::Thorough),
            max_tokens: Some(8_000),
            candidate_pool: Some(12),
            sections: vec![PackSection::ProceduralRules, PackSection::Failures],
        })
        .map_err(|error| format!("request rejected: {error:?}"))?;

        ensure_equal(
            &request.profile,
            &ContextPackProfile::Thorough,
            "explicit profile",
        )?;
        ensure_equal(&request.budget.max_tokens(), &8_000, "explicit max tokens")?;
        ensure_equal(&request.candidate_pool, &12, "explicit candidate pool")?;
        ensure_equal(
            &request.sections,
            &vec![PackSection::ProceduralRules, PackSection::Failures],
            "explicit sections",
        )
    }

    #[test]
    fn context_request_rejects_empty_query_and_zero_limits() -> TestResult {
        let empty_query = ContextRequest::from_query(" ");
        ensure(
            matches!(empty_query, Err(PackValidationError::EmptyQuery)),
            "empty context query must be rejected",
        )?;

        let zero_budget = ContextRequest::new(ContextRequestInput {
            query: "task".to_string(),
            profile: None,
            max_tokens: Some(0),
            candidate_pool: None,
            sections: Vec::new(),
        });
        ensure(
            matches!(zero_budget, Err(PackValidationError::ZeroTokenBudget)),
            "zero max tokens must be rejected",
        )?;

        let zero_pool = ContextRequest::new(ContextRequestInput {
            query: "task".to_string(),
            profile: None,
            max_tokens: None,
            candidate_pool: Some(0),
            sections: Vec::new(),
        });
        ensure(
            matches!(zero_pool, Err(PackValidationError::ZeroCandidatePool)),
            "zero candidate pool must be rejected",
        )
    }

    #[test]
    fn candidate_requires_content_provenance_tokens_and_why() -> TestResult {
        let id = memory_id(7);
        let base_provenance = vec![provenance("file://src/lib.rs#L1")?];

        let empty_content = PackCandidate::new(candidate_input(
            id,
            PackSection::Evidence,
            " ",
            5,
            base_provenance.clone(),
            "matches query",
        )?);
        ensure(
            matches!(
                empty_content,
                Err(PackValidationError::EmptyCandidateContent { .. })
            ),
            "empty content must be rejected",
        )?;

        let zero_tokens = PackCandidate::new(candidate_input(
            id,
            PackSection::Evidence,
            "memory",
            0,
            base_provenance.clone(),
            "matches query",
        )?);
        ensure(
            matches!(
                zero_tokens,
                Err(PackValidationError::ZeroCandidateTokens { .. })
            ),
            "zero-token candidate must be rejected",
        )?;

        let no_provenance = PackCandidate::new(candidate_input(
            id,
            PackSection::Evidence,
            "memory",
            5,
            Vec::new(),
            "matches query",
        )?);
        ensure(
            matches!(
                no_provenance,
                Err(PackValidationError::MissingProvenance { .. })
            ),
            "missing provenance must be rejected",
        )?;

        let no_why = PackCandidate::new(candidate_input(
            id,
            PackSection::Evidence,
            "memory",
            5,
            base_provenance,
            " ",
        )?);
        ensure(
            matches!(no_why, Err(PackValidationError::MissingWhy { .. })),
            "missing why must be rejected",
        )
    }

    #[test]
    fn pack_provenance_rendering_labels_sources() -> TestResult {
        let source = provenance("file://src/lib.rs#L42")?;
        let rendered = source.rendered();
        let locator = rendered.locator.as_deref();

        ensure_equal(
            &rendered.uri.as_str(),
            &"file://src/lib.rs#L42",
            "rendered URI",
        )?;
        ensure_equal(&rendered.scheme, &"file", "rendered scheme")?;
        ensure_equal(
            &rendered.label.as_str(),
            &"src/lib.rs:L42",
            "rendered label",
        )?;
        ensure_equal(&locator, &Some("L42"), "rendered locator")?;
        ensure_equal(&rendered.note.as_str(), &"source evidence", "rendered note")
    }

    #[test]
    fn pack_provenance_footer_is_deterministic() -> TestResult {
        let id = memory_id(8);
        let candidate = PackCandidate::new(candidate_input(
            id,
            PackSection::Evidence,
            "Use the AGENTS.md release rule before shipping.",
            12,
            vec![
                provenance("file://AGENTS.md#L10")?,
                provenance("cass-session://session-a#L20-22")?,
            ],
            "selected because release rules match the query",
        )?)
        .map_err(|error| format!("candidate rejected: {error:?}"))?;
        let budget =
            TokenBudget::new(100).map_err(|error| format!("budget rejected: {error:?}"))?;
        let draft = assemble_draft("prepare release", budget, vec![candidate])
            .map_err(|error| format!("draft rejected: {error:?}"))?;

        let footer = draft.provenance_footer();

        ensure_equal(&footer.memory_count, &1, "footer memory count")?;
        ensure_equal(&footer.source_count, &2, "footer source count")?;
        ensure_equal(
            &footer.schemes,
            &vec!["cass-session", "file"],
            "footer schemes",
        )?;
        ensure_equal(
            &footer.entries.first().map(|entry| entry.rank),
            &Some(1),
            "first footer rank",
        )?;
        ensure_equal(
            &footer.entries.first().map(|entry| entry.source_index),
            &Some(1),
            "first source index",
        )?;
        ensure_equal(
            &footer
                .entries
                .get(1)
                .map(|entry| entry.source.label.as_str()),
            &Some("cass-session session-a#L20-22"),
            "second source label",
        )
    }

    #[test]
    fn pack_quality_metrics_summarize_selected_and_omitted_items() -> TestResult {
        let budget = TokenBudget::new(25).map_err(|error| format!("budget rejected: {error:?}"))?;
        let first = candidate_in_section(
            1,
            PackSection::ProceduralRules,
            1.0,
            0.5,
            10,
            "Run cargo fmt --check before release.",
        )?
        .with_diversity_key("release-formatting");
        let redundant = candidate_in_section(
            2,
            PackSection::ProceduralRules,
            0.9,
            0.6,
            5,
            "Repeat the release formatting rule.",
        )?
        .with_diversity_key("release-formatting");
        let evidence = candidate_in_section(
            3,
            PackSection::Evidence,
            0.8,
            0.7,
            12,
            "The release checklist includes formatting evidence.",
        )?;
        let over_budget = candidate_in_section(
            4,
            PackSection::Failures,
            0.7,
            0.4,
            10,
            "A prior release failed after skipping formatter checks.",
        )?;

        let draft = assemble_draft(
            "prepare release",
            budget,
            vec![redundant, over_budget, evidence, first],
        )
        .map_err(|error| format!("draft rejected: {error:?}"))?;
        let metrics = draft.quality_metrics();

        ensure_equal(&metrics.item_count, &2, "metric item count")?;
        ensure_equal(&metrics.omitted_count, &2, "metric omitted count")?;
        ensure_equal(&metrics.used_tokens, &22, "metric used tokens")?;
        ensure_equal(&metrics.max_tokens, &25, "metric max tokens")?;
        ensure_close(metrics.budget_utilization, 0.88, "budget utilization")?;
        ensure_close(metrics.average_relevance, 0.9, "average relevance")?;
        ensure_close(metrics.average_utility, 0.6, "average utility")?;
        ensure_equal(
            &metrics.provenance_source_count,
            &2,
            "provenance source count",
        )?;
        ensure_close(
            metrics.provenance_sources_per_item,
            1.0,
            "provenance sources per item",
        )?;
        ensure(
            metrics.provenance_complete,
            "selected items have provenance",
        )?;

        let procedural = metrics
            .sections
            .iter()
            .find(|metric| metric.section == PackSection::ProceduralRules)
            .ok_or_else(|| "missing procedural section metric".to_string())?;
        ensure_equal(&procedural.item_count, &1, "procedural item count")?;
        ensure_equal(&procedural.used_tokens, &10, "procedural tokens")?;

        let evidence = metrics
            .sections
            .iter()
            .find(|metric| metric.section == PackSection::Evidence)
            .ok_or_else(|| "missing evidence section metric".to_string())?;
        ensure_equal(&evidence.item_count, &1, "evidence item count")?;
        ensure_equal(&evidence.used_tokens, &12, "evidence tokens")?;

        ensure_equal(
            &metrics.omissions.token_budget_exceeded,
            &1,
            "budget omission count",
        )?;
        ensure_equal(
            &metrics.omissions.redundant_candidates,
            &1,
            "redundant omission count",
        )
    }

    #[test]
    fn pack_quality_metrics_are_stable_for_empty_draft() -> TestResult {
        let budget =
            TokenBudget::new(100).map_err(|error| format!("budget rejected: {error:?}"))?;
        let draft = assemble_draft("empty", budget, Vec::<PackCandidate>::new())
            .map_err(|error| format!("draft rejected: {error:?}"))?;
        let metrics = draft.quality_metrics();

        ensure_equal(&metrics.item_count, &0, "empty item count")?;
        ensure_equal(&metrics.omitted_count, &0, "empty omitted count")?;
        ensure_equal(&metrics.used_tokens, &0, "empty used tokens")?;
        ensure_equal(&metrics.max_tokens, &100, "empty max tokens")?;
        ensure_close(metrics.budget_utilization, 0.0, "empty utilization")?;
        ensure_close(metrics.average_relevance, 0.0, "empty relevance")?;
        ensure_close(metrics.average_utility, 0.0, "empty utility")?;
        ensure_equal(
            &metrics.provenance_source_count,
            &0,
            "empty provenance source count",
        )?;
        ensure_close(
            metrics.provenance_sources_per_item,
            0.0,
            "empty provenance density",
        )?;
        ensure(
            metrics.provenance_complete,
            "empty packs have no missing provenance entries",
        )?;
        ensure_equal(
            &metrics
                .sections
                .iter()
                .map(|metric| metric.section)
                .collect::<Vec<_>>(),
            &PackSection::all().to_vec(),
            "empty section order",
        )?;
        for metric in &metrics.sections {
            ensure_equal(&metric.item_count, &0, "empty section item count")?;
            ensure_equal(&metric.used_tokens, &0, "empty section tokens")?;
        }
        ensure_equal(
            &metrics.omissions.token_budget_exceeded,
            &0,
            "empty budget omissions",
        )?;
        ensure_equal(
            &metrics.omissions.redundant_candidates,
            &0,
            "empty redundant omissions",
        )
    }

    #[test]
    fn assemble_draft_orders_candidates_deterministically() -> TestResult {
        let budget = match TokenBudget::new(100) {
            Ok(budget) => budget,
            Err(error) => return Err(format!("budget rejected: {error:?}")),
        };
        let lower_id = candidate(1, 0.9, 0.7, 10)?;
        let higher_utility = candidate(2, 0.9, 0.9, 10)?;
        let lower_relevance = candidate(3, 0.8, 1.0, 10)?;

        let draft = assemble_draft(
            "release workflow",
            budget,
            vec![lower_relevance, lower_id, higher_utility],
        )
        .map_err(|error| format!("draft rejected: {error:?}"))?;

        let ids: Vec<MemoryId> = draft.items.iter().map(|item| item.memory_id).collect();
        ensure_equal(
            &ids,
            &vec![memory_id(2), memory_id(1), memory_id(3)],
            "deterministic rank order",
        )?;
        ensure_equal(
            &draft.items.first().map(|item| item.rank),
            &Some(1),
            "first rank",
        )?;
        ensure_equal(
            &draft.items.get(1).map(|item| item.rank),
            &Some(2),
            "second rank",
        )
    }

    #[test]
    fn assemble_draft_omits_items_that_exceed_budget() -> TestResult {
        let budget = match TokenBudget::new(15) {
            Ok(budget) => budget,
            Err(error) => return Err(format!("budget rejected: {error:?}")),
        };

        let draft = assemble_draft(
            "format before release",
            budget,
            vec![candidate(1, 1.0, 0.5, 10)?, candidate(2, 0.9, 0.5, 10)?],
        )
        .map_err(|error| format!("draft rejected: {error:?}"))?;

        ensure_equal(&draft.used_tokens, &10, "used token count")?;
        ensure_equal(&draft.items.len(), &1, "selected item count")?;
        ensure_equal(&draft.omitted.len(), &1, "omitted item count")?;
        ensure_equal(
            &draft.omitted.first().map(|omission| omission.reason),
            &Some(PackOmissionReason::TokenBudgetExceeded),
            "omission reason",
        )?;
        ensure_equal(
            &draft
                .omitted
                .first()
                .map(|omission| omission.reason.as_str()),
            &Some("token_budget_exceeded"),
            "omission reason wire name",
        )
    }

    #[test]
    fn assemble_draft_applies_mmr_redundancy_control() -> TestResult {
        let budget = match TokenBudget::new(100) {
            Ok(budget) => budget,
            Err(error) => return Err(format!("budget rejected: {error:?}")),
        };
        let first = candidate(1, 1.0, 0.5, 10)?.with_diversity_key("release-formatting");
        let duplicate = candidate(2, 0.99, 0.5, 10)?.with_diversity_key("release-formatting");
        let diverse = candidate(3, 0.8, 0.5, 10)?.with_diversity_key("release-checks");

        let draft = assemble_draft("prepare release", budget, vec![duplicate, diverse, first])
            .map_err(|error| format!("draft rejected: {error:?}"))?;

        let ids: Vec<MemoryId> = draft.items.iter().map(|item| item.memory_id).collect();
        ensure_equal(
            &ids,
            &vec![memory_id(1), memory_id(3)],
            "MMR should select the best candidate, then the diverse candidate",
        )?;
        ensure_equal(&draft.used_tokens, &20, "used tokens after redundancy")?;
        ensure_equal(&draft.omitted.len(), &1, "one redundant candidate omitted")?;
        ensure_equal(
            &draft.omitted.first().map(|omission| omission.memory_id),
            &Some(memory_id(2)),
            "redundant candidate id",
        )?;
        ensure_equal(
            &draft.omitted.first().map(|omission| omission.reason),
            &Some(PackOmissionReason::RedundantCandidate),
            "redundant omission reason",
        )?;
        ensure_equal(
            &draft
                .omitted
                .first()
                .map(|omission| omission.reason.as_str()),
            &Some("redundant_candidate"),
            "redundant omission reason wire name",
        )
    }

    #[test]
    fn assemble_draft_deduplicates_exact_normalized_content_without_key() -> TestResult {
        let budget = match TokenBudget::new(100) {
            Ok(budget) => budget,
            Err(error) => return Err(format!("budget rejected: {error:?}")),
        };
        let first =
            candidate_with_content(1, 0.9, 0.5, 10, "Run cargo fmt --check before release.")?;
        let duplicate = candidate_with_content(
            2,
            0.8,
            0.5,
            10,
            "  Run   cargo fmt --check before release.  ",
        )?;

        let draft = assemble_draft("prepare release", budget, vec![duplicate, first])
            .map_err(|error| format!("draft rejected: {error:?}"))?;

        ensure_equal(&draft.items.len(), &1, "only one exact duplicate selected")?;
        ensure_equal(
            &draft.items.first().map(|item| item.memory_id),
            &Some(memory_id(1)),
            "highest relevance duplicate selected",
        )?;
        ensure_equal(&draft.omitted.len(), &1, "one exact duplicate omitted")?;
        ensure_equal(
            &draft.omitted.first().map(|omission| omission.reason),
            &Some(PackOmissionReason::RedundantCandidate),
            "exact duplicate omission reason",
        )
    }

    #[test]
    fn context_response_wraps_request_pack_and_degradation_contract() -> TestResult {
        let request = ContextRequest::from_query("format before release")
            .map_err(|error| format!("request rejected: {error:?}"))?;
        let draft = assemble_draft(
            request.query.clone(),
            request.budget,
            vec![candidate(1, 1.0, 0.5, 10)?],
        )
        .map_err(|error| format!("draft rejected: {error:?}"))?;
        let degraded = ContextResponseDegradation::new(
            "semantic_index_unavailable",
            ContextResponseSeverity::Medium,
            "Semantic search is unavailable; lexical retrieval was used.",
            Some("ee index rebuild --workspace .".to_string()),
        )
        .map_err(|error| format!("degradation rejected: {error:?}"))?;

        let response = ContextResponse::new(request, draft, vec![degraded])
            .map_err(|error| format!("response rejected: {error:?}"))?;

        ensure_equal(&response.schema, &"ee.response.v1", "response schema")?;
        ensure(response.success, "context response success flag")?;
        ensure_equal(
            &response.data.command,
            &CONTEXT_COMMAND,
            "context response command",
        )?;
        ensure_equal(
            &response.data.pack.items.len(),
            &1,
            "context response pack item count",
        )?;
        ensure_equal(
            &response
                .data
                .degraded
                .first()
                .map(|degraded| degraded.severity.as_str()),
            &Some("medium"),
            "degradation severity wire name",
        )?;
        ensure_equal(
            &response
                .data
                .degraded
                .first()
                .and_then(|degraded| degraded.repair.as_deref()),
            &Some("ee index rebuild --workspace ."),
            "degradation repair",
        )
    }

    #[test]
    fn context_response_rejects_mismatched_query_and_invalid_degradation() -> TestResult {
        let request = ContextRequest::from_query("prepare release")
            .map_err(|error| format!("request rejected: {error:?}"))?;
        let draft = assemble_draft("different task", request.budget, Vec::new())
            .map_err(|error| format!("draft rejected: {error:?}"))?;
        let response = ContextResponse::new(request, draft, Vec::new());
        ensure(
            matches!(
                response,
                Err(PackValidationError::ContextResponseQueryMismatch { .. })
            ),
            "mismatched response query must be rejected",
        )?;

        let empty_code = ContextResponseDegradation::new(
            " ",
            ContextResponseSeverity::Low,
            "fallback used",
            None,
        );
        ensure(
            matches!(empty_code, Err(PackValidationError::EmptyDegradationCode)),
            "empty degradation code must be rejected",
        )?;

        let empty_message = ContextResponseDegradation::new(
            "fallback_used",
            ContextResponseSeverity::High,
            " ",
            None,
        );
        ensure(
            matches!(
                empty_message,
                Err(PackValidationError::EmptyDegradationMessage { .. })
            ),
            "empty degradation message must be rejected",
        )
    }

    #[test]
    fn assemble_draft_rejects_empty_query() -> TestResult {
        let budget = match TokenBudget::new(10) {
            Ok(budget) => budget,
            Err(error) => return Err(format!("budget rejected: {error:?}")),
        };
        let draft = assemble_draft(" ", budget, vec![candidate(1, 0.5, 0.5, 5)?]);
        ensure(
            matches!(draft, Err(PackValidationError::EmptyQuery)),
            "empty query must be rejected",
        )
    }
}
