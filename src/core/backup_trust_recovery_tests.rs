use std::path::PathBuf;

use super::*;
use crate::config::WORKSPACE_MARKER;
use crate::core::backup::{
    BackupCreateOptions, BackupRestoreOptions, BackupVerifyOptions, authenticate_backup_manifest,
    backup_verification_issue_is_blocking, create_backup, hash_bytes, read_backup_manifest,
    restore_backup_to_side_path, restore_backup_to_side_path_with_recovery_hooks,
    restore_backup_to_side_path_with_verification_hook, verify_backup, work_history_error,
};
use crate::db::{
    CreateMemoryInput, StoredAgent, StoredCertificateRecord, StoredTrustQuarantine,
    compute_memory_provenance_chain_hash,
};
use crate::models::{
    MEMORY_SEAL_PLACEHOLDER_CONTENT, MemoryId, MemorySeal, RedactionLevel, WorkspaceId,
    memory_seal_commitment,
};
use crate::policy::import_auth::{authenticate_artifact, canonical_record_hash, verify_artifact};
use crate::policy::store_auth::{MacDomain, StoreAuthRoot, workspace_keys_dir};
use uuid::Uuid;

type TestResult = Result<(), String>;
type TrustState = (
    Vec<MemorySeal>,
    Vec<StoredTrustQuarantine>,
    Vec<StoredCertificateRecord>,
    Vec<StoredAgent>,
);

struct Fixture {
    root: tempfile::TempDir,
    database: PathBuf,
    options: BackupRestoreOptions,
}

fn workspace_id() -> String {
    WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string()
}

fn read_state(db: &DbConnection) -> Result<TrustState, String> {
    let ws = workspace_id();
    Ok((
        db.list_memory_seals_for_recovery(&ws)
            .map_err(|e| e.to_string())?,
        db.list_trust_quarantine(&ws, false)
            .map_err(|e| e.to_string())?,
        db.list_certificates_for_recovery(&ws)
            .map_err(|e| e.to_string())?,
        db.list_agents_for_recovery(&ws)
            .map_err(|e| e.to_string())?,
    ))
}

fn fixture(level: RedactionLevel) -> Result<Fixture, String> {
    let (root, workspace, database) =
        crate::core::backup::tests::fixture().map_err(|e| e.message())?;
    let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    let ws = workspace_id();
    let memory_id = MemoryId::from_uuid(Uuid::from_u128(2)).to_string();
    let time = "2026-09-01T00:00:00Z";
    db.execute_raw(&format!(
        "UPDATE memories SET content = '{MEMORY_SEAL_PLACEHOLDER_CONTENT}' WHERE id = '{memory_id}'"
    ))
    .map_err(|e| e.to_string())?;
    db.insert_memory_seal_for_recovery(&MemorySeal {
        memory_id,
        content_commitment: memory_seal_commitment(b"private unrevealed result"),
        sealed_at: time.to_owned(),
        revealed_at: None,
        reveal_verified: None,
    })
    .map_err(|e| e.to_string())?;
    db.insert_trust_quarantine_for_recovery(&StoredTrustQuarantine {
        workspace_id: ws.clone(),
        source_uri: "ee-test://backup".to_owned(),
        first_event_at: time.to_owned(),
        last_event_at: time.to_owned(),
        harmful_event_count: 7,
        quarantined_until: Some("2099-01-01T00:00:00Z".to_owned()),
        reason: "Repeated harmful observations".to_owned(),
        status: "active".to_owned(),
        created_at: time.to_owned(),
        updated_at: time.to_owned(),
    })
    .map_err(|e| e.to_string())?;
    db.insert_certificate_for_recovery(&StoredCertificateRecord {
        id: "cert_recovery_guard".to_owned(),
        workspace_id: ws.clone(),
        target_kind: "pack".to_owned(),
        target_id: "pack_historical".to_owned(),
        hash_algo: "blake3".to_owned(),
        content_hash: hash_bytes(b"historical certificate payload"),
        signature: None,
        signature_algorithm: None,
        signer: None,
        signed_at: None,
        verified_at: None,
        status: "pending".to_owned(),
        manifest_path: None,
        payload_path: None,
        metadata_json: "{}".to_owned(),
        created_at: time.to_owned(),
        updated_at: time.to_owned(),
    })
    .map_err(|e| e.to_string())?;
    for n in 0..2 {
        db.insert_agent_for_recovery(&StoredAgent {
            id: format!("agt_{n:026}"),
            workspace_id: ws.clone(),
            name: format!("HistoricalAgent{n}"),
            model: Some("historical-model".to_owned()),
            created_at: time.to_owned(),
            last_seen_at: time.to_owned(),
        })
        .map_err(|e| e.to_string())?;
    }
    let seeded = read_state(&db)?;
    assert_eq!(
        (
            seeded.0.len(),
            seeded.1.len(),
            seeded.2.len(),
            seeded.3.len()
        ),
        (1, 1, 1, 2)
    );
    db.close().map_err(|e| e.to_string())?;
    let backup = create_backup(&BackupCreateOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        output_dir: None,
        label: None,
        redaction_level: level,
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
        root,
        database,
        options,
    })
}

fn assert_refused(table: &str, sql: &str) -> TestResult {
    let fixture = fixture(RedactionLevel::None)?;
    let source = DbConnection::open_file(&fixture.database).map_err(|e| e.to_string())?;
    let original = read_state(&source)?;
    source.close().map_err(|e| e.to_string())?;
    let error = restore_backup_to_side_path_with_verification_hook(&fixture.options, |path| {
        let db = DbConnection::open_file(path).map_err(work_history_error)?;
        let before = read_state(&db).map_err(work_history_error)?;
        let tables = db.list_user_tables().map_err(work_history_error)?;
        let counts = tables
            .iter()
            .map(|t| db.count_table_rows(t))
            .collect::<Result<Vec<_>, _>>()
            .map_err(work_history_error)?;
        db.execute_raw(sql).map_err(work_history_error)?;
        let after = read_state(&db).map_err(work_history_error)?;
        assert_ne!(
            before, after,
            "mutation must really change well-formed trust rows"
        );
        let after_counts = tables
            .iter()
            .map(|t| db.count_table_rows(t))
            .collect::<Result<Vec<_>, _>>()
            .map_err(work_history_error)?;
        assert_eq!(
            counts, after_counts,
            "every table's count must stay identical"
        );
        db.close().map_err(work_history_error)
    })
    .err()
    .ok_or("published altered trust state")?;
    assert!(
        error
            .message()
            .contains(&format!("Restored durable content differs for {table}")),
        "{}",
        error.message()
    );
    assert!(!error.message().contains("PRIVATE_SENTINEL"));
    assert!(!fixture.options.side_path.join(WORKSPACE_MARKER).exists());
    let source = DbConnection::open_file(&fixture.database).map_err(|e| e.to_string())?;
    assert_eq!(read_state(&source)?, original);
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
fn trust_fence_rejects_quarantine_release() -> TestResult {
    assert_refused(
        "trust_quarantine",
        "UPDATE trust_quarantine SET status = 'released'",
    )
}

#[test]
fn trust_fence_rejects_quarantine_expiry_rewrite() -> TestResult {
    assert_refused(
        "trust_quarantine",
        "UPDATE trust_quarantine SET quarantined_until = '2020-01-01T00:00:00Z'",
    )
}

#[test]
fn trust_fence_rejects_fabricated_reveal() -> TestResult {
    assert_refused(
        "memory_seals",
        "UPDATE memory_seals SET revealed_at = '2026-09-02T00:00:00Z', reveal_verified = 1",
    )
}

#[test]
fn trust_fence_rejects_substituted_commitment() -> TestResult {
    assert_refused(
        "memory_seals",
        &format!(
            "UPDATE memory_seals SET content_commitment = '{}'",
            memory_seal_commitment(b"substituted result")
        ),
    )
}

#[test]
fn trust_fence_rejects_fabricated_certificate_verification() -> TestResult {
    assert_refused(
        "certificates",
        "UPDATE certificates SET status = 'valid', verified_at = '2026-09-02T00:00:00Z'",
    )
}

#[test]
fn trust_fence_rejects_agent_reattribution() -> TestResult {
    assert_refused("agents", "UPDATE agents SET name = 'PRIVATE_SENTINEL'")
}

#[test]
fn trust_history_survives_recovery_generations_without_gaining_authority() -> TestResult {
    for level in [RedactionLevel::None, RedactionLevel::Full] {
        let fixture = fixture(level)?;
        let first = restore_backup_to_side_path(&fixture.options).map_err(|e| e.message())?;
        let db =
            DbConnection::open_file(&first.restored_database_path).map_err(|e| e.to_string())?;
        let expected = read_state(&db)?;
        assert_eq!(
            (
                expected.0.len(),
                expected.1.len(),
                expected.2.len(),
                expected.3.len()
            ),
            (1, 1, 1, 2)
        );
        assert!(expected.0[0].is_sealed());
        assert_eq!(expected.1[0].status, "active");
        assert!(expected.2[0].verified_at.is_none());
        assert_eq!(expected.2[0].status, "pending");
        db.close().map_err(|e| e.to_string())?;
        let backup = create_backup(&BackupCreateOptions {
            workspace_path: fixture.options.side_path.clone(),
            database_path: Some(PathBuf::from(&first.restored_database_path)),
            output_dir: None,
            label: None,
            redaction_level: level,
            include_derived: false,
            include_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.message())?;
        let second = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: fixture.options.side_path,
            backup_path: PathBuf::from(backup.backup_path),
            side_path: fixture.root.path().join("second"),
            restore_graph_cache: false,
            dry_run: false,
        })
        .map_err(|e| e.message())?;
        let db =
            DbConnection::open_file(&second.restored_database_path).map_err(|e| e.to_string())?;
        assert_eq!(
            read_state(&db)?,
            expected,
            "restoring cannot release, reveal, verify or reattribute history"
        );
        db.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn provenance_memory_id(number: u128) -> String {
    MemoryId::from_uuid(Uuid::from_u128(number)).to_string()
}

fn read_provenance(db: &DbConnection) -> Result<Vec<BackupMemoryProvenance>, String> {
    Ok(db
        .list_memories(&workspace_id(), None, true)
        .map_err(|error| error.to_string())?
        .iter()
        .map(BackupMemoryProvenance::from)
        .collect())
}

fn provenance_fixture(
    level: RedactionLevel,
    alter_source: impl FnOnce(&DbConnection) -> TestResult,
) -> Result<Fixture, String> {
    let (root, workspace, database) =
        crate::core::backup::tests::fixture().map_err(|error| error.message())?;
    let db = DbConnection::open_file(&database).map_err(|error| error.to_string())?;
    let timestamp = "2026-09-01T00:00:00+00:00";
    let content = if level == RedactionLevel::None {
        "Recovery provenance evidence"
    } else {
        "Recovery evidence api_key=PRIVATE_PROVENANCE_BODY"
    };
    db.execute_raw(&format!(
        "UPDATE memories SET content = '{content}', created_at = '{timestamp}', updated_at = '{timestamp}'"
    ))
    .map_err(|error| error.to_string())?;
    for number in 3..=9 {
        let id = provenance_memory_id(number);
        let created = if number == 8 {
            "2026-08-01T00:00:00+00:00"
        } else {
            timestamp
        };
        let logical_id = if number == 9 {
            provenance_memory_id(8)
        } else {
            id.clone()
        };
        db.insert_memory_with_timestamps(
            &id,
            &CreateMemoryInput {
                workspace_id: workspace_id(),
                level: "semantic".to_owned(),
                kind: "fact".to_owned(),
                content: format!("Recovery provenance observation {number}"),
                workflow_id: None,
                confidence: 0.8,
                utility: 0.6,
                importance: 0.7,
                provenance_uri: Some("ee-test://backup".to_owned()),
                trust_class: "agent_validated".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
            created,
            created,
            &logical_id,
        )
        .map_err(|error| error.to_string())?;
    }
    db.restore_imported_memory_supersession(&provenance_memory_id(8), timestamp)
        .map_err(|error| error.to_string())?;
    db.execute_raw(&format!(
        "UPDATE memories SET tombstoned_at = '2026-09-02T00:00:00+00:00' WHERE id = '{}'",
        provenance_memory_id(7)
    ))
    .map_err(|error| error.to_string())?;
    for (number, status) in [
        (2, "verified"),
        (3, "mismatch"),
        (4, "missing"),
        (5, "skipped"),
        (6, "unverified"),
        (7, "mismatch"),
        (8, "missing"),
        (9, "verified"),
    ] {
        let id = provenance_memory_id(number);
        let memory = db
            .get_memory(&id)
            .map_err(|error| error.to_string())?
            .ok_or("source memory missing")?;
        let hash = match status {
            "missing" => "NULL".to_owned(),
            "mismatch" => format!("'{}'", hash_bytes(b"different source commitment")),
            _ => format!("'{}'", compute_memory_provenance_chain_hash(&memory)),
        };
        let (checked_at, note) = if (6..=8).contains(&number) {
            ("NULL", "NULL")
        } else {
            (
                "'2026-09-03T00:00:00+00:00'",
                "'api_key=PRIVATE_PROVENANCE_NOTE'",
            )
        };
        // Seed independently of the recovery writer: a broken setter must not
        // manufacture matching source and destination fixtures.
        db.execute_raw(&format!(
            "UPDATE memories SET provenance_chain_hash = {hash}, provenance_verification_status = '{status}', provenance_verified_at = {checked_at}, provenance_verification_note = {note} WHERE id = '{id}'"
        ))
        .map_err(|error| error.to_string())?;
    }
    alter_source(&db)?;
    assert_eq!(read_provenance(&db)?.len(), 8);
    db.close().map_err(|error| error.to_string())?;
    let backup = create_backup(&BackupCreateOptions {
        workspace_path: workspace.clone(),
        database_path: Some(database.clone()),
        output_dir: None,
        label: None,
        redaction_level: level,
        include_derived: false,
        include_graph_cache: false,
        dry_run: false,
    })
    .map_err(|error| error.message())?;
    assert_eq!(backup.memory_count, 8);
    assert_eq!(
        backup
            .derived
            .iter()
            .filter(|asset| asset.kind == "trust_history")
            .count(),
        1,
        "memory-only stores require authenticated provenance history too"
    );
    let side_path = root.path().join("provenance-restored");
    Ok(Fixture {
        root,
        database,
        options: BackupRestoreOptions {
            workspace_path: workspace,
            backup_path: PathBuf::from(backup.backup_path),
            side_path,
            restore_graph_cache: false,
            dry_run: false,
        },
    })
}

#[test]
fn memory_provenance_survives_restore_rebackup_and_real_reverification() -> TestResult {
    let fixture = provenance_fixture(RedactionLevel::None, |_| Ok(()))?;
    let source = DbConnection::open_file(&fixture.database).map_err(|error| error.to_string())?;
    let expected = read_provenance(&source)?;
    source.close().map_err(|error| error.to_string())?;
    let first = restore_backup_to_side_path(&fixture.options).map_err(|error| error.message())?;
    assert!(!first.degraded.iter().any(|item| {
        item.code == "backup_memory_provenance_unavailable"
    }));
    let db = DbConnection::open_file(&first.restored_database_path)
        .map_err(|error| error.to_string())?;
    assert_eq!(read_provenance(&db)?, expected);
    assert!(
        db.get_memory(&provenance_memory_id(7))
            .map_err(|error| error.to_string())?
            .ok_or("restored tombstone missing")?
            .tombstoned_at
            .is_some()
    );
    assert!(
        db.get_memory_superseded_at(&provenance_memory_id(8))
            .map_err(|error| error.to_string())?
            .is_some()
    );
    db.close().map_err(|error| error.to_string())?;
    let backup = create_backup(&BackupCreateOptions {
        workspace_path: fixture.options.side_path.clone(),
        database_path: Some(PathBuf::from(&first.restored_database_path)),
        output_dir: None,
        label: None,
        redaction_level: RedactionLevel::None,
        include_derived: false,
        include_graph_cache: false,
        dry_run: false,
    })
    .map_err(|error| error.message())?;
    let second = restore_backup_to_side_path(&BackupRestoreOptions {
        workspace_path: fixture.options.side_path,
        backup_path: PathBuf::from(backup.backup_path),
        side_path: fixture.root.path().join("provenance-second"),
        restore_graph_cache: false,
        dry_run: false,
    })
    .map_err(|error| error.message())?;
    let db = DbConnection::open_file(&second.restored_database_path)
        .map_err(|error| error.to_string())?;
    assert_eq!(read_provenance(&db)?, expected);
    let checked = db
        .verify_sampled_memory_provenance(&workspace_id(), 100)
        .map_err(|error| error.to_string())?;
    assert_eq!(checked.checked_count, 8);
    assert_eq!(checked.mismatch_count, 2);
    assert_eq!(checked.missing_count, 2);
    assert_eq!(checked.verified_count, 4);
    for (number, status) in [(3, "mismatch"), (4, "missing"), (7, "mismatch"), (8, "missing")] {
        let row = checked
            .records
            .iter()
            .find(|row| row.memory_id == provenance_memory_id(number))
            .ok_or("integrity sample omitted historical provenance")?;
        assert_eq!(row.status, status, "recovery must not heal source corruption");
    }
    db.close().map_err(|error| error.to_string())?;
    Ok(())
}

#[test]
fn redacted_memory_cannot_rebind_a_previously_verified_commitment() -> TestResult {
    // Strict retains identity, so clearing the commitment must be caused by
    // withheld content rather than a separate identifier transformation.
    let fixture = provenance_fixture(RedactionLevel::Strict, |db| {
        db.execute_raw(&format!(
            "UPDATE memories SET provenance_chain_hash = 'blake3:api_key=PRIVATE_PROVENANCE_HASH', provenance_verified_at = 'api_key=PRIVATE_PROVENANCE_TIME' WHERE id = '{}'",
            provenance_memory_id(3)
        ))
        .map_err(|error| error.to_string())?;
        Ok(())
    })?;
    let source = DbConnection::open_file(&fixture.database).map_err(|error| error.to_string())?;
    let original = source
        .get_memory(&provenance_memory_id(2))
        .map_err(|error| error.to_string())?
        .ok_or("source verified memory missing")?;
    let restored_ids = crate::core::backup::backup_memory_id_mapping(
        &source
            .list_memories(&workspace_id(), None, true)
            .map_err(|error| error.to_string())?,
        RedactionLevel::Strict,
    )
    .map_err(|error| error.message())?;
    let verified_id = restored_ids
        .get(&original.id)
        .ok_or("mapped verified memory missing")?;
    let negative_id = restored_ids
        .get(&provenance_memory_id(3))
        .ok_or("mapped negative memory missing")?;
    source.close().map_err(|error| error.to_string())?;
    let asset = fixture
        .options
        .backup_path
        .join("derived/trust-history/00000000.json");
    let bytes = std::fs::read_to_string(asset).map_err(|error| error.to_string())?;
    assert!(!bytes.contains("PRIVATE_PROVENANCE_NOTE"));
    assert!(!bytes.contains("PRIVATE_PROVENANCE_BODY"));
    assert!(!bytes.contains("PRIVATE_PROVENANCE_HASH"));
    assert!(!bytes.contains("PRIVATE_PROVENANCE_TIME"));
    assert!(!bytes.contains(
        original
            .provenance_chain_hash
            .as_deref()
            .ok_or("source commitment missing")?
    ));
    let restored =
        restore_backup_to_side_path(&fixture.options).map_err(|error| error.message())?;
    let db = DbConnection::open_file(&restored.restored_database_path)
        .map_err(|error| error.to_string())?;
    let memory = db
        .get_memory(verified_id)
        .map_err(|error| error.to_string())?
        .ok_or("restored redacted memory missing")?;
    assert_ne!(memory.content, original.content);
    assert!(!memory.content.contains("PRIVATE_PROVENANCE_BODY"));
    assert!(memory.provenance_chain_hash.is_none());
    assert_eq!(memory.provenance_verification_status, "missing");
    assert!(memory.provenance_verified_at.is_none());
    assert!(
        memory
            .provenance_verification_note
            .as_deref()
            .is_some_and(|note| !note.contains("PRIVATE_PROVENANCE_NOTE"))
    );
    let negative = db
        .get_memory(negative_id)
        .map_err(|error| error.to_string())?
        .ok_or("restored negative provenance missing")?;
    assert_eq!(negative.provenance_verification_status, "mismatch");
    assert!(negative.provenance_chain_hash.is_none());
    assert!(
        negative
            .provenance_verified_at
            .as_deref()
            .is_some_and(|time| !time.contains("PRIVATE_PROVENANCE_TIME"))
    );
    let checked = db
        .verify_sampled_memory_provenance(&workspace_id(), 100)
        .map_err(|error| error.to_string())?;
    assert_eq!(
        checked
            .records
            .iter()
            .find(|row| row.memory_id == *verified_id)
            .ok_or("redacted memory omitted from integrity sample")?
            .status,
        "missing"
    );
    db.close().map_err(|error| error.to_string())?;
    Ok(())
}

#[test]
fn claimed_verification_without_a_source_commitment_restores_as_missing() -> TestResult {
    let fixture = provenance_fixture(RedactionLevel::None, |db| {
        db.execute_raw(&format!(
            "UPDATE memories SET provenance_chain_hash = NULL WHERE id = '{}'",
            provenance_memory_id(2)
        ))
        .map_err(|error| error.to_string())?;
        Ok(())
    })?;
    let restored =
        restore_backup_to_side_path(&fixture.options).map_err(|error| error.message())?;
    let db = DbConnection::open_file(&restored.restored_database_path)
        .map_err(|error| error.to_string())?;
    let memory = db
        .get_memory(&provenance_memory_id(2))
        .map_err(|error| error.to_string())?
        .ok_or("restored memory missing")?;
    assert!(memory.provenance_chain_hash.is_none());
    assert_eq!(memory.provenance_verification_status, "missing");
    assert!(memory.provenance_verified_at.is_none());
    let checked = db
        .verify_sampled_memory_provenance(&workspace_id(), 100)
        .map_err(|error| error.to_string())?;
    assert_eq!(checked.missing_count, 3);
    db.close().map_err(|error| error.to_string())?;
    Ok(())
}

#[test]
fn both_recovery_fences_reject_changed_memory_provenance_without_count_changes() -> TestResult {
    let fixture = provenance_fixture(RedactionLevel::None, |_| Ok(()))?;
    for late in [false, true] {
        for (column, assignment) in [
            ("hash", "provenance_chain_hash = NULL"),
            ("status", "provenance_verification_status = 'verified'"),
            (
                "timestamp",
                "provenance_verified_at = '2026-10-01T00:00:00+00:00'",
            ),
            ("note", "provenance_verification_note = 'PRIVATE_MUTATION_SENTINEL'"),
        ] {
            let mut options = fixture.options.clone();
            options.side_path = fixture.root.path().join(format!("changed-{late}-{column}"));
            let mut reached_fence = false;
            let mutate = |path: &std::path::Path| {
                reached_fence = true;
                if late {
                    assert!(
                        path.parent()
                            .ok_or_else(|| work_history_error("staging parent missing"))?
                            .join("index/meta.json")
                            .is_file()
                    );
                }
                let db = DbConnection::open_file(path).map_err(work_history_error)?;
                let before = read_provenance(&db).map_err(work_history_error)?;
                db.execute_raw(&format!(
                    "UPDATE memories SET {assignment} WHERE id = '{}'",
                    provenance_memory_id(3)
                ))
                .map_err(work_history_error)?;
                let after = read_provenance(&db).map_err(work_history_error)?;
                assert_eq!(before.len(), after.len());
                assert_ne!(before, after);
                db.close().map_err(work_history_error)
            };
            let result = if late {
                restore_backup_to_side_path_with_recovery_hooks(&options, |_| Ok(()), mutate)
            } else {
                restore_backup_to_side_path_with_recovery_hooks(&options, mutate, |_| Ok(()))
            };
            let error = result.err().ok_or("published altered memory provenance")?;
            assert!(reached_fence, "restore must reach the intended mutation fence");
            assert!(
                error.message().contains("Restored durable content differs for memories"),
                "{}",
                error.message()
            );
            assert!(!error.message().contains("PRIVATE_MUTATION_SENTINEL"));
            assert!(!options.side_path.join(WORKSPACE_MARKER).exists());
        }
    }
    Ok(())
}

fn write_trust_expectation_asset(
    directory: &std::path::Path,
    name: &str,
    chunk: &BackupTrustHistory,
) -> Result<BackupRestoredDerivedAssetReport, String> {
    let path = directory.join(name);
    std::fs::write(
        &path,
        serde_json::to_vec(chunk).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok(BackupRestoredDerivedAssetReport {
        path: name.to_owned(),
        kind: "trust_history".to_owned(),
        restore_path: path.to_string_lossy().into_owned(),
        lab_episode_path: None,
    })
}

#[test]
fn provenance_expectation_rejects_incomplete_duplicate_or_substituted_carriers() -> TestResult {
    let fixture = provenance_fixture(RedactionLevel::None, |_| Ok(()))?;
    let original: BackupTrustHistory = serde_json::from_slice(
        &std::fs::read(
            fixture
                .options
                .backup_path
                .join("derived/trust-history/00000000.json"),
        )
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    assert_eq!(original.schema, crate::core::backup::TRUST_HISTORY_SCHEMA);
    assert_eq!(original.memory_provenance.as_ref().map(Vec::len), Some(8));
    let source = DbConnection::open_file(&fixture.database).map_err(|error| error.to_string())?;
    let source_before = read_provenance(&source)?;
    let asset = write_trust_expectation_asset(fixture.root.path(), "positive.json", &original)?;
    let expected = TrustExpectation::from_assets(&[asset], &original.backup_id, &workspace_id())
        .map_err(|error| error.message())?;
    expected
        .verify_connection(&source)
        .map_err(|error| error.message())?;
    // This exercises the independent content fence after the caller's manifest
    // admission, not the separate family-MAC validator. It must reject malformed
    // carrier populations even when a writer accepts or silently omits a row.
    for case in 0..8 {
        let mut chunk = original.clone();
        let mut additional = None;
        match case {
            0 => {
                let rows = chunk.memory_provenance.as_mut().ok_or("missing carrier")?;
                let duplicate = rows.first().cloned().ok_or("empty source carrier")?;
                rows.push(duplicate);
            }
            1 => {
                chunk
                    .memory_provenance
                    .as_mut()
                    .ok_or("missing carrier")?
                    .pop()
                    .ok_or("empty source carrier")?;
            }
            2 => {
                chunk
                    .memory_provenance
                    .as_mut()
                    .and_then(|rows| rows.first_mut())
                    .ok_or("missing first provenance row")?
                    .memory_id = provenance_memory_id(999);
            }
            3 => chunk.memory_provenance = None,
            4 => {
                chunk
                    .memory_provenance
                    .as_mut()
                    .and_then(|rows| rows.first_mut())
                    .ok_or("missing first provenance row")?
                    .provenance_chain_hash_version = "ee.memory.provenance_chain.v999".to_owned();
            }
            5 => chunk.schema = crate::core::backup::TRUST_HISTORY_SCHEMA_V1.to_owned(),
            6 => {
                chunk.chunk_count = 2;
                let mut legacy = original.clone();
                legacy.chunk_count = 2;
                legacy.chunk_index = 1;
                legacy.schema = crate::core::backup::TRUST_HISTORY_SCHEMA_V1.to_owned();
                legacy.memory_provenance = None;
                additional = Some(legacy);
            }
            7 => chunk.memory_provenance = Some(Vec::new()),
            _ => return Err("unhandled provenance carrier control".to_owned()),
        }
        let mut assets = vec![write_trust_expectation_asset(
            fixture.root.path(),
            &format!("invalid-{case}-first.json"),
            &chunk,
        )?];
        if let Some(chunk) = additional {
            assets.push(write_trust_expectation_asset(
                fixture.root.path(),
                &format!("invalid-{case}-second.json"),
                &chunk,
            )?);
        }
        let result = TrustExpectation::from_assets(&assets, &original.backup_id, &workspace_id())
            .and_then(|expectation| expectation.verify_connection(&source));
        let error = result.err().ok_or("admitted invalid memory provenance carrier")?;
        assert!(!error.message().contains("PRIVATE_PROVENANCE"));
        assert_eq!(read_provenance(&source)?, source_before);
    }
    source.close().map_err(|error| error.to_string())?;
    Ok(())
}

#[test]
fn legacy_provenance_fence_requires_missing_commitments_including_historical_rows() -> TestResult {
    let fixture = provenance_fixture(RedactionLevel::None, |_| Ok(()))?;
    let db = DbConnection::open_file(&fixture.database).map_err(|error| error.to_string())?;
    let expected = TrustExpectation::from_assets(&[], "legacy-backup", &workspace_id())
        .map_err(|error| error.message())?;
    assert!(
        expected.verify_connection(&db).is_err(),
        "ordinary insertion's new matching hashes cannot stand in for unavailable history"
    );
    db.execute_raw(
        "UPDATE memories SET provenance_chain_hash = NULL, provenance_verification_status = 'missing', provenance_verified_at = NULL, provenance_verification_note = 'Backup has no authenticated memory provenance history; recovery did not generate a replacement commitment.'",
    )
    .map_err(|error| error.to_string())?;
    expected
        .verify_connection(&db)
        .map_err(|error| error.message())?;
    let checked = db
        .inspect_sampled_memory_provenance(&workspace_id(), 100)
        .map_err(|error| error.to_string())?;
    assert_eq!(checked.missing_count, 8);
    assert_eq!(checked.verified_count, 0);
    for number in [7, 8] {
        db.execute_raw(&format!(
            "UPDATE memories SET provenance_chain_hash = '{}' WHERE id = '{}'",
            hash_bytes(b"invented recovery commitment"),
            provenance_memory_id(number)
        ))
        .map_err(|error| error.to_string())?;
        assert!(expected.verify_connection(&db).is_err());
        db.execute_raw(&format!(
            "UPDATE memories SET provenance_chain_hash = NULL WHERE id = '{}'",
            provenance_memory_id(number)
        ))
        .map_err(|error| error.to_string())?;
    }
    expected
        .verify_connection(&db)
        .map_err(|error| error.message())?;
    db.close().map_err(|error| error.to_string())?;
    Ok(())
}

fn sign_admission_trust_chunk(
    chunk: &mut BackupTrustHistory,
    root: &StoreAuthRoot,
) -> TestResult {
    // Sign the encoded shape directly: malformed-shape controls must reach
    // semantic admission with valid authentication, not fail in a test signer.
    chunk.authentication = None;
    let hash = canonical_record_hash(
        &serde_json::to_vec(&chunk).map_err(|error| error.to_string())?,
    );
    let context = crate::core::backup::trust_history_auth_context(
        &chunk.schema,
        &chunk.workspace_id,
    );
    let header = authenticate_artifact(
        root,
        MacDomain::NativeImportRecordsRoot,
        &context,
        &hash,
        1,
    )
    .map_err(|error| error.to_string())?;
    assert!(
        verify_artifact(
            root,
            MacDomain::NativeImportRecordsRoot,
            &context,
            &header,
            &hash,
            1,
        )
        .map_err(|error| error.to_string())?
        .is_authenticated(),
        "semantic controls require a valid family MAC"
    );
    chunk.authentication = Some(header);
    Ok(())
}

fn write_admission_archive(
    fixture: &Fixture,
    original_manifest: &serde_json::Value,
    chunks: &[BackupTrustHistory],
    root: &StoreAuthRoot,
) -> TestResult {
    let mut manifest = original_manifest.clone();
    let derived = manifest["derived"]
        .as_array_mut()
        .ok_or("source derived inventory missing")?;
    let template = derived
        .iter()
        .find(|asset| asset["kind"] == "trust_history")
        .cloned()
        .ok_or("source trust-history inventory missing")?;
    derived.retain(|asset| asset["kind"] != "trust_history");
    for (index, chunk) in chunks.iter().enumerate() {
        let relative = format!("derived/trust-history/{index:08}.json");
        let bytes = serde_json::to_vec(chunk).map_err(|error| error.to_string())?;
        std::fs::write(fixture.options.backup_path.join(&relative), &bytes)
            .map_err(|error| error.to_string())?;
        let mut asset = template.clone();
        asset["path"] = serde_json::json!(relative);
        asset["hash"] = serde_json::json!(hash_bytes(&bytes));
        asset["byte_size"] = serde_json::json!(bytes.len());
        derived.push(asset);
    }
    authenticate_backup_manifest(&mut manifest, root).map_err(|error| error.message())?;
    std::fs::write(
        fixture.options.backup_path.join("manifest.json"),
        serde_json::to_vec(&manifest).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn key_file_fingerprints(
    directory: &std::path::Path,
) -> Result<BTreeMap<PathBuf, String>, String> {
    let mut fingerprints = BTreeMap::new();
    for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if entry.file_type().map_err(|error| error.to_string())?.is_dir() {
            fingerprints.extend(key_file_fingerprints(&path)?);
        } else {
            fingerprints.insert(
                path.clone(),
                hash_bytes(&std::fs::read(path).map_err(|error| error.to_string())?),
            );
        }
    }
    Ok(fingerprints)
}

#[test]
fn public_verification_rejects_invalid_provenance_before_any_restore_writes() -> TestResult {
    let fixture = provenance_fixture(RedactionLevel::None, |_| Ok(()))?;
    let verify_options = BackupVerifyOptions {
        workspace_path: fixture.options.workspace_path.clone(),
        backup_path: fixture.options.backup_path.clone(),
    };
    let positive = verify_backup(&verify_options).map_err(|error| error.message())?;
    assert_eq!(positive.status, "verified", "{:?}", positive.issues);
    let (_, original_manifest) =
        read_backup_manifest(&fixture.options.backup_path).map_err(|error| error.message())?;
    let original: BackupTrustHistory = serde_json::from_slice(
        &std::fs::read(
            fixture.options.backup_path.join("derived/trust-history/00000000.json"),
        )
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let key_directory = workspace_keys_dir(&fixture.options.workspace_path);
    let root = StoreAuthRoot::open(&key_directory).map_err(|error| error.to_string())?;
    let keys_before = key_file_fingerprints(&key_directory)?;
    let database_before = hash_bytes(
        &std::fs::read(&fixture.database).map_err(|error| error.to_string())?,
    );
    for defect in [
        "empty",
        "missing",
        "duplicate",
        "orphan",
        "missing_carrier",
        "mixed_schema",
        "wrong_version",
        "wrong_status",
        "foreign_workspace",
        "missing_chunk",
        "tampered_mac",
        "unsigned",
    ] {
        let mut chunk = original.clone();
        let mut additional = None;
        match defect {
            "empty" => chunk.memory_provenance = Some(Vec::new()),
            "missing" => {
                chunk
                    .memory_provenance
                    .as_mut()
                    .ok_or("missing carrier")?
                    .pop()
                    .ok_or("empty carrier")?;
            }
            "duplicate" => {
                let rows = chunk.memory_provenance.as_mut().ok_or("missing carrier")?;
                let first = rows.first().cloned().ok_or("empty carrier")?;
                *rows.last_mut().ok_or("empty carrier")? = first;
            }
            "orphan" => {
                chunk
                    .memory_provenance
                    .as_mut()
                    .and_then(|rows| rows.first_mut())
                    .ok_or("empty carrier")?
                    .memory_id = provenance_memory_id(999);
            }
            "missing_carrier" => chunk.memory_provenance = None,
            "mixed_schema" => {
                chunk.chunk_count = 2;
                let mut legacy = original.clone();
                legacy.chunk_count = 2;
                legacy.chunk_index = 1;
                legacy.schema = crate::core::backup::TRUST_HISTORY_SCHEMA_V1.to_owned();
                legacy.memory_provenance = None;
                sign_admission_trust_chunk(&mut legacy, &root)?;
                additional = Some(legacy);
            }
            "wrong_version" | "wrong_status" | "tampered_mac" => {
                let row = chunk
                    .memory_provenance
                    .as_mut()
                    .and_then(|rows| rows.first_mut())
                    .ok_or("empty carrier")?;
                match defect {
                    "wrong_version" => {
                        row.provenance_chain_hash_version = "unsupported-version".to_owned();
                    }
                    "wrong_status" => {
                        row.provenance_verification_status =
                            "PRIVATE_ADMISSION_SENTINEL".to_owned();
                    }
                    _ => {
                        row.provenance_verification_note =
                            Some("PRIVATE_ADMISSION_SENTINEL".to_owned());
                    }
                }
            }
            "foreign_workspace" => {
                chunk.workspace_id = WorkspaceId::from_uuid(Uuid::from_u128(999)).to_string();
            }
            "missing_chunk" => chunk.chunk_count = 2,
            "unsigned" => chunk.authentication = None,
            _ => return Err("unhandled public provenance admission control".to_owned()),
        }
        if !matches!(defect, "tampered_mac" | "unsigned") {
            sign_admission_trust_chunk(&mut chunk, &root)?;
        }
        let mut chunks = vec![chunk];
        if let Some(additional) = additional {
            chunks.push(additional);
        }
        write_admission_archive(&fixture, &original_manifest, &chunks, &root)?;
        let report = verify_backup(&verify_options).map_err(|error| error.message())?;
        assert_eq!(report.status, "failed", "accepted {defect}");
        let blocking = report
            .issues
            .iter()
            .filter(|issue| backup_verification_issue_is_blocking(issue))
            .map(|issue| issue.code.as_str())
            .collect::<Vec<_>>();
        assert_eq!(blocking, vec!["trust_history_invalid"], "wrong boundary for {defect}");
        assert!(report.checked_derived.iter().any(|asset| asset.kind == "trust_history"));
        assert!(!report.data_json().to_string().contains("PRIVATE_ADMISSION_SENTINEL"));
        for dry_run in [true, false] {
            let mut options = fixture.options.clone();
            options.side_path = fixture.root.path().join(format!("refused-{defect}-{dry_run}"));
            options.dry_run = dry_run;
            let error = restore_backup_to_side_path(&options)
                .err()
                .ok_or_else(|| format!("accepted {defect} with dry_run={dry_run}"))?;
            assert!(
                error.message().contains("failed integrity verification"),
                "{defect}: {}",
                error.message()
            );
            assert!(!options.side_path.exists(), "created a destination for {defect}");
            assert!(!workspace_keys_dir(&options.side_path).exists());
        }
        assert_eq!(key_file_fingerprints(&key_directory)?, keys_before);
        assert_eq!(
            hash_bytes(&std::fs::read(&fixture.database).map_err(|error| error.to_string())?),
            database_before
        );
    }
    Ok(())
}

#[test]
fn public_provenance_admission_accepts_redacted_ids_and_explicit_legacy_history() -> TestResult {
    for redaction in [RedactionLevel::None, RedactionLevel::Standard] {
        let fixture = provenance_fixture(redaction, |_| Ok(()))?;
        let verify_options = BackupVerifyOptions {
            workspace_path: fixture.options.workspace_path.clone(),
            backup_path: fixture.options.backup_path.clone(),
        };
        let report = verify_backup(&verify_options).map_err(|error| error.message())?;
        assert_eq!(report.status, "verified", "{:?}", report.issues);
        let mut options = fixture.options.clone();
        options.dry_run = true;
        let preview = restore_backup_to_side_path(&options).map_err(|error| error.message())?;
        assert_eq!(preview.status, "dry_run");
        assert!(!preview.degraded.iter().any(|entry| {
            entry.code == "backup_memory_provenance_unavailable"
        }));
        assert!(!options.side_path.exists());
        let (_, manifest) =
            read_backup_manifest(&fixture.options.backup_path).map_err(|error| error.message())?;
        let mut legacy: BackupTrustHistory = serde_json::from_slice(
            &std::fs::read(
                fixture.options.backup_path.join("derived/trust-history/00000000.json"),
            )
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        legacy.schema = crate::core::backup::TRUST_HISTORY_SCHEMA_V1.to_owned();
        legacy.memory_provenance = None;
        let root = StoreAuthRoot::open(workspace_keys_dir(&fixture.options.workspace_path))
            .map_err(|error| error.to_string())?;
        sign_admission_trust_chunk(&mut legacy, &root)?;
        write_admission_archive(&fixture, &manifest, &[legacy], &root)?;
        let report = verify_backup(&verify_options).map_err(|error| error.message())?;
        assert_eq!(report.status, "verified", "{:?}", report.issues);
        let preview = restore_backup_to_side_path(&options).map_err(|error| error.message())?;
        assert_eq!(preview.status, "dry_run");
        assert!(preview.degraded.iter().any(|entry| {
            entry.code == "backup_memory_provenance_unavailable"
        }));
        assert!(!options.side_path.exists());
    }
    Ok(())
}

#[path = "backup_reference_recovery_tests.rs"]
mod references;

#[path = "backup_publication_recovery_tests.rs"]
mod publication;
