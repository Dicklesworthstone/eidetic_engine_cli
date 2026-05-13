//! Canonical memory-level lifecycle transitions (G9 / bd-17c65.7.8).
//!
//! The table in this module is the product contract for durable memory level
//! changes. Storage surfaces may keep legacy audit rows for compatibility, but
//! every promotion, demotion, or tombstone transition must also be explainable
//! through this table and a `memory.level_transition` audit row.

use serde_json::json;

/// Stable audit action for memory level lifecycle changes.
pub const MEMORY_LEVEL_TRANSITION_ACTION: &str = "memory.level_transition";

/// Stable details schema for `memory.level_transition` audit rows.
pub const MEMORY_LEVEL_TRANSITION_AUDIT_SCHEMA_V1: &str = "ee.audit.memory_level_transition.v1";

/// Failure-mode fixture code: a transition was rejected because the memory is
/// already tombstoned.
pub const LEVEL_TRANSITION_TOMBSTONED_REJECTED_CODE: &str = "level_transition_tombstoned_rejected";

/// Failure-mode fixture code: a transition requires durable evidence refs.
pub const LEVEL_TRANSITION_REQUIRES_EVIDENCE_CODE: &str = "level_transition_requires_evidence";

/// Failure-mode fixture code: a concurrent update invalidated a planned
/// transition.
pub const LEVEL_TRANSITION_CONCURRENT_CONFLICT_CODE: &str = "level_transition_concurrent_conflict";

/// Memory lifecycle states used by the transition table.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MemoryLifecycleState {
    Working,
    Episodic,
    Semantic,
    Procedural,
    Tombstoned,
}

impl MemoryLifecycleState {
    /// Stable wire form for audit details and tests.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Episodic => "episodic",
            Self::Semantic => "semantic",
            Self::Procedural => "procedural",
            Self::Tombstoned => "tombstoned",
        }
    }

    /// All lifecycle states in deterministic order.
    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Working,
            Self::Episodic,
            Self::Semantic,
            Self::Procedural,
            Self::Tombstoned,
        ]
    }
}

/// One allowed lifecycle transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryLifecycleTransition {
    pub from: MemoryLifecycleState,
    pub event: &'static str,
    pub to: MemoryLifecycleState,
    pub automatic: bool,
    pub reason: &'static str,
    pub evidence: &'static [&'static str],
}

const WORKFLOW_ID_EVIDENCE: &[&str] = &["workflow_id"];
const MANUAL_EVIDENCE: &[&str] = &["actor", "reason"];
const EPISODIC_CLUSTER_EVIDENCE: &[&str] = &["episodic_memory_ids"];
const CURATION_EVIDENCE: &[&str] = &["curation_candidate_id", "evidence_refs"];
const FEEDBACK_EVIDENCE: &[&str] = &["feedback_event_ids"];
const DECAY_EVIDENCE: &[&str] = &["decay_evaluation"];
const VALID_TO_EVIDENCE: &[&str] = &["valid_to"];

/// Canonical G9 transition table.
pub const TRANSITIONS: &[MemoryLifecycleTransition] = &[
    MemoryLifecycleTransition {
        from: MemoryLifecycleState::Working,
        event: "workflow.completed",
        to: MemoryLifecycleState::Episodic,
        automatic: true,
        reason: "workflow_close",
        evidence: WORKFLOW_ID_EVIDENCE,
    },
    MemoryLifecycleTransition {
        from: MemoryLifecycleState::Working,
        event: "manual.promote_to_episodic",
        to: MemoryLifecycleState::Episodic,
        automatic: false,
        reason: "manual_promotion",
        evidence: MANUAL_EVIDENCE,
    },
    MemoryLifecycleTransition {
        from: MemoryLifecycleState::Episodic,
        event: "repeated_observation",
        to: MemoryLifecycleState::Semantic,
        automatic: true,
        reason: "clustered_repeated_observation",
        evidence: EPISODIC_CLUSTER_EVIDENCE,
    },
    MemoryLifecycleTransition {
        from: MemoryLifecycleState::Episodic,
        event: "manual.promote_to_semantic",
        to: MemoryLifecycleState::Semantic,
        automatic: false,
        reason: "manual_promotion",
        evidence: MANUAL_EVIDENCE,
    },
    MemoryLifecycleTransition {
        from: MemoryLifecycleState::Semantic,
        event: "curate.apply",
        to: MemoryLifecycleState::Procedural,
        automatic: true,
        reason: "procedural_rule_proposal",
        evidence: CURATION_EVIDENCE,
    },
    MemoryLifecycleTransition {
        from: MemoryLifecycleState::Semantic,
        event: "manual.promote_to_procedural",
        to: MemoryLifecycleState::Procedural,
        automatic: false,
        reason: "manual_promotion",
        evidence: MANUAL_EVIDENCE,
    },
    MemoryLifecycleTransition {
        from: MemoryLifecycleState::Procedural,
        event: "feedback.harmful_decay",
        to: MemoryLifecycleState::Semantic,
        automatic: true,
        reason: "harmful_feedback_decay",
        evidence: FEEDBACK_EVIDENCE,
    },
    MemoryLifecycleTransition {
        from: MemoryLifecycleState::Procedural,
        event: "manual.demote_to_semantic",
        to: MemoryLifecycleState::Semantic,
        automatic: false,
        reason: "manual_demotion",
        evidence: MANUAL_EVIDENCE,
    },
    MemoryLifecycleTransition {
        from: MemoryLifecycleState::Semantic,
        event: "valid_to.set",
        to: MemoryLifecycleState::Episodic,
        automatic: true,
        reason: "time_bound_fact",
        evidence: VALID_TO_EVIDENCE,
    },
    MemoryLifecycleTransition {
        from: MemoryLifecycleState::Working,
        event: "decay.l3",
        to: MemoryLifecycleState::Tombstoned,
        automatic: true,
        reason: "auto_forgetting",
        evidence: DECAY_EVIDENCE,
    },
    MemoryLifecycleTransition {
        from: MemoryLifecycleState::Episodic,
        event: "decay.l3",
        to: MemoryLifecycleState::Tombstoned,
        automatic: true,
        reason: "auto_forgetting",
        evidence: DECAY_EVIDENCE,
    },
    MemoryLifecycleTransition {
        from: MemoryLifecycleState::Semantic,
        event: "decay.l3",
        to: MemoryLifecycleState::Tombstoned,
        automatic: true,
        reason: "auto_forgetting",
        evidence: DECAY_EVIDENCE,
    },
    MemoryLifecycleTransition {
        from: MemoryLifecycleState::Procedural,
        event: "decay.l3",
        to: MemoryLifecycleState::Tombstoned,
        automatic: true,
        reason: "auto_forgetting",
        evidence: DECAY_EVIDENCE,
    },
    MemoryLifecycleTransition {
        from: MemoryLifecycleState::Working,
        event: "manual.tombstone",
        to: MemoryLifecycleState::Tombstoned,
        automatic: false,
        reason: "manual_tombstone",
        evidence: MANUAL_EVIDENCE,
    },
    MemoryLifecycleTransition {
        from: MemoryLifecycleState::Episodic,
        event: "manual.tombstone",
        to: MemoryLifecycleState::Tombstoned,
        automatic: false,
        reason: "manual_tombstone",
        evidence: MANUAL_EVIDENCE,
    },
    MemoryLifecycleTransition {
        from: MemoryLifecycleState::Semantic,
        event: "manual.tombstone",
        to: MemoryLifecycleState::Tombstoned,
        automatic: false,
        reason: "manual_tombstone",
        evidence: MANUAL_EVIDENCE,
    },
    MemoryLifecycleTransition {
        from: MemoryLifecycleState::Procedural,
        event: "manual.tombstone",
        to: MemoryLifecycleState::Tombstoned,
        automatic: false,
        reason: "manual_tombstone",
        evidence: MANUAL_EVIDENCE,
    },
];

/// Find the canonical transition for a state/event pair.
#[must_use]
pub fn transition_for(
    from: MemoryLifecycleState,
    event: &str,
) -> Option<&'static MemoryLifecycleTransition> {
    TRANSITIONS
        .iter()
        .find(|transition| transition.from == from && transition.event == event)
}

/// Structured input for stable transition audit details.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryLevelTransitionAudit<'a> {
    pub memory_id: &'a str,
    pub previous_level: &'a str,
    pub new_level: &'a str,
    pub reason: &'a str,
    pub automatic: bool,
    pub event: &'a str,
    pub evidence_refs: &'a [&'a str],
    pub source_action: Option<&'a str>,
}

/// Build stable JSON details for a `memory.level_transition` audit row.
#[must_use]
pub fn level_transition_audit_details(input: &MemoryLevelTransitionAudit<'_>) -> String {
    let payload = json!({
        "schema": MEMORY_LEVEL_TRANSITION_AUDIT_SCHEMA_V1,
        "memoryId": input.memory_id,
        "previousLevel": input.previous_level,
        "newLevel": input.new_level,
        "reason": input.reason,
        "automatic": input.automatic,
        "event": input.event,
        "evidenceRefs": input.evidence_refs,
        "sourceAction": input.source_action,
    });
    let details_hash = format!(
        "blake3:{}",
        blake3::hash(payload.to_string().as_bytes()).to_hex()
    );
    let mut payload_with_hash = payload;
    payload_with_hash["detailsHash"] = json!(details_hash);
    payload_with_hash.to_string()
}
