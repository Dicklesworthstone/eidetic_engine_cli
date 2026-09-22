//! Read-only capture of the canonical index corpus.
//!
//! Dry-run corpus capture must not backfill source tables.
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
        left.anchor_kind
            .as_str()
            .cmp(right.anchor_kind.as_str())
            .then_with(|| left.anchor_value_hash.cmp(&right.anchor_value_hash))
    });
    anchors
}

pub(super) fn reembed_dry_run(
    cx: &asupersync::Cx,
    options: &super::IndexReembedOptions,
) -> Result<super::IndexReembedReport, IndexRebuildError> {
    super::index_checkpoint(cx)?;
    let start = std::time::Instant::now();
    let index_dir = options.resolve_index_dir();
    let db = DbConnection::open_file_read_only(&options.resolve_database_path())?;
    let workspace_id = super::resolve_index_workspace_id(&db, &options.workspace_path)?;
    let snapshot = super::collect_workspace_index_source_snapshot(&db, &workspace_id)?;
    super::index_checkpoint(cx)?;
    let embedding = super::ReembedEmbeddingSummary::from_posture(
        super::embedding_posture_for_document_count(
            &db,
            &workspace_id,
            &index_dir,
            snapshot.documents_total,
        )?,
    );
    let idempotency_key = super::reembed_idempotency_key(
        &workspace_id,
        &embedding.fast_model_id,
        embedding.quality_model_id.as_deref(),
        snapshot.document_counts,
    );
    let documents_embedded = embedding.documents_embedded();
    Ok(super::IndexReembedReport {
        status: super::IndexReembedStatus::DryRun,
        job_id: None,
        job_status: "dry_run_not_queued".to_owned(),
        job_type: super::SearchIndexJobType::FullRebuild.as_str().to_owned(),
        document_source: None,
        embedding_scope: "all_documents".to_owned(),
        embedding,
        memories_indexed: snapshot.memories_indexed,
        sessions_indexed: snapshot.sessions_indexed,
        artifacts_indexed: snapshot.artifacts_indexed,
        rules_indexed: snapshot.rules_indexed,
        evidence_indexed: snapshot.evidence_indexed,
        documents_embedded,
        documents_total: snapshot.documents_total,
        index_dir,
        elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        dry_run: true,
        idempotency_key,
        evidence_admission: snapshot.evidence_admission,
        errors: Vec::new(),
        runtime_profile: super::runtime_profile_for_workspace(&options.workspace_path),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::core::index::{IndexRebuildOptions, rebuild_index};
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput};

    const WORKSPACE: &str = "wsp_00000000000000000000000071";
    const MEMORY: &str = "mem_00000000000000000000000071";

    fn fixture() -> (tempfile::TempDir, DbConnection, std::path::PathBuf) {
        let root = tempfile::tempdir().expect("fixture");
        let workspace = root.path().canonicalize().expect("canonical workspace");
        std::fs::create_dir(workspace.join(".ee")).expect("store directory");
        let path = workspace.join(".ee/ee.db");
        let db = DbConnection::open_file(&path).expect("database");
        db.migrate().expect("schema");
        db.insert_workspace(
            WORKSPACE,
            &CreateWorkspaceInput {
                path: workspace.display().to_string(),
                name: Some("source snapshot".to_owned()),
            },
        )
        .expect("workspace");
        db.insert_memory_revision(
            MEMORY,
            MEMORY,
            &CreateMemoryInput {
                workspace_id: WORKSPACE.to_owned(),
                level: "procedural".to_owned(),
                kind: "rule".to_owned(),
                content: "Run `cargo fmt --check` before touching `src/db/mod.rs`.".to_owned(),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: None,
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
        )
        .expect("unanchored revision");
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
        })
        .expect("read snapshot");
        assert_eq!(projected.len(), 1);
        assert!(
            db.list_memory_anchors(MEMORY)
                .expect("unchanged anchors")
                .is_empty()
        );
        let memories = db
            .list_memories_for_retrieval_with_global(WORKSPACE, None, false)
            .expect("memories");
        let persisted =
            super::super::memory_documents_with_anchors(&db, &memories).expect("backfill");
        assert!(
            !db.list_memory_anchors(MEMORY)
                .expect("persisted anchors")
                .is_empty()
        );
        for (projected, persisted) in projected.into_iter().zip(persisted) {
            assert_eq!(projected.id(), persisted.id());
            assert_eq!(projected.content(), persisted.content());
            assert_eq!(
                projected.into_indexable().metadata,
                persisted.into_indexable().metadata
            );
        }
    }

    #[test]
    fn dry_run_counts_the_real_corpus_without_backfilling_or_creating_an_index() {
        let (root, db, path) = fixture();
        let generation = db.get_workspace_generation(WORKSPACE).expect("generation");
        let report = rebuild_index(&IndexRebuildOptions {
            workspace_path: root.path().canonicalize().expect("root"),
            database_path: Some(path),
            index_dir: None,
            dry_run: true,
        })
        .expect("dry run");
        assert_eq!(report.memories_indexed, 1);
        assert!(report.dry_run);
        assert!(!report.index_dir.exists());
        assert!(db.list_memory_anchors(MEMORY).expect("anchors").is_empty());
        assert_eq!(
            generation,
            db.get_workspace_generation(WORKSPACE).expect("generation")
        );
    }

    #[test]
    fn errors_release_owned_snapshots_but_failed_nested_begin_preserves_the_callers() {
        let (_root, _db, path) = fixture();
        let read = DbConnection::open_file_read_only(&path).expect("read-only store");
        let result = capture::<()>(&read, || Err(IndexRebuildError::Index("fixture".to_owned())));
        assert!(result.is_err());
        read.begin_read_snapshot()
            .expect("previous snapshot released");
        assert!(capture(&read, || Ok(())).is_err());
        // A nested begin may not roll back the transaction it did not own.
        read.commit_read_snapshot()
            .expect("caller still owns its snapshot");
        capture(&read, || Ok(())).expect("subsequent independent snapshot");
    }

    #[test]
    fn reembed_dry_run_does_not_mutate_sources_or_initialize_backends() {
        const CHILD_WORKSPACE: &str = "EE_TEST_REEMBED_PREVIEW_WORKSPACE";
        if let Some(workspace) = std::env::var_os(CHILD_WORKSPACE) {
            let workspace = std::path::PathBuf::from(workspace);
            assert!(super::super::ACTIVE_REMOTE_EMBEDDER.get().is_none());
            assert!(super::super::DEFAULT_SEARCH_EMBEDDER.get().is_none());
            let report = super::super::reembed_index(&super::super::IndexReembedOptions {
                workspace_path: workspace.clone(),
                database_path: Some(workspace.join(".ee/ee.db")),
                index_dir: Some(workspace.join(".ee/index")),
                dry_run: true,
            })
            .expect("read-only reembed preview");
            assert_eq!(report.status, super::super::IndexReembedStatus::DryRun);
            assert_eq!(report.memories_indexed, 1);
            assert_eq!(report.documents_total, 1);
            assert_eq!(report.documents_embedded, 0);
            assert!(report.job_id.is_none());
            assert_eq!(report.job_status, "dry_run_not_queued");
            assert_eq!(report.embedding.source, "remote_dimension_unprobed");
            assert!(super::super::ACTIVE_REMOTE_EMBEDDER.get().is_none());
            assert!(super::super::DEFAULT_SEARCH_EMBEDDER.get().is_none());
            return;
        }

        let (root, db, _path) = fixture();
        let generation = db.get_workspace_generation(WORKSPACE).expect("generation");
        assert!(
            db.list_memory_anchors(MEMORY)
                .expect("initial anchors")
                .is_empty()
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("endpoint");
        listener.set_nonblocking(true).expect("nonblocking endpoint");
        let endpoint = format!(
            "http://{}/v1",
            listener.local_addr().expect("endpoint address")
        );
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "core::index::source_snapshot::tests::reembed_dry_run_does_not_mutate_sources_or_initialize_backends",
                "--nocapture",
            ])
            .env(
                CHILD_WORKSPACE,
                root.path().canonicalize().expect("workspace"),
            )
            .env("EE_EMBED_BACKEND", "remote")
            .env("EE_EMBED_REMOTE_URL", endpoint)
            .env("EE_EMBED_REMOTE_MODEL", "passive-reembed-fixture")
            .env_remove("EE_EMBED_REMOTE_DIMENSION")
            .env_remove("EE_EMBED_REMOTE_API_KEY")
            .output()
            .expect("isolated reembed preview");
        assert!(
            output.status.success(),
            "isolated preview failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        assert!(
            db.list_memory_anchors(MEMORY)
                .expect("unchanged anchors")
                .is_empty()
        );
        assert_eq!(
            generation,
            db.get_workspace_generation(WORKSPACE).expect("generation")
        );
        assert!(
            db.list_search_index_jobs(WORKSPACE, None)
                .expect("jobs")
                .is_empty()
        );
        assert!(
            db.list_embedding_metadata_records(WORKSPACE)
                .expect("registry")
                .is_empty()
        );
        assert!(!root.path().join(".ee/index").exists());
    }
}
