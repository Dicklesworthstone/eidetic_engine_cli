//! Admit memory revisions before hydrating their bodies for `ee ask`.
//!
//! The caller owns the read snapshot. Author validity and the existing seal /
//! supersession authority must be checked in that same snapshot, before any
//! body fetch. Do not truncate by ID: the eventual answer's best evidence may
//! be anywhere in the workspace. Only body hydration is batched here; metadata
//! admission still examines the complete non-tombstoned workspace corpus.

use chrono::{DateTime, Utc};
use sqlmodel_core::Value;

use crate::db::{DbConnection, StoredMemory};
use crate::models::DomainError;

use super::{
    ASK_MEMORY_REVISION_PAGE_SIZE, corpus_storage_error, validity_contains, withheld_memory_ids,
};

pub(super) fn load_memory_revisions(
    connection: &DbConnection,
    workspace_id: &str,
    reference_time: DateTime<Utc>,
) -> Result<Vec<StoredMemory>, DomainError> {
    load_with_hydration_observer(connection, workspace_id, reference_time, |_| {})
}

/// Command advice needs only explicit rules and risk memories. Keep the same
/// lifecycle/authority decoder, but do not hydrate unrelated notes and facts
/// merely to discard them in the caller. This is a kind predicate, not a limit:
/// a relevant late-ID rule must still participate in matching.
pub(super) fn load_command_advice_revisions(
    connection: &DbConnection,
    workspace_id: &str,
    reference_time: DateTime<Utc>,
) -> Result<Vec<StoredMemory>, DomainError> {
    load_selected_revisions(
        connection,
        workspace_id,
        reference_time,
        "SELECT id, valid_from, valid_to FROM memories WHERE workspace_id = ?1 AND tombstoned_at IS NULL AND kind IN ('risk', 'anti-pattern', 'failure', 'rule') ORDER BY id ASC",
        |_| {},
    )
}

// The observer lets real-store tests assert which identities reach the body
// decoder, and commit through a second connection at that exact boundary.
// Production passes a no-op; there is no process-global hook or extra query.
fn load_with_hydration_observer(
    connection: &DbConnection,
    workspace_id: &str,
    reference_time: DateTime<Utc>,
    before_hydration: impl FnMut(&[&str]),
) -> Result<Vec<StoredMemory>, DomainError> {
    load_selected_revisions(
        connection,
        workspace_id,
        reference_time,
        "SELECT id, valid_from, valid_to FROM memories WHERE workspace_id = ?1 AND tombstoned_at IS NULL ORDER BY id ASC",
        before_hydration,
    )
}

fn load_selected_revisions(
    connection: &DbConnection,
    workspace_id: &str,
    reference_time: DateTime<Utc>,
    selection_sql: &str,
    mut before_hydration: impl FnMut(&[&str]),
) -> Result<Vec<StoredMemory>, DomainError> {
    let rows = connection
        .query(selection_sql, &[Value::Text(workspace_id.to_owned())])
        .map_err(|_| corpus_storage_error())?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    // Keep the canonical authority reader and its complete validation pass.
    // A closed seal overrides historical reference times; author expiry does
    // not replace the exclusive supersession cutoff. This also validates bad
    // revision markers even when no body would ultimately be eligible.
    let withheld = withheld_memory_ids(connection, workspace_id, reference_time)?;
    let mut admitted = Vec::new();
    for row in &rows {
        let id = match row.get(0) {
            Some(Value::Text(id)) => id.as_str(),
            _ => return Err(corpus_storage_error()),
        };
        let valid_from = optional_timestamp(row.get(1))?;
        let valid_to = optional_timestamp(row.get(2))?;
        // Evaluate validity first even for a sealed/superseded revision.
        // Neither a hidden body nor an expired first bound may conceal an
        // invalid second bound or an inverted author window.
        if validity_contains(valid_from, valid_to, reference_time)? && !withheld.contains(id) {
            admitted.push(id);
        }
    }

    // Pending review is current authority, not a historical trust score.
    // Withhold these identities before loading bodies; linked CASS evidence
    // later inherits this completed admission decision, never the other way.
    let held = super::quarantine::held_ids(
        connection,
        workspace_id,
        super::quarantine::Target::Memory,
        &admitted,
    )?;
    admitted.retain(|id| !held.contains(*id));

    let mut memories = Vec::with_capacity(admitted.len());
    for page in admitted.chunks(ASK_MEMORY_REVISION_PAGE_SIZE) {
        before_hydration(page);
        let mut loaded = connection
            .get_memories_batch(page)
            .map_err(|_| corpus_storage_error())?;
        for id in page {
            let memory = loaded.remove(*id).ok_or_else(corpus_storage_error)?;
            if memory.id != *id
                || memory.workspace_id != workspace_id
                || memory.tombstoned_at.is_some()
                || !validity_contains(
                    memory.valid_from.as_deref(),
                    memory.valid_to.as_deref(),
                    reference_time,
                )?
            {
                return Err(corpus_storage_error());
            }
            memories.push(memory);
        }
    }
    Ok(memories)
}

fn optional_timestamp(value: Option<&Value>) -> Result<Option<&str>, DomainError> {
    match value {
        Some(Value::Null) => Ok(None),
        Some(Value::Text(value)) => Ok(Some(value.as_str())),
        _ => Err(corpus_storage_error()),
    }
}

#[cfg(test)]
#[path = "ask_memory_admission_tests.rs"]
mod tests;
