//! Final-boundary primary graph checks through the real backup/restore path.

use super::*;

fn primary_state(db: &DbConnection, workspace_id: &str) -> Result<JsonValue, DomainError> {
    let memories = db
        .list_memories(workspace_id, None, true)
        .map_err(work_history_error)?;
    let mut revisions = Vec::new();
    for memory in &memories {
        revisions.push(serde_json::json!({
            "id": memory.id,
            "logicalId": db.get_memory_logical_id(&memory.id).map_err(work_history_error)?,
            "supersededAt": db.get_memory_superseded_at(&memory.id).map_err(work_history_error)?,
            "tags": db.get_memory_tags(&memory.id).map_err(work_history_error)?,
        }));
    }
    let link = db
        .get_memory_link(&MemoryLinkId::from_uuid(Uuid::from_u128(23)).to_string())
        .map_err(work_history_error)?;
    Ok(serde_json::json!({"memories": memories, "revisions": revisions, "link": link}))
}

fn assert_late_primary_change_refused(sql: &str, rewrite_records: bool) -> TestResult {
    let (root, workspace, database, workspace_id, _, head) = source()?;
    let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    let original = primary_state(&db, &workspace_id).map_err(|e| e.to_string())?;
    db.close().map_err(|e| e.to_string())?;
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
    let options = BackupRestoreOptions {
        workspace_path: workspace.clone(),
        backup_path: PathBuf::from(&created.backup_path),
        side_path: root
            .path()
            .canonicalize()
            .map_err(|e| e.to_string())?
            .join("refused"),
        restore_graph_cache: false,
        dry_run: false,
    };
    let mut staged_database = None;
    let error = restore_backup_to_side_path_with_recovery_hooks(
        &options,
        |_| Ok(()),
        |path| {
            let store = path
                .parent()
                .ok_or_else(|| work_history_error("missing staged store"))?;
            assert!(
                store.join("index/meta.json").is_file(),
                "exercise the final fence"
            );
            staged_database = Some(path.to_path_buf());
            let db = DbConnection::open_file(path).map_err(work_history_error)?;
            let tables = db.list_user_tables().map_err(work_history_error)?;
            let counts = |db: &DbConnection| {
                tables
                    .iter()
                    .map(|table| db.count_table_rows(table))
                    .collect::<Result<Vec<_>, _>>()
            };
            let before_counts = counts(&db).map_err(work_history_error)?;
            let before = primary_state(&db, &workspace_id)?;
            db.execute_raw(sql).map_err(work_history_error)?;
            assert_ne!(
                primary_state(&db, &workspace_id)?,
                before,
                "mutation must execute"
            );
            assert_eq!(counts(&db).map_err(work_history_error)?, before_counts);
            db.close().map_err(work_history_error)?;
            if rewrite_records {
                let records = store
                    .join(DEFAULT_RESTORE_DIR)
                    .join(&created.backup_id)
                    .join(RECORDS_FILE);
                let before = fs::read_to_string(&records).map_err(work_history_error)?;
                let mut rows = before
                    .lines()
                    .map(serde_json::from_str::<JsonValue>)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(work_history_error)?;
                let mut changed = 0;
                for row in &mut rows {
                    if row["schema"] == crate::models::EXPORT_MEMORY_SCHEMA_V1
                        && row["memory_id"] == head
                    {
                        row["content"] = serde_json::json!("RECOVERY_PRIVATE_CANARY");
                        changed += 1;
                    }
                }
                assert_eq!(changed, 1);
                let after = rows
                    .iter()
                    .map(serde_json::to_string)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(work_history_error)?
                    .join("\n")
                    + "\n";
                assert_ne!(after, before);
                fs::write(records, after).map_err(work_history_error)?;
            }
            Ok(())
        },
    )
    .err()
    .ok_or("published primary records changed after rebuilding")?;
    assert!(
        error.message().contains("Restored memory graph"),
        "{}",
        error.message()
    );
    for private in [
        "RECOVERY_PRIVATE_CANARY",
        head.as_str(),
        workspace_id.as_str(),
    ] {
        assert!(!error.message().contains(private));
    }
    assert!(!options.side_path.join(WORKSPACE_MARKER).exists());
    assert!(
        staged_database
            .ok_or("final callback was not executed")?
            .is_file()
    );
    let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    assert_eq!(
        primary_state(&db, &workspace_id).map_err(|e| e.to_string())?,
        original
    );
    db.close().map_err(|e| e.to_string())?;
    assert_eq!(
        verify_backup(&BackupVerifyOptions {
            workspace_path: workspace,
            backup_path: options.backup_path,
        })
        .map_err(|e| e.to_string())?
        .status,
        "verified"
    );
    Ok(())
}

#[test]
fn final_fence_rejects_changed_primary_content() -> TestResult {
    assert_late_primary_change_refused(
        "UPDATE memories SET content = 'RECOVERY_PRIVATE_CANARY' WHERE superseded_at IS NULL",
        false,
    )
}

#[test]
fn final_fence_rejects_primary_trust_escalation() -> TestResult {
    assert_late_primary_change_refused("UPDATE memories SET trust_class = 'human_explicit'", false)
}

#[test]
fn final_fence_rejects_revived_superseded_revision() -> TestResult {
    assert_late_primary_change_refused(
        "UPDATE memories SET superseded_at = NULL WHERE superseded_at IS NOT NULL",
        false,
    )
}

#[test]
fn final_fence_rejects_silently_retired_current_revision() -> TestResult {
    assert_late_primary_change_refused(
        "UPDATE memories SET superseded_at = '2026-09-01T00:00:00Z' WHERE superseded_at IS NULL",
        false,
    )
}

#[test]
fn final_fence_rejects_detached_revision_family() -> TestResult {
    assert_late_primary_change_refused(
        "UPDATE memories SET logical_id = id WHERE superseded_at IS NULL",
        false,
    )
}

#[test]
fn final_fence_rejects_rewritten_memory_tags() -> TestResult {
    assert_late_primary_change_refused(
        "UPDATE memory_tags SET tag = 'RECOVERY_PRIVATE_CANARY'",
        false,
    )
}

#[test]
fn final_fence_rejects_changed_relationship_evidence() -> TestResult {
    assert_late_primary_change_refused(
        "UPDATE memory_links SET weight = 0.125, evidence_count = 99",
        false,
    )
}

#[test]
fn final_fence_rejects_changed_primary_expiry() -> TestResult {
    assert_late_primary_change_refused(
        "UPDATE memories SET valid_to = '2098-01-01T00:00:00Z'",
        false,
    )
}

#[test]
fn final_fence_does_not_reread_a_paired_archive_and_database_rewrite() -> TestResult {
    assert_late_primary_change_refused(
        "UPDATE memories SET content = 'RECOVERY_PRIVATE_CANARY' WHERE superseded_at IS NULL",
        true,
    )
}
