use std::cmp::Ordering;
use std::fmt;

use crate::models::{MemoryId, ProvenanceUri, RESPONSE_SCHEMA_V1, UnitScore};

pub const SUBSYSTEM: &str = "pack";
pub const CONTEXT_COMMAND: &str = "context";
pub const DEFAULT_CONTEXT_MAX_TOKENS: u32 = 4_000;
pub const DEFAULT_CANDIDATE_POOL: u32 = 64;

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackOmission {
    pub memory_id: MemoryId,
    pub estimated_tokens: u32,
    pub reason: PackOmissionReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackOmissionReason {
    TokenBudgetExceeded,
}

impl PackOmissionReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TokenBudgetExceeded => "token_budget_exceeded",
        }
    }
}

impl fmt::Display for PackOmissionReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Assemble a deterministic context-pack draft from validated candidates.
///
/// Selection is intentionally simple in EE-140: candidates are ordered by
/// relevance, utility, section, and memory id, then admitted while they
/// fit the token budget. Later beads can replace the scoring objective
/// with MMR while preserving this stable input/output contract.
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
    let mut items = Vec::new();
    let mut omitted = Vec::new();

    for candidate in candidates {
        match used_tokens.checked_add(candidate.estimated_tokens) {
            Some(total) if total <= budget.max_tokens() => {
                let rank = next_rank;
                next_rank = next_rank
                    .checked_add(1)
                    .ok_or(PackValidationError::CandidateRankOverflow)?;
                used_tokens = total;
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
        ContextResponseDegradation, ContextResponseSeverity, PackCandidate, PackCandidateInput,
        PackOmissionReason, PackProvenance, PackSection, PackValidationError, TokenBudget,
        assemble_draft, subsystem_name,
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
        PackCandidate::new(PackCandidateInput {
            memory_id: memory_id(seed),
            section: PackSection::ProceduralRules,
            content: format!("memory {seed}"),
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
