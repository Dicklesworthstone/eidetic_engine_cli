//! Doctor repairs must remain usable by the ordinary incremental publisher.

use super::*;
use crate::core::index::delta;
use crate::db::{CreateMemoryInput, CreateWorkspaceInput};
use crate::search::IndexableDocument;
use std::time::Duration;

type TestResult = Result<(), String>;
const WORKSPACE: &str = "wsp_00000000000000000000000931";

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    database: PathBuf,
    staging: PathBuf,
}

impl Fixture {
    fn new(memories: u32) -> Result<Self, String> {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let workspace = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        std::fs::create_dir(workspace.join(".ee")).map_err(|error| error.to_string())?;
        let database = workspace.join(".ee/ee.db");
        let db = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
        db.migrate().map_err(|error| error.to_string())?;
        db.insert_workspace(
            WORKSPACE,
            &CreateWorkspaceInput {
                path: workspace.display().to_string(),
                name: Some("doctor delta repair".to_owned()),
            },
        )
        .map_err(|error| error.to_string())?;
        for number in 0..memories {
            let id = format!("mem_{number:026}");
            // A revision bypasses insert-time anchor extraction, so source
            // purity also proves doctor did not backfill missing anchors.
            db.insert_memory_revision(
                &id,
                &id,
                &CreateMemoryInput {
                    workspace_id: WORKSPACE.to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: format!("Run `cargo test` for `src/repair{number}.rs`."),
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
            .map_err(|error| error.to_string())?;
        }
        db.close().map_err(|error| error.to_string())?;
        let staging = workspace.join("doctor-stage");
        Ok(Self {
            _root: root,
            workspace,
            database,
            staging,
        })
    }

    fn documents(&self) -> Result<Vec<IndexableDocument>, String> {
        let db =
            DbConnection::open_file_read_only(&self.database).map_err(|error| error.to_string())?;
        collect_workspace_index_source_snapshot(&db, WORKSPACE)
            .map(|source| source.documents)
            .map_err(|error| error.to_string())
    }

    fn stage(&self) -> Result<PreparedRepair, String> {
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            stage_with_selection(&cx, &self.workspace, &self.staging, |_, _| {
                Ok(hash_fallback_embedder_stack())
            })
            .await
        })
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
    }
}

#[test]
fn doctor_repair_records_exact_source_and_supports_the_next_delta() -> TestResult {
    let fixture = Fixture::new(4)?;
    let before = std::fs::read(&fixture.database).map_err(|error| error.to_string())?;
    let mut documents = fixture.documents()?;
    assert_eq!(documents.len(), 4);
    let prepared = fixture.stage()?;
    prepared
        .check_source_generation()
        .map_err(|error| error.to_string())?;
    let stack = hash_fallback_embedder_stack();
    let actual: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture.staging.join(delta::DOC_DIGESTS_FILE))
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let expected = serde_json::to_value(delta::DocDigests::new(&stack, &documents))
        .map_err(|error| error.to_string())?;
    assert_eq!(
        actual, expected,
        "digests must bind the exact read-only projection"
    );
    let unchanged = delta::plan(&fixture.staging, &stack, &documents)
        .map_err(|error| format!("a repaired generation must accept delta planning: {error:?}"))?;
    assert!(unchanged.upserts.is_empty() && unchanged.removals.is_empty());

    // A single edited memory is a delta, not another all-corpus inference job.
    documents[0]
        .content
        .push_str(" Check the repair's exit status.");
    let changed = delta::plan(&fixture.staging, &stack, &documents)
        .map_err(|error| format!("the first post-repair write must stay incremental: {error:?}"))?;
    assert_eq!(changed.upserts.len(), 1);
    assert_eq!(changed.upserts[0].id, documents[0].id);
    assert!(changed.removals.is_empty());

    let source =
        DbConnection::open_file_read_only(&fixture.database).map_err(|error| error.to_string())?;
    for number in 0..4_u32 {
        assert!(
            source
                .list_memory_anchors(&format!("mem_{number:026}"))
                .map_err(|error| error.to_string())?
                .is_empty()
        );
    }
    assert_eq!(
        source
            .count_table_rows("search_index_jobs")
            .map_err(|error| error.to_string())?,
        0
    );
    assert_eq!(
        std::fs::read(&fixture.database).map_err(|error| error.to_string())?,
        before
    );
    Ok(())
}

#[test]
fn doctor_repair_digests_also_enable_removals_and_metadata_updates() -> TestResult {
    let fixture = Fixture::new(4)?;
    fixture.stage()?;
    let stack = hash_fallback_embedder_stack();
    let mut documents = fixture.documents()?;
    let removed_id = documents.pop().ok_or("fixture document missing")?.id;
    let removed = delta::plan(&fixture.staging, &stack, &documents)
        .map_err(|error| format!("post-repair deletion must be plannable: {error:?}"))?;
    assert_eq!(removed.removals, vec![removed_id]);
    assert!(removed.upserts.is_empty());

    let mut documents = fixture.documents()?;
    documents[0].title = Some("Updated repair instructions".to_owned());
    let metadata = delta::plan(&fixture.staging, &stack, &documents)
        .map_err(|error| format!("post-repair metadata change must be plannable: {error:?}"))?;
    assert_eq!(metadata.upserts.len(), 1);
    assert_eq!(metadata.upserts[0].id, documents[0].id);
    assert!(metadata.removals.is_empty());
    Ok(())
}

#[test]
fn empty_doctor_repair_never_selects_a_model_and_records_empty_digests() -> TestResult {
    let fixture = Fixture::new(0)?;
    let result = crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
        stage_with_selection(&cx, &fixture.workspace, &fixture.staging, |_, _| {
            Err(IndexRebuildError::Index(
                "empty repair selected a model".to_owned(),
            ))
        })
        .await?;
        let documents = Vec::new();
        let delta = delta::plan(
            &fixture.staging,
            &hash_fallback_embedder_stack(),
            &documents,
        )
        .map_err(|error| IndexRebuildError::Index(format!("empty repair digests: {error:?}")))?;
        assert!(delta.upserts.is_empty() && delta.removals.is_empty());
        Ok::<_, IndexRebuildError>(())
    })
    .map_err(|error| error.to_string())?;
    result.map_err(|error| error.to_string())
}

#[test]
fn cancelled_selection_never_publishes_admission_or_loses_the_cancel_reason() -> TestResult {
    let fixture = Fixture::new(4)?;
    let before = std::fs::read(&fixture.database).map_err(|error| error.to_string())?;
    let result = crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
        let result = stage_with_selection(&cx, &fixture.workspace, &fixture.staging, |_, _| {
            cx.set_cancel_reason(asupersync::CancelReason::user(
                "cancel doctor model selection",
            ));
            Err(IndexRebuildError::Index(
                "interrupted model operation".to_owned(),
            ))
        })
        .await;
        assert!(matches!(result, Err(IndexRebuildError::Cancelled(_))));
        assert!(
            !fixture.staging.exists(),
            "cancellation precedes generation allocation"
        );
        assert_eq!(
            std::fs::read(&fixture.database).map_err(|error| error.to_string())?,
            before
        );
        Ok::<_, String>(())
    })
    .map_err(|error| error.to_string())?;
    result
}
