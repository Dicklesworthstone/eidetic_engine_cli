//! Additive, audited code-anchor bindings and bounded historical backfill.
//!
//! Memory text, source provenance and revision identity are immutable here.
//! Validated locators are retained separately from their rebuildable reverse
//! projection. Preview and listing open the database read-only, including on
//! older schemas; apply rechecks source eligibility inside the write transaction.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::db::{
    CreateAuditInput, CreateSearchIndexJobInput, DbConnection, DbError, DbOperation,
    SearchIndexJobType, StoredMemory, generate_audit_id,
};
use crate::models::memory_anchor::{
    code_anchor_locator_is_public, extract_backfill_code_anchor_surfaces, memory_anchor_value_hash,
    normalize_code_anchor_value,
};
use crate::models::{
    CreateMemoryAnchorInput, DomainError, ExtractedAnchorSurface, MemoryAnchorKind, MemoryId,
    StoredMemoryAnchor,
};

pub const MEMORY_ANCHORS_SCHEMA_V1: &str = "ee.memory.anchors.v1";
pub const ANCHOR_BACKFILL_SCHEMA_V1: &str = "ee.index.backfill_anchors.v1";
/// A dense source memory cannot turn a bounded repair page into an unbounded
/// number of anchor writes. Oversized candidate sets are skipped in full.
pub const MAX_BACKFILL_ANCHORS_PER_MEMORY: usize = 256;

#[derive(Clone, Debug)]
pub struct AttachAnchorsOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: &'a Path,
    pub memory_id: &'a str,
    pub paths: &'a [String],
    pub symbols: &'a [String],
    pub dry_run: bool,
    pub actor: Option<&'a str>,
}

#[derive(Clone, Debug)]
pub struct BackfillAnchorsOptions<'a> {
    pub workspace_path: &'a Path,
    pub database_path: &'a Path,
    pub apply: bool,
    /// Maximum current memory rows examined, not just successful proposals.
    pub limit: usize,
    pub after: Option<&'a str>,
    pub actor: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnchorDescriptor {
    pub anchor_kind: String,
    pub anchor_value_hash: String,
    pub redacted_anchor_value: String,
    pub source: String,
    /// Exact normalized locator only when it passes recall's public screen.
    /// Canonical hashes remain available when the locator cannot be shown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
}

impl AnchorDescriptor {
    fn from_surface(surface: &ExtractedAnchorSurface) -> Self {
        let anchor = &surface.anchor;
        let value = &surface.normalized_value;
        // Share recall's public-locator boundary and verify its hash identity.
        // A redacted placeholder must never masquerade as a proposed locator.
        let public = memory_anchor_value_hash(anchor.anchor_kind, value)
            == anchor.anchor_value_hash
            && code_anchor_locator_is_public(value);
        Self {
            anchor_kind: anchor.anchor_kind.as_str().to_owned(),
            anchor_value_hash: anchor.anchor_value_hash.clone(),
            redacted_anchor_value: anchor.redacted_anchor_value.clone(),
            source: anchor.source.as_str().to_owned(),
            locator: public.then(|| value.clone()),
        }
    }
    fn from_stored(anchor: &StoredMemoryAnchor) -> Result<Self, DomainError> {
        let Some(digest) = anchor.anchor_value_hash.strip_prefix("blake3:") else {
            return Err(usage(
                "Stored code-anchor identity is malformed; run ee index rebuild.",
            ));
        };
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|ch| ch.is_ascii_digit() || matches!(ch, b'a'..=b'f'))
        {
            return Err(usage(
                "Stored code-anchor identity is malformed; run ee index rebuild.",
            ));
        }
        Ok(Self {
            anchor_kind: anchor.anchor_kind.as_str().to_owned(),
            anchor_value_hash: anchor.anchor_value_hash.clone(),
            redacted_anchor_value: format!(
                "{}:blake3:{}",
                anchor.anchor_kind.as_str(),
                &digest[..12]
            ),
            source: anchor.source.as_str().to_owned(),
            locator: None,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnchorAttachReport {
    pub schema: &'static str,
    pub version: &'static str,
    pub memory_id: String,
    pub workspace_id: String,
    pub status: &'static str,
    pub dry_run: bool,
    pub persisted: bool,
    pub changed: bool,
    pub anchors: Vec<AnchorDescriptor>,
    pub added_anchor_count: usize,
    pub audit_id: Option<String>,
    pub index_job_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnchorBackfillSkippedMemory {
    pub memory_id: String,
    pub reason: &'static str,
    pub candidate_anchor_count: usize,
    pub anchor_limit: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnchorBackfillReport {
    pub schema: &'static str,
    pub version: &'static str,
    pub dry_run: bool,
    pub scanned_memory_count: usize,
    pub proposed_memory_count: usize,
    pub proposed_anchor_count: usize,
    pub applied_memory_count: usize,
    pub applied_anchor_count: usize,
    pub skipped_memory_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped_memories: Vec<AnchorBackfillSkippedMemory>,
    pub next_after: Option<String>,
    pub proposals: Vec<AnchorAttachReport>,
}

fn storage(_: DbError) -> DomainError {
    // SQL errors can carry values. Storage diagnostics never echo locators,
    // source contents or host-specific database paths.
    DomainError::Storage {
        message: "Code-anchor storage operation failed.".to_owned(),
        repair: Some("ee doctor --json".to_owned()),
    }
}

fn usage(message: &str) -> DomainError {
    DomainError::Usage {
        message: message.to_owned(),
        repair: Some("ee memory anchors --help; ee index backfill-anchors --help".to_owned()),
    }
}

fn validate_actor(actor: Option<&str>) -> Result<(), DomainError> {
    if actor.is_some_and(|value| {
        value.trim().is_empty()
            || value.len() > 256
            || crate::policy::redact_public_replay_text(value).redacted
    }) {
        return Err(usage(
            "Actor must be a nonempty, bounded label without secret material.",
        ));
    }
    Ok(())
}

fn open(database_path: &Path, write: bool) -> Result<DbConnection, DomainError> {
    super::ensure_addressed_database_exists(database_path)?;
    let db = if write {
        DbConnection::open_file(database_path)
    } else {
        DbConnection::open_file_read_only(database_path)
    }
    .map_err(storage)?;
    if db.schema_version().map_err(storage)?.unwrap_or(0) < 123 {
        return Err(DomainError::MigrationRequired {
            message: "Code-anchor management requires the current memory lifecycle schema."
                .to_owned(),
            repair: Some("ee migrate run --workspace .".to_owned()),
        });
    }
    Ok(db)
}

fn read_snapshot<T>(
    db: &DbConnection,
    read: impl FnOnce() -> Result<T, DomainError>,
) -> Result<T, DomainError> {
    db.begin_read_snapshot().map_err(storage)?;
    struct Snapshot<'a> {
        db: &'a DbConnection,
        active: bool,
    }
    impl Drop for Snapshot<'_> {
        fn drop(&mut self) {
            if self.active {
                let _ = self.db.rollback_read_snapshot();
            }
        }
    }
    let mut snapshot = Snapshot { db, active: true };
    let value = read()?;
    db.commit_read_snapshot().map_err(storage)?;
    snapshot.active = false;
    Ok(value)
}

fn memory_in_workspace(
    db: &DbConnection,
    workspace: &str,
    id: &str,
) -> Result<StoredMemory, DomainError> {
    db.get_memory(id)
        .map_err(storage)?
        .filter(|memory| memory.workspace_id == workspace)
        .ok_or_else(|| DomainError::NotFound {
            resource: "memory".to_owned(),
            id: id.to_owned(),
            repair: Some("Use a memory ID from the addressed workspace.".to_owned()),
        })
}

fn workspace_binding(
    db: &DbConnection,
    root: &Path,
    database: &Path,
) -> Result<String, DomainError> {
    super::workspace::addressed_workspace_row(db, root, database)?
        .map(|workspace| workspace.id)
        .ok_or_else(|| DomainError::NotFound {
            resource: "workspace".to_owned(),
            id: "addressed workspace".to_owned(),
            repair: Some("Select the workspace already registered in this database.".to_owned()),
        })
}

fn require_eligible(db: &DbConnection, memory: &StoredMemory) -> Result<(), DomainError> {
    if eligible(db, memory).map_err(storage)? {
        Ok(())
    } else {
        Err(DomainError::PolicyDenied {
            message: "Cannot attach anchors to an inactive, sealed or review-held memory."
                .to_owned(),
            repair: Some(
                "Select a current, revealed memory revision that is not held for review."
                    .to_owned(),
            ),
        })
    }
}

fn eligible(db: &DbConnection, memory: &StoredMemory) -> Result<bool, DbError> {
    if memory.tombstoned_at.is_some() || db.get_memory_superseded_at(&memory.id)?.is_some() {
        return Ok(false);
    }
    let now = chrono::Utc::now();
    for raw in [
        Some(memory.created_at.as_str()),
        Some(memory.updated_at.as_str()),
        memory.valid_from.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let Ok(at) = chrono::DateTime::parse_from_rfc3339(raw) else {
            return Ok(false);
        };
        if at > now {
            return Ok(false);
        }
    }
    if let Some(raw) = memory.valid_to.as_deref() {
        let Ok(at) = chrono::DateTime::parse_from_rfc3339(raw) else {
            return Ok(false);
        };
        if at < now {
            return Ok(false);
        }
    }
    if !super::memory_lifecycle::pending_memory_review_ids(
        db,
        &memory.workspace_id,
        &[memory.id.as_str()],
    )?
    .is_empty()
    {
        return Ok(false);
    }
    if let Some(seal) = super::memory::resolve_memory_seal_lineage(db, memory)? {
        if seal.is_sealed()
            || seal.reveal_verified != Some(true)
            || crate::models::validate_attestation_seal_fields(
                &seal.content_commitment,
                &seal.sealed_at,
                seal.revealed_at.as_deref(),
                seal.reveal_verified,
            )
            .is_err()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn validate_surfaces(
    options: &AttachAnchorsOptions<'_>,
) -> Result<Vec<ExtractedAnchorSurface>, DomainError> {
    let mut unique = BTreeMap::new();
    let private_locator = || DomainError::PolicyDenied {
        message: "Code anchors cannot contain private paths or secret material.".to_owned(),
        repair: Some(
            "Use a public workspace-relative code path or identifier without credentials."
                .to_owned(),
        ),
    };
    for (kind, values) in [
        (MemoryAnchorKind::Path, options.paths),
        (MemoryAnchorKind::Symbol, options.symbols),
    ] {
        for raw in values {
            if crate::policy::redact_public_replay_text(raw).redacted {
                return Err(private_locator());
            }
            let Some(surface) = CreateMemoryAnchorInput::from_code_surface(
                options.memory_id,
                kind,
                raw,
                options.workspace_path,
                "memory.anchors",
            ) else {
                return Err(usage(
                    "Invalid code anchor: use a workspace-relative path without traversal or glob syntax, or an identifier such as Store::open.",
                ));
            };
            // Screen the normalized identity: a leading ./ must not make a
            // relative app/file path look like an absolute /app/ private path.
            if !code_anchor_locator_is_public(&surface.normalized_value) {
                return Err(private_locator());
            }
            unique.insert((kind, surface.anchor.anchor_value_hash.clone()), surface);
        }
    }
    Ok(unique.into_values().collect())
}

fn plan(
    db: &DbConnection,
    memory: &StoredMemory,
    surfaces: &[ExtractedAnchorSurface],
    dry_run: bool,
) -> Result<AnchorAttachReport, DbError> {
    let existing = db.list_memory_code_anchor_sources(&memory.id)?;
    let canonical = db.list_memory_anchors(&memory.id)?;
    let mut added = 0;
    let mut changed = false;
    for surface in surfaces {
        let anchor = &surface.anchor;
        let matches = |other: &CreateMemoryAnchorInput| {
            other.anchor_kind == anchor.anchor_kind
                && other.anchor_value_hash == anchor.anchor_value_hash
        };
        if !existing.iter().any(|other| matches(&other.anchor)) {
            added += 1;
            changed = true;
        }
        if !canonical.iter().any(|other| {
            other.anchor_kind == anchor.anchor_kind
                && other.anchor_value_hash == anchor.anchor_value_hash
        }) || !db.has_memory_code_anchor_projection(&memory.workspace_id, surface)?
        {
            changed = true;
        }
    }
    Ok(AnchorAttachReport {
        schema: MEMORY_ANCHORS_SCHEMA_V1,
        version: env!("CARGO_PKG_VERSION"),
        memory_id: memory.id.clone(),
        workspace_id: memory.workspace_id.clone(),
        status: if changed { "would_attach" } else { "unchanged" },
        dry_run,
        persisted: false,
        changed,
        anchors: surfaces
            .iter()
            .map(AnchorDescriptor::from_surface)
            .collect(),
        added_anchor_count: added,
        audit_id: None,
        index_job_id: None,
    })
}

fn apply_in_transaction(
    db: &DbConnection,
    memory: &StoredMemory,
    surfaces: &[ExtractedAnchorSurface],
    workspace_path: &Path,
    actor: Option<&str>,
    operation: &str,
) -> Result<AnchorAttachReport, DbError> {
    // Callers hold the write-owner fence. Check source bytes, lineage and
    // workspace again, rather than applying a proposal over a changed memory.
    let live = db
        .get_memory(&memory.id)?
        .filter(|live| live == memory)
        .ok_or_else(|| DbError::MalformedRow {
            operation: DbOperation::Execute,
            message: "Anchor source changed during planning; retry.".to_owned(),
        })?;
    if !eligible(db, &live)? {
        return Err(DbError::MalformedRow {
            operation: DbOperation::Execute,
            message: "Anchor source is no longer eligible; retry.".to_owned(),
        });
    }
    for surface in surfaces {
        if normalize_code_anchor_value(
            surface.anchor.anchor_kind,
            &surface.normalized_value,
            Some(workspace_path),
        )
        .is_none_or(|value| value != surface.normalized_value)
        {
            return Err(DbError::MalformedRow {
                operation: DbOperation::Execute,
                message: "Code-anchor workspace path changed during planning; retry.".to_owned(),
            });
        }
    }
    let mut report = plan(db, &live, surfaces, false)?;
    if !report.changed {
        return Ok(report);
    }
    report.added_anchor_count =
        db.attach_memory_code_anchor_sources(&live.workspace_id, &live.id, surfaces)?;
    let audit_id = generate_audit_id();
    let id = MemoryId::now().to_string();
    let index_job_id = format!("sidx_{}", id.trim_start_matches("mem_"));
    let audit_anchors = report
        .anchors
        .iter()
        .map(|anchor| {
            serde_json::json!({
                "anchorKind": anchor.anchor_kind,
                "anchorValueHash": anchor.anchor_value_hash,
                "redactedAnchorValue": anchor.redacted_anchor_value,
                "source": anchor.source,
            })
        })
        .collect::<Vec<_>>();
    db.insert_audit(&audit_id, &CreateAuditInput {
        workspace_id: Some(live.workspace_id.clone()), actor: Some(actor.unwrap_or(operation).to_owned()),
        action: "memory.anchors.attach".to_owned(), target_type: Some("memory".to_owned()), target_id: Some(live.id.clone()),
        details: Some(serde_json::json!({"operation": operation, "anchors": audit_anchors,
            "addedAnchorCount": report.added_anchor_count, "contentChanged": false, "provenanceChanged": false}).to_string()),
    })?;
    db.insert_search_index_job(
        &index_job_id,
        &CreateSearchIndexJobInput {
            workspace_id: live.workspace_id.clone(),
            job_type: SearchIndexJobType::SingleDocument,
            document_source: Some("memory".to_owned()),
            document_id: Some(live.id.clone()),
            documents_total: 1,
        },
    )?;
    report.status = "attached";
    report.persisted = true;
    report.audit_id = Some(audit_id);
    report.index_job_id = Some(index_job_id);
    Ok(report)
}

/// List or attach code anchors without creating a replacement memory revision.
pub fn attach_anchors(
    options: &AttachAnchorsOptions<'_>,
) -> Result<AnchorAttachReport, DomainError> {
    validate_actor(options.actor)?;
    if options.memory_id.parse::<MemoryId>().is_err() {
        return Err(usage("A complete memory ID is required."));
    }
    let surfaces = validate_surfaces(options)?;
    let listing = surfaces.is_empty();
    // Resolve scope and source eligibility read-only before any migration or
    // writer open. A wrong --database must never mutate somebody else's store.
    let db = open(options.database_path, false)?;
    let (workspace, memory, report) = read_snapshot(&db, || {
        let workspace = workspace_binding(&db, options.workspace_path, options.database_path)?;
        let memory = memory_in_workspace(&db, &workspace, options.memory_id)?;
        if listing {
            let mut anchors = BTreeMap::new();
            for anchor in db.list_memory_anchors(options.memory_id).map_err(storage)? {
                if matches!(
                    anchor.anchor_kind,
                    MemoryAnchorKind::Path | MemoryAnchorKind::Symbol
                ) {
                    anchors.insert(
                        (anchor.anchor_kind, anchor.anchor_value_hash.clone()),
                        AnchorDescriptor::from_stored(&anchor)?,
                    );
                }
            }
            // A canonical row stores only the identity. Recover a display
            // locator from screened current content only when its exact hash
            // matches an anchor that is already attached, never list an
            // uncreated backfill candidate as a stored anchor.
            for surface in backfill_surfaces(&memory, options.workspace_path) {
                if let Some(anchor) = anchors.get_mut(&(
                    surface.anchor.anchor_kind,
                    surface.anchor.anchor_value_hash.clone(),
                )) {
                    anchor.locator = AnchorDescriptor::from_surface(&surface).locator;
                }
            }
            for source in db
                .list_memory_code_anchor_sources(options.memory_id)
                .map_err(storage)?
            {
                let descriptor = AnchorDescriptor::from_surface(&source);
                anchors
                    .entry((
                        source.anchor.anchor_kind,
                        source.anchor.anchor_value_hash.clone(),
                    ))
                    .and_modify(|anchor| {
                        // Preserve canonical capture provenance while making
                        // the durable locator reviewable, including after the
                        // body or its derived projection no longer contains it.
                        anchor.locator = descriptor.locator.clone();
                    })
                    .or_insert(descriptor);
            }
            let report = AnchorAttachReport {
                schema: MEMORY_ANCHORS_SCHEMA_V1,
                version: env!("CARGO_PKG_VERSION"),
                memory_id: memory.id.clone(),
                workspace_id: workspace.clone(),
                status: "listed",
                dry_run: options.dry_run,
                persisted: false,
                changed: false,
                anchors: anchors.into_values().collect(),
                added_anchor_count: 0,
                audit_id: None,
                index_job_id: None,
            };
            return Ok((workspace, memory, report));
        }
        require_eligible(&db, &memory)?;
        let report = plan(&db, &memory, &surfaces, options.dry_run).map_err(storage)?;
        Ok((workspace, memory, report))
    })?;
    db.close().map_err(storage)?;
    if listing || options.dry_run || !report.changed {
        return Ok(report);
    }
    let db = open(options.database_path, true)?;
    if workspace_binding(&db, options.workspace_path, options.database_path)? != workspace {
        return Err(usage("Workspace binding changed during planning; retry."));
    }
    db.migrate().map_err(storage)?;
    db.with_transaction(|| {
        apply_in_transaction(
            &db,
            &memory,
            &surfaces,
            options.workspace_path,
            options.actor,
            "ee memory anchors",
        )
    })
    .map_err(storage)
}

fn backfill_surfaces(memory: &StoredMemory, workspace_path: &Path) -> Vec<ExtractedAnchorSurface> {
    // Contextual detection must happen before extraction: a string that looks
    // like a path may actually be a password value in the original sentence.
    let screened = crate::policy::redact_public_replay_body(&memory.content);
    extract_backfill_code_anchor_surfaces(&memory.id, &screened.content, Some(workspace_path))
        .into_iter()
        .filter(|surface| {
            code_anchor_locator_is_public(&surface.normalized_value)
                && normalize_code_anchor_value(
                    surface.anchor.anchor_kind,
                    &surface.normalized_value,
                    Some(workspace_path),
                )
                .is_some_and(|normalized| normalized == surface.normalized_value)
        })
        .collect()
}

/// Preview a deterministic bounded page, or atomically apply that page.
/// No files are opened from anchor values and no provenance URI is inferred.
pub fn backfill_anchors(
    options: &BackfillAnchorsOptions<'_>,
) -> Result<AnchorBackfillReport, DomainError> {
    validate_actor(options.actor)?;
    if options.limit == 0 || options.limit > 10_000 {
        return Err(usage("Backfill --limit must be between 1 and 10000."));
    }
    if options
        .after
        .is_some_and(|id| id.parse::<MemoryId>().is_err())
    {
        return Err(usage(
            "Backfill --after must be a memory ID from a previous page.",
        ));
    }
    let db = open(options.database_path, false)?;
    let workspace = read_snapshot(&db, || {
        workspace_binding(&db, options.workspace_path, options.database_path)
    })?;
    let db = if options.apply {
        db.close().map_err(storage)?;
        let writer = open(options.database_path, true)?;
        if workspace_binding(&writer, options.workspace_path, options.database_path)? != workspace {
            return Err(usage("Workspace binding changed during planning; retry."));
        }
        writer.migrate().map_err(storage)?;
        writer
    } else {
        db
    };
    let run = || -> Result<AnchorBackfillReport, DbError> {
        let mut memories =
            db.memory_code_anchor_backfill_page(&workspace, options.after, options.limit + 1)?;
        let more = memories.len() > options.limit;
        memories.truncate(options.limit);
        let next_after = more
            .then(|| memories.last().map(|memory| memory.id.clone()))
            .flatten();
        let scanned = memories.len();
        let mut proposals = Vec::new();
        let mut skipped_memories = Vec::new();
        for memory in &memories {
            if !eligible(&db, memory)? {
                continue;
            }
            let surfaces = backfill_surfaces(memory, options.workspace_path);
            if surfaces.len() > MAX_BACKFILL_ANCHORS_PER_MEMORY {
                skipped_memories.push(AnchorBackfillSkippedMemory {
                    memory_id: memory.id.clone(),
                    reason: "anchor_limit_exceeded",
                    candidate_anchor_count: surfaces.len(),
                    anchor_limit: MAX_BACKFILL_ANCHORS_PER_MEMORY,
                });
                continue;
            }
            if surfaces.is_empty() {
                continue;
            }
            let preview = plan(&db, memory, &surfaces, !options.apply)?;
            if !preview.changed {
                continue;
            }
            proposals.push(if options.apply {
                apply_in_transaction(
                    &db,
                    memory,
                    &surfaces,
                    options.workspace_path,
                    options.actor,
                    "ee index backfill-anchors",
                )?
            } else {
                preview
            });
        }
        let count = proposals
            .iter()
            .map(|proposal| proposal.added_anchor_count)
            .sum();
        Ok(AnchorBackfillReport {
            schema: ANCHOR_BACKFILL_SCHEMA_V1,
            version: env!("CARGO_PKG_VERSION"),
            dry_run: !options.apply,
            scanned_memory_count: scanned,
            proposed_memory_count: proposals.len(),
            proposed_anchor_count: count,
            applied_memory_count: if options.apply { proposals.len() } else { 0 },
            applied_anchor_count: if options.apply { count } else { 0 },
            skipped_memory_count: scanned - proposals.len(),
            skipped_memories,
            next_after,
            proposals,
        })
    };
    if options.apply {
        db.with_transaction(run).map_err(storage)
    } else {
        read_snapshot(&db, || run().map_err(storage))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput};
    use crate::models::{MemoryAnchorFreshnessState, MemoryAnchorSource};

    const WORKSPACE: &str = "wsp_00000000000000000000000841";
    const MEMORY: &str = "mem_00000000000000000000000841";

    fn fixture(root: &Path) -> (DbConnection, StoredMemory) {
        let db = DbConnection::open_memory().unwrap();
        db.migrate().unwrap();
        db.insert_workspace(
            WORKSPACE,
            &CreateWorkspaceInput {
                path: root.to_string_lossy().into_owned(),
                name: Some("anchor-tests".to_owned()),
            },
        )
        .unwrap();
        db.insert_memory(
            MEMORY,
            &CreateMemoryInput {
                workspace_id: WORKSPACE.to_owned(),
                level: "semantic".to_owned(),
                kind: "fact".to_owned(),
                content: "Original observation with no code locator.".to_owned(),
                workflow_id: None,
                confidence: 0.8,
                utility: 0.7,
                importance: 0.6,
                provenance_uri: Some("manual://source-observation".to_owned()),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
        )
        .unwrap();
        let memory = db.get_memory(MEMORY).unwrap().unwrap();
        (db, memory)
    }

    fn surface(root: &Path) -> ExtractedAnchorSurface {
        CreateMemoryAnchorInput::from_code_surface(
            MEMORY,
            MemoryAnchorKind::Path,
            "src/transaction.rs",
            root,
            "memory.anchors",
        )
        .unwrap()
    }

    #[test]
    fn preview_identifies_public_locators_without_exposing_memory_source() {
        let root = tempfile::tempdir().unwrap();
        let (db, memory) = fixture(root.path());
        let path = surface(root.path());
        let symbol = CreateMemoryAnchorInput::from_code_surface(
            MEMORY,
            MemoryAnchorKind::Symbol,
            "Store::commit()",
            root.path(),
            "memory.anchors",
        )
        .unwrap();
        let preview = plan(&db, &memory, &[path.clone(), symbol], true).unwrap();
        let json = serde_json::to_value(&preview).unwrap();
        assert_eq!(json["status"], "would_attach");
        assert_eq!(json["anchors"][0]["locator"], "src/transaction.rs");
        assert_eq!(json["anchors"][1]["locator"], "Store::commit");
        assert_eq!(
            json["anchors"][0]["anchorValueHash"],
            path.anchor.anchor_value_hash
        );
        assert!(
            json["anchors"][0]["redactedAnchorValue"]
                .as_str()
                .unwrap()
                .starts_with("path:blake3:")
        );
        let rendered = json.to_string();
        assert!(!rendered.contains(&memory.content));
        assert!(!rendered.contains(memory.provenance_uri.as_deref().unwrap()));
        assert!(db.list_memory_anchors(MEMORY).unwrap().is_empty());
        assert!(
            db.list_memory_code_anchor_sources(MEMORY)
                .unwrap()
                .is_empty()
        );
        assert_eq!(db.get_memory(MEMORY).unwrap().unwrap(), memory);
    }

    #[test]
    fn attachment_audit_retains_hashes_without_copying_readable_locators() {
        let root = tempfile::tempdir().unwrap();
        let (db, memory) = fixture(root.path());
        let surface = surface(root.path());
        let report = db
            .with_transaction(|| {
                apply_in_transaction(
                    &db,
                    &memory,
                    std::slice::from_ref(&surface),
                    root.path(),
                    None,
                    "ee memory anchors",
                )
            })
            .unwrap();
        assert_eq!(
            report.anchors[0].locator.as_deref(),
            Some("src/transaction.rs")
        );
        let audit = db
            .get_audit(report.audit_id.as_deref().unwrap())
            .unwrap()
            .unwrap();
        let details = audit.details.as_deref().unwrap();
        assert!(!details.contains("src/transaction.rs"));
        assert!(!details.contains("locator"));
        let identity: serde_json::Value = serde_json::from_str(details).unwrap();
        assert_eq!(
            identity["anchors"][0]["anchorValueHash"],
            surface.anchor.anchor_value_hash
        );
        assert_eq!(db.get_memory(MEMORY).unwrap().unwrap(), memory);
    }

    #[test]
    fn descriptor_omits_private_locators_and_preserves_hash_identity() {
        let root = tempfile::tempdir().unwrap();
        let mut surface = surface(root.path());
        for private in [
            format!("src/ghp_{}.rs", "a".repeat(36)),
            "/home/operator/private.rs".to_owned(),
            "src/home/operator/private.rs".to_owned(),
        ] {
            // Exercise the display boundary independently of ingress: even a
            // hash-consistent reconstructed surface must not publish a private
            // value. A mismatched hash must not mask a broken redaction check.
            surface.normalized_value = private.clone();
            let hash = memory_anchor_value_hash(surface.anchor.anchor_kind, &private);
            surface.anchor.anchor_value_hash = hash.clone();
            surface.anchor.redacted_anchor_value = format!(
                "path:blake3:{}",
                &hash.strip_prefix("blake3:").unwrap()[..12]
            );
            let descriptor = AnchorDescriptor::from_surface(&surface);
            assert_eq!(descriptor.anchor_value_hash, hash);
            assert!(descriptor.locator.is_none());
            let json = serde_json::to_value(descriptor).unwrap();
            assert!(json.get("locator").is_none());
            assert!(!json.to_string().contains(&private));
        }
    }

    #[test]
    fn descriptor_omits_unbound_public_locator_when_surface_hash_mismatches() {
        let root = tempfile::tempdir().unwrap();
        let mut surface = surface(root.path());
        let hash = surface.anchor.anchor_value_hash.clone();
        surface.normalized_value = "src/substituted.rs".to_owned();
        let descriptor = AnchorDescriptor::from_surface(&surface);
        assert_eq!(descriptor.anchor_value_hash, hash);
        assert!(descriptor.locator.is_none());
        assert!(
            !serde_json::to_string(&descriptor)
                .unwrap()
                .contains("src/substituted.rs")
        );
    }

    #[test]
    fn private_code_locators_are_rejected_before_database_access() {
        let root = tempfile::tempdir().unwrap();
        let database_path = root.path().join("absent.db");
        for path in ["src/app/page.tsx", "src/home/a.rs"] {
            let paths = vec![path.to_owned()];
            let result = attach_anchors(&AttachAnchorsOptions {
                workspace_path: root.path(),
                database_path: &database_path,
                memory_id: MEMORY,
                paths: &paths,
                symbols: &[],
                dry_run: false,
                actor: None,
            });
            assert!(matches!(result, Err(DomainError::PolicyDenied { .. })));
            assert!(!database_path.exists());
        }
        let paths = vec!["./app/page.tsx".to_owned()];
        let normalized = validate_surfaces(&AttachAnchorsOptions {
            workspace_path: root.path(),
            database_path: &database_path,
            memory_id: MEMORY,
            paths: &paths,
            symbols: &[],
            dry_run: true,
            actor: None,
        })
        .unwrap();
        assert_eq!(normalized[0].normalized_value, "app/page.tsx");
    }

    #[test]
    fn failed_anchor_audit_rolls_back_sources_projection_and_generation() {
        let root = tempfile::tempdir().unwrap();
        let (db, memory) = fixture(root.path());
        let generation = db.get_workspace_generation(WORKSPACE).unwrap();
        db.execute_raw("CREATE TRIGGER reject_anchor_audit BEFORE INSERT ON audit_log WHEN NEW.action = 'memory.anchors.attach' BEGIN SELECT RAISE(ABORT, 'test audit unavailable'); END;").unwrap();
        let result = db.with_transaction(|| {
            apply_in_transaction(
                &db,
                &memory,
                &[surface(root.path())],
                root.path(),
                None,
                "ee memory anchors",
            )
        });
        assert!(result.is_err());
        assert!(
            db.list_memory_code_anchor_sources(MEMORY)
                .unwrap()
                .is_empty()
        );
        assert!(db.list_memory_anchors(MEMORY).unwrap().is_empty());
        assert_eq!(db.memory_anchor_index_generation(WORKSPACE).unwrap(), None);
        assert_eq!(db.get_workspace_generation(WORKSPACE).unwrap(), generation);
        assert!(
            db.list_search_index_jobs(WORKSPACE, None)
                .unwrap()
                .is_empty()
        );
        assert_eq!(db.get_memory(MEMORY).unwrap().unwrap(), memory);
    }

    #[test]
    fn surviving_source_binding_is_available_to_search_metadata_before_rebuild() {
        let root = tempfile::tempdir().unwrap();
        let (db, memory) = fixture(root.path());
        let surface = surface(root.path());
        db.with_transaction(|| {
            apply_in_transaction(
                &db,
                &memory,
                std::slice::from_ref(&surface),
                root.path(),
                None,
                "ee memory anchors",
            )
        })
        .unwrap();
        db.execute_raw("DELETE FROM memory_anchors; DELETE FROM memory_anchor_index;")
            .unwrap();
        let metadata = db
            .memory_index_metadata_in_current_snapshot(&[MEMORY])
            .unwrap();
        assert_eq!(metadata.by_memory_id[MEMORY].anchors.len(), 1);
        assert_eq!(
            metadata.by_memory_id[MEMORY].anchors[0].anchor_value_hash,
            surface.anchor.anchor_value_hash
        );
        assert!(
            db.list_memory_anchors(MEMORY).unwrap().is_empty(),
            "metadata projection is read-only"
        );
        db.with_transaction(|| {
            db.refresh_memory_anchor_index_for_memory(WORKSPACE, MEMORY, &memory.content)
        })
        .unwrap();
        let recalled = crate::core::recall::run_recall(
            &db,
            WORKSPACE,
            &crate::core::recall::RecallQuery {
                paths: vec!["src/transaction.rs".to_owned()],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(recalled.items.len(), 1);
        assert_eq!(recalled.items[0].memory_id, MEMORY);
        assert_eq!(db.list_memory_anchors(MEMORY).unwrap().len(), 1);
        assert_eq!(db.get_memory(MEMORY).unwrap().unwrap(), memory);
    }

    #[test]
    fn source_provenance_survives_canonical_freshness_updates() {
        let root = tempfile::tempdir().unwrap();
        let (db, memory) = fixture(root.path());
        let surface = surface(root.path());
        db.with_transaction(|| {
            apply_in_transaction(
                &db,
                &memory,
                std::slice::from_ref(&surface),
                root.path(),
                None,
                "ee memory anchors",
            )
        })
        .unwrap();
        let mut canonical = surface.anchor.clone();
        canonical.generation = 100;
        canonical.freshness_state = MemoryAnchorFreshnessState::Stale;
        canonical.source = MemoryAnchorSource::IndexRebuild;
        canonical.provenance = "index_rebuild".to_owned();
        db.upsert_memory_anchors(&[canonical]).unwrap();
        let sources = db.list_memory_code_anchor_sources(MEMORY).unwrap();
        assert_eq!(sources[0].anchor.source, MemoryAnchorSource::Explicit);
        assert_eq!(sources[0].anchor.provenance, "memory.anchors");
        assert_eq!(
            sources[0].anchor.freshness_state,
            MemoryAnchorFreshnessState::Stale
        );
        assert_eq!(sources[0].anchor.generation, 100);
    }

    #[test]
    fn backfill_never_turns_secret_context_into_a_code_locator() {
        let root = tempfile::tempdir().unwrap();
        let (_db, mut memory) = fixture(root.path());
        memory.content = "Review src/visible.rs before changing it.".to_owned();
        assert_eq!(backfill_surfaces(&memory, root.path()).len(), 1);
        memory.content = "password: src/private-value.rs".to_owned();
        assert!(backfill_surfaces(&memory, root.path()).is_empty());
        memory.content = format!("See `src/ghp_{}.rs`", "a".repeat(36));
        assert!(backfill_surfaces(&memory, root.path()).is_empty());
    }

    #[test]
    fn listing_reconstructs_redacted_labels_instead_of_trusting_stored_text() {
        let root = tempfile::tempdir().unwrap();
        let (db, memory) = fixture(root.path());
        db.with_transaction(|| {
            apply_in_transaction(
                &db,
                &memory,
                &[surface(root.path())],
                root.path(),
                None,
                "ee memory anchors",
            )
        })
        .unwrap();
        let mut canonical = db.list_memory_anchors(MEMORY).unwrap().remove(0);
        canonical.redacted_anchor_value = format!("ghp_{}", "a".repeat(36));
        let descriptor = AnchorDescriptor::from_stored(&canonical).unwrap();
        assert!(!descriptor.redacted_anchor_value.contains("ghp_"));
        assert!(descriptor.redacted_anchor_value.starts_with("path:blake3:"));
        canonical.anchor_value_hash = format!("blake3:{}", "x".repeat(64));
        assert!(AnchorDescriptor::from_stored(&canonical).is_err());
    }

    #[test]
    fn storage_boundary_rejects_credential_shaped_bindings() {
        let root = tempfile::tempdir().unwrap();
        let (db, memory) = fixture(root.path());
        let unsafe_surface = CreateMemoryAnchorInput::from_stored_code_surface(
            MEMORY,
            MemoryAnchorKind::Path,
            &format!("src/ghp_{}.rs", "a".repeat(36)),
            MemoryAnchorSource::Explicit,
            "memory.anchors",
        )
        .unwrap();
        assert!(
            db.with_transaction(|| db.attach_memory_code_anchor_sources(
                WORKSPACE,
                MEMORY,
                &[surface(root.path()), unsafe_surface]
            ))
            .is_err()
        );
        assert!(
            db.list_memory_code_anchor_sources(MEMORY)
                .unwrap()
                .is_empty()
        );
        assert!(db.list_memory_anchors(MEMORY).unwrap().is_empty());
        assert_eq!(db.get_memory(MEMORY).unwrap().unwrap(), memory);
    }

    #[test]
    fn storage_boundary_rejects_raw_labels_and_rederives_capture_hashes() {
        let root = tempfile::tempdir().unwrap();
        let (db, _) = fixture(root.path());
        let original = surface(root.path());
        let mut unsafe_label = original.clone();
        unsafe_label.anchor.redacted_anchor_value = format!("ghp_{}", "a".repeat(36));
        assert!(
            db.attach_memory_code_anchor_sources(
                WORKSPACE,
                MEMORY,
                &[original.clone(), unsafe_label]
            )
            .is_err()
        );
        assert!(
            db.list_memory_code_anchor_sources(MEMORY)
                .unwrap()
                .is_empty()
        );
        let mut wrong_source = original.clone();
        wrong_source.anchor.source = MemoryAnchorSource::Remember;
        assert!(
            db.attach_memory_code_anchor_sources(WORKSPACE, MEMORY, &[wrong_source])
                .is_err()
        );
        let mut unsafe_capture = original.clone();
        unsafe_capture.anchor.captured_span_hash = format!("ghp_{}", "a".repeat(36));
        db.with_transaction(|| {
            db.attach_memory_code_anchor_sources(WORKSPACE, MEMORY, &[unsafe_capture])
        })
        .unwrap();
        let stored = db.list_memory_anchors(MEMORY).unwrap().remove(0);
        assert_eq!(
            stored.redacted_anchor_value,
            original.anchor.redacted_anchor_value
        );
        assert_eq!(
            stored.captured_span_hash,
            original.anchor.captured_span_hash
        );
    }

    #[test]
    fn reattachment_repairs_a_misbound_projection_without_rewriting_the_anchor() {
        let root = tempfile::tempdir().unwrap();
        let (db, memory) = fixture(root.path());
        let surface = surface(root.path());
        db.with_transaction(|| {
            apply_in_transaction(
                &db,
                &memory,
                std::slice::from_ref(&surface),
                root.path(),
                None,
                "ee memory anchors",
            )
        })
        .unwrap();
        let before = db.list_memory_anchors(MEMORY).unwrap();
        db.execute_raw(
            "UPDATE memory_anchor_index SET normalized_path = 'src/wrong.rs', generation = 999999;",
        )
        .unwrap();
        let repaired = db
            .with_transaction(|| {
                apply_in_transaction(
                    &db,
                    &memory,
                    std::slice::from_ref(&surface),
                    root.path(),
                    None,
                    "ee memory anchors",
                )
            })
            .unwrap();
        assert!(repaired.persisted);
        assert_eq!(repaired.added_anchor_count, 0);
        assert!(
            db.has_memory_code_anchor_projection(WORKSPACE, &surface)
                .unwrap()
        );
        assert_eq!(db.list_memory_anchors(MEMORY).unwrap(), before);
        assert_eq!(db.get_memory(MEMORY).unwrap().unwrap(), memory);
    }
}
