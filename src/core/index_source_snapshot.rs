//! Read-only capture of the canonical index corpus.
//!
//! Inspection and reversible doctor repair must not backfill source tables.
//! Missing precision anchors use the same pure extractor as a writable rebuild,
//! but remain request-local. The owner releases only a snapshot it began.

use super::{DbConnection, IndexRebuildError};
use crate::db::DatabaseOpenMode;
use crate::models::{MemoryAnchorSource, StoredMemoryAnchor, extract_precision_memory_anchors};

pub(super) fn capture<T>(
    db: &DbConnection,
    collect: impl FnOnce() -> Result<T, IndexRebuildError>,
) -> Result<T, IndexRebuildError> {
    if db.mode() != DatabaseOpenMode::ReadOnly {
        return db.with_transaction_error(collect);
    }
    let snapshot = ReadSnapshot::begin(db)?;
    let result = collect()?;
    snapshot.finish()?;
    Ok(result)
}

struct ReadSnapshot<'a> {
    db: &'a DbConnection,
    active: bool,
}

impl<'a> ReadSnapshot<'a> {
    fn begin(db: &'a DbConnection) -> Result<Self, IndexRebuildError> {
        db.begin_read_snapshot()?;
        Ok(Self { db, active: true })
    }

    fn finish(mut self) -> Result<(), IndexRebuildError> {
        self.db.commit_read_snapshot()?;
        self.active = false;
        Ok(())
    }
}

impl Drop for ReadSnapshot<'_> {
    fn drop(&mut self) {
        if self.active && self.db.rollback_read_snapshot().is_err() {
            tracing::error!(target: "ee::index", "failed to release index source read snapshot");
        }
    }
}

pub(super) fn projected_anchors(memory_id: &str, content: &str) -> Vec<StoredMemoryAnchor> {
    let mut anchors: Vec<_> = extract_precision_memory_anchors(
        memory_id,
        content,
        MemoryAnchorSource::IndexRebuild,
        Some("index_rebuild"),
    )
    .into_iter()
    .map(|anchor| StoredMemoryAnchor {
        memory_id: anchor.memory_id,
        anchor_kind: anchor.anchor_kind,
        anchor_value_hash: anchor.anchor_value_hash,
        redacted_anchor_value: anchor.redacted_anchor_value,
        confidence: anchor.confidence,
        source: anchor.source,
        provenance: anchor.provenance,
        captured_span_hash: anchor.captured_span_hash,
        freshness_state: anchor.freshness_state,
        generation: anchor.generation,
        // These are projections, not stored anchor rows. The search projector
        // consumes neither timestamp; do not invent capture-time evidence.
        created_at: String::new(),
        updated_at: String::new(),
    })
    .collect();
    // Match list_memory_anchors ordering, not the extractor's traversal order.
    anchors.sort_by(|left, right| {
        left.anchor_kind.as_str().cmp(right.anchor_kind.as_str())
            .then_with(|| left.anchor_value_hash.cmp(&right.anchor_value_hash))
    });
    anchors
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput};
    use crate::core::index::{IndexRebuildOptions, rebuild_index};

    const WORKSPACE: &str = "wsp_00000000000000000000000071";
    const MEMORY: &str = "mem_00000000000000000000000071";

    fn fixture() -> (tempfile::TempDir, DbConnection, std::path::PathBuf) {
        let root = tempfile::tempdir().expect("fixture");
        let workspace = root.path().canonicalize().expect("canonical workspace");
        std::fs::create_dir(workspace.join(".ee")).expect("store directory");
        let path = workspace.join(".ee/ee.db");
        let db = DbConnection::open_file(&path).expect("database");
        db.migrate().expect("schema");
        db.insert_workspace(WORKSPACE, &CreateWorkspaceInput {
            path: workspace.display().to_string(), name: Some("source snapshot".to_owned()),
        }).expect("workspace");
        db.insert_memory_revision(MEMORY, MEMORY, &CreateMemoryInput {
            workspace_id: WORKSPACE.to_owned(), level: "procedural".to_owned(), kind: "rule".to_owned(),
            content: "Run `cargo fmt --check` before touching `src/db/mod.rs`.".to_owned(),
            workflow_id: None, confidence: 0.9, utility: 0.5, importance: 0.5,
            provenance_uri: None, trust_class: "human_explicit".to_owned(), trust_subclass: None,
            tags: Vec::new(), valid_from: None, valid_to: None,
        }).expect("unanchored revision");
        (root, db, path)
    }

    #[test]
    fn read_only_projection_matches_writable_backfill_without_mutating_anchors() {
        let (_root, db, path) = fixture();
        assert!(db.list_memory_anchors(MEMORY).expect("anchors").is_empty());
        let read = DbConnection::open_file_read_only(&path).expect("read-only store");
        let projected = capture(&read, || {
            let memories = read.list_memories_for_retrieval_with_global(WORKSPACE, None, false)?;
            super::super::memory_documents_with_anchors(&read, &memories)
        }).expect("read snapshot");
        assert_eq!(projected.len(), 1);
        assert!(db.list_memory_anchors(MEMORY).expect("unchanged anchors").is_empty());
        let memories = db.list_memories_for_retrieval_with_global(WORKSPACE, None, false).expect("memories");
        let persisted = super::super::memory_documents_with_anchors(&db, &memories).expect("backfill");
        assert!(!db.list_memory_anchors(MEMORY).expect("persisted anchors").is_empty());
        for (projected, persisted) in projected.into_iter().zip(persisted) {
            assert_eq!(projected.id(), persisted.id());
            assert_eq!(projected.content(), persisted.content());
            assert_eq!(projected.into_indexable().metadata, persisted.into_indexable().metadata);
        }
    }

    #[test]
    fn dry_run_counts_the_real_corpus_without_backfilling_or_creating_an_index() {
        let (root, db, path) = fixture();
        let generation = db.get_workspace_generation(WORKSPACE).expect("generation");
        let report = rebuild_index(&IndexRebuildOptions {
            workspace_path: root.path().canonicalize().expect("root"),
            database_path: Some(path), index_dir: None, dry_run: true,
        }).expect("dry run");
        assert_eq!(report.memories_indexed, 1);
        assert!(report.dry_run);
        assert!(!report.index_dir.exists());
        assert!(db.list_memory_anchors(MEMORY).expect("anchors").is_empty());
        assert_eq!(generation, db.get_workspace_generation(WORKSPACE).expect("generation"));
    }

    #[test]
    fn errors_release_owned_snapshots_but_failed_nested_begin_preserves_the_callers() {
        let (_root, _db, path) = fixture();
        let read = DbConnection::open_file_read_only(&path).expect("read-only store");
        let result = capture::<()>(&read, || Err(IndexRebuildError::Index("fixture".to_owned())));
        assert!(result.is_err());
        read.begin_read_snapshot().expect("previous snapshot released");
        assert!(capture(&read, || Ok(())).is_err());
        // A nested begin may not roll back the transaction it did not own.
        read.commit_read_snapshot().expect("caller still owns its snapshot");
        capture(&read, || Ok(())).expect("subsequent independent snapshot");
    }
}
