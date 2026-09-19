//! Real restore/publication checks, including corruptions that preserve counts.

use super::tests::{fixture, recovery_agent_profile, recovery_feedback, recovery_rule};
use super::*;
use crate::models::{MemoryId, WorkspaceId};
use uuid::Uuid;

type TestResult = Result<(), String>;

fn learning_fixture(
    redaction: RedactionLevel,
) -> Result<(tempfile::TempDir, PathBuf, BackupRestoreOptions), String> {
    let (root, workspace, database) = fixture().map_err(|e| e.message())?;
    let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
    let memory_id = MemoryId::from_uuid(Uuid::from_u128(2)).to_string();
    let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    let rule = recovery_rule(&workspace_id, 0);
    db.with_transaction(|| {
        db.insert_procedural_rule_for_recovery(&rule)?;
        db.insert_procedural_rule_for_recovery(&recovery_rule(&workspace_id, 1))?;
        db.restore_rule_source(&rule.id, &memory_id)?;
        db.restore_rule_tag(&rule.id, "release")?;
        db.insert_feedback_event_for_recovery(&recovery_feedback(&workspace_id, &memory_id, 1))?;
        db.insert_agent_context_profile_for_recovery(&recovery_agent_profile(
            &workspace_id,
            &memory_id,
            "RecoveryAgent",
        ))
    })
    .map_err(|e| e.to_string())?;
    db.close().map_err(|e| e.to_string())?;
    let backup = create_backup(&BackupCreateOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        output_dir: None,
        label: None,
        redaction_level: redaction,
        include_derived: false,
        include_graph_cache: false,
        dry_run: false,
    })
    .map_err(|e| e.message())?;
    assert!(backup.recovery_inventory.snapshot_coverage_complete);
    let restore = BackupRestoreOptions {
        workspace_path: workspace,
        backup_path: PathBuf::from(backup.backup_path),
        side_path: root.path().join("restored"),
        restore_graph_cache: false,
        dry_run: false,
    };
    Ok((root, database, restore))
}

#[test]
fn learned_content_survives_restore_and_rebackup_without_replaying_feedback() -> TestResult {
    for redaction in [
        RedactionLevel::None,
        RedactionLevel::Standard,
        RedactionLevel::Strict,
    ] {
        let (root, _source_database, mut options) = learning_fixture(redaction)?;
        for generation in 0..2 {
            let restored = restore_backup_to_side_path(&options).map_err(|e| e.message())?;
            assert_eq!(restored.restored_rule_count, 2);
            assert_eq!(restored.restored_rule_source_count, 1);
            assert_eq!(restored.restored_rule_tag_count, 1);
            assert_eq!(restored.restored_feedback_count, 1);
            assert_eq!(restored.restored_agent_profile_count, 1);
            let db = DbConnection::open_file(&restored.restored_database_path)
                .map_err(|e| e.to_string())?;
            let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
            let original = recovery_rule(&workspace_id, 0);
            assert_eq!(
                db.get_procedural_rule(&original.id)
                    .map_err(|e| e.to_string())?,
                Some(original)
            );
            let feedback = db
                .list_feedback_events(&workspace_id)
                .map_err(|e| e.to_string())?;
            assert_eq!(feedback.len(), 1);
            assert_eq!(
                feedback[0].applied_at.as_deref(),
                Some("2026-09-01T00:01:00Z")
            );
            assert_eq!(feedback[0].weight, 0.5);
            let profiles = db
                .list_agent_context_profiles_for_recovery(&workspace_id)
                .map_err(|e| e.to_string())?;
            assert_eq!(profiles.len(), 1);
            assert_eq!(profiles[0].weight_cached, 0.04);
            db.close().map_err(|e| e.to_string())?;
            if generation == 0 {
                let backup = create_backup(&BackupCreateOptions {
                    workspace_path: options.side_path.clone(),
                    database_path: Some(PathBuf::from(restored.restored_database_path)),
                    output_dir: None,
                    label: None,
                    redaction_level: redaction,
                    include_derived: false,
                    include_graph_cache: false,
                    dry_run: false,
                })
                .map_err(|e| e.message())?;
                options.workspace_path = options.side_path;
                options.backup_path = PathBuf::from(backup.backup_path);
                options.side_path = root.path().join("restored-again");
            }
        }
    }
    Ok(())
}

fn assert_same_count_corruption_refused(table: &str, sql: &str) -> TestResult {
    let (_root, source_database, options) = learning_fixture(RedactionLevel::None)?;
    let error = restore_backup_to_side_path_with_verification_hook(&options, |path| {
        let db = DbConnection::open_file(path).map_err(work_history_error)?;
        let tables = db.list_user_tables().map_err(work_history_error)?;
        let before = tables
            .iter()
            .map(|name| db.count_table_rows(name))
            .collect::<Result<Vec<_>, _>>()
            .map_err(work_history_error)?;
        db.execute_raw(sql).map_err(work_history_error)?;
        let after = tables
            .iter()
            .map(|name| db.count_table_rows(name))
            .collect::<Result<Vec<_>, _>>()
            .map_err(work_history_error)?;
        assert_eq!(
            before, after,
            "corruption must preserve every table's count"
        );
        db.close().map_err(work_history_error)
    })
    .err()
    .ok_or_else(|| format!("published changed {table} content"))?;
    assert!(
        error
            .message()
            .contains(&format!("Restored durable content differs for {table}")),
        "{}",
        error.message()
    );
    assert!(!error.message().contains("MUTATION_SENTINEL"));
    assert!(!error.message().contains("RecoveryAgent"));
    assert!(!options.side_path.join(WORKSPACE_MARKER).exists());
    let db = DbConnection::open_file(&source_database).map_err(|e| e.to_string())?;
    let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
    let original = recovery_rule(&workspace_id, 0);
    assert_eq!(
        db.get_procedural_rule(&original.id)
            .map_err(|e| e.to_string())?,
        Some(original)
    );
    db.close().map_err(|e| e.to_string())?;
    let verified = verify_backup(&BackupVerifyOptions {
        workspace_path: options.workspace_path,
        backup_path: options.backup_path,
    })
    .map_err(|e| e.message())?;
    assert_eq!(verified.status, "verified");
    Ok(())
}

#[test]
fn learned_content_fence_rejects_changed_rule_authority() -> TestResult {
    assert_same_count_corruption_refused(
        "procedural_rules",
        "UPDATE procedural_rules SET protected = 0, content = 'MUTATION_SENTINEL'",
    )
}

#[test]
fn learned_content_fence_rejects_reassigned_rule_evidence() -> TestResult {
    let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
    let other = recovery_rule(&workspace_id, 1).id;
    assert_same_count_corruption_refused(
        "rule_source_memories",
        &format!("UPDATE rule_source_memories SET rule_id = '{other}'"),
    )
}

#[test]
fn learned_content_fence_rejects_changed_rule_tags() -> TestResult {
    assert_same_count_corruption_refused(
        "rule_tags",
        "UPDATE rule_tags SET tag = 'MUTATION_SENTINEL'",
    )
}

#[test]
fn learned_content_fence_rejects_feedback_reapplication() -> TestResult {
    assert_same_count_corruption_refused(
        "feedback_events",
        "UPDATE feedback_events SET applied_at = NULL",
    )
}

#[test]
fn learned_content_fence_rejects_changed_agent_bias() -> TestResult {
    // Stay inside the +/-0.05 schema bound, so this exercises the publication
    // fence after a successful write rather than an earlier CHECK refusal.
    assert_same_count_corruption_refused(
        "agent_context_profiles",
        "UPDATE agent_context_profiles SET weight_cached = -0.04",
    )
}
