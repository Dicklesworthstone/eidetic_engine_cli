//! Situation, task-signature, routing, and situation-link schema contracts (EE-420).
//!
//! These records describe task shape and routing evidence. They are stable
//! wire contracts for later `ee situation` routing commands; defining them here
//! does not persist, classify, or link situations by itself.

use std::fmt;
use std::str::FromStr;

// ============================================================================
// Schema Constants
// ============================================================================

/// Schema for situation classification command results.
pub const SITUATION_CLASSIFY_SCHEMA_V1: &str = "ee.situation.classify.v1";

/// Schema for situation details command output.
pub const SITUATION_SHOW_SCHEMA_V1: &str = "ee.situation.show.v1";

/// Schema for situation explanation command output.
pub const SITUATION_EXPLAIN_SCHEMA_V1: &str = "ee.situation.explain.v1";

/// Schema for a normalized situation record.
pub const SITUATION_SCHEMA_V1: &str = "ee.situation.v1";

/// Schema for deterministic task signatures.
pub const TASK_SIGNATURE_SCHEMA_V1: &str = "ee.task_signature.v1";

/// Schema for evidence features extracted from a task signature.
pub const FEATURE_EVIDENCE_SCHEMA_V1: &str = "ee.situation.feature_evidence.v1";

/// Schema for routing choices derived from a situation.
pub const ROUTING_DECISION_SCHEMA_V1: &str = "ee.situation.routing_decision.v1";

/// Schema for explainable links between situations.
pub const SITUATION_LINK_SCHEMA_V1: &str = "ee.situation.link.v1";

/// Schema for the situation schema catalog.
pub const SITUATION_SCHEMA_CATALOG_V1: &str = "ee.situation.schemas.v1";

const JSON_SCHEMA_DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

// ============================================================================
// Stable Wire Enums
// ============================================================================

/// Category of task situation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SituationCategory {
    BugFix,
    Feature,
    Refactor,
    Investigation,
    Documentation,
    Testing,
    Configuration,
    Deployment,
    Review,
    Unknown,
}

impl SituationCategory {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BugFix => "bug_fix",
            Self::Feature => "feature",
            Self::Refactor => "refactor",
            Self::Investigation => "investigation",
            Self::Documentation => "documentation",
            Self::Testing => "testing",
            Self::Configuration => "configuration",
            Self::Deployment => "deployment",
            Self::Review => "review",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::BugFix => "Fixing a bug or defect in existing functionality",
            Self::Feature => "Adding new functionality or capability",
            Self::Refactor => "Restructuring code without changing behavior",
            Self::Investigation => "Exploring or debugging to understand a problem",
            Self::Documentation => "Writing or updating documentation",
            Self::Testing => "Adding or modifying tests",
            Self::Configuration => "Changing configuration or settings",
            Self::Deployment => "Deploying or releasing changes",
            Self::Review => "Reviewing code or design",
            Self::Unknown => "Situation category could not be determined",
        }
    }

    /// All known categories for stable iteration.
    pub const ALL: &'static [Self] = &[
        Self::BugFix,
        Self::Feature,
        Self::Refactor,
        Self::Investigation,
        Self::Documentation,
        Self::Testing,
        Self::Configuration,
        Self::Deployment,
        Self::Review,
        Self::Unknown,
    ];
}

impl fmt::Display for SituationCategory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SituationCategory {
    type Err = ParseSituationValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let normalized = input.trim().to_ascii_lowercase().replace('-', "_");
        match normalized.as_str() {
            "bug_fix" | "bugfix" | "fix" => Ok(Self::BugFix),
            "feature" | "feat" => Ok(Self::Feature),
            "refactor" | "refactoring" => Ok(Self::Refactor),
            "investigation" | "investigate" | "debug" => Ok(Self::Investigation),
            "documentation" | "docs" | "doc" => Ok(Self::Documentation),
            "testing" | "test" | "tests" => Ok(Self::Testing),
            "configuration" | "config" | "cfg" => Ok(Self::Configuration),
            "deployment" | "deploy" | "release" => Ok(Self::Deployment),
            "review" | "code_review" => Ok(Self::Review),
            "unknown" => Ok(Self::Unknown),
            _ => Err(ParseSituationValueError::new(
                "situation_category",
                input,
                "bug_fix, feature, refactor, investigation, documentation, testing, configuration, deployment, review, unknown",
            )),
        }
    }
}

/// Confidence band for deterministic situation classification and routing.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SituationConfidence {
    High,
    Medium,
    Low,
}

impl SituationConfidence {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    #[must_use]
    pub const fn threshold(self) -> f32 {
        match self {
            Self::High => 0.8,
            Self::Medium => 0.5,
            Self::Low => 0.0,
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::High, Self::Medium, Self::Low]
    }
}

impl fmt::Display for SituationConfidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SituationConfidence {
    type Err = ParseSituationValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "high" => Ok(Self::High),
            "medium" => Ok(Self::Medium),
            "low" => Ok(Self::Low),
            _ => Err(ParseSituationValueError::new(
                "situation_confidence",
                input,
                "high, medium, low",
            )),
        }
    }
}

/// Feature kind extracted from task text, repository state, or command history.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SituationFeatureType {
    Keyword,
    Path,
    Command,
    Dependency,
    ErrorSignal,
    RepositoryFingerprint,
    AgentIntent,
}

impl SituationFeatureType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Keyword => "keyword",
            Self::Path => "path",
            Self::Command => "command",
            Self::Dependency => "dependency",
            Self::ErrorSignal => "error_signal",
            Self::RepositoryFingerprint => "repository_fingerprint",
            Self::AgentIntent => "agent_intent",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 7] {
        [
            Self::Keyword,
            Self::Path,
            Self::Command,
            Self::Dependency,
            Self::ErrorSignal,
            Self::RepositoryFingerprint,
            Self::AgentIntent,
        ]
    }
}

impl fmt::Display for SituationFeatureType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SituationFeatureType {
    type Err = ParseSituationValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "keyword" => Ok(Self::Keyword),
            "path" => Ok(Self::Path),
            "command" => Ok(Self::Command),
            "dependency" => Ok(Self::Dependency),
            "error_signal" => Ok(Self::ErrorSignal),
            "repository_fingerprint" => Ok(Self::RepositoryFingerprint),
            "agent_intent" => Ok(Self::AgentIntent),
            _ => Err(ParseSituationValueError::new(
                "situation_feature_type",
                input,
                "keyword, path, command, dependency, error_signal, repository_fingerprint, agent_intent",
            )),
        }
    }
}

/// Stable routing surfaces that may consume a situation signature.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SituationRoutingSurface {
    ContextProfile,
    PreflightProfile,
    ProcedureCandidate,
    FixtureFamily,
    TripwireCandidate,
    CounterfactualReplay,
    ManualReview,
}

impl SituationRoutingSurface {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ContextProfile => "context_profile",
            Self::PreflightProfile => "preflight_profile",
            Self::ProcedureCandidate => "procedure_candidate",
            Self::FixtureFamily => "fixture_family",
            Self::TripwireCandidate => "tripwire_candidate",
            Self::CounterfactualReplay => "counterfactual_replay",
            Self::ManualReview => "manual_review",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 7] {
        [
            Self::ContextProfile,
            Self::PreflightProfile,
            Self::ProcedureCandidate,
            Self::FixtureFamily,
            Self::TripwireCandidate,
            Self::CounterfactualReplay,
            Self::ManualReview,
        ]
    }
}

impl fmt::Display for SituationRoutingSurface {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SituationRoutingSurface {
    type Err = ParseSituationValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "context_profile" => Ok(Self::ContextProfile),
            "preflight_profile" => Ok(Self::PreflightProfile),
            "procedure_candidate" => Ok(Self::ProcedureCandidate),
            "fixture_family" => Ok(Self::FixtureFamily),
            "tripwire_candidate" => Ok(Self::TripwireCandidate),
            "counterfactual_replay" => Ok(Self::CounterfactualReplay),
            "manual_review" => Ok(Self::ManualReview),
            _ => Err(ParseSituationValueError::new(
                "situation_routing_surface",
                input,
                "context_profile, preflight_profile, procedure_candidate, fixture_family, tripwire_candidate, counterfactual_replay, manual_review",
            )),
        }
    }
}

/// Replay posture selected by routing for this situation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SituationReplayPolicy {
    NotEligible,
    DryRunOnly,
    Allowed,
}

impl SituationReplayPolicy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotEligible => "not_eligible",
            Self::DryRunOnly => "dry_run_only",
            Self::Allowed => "allowed",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::NotEligible, Self::DryRunOnly, Self::Allowed]
    }
}

impl fmt::Display for SituationReplayPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SituationReplayPolicy {
    type Err = ParseSituationValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "not_eligible" => Ok(Self::NotEligible),
            "dry_run_only" => Ok(Self::DryRunOnly),
            "allowed" => Ok(Self::Allowed),
            _ => Err(ParseSituationValueError::new(
                "situation_replay_policy",
                input,
                "not_eligible, dry_run_only, allowed",
            )),
        }
    }
}

/// Explainable relation between two situations.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SituationLinkRelation {
    Similar,
    Refines,
    Supersedes,
    Blocks,
    Contrasts,
    CoOccurs,
}

impl SituationLinkRelation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Similar => "similar",
            Self::Refines => "refines",
            Self::Supersedes => "supersedes",
            Self::Blocks => "blocks",
            Self::Contrasts => "contrasts",
            Self::CoOccurs => "co_occurs",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 6] {
        [
            Self::Similar,
            Self::Refines,
            Self::Supersedes,
            Self::Blocks,
            Self::Contrasts,
            Self::CoOccurs,
        ]
    }
}

impl fmt::Display for SituationLinkRelation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SituationLinkRelation {
    type Err = ParseSituationValueError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "similar" => Ok(Self::Similar),
            "refines" => Ok(Self::Refines),
            "supersedes" => Ok(Self::Supersedes),
            "blocks" => Ok(Self::Blocks),
            "contrasts" => Ok(Self::Contrasts),
            "co_occurs" | "co-occurs" => Ok(Self::CoOccurs),
            _ => Err(ParseSituationValueError::new(
                "situation_link_relation",
                input,
                "similar, refines, supersedes, blocks, contrasts, co_occurs",
            )),
        }
    }
}

/// Error returned when a stable situation wire value cannot be parsed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseSituationValueError {
    field: &'static str,
    value: String,
    expected: &'static str,
}

impl ParseSituationValueError {
    #[must_use]
    pub fn new(field: &'static str, value: impl Into<String>, expected: &'static str) -> Self {
        Self {
            field,
            value: value.into(),
            expected,
        }
    }

    #[must_use]
    pub const fn field(&self) -> &'static str {
        self.field
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    #[must_use]
    pub const fn expected(&self) -> &'static str {
        self.expected
    }
}

impl fmt::Display for ParseSituationValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid {} value {:?}; expected {}",
            self.field, self.value, self.expected
        )
    }
}

impl std::error::Error for ParseSituationValueError {}

// ============================================================================
// Domain Records
// ============================================================================

/// Normalized situation record that other EE subsystems may route on.
#[derive(Clone, Debug, PartialEq)]
pub struct Situation {
    pub schema: &'static str,
    pub situation_id: String,
    pub category: SituationCategory,
    pub task_signature_id: String,
    pub title: String,
    pub summary: String,
    pub source_text_hash: String,
    pub confidence: SituationConfidence,
    pub confidence_score: f32,
    pub evidence_ids: Vec<String>,
    pub link_count: u32,
    pub created_at: String,
    pub updated_at: String,
}

impl Situation {
    #[must_use]
    pub fn new(
        situation_id: impl Into<String>,
        category: SituationCategory,
        task_signature_id: impl Into<String>,
        title: impl Into<String>,
        summary: impl Into<String>,
        source_text_hash: impl Into<String>,
        created_at: impl Into<String>,
    ) -> Self {
        let created_at = created_at.into();
        Self {
            schema: SITUATION_SCHEMA_V1,
            situation_id: situation_id.into(),
            category,
            task_signature_id: task_signature_id.into(),
            title: title.into(),
            summary: summary.into(),
            source_text_hash: source_text_hash.into(),
            confidence: SituationConfidence::Low,
            confidence_score: 0.0,
            evidence_ids: Vec::new(),
            link_count: 0,
            updated_at: created_at.clone(),
            created_at,
        }
    }

    #[must_use]
    pub fn with_confidence(mut self, confidence: SituationConfidence, score: f32) -> Self {
        self.confidence = confidence;
        self.confidence_score = bounded_unit(score);
        self
    }

    #[must_use]
    pub fn with_evidence(mut self, evidence_id: impl Into<String>) -> Self {
        self.evidence_ids.push(evidence_id.into());
        self
    }

    #[must_use]
    pub const fn with_link_count(mut self, link_count: u32) -> Self {
        self.link_count = link_count;
        self
    }
}

/// Deterministic task signature extracted from text and repository context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskSignature {
    pub schema: &'static str,
    pub signature_id: String,
    pub normalized_text_hash: String,
    pub repository_fingerprint: Option<String>,
    pub language_set: Vec<String>,
    pub command_families: Vec<String>,
    pub path_globs: Vec<String>,
    pub risk_tags: Vec<String>,
    pub feature_count: u32,
    pub created_at: String,
}

impl TaskSignature {
    #[must_use]
    pub fn new(
        signature_id: impl Into<String>,
        normalized_text_hash: impl Into<String>,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: TASK_SIGNATURE_SCHEMA_V1,
            signature_id: signature_id.into(),
            normalized_text_hash: normalized_text_hash.into(),
            repository_fingerprint: None,
            language_set: Vec::new(),
            command_families: Vec::new(),
            path_globs: Vec::new(),
            risk_tags: Vec::new(),
            feature_count: 0,
            created_at: created_at.into(),
        }
    }

    #[must_use]
    pub fn repository_fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
        self.repository_fingerprint = Some(fingerprint.into());
        self
    }

    #[must_use]
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language_set.push(language.into());
        self
    }

    #[must_use]
    pub fn with_command_family(mut self, family: impl Into<String>) -> Self {
        self.command_families.push(family.into());
        self
    }

    #[must_use]
    pub fn with_path_glob(mut self, path_glob: impl Into<String>) -> Self {
        self.path_globs.push(path_glob.into());
        self
    }

    #[must_use]
    pub fn with_risk_tag(mut self, risk_tag: impl Into<String>) -> Self {
        self.risk_tags.push(risk_tag.into());
        self
    }

    #[must_use]
    pub const fn with_feature_count(mut self, feature_count: u32) -> Self {
        self.feature_count = feature_count;
        self
    }
}

/// Evidence feature extracted from a task signature.
#[derive(Clone, Debug, PartialEq)]
pub struct FeatureEvidence {
    pub schema: &'static str,
    pub evidence_id: String,
    pub signature_id: String,
    pub feature_name: String,
    pub feature_type: SituationFeatureType,
    pub value: String,
    pub weight: f32,
    pub source: String,
    pub observed_at: String,
}

impl FeatureEvidence {
    #[must_use]
    pub fn new(
        evidence_id: impl Into<String>,
        signature_id: impl Into<String>,
        feature_name: impl Into<String>,
        feature_type: SituationFeatureType,
        value: impl Into<String>,
        source: impl Into<String>,
        observed_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: FEATURE_EVIDENCE_SCHEMA_V1,
            evidence_id: evidence_id.into(),
            signature_id: signature_id.into(),
            feature_name: feature_name.into(),
            feature_type,
            value: value.into(),
            weight: 1.0,
            source: source.into(),
            observed_at: observed_at.into(),
        }
    }

    #[must_use]
    pub fn with_weight(mut self, weight: f32) -> Self {
        self.weight = bounded_unit(weight);
        self
    }
}

/// Deterministic routing decision derived from a situation.
#[derive(Clone, Debug, PartialEq)]
pub struct RoutingDecision {
    pub schema: &'static str,
    pub routing_id: String,
    pub situation_id: String,
    pub surface: SituationRoutingSurface,
    pub confidence: SituationConfidence,
    pub confidence_score: f32,
    pub selected_profile: Option<String>,
    pub retrieval_profile: Option<String>,
    pub preflight_profile: Option<String>,
    pub procedure_candidate_ids: Vec<String>,
    pub fixture_ids: Vec<String>,
    pub tripwire_candidate_ids: Vec<String>,
    pub replay_policy: SituationReplayPolicy,
    pub reasons: Vec<String>,
    pub created_at: String,
}

impl RoutingDecision {
    #[must_use]
    pub fn new(
        routing_id: impl Into<String>,
        situation_id: impl Into<String>,
        surface: SituationRoutingSurface,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: ROUTING_DECISION_SCHEMA_V1,
            routing_id: routing_id.into(),
            situation_id: situation_id.into(),
            surface,
            confidence: SituationConfidence::Low,
            confidence_score: 0.0,
            selected_profile: None,
            retrieval_profile: None,
            preflight_profile: None,
            procedure_candidate_ids: Vec::new(),
            fixture_ids: Vec::new(),
            tripwire_candidate_ids: Vec::new(),
            replay_policy: SituationReplayPolicy::NotEligible,
            reasons: Vec::new(),
            created_at: created_at.into(),
        }
    }

    #[must_use]
    pub fn with_confidence(mut self, confidence: SituationConfidence, score: f32) -> Self {
        self.confidence = confidence;
        self.confidence_score = bounded_unit(score);
        self
    }

    #[must_use]
    pub fn selected_profile(mut self, profile: impl Into<String>) -> Self {
        self.selected_profile = Some(profile.into());
        self
    }

    #[must_use]
    pub fn retrieval_profile(mut self, profile: impl Into<String>) -> Self {
        self.retrieval_profile = Some(profile.into());
        self
    }

    #[must_use]
    pub fn preflight_profile(mut self, profile: impl Into<String>) -> Self {
        self.preflight_profile = Some(profile.into());
        self
    }

    #[must_use]
    pub fn with_procedure_candidate(mut self, procedure_id: impl Into<String>) -> Self {
        self.procedure_candidate_ids.push(procedure_id.into());
        self
    }

    #[must_use]
    pub fn with_fixture(mut self, fixture_id: impl Into<String>) -> Self {
        self.fixture_ids.push(fixture_id.into());
        self
    }

    #[must_use]
    pub fn with_tripwire_candidate(mut self, tripwire_id: impl Into<String>) -> Self {
        self.tripwire_candidate_ids.push(tripwire_id.into());
        self
    }

    #[must_use]
    pub const fn replay_policy(mut self, replay_policy: SituationReplayPolicy) -> Self {
        self.replay_policy = replay_policy;
        self
    }

    #[must_use]
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reasons.push(reason.into());
        self
    }
}

/// Explainable edge between two situation records.
#[derive(Clone, Debug, PartialEq)]
pub struct SituationLink {
    pub schema: &'static str,
    pub link_id: String,
    pub source_situation_id: String,
    pub target_situation_id: String,
    pub relation: SituationLinkRelation,
    pub directed: bool,
    pub confidence: SituationConfidence,
    pub confidence_score: f32,
    pub evidence_ids: Vec<String>,
    pub created_at: String,
}

impl SituationLink {
    #[must_use]
    pub fn new(
        link_id: impl Into<String>,
        source_situation_id: impl Into<String>,
        target_situation_id: impl Into<String>,
        relation: SituationLinkRelation,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            schema: SITUATION_LINK_SCHEMA_V1,
            link_id: link_id.into(),
            source_situation_id: source_situation_id.into(),
            target_situation_id: target_situation_id.into(),
            relation,
            directed: false,
            confidence: SituationConfidence::Low,
            confidence_score: 0.0,
            evidence_ids: Vec::new(),
            created_at: created_at.into(),
        }
    }

    #[must_use]
    pub const fn directed(mut self) -> Self {
        self.directed = true;
        self
    }

    #[must_use]
    pub fn with_confidence(mut self, confidence: SituationConfidence, score: f32) -> Self {
        self.confidence = confidence;
        self.confidence_score = bounded_unit(score);
        self
    }

    #[must_use]
    pub fn with_evidence(mut self, evidence_id: impl Into<String>) -> Self {
        self.evidence_ids.push(evidence_id.into());
        self
    }
}

fn bounded_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

// ============================================================================
// Schema Catalog
// ============================================================================

/// Field descriptor used by the situation schema catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SituationFieldSchema {
    pub name: &'static str,
    pub type_name: &'static str,
    pub required: bool,
    pub description: &'static str,
}

impl SituationFieldSchema {
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

/// Stable JSON-schema-like catalog entry for situation records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SituationObjectSchema {
    pub schema_name: &'static str,
    pub schema_uri: &'static str,
    pub kind: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub fields: &'static [SituationFieldSchema],
}

impl SituationObjectSchema {
    #[must_use]
    pub fn required_count(&self) -> usize {
        self.fields.iter().filter(|field| field.required).count()
    }
}

const SITUATION_FIELDS: &[SituationFieldSchema] = &[
    SituationFieldSchema::new("schema", "string", true, "Schema identifier."),
    SituationFieldSchema::new(
        "situationId",
        "string",
        true,
        "Stable situation identifier.",
    ),
    SituationFieldSchema::new("category", "string", true, "Stable situation category."),
    SituationFieldSchema::new(
        "taskSignatureId",
        "string",
        true,
        "Task signature this situation was derived from.",
    ),
    SituationFieldSchema::new("title", "string", true, "Short situation title."),
    SituationFieldSchema::new("summary", "string", true, "Short situation summary."),
    SituationFieldSchema::new(
        "sourceTextHash",
        "string",
        true,
        "Hash of the normalized task text used to classify the situation.",
    ),
    SituationFieldSchema::new(
        "confidence",
        "string",
        true,
        "Classification confidence band.",
    ),
    SituationFieldSchema::new(
        "confidenceScore",
        "number",
        true,
        "Bounded confidence score from 0.0 to 1.0.",
    ),
    SituationFieldSchema::new(
        "evidenceIds",
        "array<string>",
        true,
        "Feature evidence identifiers supporting this situation.",
    ),
    SituationFieldSchema::new(
        "linkCount",
        "integer",
        true,
        "Number of known links to other situations.",
    ),
    SituationFieldSchema::new("createdAt", "string", true, "RFC 3339 creation timestamp."),
    SituationFieldSchema::new("updatedAt", "string", true, "RFC 3339 update timestamp."),
];

const TASK_SIGNATURE_FIELDS: &[SituationFieldSchema] = &[
    SituationFieldSchema::new("schema", "string", true, "Schema identifier."),
    SituationFieldSchema::new(
        "signatureId",
        "string",
        true,
        "Stable task signature identifier.",
    ),
    SituationFieldSchema::new(
        "normalizedTextHash",
        "string",
        true,
        "Hash of normalized task text.",
    ),
    SituationFieldSchema::new(
        "repositoryFingerprint",
        "string|null",
        false,
        "Optional repository fingerprint used for routing.",
    ),
    SituationFieldSchema::new(
        "languageSet",
        "array<string>",
        true,
        "Sorted language identifiers detected for the task.",
    ),
    SituationFieldSchema::new(
        "commandFamilies",
        "array<string>",
        true,
        "Sorted command families mentioned or inferred.",
    ),
    SituationFieldSchema::new(
        "pathGlobs",
        "array<string>",
        true,
        "Sorted path globs relevant to the task.",
    ),
    SituationFieldSchema::new(
        "riskTags",
        "array<string>",
        true,
        "Sorted risk tags derived from task and repository context.",
    ),
    SituationFieldSchema::new(
        "featureCount",
        "integer",
        true,
        "Number of feature evidence records supporting this signature.",
    ),
    SituationFieldSchema::new("createdAt", "string", true, "RFC 3339 creation timestamp."),
];

const FEATURE_EVIDENCE_FIELDS: &[SituationFieldSchema] = &[
    SituationFieldSchema::new("schema", "string", true, "Schema identifier."),
    SituationFieldSchema::new(
        "evidenceId",
        "string",
        true,
        "Stable feature evidence identifier.",
    ),
    SituationFieldSchema::new("signatureId", "string", true, "Task signature identifier."),
    SituationFieldSchema::new("featureName", "string", true, "Stable feature name."),
    SituationFieldSchema::new("featureType", "string", true, "Feature extraction type."),
    SituationFieldSchema::new("value", "string", true, "Captured feature value."),
    SituationFieldSchema::new(
        "weight",
        "number",
        true,
        "Bounded feature weight from 0.0 to 1.0.",
    ),
    SituationFieldSchema::new(
        "source",
        "string",
        true,
        "Feature source, such as text, repository, command history, or import.",
    ),
    SituationFieldSchema::new(
        "observedAt",
        "string",
        true,
        "RFC 3339 observation timestamp.",
    ),
];

const ROUTING_DECISION_FIELDS: &[SituationFieldSchema] = &[
    SituationFieldSchema::new("schema", "string", true, "Schema identifier."),
    SituationFieldSchema::new(
        "routingId",
        "string",
        true,
        "Stable routing decision identifier.",
    ),
    SituationFieldSchema::new(
        "situationId",
        "string",
        true,
        "Routed situation identifier.",
    ),
    SituationFieldSchema::new("surface", "string", true, "Routing surface being selected."),
    SituationFieldSchema::new("confidence", "string", true, "Routing confidence band."),
    SituationFieldSchema::new(
        "confidenceScore",
        "number",
        true,
        "Bounded routing confidence score from 0.0 to 1.0.",
    ),
    SituationFieldSchema::new(
        "selectedProfile",
        "string|null",
        false,
        "Selected profile for the target surface.",
    ),
    SituationFieldSchema::new(
        "retrievalProfile",
        "string|null",
        false,
        "Context retrieval profile selected for this situation.",
    ),
    SituationFieldSchema::new(
        "preflightProfile",
        "string|null",
        false,
        "Preflight profile selected for this situation.",
    ),
    SituationFieldSchema::new(
        "procedureCandidateIds",
        "array<string>",
        true,
        "Candidate procedure identifiers selected by routing.",
    ),
    SituationFieldSchema::new(
        "fixtureIds",
        "array<string>",
        true,
        "Fixture identifiers selected by routing.",
    ),
    SituationFieldSchema::new(
        "tripwireCandidateIds",
        "array<string>",
        true,
        "Candidate tripwire identifiers selected by routing.",
    ),
    SituationFieldSchema::new(
        "replayPolicy",
        "string",
        true,
        "Counterfactual replay eligibility policy.",
    ),
    SituationFieldSchema::new(
        "reasons",
        "array<string>",
        true,
        "Stable reasons explaining the routing decision.",
    ),
    SituationFieldSchema::new("createdAt", "string", true, "RFC 3339 creation timestamp."),
];

const SITUATION_LINK_FIELDS: &[SituationFieldSchema] = &[
    SituationFieldSchema::new("schema", "string", true, "Schema identifier."),
    SituationFieldSchema::new(
        "linkId",
        "string",
        true,
        "Stable situation-link identifier.",
    ),
    SituationFieldSchema::new(
        "sourceSituationId",
        "string",
        true,
        "Source situation identifier.",
    ),
    SituationFieldSchema::new(
        "targetSituationId",
        "string",
        true,
        "Target situation identifier.",
    ),
    SituationFieldSchema::new(
        "relation",
        "string",
        true,
        "Stable situation-link relation.",
    ),
    SituationFieldSchema::new(
        "directed",
        "boolean",
        true,
        "Whether link direction is meaningful.",
    ),
    SituationFieldSchema::new("confidence", "string", true, "Link confidence band."),
    SituationFieldSchema::new(
        "confidenceScore",
        "number",
        true,
        "Bounded link confidence score from 0.0 to 1.0.",
    ),
    SituationFieldSchema::new(
        "evidenceIds",
        "array<string>",
        true,
        "Evidence identifiers supporting the link.",
    ),
    SituationFieldSchema::new("createdAt", "string", true, "RFC 3339 creation timestamp."),
];

#[must_use]
pub const fn situation_schemas() -> [SituationObjectSchema; 5] {
    [
        SituationObjectSchema {
            schema_name: SITUATION_SCHEMA_V1,
            schema_uri: "urn:ee:schema:situation:v1",
            kind: "situation",
            title: "Situation",
            description: "Normalized task situation record used for routing and retrieval.",
            fields: SITUATION_FIELDS,
        },
        SituationObjectSchema {
            schema_name: TASK_SIGNATURE_SCHEMA_V1,
            schema_uri: "urn:ee:schema:task-signature:v1",
            kind: "task_signature",
            title: "TaskSignature",
            description: "Deterministic signature extracted from task text and repository context.",
            fields: TASK_SIGNATURE_FIELDS,
        },
        SituationObjectSchema {
            schema_name: FEATURE_EVIDENCE_SCHEMA_V1,
            schema_uri: "urn:ee:schema:situation-feature-evidence:v1",
            kind: "feature_evidence",
            title: "FeatureEvidence",
            description: "Evidence feature contributing to a task signature or route.",
            fields: FEATURE_EVIDENCE_FIELDS,
        },
        SituationObjectSchema {
            schema_name: ROUTING_DECISION_SCHEMA_V1,
            schema_uri: "urn:ee:schema:situation-routing-decision:v1",
            kind: "routing_decision",
            title: "RoutingDecision",
            description: "Explainable routing decision derived from a situation.",
            fields: ROUTING_DECISION_FIELDS,
        },
        SituationObjectSchema {
            schema_name: SITUATION_LINK_SCHEMA_V1,
            schema_uri: "urn:ee:schema:situation-link:v1",
            kind: "situation_link",
            title: "SituationLink",
            description: "Explainable relation between two situations.",
            fields: SITUATION_LINK_FIELDS,
        },
    ]
}

#[must_use]
pub fn situation_schema_catalog_json() -> String {
    let schemas = situation_schemas();
    let mut output = String::from("{\n");
    output.push_str(&format!(
        "  \"schema\": \"{SITUATION_SCHEMA_CATALOG_V1}\",\n"
    ));
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    const SITUATION_SCHEMA_GOLDEN: &str =
        include_str!("../../tests/fixtures/golden/models/situation_schemas.json.golden");

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn schema_constants_are_stable() -> TestResult {
        ensure(
            SITUATION_CLASSIFY_SCHEMA_V1,
            "ee.situation.classify.v1",
            "classify",
        )?;
        ensure(SITUATION_SHOW_SCHEMA_V1, "ee.situation.show.v1", "show")?;
        ensure(
            SITUATION_EXPLAIN_SCHEMA_V1,
            "ee.situation.explain.v1",
            "explain",
        )?;
        ensure(SITUATION_SCHEMA_V1, "ee.situation.v1", "situation")?;
        ensure(
            TASK_SIGNATURE_SCHEMA_V1,
            "ee.task_signature.v1",
            "task signature",
        )?;
        ensure(
            FEATURE_EVIDENCE_SCHEMA_V1,
            "ee.situation.feature_evidence.v1",
            "feature evidence",
        )?;
        ensure(
            ROUTING_DECISION_SCHEMA_V1,
            "ee.situation.routing_decision.v1",
            "routing decision",
        )?;
        ensure(
            SITUATION_LINK_SCHEMA_V1,
            "ee.situation.link.v1",
            "situation link",
        )?;
        ensure(
            SITUATION_SCHEMA_CATALOG_V1,
            "ee.situation.schemas.v1",
            "catalog",
        )
    }

    #[test]
    fn stable_wire_enums_round_trip() -> TestResult {
        for category in SituationCategory::ALL {
            ensure(
                SituationCategory::from_str(category.as_str()),
                Ok(*category),
                "category",
            )?;
        }
        for confidence in SituationConfidence::all() {
            ensure(
                SituationConfidence::from_str(confidence.as_str()),
                Ok(confidence),
                "confidence",
            )?;
        }
        for feature_type in SituationFeatureType::all() {
            ensure(
                SituationFeatureType::from_str(feature_type.as_str()),
                Ok(feature_type),
                "feature type",
            )?;
        }
        for surface in SituationRoutingSurface::all() {
            ensure(
                SituationRoutingSurface::from_str(surface.as_str()),
                Ok(surface),
                "routing surface",
            )?;
        }
        for replay_policy in SituationReplayPolicy::all() {
            ensure(
                SituationReplayPolicy::from_str(replay_policy.as_str()),
                Ok(replay_policy),
                "replay policy",
            )?;
        }
        for relation in SituationLinkRelation::all() {
            ensure(
                SituationLinkRelation::from_str(relation.as_str()),
                Ok(relation),
                "link relation",
            )?;
        }
        ensure(
            SituationCategory::from_str("mystery").map_err(|error| error.field()),
            Err("situation_category"),
            "invalid category field",
        )
    }

    #[test]
    fn builders_set_schemas_and_safe_defaults() -> TestResult {
        let situation = Situation::new(
            "sit-001",
            SituationCategory::BugFix,
            "sig-001",
            "Fix failing release",
            "Release workflow is failing after packaging changes.",
            "blake3:abc123",
            "2026-05-01T08:00:00Z",
        )
        .with_confidence(SituationConfidence::High, 2.0)
        .with_evidence("feat-001")
        .with_link_count(1);
        ensure(situation.schema, SITUATION_SCHEMA_V1, "situation schema")?;
        ensure(situation.confidence_score, 1.0, "bounded confidence")?;
        ensure(
            situation.updated_at,
            situation.created_at,
            "updated default",
        )?;

        let signature = TaskSignature::new("sig-001", "blake3:def456", "2026-05-01T08:00:00Z")
            .repository_fingerprint("repo:rust-cli")
            .with_language("rust")
            .with_command_family("cargo")
            .with_path_glob("src/**")
            .with_risk_tag("release")
            .with_feature_count(1);
        ensure(
            signature.schema,
            TASK_SIGNATURE_SCHEMA_V1,
            "signature schema",
        )?;
        ensure(
            signature.repository_fingerprint.as_deref(),
            Some("repo:rust-cli"),
            "repo fingerprint",
        )?;

        let feature = FeatureEvidence::new(
            "feat-001",
            "sig-001",
            "release keyword",
            SituationFeatureType::Keyword,
            "release",
            "task_text",
            "2026-05-01T08:00:01Z",
        )
        .with_weight(f32::NAN);
        ensure(feature.schema, FEATURE_EVIDENCE_SCHEMA_V1, "feature schema")?;
        ensure(feature.weight, 0.0, "nan weight coerces to zero")?;

        let route = RoutingDecision::new(
            "route-001",
            "sit-001",
            SituationRoutingSurface::PreflightProfile,
            "2026-05-01T08:00:02Z",
        )
        .with_confidence(SituationConfidence::Medium, 0.65)
        .selected_profile("release-preflight")
        .retrieval_profile("compact")
        .preflight_profile("release")
        .with_procedure_candidate("proc-001")
        .with_fixture("fixture-001")
        .with_tripwire_candidate("tripwire-001")
        .replay_policy(SituationReplayPolicy::DryRunOnly)
        .with_reason("release keyword and cargo command family matched");
        ensure(route.schema, ROUTING_DECISION_SCHEMA_V1, "routing schema")?;
        ensure(
            route.replay_policy,
            SituationReplayPolicy::DryRunOnly,
            "replay policy",
        )?;

        let link = SituationLink::new(
            "sitlink-001",
            "sit-001",
            "sit-002",
            SituationLinkRelation::Similar,
            "2026-05-01T08:00:03Z",
        )
        .directed()
        .with_confidence(SituationConfidence::Medium, 0.72)
        .with_evidence("feat-001");
        ensure(link.schema, SITUATION_LINK_SCHEMA_V1, "link schema")?;
        ensure(link.directed, true, "directed")
    }

    #[test]
    fn situation_schema_catalog_order_is_stable() -> TestResult {
        let schemas = situation_schemas();
        ensure(schemas.len(), 5, "schema count")?;
        let schema_names: Vec<_> = schemas.iter().map(|schema| schema.schema_name).collect();
        ensure(
            schema_names,
            vec![
                SITUATION_SCHEMA_V1,
                TASK_SIGNATURE_SCHEMA_V1,
                FEATURE_EVIDENCE_SCHEMA_V1,
                ROUTING_DECISION_SCHEMA_V1,
                SITUATION_LINK_SCHEMA_V1,
            ],
            "schema order",
        )
    }

    #[test]
    fn situation_schema_catalog_matches_golden_fixture() {
        assert_eq!(situation_schema_catalog_json(), SITUATION_SCHEMA_GOLDEN);
    }

    #[test]
    fn situation_schema_catalog_is_valid_json() -> TestResult {
        let parsed: serde_json::Value = serde_json::from_str(SITUATION_SCHEMA_GOLDEN)
            .map_err(|error| format!("situation schema golden must be valid JSON: {error}"))?;
        ensure(
            parsed.get("schema").and_then(serde_json::Value::as_str),
            Some(SITUATION_SCHEMA_CATALOG_V1),
            "catalog schema",
        )?;
        let schemas = parsed
            .get("schemas")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "schemas must be an array".to_string())?;
        ensure(schemas.len(), 5, "catalog length")
    }
}
