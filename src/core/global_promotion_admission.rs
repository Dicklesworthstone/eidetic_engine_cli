//! Source authority and non-mutating previews for cross-workspace promotion.
//!
//! Promotion copies an admitted source snapshot; it is not an escape hatch
//! from author expiry, supersession, or commit/reveal. A refused source never
//! opens the destination. Previewing an absent store never creates it.

use chrono::{DateTime, Utc};
use sqlmodel_core::Value;

use super::{
    PromoteGlobalOptions, PromotionAction, PromotionCandidate, PromotionInput, PromotionPlan,
    PromotionRefusal, PromotionReport, PromotionVerdict, plan_promotion,
};
use crate::core::global_store::{GlobalStorePaths, global_workspace_id};
use crate::db::{DbConnection, DbError, DbOperation, StoredMemory};

type DbResult<T> = crate::db::Result<T>;

fn invalid_authority() -> DbError {
    DbError::MalformedRow {
        operation: DbOperation::Query,
        message: "Could not verify global-promotion lifecycle metadata".to_owned(),
    }
}

fn optional_text(value: Option<&Value>) -> DbResult<Option<&str>> {
    match value {
        Some(Value::Null) => Ok(None),
        Some(Value::Text(text)) => Ok(Some(text)),
        _ => Err(invalid_authority()),
    }
}

fn timestamp(value: &str) -> DbResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|_| invalid_authority())
}

/// Author bounds are inclusive. Promotion has no historical mode: a revision
/// with a successor must not become a new current global head, even if its
/// supersession cutoff is future-dated. Validate all metadata before deciding.
fn lifecycle_refusal(
    from: Option<&str>,
    to: Option<&str>,
    superseded: Option<&str>,
    reference: DateTime<Utc>,
) -> DbResult<Option<PromotionRefusal>> {
    let from = from.map(timestamp).transpose()?;
    let to = to.map(timestamp).transpose()?;
    let superseded = superseded.map(timestamp).transpose()?;
    if from.zip(to).is_some_and(|(from, to)| from > to) {
        return Err(invalid_authority());
    }
    Ok(if superseded.is_some() {
        Some(PromotionRefusal::Superseded)
    } else if from.is_some_and(|from| reference < from) {
        Some(PromotionRefusal::NotYetValid)
    } else if to.is_some_and(|to| reference > to) {
        Some(PromotionRefusal::Expired)
    } else {
        None
    })
}

pub(super) fn load_source(
    options: &PromoteGlobalOptions<'_>,
    reference: DateTime<Utc>,
) -> Result<(StoredMemory, PromotionPlan), String> {
    load_source_with_boundary(options, reference, || Ok(()))
}

fn load_source_with_boundary(
    options: &PromoteGlobalOptions<'_>,
    reference: DateTime<Utc>,
    after_body: impl FnOnce() -> Result<(), String>,
) -> Result<(StoredMemory, PromotionPlan), String> {
    let db = DbConnection::open_file_read_only(options.workspace_database_path)
        .map_err(|_| "Could not open the existing workspace database for promotion".to_owned())?;
    let snapshot = ReadSnapshot::begin(&db)
        .map_err(|_| "Could not begin the promotion source snapshot".to_owned())?;
    let memory = db
        .get_memory(options.memory_id)
        .map_err(|_| "Could not read the promotion source".to_owned())?
        .ok_or_else(|| "Promotion source memory not found".to_owned())?;
    after_body()?;
    // Always consult the sidecar. A populated body is not proof of reveal;
    // conversely, an explicitly revealed literal placeholder is public text.
    let seal = db
        .get_memory_seal(&memory.id)
        .map_err(|_| "Could not verify memory seal sidecar for promotion".to_owned())?;
    if let Some(raw) = seal.as_ref().and_then(|seal| seal.revealed_at.as_deref()) {
        timestamp(raw).map_err(|_| "Invalid promotion reveal metadata; source withheld".to_owned())?;
    }
    let markers = db
        .query(
            "SELECT superseded_at FROM memories WHERE id = ?1",
            &[Value::Text(memory.id.clone())],
        )
        .map_err(|_| "Could not verify promotion revision metadata".to_owned())?;
    let marker = markers
        .first()
        .filter(|_| markers.len() == 1)
        .ok_or_else(|| "Could not verify promotion source identity".to_owned())?;
    let superseded = optional_text(marker.get(0))
        .map_err(|_| "Invalid promotion revision metadata; source withheld".to_owned())?;
    let lifecycle = lifecycle_refusal(
        memory.valid_from.as_deref(),
        memory.valid_to.as_deref(),
        superseded,
        reference,
    )
    .map_err(|_| "Invalid promotion lifecycle metadata; source withheld".to_owned())?;
    let mut plan = plan_promotion(&PromotionInput {
        candidate: PromotionCandidate {
            memory_id: memory.id.clone(),
            workspace_id: memory.workspace_id.clone(),
            content: memory.content.clone(),
            level: memory.level.clone(),
            kind: memory.kind.clone(),
            trust_class: memory.trust_class.clone(),
            confidence: memory.confidence,
            tombstoned: memory.tombstoned_at.is_some(),
            sealed: seal.is_some_and(|seal| seal.is_sealed()),
        },
        nearest_global_duplicate: None,
        merge_similarity: None,
        global_lane_available: options.global_lane_available,
    });
    if plan.allowed()
        && let Some(refusal) = lifecycle
    {
        plan.verdict = PromotionVerdict::Refuse { refusal };
        plan.audit_action = "memory.promote_global_refused";
    }
    snapshot
        .finish()
        .map_err(|_| "Could not release the promotion source snapshot".to_owned())?;
    Ok((memory, plan))
}

pub(super) fn set_duplicate(plan: &mut PromotionPlan, twin: Option<&str>) {
    if plan.allowed() {
        plan.verdict = PromotionVerdict::Allow {
            action: twin.map_or(PromotionAction::Insert, |id| PromotionAction::MergeInto {
                global_memory_id: id.to_owned(),
            }),
        };
    }
}

pub(super) fn preview_report(plan: PromotionPlan, twin: Option<String>) -> PromotionReport {
    PromotionReport {
        plan,
        executed: false,
        global_memory_id: twin,
        already_promoted: false,
        index_job_id: None,
        index_status: "not_applicable".to_owned(),
        index_error: None,
    }
}

/// Read only the exact-content candidate identities and lifecycle columns.
/// The caller owns the snapshot/transaction. Expired, sealed, superseded,
/// weakly trusted, and differently typed twins cannot absorb a valid source.
/// A shorter-lived twin cannot silently shorten the promoted knowledge's life.
pub(super) fn find_twin(
    db: &DbConnection,
    workspace: &str,
    source: &StoredMemory,
    reference: DateTime<Utc>,
) -> DbResult<Option<String>> {
    let source_end = source.valid_to.as_deref().map(timestamp).transpose()?;
    let rows = db.query(
        "SELECT m.id, m.valid_from, m.valid_to, m.superseded_at, s.memory_id, s.revealed_at FROM memories m LEFT JOIN memory_seals s ON s.memory_id = m.id WHERE m.workspace_id = ?1 AND m.content = ?2 AND m.level = ?3 AND m.kind = ?4 AND m.tombstoned_at IS NULL AND m.trust_class IN ('human_explicit', 'agent_validated') ORDER BY m.id ASC",
        &[
            Value::Text(workspace.to_owned()),
            Value::Text(source.content.clone()),
            Value::Text(source.level.clone()),
            Value::Text(source.kind.clone()),
        ],
    )?;
    for row in rows {
        let Some(Value::Text(id)) = row.get(0) else {
            return Err(invalid_authority());
        };
        let from = optional_text(row.get(1))?;
        let to = optional_text(row.get(2))?;
        let superseded = optional_text(row.get(3))?;
        let seal_id = optional_text(row.get(4))?;
        let revealed = optional_text(row.get(5))?;
        if seal_id.is_some_and(|seal_id| seal_id != id.as_str())
            || (seal_id.is_none() && revealed.is_some())
        {
            return Err(invalid_authority());
        }
        // Match MemorySeal::is_sealed, additionally validating persisted times.
        let revealed = revealed.map(timestamp).transpose()?;
        let lifecycle = lifecycle_refusal(from, to, superseded, reference)?;
        if lifecycle.is_some() || (seal_id.is_some() && revealed.is_none()) {
            continue;
        }
        let end = to.map(timestamp).transpose()?;
        if end.is_some_and(|end| source_end.is_none_or(|source_end| end < source_end)) {
            continue;
        }
        return Ok(Some(id.clone()));
    }
    Ok(None)
}

fn bound_global_workspace(db: &DbConnection, paths: &GlobalStorePaths) -> Result<String, String> {
    if db.needs_migration().map_err(|_| "Could not inspect global store schema".to_owned())? {
        return Err("Global store needs migration; preview and existing-record operations do not migrate stores".to_owned());
    }
    crate::core::workspace::select_existing_workspace_row(
        db,
        &global_workspace_id(paths),
        &[paths.root.as_path()],
    )
    .map_err(|_| "Could not verify global workspace binding".to_owned())?
    .map(|workspace| workspace.id)
    .ok_or_else(|| "Existing global store has no matching workspace binding".to_owned())
}

pub(super) fn preview_twin(
    paths: &GlobalStorePaths,
    source: &StoredMemory,
    reference: DateTime<Utc>,
) -> Result<Option<String>, String> {
    if !paths.database_path.try_exists().map_err(|_| "Could not inspect global store".to_owned())? {
        return Ok(None);
    }
    let db = DbConnection::open_file_read_only(&paths.database_path)
        .map_err(|_| "Could not open existing global store read-only".to_owned())?;
    let snapshot = ReadSnapshot::begin(&db)
        .map_err(|_| "Could not begin global preview snapshot".to_owned())?;
    let workspace = bound_global_workspace(&db, paths)?;
    let twin = find_twin(&db, &workspace, source, reference)
        .map_err(|_| "Could not verify global duplicate lifecycle".to_owned())?;
    snapshot.finish().map_err(|_| "Could not release global preview snapshot".to_owned())?;
    Ok(twin)
}

/// Demotion and feedback address existing records; neither is an initializer.
/// Their dry-run path is strictly read-only. Binding is checked without schema
/// migration or inserting a replacement workspace row.
pub(super) fn open_existing_global(
    paths: &GlobalStorePaths,
    read_only: bool,
) -> Result<(DbConnection, String), String> {
    if !paths.database_path.try_exists().map_err(|_| "Could not inspect global store".to_owned())? {
        return Err("Global store does not exist".to_owned());
    }
    let db = if read_only {
        DbConnection::open_file_read_only(&paths.database_path)
    } else {
        DbConnection::open_file(&paths.database_path)
    }
    .map_err(|_| "Could not open existing global store".to_owned())?;
    let workspace = bound_global_workspace(&db, paths)?;
    Ok((db, workspace))
}

pub(super) struct ReadSnapshot<'a> {
    db: &'a DbConnection,
    active: bool,
}

impl<'a> ReadSnapshot<'a> {
    pub(super) fn begin(db: &'a DbConnection) -> DbResult<Self> {
        db.begin_read_snapshot()?;
        Ok(Self { db, active: true })
    }

    pub(super) fn finish(mut self) -> DbResult<()> {
        self.db.rollback_read_snapshot()?;
        self.active = false;
        Ok(())
    }
}

impl Drop for ReadSnapshot<'_> {
    fn drop(&mut self) {
        if self.active && self.db.rollback_read_snapshot().is_err() {
            tracing::error!(target: "ee::global::promotion", "could not release promotion read snapshot");
        }
    }
}

#[cfg(test)]
#[path = "global_promotion_admission_tests.rs"]
mod tests;
