//! Canonical memory-level lifecycle transitions (G9 / bd-17c65.7.8).
//!
//! The table in this module is the product contract for durable memory level
//! changes. Storage surfaces may keep legacy audit rows for compatibility, but
//! every promotion, demotion, or tombstone transition must also be explainable
//! through this table and a `memory.level_transition` audit row.
//! Live seal admission is a separate lifecycle question from backup validity:
//! a closed seal withholds a body even when that body's storage is inconsistent.

use serde_json::json;

/// Resolve current memory review holds in the caller's source snapshot.
///
/// Review status is independent of source validity, historical clocks and
/// indexed trust. Read only identities in bounded, deduplicated batches: a
/// rule-target hold never aliases a source memory, and private feedback text
/// is neither needed nor returned. Empty candidate sets need no authority IO.
/// This helper never begins, commits or releases the caller's transaction.
pub(crate) fn pending_memory_review_ids(
    connection: &crate::db::DbConnection,
    workspace_id: &str,
    candidate_ids: &[&str],
) -> crate::db::Result<std::collections::BTreeSet<String>> {
    use std::collections::BTreeSet;

    use sqlmodel_core::Value;

    let candidates = candidate_ids.iter().copied().collect::<BTreeSet<_>>();
    let ids = candidates.into_iter().collect::<Vec<_>>();
    let mut held = BTreeSet::new();
    for page in ids.chunks(256) {
        let slots = (2..page.len() + 2)
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT DISTINCT target_id FROM feedback_quarantine WHERE workspace_id = ?1 AND target_type = 'memory' AND status = 'pending' AND target_id IN ({slots}) ORDER BY target_id ASC"
        );
        let mut values = vec![Value::Text(workspace_id.to_owned())];
        values.extend(page.iter().map(|id| Value::Text((*id).to_owned())));
        for row in connection
            .query(&sql, &values)
            .map_err(|_| review_admission_error())?
        {
            let id = row
                .get(0)
                .and_then(Value::as_str)
                .ok_or_else(review_admission_error)?;
            if page.binary_search(&id).is_err() {
                return Err(review_admission_error());
            }
            held.insert(id.to_owned());
        }
    }
    Ok(held)
}

fn review_admission_error() -> crate::db::DbError {
    crate::db::DbError::MalformedRow {
        operation: crate::db::DbOperation::Query,
        message: "Could not verify pending memory review authority".to_owned(),
    }
}

/// Read seal authority for a live corpus without reading or exporting bodies.
///
/// The backup reader also checks that a closed seal has placeholder content.
/// That stricter export contract must not turn a single damaged sealed row into
/// an outage for unrelated public memories. This reader still validates every
/// seal with the same model validator as storage and attestation. Invalid
/// metadata fails the read; inconsistent body bytes never grant visibility.
///
/// One bound query loads the workspace's seals, including retired history. The
/// caller owns the surrounding body/authority snapshot; this function neither
/// opens a store nor starts, commits, or releases any transaction.
pub(crate) fn load_memory_seals_for_admission(
    connection: &crate::db::DbConnection,
    workspace_id: &str,
) -> crate::db::Result<Vec<crate::models::MemorySeal>> {
    use sqlmodel_core::Value;

    connection
        .query(
            "SELECT s.memory_id, s.content_commitment, s.sealed_at, s.revealed_at, s.reveal_verified FROM memory_seals s JOIN memories m ON m.id = s.memory_id WHERE m.workspace_id = ?1 ORDER BY s.memory_id",
            &[Value::Text(workspace_id.to_owned())],
        )
        .map_err(|_| seal_admission_error())?
        .iter()
        .map(decode_admission_seal)
        .collect()
}

fn seal_admission_error() -> crate::db::DbError {
    crate::db::DbError::MalformedRow {
        operation: crate::db::DbOperation::Query,
        message: "Could not verify live memory seal authority".to_owned(),
    }
}

fn decode_admission_seal(row: &sqlmodel_core::Row) -> crate::db::Result<crate::models::MemorySeal> {
    use sqlmodel_core::Value;

    let text = |index| {
        row.get(index)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(seal_admission_error)
    };
    let revealed_at = match row.get(3) {
        Some(Value::Null) => None,
        Some(Value::Text(value)) => Some(value.clone()),
        _ => return Err(seal_admission_error()),
    };
    let reveal_verified = match row.get(4) {
        Some(Value::Null) => None,
        Some(value) => match value.as_i64() {
            Some(0) => Some(false),
            Some(1) => Some(true),
            _ => return Err(seal_admission_error()),
        },
        None => return Err(seal_admission_error()),
    };
    let seal = crate::models::MemorySeal {
        memory_id: text(0)?,
        content_commitment: text(1)?,
        sealed_at: text(2)?,
        revealed_at,
        reveal_verified,
    };
    crate::models::validate_attestation_seal_fields(
        &seal.content_commitment,
        &seal.sealed_at,
        seal.revealed_at.as_deref(),
        seal.reveal_verified,
    )
    .map_err(|_| seal_admission_error())?;
    Ok(seal)
}

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
    pub previous_trust_class: Option<&'a str>,
    pub new_trust_class: Option<&'a str>,
}

/// Build stable JSON details for a `memory.level_transition` audit row.
#[must_use]
pub fn level_transition_audit_details(input: &MemoryLevelTransitionAudit<'_>) -> String {
    let mut payload = json!({
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
    if let (Some(previous), Some(new)) = (input.previous_trust_class, input.new_trust_class) {
        payload["previousTrustClass"] = json!(previous);
        payload["newTrustClass"] = json!(new);
    }
    let details_hash = format!(
        "blake3:{}",
        blake3::hash(payload.to_string().as_bytes()).to_hex()
    );
    let mut payload_with_hash = payload;
    payload_with_hash["detailsHash"] = json!(details_hash);
    payload_with_hash.to_string()
}

#[cfg(test)]
mod seal_admission_tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};
    use sqlmodel_core::{Row, Value};

    const WORKSPACE: &str = "wsp_00000000000000000000000091";
    const OTHER: &str = "wsp_00000000000000000000000092";
    const MEMORY: &str = "mem_00000000000000000000000091";
    const TIME: &str = "2026-09-17T12:00:00Z";

    fn row(revealed: Value, verified: Value) -> Row {
        Row::new(
            [
                "memory_id",
                "content_commitment",
                "sealed_at",
                "revealed_at",
                "reveal_verified",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            vec![
                Value::Text(MEMORY.to_owned()),
                Value::Text(format!("blake3:{}", "a".repeat(64))),
                Value::Text(TIME.to_owned()),
                revealed,
                verified,
            ],
        )
    }

    fn seed(db: &DbConnection, workspace: &str, id: &str) {
        db.insert_memory(
            id,
            &CreateMemoryInput {
                workspace_id: workspace.to_owned(),
                level: "semantic".to_owned(),
                kind: "note".to_owned(),
                content: "PRIVATE-BODY must never enter a seal query".to_owned(),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: None,
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: Some(TIME.to_owned()),
                valid_to: None,
            },
        )
        .unwrap();
        db.insert_memory_seal(id, &format!("blake3:{}", "a".repeat(64)), TIME)
            .unwrap();
    }

    fn fixture() -> (tempfile::TempDir, DbConnection) {
        let root = tempfile::tempdir().unwrap();
        let db = DbConnection::open_file(&root.path().join("seal.db")).unwrap();
        db.migrate().unwrap();
        for (id, path) in [
            (WORKSPACE, root.path().join("workspace")),
            (OTHER, root.path().join("other")),
        ] {
            db.insert_workspace(
                id,
                &CreateWorkspaceInput {
                    path: path.to_string_lossy().into_owned(),
                    name: None,
                },
            )
            .unwrap();
        }
        (root, db)
    }

    #[test]
    fn seal_decoder_uses_the_shared_chronological_reveal_contract() {
        assert!(
            decode_admission_seal(&row(Value::Null, Value::Null))
                .unwrap()
                .is_sealed()
        );
        let revealed = decode_admission_seal(&row(
            Value::Text("2026-09-17T08:00:00-04:00".to_owned()),
            Value::BigInt(1),
        ))
        .unwrap();
        assert!(!revealed.is_sealed());
        assert_eq!(revealed.reveal_verified, Some(true));
        for (at, flag) in [
            (
                Value::Text("2026-09-17T11:59:59Z".to_owned()),
                Value::BigInt(1),
            ),
            (Value::Text(TIME.to_owned()), Value::BigInt(0)),
            (Value::Text(TIME.to_owned()), Value::Null),
            (Value::Null, Value::BigInt(1)),
        ] {
            assert!(decode_admission_seal(&row(at, flag)).is_err());
        }
    }

    #[test]
    fn malformed_reveal_types_and_values_never_become_an_unsealed_row() {
        for (at, flag) in [
            (Value::Text("PRIVATE-REVEAL".to_owned()), Value::BigInt(1)),
            (Value::BigInt(1), Value::BigInt(1)),
            (Value::Text(TIME.to_owned()), Value::Text("1".to_owned())),
            (Value::Text(TIME.to_owned()), Value::BigInt(-1)),
            (Value::Text(TIME.to_owned()), Value::BigInt(2)),
        ] {
            let error = decode_admission_seal(&row(at, flag)).unwrap_err();
            assert!(!format!("{error:?}").contains("PRIVATE-REVEAL"));
            assert!(!format!("{error:?}").contains(MEMORY));
        }
        assert!(decode_admission_seal(&Row::new(Vec::new(), Vec::new())).is_err());
    }

    #[test]
    fn live_reader_preserves_closed_body_exclusion_and_strict_backup_validation() {
        let (_root, db) = fixture();
        seed(&db, WORKSPACE, MEMORY);
        let before = db.get_memory(MEMORY).unwrap();
        let audits = db.count_table_rows("audit_log").unwrap();
        let seals = load_memory_seals_for_admission(&db, WORKSPACE).unwrap();
        assert_eq!(seals, vec![db.get_memory_seal(MEMORY).unwrap().unwrap()]);
        assert!(seals[0].is_sealed());
        assert!(db.list_memory_seals_for_recovery(WORKSPACE).is_err());
        assert_eq!(db.get_memory(MEMORY).unwrap(), before);
        assert_eq!(db.count_table_rows("audit_log").unwrap(), audits);
        assert!(db.mark_memory_seal_revealed(MEMORY, TIME).unwrap());
        assert_eq!(
            load_memory_seals_for_admission(&db, WORKSPACE).unwrap(),
            db.list_memory_seals_for_recovery(WORKSPACE).unwrap()
        );
    }

    #[test]
    fn foreign_seal_metadata_never_poison_or_widen_the_addressed_workspace() {
        let (_root, db) = fixture();
        seed(&db, OTHER, MEMORY);
        db.execute_raw("UPDATE memory_seals SET sealed_at = 'PRIVATE-FOREIGN-TIME'")
            .unwrap();
        assert!(
            load_memory_seals_for_admission(&db, WORKSPACE)
                .unwrap()
                .is_empty()
        );
        assert!(
            load_memory_seals_for_admission(&db, "' OR 1 = 1 --")
                .unwrap()
                .is_empty()
        );
        let error = load_memory_seals_for_admission(&db, OTHER).unwrap_err();
        assert!(!format!("{error:?}").contains("PRIVATE-FOREIGN-TIME"));
    }

    #[test]
    fn admission_borrows_the_readers_snapshot_without_releasing_it() {
        let (root, writer) = fixture();
        seed(&writer, WORKSPACE, MEMORY);
        let reader = DbConnection::open_file_read_only(&root.path().join("seal.db")).unwrap();
        reader.begin_read_snapshot().unwrap();
        assert!(load_memory_seals_for_admission(&reader, WORKSPACE).unwrap()[0].is_sealed());
        assert!(writer.mark_memory_seal_revealed(MEMORY, TIME).unwrap());
        assert!(load_memory_seals_for_admission(&reader, WORKSPACE).unwrap()[0].is_sealed());
        reader
            .commit_read_snapshot()
            .expect("reader still owns its transaction");
        assert!(!load_memory_seals_for_admission(&reader, WORKSPACE).unwrap()[0].is_sealed());
    }

    #[test]
    fn missing_seal_storage_is_an_error_not_an_authoritatively_empty_set() {
        let db = DbConnection::open_memory().unwrap();
        let error = load_memory_seals_for_admission(&db, WORKSPACE).unwrap_err();
        assert!(matches!(error, crate::db::DbError::MalformedRow { .. }));
        assert!(!format!("{error:?}").contains("SELECT"));
        assert!(!format!("{error:?}").contains(WORKSPACE));
    }
}

#[cfg(test)]
mod review_admission_tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::pending_memory_review_ids;
    use crate::db::{CreateFeedbackQuarantineInput, CreateWorkspaceInput, DbConnection};
    use std::collections::BTreeSet;

    const WORKSPACE: &str = "wsp_00000000000000000000000901";
    const OTHER: &str = "wsp_00000000000000000000000902";

    fn fixture() -> DbConnection {
        let db = DbConnection::open_memory().unwrap();
        db.migrate().unwrap();
        for workspace in [WORKSPACE, OTHER] {
            db.insert_workspace(
                workspace,
                &CreateWorkspaceInput {
                    path: format!("/review-admission/{workspace}"),
                    name: None,
                },
            )
            .unwrap();
        }
        db
    }

    fn hold(db: &DbConnection, number: u32, workspace: &str, kind: &str, target: &str) {
        db.insert_feedback_quarantine(
            &format!("fq_{number:026}"),
            &CreateFeedbackQuarantineInput {
                workspace_id: workspace.to_owned(),
                source_id: "PRIVATE-REVIEW-SOURCE".to_owned(),
                target_type: kind.to_owned(),
                target_id: target.to_owned(),
                signal: "harmful".to_owned(),
                weight: 1.0,
                source_type: "outcome_observed".to_owned(),
                proposed_event_id: None,
                recorded_at: "2026-01-01T00:00:00Z".to_owned(),
                reason: "PRIVATE-REVIEW-REASON".to_owned(),
                event_reason: None,
                evidence_json: None,
                session_id: None,
                raw_event_hash: format!("blake3:{}", "a".repeat(64)),
            },
        )
        .unwrap();
    }

    #[test]
    fn holds_bind_exact_native_ownership_and_only_pending_events() {
        let db = fixture();
        let memory = "mem_00000000000000000000000901";
        hold(&db, 1, OTHER, "memory", memory);
        hold(&db, 2, WORKSPACE, "rule", memory);
        assert!(
            pending_memory_review_ids(&db, WORKSPACE, &[memory])
                .unwrap()
                .is_empty()
        );
        hold(&db, 3, WORKSPACE, "memory", memory);
        hold(&db, 4, WORKSPACE, "memory", memory);
        assert_eq!(
            pending_memory_review_ids(&db, WORKSPACE, &[memory, memory]).unwrap(),
            BTreeSet::from([memory.to_owned()])
        );
        assert!(
            db.update_feedback_quarantine_status(
                &format!("fq_{:026}", 3),
                "released",
                Some("operator"),
                None,
            )
            .unwrap()
        );
        assert!(
            !pending_memory_review_ids(&db, WORKSPACE, &[memory])
                .unwrap()
                .is_empty()
        );
        assert!(
            db.update_feedback_quarantine_status(
                &format!("fq_{:026}", 4),
                "rejected",
                Some("operator"),
                None,
            )
            .unwrap()
        );
        assert!(
            pending_memory_review_ids(&db, WORKSPACE, &[memory])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn review_lookup_pages_and_deduplicates_without_interpreting_bound_values() {
        let db = fixture();
        let ids = (1000..1513)
            .map(|id| format!("mem_{id:026}"))
            .collect::<Vec<_>>();
        let mut expected = BTreeSet::new();
        for (number, index) in [0, 255, 256, 512].into_iter().enumerate() {
            hold(&db, number as u32 + 1, WORKSPACE, "memory", &ids[index]);
            expected.insert(ids[index].clone());
        }
        let mut candidates = ids.iter().map(String::as_str).collect::<Vec<_>>();
        candidates.extend([ids[0].as_str(), "' OR 1 = 1 --"]);
        assert_eq!(
            pending_memory_review_ids(&db, WORKSPACE, &candidates).unwrap(),
            expected
        );
        assert!(
            pending_memory_review_ids(&db, "' OR 1 = 1 --", &candidates)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn empty_candidates_need_no_storage_and_nonempty_failure_is_sanitized() {
        let db = DbConnection::open_memory().unwrap();
        assert!(
            pending_memory_review_ids(&db, WORKSPACE, &[])
                .unwrap()
                .is_empty()
        );
        let error = pending_memory_review_ids(&db, WORKSPACE, &["PRIVATE-CANDIDATE"]).unwrap_err();
        let diagnostic = format!("{error:?}");
        assert!(diagnostic.contains("pending memory review authority"));
        assert!(!diagnostic.contains("PRIVATE-CANDIDATE"));
        assert!(!diagnostic.contains("feedback_quarantine"));
    }

    #[test]
    fn review_lookup_does_not_finish_or_replace_the_callers_snapshot() {
        let db = fixture();
        let memory = "mem_00000000000000000000000901";
        hold(&db, 1, WORKSPACE, "memory", memory);
        db.begin_read_snapshot().unwrap();
        assert_eq!(
            pending_memory_review_ids(&db, WORKSPACE, &[memory])
                .unwrap()
                .len(),
            1
        );
        assert!(db.begin_read_snapshot().is_err());
        db.rollback_read_snapshot().unwrap();
        db.begin_read_snapshot().unwrap();
        db.rollback_read_snapshot().unwrap();
    }
}
