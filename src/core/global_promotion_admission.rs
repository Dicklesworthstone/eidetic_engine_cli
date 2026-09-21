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
        timestamp(raw)
            .map_err(|_| "Invalid promotion reveal metadata; source withheld".to_owned())?;
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
/// differently trusted, and differently typed twins cannot absorb a source.
/// Authored bounds must match as instants, including null/unbounded starts and
/// ends. Promotion changes scope, never the source's lifetime or attestation.
pub(super) fn find_twin(
    db: &DbConnection,
    workspace: &str,
    source: &StoredMemory,
    reference: DateTime<Utc>,
) -> DbResult<Option<String>> {
    let source_start = source.valid_from.as_deref().map(timestamp).transpose()?;
    let source_end = source.valid_to.as_deref().map(timestamp).transpose()?;
    let rows = db.query(
        "SELECT m.id, m.valid_from, m.valid_to, m.superseded_at, s.memory_id, s.revealed_at FROM memories m LEFT JOIN memory_seals s ON s.memory_id = m.id WHERE m.workspace_id = ?1 AND m.content = ?2 AND m.level = ?3 AND m.kind = ?4 AND m.tombstoned_at IS NULL AND m.trust_class = ?5 AND m.trust_subclass IS ?6 ORDER BY m.id ASC",
        &[
            Value::Text(workspace.to_owned()),
            Value::Text(source.content.clone()),
            Value::Text(source.level.clone()),
            Value::Text(source.kind.clone()),
            Value::Text(source.trust_class.clone()),
            source
                .trust_subclass
                .clone()
                .map_or(Value::Null, Value::Text),
        ],
    )?;
    for row in rows {
        let Some(Value::Text(id)) = row.get(0) else {
            return Err(invalid_authority());
        };
        if id.parse::<crate::models::MemoryId>().is_err() {
            return Err(invalid_authority());
        }
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
        let start = from.map(timestamp).transpose()?;
        let end = to.map(timestamp).transpose()?;
        if start != source_start || end != source_end {
            continue;
        }
        return Ok(Some(id.clone()));
    }
    Ok(None)
}

fn bound_global_workspace(db: &DbConnection, paths: &GlobalStorePaths) -> Result<String, String> {
    if db
        .needs_migration()
        .map_err(|_| "Could not inspect global store schema".to_owned())?
    {
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
    if !paths
        .database_path
        .try_exists()
        .map_err(|_| "Could not inspect global store".to_owned())?
    {
        return Ok(None);
    }
    let db = DbConnection::open_file_read_only(&paths.database_path)
        .map_err(|_| "Could not open existing global store read-only".to_owned())?;
    let snapshot = ReadSnapshot::begin(&db)
        .map_err(|_| "Could not begin global preview snapshot".to_owned())?;
    let workspace = bound_global_workspace(&db, paths)?;
    let twin = find_twin(&db, &workspace, source, reference)
        .map_err(|_| "Could not verify global duplicate lifecycle".to_owned())?;
    snapshot
        .finish()
        .map_err(|_| "Could not release global preview snapshot".to_owned())?;
    Ok(twin)
}

/// Demotion and feedback address existing records; neither is an initializer.
/// Their dry-run path is strictly read-only. Binding is checked without schema
/// migration or inserting a replacement workspace row.
pub(super) fn open_existing_global(
    paths: &GlobalStorePaths,
    read_only: bool,
) -> Result<(DbConnection, String), String> {
    if !paths
        .database_path
        .try_exists()
        .map_err(|_| "Could not inspect global store".to_owned())?
    {
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

#[cfg(test)]
mod duplicate_compatibility_tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput};

    const SOURCE: &str = "mem_00000000000000000000000071";
    const WORKSPACE: &str = "wsp_00000000000000000000000071";
    const FROM: &str = "2020-01-01T00:00:00Z";
    const TO: &str = "2099-01-01T00:00:00Z";

    struct Fixture {
        _root: tempfile::TempDir,
        source_path: std::path::PathBuf,
        paths: GlobalStorePaths,
        memory: StoredMemory,
        db: DbConnection,
        workspace: String,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let physical = root.path().canonicalize().unwrap();
            let source_path = physical.join("source.db");
            let source = DbConnection::open_file(&source_path).unwrap();
            source.migrate().unwrap();
            source
                .insert_workspace(
                    WORKSPACE,
                    &CreateWorkspaceInput {
                        path: physical.to_string_lossy().into_owned(),
                        name: None,
                    },
                )
                .unwrap();
            source
                .insert_memory(
                    SOURCE,
                    &CreateMemoryInput {
                        workspace_id: WORKSPACE.to_owned(),
                        level: "procedural".to_owned(),
                        kind: "rule".to_owned(),
                        content: "Run cargo fmt before release.".to_owned(),
                        workflow_id: None,
                        confidence: 0.9,
                        utility: 0.5,
                        importance: 0.5,
                        provenance_uri: None,
                        trust_class: "human_explicit".to_owned(),
                        trust_subclass: None,
                        tags: Vec::new(),
                        valid_from: Some(FROM.to_owned()),
                        valid_to: Some(TO.to_owned()),
                    },
                )
                .unwrap();
            let memory = source.get_memory(SOURCE).unwrap().unwrap();
            source.close().unwrap();
            let paths = GlobalStorePaths::from_root(&physical.join("global"));
            let (db, workspace) =
                crate::core::global_store::open_or_create_global_store(&paths).unwrap();
            Self {
                _root: root,
                source_path,
                paths,
                memory,
                db,
                workspace,
            }
        }

        fn options(&self, dry_run: bool) -> PromoteGlobalOptions<'_> {
            PromoteGlobalOptions {
                workspace_database_path: &self.source_path,
                memory_id: SOURCE,
                global_paths: &self.paths,
                global_lane_available: true,
                actor: None,
                dry_run,
            }
        }

        fn publish(&self) -> String {
            super::super::persist_global_promotion(
                &self.db,
                &self.workspace,
                &self.memory,
                None,
                timestamp("2030-01-01T00:00:00Z").unwrap(),
            )
            .unwrap()
            .0
        }

        fn matched(&self, memory: &StoredMemory) -> Option<String> {
            let snapshot = ReadSnapshot::begin(&self.db).unwrap();
            let matched = find_twin(
                &self.db,
                &self.workspace,
                memory,
                timestamp("2030-01-01T00:00:00Z").unwrap(),
            )
            .unwrap();
            snapshot.finish().unwrap();
            matched
        }

        fn update(&self, id: &str, fields: &str) {
            self.db
                .execute_raw(&format!("UPDATE memories SET {fields} WHERE id = '{id}'"))
                .unwrap();
        }
    }

    #[test]
    fn compatible_twins_cannot_replace_authored_lifetime_type_or_trust() {
        let f = Fixture::new();
        let id = f.publish();
        for (changed, restored) in [
            ("valid_from = NULL", "valid_from = '2020-01-01T00:00:00Z'"),
            (
                "valid_from = '2021-01-01T00:00:00Z'",
                "valid_from = '2020-01-01T00:00:00Z'",
            ),
            ("valid_to = NULL", "valid_to = '2099-01-01T00:00:00Z'"),
            (
                "valid_to = '2100-01-01T00:00:00Z'",
                "valid_to = '2099-01-01T00:00:00Z'",
            ),
            (
                "trust_class = 'agent_validated'",
                "trust_class = 'human_explicit'",
            ),
            ("kind = 'note'", "kind = 'rule'"),
        ] {
            f.update(&id, changed);
            assert_eq!(f.matched(&f.memory), None, "{changed}");
            f.update(&id, restored);
            assert_eq!(f.matched(&f.memory), Some(id.clone()));
        }
        // A null stored subclass is not a wildcard for a distinct source's
        // attestation. No fabricated subclass needs to be persisted here.
        let mut differently_attested = f.memory.clone();
        differently_attested.trust_subclass = Some("different-attestation".to_owned());
        assert_eq!(f.matched(&differently_attested), None);
    }

    #[test]
    fn equivalent_timezone_bounds_match_without_bypassing_seal_or_revision_authority() {
        let f = Fixture::new();
        let id = f.publish();
        f.update(
            &id,
            "valid_from = '2019-12-31T19:00:00-05:00', valid_to = '2099-01-01T01:00:00+01:00'",
        );
        assert_eq!(f.matched(&f.memory), Some(id.clone()));
        f.db.insert_memory_seal(
            &id,
            &crate::models::memory_seal_commitment(f.memory.content.as_bytes()),
            FROM,
        )
        .unwrap();
        assert_eq!(f.matched(&f.memory), None);
        assert!(f.db.mark_memory_seal_revealed(&id, FROM).unwrap());
        assert_eq!(f.matched(&f.memory), Some(id.clone()));
        f.update(&id, "superseded_at = '2098-01-01T00:00:00Z'");
        assert_eq!(f.matched(&f.memory), None);
    }

    #[test]
    fn preview_and_real_publication_keep_finite_advice_separate_from_an_unbounded_twin() {
        let f = Fixture::new();
        let original = f.publish();
        f.update(&original, "valid_to = NULL");
        let audits = f.db.count_table_rows("audit_log").unwrap();
        let jobs = f.db.count_table_rows("search_index_jobs").unwrap();
        let preview = super::super::promote_global(&f.options(true)).unwrap();
        assert!(!preview.executed);
        assert!(preview.global_memory_id.is_none());
        assert_eq!(f.db.count_table_rows("audit_log").unwrap(), audits);
        assert_eq!(f.db.count_table_rows("search_index_jobs").unwrap(), jobs);
        let report = super::super::promote_global(&f.options(false)).unwrap();
        assert!(report.executed && !report.already_promoted);
        let bounded = report.global_memory_id.unwrap();
        assert_ne!(bounded, original);
        let copy = f.db.get_memory(&bounded).unwrap().unwrap();
        assert_eq!(copy.valid_from, f.memory.valid_from);
        assert_eq!(copy.valid_to, f.memory.valid_to);
        let retry = super::super::promote_global(&f.options(false)).unwrap();
        assert!(retry.executed && retry.already_promoted);
        assert_eq!(retry.global_memory_id.as_deref(), Some(bounded.as_str()));
        assert!(retry.index_job_id.is_none());
        assert_eq!(f.db.count_table_rows("memories").unwrap(), 2);
    }
}
