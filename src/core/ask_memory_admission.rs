//! Admit memory revisions before hydrating their bodies for `ee ask`.
//!
//! The caller owns the read snapshot. Author validity and the existing seal /
//! supersession authority must be checked in that same snapshot, before any
//! body fetch. Both metadata reads and body hydration use bounded pages, not
//! a corpus limit: the eventual answer's best evidence may be anywhere in the
//! workspace. Validate every selected row before hydrating any admitted body.

use chrono::{DateTime, Utc};
use sqlmodel_core::{Row, Value};

use crate::db::{DbConnection, StoredMemory};
use crate::models::DomainError;

use super::{
    ASK_MEMORY_REVISION_PAGE_SIZE, corpus_storage_error, validity_contains, withheld_memory_ids,
};

#[derive(Clone, Copy)]
enum RevisionSelection {
    All,
    CommandAdvice,
}

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
        RevisionSelection::CommandAdvice,
        |_| {},
        |_| {},
    )
}

// The observers let real-store tests assert the bounded reads and commit
// through a second connection at exact metadata/body boundaries. Production
// passes no-ops; there is no process-global hook or extra query.
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
        RevisionSelection::All,
        before_hydration,
        |_| {},
    )
}

fn metadata_page(
    connection: &DbConnection,
    workspace_id: &str,
    selection: RevisionSelection,
    after: Option<&str>,
) -> Result<Vec<Row>, DomainError> {
    let kinds = match selection {
        RevisionSelection::All => "",
        RevisionSelection::CommandAdvice => {
            " AND kind IN ('risk', 'anti-pattern', 'failure', 'rule')"
        }
    };
    // Use a strict keyset bound, not OFFSET or a nullable-parameter OR. Only
    // binary-owned predicates enter SQL; workspace and cursor remain data.
    let continuation = if after.is_some() { " AND id > ?3" } else { "" };
    let sql = format!(
        "SELECT id, valid_from, valid_to FROM memories WHERE workspace_id = ?1 AND tombstoned_at IS NULL{kinds}{continuation} ORDER BY id ASC LIMIT ?2"
    );
    let mut parameters = vec![
        Value::Text(workspace_id.to_owned()),
        Value::BigInt(ASK_MEMORY_REVISION_PAGE_SIZE as i64),
    ];
    if let Some(after) = after {
        parameters.push(Value::Text(after.to_owned()));
    }
    let rows = connection
        .query(&sql, &parameters)
        .map_err(|_| corpus_storage_error())?;
    if rows.len() > ASK_MEMORY_REVISION_PAGE_SIZE {
        return Err(corpus_storage_error());
    }
    Ok(rows)
}

fn load_selected_revisions(
    connection: &DbConnection,
    workspace_id: &str,
    reference_time: DateTime<Utc>,
    selection: RevisionSelection,
    mut before_hydration: impl FnMut(&[&str]),
    mut after_metadata_page: impl FnMut(usize),
) -> Result<Vec<StoredMemory>, DomainError> {
    let mut rows = metadata_page(connection, workspace_id, selection, None)?;
    if rows.is_empty() {
        after_metadata_page(0);
        return Ok(Vec::new());
    }

    // Keep the canonical authority reader and its complete validation pass.
    // A closed seal overrides historical reference times; author expiry does
    // not replace the exclusive supersession cutoff. This also validates bad
    // revision markers even when no body would ultimately be eligible.
    let withheld = withheld_memory_ids(connection, workspace_id, reference_time)?;
    let mut admitted_ids = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let count = rows.len();
        for row in rows {
            let id = match row.get(0) {
                Some(Value::Text(id)) => id.as_str(),
                _ => return Err(corpus_storage_error()),
            };
            // Never spin or silently omit evidence if a backend returns a
            // repeated/out-of-order key, including across page boundaries.
            if cursor.as_deref().is_some_and(|previous| id <= previous) {
                return Err(corpus_storage_error());
            }
            let valid_from = optional_timestamp(row.get(1))?;
            let valid_to = optional_timestamp(row.get(2))?;
            // Even hidden rows must have valid, ordered author bounds. A bad
            // final page withholds the entire corpus before the first body read.
            if validity_contains(valid_from, valid_to, reference_time)? && !withheld.contains(id) {
                admitted_ids.push(id.to_owned());
            }
            cursor = Some(id.to_owned());
        }
        after_metadata_page(count);
        if count < ASK_MEMORY_REVISION_PAGE_SIZE {
            break;
        }
        rows = metadata_page(connection, workspace_id, selection, cursor.as_deref())?;
    }

    // Retain only eligible identities, not all historical timestamp cells.
    // The result corpus and canonical seal/revision authority still scale with
    // their actual populations; paging is not a claim of constant total memory.
    let mut admitted: Vec<_> = admitted_ids.iter().map(String::as_str).collect();
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
