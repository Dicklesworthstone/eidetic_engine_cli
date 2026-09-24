//! Exact instant semantics for memory applicability, recency and end markers.
//!
//! RFC3339 has multiple spellings for the same instant. Neither text ordering
//! nor SQLite's millisecond Julian-day projection can decide a nanosecond
//! boundary. Historical rows stay unchanged: parse their offsets and fractions
//! at the read boundary, and perform conditional writes against the original
//! stored value so a concurrent writer cannot be lost.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use sqlmodel_core::Value;

use super::{
    DbConnection, DbError, DbOperation, Result, StoredMemory, TagCount, canonicalize_tag_filter,
    optional_text, required_text, stored_memory_from_row,
};

const MEMORY_COLUMNS: &str = "id, workspace_id, level, kind, content, workflow_id, confidence, utility, importance, provenance_uri, trust_class, trust_subclass, provenance_chain_hash, provenance_chain_hash_version, provenance_verification_status, provenance_verified_at, provenance_verification_note, created_at, updated_at, tombstoned_at, valid_from, valid_to";
const PAGE_SIZE: usize = 128;

fn malformed(message: &str) -> DbError {
    DbError::MalformedRow {
        operation: DbOperation::Query,
        message: message.to_owned(),
    }
}

fn instant(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn reference(raw: &str) -> Result<DateTime<Utc>> {
    instant(raw).ok_or_else(|| malformed("Memory reference time must be RFC3339"))
}

fn admits_bound(
    raw: Option<&str>,
    reference: DateTime<Utc>,
    predicate: impl FnOnce(DateTime<Utc>, DateTime<Utc>) -> bool,
) -> bool {
    match raw {
        None => true,
        Some(raw) => instant(raw).is_some_and(|value| predicate(value, reference)),
    }
}

/// Current heads, with author expiry applied at its inclusive endpoint. This
/// intentionally retains the existing listing contract: unlike retrieval, a
/// listing need not hide an author-scheduled future start. The history switch
/// still includes tombstoned and superseded rows without applicability filters.
pub(super) fn current(
    db: &DbConnection,
    workspace: &str,
    level: Option<&str>,
    include_history: bool,
    as_of: &str,
) -> Result<Vec<StoredMemory>> {
    let at = reference(as_of)?;
    let mut sql = format!("SELECT {MEMORY_COLUMNS} FROM memories WHERE workspace_id = ?1");
    let mut params = vec![Value::Text(workspace.to_owned())];
    if let Some(level) = level {
        sql.push_str(" AND level = ?2");
        params.push(Value::Text(level.to_owned()));
    }
    if !include_history {
        sql.push_str(" AND tombstoned_at IS NULL AND superseded_at IS NULL");
    }
    sql.push_str(" ORDER BY id ASC");
    db.query(&sql, &params)?
        .iter()
        .filter_map(|row| match stored_memory_from_row(row) {
            Ok(memory)
                if include_history
                    || admits_bound(memory.valid_to.as_deref(), at, |end, at| end >= at) =>
            {
                Some(Ok(memory))
            }
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

pub(super) fn by_tag(
    db: &DbConnection,
    workspace: &str,
    tag: &str,
    as_of: &str,
) -> Result<Vec<String>> {
    let at = reference(as_of)?;
    let rows = db.query(
        "SELECT m.id, m.valid_to FROM memories m JOIN memory_tags mt ON m.id = mt.memory_id WHERE m.workspace_id = ?1 AND mt.tag = ?2 AND m.tombstoned_at IS NULL AND m.superseded_at IS NULL ORDER BY m.id ASC",
        &[Value::Text(workspace.to_owned()), Value::Text(canonicalize_tag_filter(tag))],
    )?;
    let mut ids = Vec::new();
    for row in &rows {
        if admits_bound(optional_text(row, 1)?, at, |end, at| end >= at) {
            ids.push(required_text(row, 0, DbOperation::Query, "id")?.to_owned());
        }
    }
    Ok(ids)
}

pub(super) fn tag_counts(db: &DbConnection, workspace: &str, as_of: &str) -> Result<Vec<TagCount>> {
    let at = reference(as_of)?;
    // Project only the tag and expiry, not a second copy of memory content.
    // One source query keeps membership and applicability in one snapshot.
    let rows = db.query(
        "SELECT mt.tag, m.valid_to FROM memory_tags mt JOIN memories m ON mt.memory_id = m.id WHERE m.workspace_id = ?1 AND m.tombstoned_at IS NULL AND m.superseded_at IS NULL ORDER BY mt.tag ASC, m.id ASC",
        &[Value::Text(workspace.to_owned())],
    )?;
    let mut counts = BTreeMap::<String, u32>::new();
    for row in &rows {
        if admits_bound(optional_text(row, 1)?, at, |end, at| end >= at) {
            let tag = required_text(row, 0, DbOperation::Query, "tag")?;
            let count = counts.entry(tag.to_owned()).or_default();
            *count = count
                .checked_add(1)
                .ok_or_else(|| malformed("Memory tag count overflow"))?;
        }
    }
    let mut result: Vec<_> = counts
        .into_iter()
        .map(|(tag, count)| TagCount { tag, count })
        .collect();
    result.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.tag.cmp(&right.tag))
    });
    Ok(result)
}

/// A savepoint nests inside a caller-owned transaction and also works on a
/// read-only connection. It pins all pages to one view without acquiring the
/// write-owner gate or committing/rolling back the caller's transaction.
struct ReadScope<'a> {
    db: &'a DbConnection,
    active: bool,
}

impl<'a> ReadScope<'a> {
    fn begin(db: &'a DbConnection) -> Result<Self> {
        db.execute_read_snapshot_raw(
            DbOperation::BeginTransaction,
            "SAVEPOINT ee_memory_temporal",
        )?;
        Ok(Self { db, active: true })
    }

    fn finish(mut self) -> Result<()> {
        self.db.execute_read_snapshot_raw(
            DbOperation::CommitTransaction,
            "RELEASE ee_memory_temporal",
        )?;
        self.active = false;
        Ok(())
    }
}

impl Drop for ReadScope<'_> {
    fn drop(&mut self) {
        if self.active {
            let rolled_back = self.db.execute_read_snapshot_raw(
                DbOperation::RollbackTransaction,
                "ROLLBACK TO ee_memory_temporal",
            );
            let released = self.db.execute_read_snapshot_raw(
                DbOperation::RollbackTransaction,
                "RELEASE ee_memory_temporal",
            );
            if rolled_back.is_err() || released.is_err() {
                tracing::error!("Failed to release exact memory read snapshot");
            }
        }
    }
}

/// Read lightweight applicability metadata once, by an identity cursor, and
/// retain only the best `limit` identities. Exact instants, not SQL's date
/// parser or an approximate sort key, decide eligibility and final ordering.
/// Expired/future history cannot exhaust an early candidate page. Body loading
/// is deferred until selection is complete, in bind-safe batches inside the
/// SAME snapshot. The all-history resume path does not repeatedly sort and skip
/// full memory bodies through increasing OFFSETs.
pub(super) fn recent(
    db: &DbConnection,
    workspace: &str,
    as_of: &str,
    limit: u32,
) -> Result<Vec<StoredMemory>> {
    recent_with_boundary(db, workspace, as_of, limit, || Ok(()))
}

// Test seam for a real second writer between selection and hydration. It does
// not replace the database, relax admission, or run outside the owned scope.
fn recent_with_boundary(
    db: &DbConnection,
    workspace: &str,
    as_of: &str,
    limit: u32,
    after_selection: impl FnOnce() -> Result<()>,
) -> Result<Vec<StoredMemory>> {
    let at = reference(as_of)?;
    if limit == 0 {
        return Ok(Vec::new());
    }
    let limit = usize::try_from(limit).map_err(|_| malformed("Memory recency limit overflow"))?;
    let snapshot = ReadScope::begin(db)?;
    let mut selected = BTreeSet::<(Reverse<DateTime<Utc>>, String)>::new();
    let mut cursor: Option<String> = None;
    loop {
        // IDs are only a traversal cursor, never a substitute for chronology.
        // Keeping dates in SQL would both repeat a computed sort per page and
        // let that parser silently discard instants accepted by our contract.
        let mut sql = "SELECT id, created_at, updated_at, valid_from, valid_to, superseded_at FROM memories WHERE workspace_id = ?1 AND tombstoned_at IS NULL".to_owned();
        let mut parameters = vec![
            Value::Text(workspace.to_owned()),
            Value::BigInt(PAGE_SIZE as i64),
        ];
        if let Some(after) = &cursor {
            sql.push_str(" AND id > ?3");
            parameters.push(Value::Text(after.clone()));
        }
        sql.push_str(" ORDER BY id ASC LIMIT ?2");
        let rows = db.query(&sql, &parameters)?;
        for row in &rows {
            let id = required_text(row, 0, DbOperation::Query, "id")?;
            if cursor.as_deref().is_some_and(|previous| id <= previous) {
                return Err(malformed("Memory recency cursor did not advance"));
            }
            cursor = Some(id.to_owned());
            let (Some(created), Some(updated)) = (
                instant(required_text(row, 1, DbOperation::Query, "created_at")?),
                instant(required_text(row, 2, DbOperation::Query, "updated_at")?),
            ) else {
                continue;
            };
            if created > at
                || updated > at
                || !admits_bound(optional_text(row, 3)?, at, |start, at| start <= at)
                || !admits_bound(optional_text(row, 4)?, at, |end, at| end >= at)
                || !admits_bound(optional_text(row, 5)?, at, |end, at| end > at)
            {
                continue;
            }
            selected.insert((Reverse(created), id.to_owned()));
            if selected.len() > limit {
                selected.pop_last();
            }
        }
        if rows.len() < PAGE_SIZE {
            break;
        }
    }
    after_selection()?;
    let ids: Vec<_> = selected.into_iter().map(|(_, id)| id).collect();
    let mut memories = Vec::with_capacity(ids.len());
    for page in ids.chunks(PAGE_SIZE) {
        let page_ids: Vec<_> = page.iter().map(String::as_str).collect();
        let mut loaded = db.get_memories_batch(&page_ids)?;
        for id in page_ids {
            let memory = loaded
                .remove(id)
                .ok_or_else(|| malformed("Selected memory is missing from the read snapshot"))?;
            if memory.id != id || memory.workspace_id != workspace || memory.tombstoned_at.is_some()
            {
                return Err(malformed(
                    "Selected memory does not belong to the read snapshot",
                ));
            }
            memories.push(memory);
        }
    }
    snapshot.finish()?;
    Ok(memories)
}

#[derive(Clone, Copy)]
pub(super) enum EndColumn {
    ValidTo,
    SupersededAt,
}

/// Preserve the existing monotonic tightening contract, but compare instants,
/// not spellings. Equality is a no-op even across offsets. Compare-and-swap
/// binds the exact prior value (including NULL); no broad UPDATE can overwrite
/// a concurrent end-marker change. Persistent contention is an explicit error.
pub(super) fn tighten_end(
    db: &DbConnection,
    id: &str,
    raw: &str,
    column: EndColumn,
) -> Result<bool> {
    let at = reference(raw)?;
    let column = match column {
        EndColumn::ValidTo => "valid_to",
        EndColumn::SupersededAt => "superseded_at",
    };
    for _ in 0..3 {
        let rows = db.query(
            &format!("SELECT {column} FROM memories WHERE id = ?1 AND tombstoned_at IS NULL"),
            &[Value::Text(id.to_owned())],
        )?;
        let Some(row) = rows.first() else {
            return Ok(false);
        };
        let previous = optional_text(row, 0)?;
        if let Some(previous) = previous {
            let end = instant(previous)
                .ok_or_else(|| malformed("Stored memory end marker is not RFC3339"))?;
            if end <= at {
                return Ok(false);
            }
        }
        let affected = db.execute_for(
            DbOperation::Execute,
            &format!("UPDATE memories SET {column} = ?1, updated_at = ?3 WHERE id = ?2 AND tombstoned_at IS NULL AND {column} IS ?4"),
            &[
                Value::Text(raw.to_owned()),
                Value::Text(id.to_owned()),
                Value::Text(Utc::now().to_rfc3339()),
                previous.map_or(Value::Null, |value| Value::Text(value.to_owned())),
            ],
        )?;
        if affected > 0 {
            return Ok(true);
        }
    }
    Err(malformed(
        "Memory end marker changed during update; retry against current state",
    ))
}

#[cfg(test)]
#[path = "memory_temporal_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "memory_recent_scan_tests.rs"]
mod recent_scan_tests;
