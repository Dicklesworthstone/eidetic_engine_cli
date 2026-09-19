//! Real backup/restore tests of work, artifact and audit fidelity.

use super::*;
use crate::core::backup::*;
use crate::db::{CreateTaskEpisodeInput, StoredArtifact, StoredArtifactLink, StoredSearchIndexJob};
use crate::models::{MemoryId, WorkspaceId};
use std::path::{Path, PathBuf};
use uuid::Uuid;

type TestResult = Result<(), String>;
const AUDIT_ID: &str = "audit_00000000000000000000000001";
const TIME: &str = "2026-09-01T00:00:00Z";

fn seed(db: &DbConnection, workspace: &str) -> Result<(), crate::db::DbError> {
    let memory = MemoryId::from_uuid(Uuid::from_u128(2)).to_string();
    db.insert_journal_entry_for_recovery(&StoredJournalEntry {
        entry_id: "journal-operational".to_owned(),
        workspace_id: workspace.to_owned(),
        agent_name: Some("RecoveryAgent".to_owned()),
        session_key: Some("source-session".to_owned()),
        kind: "note".to_owned(),
        source: "manual".to_owned(),
        body: "Orbitgate build failed; preserve the failed outcome.".to_owned(),
        structured: Some(r#"{"exitCode":1}"#.to_owned()),
        redaction_report: "{}".to_owned(),
        instruction_risk: "high".to_owned(),
        created_at: TIME.to_owned(),
        distilled_at: Some(TIME.to_owned()),
        tombstoned_at: Some(TIME.to_owned()),
    })?;
    for (n, status) in ["running", "failed", "cancelled"].into_iter().enumerate() {
        db.insert_search_index_job_for_recovery(&StoredSearchIndexJob {
            id: format!("sidx_{n:026}"),
            workspace_id: workspace.to_owned(),
            job_type: "single_document".to_owned(),
            document_source: Some("memory".to_owned()),
            document_id: Some(memory.clone()),
            status: status.to_owned(),
            documents_total: 1,
            documents_indexed: 1,
            error_message: Some("source index worker failed".to_owned()),
            created_at: TIME.to_owned(),
            started_at: Some(TIME.to_owned()),
            completed_at: (status != "running").then(|| TIME.to_owned()),
        })?;
    }
    db.insert_task_episode_with_created_at(
        "ep_000000000000000000000000001",
        &CreateTaskEpisodeInput {
            workspace_id: Some(workspace.to_owned()),
            session_id: None,
            task_input: "Orbitgate recovery preserves failure".to_owned(),
            retrieved_memory_ids: vec![memory.clone()],
            context_pack_id: None,
            actions: vec![],
            outcome: "failure".to_owned(),
            outcome_details: Some("The source command failed.".to_owned()),
            started_at: TIME.to_owned(),
            ended_at: Some(TIME.to_owned()),
            duration_ms: Some(75),
            agent: Some("RecoveryAgent".to_owned()),
            episode_hash: None,
        },
        TIME,
    )?;
    let artifact_id = "art_00000000000000000000000000";
    let snippet = "Orbitgate failed build evidence";
    db.insert_artifact_for_recovery(&StoredArtifact {
        id: artifact_id.to_owned(),
        workspace_id: workspace.to_owned(),
        source_kind: "file".to_owned(),
        artifact_type: "build_log".to_owned(),
        original_path: Some("build.log".to_owned()),
        canonical_path: Some("/recorded/Orbitgate/build.log".to_owned()),
        external_ref: None,
        content_hash: hash_bytes(b"original build log"),
        media_type: "text/plain".to_owned(),
        size_bytes: 256,
        redaction_status: "checked".to_owned(),
        snippet: Some(snippet.to_owned()),
        snippet_hash: Some(hash_bytes(snippet.as_bytes())),
        provenance_uri: None,
        metadata_json: r#"{"exitCode":1}"#.to_owned(),
        created_at: TIME.to_owned(),
        updated_at: TIME.to_owned(),
    })?;
    db.insert_artifact_link_for_recovery(&StoredArtifactLink {
        artifact_id: artifact_id.to_owned(),
        target_type: "memory".to_owned(),
        target_id: memory,
        relation: "supports".to_owned(),
        created_at: TIME.to_owned(),
        metadata_json: Some(r#"{"line":5}"#.to_owned()),
    })?;
    Ok(())
}

fn snapshot(db: &DbConnection, workspace: &str) -> Result<serde_json::Value, crate::db::DbError> {
    let episodes: Vec<_> = db
        .list_task_episodes(Some(workspace), None, u32::MAX)?
        .iter()
        .map(|row| {
            task_episode_json(row, "", RedactionLevel::None, &BTreeMap::new())["episode"].clone()
        })
        .collect();
    let artifacts = db.list_artifacts(workspace, None)?;
    let mut links = Vec::new();
    for row in &artifacts {
        links.extend(db.list_artifact_links(&row.id)?);
    }
    Ok(serde_json::json!({
        "journals": db.list_journal_entries(workspace, &JournalEntryListFilter {limit: u32::MAX, ..Default::default()})?,
        "jobs": db.list_search_index_jobs(workspace, None)?,
        "episodes": episodes,
        "artifacts": artifacts,
        "links": links,
        "originalAudit": db.get_audit(AUDIT_ID)?,
    }))
}

fn create(
    workspace: &Path,
    database: &Path,
    redaction: RedactionLevel,
) -> Result<BackupCreateReport, String> {
    create_backup(&BackupCreateOptions {
        workspace_path: workspace.to_owned(),
        database_path: Some(database.to_owned()),
        output_dir: None,
        label: None,
        redaction_level: redaction,
        include_derived: false,
        include_graph_cache: false,
        dry_run: false,
    })
    .map_err(|e| e.message())
}

fn assert_rejected(table: &str, sql: &str) -> TestResult {
    for late in [false, true] {
        let (root, workspace, database) =
            crate::core::backup::tests::fixture().map_err(|e| e.message())?;
        let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        seed(&db, &workspace_id).map_err(|e| e.to_string())?;
        db.close().map_err(|e| e.to_string())?;
        let backup = create(&workspace, &database, RedactionLevel::None)?;
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        let original = snapshot(&db, &workspace_id).map_err(|e| e.to_string())?;
        db.close().map_err(|e| e.to_string())?;
        let options = BackupRestoreOptions {
            workspace_path: workspace.clone(),
            backup_path: PathBuf::from(&backup.backup_path),
            side_path: root
                .path()
                .canonicalize()
                .map_err(|e| e.to_string())?
                .join("refused"),
            restore_graph_cache: false,
            dry_run: false,
        };
        let changed = std::cell::Cell::new(false);
        let mutate = |path: &Path| -> Result<(), DomainError> {
            if late {
                assert!(
                    path.parent()
                        .ok_or_else(|| work_history_error("no staged parent"))?
                        .join("index/meta.json")
                        .is_file()
                );
            }
            let db = DbConnection::open_file(path).map_err(work_history_error)?;
            let before = snapshot(&db, &workspace_id).map_err(work_history_error)?;
            let count = db.count_table_rows(table).map_err(work_history_error)?;
            crate::core::backup::recovery_faults::inject_history_corruption(&db, table, sql)?;
            assert_ne!(
                before,
                snapshot(&db, &workspace_id).map_err(work_history_error)?
            );
            assert_eq!(
                count,
                db.count_table_rows(table).map_err(work_history_error)?
            );
            db.close().map_err(work_history_error)?;
            changed.set(true);
            Ok(())
        };
        let error = restore_backup_to_side_path_with_recovery_hooks(
            &options,
            |path| if late { Ok(()) } else { mutate(path) },
            |path| if late { mutate(path) } else { Ok(()) },
        )
        .err()
        .ok_or("published changed operational history")?;
        assert!(
            changed.get(),
            "required corruption never executed: {}",
            error.message()
        );
        assert!(
            error.message().contains(table),
            "wrong refusal: {}",
            error.message()
        );
        assert!(!error.message().contains("OPERATIONAL_PRIVATE_CANARY"));
        assert!(!options.side_path.join(WORKSPACE_MARKER).exists());
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        assert_eq!(
            snapshot(&db, &workspace_id).map_err(|e| e.to_string())?,
            original
        );
        db.close().map_err(|e| e.to_string())?;
        assert_eq!(
            verify_backup(&BackupVerifyOptions {
                workspace_path: workspace,
                backup_path: options.backup_path,
            })
            .map_err(|e| e.message())?
            .status,
            "verified"
        );
    }
    Ok(())
}

#[test]
fn operational_fence_preserves_consumed_and_tombstoned_journals() -> TestResult {
    assert_rejected(
        "journal_entries",
        "UPDATE journal_entries SET distilled_at = NULL, tombstoned_at = NULL",
    )
}

#[test]
fn operational_fence_preserves_journal_instruction_risk() -> TestResult {
    assert_rejected(
        "journal_entries",
        "UPDATE journal_entries SET instruction_risk = 'none'",
    )
}

#[test]
fn operational_fence_preserves_failed_job_history() -> TestResult {
    assert_rejected(
        "search_index_jobs",
        "UPDATE search_index_jobs SET status = 'completed', error_message = NULL WHERE status = 'failed'",
    )
}

#[test]
fn operational_fence_preserves_job_target_identity() -> TestResult {
    assert_rejected(
        "search_index_jobs",
        "UPDATE search_index_jobs SET document_id = 'OPERATIONAL_PRIVATE_CANARY'",
    )
}

#[test]
fn operational_fence_preserves_task_failure_and_retrieved_context() -> TestResult {
    assert_rejected(
        "task_episodes",
        "UPDATE task_episodes SET outcome = 'success', retrieved_memory_ids = '[]'",
    )
}

#[test]
fn operational_fence_preserves_artifact_content_and_provenance() -> TestResult {
    assert_rejected(
        "artifacts",
        "UPDATE artifacts SET snippet = 'OPERATIONAL_PRIVATE_CANARY', provenance_uri = 'changed'",
    )
}

#[test]
fn operational_fence_preserves_artifact_evidence_relationships() -> TestResult {
    assert_rejected(
        "artifact_links",
        "UPDATE artifact_links SET relation = 'contradicts', metadata_json = '{}'",
    )
}

#[test]
fn operational_fence_preserves_original_audit_even_with_new_restore_audits() -> TestResult {
    assert_rejected(
        "audit_log",
        "UPDATE audit_log SET action = 'OPERATIONAL_PRIVATE_CANARY' WHERE id = 'audit_00000000000000000000000001'",
    )
}

#[test]
fn operational_history_survives_two_recovery_generations() -> TestResult {
    for redaction in [RedactionLevel::None, RedactionLevel::Standard] {
        let (root, mut workspace, mut database) =
            crate::core::backup::tests::fixture().map_err(|e| e.message())?;
        let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
        let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
        seed(&db, &workspace_id).map_err(|e| e.to_string())?;
        db.close().map_err(|e| e.to_string())?;
        let mut first = None;
        for round in 0..2 {
            let backup = create(&workspace, &database, redaction)?;
            let side = root
                .path()
                .canonicalize()
                .map_err(|e| e.to_string())?
                .join(format!("round-{round}"));
            let restored = restore_backup_to_side_path(&BackupRestoreOptions {
                workspace_path: workspace,
                backup_path: PathBuf::from(backup.backup_path),
                side_path: side.clone(),
                restore_graph_cache: false,
                dry_run: false,
            })
            .map_err(|e| e.message())?;
            let db = DbConnection::open_file(&restored.restored_database_path)
                .map_err(|e| e.to_string())?;
            let state = snapshot(&db, &workspace_id).map_err(|e| e.to_string())?;
            assert_eq!(state["journals"][0]["instructionRisk"], "high");
            assert_eq!(state["journals"][0]["distilledAt"], TIME);
            assert_eq!(state["journals"][0]["tombstonedAt"], TIME);
            assert_eq!(state["episodes"][0]["outcome"], "failure");
            let jobs = db
                .list_search_index_jobs(&workspace_id, None)
                .map_err(|e| e.to_string())?;
            assert_eq!(jobs.len(), 3);
            for status in ["pending", "failed", "cancelled"] {
                assert_eq!(jobs.iter().filter(|job| job.status == status).count(), 1);
            }
            let resumed = jobs
                .iter()
                .find(|job| job.status == "pending")
                .ok_or("missing interrupted job")?;
            assert_eq!(resumed.documents_indexed, 0);
            assert!(
                resumed.started_at.is_none()
                    && resumed.completed_at.is_none()
                    && resumed.error_message.is_none()
            );
            assert_eq!(
                state["artifacts"]
                    .as_array()
                    .ok_or("missing artifacts")?
                    .len(),
                1
            );
            assert_eq!(state["links"].as_array().ok_or("missing links")?.len(), 1);
            assert_eq!(state["originalAudit"]["action"], "memory.create");
            if let Some(previous) = &first {
                assert_eq!(previous, &state);
            } else {
                first = Some(state);
            }
            db.close().map_err(|e| e.to_string())?;
            workspace = side;
            database = PathBuf::from(restored.restored_database_path);
        }
    }
    Ok(())
}
