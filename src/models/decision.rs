//! Decision-plane tracking metadata (EE-364).
//!
//! Provides `policy_id`, `decision_id`, and `trace_id` fields for records
//! that affect ranking, packing, curation, repair ordering, or cache admission.
//! This enables shadow-run comparisons, policy replay, and audit trails.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Schema identifier for decision plane records.
pub const DECISION_PLANE_SCHEMA_V1: &str = "ee.decision_plane.v1";

/// Decision plane types that can be tracked.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionPlane {
    /// Context pack assembly decisions.
    Packing,
    /// Search result ranking decisions.
    Ranking,
    /// Memory curation decisions (promote, archive, tombstone).
    Curation,
    /// Repair task ordering decisions.
    RepairOrder,
    /// Cache admission/eviction decisions.
    CacheAdmission,
    /// Causal trace observation decisions.
    Observe,
}

impl DecisionPlane {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Packing => "packing",
            Self::Ranking => "ranking",
            Self::Curation => "curation",
            Self::RepairOrder => "repair_order",
            Self::CacheAdmission => "cache_admission",
            Self::Observe => "observe",
        }
    }

    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Packing,
            Self::Ranking,
            Self::Curation,
            Self::RepairOrder,
            Self::CacheAdmission,
            Self::Observe,
        ]
    }
}

impl fmt::Display for DecisionPlane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseDecisionPlaneError {
    pub invalid: String,
}

impl fmt::Display for ParseDecisionPlaneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid decision plane '{}'; expected one of: packing, ranking, curation, repair_order, cache_admission, observe",
            self.invalid
        )
    }
}

impl std::error::Error for ParseDecisionPlaneError {}

impl FromStr for DecisionPlane {
    type Err = ParseDecisionPlaneError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "packing" => Ok(Self::Packing),
            "ranking" => Ok(Self::Ranking),
            "curation" => Ok(Self::Curation),
            "repair_order" => Ok(Self::RepairOrder),
            "cache_admission" => Ok(Self::CacheAdmission),
            "observe" => Ok(Self::Observe),
            _ => Err(ParseDecisionPlaneError {
                invalid: s.to_owned(),
            }),
        }
    }
}

/// Metadata for tracking decisions in the decision plane.
///
/// Records that affect ranking, packing, curation, repair ordering, or
/// cache admission should include this metadata to enable:
/// - Shadow-run comparisons between policies
/// - Deterministic replay of decisions
/// - Audit trails linking decisions to policies
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DecisionPlaneMetadata {
    /// The policy that governed this decision (e.g., "default", "aggressive-decay").
    /// Optional: None means the default/incumbent policy was used.
    pub policy_id: Option<String>,

    /// Unique identifier for this specific decision instance.
    /// Optional: can be generated lazily when auditing is needed.
    pub decision_id: Option<String>,

    /// Trace identifier linking related decisions across operations.
    /// Used for distributed tracing and request correlation.
    pub trace_id: Option<String>,
}

impl DecisionPlaneMetadata {
    /// Create empty metadata (all fields None).
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            policy_id: None,
            decision_id: None,
            trace_id: None,
        }
    }

    /// Create metadata with a policy ID only.
    #[must_use]
    pub fn with_policy(policy_id: impl Into<String>) -> Self {
        Self {
            policy_id: Some(policy_id.into()),
            decision_id: None,
            trace_id: None,
        }
    }

    /// Create metadata with all fields.
    #[must_use]
    pub fn full(
        policy_id: impl Into<String>,
        decision_id: impl Into<String>,
        trace_id: impl Into<String>,
    ) -> Self {
        Self {
            policy_id: Some(policy_id.into()),
            decision_id: Some(decision_id.into()),
            trace_id: Some(trace_id.into()),
        }
    }

    /// Check if this has any tracking information.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.policy_id.is_none() && self.decision_id.is_none() && self.trace_id.is_none()
    }

    /// Check if this has a policy assigned.
    #[must_use]
    pub const fn has_policy(&self) -> bool {
        self.policy_id.is_some()
    }

    /// Check if this has full audit information.
    #[must_use]
    pub const fn is_auditable(&self) -> bool {
        self.policy_id.is_some() && self.decision_id.is_some()
    }

    /// Builder-style: set policy ID.
    #[must_use]
    pub fn policy(mut self, policy_id: impl Into<String>) -> Self {
        self.policy_id = Some(policy_id.into());
        self
    }

    /// Builder-style: set decision ID.
    #[must_use]
    pub fn decision(mut self, decision_id: impl Into<String>) -> Self {
        self.decision_id = Some(decision_id.into());
        self
    }

    /// Builder-style: set trace ID.
    #[must_use]
    pub fn trace(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }
}

/// A decision record that tracks what decision was made and why.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionRecord {
    /// Schema identifier.
    pub schema: String,

    /// Which decision plane this belongs to.
    pub plane: DecisionPlane,

    /// Tracking metadata (policy, decision, trace IDs).
    pub metadata: DecisionPlaneMetadata,

    /// When the decision was made.
    pub decided_at: String,

    /// The outcome or action taken.
    pub outcome: String,

    /// Optional explanation or reasoning.
    pub reason: Option<String>,

    /// Confidence or score if applicable.
    pub confidence: Option<f64>,

    /// Whether this was a shadow decision (not actually applied).
    pub shadow: bool,

    /// If shadow, the incumbent decision it was compared against.
    pub incumbent_outcome: Option<String>,
}

impl DecisionRecord {
    #[must_use]
    pub fn builder() -> DecisionRecordBuilder {
        DecisionRecordBuilder::default()
    }
}

#[derive(Clone, Debug, Default)]
pub struct DecisionRecordBuilder {
    plane: Option<DecisionPlane>,
    metadata: DecisionPlaneMetadata,
    decided_at: Option<String>,
    outcome: Option<String>,
    reason: Option<String>,
    confidence: Option<f64>,
    shadow: bool,
    incumbent_outcome: Option<String>,
}

impl DecisionRecordBuilder {
    #[must_use]
    pub fn plane(mut self, plane: DecisionPlane) -> Self {
        self.plane = Some(plane);
        self
    }

    #[must_use]
    pub fn metadata(mut self, metadata: DecisionPlaneMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    #[must_use]
    pub fn policy_id(mut self, policy_id: impl Into<String>) -> Self {
        self.metadata.policy_id = Some(policy_id.into());
        self
    }

    #[must_use]
    pub fn decision_id(mut self, decision_id: impl Into<String>) -> Self {
        self.metadata.decision_id = Some(decision_id.into());
        self
    }

    #[must_use]
    pub fn trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.metadata.trace_id = Some(trace_id.into());
        self
    }

    #[must_use]
    pub fn decided_at(mut self, decided_at: impl Into<String>) -> Self {
        self.decided_at = Some(decided_at.into());
        self
    }

    #[must_use]
    pub fn outcome(mut self, outcome: impl Into<String>) -> Self {
        self.outcome = Some(outcome.into());
        self
    }

    #[must_use]
    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    #[must_use]
    pub fn confidence(mut self, confidence: f64) -> Self {
        self.confidence = Some(confidence);
        self
    }

    #[must_use]
    pub fn shadow(mut self, shadow: bool) -> Self {
        self.shadow = shadow;
        self
    }

    #[must_use]
    pub fn incumbent_outcome(mut self, incumbent_outcome: impl Into<String>) -> Self {
        self.incumbent_outcome = Some(incumbent_outcome.into());
        self
    }

    #[must_use]
    pub fn build(self) -> DecisionRecord {
        DecisionRecord {
            schema: DECISION_PLANE_SCHEMA_V1.to_owned(),
            plane: self.plane.unwrap_or(DecisionPlane::Packing),
            metadata: self.metadata,
            decided_at: self.decided_at.unwrap_or_default(),
            outcome: self.outcome.unwrap_or_default(),
            reason: self.reason,
            confidence: self.confidence,
            shadow: self.shadow,
            incumbent_outcome: self.incumbent_outcome,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn decision_plane_roundtrip() -> TestResult {
        for plane in DecisionPlane::all() {
            let s = plane.as_str();
            let parsed: DecisionPlane = s
                .parse()
                .map_err(|e: ParseDecisionPlaneError| e.to_string())?;
            ensure(parsed, *plane, &format!("roundtrip {s}"))?;
        }
        Ok(())
    }

    #[test]
    fn decision_plane_display() {
        assert_eq!(DecisionPlane::Packing.to_string(), "packing");
        assert_eq!(DecisionPlane::Ranking.to_string(), "ranking");
        assert_eq!(DecisionPlane::Curation.to_string(), "curation");
        assert_eq!(DecisionPlane::RepairOrder.to_string(), "repair_order");
        assert_eq!(DecisionPlane::CacheAdmission.to_string(), "cache_admission");
    }

    #[test]
    fn decision_plane_metadata_empty() {
        let meta = DecisionPlaneMetadata::empty();
        assert!(meta.is_empty());
        assert!(!meta.has_policy());
        assert!(!meta.is_auditable());
    }

    #[test]
    fn decision_plane_metadata_with_policy() {
        let meta = DecisionPlaneMetadata::with_policy("aggressive-decay");
        assert!(!meta.is_empty());
        assert!(meta.has_policy());
        assert!(!meta.is_auditable());
        assert_eq!(meta.policy_id, Some("aggressive-decay".to_owned()));
    }

    #[test]
    fn decision_plane_metadata_full() {
        let meta = DecisionPlaneMetadata::full("policy-1", "dec-001", "trace-abc");
        assert!(!meta.is_empty());
        assert!(meta.has_policy());
        assert!(meta.is_auditable());
        assert_eq!(meta.policy_id, Some("policy-1".to_owned()));
        assert_eq!(meta.decision_id, Some("dec-001".to_owned()));
        assert_eq!(meta.trace_id, Some("trace-abc".to_owned()));
    }

    #[test]
    fn decision_plane_metadata_builder_pattern() {
        let meta = DecisionPlaneMetadata::empty()
            .policy("my-policy")
            .decision("dec-123")
            .trace("trace-xyz");

        assert_eq!(meta.policy_id, Some("my-policy".to_owned()));
        assert_eq!(meta.decision_id, Some("dec-123".to_owned()));
        assert_eq!(meta.trace_id, Some("trace-xyz".to_owned()));
    }

    #[test]
    fn decision_record_builder() {
        let record = DecisionRecord::builder()
            .plane(DecisionPlane::Curation)
            .policy_id("curation-v2")
            .decision_id("dec-456")
            .trace_id("trace-req-1")
            .decided_at("2026-04-30T12:00:00Z")
            .outcome("archive")
            .reason("Low confidence, no recent access")
            .confidence(0.3)
            .shadow(false)
            .build();

        assert_eq!(record.schema, DECISION_PLANE_SCHEMA_V1);
        assert_eq!(record.plane, DecisionPlane::Curation);
        assert_eq!(record.metadata.policy_id, Some("curation-v2".to_owned()));
        assert_eq!(record.outcome, "archive");
        assert_eq!(record.confidence, Some(0.3));
        assert!(!record.shadow);
    }

    #[test]
    fn decision_record_shadow_comparison() {
        let record = DecisionRecord::builder()
            .plane(DecisionPlane::Ranking)
            .policy_id("experimental-ranker")
            .decided_at("2026-04-30T12:00:00Z")
            .outcome("rank-3")
            .shadow(true)
            .incumbent_outcome("rank-1")
            .build();

        assert!(record.shadow);
        assert_eq!(record.incumbent_outcome, Some("rank-1".to_owned()));
    }

    #[test]
    fn decision_record_serializes_to_json() {
        let record = DecisionRecord::builder()
            .plane(DecisionPlane::Packing)
            .policy_id("budget-tight")
            .decided_at("2026-04-30T12:00:00Z")
            .outcome("include")
            .build();

        let json = serde_json::to_string(&record).expect("serialize");
        assert!(json.contains(r#""schema":"ee.decision_plane.v1""#));
        assert!(json.contains(r#""plane":"packing""#));
        assert!(json.contains(r#""policy_id":"budget-tight""#));
    }

    #[test]
    fn decision_plane_metadata_serializes() {
        let meta = DecisionPlaneMetadata::full("pol-1", "dec-1", "trace-1");
        let json = serde_json::to_string(&meta).expect("serialize");
        assert!(json.contains(r#""policy_id":"pol-1""#));
        assert!(json.contains(r#""decision_id":"dec-1""#));
        assert!(json.contains(r#""trace_id":"trace-1""#));
    }

    #[test]
    fn parse_invalid_decision_plane_error() {
        let result: Result<DecisionPlane, _> = "invalid".parse();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("invalid decision plane"));
    }

    #[test]
    fn all_decision_planes_covered() {
        let all = DecisionPlane::all();
        assert_eq!(all.len(), 6);
        assert!(all.contains(&DecisionPlane::Packing));
        assert!(all.contains(&DecisionPlane::Ranking));
        assert!(all.contains(&DecisionPlane::Curation));
        assert!(all.contains(&DecisionPlane::RepairOrder));
        assert!(all.contains(&DecisionPlane::CacheAdmission));
        assert!(all.contains(&DecisionPlane::Observe));
    }
}
