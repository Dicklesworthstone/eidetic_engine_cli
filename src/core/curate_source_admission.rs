//! Live authority for explicit memory derivations and reflection packages.
//!
//! Historical memory versions remain valid, hash-locked evidence under ADR
//! 0043. A closed seal or pending review is different: neither a retained body
//! nor an old proposal authorizes a new disclosure or derived object. This
//! boundary does not change the lifecycle of already-created memories or rules.

use crate::db::{DbConnection, StoredMemory};
use crate::models::DomainError;

use super::{CurateValidationIssue, validation_issue};

/// Keep source bodies and their current sidecars in one view. Borrow an apply
/// or ingest transaction without committing or rolling back its owner's work.
pub(crate) fn with_snapshot<T>(
    connection: &DbConnection,
    read: impl FnOnce() -> Result<T, DomainError>,
) -> Result<T, DomainError> {
    let owned = match connection.begin_read_snapshot() {
        Ok(()) => true,
        Err(error) if crate::db::db_error_is_nested_transaction(&error) => false,
        Err(_) => return Err(snapshot_error()),
    };
    struct Snapshot<'a> {
        connection: &'a DbConnection,
        owned: bool,
    }
    impl Drop for Snapshot<'_> {
        fn drop(&mut self) {
            if self.owned {
                let _ = self.connection.rollback_read_snapshot();
            }
        }
    }
    let mut snapshot = Snapshot { connection, owned };
    let result = read()?;
    if snapshot.owned {
        connection
            .rollback_read_snapshot()
            .map_err(|_| snapshot_error())?;
        snapshot.owned = false;
    }
    Ok(result)
}

fn snapshot_error() -> DomainError {
    DomainError::Storage {
        message: "Could not verify derivation source authority in one source snapshot".to_owned(),
        repair: Some("Repair storage and retry the source proposal or validation.".to_owned()),
    }
}

pub(super) fn unavailable_issue() -> CurateValidationIssue {
    validation_issue(
        "derived_source_memory_authority_unavailable",
        "Could not verify current memory source seal and review authority.".to_owned(),
        "Repair storage and retry validation; unavailable authority never grants source access.",
    )
}

/// The caller has already checked source ownership and tombstone state and
/// owns the snapshot spanning that memory read and these authority reads.
pub(crate) fn validate_memory_authority(
    connection: &DbConnection,
    memory: &StoredMemory,
) -> Result<(), CurateValidationIssue> {
    let seal = connection
        .get_effective_memory_seal(&memory.id)
        .map_err(|_| unavailable_issue())?;
    if seal.as_ref().is_some_and(|seal| seal.is_sealed()) {
        return Err(validation_issue(
            "derived_source_memory_sealed",
            format!("Memory source {} has a closed seal.", memory.id),
            "Reveal the source through the verified memory reveal workflow or choose another source.",
        ));
    }
    let held = crate::core::memory_lifecycle::pending_memory_review_ids(
        connection,
        &memory.workspace_id,
        &[memory.id.as_str()],
    )
    .map_err(|_| unavailable_issue())?;
    if held.contains(&memory.id) {
        return Err(validation_issue(
            "derived_source_memory_review_pending",
            format!("Memory source {} is held for pending review.", memory.id),
            "Resolve the source memory review before proposing or applying a new derivation.",
        ));
    }
    Ok(())
}

pub(crate) fn require_memory_authority(
    connection: &DbConnection,
    memory: &StoredMemory,
) -> Result<(), DomainError> {
    validate_memory_authority(connection, memory).map_err(|issue| {
        let message = format!("{}: {}", issue.code, issue.message);
        let repair = Some(issue.repair);
        if issue.code == "derived_source_memory_authority_unavailable" {
            DomainError::Storage { message, repair }
        } else {
            DomainError::Usage { message, repair }
        }
    })
}

/// Resolve a workspace's withheld sources in bounded batches before reading
/// their bodies into a derivation. The caller has checked source ownership
/// and keeps the body scan and both authority reads in one snapshot.
pub(crate) fn withheld_memory_ids(
    connection: &DbConnection,
    workspace_id: &str,
    memory_ids: &[&str],
) -> Result<std::collections::BTreeSet<String>, CurateValidationIssue> {
    let seals = connection
        .get_effective_memory_seals_batch(memory_ids)
        .map_err(|_| unavailable_issue())?;
    let mut withheld = crate::core::memory_lifecycle::pending_memory_review_ids(
        connection,
        workspace_id,
        memory_ids,
    )
    .map_err(|_| unavailable_issue())?;
    withheld.extend(
        seals
            .into_iter()
            // The sidecar names its authority root; the mask must name the
            // requested revision whose body the caller would disclose.
            .filter_map(|(memory_id, seal)| seal.is_sealed().then_some(memory_id)),
    );
    Ok(withheld)
}

/// A reviewed proposal authorizes one new procedural object, not continued
/// access to source memories that were subsequently sealed or held for review.
/// Check all memory provenance, including learning-cluster audit references,
/// in one snapshot. Already-applied candidates are independent durable objects:
/// replaying their receipt/publication must not retroactively revoke them.
pub(super) fn require_procedural_candidate_sources(
    connection: &DbConnection,
    stored: &crate::db::StoredCurationCandidate,
) -> Result<(), DomainError> {
    if stored.status != crate::curate::CandidateStatus::Approved.as_str()
        || !matches!(
            stored.candidate_type.as_str(),
            "rule" | "anti_pattern_proposal" | "procedure"
        )
    {
        return Ok(());
    }
    with_snapshot(connection, || {
        let mut ids =
            super::source_memory_ids_for_rule_candidate_with_audits(connection, stored, true)?;
        if let Some(target) = &stored.target_memory_id {
            ids.push(target.clone());
        }
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            let memory = connection
                .get_memory(&id)
                .map_err(|_| DomainError::Storage {
                    message: "Could not inspect procedural candidate source authority.".to_owned(),
                    repair: Some("Repair storage and retry curation apply.".to_owned()),
                })?
                .ok_or_else(|| DomainError::Usage {
                    message: format!(
                        "derived_source_memory_missing: Memory source {id} is missing."
                    ),
                    repair: Some(
                        "Re-propose the candidate from existing source memories.".to_owned(),
                    ),
                })?;
            if memory.workspace_id != stored.workspace_id {
                return Err(DomainError::Usage {
                    message: format!(
                        "derived_source_workspace_mismatch: Memory source {id} is outside this workspace."
                    ),
                    repair: Some(
                        "Re-propose the candidate from sources in the same workspace.".to_owned(),
                    ),
                });
            }
            if memory.tombstoned_at.is_some() {
                return Err(DomainError::Usage {
                    message: format!(
                        "derived_source_memory_tombstoned: Memory source {id} is tombstoned."
                    ),
                    repair: Some(
                        "Re-propose the candidate from non-tombstoned source memories.".to_owned(),
                    ),
                });
            }
            require_memory_authority(connection, &memory)?;
        }
        Ok(())
    })
}
