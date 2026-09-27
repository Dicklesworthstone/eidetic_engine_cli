//! Coherent, decision-only source reads for list, revisit, and record preview.
//!
//! Paging bounds individual allocations, not the completeness of the result.
//! Bodies, exact typed fields, revision markers and lineage share one snapshot.

use super::*;
use sqlmodel_core::Value;

const PAGE_SIZE: usize = 256;

fn read_error() -> DomainError {
    decide_storage_error("Could not read a coherent decision snapshot; no partial result returned")
}

pub(super) fn load(
    scope: &mut DecideScope,
    include_superseded: bool,
    now: DateTime<Utc>,
) -> Result<Vec<DecideItem>, DomainError> {
    if !scope.database_path.exists() {
        return Ok(Vec::new());
    }
    let connection = open_decide_database_read_only(&scope.database_path)?;
    read_with_observer(&connection, scope, include_superseded, now, |_| Ok(()))
}

fn read_with_observer(
    connection: &DbConnection,
    scope: &mut DecideScope,
    include_superseded: bool,
    now: DateTime<Utc>,
    mut before_hydration: impl FnMut(&[&str]) -> Result<(), DomainError>,
) -> Result<Vec<DecideItem>, DomainError> {
    let snapshot = ReadSnapshot::begin(connection)?;
    scope.workspace_id = bound_workspace_id_or_hash(
        connection,
        &scope.workspace_id,
        &[scope.workspace_path.as_path()],
    )
    .map_err(|_| read_error())?;
    let mut after = String::new();
    let mut decisions = Vec::new();
    loop {
        // Identity heads are clock-free: author expiry is not supersession.
        // Do not hydrate an entire transcript-heavy workspace and then throw
        // its non-decision bodies away. A late-ID decision is never truncated.
        let rows = connection
            .query(
                "SELECT id FROM memories WHERE workspace_id = ?1 AND kind = 'decision' AND tombstoned_at IS NULL AND (?2 = 1 OR superseded_at IS NULL) AND id > ?3 ORDER BY id ASC LIMIT ?4",
                &[
                    Value::Text(scope.workspace_id.clone()),
                    Value::BigInt(if include_superseded { 1 } else { 0 }),
                    Value::Text(after.clone()),
                    Value::BigInt(PAGE_SIZE as i64),
                ],
            )
            .map_err(|_| read_error())?;
        if rows.is_empty() {
            break;
        }
        let mut ids = Vec::with_capacity(rows.len());
        for row in &rows {
            let id = row.get(0).and_then(Value::as_str).ok_or_else(read_error)?;
            if id <= after.as_str() {
                return Err(read_error());
            }
            after = id.to_owned();
            ids.push(id);
        }
        before_hydration(&ids)?;
        let mut memories = connection.get_memories_batch(&ids).map_err(|_| read_error())?;
        for id in ids {
            let memory = memories.remove(id).ok_or_else(read_error)?;
            if memory.id != id
                || memory.workspace_id != scope.workspace_id
                || memory.kind != "decision"
                || memory.tombstoned_at.is_some()
            {
                return Err(read_error());
            }
            let item = memory_to_decide_item(connection, &memory, now).map_err(|_| read_error())?;
            if !include_superseded && item.superseded {
                return Err(read_error());
            }
            decisions.push(item);
        }
        if rows.len() < PAGE_SIZE {
            break;
        }
    }
    snapshot.finish()?;
    Ok(decisions)
}

struct ReadSnapshot<'a> {
    connection: &'a DbConnection,
    active: bool,
}

impl<'a> ReadSnapshot<'a> {
    fn begin(connection: &'a DbConnection) -> Result<Self, DomainError> {
        connection.begin_read_snapshot().map_err(|_| read_error())?;
        // Construct ownership only after BEGIN succeeds. A failed nested begin
        // cannot release a transaction that belongs to our caller.
        Ok(Self {
            connection,
            active: true,
        })
    }

    fn finish(mut self) -> Result<(), DomainError> {
        self.connection
            .commit_read_snapshot()
            .map_err(|_| read_error())?;
        self.active = false;
        Ok(())
    }
}

impl Drop for ReadSnapshot<'_> {
    fn drop(&mut self) {
        if self.active && self.connection.rollback_read_snapshot().is_err() {
            tracing::error!("failed to release the owned decision read snapshot");
        }
    }
}

#[cfg(test)]
#[path = "decide_read_tests.rs"]
mod tests;
