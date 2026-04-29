//! Curation subsystem (EE-180, ADR-0006).
//!
//! Curation candidates are auditable proposals for memory mutations:
//! consolidation, promotion, deprecation, supersession, tombstoning, etc.
//! No silent durable mutation — every change goes through this queue.

use std::fmt;
use std::str::FromStr;

pub const SUBSYSTEM: &str = "curate";

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
    pub proposed_confidence: Option<f32>,
    pub proposed_trust_class: Option<String>,
    pub source_type: CandidateSource,
    pub source_id: Option<String>,
    pub reason: String,
    pub confidence: f32,
    pub ttl_expires_at: Option<String>,
}

/// Errors during candidate validation.
#[derive(Clone, Debug, Eq, PartialEq)]
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
        proposed_content: input
            .proposed_content
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty()),
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

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{
        CandidateInput, CandidateSource, CandidateStatus, CandidateType, CandidateValidationError,
        ParseCandidateSourceError, ParseCandidateStatusError, ParseCandidateTypeError,
        subsystem_name, validate_candidate, validate_status_transition,
    };

    #[test]
    fn subsystem_name_is_stable() {
        assert_eq!(subsystem_name(), "curate");
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
}
