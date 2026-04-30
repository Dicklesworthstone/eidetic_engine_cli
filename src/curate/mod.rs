//! Curation subsystem (EE-180, ADR-0006).
//!
//! Curation candidates are auditable proposals for memory mutations:
//! consolidation, promotion, deprecation, supersession, tombstoning, etc.
//! No silent durable mutation — every change goes through this queue.

pub mod regret;

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

pub const SUBSYSTEM: &str = "curate";
pub const DEFAULT_SPECIFICITY_MIN: f32 = 0.45;
pub const CANDIDATE_TOO_GENERIC_CODE: &str = "candidate_too_generic";

const SCORE_SCALE: f32 = 10_000.0;
const KNOWN_COMMANDS: &[&str] = &[
    "br", "bv", "cargo", "cass", "ee", "gh", "git", "rch", "rustfmt", "ubs",
];
const TECHNOLOGY_TOKENS: &[&str] = &[
    "adr",
    "agent",
    "asupersync",
    "beads",
    "blake3",
    "cargo",
    "cass",
    "clippy",
    "frankensearch",
    "frankensqlite",
    "fts5",
    "json",
    "jsonl",
    "labruntime",
    "mcp",
    "rust",
    "rustfmt",
    "sqlmodel",
    "sqlite",
    "toml",
    "toon",
    "yaml",
];
const GENERIC_TOKENS: &[&str] = &[
    "always", "better", "careful", "clean", "code", "correct", "function", "good", "handle",
    "helpful", "improve", "logic", "nice", "properly", "quality", "review", "safe", "stuff",
    "system", "thing", "things", "useful", "work",
];
const METRIC_UNITS: &[&str] = &[
    "%", "b", "bytes", "gb", "kb", "mb", "ms", "s", "sec", "secs", "seconds", "tokens",
];
const FILE_EXTENSIONS: &[&str] = &[
    ".md", ".rs", ".toml", ".json", ".jsonl", ".yaml", ".yml", ".sql", ".db", ".sqlite", ".txt",
];
const FILE_PREFIXES: &[&str] = &[
    "/", "./", "../", ".beads/", ".github/", "crates/", "docs/", "src/", "target/", "tests/",
];

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

/// Type of curation action being proposed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CandidateType {
    /// Merge multiple memories into a more general form.
    Consolidate,
    /// Raise confidence or trust class based on validation.
    Promote,
    /// Lower confidence or mark as less relevant.
    Deprecate,
    /// Replace with a newer, more accurate memory.
    Supersede,
    /// Mark as deleted without physical removal.
    Tombstone,
    /// Combine two memories into one.
    Merge,
    /// Split a memory into multiple more specific ones.
    Split,
    /// Withdraw a previous assertion due to contradiction.
    Retract,
}

impl CandidateType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Consolidate => "consolidate",
            Self::Promote => "promote",
            Self::Deprecate => "deprecate",
            Self::Supersede => "supersede",
            Self::Tombstone => "tombstone",
            Self::Merge => "merge",
            Self::Split => "split",
            Self::Retract => "retract",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 8] {
        [
            Self::Consolidate,
            Self::Promote,
            Self::Deprecate,
            Self::Supersede,
            Self::Tombstone,
            Self::Merge,
            Self::Split,
            Self::Retract,
        ]
    }
}

impl fmt::Display for CandidateType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error when parsing an invalid candidate type string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseCandidateTypeError {
    input: String,
}

impl ParseCandidateTypeError {
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseCandidateTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown candidate type `{}`; expected one of consolidate, promote, deprecate, supersede, tombstone, merge, split, retract",
            self.input
        )
    }
}

impl std::error::Error for ParseCandidateTypeError {}

impl FromStr for CandidateType {
    type Err = ParseCandidateTypeError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "consolidate" => Ok(Self::Consolidate),
            "promote" => Ok(Self::Promote),
            "deprecate" => Ok(Self::Deprecate),
            "supersede" => Ok(Self::Supersede),
            "tombstone" => Ok(Self::Tombstone),
            "merge" => Ok(Self::Merge),
            "split" => Ok(Self::Split),
            "retract" => Ok(Self::Retract),
            _ => Err(ParseCandidateTypeError {
                input: input.to_owned(),
            }),
        }
    }
}

/// Source that proposed the curation candidate.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CandidateSource {
    /// Agent inferred from context or patterns.
    AgentInference,
    /// Rule engine triggered by configured policy.
    RuleEngine,
    /// Human explicitly requested the curation.
    HumanRequest,
    /// Feedback event (positive or negative).
    FeedbackEvent,
    /// Contradiction detected with another memory.
    ContradictionDetected,
    /// Decay trigger based on age or inactivity.
    DecayTrigger,
}

impl CandidateSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentInference => "agent_inference",
            Self::RuleEngine => "rule_engine",
            Self::HumanRequest => "human_request",
            Self::FeedbackEvent => "feedback_event",
            Self::ContradictionDetected => "contradiction_detected",
            Self::DecayTrigger => "decay_trigger",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 6] {
        [
            Self::AgentInference,
            Self::RuleEngine,
            Self::HumanRequest,
            Self::FeedbackEvent,
            Self::ContradictionDetected,
            Self::DecayTrigger,
        ]
    }
}

impl fmt::Display for CandidateSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error when parsing an invalid candidate source string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseCandidateSourceError {
    input: String,
}

impl ParseCandidateSourceError {
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseCandidateSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown candidate source `{}`; expected one of agent_inference, rule_engine, human_request, feedback_event, contradiction_detected, decay_trigger",
            self.input
        )
    }
}

impl std::error::Error for ParseCandidateSourceError {}

impl FromStr for CandidateSource {
    type Err = ParseCandidateSourceError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "agent_inference" => Ok(Self::AgentInference),
            "rule_engine" => Ok(Self::RuleEngine),
            "human_request" => Ok(Self::HumanRequest),
            "feedback_event" => Ok(Self::FeedbackEvent),
            "contradiction_detected" => Ok(Self::ContradictionDetected),
            "decay_trigger" => Ok(Self::DecayTrigger),
            _ => Err(ParseCandidateSourceError {
                input: input.to_owned(),
            }),
        }
    }
}

/// Status of a curation candidate.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CandidateStatus {
    /// Awaiting review.
    Pending,
    /// Approved by reviewer.
    Approved,
    /// Rejected by reviewer.
    Rejected,
    /// Expired due to TTL.
    Expired,
    /// Applied to target memory.
    Applied,
}

impl CandidateStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
            Self::Applied => "applied",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Pending,
            Self::Approved,
            Self::Rejected,
            Self::Expired,
            Self::Applied,
        ]
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Rejected | Self::Expired | Self::Applied)
    }
}

impl fmt::Display for CandidateStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error when parsing an invalid candidate status string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseCandidateStatusError {
    input: String,
}

impl ParseCandidateStatusError {
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseCandidateStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown candidate status `{}`; expected one of pending, approved, rejected, expired, applied",
            self.input
        )
    }
}

impl std::error::Error for ParseCandidateStatusError {}

impl FromStr for CandidateStatus {
    type Err = ParseCandidateStatusError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "pending" => Ok(Self::Pending),
            "approved" => Ok(Self::Approved),
            "rejected" => Ok(Self::Rejected),
            "expired" => Ok(Self::Expired),
            "applied" => Ok(Self::Applied),
            _ => Err(ParseCandidateStatusError {
                input: input.to_owned(),
            }),
        }
    }
}

/// Input for creating a new curation candidate.
#[derive(Clone, Debug)]
pub struct CandidateInput {
    pub workspace_id: String,
    pub candidate_type: CandidateType,
    pub target_memory_id: String,
    pub proposed_content: Option<String>,
    pub proposed_confidence: Option<f32>,
    pub proposed_trust_class: Option<String>,
    pub source_type: CandidateSource,
    pub source_id: Option<String>,
    pub reason: String,
    pub confidence: f32,
    pub ttl_seconds: Option<u64>,
}

/// A validated curation candidate ready for storage.
#[derive(Clone, Debug)]
pub struct ValidatedCandidate {
    pub workspace_id: String,
    pub candidate_type: CandidateType,
    pub target_memory_id: String,
    pub proposed_content: Option<String>,
    pub specificity_report: Option<SpecificityReport>,
    pub proposed_confidence: Option<f32>,
    pub proposed_trust_class: Option<String>,
    pub source_type: CandidateSource,
    pub source_id: Option<String>,
    pub reason: String,
    pub confidence: f32,
    pub ttl_expires_at: Option<String>,
}

/// Errors during candidate validation.
#[derive(Clone, Debug, PartialEq)]
pub enum CandidateValidationError {
    EmptyWorkspaceId,
    EmptyTargetMemoryId,
    EmptyReason,
    ConfidenceOutOfRange {
        value: String,
    },
    ProposedConfidenceOutOfRange {
        value: String,
    },
    InvalidProposedTrustClass {
        value: String,
    },
    ContentRequiredForType {
        candidate_type: CandidateType,
    },
    ContentForbiddenForType {
        candidate_type: CandidateType,
    },
    CandidateTooGeneric {
        score: String,
        threshold: String,
        rejected_reasons: Vec<&'static str>,
    },
    InvalidStatusTransition {
        from: CandidateStatus,
        to: CandidateStatus,
    },
    CandidateExpired,
    CandidateAlreadyTerminal {
        status: CandidateStatus,
    },
}

impl fmt::Display for CandidateValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyWorkspaceId => f.write_str("workspace ID must not be empty"),
            Self::EmptyTargetMemoryId => f.write_str("target memory ID must not be empty"),
            Self::EmptyReason => f.write_str("reason must not be empty"),
            Self::ConfidenceOutOfRange { value } => {
                write!(f, "confidence `{value}` must be between 0.0 and 1.0")
            }
            Self::ProposedConfidenceOutOfRange { value } => {
                write!(
                    f,
                    "proposed confidence `{value}` must be between 0.0 and 1.0"
                )
            }
            Self::InvalidProposedTrustClass { value } => {
                write!(f, "invalid proposed trust class `{value}`")
            }
            Self::ContentRequiredForType { candidate_type } => {
                write!(
                    f,
                    "proposed content is required for {candidate_type} candidates"
                )
            }
            Self::ContentForbiddenForType { candidate_type } => {
                write!(
                    f,
                    "proposed content is not allowed for {candidate_type} candidates"
                )
            }
            Self::CandidateTooGeneric {
                score,
                threshold,
                rejected_reasons,
            } => {
                write!(
                    f,
                    "candidate proposed content failed specificity (score {score}, threshold {threshold}): {}",
                    rejected_reasons.join(", ")
                )
            }
            Self::InvalidStatusTransition { from, to } => {
                write!(f, "cannot transition from {from} to {to}")
            }
            Self::CandidateExpired => f.write_str("candidate has expired"),
            Self::CandidateAlreadyTerminal { status } => {
                write!(f, "candidate is already in terminal state {status}")
            }
        }
    }
}

impl std::error::Error for CandidateValidationError {}

impl CandidateValidationError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::EmptyWorkspaceId => "empty_workspace_id",
            Self::EmptyTargetMemoryId => "empty_target_memory_id",
            Self::EmptyReason => "empty_reason",
            Self::ConfidenceOutOfRange { .. } => "confidence_out_of_range",
            Self::ProposedConfidenceOutOfRange { .. } => "proposed_confidence_out_of_range",
            Self::InvalidProposedTrustClass { .. } => "invalid_proposed_trust_class",
            Self::ContentRequiredForType { .. } => "content_required_for_type",
            Self::ContentForbiddenForType { .. } => "content_forbidden_for_type",
            Self::CandidateTooGeneric { .. } => CANDIDATE_TOO_GENERIC_CODE,
            Self::InvalidStatusTransition { .. } => "invalid_status_transition",
            Self::CandidateExpired => "candidate_expired",
            Self::CandidateAlreadyTerminal { .. } => "candidate_already_terminal",
        }
    }
}

impl CandidateType {
    /// Whether this candidate type requires proposed content.
    #[must_use]
    pub const fn requires_content(self) -> bool {
        matches!(
            self,
            Self::Consolidate | Self::Supersede | Self::Merge | Self::Split
        )
    }

    /// Whether this candidate type forbids proposed content.
    #[must_use]
    pub const fn forbids_content(self) -> bool {
        matches!(self, Self::Tombstone | Self::Retract)
    }
}

impl CandidateStatus {
    /// Check if a status transition is valid.
    #[must_use]
    pub const fn can_transition_to(self, target: Self) -> bool {
        match (self, target) {
            // From pending: can go to approved, rejected, or expired
            (Self::Pending, Self::Approved | Self::Rejected | Self::Expired) => true,
            // From approved: can go to applied or rejected
            (Self::Approved, Self::Applied | Self::Rejected) => true,
            // Terminal states cannot transition
            (Self::Rejected | Self::Expired | Self::Applied, _) => false,
            // Same state is always allowed (no-op)
            (from, to) if from as u8 == to as u8 => true,
            _ => false,
        }
    }
}

/// Weights used by the deterministic curation specificity scorer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpecificityWeights {
    pub command_block: f32,
    pub inline_command: f32,
    pub file_path: f32,
    pub error_code: f32,
    pub metric_threshold: f32,
    pub branch_or_tag: f32,
    pub provenance_uri: f32,
    pub technology_name: f32,
    pub concrete_token_density: f32,
}

impl Default for SpecificityWeights {
    fn default() -> Self {
        Self {
            command_block: 0.18,
            inline_command: 0.30,
            file_path: 0.26,
            error_code: 0.14,
            metric_threshold: 0.14,
            branch_or_tag: 0.08,
            provenance_uri: 0.08,
            technology_name: 0.12,
            concrete_token_density: 0.18,
        }
    }
}

/// Configuration for curation specificity validation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpecificityConfig {
    pub minimum_score: f32,
    pub weights: SpecificityWeights,
}

impl Default for SpecificityConfig {
    fn default() -> Self {
        Self {
            minimum_score: DEFAULT_SPECIFICITY_MIN,
            weights: SpecificityWeights::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SpecificityTokenKind {
    BranchOrTag,
    Command,
    ErrorCode,
    FilePath,
    MetricThreshold,
    ProvenanceUri,
    RedactedConcrete,
    TechnologyName,
}

impl SpecificityTokenKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BranchOrTag => "branch_or_tag",
            Self::Command => "command",
            Self::ErrorCode => "error_code",
            Self::FilePath => "file_path",
            Self::MetricThreshold => "metric_threshold",
            Self::ProvenanceUri => "provenance_uri",
            Self::RedactedConcrete => "redacted_concrete",
            Self::TechnologyName => "technology_name",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SpecificityPlatform {
    Linux,
    MacOs,
    Windows,
}

impl SpecificityPlatform {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::MacOs => "macos",
            Self::Windows => "windows",
        }
    }
}

/// A concrete token found in proposed curation content.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SpecificityToken {
    pub kind: SpecificityTokenKind,
    pub value: String,
    pub redacted: bool,
}

/// Structural evidence used to score proposed curation content.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SpecificityStructuralSignals {
    pub has_command_block: bool,
    pub has_inline_command: bool,
    pub has_file_path: bool,
    pub has_error_code: bool,
    pub has_metric_threshold: bool,
    pub has_branch_or_tag: bool,
    pub has_provenance_uri: bool,
    pub has_technology_name: bool,
    pub has_instruction_like_content: bool,
}

impl SpecificityStructuralSignals {
    #[must_use]
    pub const fn has_specificity_signal(&self) -> bool {
        self.has_command_block
            || self.has_inline_command
            || self.has_file_path
            || self.has_error_code
            || self.has_metric_threshold
            || self.has_branch_or_tag
            || self.has_provenance_uri
            || self.has_technology_name
    }
}

/// Deterministic specificity report for a proposed curation rule.
#[derive(Clone, Debug, PartialEq)]
pub struct SpecificityReport {
    pub score: f32,
    pub threshold: f32,
    pub passes_threshold: bool,
    pub concrete_tokens: Vec<SpecificityToken>,
    pub redacted_concrete_tokens: Vec<SpecificityToken>,
    pub generic_tokens: Vec<String>,
    pub structural_signals: SpecificityStructuralSignals,
    pub platform: Option<SpecificityPlatform>,
    pub rejected_reasons: Vec<&'static str>,
}

/// Score proposed curation content using the default specificity contract.
#[must_use]
pub fn specificity_score(rule_text: &str) -> SpecificityReport {
    specificity_score_with_config(rule_text, &SpecificityConfig::default())
}

/// Score proposed curation content using an explicit specificity config.
#[must_use]
pub fn specificity_score_with_config(
    rule_text: &str,
    config: &SpecificityConfig,
) -> SpecificityReport {
    let mut tokens = collect_specificity_tokens(rule_text);
    sort_specificity_tokens(&mut tokens);
    let redacted_tokens = tokens
        .iter()
        .filter(|token| token.redacted)
        .cloned()
        .collect::<Vec<_>>();
    let generic_tokens = collect_generic_tokens(rule_text);
    let instruction_report = crate::policy::detect_instruction_like_content(rule_text);
    let structural_signals =
        structural_signals(rule_text, &tokens, instruction_report.is_instruction_like);
    let scoring_token_count = tokens.iter().filter(|token| !token.redacted).count();
    let score = specificity_weighted_sum(scoring_token_count, &structural_signals, config);
    let passes_threshold = score >= config.minimum_score
        && scoring_token_count > 0
        && structural_signals.has_specificity_signal()
        && !instruction_report.is_instruction_like;
    let mut rejected_reasons = specificity_rejected_reasons(
        rule_text,
        score,
        scoring_token_count,
        &generic_tokens,
        &structural_signals,
        &instruction_report.rejected_reasons,
        config,
    );
    if !passes_threshold {
        push_reason(&mut rejected_reasons, CANDIDATE_TOO_GENERIC_CODE);
    }
    rejected_reasons.sort_unstable();
    rejected_reasons.dedup();

    SpecificityReport {
        score,
        threshold: config.minimum_score,
        passes_threshold,
        concrete_tokens: tokens,
        redacted_concrete_tokens: redacted_tokens,
        generic_tokens,
        structural_signals,
        platform: detect_platform(rule_text),
        rejected_reasons,
    }
}

fn specificity_weighted_sum(
    scoring_token_count: usize,
    signals: &SpecificityStructuralSignals,
    config: &SpecificityConfig,
) -> f32 {
    let weights = config.weights;
    let mut score = 0.0_f32;
    if signals.has_command_block {
        score += weights.command_block;
    }
    if signals.has_inline_command {
        score += weights.inline_command;
    }
    if signals.has_file_path {
        score += weights.file_path;
    }
    if signals.has_error_code {
        score += weights.error_code;
    }
    if signals.has_metric_threshold {
        score += weights.metric_threshold;
    }
    if signals.has_branch_or_tag {
        score += weights.branch_or_tag;
    }
    if signals.has_provenance_uri {
        score += weights.provenance_uri;
    }
    if signals.has_technology_name {
        score += weights.technology_name;
    }

    let density = (scoring_token_count as f32 / 4.0).min(1.0);
    score += weights.concrete_token_density * density;
    round_score(score.clamp(0.0, 1.0))
}

fn specificity_rejected_reasons(
    rule_text: &str,
    score: f32,
    scoring_token_count: usize,
    generic_tokens: &[String],
    signals: &SpecificityStructuralSignals,
    instruction_reasons: &[&'static str],
    config: &SpecificityConfig,
) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if rule_text.trim().is_empty() {
        push_reason(&mut reasons, "empty_input");
    }
    if scoring_token_count == 0 {
        push_reason(&mut reasons, "no_concrete_tokens_found");
    }
    if scoring_token_count == 0 && !generic_tokens.is_empty() {
        push_reason(&mut reasons, "all_tokens_generic");
    }
    if !signals.has_specificity_signal() {
        push_reason(&mut reasons, "no_structural_signal");
    }
    if score < config.minimum_score {
        push_reason(&mut reasons, "below_specificity_threshold");
    }
    for reason in instruction_reasons {
        push_reason(&mut reasons, reason);
    }
    reasons
}

fn push_reason(reasons: &mut Vec<&'static str>, reason: &'static str) {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

fn collect_specificity_tokens(input: &str) -> Vec<SpecificityToken> {
    let lexical_tokens = lexical_tokens(input);
    let mut tokens = Vec::new();
    collect_inline_code_tokens(input, &mut tokens);
    collect_fenced_command_tokens(input, &mut tokens);
    collect_lexical_concrete_tokens(&lexical_tokens, &mut tokens);
    tokens
}

fn collect_inline_code_tokens(input: &str, tokens: &mut Vec<SpecificityToken>) {
    for (index, segment) in input.split('`').enumerate() {
        if index % 2 == 1 && !segment.trim().is_empty() {
            let trimmed = segment.trim();
            if looks_like_command(trimmed) {
                push_specificity_token(tokens, SpecificityTokenKind::Command, trimmed);
            }
        }
    }
}

fn collect_fenced_command_tokens(input: &str, tokens: &mut Vec<SpecificityToken>) {
    let mut in_fence = false;
    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence && looks_like_command(trimmed) {
            push_specificity_token(tokens, SpecificityTokenKind::Command, trimmed);
        }
    }
}

fn collect_lexical_concrete_tokens(lexical_tokens: &[String], tokens: &mut Vec<SpecificityToken>) {
    for (index, token) in lexical_tokens.iter().enumerate() {
        let lower = token.to_ascii_lowercase();
        if let Some(class) = redaction_class(token) {
            push_redacted_specificity_token(tokens, class);
        }
        if KNOWN_COMMANDS.contains(&lower.as_str()) {
            push_specificity_token(
                tokens,
                SpecificityTokenKind::Command,
                &command_phrase(lexical_tokens, index),
            );
        }
        if looks_like_file_path(token) {
            push_specificity_token(tokens, SpecificityTokenKind::FilePath, token);
        }
        if looks_like_error_code(token)
            || (lower == "code"
                && index > 0
                && lexical_tokens[index - 1].eq_ignore_ascii_case("exit")
                && lexical_tokens
                    .get(index + 1)
                    .is_some_and(|next| next.chars().all(|ch| ch.is_ascii_digit())))
        {
            push_specificity_token(
                tokens,
                SpecificityTokenKind::ErrorCode,
                &error_phrase(lexical_tokens, index),
            );
        }
        if looks_like_metric_threshold(token)
            || lexical_tokens
                .get(index + 1)
                .is_some_and(|next| token_has_digit(token) && is_metric_unit(next))
        {
            push_specificity_token(
                tokens,
                SpecificityTokenKind::MetricThreshold,
                &metric_phrase(lexical_tokens, index),
            );
        }
        if looks_like_branch_or_tag(token) {
            push_specificity_token(tokens, SpecificityTokenKind::BranchOrTag, token);
        }
        if looks_like_provenance_uri(token) {
            push_specificity_token(tokens, SpecificityTokenKind::ProvenanceUri, token);
        }
        if TECHNOLOGY_TOKENS.contains(&lower.as_str()) {
            push_specificity_token(tokens, SpecificityTokenKind::TechnologyName, &lower);
        }
    }
}

fn structural_signals(
    input: &str,
    tokens: &[SpecificityToken],
    has_instruction_like_content: bool,
) -> SpecificityStructuralSignals {
    SpecificityStructuralSignals {
        has_command_block: input.contains("```"),
        has_inline_command: tokens
            .iter()
            .any(|token| token.kind == SpecificityTokenKind::Command),
        has_file_path: tokens
            .iter()
            .any(|token| token.kind == SpecificityTokenKind::FilePath),
        has_error_code: tokens
            .iter()
            .any(|token| token.kind == SpecificityTokenKind::ErrorCode),
        has_metric_threshold: tokens
            .iter()
            .any(|token| token.kind == SpecificityTokenKind::MetricThreshold),
        has_branch_or_tag: tokens
            .iter()
            .any(|token| token.kind == SpecificityTokenKind::BranchOrTag),
        has_provenance_uri: tokens
            .iter()
            .any(|token| token.kind == SpecificityTokenKind::ProvenanceUri),
        has_technology_name: tokens
            .iter()
            .any(|token| token.kind == SpecificityTokenKind::TechnologyName),
        has_instruction_like_content,
    }
}

fn lexical_tokens(input: &str) -> Vec<String> {
    input
        .split_whitespace()
        .map(trim_token)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

fn trim_token(token: &str) -> &str {
    token
        .trim_start_matches(|ch: char| {
            matches!(
                ch,
                ',' | ';' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}'
            )
        })
        .trim_end_matches(|ch: char| {
            matches!(
                ch,
                ',' | ';' | ':' | '.' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}'
            )
        })
}

fn push_specificity_token(
    tokens: &mut Vec<SpecificityToken>,
    kind: SpecificityTokenKind,
    value: &str,
) {
    let trimmed = trim_token(value).trim();
    if trimmed.is_empty() {
        return;
    }
    tokens.push(SpecificityToken {
        kind,
        value: trimmed.to_string(),
        redacted: false,
    });
}

fn push_redacted_specificity_token(tokens: &mut Vec<SpecificityToken>, class: &'static str) {
    tokens.push(SpecificityToken {
        kind: SpecificityTokenKind::RedactedConcrete,
        value: format!("REDACTED:{class}"),
        redacted: true,
    });
}

fn sort_specificity_tokens(tokens: &mut Vec<SpecificityToken>) {
    tokens.sort();
    tokens.dedup();
}

fn collect_generic_tokens(input: &str) -> Vec<String> {
    let mut tokens = BTreeSet::new();
    for token in lexical_tokens(input) {
        let lower = token.to_ascii_lowercase();
        if GENERIC_TOKENS.contains(&lower.as_str()) {
            tokens.insert(lower);
        }
    }
    tokens.into_iter().collect()
}

fn command_phrase(tokens: &[String], start: usize) -> String {
    let mut out = Vec::new();
    for token in tokens.iter().skip(start).take(4) {
        let lower = token.to_ascii_lowercase();
        if out.is_empty()
            || token.starts_with('-')
            || lower
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        {
            out.push(token.as_str());
        } else {
            break;
        }
    }
    out.join(" ")
}

fn error_phrase(tokens: &[String], index: usize) -> String {
    if index > 0
        && tokens[index - 1].eq_ignore_ascii_case("exit")
        && tokens[index].eq_ignore_ascii_case("code")
        && tokens
            .get(index + 1)
            .is_some_and(|next| next.chars().all(|ch| ch.is_ascii_digit()))
    {
        format!("exit code {}", tokens[index + 1])
    } else {
        tokens[index].clone()
    }
}

fn metric_phrase(tokens: &[String], index: usize) -> String {
    match tokens.get(index + 1) {
        Some(next) if token_has_digit(&tokens[index]) && is_metric_unit(next) => {
            format!("{} {}", tokens[index], next)
        }
        _ => tokens[index].clone(),
    }
}

fn looks_like_command(input: &str) -> bool {
    let tokens = lexical_tokens(input);
    tokens
        .first()
        .is_some_and(|token| KNOWN_COMMANDS.contains(&token.to_ascii_lowercase().as_str()))
}

fn looks_like_file_path(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    let has_prefix = FILE_PREFIXES.iter().any(|prefix| lower.starts_with(prefix));
    let has_extension = FILE_EXTENSIONS
        .iter()
        .any(|extension| lower.ends_with(extension));
    (has_prefix && (token.contains('/') || has_extension))
        || (has_extension && token.chars().any(|ch| ch == '/' || ch == '.'))
}

fn looks_like_error_code(token: &str) -> bool {
    let trimmed = trim_token(token).trim_end_matches(':');
    let upper = trimmed.to_ascii_uppercase();
    if upper.len() >= 5
        && upper.starts_with('E')
        && upper[1..].chars().all(|ch| ch.is_ascii_digit())
    {
        return true;
    }
    upper.split_once('-').is_some_and(|(prefix, suffix)| {
        (2..=8).contains(&prefix.len())
            && prefix.chars().all(|ch| ch.is_ascii_uppercase())
            && suffix.chars().any(|ch| ch.is_ascii_digit())
    })
}

fn looks_like_metric_threshold(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    token_has_digit(&lower)
        && METRIC_UNITS
            .iter()
            .any(|unit| lower.ends_with(unit) || lower.contains(&format!("/{unit}")))
}

fn token_has_digit(token: &str) -> bool {
    token.chars().any(|ch| ch.is_ascii_digit())
}

fn is_metric_unit(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    METRIC_UNITS.contains(&lower.as_str())
}

fn looks_like_branch_or_tag(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    lower == "main"
        || lower.starts_with("release/")
        || (lower.starts_with('v')
            && lower[1..].split('.').count() >= 2
            && lower[1..].split('.').all(|segment| {
                !segment.is_empty() && segment.chars().all(|ch| ch.is_ascii_digit())
            }))
}

fn looks_like_provenance_uri(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    lower.starts_with("cass:")
        || lower.starts_with("file:")
        || lower.starts_with("session:")
        || lower.starts_with("mem_")
}

fn redaction_class(token: &str) -> Option<&'static str> {
    let lower = token.to_ascii_lowercase();
    if lower.contains(concat!("api", "_", "key")) || lower.contains(concat!("api", "-", "key")) {
        Some(concat!("api", "_", "key"))
    } else if lower.contains(concat!("private", "_", "key"))
        || lower.contains(concat!("private", "-", "key"))
    {
        Some(concat!("private", "_", "key"))
    } else if lower.contains(concat!("pass", "word")) {
        Some(concat!("pass", "word"))
    } else if lower.contains(concat!("to", "ken")) || lower.contains("bearer") {
        Some(concat!("to", "ken"))
    } else {
        None
    }
}

fn detect_platform(input: &str) -> Option<SpecificityPlatform> {
    let lower = input.to_ascii_lowercase();
    if lower.contains("linux") || lower.contains("/proc/") {
        Some(SpecificityPlatform::Linux)
    } else if lower.contains("macos") || lower.contains("darwin") {
        Some(SpecificityPlatform::MacOs)
    } else if lower.contains("windows") || lower.contains("powershell") || lower.contains(".ps1") {
        Some(SpecificityPlatform::Windows)
    } else {
        None
    }
}

fn round_score(score: f32) -> f32 {
    (score * SCORE_SCALE).round() / SCORE_SCALE
}

/// Validate a candidate input and produce a validated candidate.
pub fn validate_candidate(
    input: CandidateInput,
    now_rfc3339: &str,
) -> Result<ValidatedCandidate, CandidateValidationError> {
    // Validate required fields
    if input.workspace_id.trim().is_empty() {
        return Err(CandidateValidationError::EmptyWorkspaceId);
    }
    if input.target_memory_id.trim().is_empty() {
        return Err(CandidateValidationError::EmptyTargetMemoryId);
    }
    if input.reason.trim().is_empty() {
        return Err(CandidateValidationError::EmptyReason);
    }

    // Validate confidence
    if !(0.0..=1.0).contains(&input.confidence) {
        return Err(CandidateValidationError::ConfidenceOutOfRange {
            value: input.confidence.to_string(),
        });
    }

    // Validate proposed confidence if present
    if let Some(pc) = input.proposed_confidence {
        if !(0.0..=1.0).contains(&pc) {
            return Err(CandidateValidationError::ProposedConfidenceOutOfRange {
                value: pc.to_string(),
            });
        }
    }

    // Validate proposed trust class if present
    if let Some(ref tc) = input.proposed_trust_class {
        let valid_classes = [
            "human_explicit",
            "agent_validated",
            "agent_assertion",
            "cass_evidence",
            "legacy_import",
        ];
        if !valid_classes.contains(&tc.as_str()) {
            return Err(CandidateValidationError::InvalidProposedTrustClass { value: tc.clone() });
        }
    }

    // Validate content requirements based on candidate type
    let has_content = input
        .proposed_content
        .as_ref()
        .is_some_and(|c| !c.trim().is_empty());
    if input.candidate_type.requires_content() && !has_content {
        return Err(CandidateValidationError::ContentRequiredForType {
            candidate_type: input.candidate_type,
        });
    }
    if input.candidate_type.forbids_content() && has_content {
        return Err(CandidateValidationError::ContentForbiddenForType {
            candidate_type: input.candidate_type,
        });
    }

    let proposed_content = input
        .proposed_content
        .map(|content| content.trim().to_string())
        .filter(|content| !content.is_empty());
    let specificity_report = proposed_content
        .as_ref()
        .map(|content| specificity_score(content));
    if let Some(report) = &specificity_report
        && !report.passes_threshold
    {
        return Err(CandidateValidationError::CandidateTooGeneric {
            score: format!("{:.4}", report.score),
            threshold: format!("{:.4}", report.threshold),
            rejected_reasons: report.rejected_reasons.clone(),
        });
    }

    // Calculate TTL expiry
    let ttl_expires_at = input.ttl_seconds.map(|secs| {
        // Simple: just store as "now + N seconds" string
        // In real impl would use chrono to calculate actual timestamp
        format!("{now_rfc3339}+{secs}s")
    });

    Ok(ValidatedCandidate {
        workspace_id: input.workspace_id.trim().to_string(),
        candidate_type: input.candidate_type,
        target_memory_id: input.target_memory_id.trim().to_string(),
        proposed_content,
        specificity_report,
        proposed_confidence: input.proposed_confidence,
        proposed_trust_class: input.proposed_trust_class,
        source_type: input.source_type,
        source_id: input
            .source_id
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        reason: input.reason.trim().to_string(),
        confidence: input.confidence,
        ttl_expires_at,
    })
}

/// Validate a status transition.
pub fn validate_status_transition(
    current: CandidateStatus,
    target: CandidateStatus,
) -> Result<(), CandidateValidationError> {
    if current.is_terminal() {
        return Err(CandidateValidationError::CandidateAlreadyTerminal { status: current });
    }
    if !current.can_transition_to(target) {
        return Err(CandidateValidationError::InvalidStatusTransition {
            from: current,
            to: target,
        });
    }
    Ok(())
}

// ============================================================================
// EE-346: Calibrated Curation Risk Certificates
// ============================================================================

/// Schema identifier for curation risk certificates.
pub const RISK_CERTIFICATE_SCHEMA_V1: &str = "ee.curate.risk_certificate.v1";

/// Calibrated risk level for a curation action.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub enum RiskLevel {
    /// Low risk: action is safe, reversible, and well-understood.
    Low,
    /// Medium risk: action has some uncertainty or moderate impact.
    Medium,
    /// High risk: action has significant uncertainty or major impact.
    High,
    /// Critical risk: action is irreversible or has cascading effects.
    Critical,
}

impl RiskLevel {
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

    #[must_use]
    pub const fn requires_human_review(self) -> bool {
        matches!(self, Self::High | Self::Critical)
    }

    #[must_use]
    pub const fn numeric_level(self) -> u8 {
        match self {
            Self::Low => 1,
            Self::Medium => 2,
            Self::High => 3,
            Self::Critical => 4,
        }
    }
}

impl fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error when parsing an invalid risk level string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseRiskLevelError {
    input: String,
}

impl ParseRiskLevelError {
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseRiskLevelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown risk level `{}`; expected one of low, medium, high, critical",
            self.input
        )
    }
}

impl std::error::Error for ParseRiskLevelError {}

impl FromStr for RiskLevel {
    type Err = ParseRiskLevelError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "critical" => Ok(Self::Critical),
            _ => Err(ParseRiskLevelError {
                input: input.to_owned(),
            }),
        }
    }
}

/// A factor that contributes to the risk assessment.
#[derive(Clone, Debug)]
pub struct RiskFactor {
    /// Factor name (e.g., "irreversibility", "cascade_potential").
    pub name: String,
    /// Weight of this factor in the overall risk score (0.0 to 1.0).
    pub weight: f32,
    /// Contribution to risk (0.0 = no risk, 1.0 = maximum risk).
    pub contribution: f32,
    /// Human-readable description of why this factor applies.
    pub reason: String,
}

impl RiskFactor {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        weight: f32,
        contribution: f32,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            weight: weight.clamp(0.0, 1.0),
            contribution: contribution.clamp(0.0, 1.0),
            reason: reason.into(),
        }
    }

    #[must_use]
    pub fn weighted_contribution(&self) -> f32 {
        self.weight * self.contribution
    }
}

/// Calibrated probability estimates for curation outcomes.
#[derive(Clone, Debug, Default)]
pub struct OutcomeProbabilities {
    /// Probability that the action will succeed as intended.
    pub success: f32,
    /// Probability of partial success (some goals achieved).
    pub partial_success: f32,
    /// Probability that the action has no effect.
    pub no_effect: f32,
    /// Probability of negative consequences.
    pub negative_outcome: f32,
    /// Probability of cascading failures.
    pub cascade_failure: f32,
}

impl OutcomeProbabilities {
    #[must_use]
    pub fn new(
        success: f32,
        partial_success: f32,
        no_effect: f32,
        negative_outcome: f32,
        cascade_failure: f32,
    ) -> Self {
        Self {
            success: success.clamp(0.0, 1.0),
            partial_success: partial_success.clamp(0.0, 1.0),
            no_effect: no_effect.clamp(0.0, 1.0),
            negative_outcome: negative_outcome.clamp(0.0, 1.0),
            cascade_failure: cascade_failure.clamp(0.0, 1.0),
        }
    }

    #[must_use]
    pub fn total(&self) -> f32 {
        self.success
            + self.partial_success
            + self.no_effect
            + self.negative_outcome
            + self.cascade_failure
    }

    #[must_use]
    pub fn is_calibrated(&self) -> bool {
        let total = self.total();
        (total - 1.0).abs() < 0.01
    }

    #[must_use]
    pub fn expected_positive(&self) -> f32 {
        self.success + self.partial_success
    }

    #[must_use]
    pub fn expected_negative(&self) -> f32 {
        self.negative_outcome + self.cascade_failure
    }
}

/// A recommendation based on the risk assessment.
#[derive(Clone, Debug)]
pub struct RiskRecommendation {
    /// Action to take (e.g., "proceed", "review", "defer", "reject").
    pub action: String,
    /// Confidence in this recommendation (0.0 to 1.0).
    pub confidence: f32,
    /// Human-readable explanation.
    pub explanation: String,
}

impl RiskRecommendation {
    #[must_use]
    pub fn proceed(confidence: f32, explanation: impl Into<String>) -> Self {
        Self {
            action: "proceed".to_owned(),
            confidence: confidence.clamp(0.0, 1.0),
            explanation: explanation.into(),
        }
    }

    #[must_use]
    pub fn review(confidence: f32, explanation: impl Into<String>) -> Self {
        Self {
            action: "review".to_owned(),
            confidence: confidence.clamp(0.0, 1.0),
            explanation: explanation.into(),
        }
    }

    #[must_use]
    pub fn defer(confidence: f32, explanation: impl Into<String>) -> Self {
        Self {
            action: "defer".to_owned(),
            confidence: confidence.clamp(0.0, 1.0),
            explanation: explanation.into(),
        }
    }

    #[must_use]
    pub fn reject(confidence: f32, explanation: impl Into<String>) -> Self {
        Self {
            action: "reject".to_owned(),
            confidence: confidence.clamp(0.0, 1.0),
            explanation: explanation.into(),
        }
    }
}

/// A calibrated risk certificate for a curation action.
#[derive(Clone, Debug)]
pub struct RiskCertificate {
    /// Schema identifier.
    pub schema: String,
    /// Candidate type being assessed.
    pub candidate_type: CandidateType,
    /// Target memory ID.
    pub target_memory_id: String,
    /// Overall risk level.
    pub risk_level: RiskLevel,
    /// Aggregate risk score (0.0 to 1.0).
    pub risk_score: f32,
    /// Individual risk factors.
    pub factors: Vec<RiskFactor>,
    /// Calibrated outcome probabilities.
    pub probabilities: OutcomeProbabilities,
    /// Primary recommendation.
    pub recommendation: RiskRecommendation,
    /// Whether this certificate is in report-only mode.
    pub report_only: bool,
    /// Timestamp when the certificate was generated.
    pub generated_at: String,
}

impl RiskCertificate {
    #[must_use]
    pub fn builder() -> RiskCertificateBuilder {
        RiskCertificateBuilder::default()
    }

    #[must_use]
    pub fn requires_human_review(&self) -> bool {
        self.risk_level.requires_human_review()
    }

    #[must_use]
    pub fn is_actionable(&self) -> bool {
        !self.report_only && !self.requires_human_review()
    }
}

/// Builder for constructing risk certificates.
#[derive(Clone, Debug, Default)]
pub struct RiskCertificateBuilder {
    candidate_type: Option<CandidateType>,
    target_memory_id: Option<String>,
    factors: Vec<RiskFactor>,
    probabilities: OutcomeProbabilities,
    report_only: bool,
    generated_at: Option<String>,
}

impl RiskCertificateBuilder {
    #[must_use]
    pub fn candidate_type(mut self, candidate_type: CandidateType) -> Self {
        self.candidate_type = Some(candidate_type);
        self
    }

    #[must_use]
    pub fn target_memory_id(mut self, id: impl Into<String>) -> Self {
        self.target_memory_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn add_factor(mut self, factor: RiskFactor) -> Self {
        self.factors.push(factor);
        self
    }

    #[must_use]
    pub fn probabilities(mut self, probabilities: OutcomeProbabilities) -> Self {
        self.probabilities = probabilities;
        self
    }

    #[must_use]
    pub fn report_only(mut self, report_only: bool) -> Self {
        self.report_only = report_only;
        self
    }

    #[must_use]
    pub fn generated_at(mut self, timestamp: impl Into<String>) -> Self {
        self.generated_at = Some(timestamp.into());
        self
    }

    #[must_use]
    pub fn build(self) -> RiskCertificate {
        let risk_score = calculate_risk_score(&self.factors);
        let risk_level = risk_level_from_score(risk_score);
        let recommendation = generate_recommendation(risk_level, risk_score, &self.probabilities);

        RiskCertificate {
            schema: RISK_CERTIFICATE_SCHEMA_V1.to_owned(),
            candidate_type: self.candidate_type.unwrap_or(CandidateType::Promote),
            target_memory_id: self.target_memory_id.unwrap_or_default(),
            risk_level,
            risk_score,
            factors: self.factors,
            probabilities: self.probabilities,
            recommendation,
            report_only: self.report_only,
            generated_at: self.generated_at.unwrap_or_default(),
        }
    }
}

fn calculate_risk_score(factors: &[RiskFactor]) -> f32 {
    if factors.is_empty() {
        return 0.0;
    }
    let total_weight: f32 = factors.iter().map(|f| f.weight).sum();
    if total_weight == 0.0 {
        return 0.0;
    }
    let weighted_sum: f32 = factors.iter().map(|f| f.weighted_contribution()).sum();
    (weighted_sum / total_weight).clamp(0.0, 1.0)
}

fn risk_level_from_score(score: f32) -> RiskLevel {
    if score < 0.25 {
        RiskLevel::Low
    } else if score < 0.50 {
        RiskLevel::Medium
    } else if score < 0.75 {
        RiskLevel::High
    } else {
        RiskLevel::Critical
    }
}

fn generate_recommendation(
    level: RiskLevel,
    score: f32,
    probabilities: &OutcomeProbabilities,
) -> RiskRecommendation {
    let confidence = 1.0 - score;
    match level {
        RiskLevel::Low => RiskRecommendation::proceed(
            confidence,
            format!(
                "Low risk (score {:.2}). Expected success rate: {:.0}%.",
                score,
                probabilities.expected_positive() * 100.0
            ),
        ),
        RiskLevel::Medium => {
            if probabilities.expected_positive() > 0.7 {
                RiskRecommendation::proceed(
                    confidence * 0.8,
                    format!(
                        "Medium risk but high success likelihood ({:.0}%). Proceed with monitoring.",
                        probabilities.expected_positive() * 100.0
                    ),
                )
            } else {
                RiskRecommendation::review(
                    confidence,
                    format!(
                        "Medium risk (score {:.2}). Review recommended before proceeding.",
                        score
                    ),
                )
            }
        }
        RiskLevel::High => RiskRecommendation::review(
            confidence,
            format!(
                "High risk (score {:.2}). Human review required. Negative outcome probability: {:.0}%.",
                score,
                probabilities.expected_negative() * 100.0
            ),
        ),
        RiskLevel::Critical => {
            if probabilities.cascade_failure > 0.1 {
                RiskRecommendation::reject(
                    confidence,
                    format!(
                        "Critical risk with cascade potential ({:.0}%). Action not recommended.",
                        probabilities.cascade_failure * 100.0
                    ),
                )
            } else {
                RiskRecommendation::defer(
                    confidence,
                    format!(
                        "Critical risk (score {:.2}). Defer until additional validation available.",
                        score
                    ),
                )
            }
        }
    }
}

/// Assess the risk of a curation candidate.
#[must_use]
pub fn assess_risk(candidate: &ValidatedCandidate, report_only: bool) -> RiskCertificate {
    let mut builder = RiskCertificate::builder()
        .candidate_type(candidate.candidate_type)
        .target_memory_id(&candidate.target_memory_id)
        .report_only(report_only);

    builder = builder.add_factor(RiskFactor::new(
        "irreversibility",
        0.3,
        candidate.candidate_type.irreversibility_score(),
        format!(
            "{} actions have {} reversibility",
            candidate.candidate_type,
            if candidate.candidate_type.irreversibility_score() > 0.5 {
                "low"
            } else {
                "high"
            }
        ),
    ));

    builder = builder.add_factor(RiskFactor::new(
        "confidence",
        0.25,
        1.0 - candidate.confidence,
        format!(
            "Candidate confidence is {:.0}%",
            candidate.confidence * 100.0
        ),
    ));

    let source_risk = match candidate.source_type {
        CandidateSource::HumanRequest => 0.1,
        CandidateSource::RuleEngine => 0.2,
        CandidateSource::FeedbackEvent => 0.3,
        CandidateSource::AgentInference => 0.5,
        CandidateSource::ContradictionDetected => 0.6,
        CandidateSource::DecayTrigger => 0.4,
    };
    builder = builder.add_factor(RiskFactor::new(
        "source_reliability",
        0.2,
        source_risk,
        format!(
            "Source type {} has {} reliability",
            candidate.source_type,
            if source_risk < 0.3 {
                "high"
            } else {
                "moderate"
            }
        ),
    ));

    let cascade_potential = if candidate.candidate_type == CandidateType::Tombstone
        || candidate.candidate_type == CandidateType::Retract
    {
        0.7
    } else if candidate.candidate_type == CandidateType::Supersede {
        0.5
    } else {
        0.2
    };
    builder = builder.add_factor(RiskFactor::new(
        "cascade_potential",
        0.25,
        cascade_potential,
        format!(
            "{} may affect {} downstream memories",
            candidate.candidate_type,
            if cascade_potential > 0.5 {
                "many"
            } else {
                "few"
            }
        ),
    ));

    let base_success = candidate.confidence * 0.7 + 0.2;
    builder = builder.probabilities(OutcomeProbabilities::new(
        base_success * 0.7,
        base_success * 0.2,
        0.1 * (1.0 - candidate.confidence),
        (1.0 - base_success) * 0.7,
        (1.0 - base_success) * 0.3 * cascade_potential,
    ));

    builder.build()
}

impl CandidateType {
    #[must_use]
    pub const fn irreversibility_score(self) -> f32 {
        match self {
            Self::Promote | Self::Deprecate => 0.2,
            Self::Consolidate | Self::Merge => 0.4,
            Self::Supersede | Self::Split => 0.5,
            Self::Retract => 0.7,
            Self::Tombstone => 0.9,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{
        CANDIDATE_TOO_GENERIC_CODE, CandidateInput, CandidateSource, CandidateStatus,
        CandidateType, CandidateValidationError, ParseCandidateSourceError,
        ParseCandidateStatusError, ParseCandidateTypeError, SpecificityPlatform, specificity_score,
        subsystem_name, validate_candidate, validate_status_transition,
    };

    #[test]
    fn subsystem_name_is_stable() {
        assert_eq!(subsystem_name(), "curate");
    }

    #[test]
    fn specificity_score_rejects_empty_input() {
        let report = specificity_score(" \n\t ");

        assert_eq!(report.score, 0.0);
        assert!(!report.passes_threshold);
        assert!(report.concrete_tokens.is_empty());
        assert!(report.rejected_reasons.contains(&"empty_input"));
        assert!(
            report
                .rejected_reasons
                .contains(&CANDIDATE_TOO_GENERIC_CODE)
        );
    }

    #[test]
    fn specificity_score_rejects_generic_platitudes() {
        let report = specificity_score("Always write good code and improve the system.");

        assert!(!report.passes_threshold);
        assert!(report.score < report.threshold);
        assert!(report.generic_tokens.contains(&"code".to_string()));
        assert!(report.generic_tokens.contains(&"system".to_string()));
        assert!(
            report
                .rejected_reasons
                .contains(&"no_concrete_tokens_found")
        );
    }

    #[test]
    fn specificity_score_accepts_release_rule_fixture() {
        let text = include_str!("../../tests/fixtures/specificity/positive_release_rule.txt");
        let report = specificity_score(text);

        assert!(report.passes_threshold, "{report:?}");
        assert!(report.score >= report.threshold);
        assert!(report.structural_signals.has_inline_command);
        assert!(report.structural_signals.has_branch_or_tag);
        assert!(report.structural_signals.has_provenance_uri);
    }

    #[test]
    fn specificity_score_detects_structural_signals() {
        let text = "\
Run this on Linux:
```bash
rch exec -- cargo test db
```
Then inspect src/db/mod.rs for E0308, keep p99 under 250ms, and land on main from file:docs/testing.md.";
        let report = specificity_score(text);

        assert!(report.passes_threshold, "{report:?}");
        assert!(report.structural_signals.has_command_block);
        assert!(report.structural_signals.has_file_path);
        assert!(report.structural_signals.has_error_code);
        assert!(report.structural_signals.has_metric_threshold);
        assert!(report.structural_signals.has_branch_or_tag);
        assert!(report.structural_signals.has_provenance_uri);
        assert_eq!(report.platform, Some(SpecificityPlatform::Linux));
    }

    #[test]
    fn specificity_score_redacts_sensitive_concrete_tokens() {
        let key_name = concat!("OPENAI", "_", "API", "_", "KEY");
        let key_value = concat!("sk", "-", "test");
        let text =
            format!("Run `cargo test` before using {key_name}={key_value} in src/config/file.rs.");

        let report = specificity_score(&text);

        assert!(report.passes_threshold, "{report:?}");
        assert_eq!(report.redacted_concrete_tokens.len(), 1);
        assert!(
            report
                .redacted_concrete_tokens
                .iter()
                .any(|token| token.value == concat!("REDACTED:", "api", "_", "key"))
        );
        assert!(
            report
                .concrete_tokens
                .iter()
                .all(|token| !token.value.contains(key_value))
        );
    }

    #[test]
    fn specificity_score_rejects_instruction_like_concrete_content() {
        let text = include_str!("../../tests/fixtures/specificity/negative_instruction_like.txt");
        let report = specificity_score(text);

        assert!(!report.passes_threshold);
        assert!(report.score >= report.threshold);
        assert!(report.structural_signals.has_instruction_like_content);
        assert!(
            report
                .rejected_reasons
                .contains(&"instruction_like_content")
        );
    }

    #[test]
    fn specificity_score_is_idempotent_and_whitespace_stable() {
        let compact =
            specificity_score("Run `cargo fmt --check` before editing src/curate/mod.rs.");
        let spaced =
            specificity_score("Run   `cargo fmt --check`\n\nbefore\tediting   src/curate/mod.rs.");
        let repeated =
            specificity_score("Run `cargo fmt --check` before editing src/curate/mod.rs.");

        assert_eq!(compact, repeated);
        assert_eq!(compact.score, spaced.score);
        assert_eq!(compact.concrete_tokens, spaced.concrete_tokens);
    }

    #[test]
    fn specificity_score_is_monotonic_when_adding_concrete_tokens() {
        let generic = specificity_score("Always write better code.");
        let concrete = specificity_score("Always write better code. Run `cargo fmt --check`.");

        assert!(concrete.score >= generic.score);
        assert!(concrete.structural_signals.has_inline_command);
    }

    #[test]
    fn specificity_fixture_corpus_matches_expectations() {
        let positives = [
            include_str!("../../tests/fixtures/specificity/positive_release_rule.txt"),
            include_str!("../../tests/fixtures/specificity/positive_migration_rule.txt"),
            include_str!("../../tests/fixtures/specificity/positive_metric_rule.txt"),
        ];
        for fixture in positives {
            let report = specificity_score(fixture);
            assert!(
                report.passes_threshold,
                "positive fixture failed: {report:?}"
            );
        }

        let negatives = [
            include_str!("../../tests/fixtures/specificity/negative_generic_platitude.txt"),
            include_str!("../../tests/fixtures/specificity/negative_fake_path.txt"),
            include_str!("../../tests/fixtures/specificity/negative_misspelled_command.txt"),
            include_str!("../../tests/fixtures/specificity/negative_instruction_like.txt"),
        ];
        for fixture in negatives {
            let report = specificity_score(fixture);
            assert!(
                !report.passes_threshold,
                "negative fixture passed unexpectedly: {report:?}"
            );
        }
    }

    #[test]
    fn specificity_score_handles_multilingual_context_with_concrete_command() {
        let report =
            specificity_score("Antes de release, run `cargo clippy --all-targets` on main.");

        assert!(report.passes_threshold, "{report:?}");
        assert!(report.structural_signals.has_inline_command);
    }

    #[test]
    fn specificity_score_handles_very_long_input_deterministically() {
        let mut text = "Always write good code. ".repeat(600);
        text.push_str("Run `rch exec -- cargo test curate` before editing src/curate/mod.rs.");

        let first = specificity_score(&text);
        let second = specificity_score(&text);

        assert_eq!(first, second);
        assert!(first.passes_threshold, "{first:?}");
    }

    #[test]
    fn candidate_type_round_trip_for_every_variant() {
        for ct in CandidateType::all() {
            let rendered = ct.to_string();
            let parsed = CandidateType::from_str(&rendered)
                .unwrap_or_else(|e| panic!("candidate type {ct:?} failed to round-trip: {e}"));
            assert_eq!(parsed, ct);
        }
    }

    #[test]
    fn candidate_type_rejects_unknown_input() {
        let err = CandidateType::from_str("unknown_type");
        assert!(matches!(err, Err(ParseCandidateTypeError { .. })));
    }

    #[test]
    fn candidate_source_round_trip_for_every_variant() {
        for cs in CandidateSource::all() {
            let rendered = cs.to_string();
            let parsed = CandidateSource::from_str(&rendered)
                .unwrap_or_else(|e| panic!("candidate source {cs:?} failed to round-trip: {e}"));
            assert_eq!(parsed, cs);
        }
    }

    #[test]
    fn candidate_source_rejects_unknown_input() {
        let err = CandidateSource::from_str("unknown_source");
        assert!(matches!(err, Err(ParseCandidateSourceError { .. })));
    }

    #[test]
    fn candidate_status_round_trip_for_every_variant() {
        for cs in CandidateStatus::all() {
            let rendered = cs.to_string();
            let parsed = CandidateStatus::from_str(&rendered)
                .unwrap_or_else(|e| panic!("candidate status {cs:?} failed to round-trip: {e}"));
            assert_eq!(parsed, cs);
        }
    }

    #[test]
    fn candidate_status_rejects_unknown_input() {
        let err = CandidateStatus::from_str("unknown_status");
        assert!(matches!(err, Err(ParseCandidateStatusError { .. })));
    }

    #[test]
    fn candidate_status_terminal_states() {
        assert!(!CandidateStatus::Pending.is_terminal());
        assert!(!CandidateStatus::Approved.is_terminal());
        assert!(CandidateStatus::Rejected.is_terminal());
        assert!(CandidateStatus::Expired.is_terminal());
        assert!(CandidateStatus::Applied.is_terminal());
    }

    fn valid_input() -> CandidateInput {
        CandidateInput {
            workspace_id: "ws_123".to_string(),
            candidate_type: CandidateType::Promote,
            target_memory_id: "mem_456".to_string(),
            proposed_content: None,
            proposed_confidence: Some(0.8),
            proposed_trust_class: Some("agent_validated".to_string()),
            source_type: CandidateSource::FeedbackEvent,
            source_id: Some("feedback_789".to_string()),
            reason: "Positive feedback received".to_string(),
            confidence: 0.75,
            ttl_seconds: Some(3600),
        }
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn validate_candidate_accepts_valid_input() {
        let input = valid_input();
        let result = validate_candidate(input, "2026-04-29T12:00:00Z");
        assert!(result.is_ok());
        let validated = result.unwrap();
        assert_eq!(validated.workspace_id, "ws_123");
        assert_eq!(validated.confidence, 0.75);
        assert!(validated.ttl_expires_at.is_some());
    }

    #[test]
    fn validate_candidate_rejects_empty_workspace_id() {
        let mut input = valid_input();
        input.workspace_id = "  ".to_string();
        let result = validate_candidate(input, "2026-04-29T12:00:00Z");
        assert!(matches!(
            result,
            Err(CandidateValidationError::EmptyWorkspaceId)
        ));
    }

    #[test]
    fn validate_candidate_rejects_empty_target_memory_id() {
        let mut input = valid_input();
        input.target_memory_id = "".to_string();
        let result = validate_candidate(input, "2026-04-29T12:00:00Z");
        assert!(matches!(
            result,
            Err(CandidateValidationError::EmptyTargetMemoryId)
        ));
    }

    #[test]
    fn validate_candidate_rejects_empty_reason() {
        let mut input = valid_input();
        input.reason = "   ".to_string();
        let result = validate_candidate(input, "2026-04-29T12:00:00Z");
        assert!(matches!(result, Err(CandidateValidationError::EmptyReason)));
    }

    #[test]
    fn validate_candidate_rejects_confidence_out_of_range() {
        let mut input = valid_input();
        input.confidence = 1.5;
        let result = validate_candidate(input, "2026-04-29T12:00:00Z");
        assert!(matches!(
            result,
            Err(CandidateValidationError::ConfidenceOutOfRange { .. })
        ));
    }

    #[test]
    fn validate_candidate_rejects_proposed_confidence_out_of_range() {
        let mut input = valid_input();
        input.proposed_confidence = Some(-0.1);
        let result = validate_candidate(input, "2026-04-29T12:00:00Z");
        assert!(matches!(
            result,
            Err(CandidateValidationError::ProposedConfidenceOutOfRange { .. })
        ));
    }

    #[test]
    fn validate_candidate_rejects_invalid_trust_class() {
        let mut input = valid_input();
        input.proposed_trust_class = Some("invalid_class".to_string());
        let result = validate_candidate(input, "2026-04-29T12:00:00Z");
        assert!(matches!(
            result,
            Err(CandidateValidationError::InvalidProposedTrustClass { .. })
        ));
    }

    #[test]
    fn validate_candidate_requires_content_for_consolidate() {
        let mut input = valid_input();
        input.candidate_type = CandidateType::Consolidate;
        input.proposed_content = None;
        let result = validate_candidate(input, "2026-04-29T12:00:00Z");
        assert!(matches!(
            result,
            Err(CandidateValidationError::ContentRequiredForType { .. })
        ));
    }

    #[test]
    fn validate_candidate_rejects_generic_proposed_content() {
        let mut input = valid_input();
        input.candidate_type = CandidateType::Consolidate;
        input.proposed_content = Some("Always write good code.".to_string());

        let result = validate_candidate(input, "2026-04-29T12:00:00Z");

        match result {
            Err(CandidateValidationError::CandidateTooGeneric {
                rejected_reasons, ..
            }) => {
                assert!(rejected_reasons.contains(&CANDIDATE_TOO_GENERIC_CODE));
                assert!(rejected_reasons.contains(&"below_specificity_threshold"));
            }
            other => panic!("expected generic rejection, got {other:?}"),
        }
    }

    #[test]
    fn validate_candidate_accepts_specific_proposed_content() {
        let mut input = valid_input();
        input.candidate_type = CandidateType::Consolidate;
        input.proposed_content =
            Some("Run `cargo fmt --check` before editing src/curate/mod.rs on main.".to_string());

        let result = validate_candidate(input, "2026-04-29T12:00:00Z");

        match result {
            Ok(candidate) => {
                let Some(report) = candidate.specificity_report else {
                    panic!("expected specificity report");
                };
                assert!(report.passes_threshold, "{report:?}");
            }
            Err(error) => panic!("specific candidate should pass: {error:?}"),
        }
    }

    #[test]
    fn candidate_too_generic_error_exposes_stable_code() {
        let error = CandidateValidationError::CandidateTooGeneric {
            score: "0.0000".to_string(),
            threshold: "0.4500".to_string(),
            rejected_reasons: vec![CANDIDATE_TOO_GENERIC_CODE],
        };

        assert_eq!(error.code(), CANDIDATE_TOO_GENERIC_CODE);
        assert!(error.to_string().contains(CANDIDATE_TOO_GENERIC_CODE));
    }

    #[test]
    fn validate_candidate_forbids_content_for_tombstone() {
        let mut input = valid_input();
        input.candidate_type = CandidateType::Tombstone;
        input.proposed_content = Some("should not be here".to_string());
        let result = validate_candidate(input, "2026-04-29T12:00:00Z");
        assert!(matches!(
            result,
            Err(CandidateValidationError::ContentForbiddenForType { .. })
        ));
    }

    #[test]
    fn validate_status_transition_allows_valid_transitions() {
        assert!(
            validate_status_transition(CandidateStatus::Pending, CandidateStatus::Approved).is_ok()
        );
        assert!(
            validate_status_transition(CandidateStatus::Pending, CandidateStatus::Rejected).is_ok()
        );
        assert!(
            validate_status_transition(CandidateStatus::Pending, CandidateStatus::Expired).is_ok()
        );
        assert!(
            validate_status_transition(CandidateStatus::Approved, CandidateStatus::Applied).is_ok()
        );
        assert!(
            validate_status_transition(CandidateStatus::Approved, CandidateStatus::Rejected)
                .is_ok()
        );
    }

    #[test]
    fn validate_status_transition_rejects_terminal_source() {
        let result = validate_status_transition(CandidateStatus::Applied, CandidateStatus::Pending);
        assert!(matches!(
            result,
            Err(CandidateValidationError::CandidateAlreadyTerminal { .. })
        ));
    }

    #[test]
    fn validate_status_transition_rejects_invalid_transition() {
        let result = validate_status_transition(CandidateStatus::Pending, CandidateStatus::Applied);
        assert!(matches!(
            result,
            Err(CandidateValidationError::InvalidStatusTransition { .. })
        ));
    }

    #[test]
    fn candidate_type_content_requirements() {
        assert!(CandidateType::Consolidate.requires_content());
        assert!(CandidateType::Supersede.requires_content());
        assert!(CandidateType::Merge.requires_content());
        assert!(CandidateType::Split.requires_content());
        assert!(!CandidateType::Promote.requires_content());
        assert!(!CandidateType::Deprecate.requires_content());

        assert!(CandidateType::Tombstone.forbids_content());
        assert!(CandidateType::Retract.forbids_content());
        assert!(!CandidateType::Promote.forbids_content());
    }

    // ========================================================================
    // EE-346: Risk Certificate Tests
    // ========================================================================

    use super::{
        OutcomeProbabilities, ParseRiskLevelError, RISK_CERTIFICATE_SCHEMA_V1, RiskCertificate,
        RiskFactor, RiskLevel, RiskRecommendation, ValidatedCandidate, assess_risk,
    };

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn risk_level_as_str() -> TestResult {
        ensure(RiskLevel::Low.as_str(), "low", "low")?;
        ensure(RiskLevel::Medium.as_str(), "medium", "medium")?;
        ensure(RiskLevel::High.as_str(), "high", "high")?;
        ensure(RiskLevel::Critical.as_str(), "critical", "critical")
    }

    #[test]
    fn risk_level_parse_roundtrip() -> TestResult {
        for level in RiskLevel::all() {
            let s = level.as_str();
            let parsed: RiskLevel = s.parse().map_err(|e: ParseRiskLevelError| e.to_string())?;
            ensure(parsed, level, &format!("roundtrip {s}"))?;
        }
        Ok(())
    }

    #[test]
    fn risk_level_requires_human_review() -> TestResult {
        ensure(RiskLevel::Low.requires_human_review(), false, "low")?;
        ensure(RiskLevel::Medium.requires_human_review(), false, "medium")?;
        ensure(RiskLevel::High.requires_human_review(), true, "high")?;
        ensure(
            RiskLevel::Critical.requires_human_review(),
            true,
            "critical",
        )
    }

    #[test]
    fn risk_level_numeric() -> TestResult {
        ensure(RiskLevel::Low.numeric_level(), 1, "low")?;
        ensure(RiskLevel::Medium.numeric_level(), 2, "medium")?;
        ensure(RiskLevel::High.numeric_level(), 3, "high")?;
        ensure(RiskLevel::Critical.numeric_level(), 4, "critical")
    }

    #[test]
    fn risk_factor_weighted_contribution() {
        let factor = RiskFactor::new("test", 0.5, 0.8, "test reason");
        let expected = 0.4;
        let actual = factor.weighted_contribution();
        assert!((actual - expected).abs() < 0.001);
    }

    #[test]
    fn risk_factor_clamps_values() {
        let factor = RiskFactor::new("test", 1.5, -0.2, "test");
        assert!(factor.weight <= 1.0);
        assert!(factor.contribution >= 0.0);
    }

    #[test]
    fn outcome_probabilities_total() {
        let probs = OutcomeProbabilities::new(0.5, 0.2, 0.1, 0.15, 0.05);
        let total = probs.total();
        assert!((total - 1.0).abs() < 0.001);
    }

    #[test]
    fn outcome_probabilities_is_calibrated() {
        let calibrated = OutcomeProbabilities::new(0.5, 0.2, 0.1, 0.15, 0.05);
        assert!(calibrated.is_calibrated());

        let uncalibrated = OutcomeProbabilities::new(0.9, 0.9, 0.9, 0.9, 0.9);
        assert!(!uncalibrated.is_calibrated());
    }

    #[test]
    fn outcome_probabilities_expected_values() {
        let probs = OutcomeProbabilities::new(0.5, 0.2, 0.1, 0.15, 0.05);
        assert!((probs.expected_positive() - 0.7).abs() < 0.001);
        assert!((probs.expected_negative() - 0.2).abs() < 0.001);
    }

    #[test]
    fn risk_recommendation_constructors() {
        let proceed = RiskRecommendation::proceed(0.9, "safe");
        assert_eq!(proceed.action, "proceed");
        assert!((proceed.confidence - 0.9).abs() < 0.001);

        let review = RiskRecommendation::review(0.8, "needs review");
        assert_eq!(review.action, "review");

        let defer = RiskRecommendation::defer(0.7, "wait");
        assert_eq!(defer.action, "defer");

        let reject = RiskRecommendation::reject(0.6, "too risky");
        assert_eq!(reject.action, "reject");
    }

    #[test]
    fn risk_certificate_builder_defaults() {
        let cert = RiskCertificate::builder()
            .target_memory_id("mem-001")
            .build();

        assert_eq!(cert.schema, RISK_CERTIFICATE_SCHEMA_V1);
        assert_eq!(cert.target_memory_id, "mem-001");
        assert!(!cert.report_only);
    }

    #[test]
    fn risk_certificate_builder_with_factors() {
        let cert = RiskCertificate::builder()
            .candidate_type(CandidateType::Tombstone)
            .target_memory_id("mem-002")
            .add_factor(RiskFactor::new(
                "irreversibility",
                0.5,
                0.9,
                "tombstone is permanent",
            ))
            .add_factor(RiskFactor::new(
                "cascade",
                0.3,
                0.7,
                "may affect downstream",
            ))
            .report_only(true)
            .build();

        assert_eq!(cert.candidate_type, CandidateType::Tombstone);
        assert_eq!(cert.factors.len(), 2);
        assert!(cert.report_only);
        assert!(cert.risk_score > 0.0);
    }

    #[test]
    fn risk_certificate_requires_human_review() {
        let low_risk = RiskCertificate::builder()
            .add_factor(RiskFactor::new("test", 1.0, 0.1, "low"))
            .build();
        assert!(!low_risk.requires_human_review());

        let high_risk = RiskCertificate::builder()
            .add_factor(RiskFactor::new("test", 1.0, 0.8, "high"))
            .build();
        assert!(high_risk.requires_human_review());
    }

    #[test]
    fn risk_certificate_is_actionable() {
        let actionable = RiskCertificate::builder()
            .add_factor(RiskFactor::new("test", 1.0, 0.1, "low"))
            .report_only(false)
            .build();
        assert!(actionable.is_actionable());

        let report_only = RiskCertificate::builder()
            .add_factor(RiskFactor::new("test", 1.0, 0.1, "low"))
            .report_only(true)
            .build();
        assert!(!report_only.is_actionable());

        let high_risk = RiskCertificate::builder()
            .add_factor(RiskFactor::new("test", 1.0, 0.8, "high"))
            .report_only(false)
            .build();
        assert!(!high_risk.is_actionable());
    }

    #[test]
    fn assess_risk_low_confidence_candidate() {
        let candidate = ValidatedCandidate {
            workspace_id: "ws-001".to_owned(),
            candidate_type: CandidateType::Promote,
            target_memory_id: "mem-001".to_owned(),
            proposed_content: None,
            specificity_report: None,
            proposed_confidence: Some(0.9),
            proposed_trust_class: None,
            source_type: CandidateSource::HumanRequest,
            source_id: None,
            reason: "test".to_owned(),
            confidence: 0.3,
            ttl_expires_at: None,
        };

        let cert = assess_risk(&candidate, true);
        assert!(cert.report_only);
        assert!(cert.risk_score > 0.3);
    }

    #[test]
    fn assess_risk_tombstone_high_risk() {
        let candidate = ValidatedCandidate {
            workspace_id: "ws-001".to_owned(),
            candidate_type: CandidateType::Tombstone,
            target_memory_id: "mem-001".to_owned(),
            proposed_content: None,
            specificity_report: None,
            proposed_confidence: None,
            proposed_trust_class: None,
            source_type: CandidateSource::AgentInference,
            source_id: None,
            reason: "no longer relevant".to_owned(),
            confidence: 0.5,
            ttl_expires_at: None,
        };

        let cert = assess_risk(&candidate, false);
        assert!(cert.risk_level >= RiskLevel::Medium);
        assert!(cert.factors.len() >= 4);
    }

    #[test]
    fn assess_risk_human_request_lower_risk() {
        let candidate = ValidatedCandidate {
            workspace_id: "ws-001".to_owned(),
            candidate_type: CandidateType::Promote,
            target_memory_id: "mem-001".to_owned(),
            proposed_content: None,
            specificity_report: None,
            proposed_confidence: Some(0.95),
            proposed_trust_class: None,
            source_type: CandidateSource::HumanRequest,
            source_id: None,
            reason: "verified correct".to_owned(),
            confidence: 0.95,
            ttl_expires_at: None,
        };

        let cert = assess_risk(&candidate, false);
        assert_eq!(cert.risk_level, RiskLevel::Low);
        assert_eq!(cert.recommendation.action, "proceed");
    }

    #[test]
    fn candidate_type_irreversibility_scores() {
        assert!(CandidateType::Tombstone.irreversibility_score() > 0.8);
        assert!(CandidateType::Retract.irreversibility_score() > 0.6);
        assert!(CandidateType::Promote.irreversibility_score() < 0.3);
        assert!(CandidateType::Deprecate.irreversibility_score() < 0.3);
    }
}
