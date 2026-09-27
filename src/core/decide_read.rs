//! Coherent, decision-only source reads for list, revisit, and record preview.
//!
//! Paging bounds individual allocations, not the completeness of the result.
//! Bodies, exact typed fields, revision markers and lineage share one snapshot.

use super::*;
use std::collections::BTreeSet;
use sqlmodel_core::Value;

const PAGE_SIZE: usize = 256;

#[derive(Clone, Copy)]
enum ReadIntent<'a> {
    Public,
    // Visibility is not headship. A hidden decision must never make its topic
    // look unused to the dry-run or the writer's uniqueness check.
    Record(&'a DecisionFields),
}

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

pub(super) fn load_record_heads(
    scope: &mut DecideScope,
    fields: &DecisionFields,
    now: DateTime<Utc>,
) -> Result<Vec<DecideItem>, DomainError> {
    if !scope.database_path.exists() {
        return Ok(Vec::new());
    }
    let connection = open_decide_database_read_only(&scope.database_path)?;
    let snapshot = ReadSnapshot::begin(&connection)?;
    bind_scope(&connection, scope)?;
    let heads = record_heads_in_current_snapshot(&connection, &scope.workspace_id, fields, now)?;
    snapshot.finish()?;
    Ok(heads)
}

/// The writer calls this only AFTER acquiring its transaction. This reader
/// borrows that transaction; it must not start, finish, or roll it back.
pub(super) fn record_heads_in_current_snapshot(
    connection: &DbConnection,
    workspace_id: &str,
    fields: &DecisionFields,
    now: DateTime<Utc>,
) -> Result<Vec<DecideItem>, DomainError> {
    read_current_snapshot(
        connection,
        workspace_id,
        false,
        now,
        ReadIntent::Record(fields),
        |_| Ok(()),
    )
}

fn bind_scope(connection: &DbConnection, scope: &mut DecideScope) -> Result<(), DomainError> {
    scope.workspace_id = bound_workspace_id_or_hash(
        connection,
        &scope.workspace_id,
        &[scope.workspace_path.as_path()],
    )
    .map_err(|_| read_error())?;
    Ok(())
}

fn read_with_observer(
    connection: &DbConnection,
    scope: &mut DecideScope,
    include_superseded: bool,
    now: DateTime<Utc>,
    before_hydration: impl FnMut(&[&str]) -> Result<(), DomainError>,
) -> Result<Vec<DecideItem>, DomainError> {
    let snapshot = ReadSnapshot::begin(connection)?;
    bind_scope(connection, scope)?;
    let decisions = read_current_snapshot(
        connection,
        &scope.workspace_id,
        include_superseded,
        now,
        ReadIntent::Public,
        before_hydration,
    )?;
    snapshot.finish()?;
    Ok(decisions)
}

fn read_current_snapshot(
    connection: &DbConnection,
    workspace_id: &str,
    include_superseded: bool,
    now: DateTime<Utc>,
    intent: ReadIntent<'_>,
    mut before_hydration: impl FnMut(&[&str]) -> Result<(), DomainError>,
) -> Result<Vec<DecideItem>, DomainError> {
    let mut after = String::new();
    let mut decisions = Vec::new();
    let mut lineage = lineage::DecisionLineage::new(workspace_id);
    let mut closed = None;
    loop {
        // Identity heads are clock-free: author expiry is not supersession.
        // Do not hydrate an entire transcript-heavy workspace and then throw
        // its non-decision bodies away. A late-ID decision is never truncated.
        let rows = connection
            .query(
                "SELECT id FROM memories WHERE workspace_id = ?1 AND kind = 'decision' AND tombstoned_at IS NULL AND (?2 = 1 OR superseded_at IS NULL) AND id > ?3 ORDER BY id ASC LIMIT ?4",
                &[
                    Value::Text(workspace_id.to_owned()),
                    Value::BigInt(if include_superseded { 1 } else { 0 }),
                    Value::Text(after.clone()),
                    Value::BigInt(PAGE_SIZE as i64),
                ],
            )
            .map_err(|_| read_error())?;
        if rows.is_empty() {
            break;
        }
        if rows.len() > PAGE_SIZE {
            return Err(read_error());
        }
        // Resolve real seal state once, without inspecting a sealed body's
        // spelling. An empty decision store needs no source-authority reads.
        if closed.is_none() {
            closed = Some(
                crate::core::memory_lifecycle::load_memory_seals_for_admission(
                    connection,
                    workspace_id,
                )
                .map_err(|_| read_error())?
                .into_iter()
                .filter(|seal| seal.is_sealed())
                .map(|seal| seal.memory_id)
                .collect::<BTreeSet<_>>(),
            );
        }
        let closed = closed.as_ref().ok_or_else(read_error)?;
        let mut ids = Vec::with_capacity(rows.len());
        for row in &rows {
            let id = row.get(0).and_then(Value::as_str).ok_or_else(read_error)?;
            if id <= after.as_str() {
                return Err(read_error());
            }
            after = id.to_owned();
            ids.push(id);
        }
        let held = crate::core::memory_lifecycle::pending_memory_review_ids(
            connection,
            workspace_id,
            &ids,
        )
        .map_err(|_| read_error())?;
        match intent {
            ReadIntent::Public => {
                ids.retain(|id| !closed.contains(*id) && !held.contains(*id));
            }
            ReadIntent::Record(_) => {
                // Sealing can replace the source body with a placeholder.
                // Without a readable topic, absence of a competing head is
                // unknowable. Tags are hints, not a second identity authority.
                if ids.iter().any(|id| closed.contains(*id)) {
                    return Err(record_authority_error());
                }
            }
        }
        if ids.is_empty() {
            // Advance with the raw page, not the number of admitted sources.
            // A full page of held rows must not hide an eligible later topic.
            if rows.len() < PAGE_SIZE {
                break;
            }
            continue;
        }
        before_hydration(&ids)?;
        let mut memories = connection.get_memories_batch(&ids).map_err(|_| read_error())?;
        let mut eligible = Vec::with_capacity(ids.len());
        for id in ids {
            let memory = memories.remove(id).ok_or_else(read_error)?;
            if memory.id != id
                || memory.workspace_id != workspace_id
                || memory.kind != "decision"
                || memory.tombstoned_at.is_some()
            {
                return Err(read_error());
            }
            if memory.content == crate::models::MEMORY_SEAL_PLACEHOLDER_CONTENT {
                match intent {
                    ReadIntent::Public => continue,
                    ReadIntent::Record(_) => return Err(record_authority_error()),
                }
            }
            if let ReadIntent::Record(fields) = intent {
                // Internal topic comparison does not publish held content.
                // Do not decode unrelated sidecars or traverse their lineage.
                let topic = topic_from_content(&memory.content)
                    .unwrap_or_else(|| memory.content.clone());
                let related = fields.supersedes.as_deref() == Some(id)
                    || normalize_decision_topic(&topic) == fields.normalized_topic;
                if !related {
                    continue;
                }
                if held.contains(id) {
                    return Err(record_authority_error());
                }
            }
            eligible.push(memory);
        }
        let roots: Vec<_> = eligible.iter().map(|memory| memory.id.as_str()).collect();
        lineage.load(connection, &roots)?;
        for memory in eligible {
            let item = memory_to_decide_item(connection, &memory, now, lineage.depth(&memory.id)?)
                .map_err(|_| read_error())?;
            if !include_superseded && item.superseded {
                return Err(read_error());
            }
            decisions.push(item);
        }
        if rows.len() < PAGE_SIZE {
            break;
        }
    }
    Ok(decisions)
}

fn record_authority_error() -> DomainError {
    DomainError::PolicyDenied {
        message: "Current decision source authority prevents recording or replacing this topic; no decision was recorded".to_owned(),
        repair: Some(
            "Review pending decision feedback or explicitly reveal sealed current decisions, then retry. A hidden head is not an unused topic.".to_owned(),
        ),
    }
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

#[cfg(test)]
#[path = "decide_admission_tests.rs"]
mod admission_tests;
