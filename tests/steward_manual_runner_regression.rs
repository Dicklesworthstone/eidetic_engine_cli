use std::fs;
use std::path::Path;

use ee::core::curate::{CurateDispositionOptions, run_curation_disposition};
use ee::curate::{CandidateSource, CandidateType};
use ee::db::{CreateCurationCandidateInput, CreateMemoryInput, CreateWorkspaceInput, DbConnection};
use ee::steward::{JobType, ManualRunner, RunOutcome, RunnerOptions};

type TestResult = Result<(), String>;

const WORKSPACE_ID: &str = "wsp_00000000000000000000000001";
const MEMORY_ID: &str = "mem_00000000000000000000000001";
const CANDIDATE_ID: &str = "curate_00000000000000000000000001";
const NOW: &str = "2026-06-13T00:00:00Z";

#[test]
fn curation_review_zero_item_budget_cancels_before_disposition_apply() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace_path = temp
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let ee_dir = workspace_path.join(".ee");
    fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
    let database_path = ee_dir.join("ee.db");

    {
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &CreateWorkspaceInput {
                    path: workspace_path.to_string_lossy().into_owned(),
                    name: Some("steward-curation-budget-regression".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                MEMORY_ID,
                &CreateMemoryInput {
                    workspace_id: WORKSPACE_ID.to_owned(),
                    level: "episodic".to_owned(),
                    kind: "note".to_owned(),
                    content: "curation budget regression fixture".to_owned(),
                    workflow_id: None,
                    confidence: 0.52,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: Some("test://steward-curation-budget".to_owned()),
                    trust_class: "agent_assertion".to_owned(),
                    trust_subclass: None,
                    tags: vec!["steward".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_curation_candidate(
                CANDIDATE_ID,
                &CreateCurationCandidateInput {
                    workspace_id: WORKSPACE_ID.to_owned(),
                    candidate_type: CandidateType::Promote.as_str().to_owned(),
                    target_memory_id: Some(MEMORY_ID.to_owned()),
                    proposed_content: None,
                    proposed_confidence: Some(0.86),
                    proposed_trust_class: Some("agent_validated".to_owned()),
                    source_type: CandidateSource::RuleEngine.as_str().to_owned(),
                    source_id: Some("steward-curation-budget-regression".to_owned()),
                    reason: "old pending curation item should be counted before mutation"
                        .to_owned(),
                    confidence: 0.86,
                    status: Some("pending".to_owned()),
                    created_at: Some("2026-05-01T00:00:00Z".to_owned()),
                    ttl_expires_at: None,
                    derivation_source_refs_json: None,
                    derivation_metadata_json: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;
    }

    let preview = run_curation_disposition(&CurateDispositionOptions {
        workspace_path: &workspace_path,
        database_path: Some(database_path.as_path()),
        actor: Some("steward-curation-budget-regression"),
        apply: false,
        structural_decay: false,
        now_rfc3339: Some(NOW),
    })
    .map_err(|error| error.message())?;
    assert_eq!(preview.summary.total_candidates, 1);
    assert_eq!(preview.summary.due_count, 1);
    assert!(!preview.durable_mutation);

    let mut runner = ManualRunner::new(
        RunnerOptions::new()
            .with_workspace_path(workspace_path.clone())
            .with_database_path(database_path.clone())
            .with_workspace_id(WORKSPACE_ID)
            .with_item_limit(0)
            .with_structural_decay(false)
            .with_as_of(NOW)
            .with_actor("steward-curation-budget-regression"),
    );
    let result = runner.run_job_type(
        JobType::CurationReview,
        Some("zero item budget regression".to_owned()),
    );

    assert_eq!(result.outcome, RunOutcome::Cancelled);
    assert_eq!(result.items_processed, Some(1));
    let details = result
        .details
        .ok_or_else(|| "curation review details missing".to_owned())?;
    assert_eq!(details["summary"]["totalCandidates"].as_u64(), Some(1));
    assert_eq!(details["summary"]["dueCount"].as_u64(), Some(1));
    assert_eq!(details["durableMutation"].as_bool(), Some(false));
    assert_eq!(details["cancelledBeforeMutation"].as_bool(), Some(true));

    let connection = DbConnection::open_file(database_path).map_err(|error| error.to_string())?;
    let candidate = connection
        .get_curation_candidate(WORKSPACE_ID, CANDIDATE_ID)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "candidate missing after cancelled run".to_owned())?;
    assert_eq!(candidate.status, "pending");
    assert_eq!(candidate.review_state, "new");
    assert!(candidate.reviewed_at.is_none());
    assert!(candidate.snoozed_until.is_none());
    connection.close().map_err(|error| error.to_string())
}

#[test]
fn cache_pruning_uses_persisted_workspace_id_without_explicit_option() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace_path = temp
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let ee_dir = workspace_path.join(".ee");
    fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
    let database_path = ee_dir.join("ee.db");
    let cache_root = workspace_path.join("pack-l2-root");
    let cache_workspace = cache_root.join(WORKSPACE_ID);
    fs::create_dir_all(&cache_workspace).map_err(|error| error.to_string())?;
    let stale_entry = cache_workspace.join("stale-pack.json");
    fs::write(
        &stale_entry,
        br#"{"schema":"ee.pack.l2_cache.entry.v1","storedAtEpochSeconds":1,"body":{"stale":true}}"#,
    )
    .map_err(|error| error.to_string())?;
    let cache_root_literal = serde_json::to_string(&cache_root.to_string_lossy().to_string())
        .map_err(|error| error.to_string())?;
    fs::write(
        ee_dir.join("config.toml"),
        format!(
            "[cache.pack_l2]\ndirectory = {cache_root_literal}\nmax_bytes = 1\nmax_age_days = 30\n"
        ),
    )
    .map_err(|error| error.to_string())?;

    {
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &CreateWorkspaceInput {
                    path: workspace_path.to_string_lossy().into_owned(),
                    name: Some("steward-cache-workspace-regression".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;
    }

    let mut runner = ManualRunner::new(
        RunnerOptions::new()
            .with_workspace_path(workspace_path)
            .with_database_path(database_path),
    );
    let result = runner.run_job_type(
        JobType::CachePruning,
        Some("cache workspace id regression".to_owned()),
    );

    assert_eq!(result.outcome, RunOutcome::Success);
    assert_eq!(result.items_processed, Some(1));
    let details = result
        .details
        .ok_or_else(|| "cache pruning details missing".to_owned())?;
    assert_eq!(details["workspaceId"].as_str(), Some(WORKSPACE_ID));
    assert_eq!(details["filesDeleted"].as_u64(), Some(1));
    assert_eq!(details["durableMutation"].as_bool(), Some(true));
    assert!(!stale_entry.exists());

    Ok(())
}

#[test]
fn backup_export_item_budget_cancels_before_creating_backup() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace_path = temp
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let ee_dir = workspace_path.join(".ee");
    fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
    let database_path = ee_dir.join("ee.db");

    {
        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                WORKSPACE_ID,
                &CreateWorkspaceInput {
                    path: workspace_path.to_string_lossy().into_owned(),
                    name: Some("steward-backup-budget-regression".to_owned()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                MEMORY_ID,
                &CreateMemoryInput {
                    workspace_id: WORKSPACE_ID.to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "backup budget regression fixture".to_owned(),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.7,
                    importance: 0.6,
                    provenance_uri: Some("test://steward-backup-budget".to_owned()),
                    trust_class: "agent_validated".to_owned(),
                    trust_subclass: None,
                    tags: vec!["backup".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;
    }

    let mut runner = ManualRunner::new(
        RunnerOptions::new()
            .with_workspace_path(workspace_path)
            .with_database_path(database_path)
            .with_item_limit(1),
    );
    let result = runner.run_job_type(
        JobType::BackupExport,
        Some("backup item budget regression".to_owned()),
    );

    assert_eq!(result.outcome, RunOutcome::Cancelled);
    assert!(result.items_processed.unwrap_or(0) > 1);
    let details = result
        .details
        .ok_or_else(|| "backup export details missing".to_owned())?;
    assert_eq!(details["durableMutation"].as_bool(), Some(false));
    assert_eq!(details["cancelledBeforeMutation"].as_bool(), Some(true));
    let backup_path = details["backupPath"]
        .as_str()
        .ok_or_else(|| "backup path missing".to_owned())?;
    assert!(!Path::new(backup_path).exists());

    Ok(())
}
