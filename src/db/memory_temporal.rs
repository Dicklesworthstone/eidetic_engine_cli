//! Exact instant semantics for memory applicability, recency and end markers.
//!
//! RFC3339 has multiple spellings for the same instant. Neither text ordering
//! nor SQLite's millisecond Julian-day projection can decide a nanosecond
//! boundary. Historical rows stay unchanged: parse their offsets and fractions
//! at the read boundary, and perform conditional writes against the original
//! stored value so a concurrent writer cannot be lost.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, SecondsFormat, Utc};
use sqlmodel_core::Value;

use super::{
    DbConnection, DbError, DbOperation, Result, StoredMemory, TagCount, canonicalize_tag_filter,
    optional_text, required_text, stored_memory_from_row,
};

// Keep exact evidence-time writes within the database temporal boundary.
#[path = "retrieval_affinity_time.rs"]
mod retrieval_affinity_time;

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

/// A missing author start inherits the row's creation instant, expressed in
/// the validity-column canon rather than the bookkeeping-column spelling.
/// Native captures, revisions and timestamp-preserving imports all use this
/// boundary; explicit author bounds remain the caller's validated values.
/// AutoSi matches the core/import normalizer and retains every nanosecond.
/// Historical rows stay untouched; applicability readers compare parsed instants.
pub(super) fn default_valid_from(created_at: &str) -> Result<String> {
    instant(created_at)
        .map(|value| value.to_rfc3339_opts(SecondsFormat::AutoSi, true))
        .ok_or_else(|| DbError::MalformedRow {
            operation: DbOperation::Execute,
            message: "Memory created_at must be RFC3339 to derive valid_from".to_owned(),
        })
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

impl DbConnection {
    /// Visit retrieval-source memories in bounded, canonical-ID pages.
    ///
    /// The caller owns the read snapshot across this visitor, sidecar reads
    /// and every other source class. This method never starts, finishes or
    /// rolls back that transaction. Callback errors (including cancellation
    /// and a delta-budget refusal) propagate without reading another page.
    ///
    /// Match `list_memories_for_retrieval_with_global(_, None, false)` exactly:
    /// author-expired, future and superseded versions remain indexable, while
    /// tombstoned rows do not. Same-workspace globals appear only once. Scope
    /// tags are resolved from the same snapshot, never an earlier cache.
    /// Only one page of source bodies is retained here, not the whole store.
    pub(crate) fn visit_index_memory_batches_in_current_snapshot<E>(
        &self,
        workspace: &str,
        mut visitor: impl FnMut(&[StoredMemory]) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E>
    where
        E: From<DbError>,
    {
        let request = asupersync::Cx::current();
        let page_size = super::MEMORY_INDEX_METADATA_BATCH_SIZE;
        let mut cursor: Option<String> = None;
        loop {
            super::checkpoint_retry_cx(DbOperation::Query, request.as_ref())?;
            let mut sql = format!(
                "SELECT {MEMORY_COLUMNS} FROM memories m WHERE (m.workspace_id = ?1 OR EXISTS (SELECT 1 FROM memory_tags mt WHERE mt.memory_id = m.id AND lower(replace(trim(mt.tag), '-', '_')) IN (?2, ?3))) AND m.tombstoned_at IS NULL"
            );
            let mut parameters = vec![
                Value::Text(workspace.to_owned()),
                Value::Text(super::GLOBAL_MEMORY_SCOPE_TAG.to_owned()),
                Value::Text(super::HOUSE_RULE_MEMORY_SCOPE_TAG.to_owned()),
                Value::BigInt(page_size as i64),
            ];
            if let Some(after) = &cursor {
                sql.push_str(" AND m.id > ?5");
                parameters.push(Value::Text(after.clone()));
            }
            sql.push_str(" ORDER BY m.id ASC LIMIT ?4");
            let rows = self.query_for(DbOperation::Query, &sql, &parameters)?;
            if rows.len() > page_size {
                return Err(malformed("Index memory page exceeded its source bound").into());
            }
            let page_len = rows.len();
            if page_len == 0 {
                break;
            }
            let mut page = Vec::with_capacity(page_len);
            // Consume raw rows before exposing the page, so the callback does
            // not retain a second raw-row copy while projecting sidecars.
            for row in rows {
                super::checkpoint_retry_cx(DbOperation::Query, request.as_ref())?;
                let memory = stored_memory_from_row(&row)?;
                if cursor
                    .as_deref()
                    .is_some_and(|previous| memory.id.as_str() <= previous)
                {
                    return Err(malformed("Index memory cursor did not advance").into());
                }
                cursor = Some(memory.id.clone());
                page.push(memory);
            }
            visitor(&page)?;
            super::checkpoint_retry_cx(DbOperation::Query, request.as_ref())?;
            if page_len < page_size {
                break;
            }
        }
        Ok(())
    }
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

#[cfg(test)]
mod index_stream_tests {
    use super::*;
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput};
    use crate::models::MemoryId;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;
    const LOCAL: &str = "wsp_01234567890123456789012345";
    const FOREIGN: &str = "wsp_01234567890123456789012346";

    fn id(number: usize) -> String {
        MemoryId::from_uuid(uuid::Uuid::from_u128(number as u128)).to_string()
    }

    fn setup(db: &DbConnection) -> Result<()> {
        db.migrate()?;
        for (workspace, path) in [
            (LOCAL, "/index-page-local"),
            (FOREIGN, "/index-page-foreign"),
        ] {
            db.insert_workspace(
                workspace,
                &CreateWorkspaceInput {
                    path: path.to_owned(),
                    name: None,
                },
            )?;
        }
        Ok(())
    }

    fn insert(db: &DbConnection, number: usize, workspace: &str, tags: Vec<String>) -> Result<()> {
        db.insert_memory(
            &id(number),
            &CreateMemoryInput {
                workspace_id: workspace.to_owned(),
                level: "semantic".to_owned(),
                kind: "fact".to_owned(),
                content: format!("Source {number} preserves its complete UTF-8 text: café 資料."),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: Some(format!("test://index-memory/{number}")),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags,
                valid_from: None,
                valid_to: None,
            },
        )
    }

    fn seed(db: &DbConnection, count: usize) -> Result<()> {
        db.with_transaction(|| {
            for number in (1..=count).rev() {
                insert(db, number, LOCAL, Vec::new())?;
            }
            Ok(())
        })
    }

    fn collect(db: &DbConnection, workspace: &str) -> Result<(Vec<StoredMemory>, Vec<usize>)> {
        let snapshot = ReadScope::begin(db)?;
        let mut memories = Vec::new();
        let mut page_sizes = Vec::new();
        db.visit_index_memory_batches_in_current_snapshot::<DbError>(workspace, |page| {
            page_sizes.push(page.len());
            memories.extend_from_slice(page);
            Ok(())
        })?;
        snapshot.finish()?;
        Ok((memories, page_sizes))
    }

    #[test]
    fn full_rows_and_page_boundaries_match_the_existing_retrieval_source() -> TestResult {
        let bound = crate::db::MEMORY_INDEX_METADATA_BATCH_SIZE;
        for count in [0, 1, bound - 1, bound, bound + 1, 2 * bound, 2 * bound + 1] {
            let db = DbConnection::open_memory()?;
            setup(&db)?;
            seed(&db, count)?;
            let expected = db.list_memories_for_retrieval_with_global(LOCAL, None, false)?;
            let (actual, sizes) = collect(&db, LOCAL)?;
            assert_eq!(actual, expected);
            assert_eq!(actual.len(), count);
            assert_eq!(sizes.iter().sum::<usize>(), count);
            assert_eq!(sizes.len(), count.div_ceil(bound));
            assert!(sizes.iter().all(|&size| size > 0 && size <= bound));
            assert!(sizes.iter().rev().skip(1).all(|&size| size == bound));
        }
        Ok(())
    }

    #[test]
    fn global_scope_is_exact_and_historical_versions_are_not_discarded() -> TestResult {
        let db = DbConnection::open_memory()?;
        setup(&db)?;
        db.with_transaction(|| {
            for number in (1..=513).rev() {
                let workspace = if number % 4 == 0 { FOREIGN } else { LOCAL };
                let tags = match number % 8 {
                    0 => vec!["global".to_owned(), "house_rule".to_owned()],
                    1 => vec!["global".to_owned()],
                    _ => Vec::new(),
                };
                insert(&db, number, workspace, tags)?;
            }
            Ok(())
        })?;
        // Legacy tag spelling must use the same normalization as the full reader.
        db.execute_for(DbOperation::Execute, "UPDATE memory_tags SET tag = ' House-Rule ' WHERE memory_id = ?1 AND tag = 'house_rule'", &[Value::Text(id(8))])?;
        db.execute_for(
            DbOperation::Execute,
            "UPDATE memories SET valid_to = '2020-01-01T00:00:00Z' WHERE id = ?1",
            &[Value::Text(id(1))],
        )?;
        db.execute_for(
            DbOperation::Execute,
            "UPDATE memories SET valid_from = '2099-01-01T00:00:00Z' WHERE id = ?1",
            &[Value::Text(id(2))],
        )?;
        db.execute_for(
            DbOperation::Execute,
            "UPDATE memories SET superseded_at = '2026-01-01T00:00:00Z' WHERE id = ?1",
            &[Value::Text(id(3))],
        )?;
        db.execute_for(
            DbOperation::Execute,
            "UPDATE memories SET tombstoned_at = '2026-01-01T00:00:00Z' WHERE id = ?1",
            &[Value::Text(id(16))],
        )?;
        for workspace in [LOCAL, FOREIGN] {
            let expected = db.list_memories_for_retrieval_with_global(workspace, None, false)?;
            let (actual, pages) = collect(&db, workspace)?;
            assert_eq!(actual, expected);
            assert!(pages.len() > 1);
            assert!(actual.windows(2).all(|rows| rows[0].id < rows[1].id));
        }
        let (actual, _) = collect(&db, LOCAL)?;
        let ids: BTreeSet<_> = actual.into_iter().map(|row| row.id).collect();
        for number in [1, 2, 3, 8] {
            assert!(ids.contains(&id(number)));
        }
        for number in [4, 16] {
            assert!(!ids.contains(&id(number)));
        }
        Ok(())
    }

    #[test]
    fn callback_failure_stops_after_one_page_without_committing_outer_writes() -> TestResult {
        let db = DbConnection::open_memory()?;
        setup(&db)?;
        seed(&db, 257)?;
        let before = db.get_memory(&id(1))?.ok_or("source")?;
        let failed: Result<()> = db.with_transaction(|| {
            db.execute_for(
                DbOperation::Execute,
                "UPDATE memories SET content = 'transaction-local edit' WHERE id = ?1",
                &[Value::Text(id(1))],
            )?;
            let mut pages = 0;
            let result =
                db.visit_index_memory_batches_in_current_snapshot::<DbError>(LOCAL, |_| {
                    pages += 1;
                    Err(malformed("injected consumer refusal"))
                });
            assert_eq!(pages, 1);
            assert!(
                result
                    .expect_err("callback error")
                    .to_string()
                    .contains("injected consumer refusal")
            );
            assert_eq!(
                db.get_memory(&id(1))?
                    .expect("source still inside transaction")
                    .content,
                "transaction-local edit"
            );
            Err(malformed("outer rollback requested"))
        });
        assert!(failed.is_err());
        assert_eq!(db.get_memory(&id(1))?.ok_or("rolled back source")?, before);
        assert_eq!(collect(&db, LOCAL)?.0.len(), 257);
        Ok(())
    }

    #[test]
    fn body_and_global_tag_changes_between_pages_wait_for_the_next_snapshot() -> TestResult {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("memory-pages.db");
        let writer = DbConnection::open_file(&path)?;
        setup(&writer)?;
        seed(&writer, 260)?;
        insert(&writer, 300, FOREIGN, vec!["global".to_owned()])?;
        let reader = DbConnection::open_file_read_only(&path)?;
        reader.begin_read_snapshot()?;
        let before = reader.list_memories_for_retrieval_with_global(LOCAL, None, false)?;
        let mut actual = Vec::new();
        let mut pages = 0;
        reader.visit_index_memory_batches_in_current_snapshot::<DbError>(LOCAL, |page| {
            pages += 1;
            actual.extend_from_slice(page);
            if pages == 1 {
                writer.with_transaction(|| {
                    writer.execute_for(DbOperation::Execute, "UPDATE memories SET content = 'replacement belongs to the next snapshot' WHERE id = ?1", &[Value::Text(id(200))])?;
                    writer.remove_memory_tags(&id(300), &["global".to_owned()])?;
                    insert(&writer, 0, LOCAL, Vec::new())?;
                    Ok(())
                })?;
            }
            Ok(())
        })?;
        assert_eq!(pages, 3);
        assert_eq!(actual, before);
        reader.commit_read_snapshot()?;
        let (after, _) = collect(&reader, LOCAL)?;
        assert!(after.iter().all(|memory| memory.id != id(300)));
        assert!(after.iter().any(|memory| memory.id == id(0)));
        assert!(after.iter().any(|memory| memory.id == id(200)
            && memory.content == "replacement belongs to the next snapshot"));
        assert_eq!(
            after,
            reader.list_memories_for_retrieval_with_global(LOCAL, None, false)?
        );
        Ok(())
    }

    #[test]
    fn missing_storage_is_not_an_empty_success_and_preserves_the_callers_snapshot() -> TestResult {
        let db = DbConnection::open_memory()?;
        db.begin_read_snapshot()?;
        let mut invoked = false;
        let result = db.visit_index_memory_batches_in_current_snapshot::<DbError>(LOCAL, |_| {
            invoked = true;
            Ok(())
        });
        assert!(result.is_err());
        assert!(!invoked);
        db.commit_read_snapshot()?;
        Ok(())
    }
}
