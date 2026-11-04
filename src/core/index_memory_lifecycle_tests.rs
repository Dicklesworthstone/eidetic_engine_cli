//! Manual curation must schedule durable index work for both retirement and
//! restoration. Inspect the published vectors directly so live search fallback
//! cannot make a missing index update appear successful.

use super::*;
use crate::core::curate::{
    CurateTombstoneOptions, CurateUntombstoneOptions, run_curate_tombstone, run_curate_untombstone,
};
use crate::db::{CreateMemoryInput, CreateWorkspaceInput};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn published_ids(index_dir: &Path) -> TestResult<BTreeSet<String>> {
    let index =
        frankensearch::TwoTierIndex::open(index_dir, frankensearch::TwoTierConfig::default())?;
    let mut ids = BTreeSet::new();
    for position in 0..index.doc_count() {
        ids.insert(index.doc_id_at(position)?.to_owned());
    }
    Ok(ids)
}

fn assert_published_memories(
    connection: &DbConnection,
    options: &IndexProcessingOptions,
    expected: &[&str],
) -> TestResult {
    assert_eq!(
        published_ids(&options.resolve_index_dir())?,
        expected
            .iter()
            .map(|id| (*id).to_owned())
            .collect::<BTreeSet<_>>(),
        "the persisted index must reflect the completed lifecycle mutation"
    );
    let status = get_index_status_with_connection(
        &IndexStatusOptions {
            workspace_path: options.workspace_path.clone(),
            database_path: options.database_path.clone(),
            index_dir: options.index_dir.clone(),
        },
        Some(connection),
    )?;
    assert_eq!(status.health, IndexHealth::Ready, "{status:?}");
    assert_eq!(status.db_generation, status.index_generation, "{status:?}");
    assert_eq!(
        status.index_document_counts,
        Some(IndexDocumentCounts::memory_only(u32::try_from(
            expected.len()
        )?)),
        "{status:?}"
    );
    Ok(())
}

fn drain(
    options: &IndexProcessingOptions,
    coalesced: bool,
) -> Result<IndexProcessingReport, IndexRebuildError> {
    if coalesced {
        process_index_jobs_coalesced(options)
    } else {
        process_index_jobs(options)
    }
}

fn assert_lifecycle_job_drains(
    connection: &DbConnection,
    workspace_id: &str,
    memory_id: &str,
    options: &IndexProcessingOptions,
    coalesced: bool,
) -> TestResult {
    let jobs = connection.list_pending_search_index_jobs(workspace_id, None)?;
    assert_eq!(
        jobs.len(),
        1,
        "curation must queue exactly one job: {jobs:?}"
    );
    let job = &jobs[0];
    assert_eq!(
        job.job_type_enum(),
        Some(SearchIndexJobType::SingleDocument)
    );
    assert_eq!(job.document_source.as_deref(), Some("memory"));
    assert_eq!(job.document_id.as_deref(), Some(memory_id));
    assert_eq!(job.documents_total, 1);

    let report = drain(options, coalesced)?;
    assert_eq!(report.status, IndexProcessingStatus::Success, "{report:?}");
    assert_eq!(report.processed_jobs, 1, "{report:?}");
    assert_eq!(report.completed_jobs, 1, "{report:?}");
    assert_eq!(report.failed_jobs, 0, "{report:?}");
    assert_eq!(report.jobs.len(), 1, "{report:?}");
    assert_eq!(report.jobs[0].job_id, job.id);
    let completed = connection
        .get_search_index_job(&job.id)?
        .ok_or("completed lifecycle job disappeared")?;
    assert_eq!(
        completed.status_enum(),
        Some(SearchIndexJobStatus::Completed)
    );
    assert!(
        connection
            .list_pending_search_index_jobs(workspace_id, None)?
            .is_empty()
    );
    Ok(())
}

fn assert_manual_memory_lifecycle_round_trip(coalesced: bool) -> TestResult {
    let root = tempfile::tempdir()?;
    let workspace = root.path().canonicalize()?;
    std::fs::create_dir_all(workspace.join(".ee"))?;
    let database = workspace.join(".ee/ee.db");
    let index_dir = workspace.join(".ee/index");
    let workspace_id = crate::models::WorkspaceId::now().to_string();
    let memory_id = MemoryId::now().to_string();
    let survivor_id = MemoryId::now().to_string();
    let _embedder_guard = install_test_hash_workspace_embedder(&workspace_id);
    let connection = DbConnection::open_file(&database)?;
    connection.migrate()?;
    connection.insert_workspace(
        &workspace_id,
        &CreateWorkspaceInput {
            path: workspace.to_string_lossy().into_owned(),
            name: Some("manual memory lifecycle".to_owned()),
        },
    )?;
    for (id, content) in [
        (
            &memory_id,
            "Quartz kestrel release checks can be retired and restored.",
        ),
        (
            &survivor_id,
            "Indigo lantern deployment notes remain available.",
        ),
    ] {
        connection.insert_memory(
            id,
            &CreateMemoryInput {
                workspace_id: workspace_id.clone(),
                level: "semantic".to_owned(),
                kind: "fact".to_owned(),
                content: content.to_owned(),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: Some("test://memory-lifecycle".to_owned()),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
        )?;
    }
    let initial_job_id = format!(
        "sidx_{}",
        MemoryId::now().to_string().trim_start_matches("mem_")
    );
    connection.insert_search_index_job(
        &initial_job_id,
        &CreateSearchIndexJobInput {
            workspace_id: workspace_id.clone(),
            job_type: SearchIndexJobType::FullRebuild,
            document_source: None,
            document_id: None,
            documents_total: 2,
        },
    )?;
    let options = IndexProcessingOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        index_dir: Some(index_dir.clone()),
        dry_run: false,
        job_limit: Some(1),
    };
    let baseline = drain(&options, coalesced)?;
    assert_eq!(
        baseline.status,
        IndexProcessingStatus::Success,
        "{baseline:?}"
    );
    assert_eq!(baseline.completed_jobs, 1, "{baseline:?}");
    assert_published_memories(&connection, &options, &[&memory_id, &survivor_id])?;

    let tombstone = run_curate_tombstone(&CurateTombstoneOptions {
        workspace_path: &workspace,
        database_path: Some(&database),
        memory_id: &memory_id,
        actor: Some("test:manual-lifecycle"),
        dry_run: false,
        allow_tombstone_load_bearing: false,
        reason: Some("retire after review"),
    })?;
    assert!(tombstone.persisted && tombstone.audit_id.is_some());
    assert_lifecycle_job_drains(&connection, &workspace_id, &memory_id, &options, coalesced)?;
    assert_published_memories(&connection, &options, &[&survivor_id])?;

    let restored = run_curate_untombstone(&CurateUntombstoneOptions {
        workspace_path: &workspace,
        database_path: Some(&database),
        memory_id: &memory_id,
        actor: Some("test:manual-lifecycle"),
        dry_run: false,
        reason: Some("restore after review"),
    })?;
    assert!(restored.persisted && restored.audit_id.is_some());
    assert!(
        !published_ids(&index_dir)?.contains(&memory_id),
        "a restored source row alone must not mask missing index work"
    );
    assert_lifecycle_job_drains(&connection, &workspace_id, &memory_id, &options, coalesced)?;
    assert_published_memories(&connection, &options, &[&memory_id, &survivor_id])?;

    let repeated = drain(&options, coalesced)?;
    assert_eq!(repeated.status, IndexProcessingStatus::NoPendingJobs);
    assert_eq!(repeated.processed_jobs, 0);
    assert_published_memories(&connection, &options, &[&memory_id, &survivor_id])?;
    connection.close()?;
    Ok(())
}

#[test]
fn curate_tombstone_and_restore_republish_from_ordinary_queue() -> TestResult {
    assert_manual_memory_lifecycle_round_trip(false)
}

#[test]
fn curate_tombstone_and_restore_republish_from_coalesced_queue() -> TestResult {
    assert_manual_memory_lifecycle_round_trip(true)
}
