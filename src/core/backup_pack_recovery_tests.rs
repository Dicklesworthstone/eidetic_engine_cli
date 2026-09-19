use std::path::PathBuf;

use uuid::Uuid;

use super::*;
use crate::config::WORKSPACE_MARKER;
use crate::core::backup::tests::{fixture as memory_fixture, seed_recovery_pack};
use crate::core::backup::{
    BackupCassSessionRecord, BackupCreateOptions, BackupRestoreOptions, BackupVerifyOptions,
    create_backup, hash_bytes, restore_backup_to_side_path,
    restore_backup_to_side_path_with_verification_hook, verify_backup, work_history_error,
};
use crate::db::{
    CreateEvidenceSpanInput, CreatePackEvidenceItemInput, CreatePackRecordInput,
    EvidenceProducerKind,
};
use crate::models::{EvidenceId, PackId, RedactionLevel, SessionId, WorkspaceId};

type TestResult = Result<(), String>;

struct Fixture {
    _root: tempfile::TempDir,
    database: PathBuf,
    options: BackupRestoreOptions,
    ids: Vec<String>,
}

fn fixture(redaction: RedactionLevel) -> Result<Fixture, String> {
    let (root, workspace, database) = memory_fixture().map_err(|e| e.message())?;
    let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    // Nonlexical admission order; the second pack models a pre-ledger store.
    let modern = seed_recovery_pack(&db, 80)?;
    let legacy = seed_recovery_pack(&db, 3)?;
    let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
    let session_id = SessionId::from_uuid(Uuid::from_u128(20)).to_string();
    let session = BackupCassSessionRecord {
        id: session_id.clone(),
        workspace_id: workspace_id.clone(),
        source_locator_hash: hash_bytes(b"cass://portable-session"),
        source_metadata_hash: None,
        agent_name: Some("codex".to_owned()),
        model: None,
        started_at: Some("2026-09-01T00:00:00Z".to_owned()),
        ended_at: Some("2026-09-01T00:01:00Z".to_owned()),
        message_count: 2,
        token_count: Some(64),
        content_hash: hash_bytes(b"portable CASS session"),
        imported_at: "2026-09-01T00:02:00Z".to_owned(),
        updated_at: "2026-09-01T00:03:00Z".to_owned(),
    };
    db.insert_session_for_recovery(&session.into_restored(workspace_id.clone()))
        .map_err(|e| e.to_string())?;
    let evidence_id = EvidenceId::from_uuid(Uuid::from_u128(21)).to_string();
    let excerpt = "The release workflow verified the signed artifact successfully.";
    db.insert_evidence_span(
        &evidence_id,
        &CreateEvidenceSpanInput {
            workspace_id: workspace_id.clone(),
            session_id,
            memory_id: None,
            producer_kind: EvidenceProducerKind::CassImport,
            cass_span_id: "cass://recovery/1".to_owned(),
            span_kind: "message".to_owned(),
            start_line: 1,
            end_line: 2,
            start_byte: None,
            end_byte: None,
            role: Some("assistant".to_owned()),
            excerpt: excerpt.to_owned(),
            content_hash: hash_bytes(excerpt.as_bytes()),
            metadata_json: None,
            inherited_redaction_classes: Vec::new(),
        },
    )
    .map_err(|e| e.to_string())?;
    let span = db
        .get_evidence_span(&evidence_id)
        .map_err(|e| e.to_string())?
        .ok_or("missing fixture evidence")?;
    let evidence_pack = PackId::from_uuid(Uuid::from_u128(22)).to_string();
    db.insert_pack_record_with_timings_task_lens_and_evidence(
        &evidence_pack,
        &CreatePackRecordInput {
            workspace_id,
            query: "release verification".to_owned(),
            profile: "balanced".to_owned(),
            max_tokens: 4000,
            used_tokens: 32,
            item_count: 1,
            omitted_count: 0,
            pack_hash: hash_bytes(b"historical native evidence pack"),
            degraded_json: None,
            created_by: None,
        },
        &[],
        &[CreatePackEvidenceItemInput {
            pack_id: evidence_pack.clone(),
            evidence_id,
            entity_revision: span.pack_entity_revision(),
            rank: 1,
            section: "evidence".to_owned(),
            estimated_tokens: 32,
            relevance: 0.8,
            utility: 0.6,
            why: "Direct release verification evidence.".to_owned(),
            provenance_json: "{}".to_owned(),
            trust_class: "cass_evidence".to_owned(),
            trust_subclass: None,
        }],
        &[],
        None,
    )
    .map_err(|e| e.to_string())?;
    db.execute_raw(&format!(
        "UPDATE pack_records SET ledger_json = NULL, ledger_hash = NULL WHERE id IN ('{}', '{evidence_pack}')",
        legacy.record.id,
    ))
    .map_err(|e| e.to_string())?;
    let ids = vec![modern.record.id, legacy.record.id, evidence_pack];
    assert!(
        db.get_pack_history_for_recovery(&ids[0])
            .map_err(|e| e.to_string())?
            .record
            .ledger_json
            .is_some()
    );
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
    let options = BackupRestoreOptions {
        workspace_path: workspace,
        backup_path: PathBuf::from(backup.backup_path),
        side_path: root.path().join("restored"),
        restore_graph_cache: false,
        dry_run: false,
    };
    Ok(Fixture {
        _root: root,
        database,
        options,
        ids,
    })
}

#[test]
fn pack_content_fence_preserves_modern_legacy_and_native_evidence_history() -> TestResult {
    for redaction in [
        RedactionLevel::None,
        RedactionLevel::Standard,
        RedactionLevel::Full,
    ] {
        let fixture = fixture(redaction)?;
        let restored = restore_backup_to_side_path(&fixture.options).map_err(|e| e.message())?;
        assert_eq!(restored.restored_pack_history.records, 3);
        assert_eq!(restored.restored_pack_history.evidence_items, 1);
        let db =
            DbConnection::open_file(&restored.restored_database_path).map_err(|e| e.to_string())?;
        let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
        assert_eq!(
            db.list_pack_record_ids_for_recovery(&workspace_id)
                .map_err(|e| e.to_string())?,
            fixture.ids
        );
        for (index, id) in fixture.ids.iter().enumerate() {
            let history = db
                .get_pack_history_for_recovery(id)
                .map_err(|e| e.to_string())?;
            assert_eq!(history.record.ledger_json.is_some(), index == 0);
        }
        let sessions = db.list_sessions(&workspace_id).map_err(|e| e.to_string())?;
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].source_path.is_none());
        assert_eq!(
            sessions[0].cass_session_id,
            crate::core::backup::portable_cass_session_id(&sessions[0].id)
        );
        let spans = db
            .list_evidence_spans_for_workspace(&workspace_id)
            .map_err(|e| e.to_string())?;
        assert_eq!(spans.len(), 1);
        if redaction == RedactionLevel::Full {
            assert_eq!(spans[0].search_eligibility, "denied");
            assert_eq!(spans[0].pack_eligibility, "denied");
            assert!(spans[0].canonical_excerpt_hash.is_none());
        }
        db.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn assert_corruption_refused(
    table: &str,
    statement: impl FnOnce(&[String]) -> String,
) -> TestResult {
    assert_corruption_refused_with_redaction(table, RedactionLevel::None, statement)
}

fn assert_corruption_refused_with_redaction(
    table: &str,
    redaction: RedactionLevel,
    statement: impl FnOnce(&[String]) -> String,
) -> TestResult {
    let fixture = fixture(redaction)?;
    let sql = statement(&fixture.ids);
    let error = restore_backup_to_side_path_with_verification_hook(&fixture.options, |path| {
        let db = DbConnection::open_file(path).map_err(work_history_error)?;
        let tables = db.list_user_tables().map_err(work_history_error)?;
        let before = tables
            .iter()
            .map(|name| db.count_table_rows(name))
            .collect::<Result<Vec<_>, _>>()
            .map_err(work_history_error)?;
        db.execute_raw(&sql).map_err(work_history_error)?;
        let after = tables
            .iter()
            .map(|name| db.count_table_rows(name))
            .collect::<Result<Vec<_>, _>>()
            .map_err(work_history_error)?;
        assert_eq!(before, after);
        // A count-only fence AND an internally valid replay ledger would accept
        // these corruptions. Fidelity to the admitted source must still fail.
        for id in &fixture.ids {
            db.get_pack_history_for_recovery(id)
                .map_err(work_history_error)?;
        }
        db.close().map_err(work_history_error)
    })
    .err()
    .ok_or_else(|| format!("published changed {table}"))?;
    assert!(
        error
            .message()
            .contains(&format!("Restored durable content differs for {table}")),
        "{}",
        error.message()
    );
    assert!(!error.message().contains("MUTATION_SENTINEL"));
    assert!(!fixture.options.side_path.join(WORKSPACE_MARKER).exists());
    let source = DbConnection::open_file(&fixture.database).map_err(|e| e.to_string())?;
    assert!(
        source
            .get_pack_history_for_recovery(&fixture.ids[0])
            .map_err(|e| e.to_string())?
            .record
            .ledger_json
            .is_some()
    );
    source.close().map_err(|e| e.to_string())?;
    let verified = verify_backup(&BackupVerifyOptions {
        workspace_path: fixture.options.workspace_path,
        backup_path: fixture.options.backup_path,
    })
    .map_err(|e| e.message())?;
    assert_eq!(verified.status, "verified");
    Ok(())
}

#[test]
fn pack_content_fence_rejects_erased_replay_ledger() -> TestResult {
    assert_corruption_refused("pack_records", |ids| {
        format!(
            "UPDATE pack_records SET ledger_json = NULL, ledger_hash = NULL WHERE id = '{}'",
            ids[0],
        )
    })
}

#[test]
fn pack_content_fence_rejects_changed_legacy_selection() -> TestResult {
    assert_corruption_refused("pack_items", |ids| {
        format!(
            "UPDATE pack_items SET why = 'MUTATION_SENTINEL' WHERE pack_id = '{}'",
            ids[1],
        )
    })
}

#[test]
fn pack_content_fence_rejects_changed_native_evidence() -> TestResult {
    assert_corruption_refused("pack_evidence_items", |_| {
        "UPDATE pack_evidence_items SET trust_subclass = 'MUTATION_SENTINEL'".to_owned()
    })
}

#[test]
fn pack_content_fence_rejects_omission_token_drift() -> TestResult {
    assert_corruption_refused("pack_omissions", |ids| {
        format!(
            "UPDATE pack_omissions SET estimated_tokens = 33 WHERE pack_id = '{}'",
            ids[1],
        )
    })
}

#[test]
fn pack_content_fence_rejects_changed_impression_join_key() -> TestResult {
    assert_corruption_refused("pack_candidate_impressions", |_| {
        format!(
            "UPDATE pack_candidate_impressions SET query_hash = '{}'",
            hash_bytes(b"changed query"),
        )
    })
}

#[test]
fn pack_content_fence_rejects_changed_baseline_chronology() -> TestResult {
    assert_corruption_refused("pack_baselines", |_| {
        "UPDATE pack_baselines SET created_at = '2026-09-02T00:00:00Z'".to_owned()
    })
}

#[test]
fn pack_content_fence_rejects_reordered_admission_without_changing_any_record() -> TestResult {
    assert_corruption_refused("pack_records admission order", |ids| {
        format!(
            "UPDATE pack_records SET rowid = 100000 WHERE id = '{}'",
            ids[0],
        )
    })
}

#[test]
fn pack_content_expectation_rejects_duplicate_chunks_and_backup_substitution() -> TestResult {
    let fixture = fixture(RedactionLevel::None)?;
    let mut assets = Vec::new();
    let directory = fixture.options.backup_path.join("derived/pack-history");
    for entry in directory.read_dir().map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        assets.push(BackupRestoredDerivedAssetReport {
            path: path
                .file_name()
                .ok_or("asset filename")?
                .to_string_lossy()
                .into_owned(),
            kind: "pack_history".to_owned(),
            restore_path: path.to_string_lossy().into_owned(),
            lab_episode_path: None,
        });
    }
    let first: BackupPackHistory = serde_json::from_value(
        read_restored_derived_json(assets.first().ok_or("missing pack assets")?)
            .map_err(|e| e.message())?,
    )
    .map_err(|e| e.to_string())?;
    assert!(PackExpectation::from_assets(&assets, "substituted", &first.workspace_id).is_err());
    PackExpectation::from_assets(&assets, &first.backup_id, &first.workspace_id)
        .map_err(|e| e.message())?;
    assets.push(assets[0].clone());
    assert!(PackExpectation::from_assets(&assets, &first.backup_id, &first.workspace_id).is_err());
    Ok(())
}

#[test]
fn evidence_content_fence_rejects_resurrected_host_path() -> TestResult {
    assert_corruption_refused("sessions", |_| {
        "UPDATE sessions SET source_path = '/MUTATION_SENTINEL/private-session.json'".to_owned()
    })
}

#[test]
fn evidence_content_fence_rejects_changed_session_identity_hash() -> TestResult {
    assert_corruption_refused("sessions", |_| {
        format!(
            "UPDATE sessions SET content_hash = '{}'",
            hash_bytes(b"substituted transcript"),
        )
    })
}

#[test]
fn evidence_content_fence_rejects_changed_excerpt_with_consistent_content_hash() -> TestResult {
    assert_corruption_refused("evidence_spans", |_| {
        format!(
            "UPDATE evidence_spans SET excerpt = 'MUTATION_SENTINEL', content_hash = '{}'",
            hash_bytes(b"MUTATION_SENTINEL"),
        )
    })
}

#[test]
fn evidence_content_fence_rejects_promoted_redacted_evidence() -> TestResult {
    assert_corruption_refused_with_redaction("evidence_spans", RedactionLevel::Full, |_| {
        "UPDATE evidence_spans SET pack_eligibility = 'admitted', search_eligibility = 'admitted'"
            .to_owned()
    })
}

#[test]
fn evidence_content_fence_rejects_changed_provenance_epoch() -> TestResult {
    assert_corruption_refused("evidence_spans", |_| {
        "UPDATE evidence_spans SET security_policy_epoch = security_policy_epoch + 1".to_owned()
    })
}

/// Compare the durable rows across independently created recovery points, not
/// the deliberately different backup IDs, capture times or artifact hashes.
#[test]
fn context_history_survives_rebackup_without_regaining_evidence_authority() -> TestResult {
    fn snapshot(
        path: &str,
        workspace_id: &str,
    ) -> Result<
        (
            Vec<StoredPackHistory>,
            Vec<crate::db::StoredSession>,
            Vec<crate::db::StoredEvidenceSpan>,
        ),
        String,
    > {
        let db = DbConnection::open_file(path).map_err(|e| e.to_string())?;
        let packs = db
            .list_pack_record_ids_for_recovery(workspace_id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|id| db.get_pack_history_for_recovery(&id))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let sessions = db.list_sessions(workspace_id).map_err(|e| e.to_string())?;
        let evidence = db
            .list_evidence_spans_for_workspace(workspace_id)
            .map_err(|e| e.to_string())?;
        db.close().map_err(|e| e.to_string())?;
        Ok((packs, sessions, evidence))
    }

    for redaction in [RedactionLevel::None, RedactionLevel::Full] {
        let fixture = fixture(redaction)?;
        let first = restore_backup_to_side_path(&fixture.options).map_err(|e| e.message())?;
        let workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string();
        let before = snapshot(&first.restored_database_path, &workspace_id)?;
        assert_eq!(before.0.len(), 3);
        assert_eq!(before.1.len(), 1);
        assert_eq!(before.2.len(), 1);
        if redaction == RedactionLevel::Full {
            assert_eq!(before.2[0].pack_eligibility, "denied");
            assert_eq!(before.2[0].search_eligibility, "denied");
        }

        let rebackup = create_backup(&BackupCreateOptions {
            workspace_path: fixture.options.side_path.clone(),
            database_path: Some(PathBuf::from(&first.restored_database_path)),
            output_dir: None,
            label: None,
            redaction_level: redaction,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.message())?;
        assert_ne!(rebackup.backup_id, first.backup_id);
        let second = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: fixture.options.side_path,
            backup_path: PathBuf::from(rebackup.backup_path),
            side_path: fixture._root.path().join("restored-again"),
            restore_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.message())?;
        assert_eq!(
            snapshot(&second.restored_database_path, &workspace_id)?,
            before,
            "pack admission order, exact replay state, portable locators and every evidence field must survive a second recovery point"
        );
    }
    Ok(())
}
