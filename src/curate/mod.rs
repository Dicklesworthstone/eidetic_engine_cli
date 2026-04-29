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

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{
        CandidateSource, CandidateStatus, CandidateType, ParseCandidateSourceError,
        ParseCandidateStatusError, ParseCandidateTypeError, subsystem_name,
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
}
