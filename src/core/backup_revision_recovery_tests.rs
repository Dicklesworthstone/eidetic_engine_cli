//! No-mock recovery through create, authenticate, restore and index publication.

use super::*;
use crate::db::{CreateMemoryInput, CreateMemoryLinkInput, MemoryLinkRelation, MemoryLinkSource};
use crate::models::{MemoryId, MemoryLinkId, WorkspaceId};
use uuid::Uuid;

type TestResult = Result<(), String>;

#[path = "backup_primary_publication_tests.rs"]
mod publication;

fn source() -> Result<(tempfile::TempDir, PathBuf, PathBuf, String, String, String), String> {
    let root = tempfile::tempdir().map_err(|e| e.to_string())?;
    let workspace = root
        .path()
        .canonicalize()
        .map_err(|e| e.to_string())?
        .join("workspace");
    fs::create_dir_all(workspace.join(WORKSPACE_MARKER)).map_err(|e| e.to_string())?;
    let database = workspace.join(WORKSPACE_MARKER).join(DEFAULT_DB_FILE);
    let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    db.migrate().map_err(|e| e.to_string())?;
    let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(11)).to_string();
    let prior = MemoryId::from_uuid(Uuid::from_u128(21)).to_string();
    let head = MemoryId::from_uuid(Uuid::from_u128(22)).to_string();
    db.insert_workspace(
        &workspace_id,
        &CreateWorkspaceInput {
            path: workspace.to_string_lossy().into_owned(),
            name: Some("Recovery test".to_owned()),
        },
    )
    .map_err(|e| e.to_string())?;
    for (id, body, created, expiry) in [
        (
            &prior,
            "Café release builds used the legacy manifest.",
            "2026-05-01T00:00:00+00:00",
            Some("2099-01-01T00:00:00Z"),
        ),
        (
            &head,
            "Café release builds use the canonical manifest.",
            "2026-06-01T00:00:00+00:00",
            None,
        ),
    ] {
        db.insert_memory_with_timestamps(
            id,
            &CreateMemoryInput {
                workspace_id: workspace_id.clone(),
                level: "semantic".to_owned(),
                kind: "fact".to_owned(),
                content: body.to_owned(),
                workflow_id: None,
                confidence: 0.9,
                utility: 0.7,
                importance: 0.8,
                provenance_uri: Some("ee-export://recovery".to_owned()),
                trust_class: "agent_validated".to_owned(),
                trust_subclass: None,
                tags: vec!["release".to_owned()],
                valid_from: Some(
                    if id == &prior {
                        "2026-05-01T00:00:00Z"
                    } else {
                        "2026-06-01T00:00:00Z"
                    }
                    .to_owned(),
                ),
                valid_to: expiry.map(str::to_owned),
            },
            created,
            created,
            &prior,
        )
        .map_err(|e| e.to_string())?;
    }
    db.restore_imported_memory_supersession(&prior, "2026-06-01T00:00:00Z")
        .map_err(|e| e.to_string())?;
    db.insert_memory_link_at(
        &MemoryLinkId::from_uuid(Uuid::from_u128(23)).to_string(),
        &CreateMemoryLinkInput {
            src_memory_id: prior.clone(),
            dst_memory_id: head.clone(),
            relation: MemoryLinkRelation::Supports,
            weight: 0.75,
            confidence: 0.5,
            directed: true,
            evidence_count: 2,
            last_reinforced_at: None,
            source: MemoryLinkSource::Agent,
            created_by: Some("recovery-test".to_owned()),
            metadata_json: None,
        },
        "2026-06-01T00:00:00+00:00",
    )
    .map_err(|e| e.to_string())?;
    db.close().map_err(|e| e.to_string())?;
    Ok((root, workspace, database, workspace_id, prior, head))
}

#[test]
fn backup_revision_recovery_preserves_history_expiry_and_published_index() -> TestResult {
    for redaction in [
        RedactionLevel::None,
        RedactionLevel::Strict,
        RedactionLevel::Standard,
    ] {
        let (root, workspace, database, workspace_id, prior, head) = source()?;
        let created = create_backup(&BackupCreateOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database.clone()),
            output_dir: None,
            label: None,
            redaction_level: redaction,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.to_string())?;
        assert!(created.recovery_inventory.snapshot_coverage_complete);
        let backup = PathBuf::from(&created.backup_path);
        let lines = fs::read_to_string(backup.join(RECORDS_FILE)).map_err(|e| e.to_string())?;
        let memories: Vec<ExportMemoryRecord> = lines
            .lines()
            .filter_map(|line| {
                let value: JsonValue = serde_json::from_str(line).ok()?;
                (value["schema"] == crate::models::EXPORT_MEMORY_SCHEMA_V1)
                    .then(|| serde_json::from_value(value))
            })
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        assert_eq!(memories.len(), 2);
        let exported_prior = crate::output::jsonl_export::redact_identifier(&prior, redaction);
        let exported_head = crate::output::jsonl_export::redact_identifier(&head, redaction);
        let old = memories
            .iter()
            .find(|m| m.memory_id == exported_prior)
            .ok_or("exported prior")?;
        assert_eq!(old.superseded_at.as_deref(), Some("2026-06-01T00:00:00Z"));
        assert_eq!(old.superseded_by.as_deref(), Some(exported_head.as_str()));
        let restored_prior = import_memory_id(old, redaction).map_err(|e| e.message)?;
        let restored_head = import_memory_id(
            memories
                .iter()
                .find(|m| m.memory_id == exported_head)
                .ok_or("exported head")?,
            redaction,
        )
        .map_err(|e| e.message)?;
        let side_path = root
            .path()
            .canonicalize()
            .map_err(|e| e.to_string())?
            .join("restored");
        let result = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: backup,
            side_path: side_path.clone(),
            restore_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.to_string())?;
        assert_eq!(result.imported_memory_count, 2);
        let db =
            DbConnection::open_file(&result.restored_database_path).map_err(|e| e.to_string())?;
        assert_eq!(
            db.get_memory_superseded_at(&restored_prior)
                .map_err(|e| e.to_string())?
                .as_deref(),
            Some("2026-06-01T00:00:00Z")
        );
        assert_eq!(
            db.get_memory(&restored_prior)
                .map_err(|e| e.to_string())?
                .ok_or("restored prior")?
                .valid_to
                .as_deref(),
            Some("2099-01-01T00:00:00Z")
        );
        assert_eq!(
            db.filter_current_memory_ids(&[restored_prior.clone(), restored_head.clone()])
                .map_err(|e| e.to_string())?,
            BTreeSet::from([restored_head.clone()])
        );
        assert_eq!(
            db.list_memories(&workspace_id, None, true)
                .map_err(|e| e.to_string())?
                .len(),
            2
        );
        db.close().map_err(|e| e.to_string())?;
        let status =
            crate::core::index::get_index_status(&crate::core::index::IndexStatusOptions {
                workspace_path: side_path.clone(),
                database_path: Some(PathBuf::from(&result.restored_database_path)),
                index_dir: None,
            })
            .map_err(|e| e.to_string())?;
        assert_eq!(status.health, crate::core::index::IndexHealth::Ready);
        assert_eq!(status.db_generation, status.index_generation);
        // Historical search requires both physical versions in the index.
        // One current DB head is not a one-document index. Prove the public
        // temporal behavior instead of discarding history to satisfy a count.
        assert_eq!(status.index_document_count, Some(2));
        for (reference, expected) in [
            ("2026-07-01T00:00:00Z", restored_head.as_str()),
            ("2026-05-15T00:00:00Z", restored_prior.as_str()),
            ("2026-06-01T00:00:00Z", restored_head.as_str()),
        ] {
            let search =
                crate::core::search::run_search_unaudited(&crate::core::search::SearchOptions {
                    workspace_path: side_path.clone(),
                    database_path: Some(PathBuf::from(&result.restored_database_path)),
                    index_dir: None,
                    query: "release builds manifest".to_owned(),
                    limit: 10,
                    speed: crate::search::SpeedMode::Default,
                    explain: false,
                    as_of: Some(
                        chrono::DateTime::parse_from_rfc3339(reference)
                            .map_err(|e| e.to_string())?
                            .with_timezone(&Utc),
                    ),
                    include_tombstoned: false,
                    include_expired: false,
                    include_future: false,
                    include_stale: false,
                    relevance_floor: Some(0.0),
                    dedup_mode: crate::core::search::SearchDedupMode::DocId,
                    source_mode: crate::core::search::SearchSourceMode::LexicalOnly,
                    strict_source_mode: true,
                    memory_scope: crate::models::MemoryScope::Workspace,
                    strict_scope: false,
                })
                .map_err(|e| e.to_string())?;
            assert_eq!(
                search
                    .results
                    .iter()
                    .map(|hit| hit.doc_id.as_str())
                    .collect::<Vec<_>>(),
                vec![expected],
                "{redaction:?} at {reference}: {:?}",
                search.degraded,
            );
        }
    }
    Ok(())
}

#[test]
fn backup_revision_recovery_blocks_same_count_corruption_before_publication() -> TestResult {
    let (root, workspace, database, _, _, head) = source()?;
    let created = create_backup(&BackupCreateOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        output_dir: None,
        label: None,
        redaction_level: RedactionLevel::None,
        include_derived: false,
        include_graph_cache: false,
        dry_run: false,
    })
    .map_err(|e| e.to_string())?;
    let side_path = root
        .path()
        .canonicalize()
        .map_err(|e| e.to_string())?
        .join("refused");
    let options = BackupRestoreOptions {
        workspace_path: workspace.clone(),
        backup_path: PathBuf::from(&created.backup_path),
        side_path: side_path.clone(),
        restore_graph_cache: false,
        dry_run: false,
    };
    let failed = restore_backup_to_side_path_with_verification_hook(&options, |staged| {
        let db = DbConnection::open_file(staged).map_err(work_history_error)?;
        let original = db
            .get_memory(&head)
            .map_err(work_history_error)?
            .ok_or_else(|| work_history_error("fixture head absent"))?;
        assert!(
            db.update_memory_trust_class(&head, "agent_assertion")
                .map_err(work_history_error)?
        );
        db.restore_imported_memory_updated_at(&head, &original.updated_at)
            .map_err(work_history_error)?;
        assert_eq!(
            db.count_table_rows("memories")
                .map_err(work_history_error)?,
            2
        );
        db.close().map_err(work_history_error)
    })
    .expect_err("same-count corruption must prevent publication");
    assert!(
        failed.message().contains("Restored memory graph"),
        "{}",
        failed.message()
    );
    assert!(!side_path.join(WORKSPACE_MARKER).exists());
    assert!(
        side_path
            .read_dir()
            .map_err(|e| e.to_string())?
            .next()
            .is_some(),
        "unpublished diagnostic staging retained"
    );
    let source = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    assert_eq!(
        source
            .get_memory(&head)
            .map_err(|e| e.to_string())?
            .ok_or("source head")?
            .trust_class,
        "agent_validated"
    );
    source.close().map_err(|e| e.to_string())?;
    let verified = verify_backup(&BackupVerifyOptions {
        workspace_path: workspace,
        backup_path: PathBuf::from(created.backup_path),
    })
    .map_err(|e| e.to_string())?;
    assert_eq!(verified.status, "verified");
    Ok(())
}
