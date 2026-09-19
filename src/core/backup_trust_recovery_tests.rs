use std::path::PathBuf;

use super::*;
use crate::config::WORKSPACE_MARKER;
use crate::core::backup::{
    BackupCreateOptions, BackupRestoreOptions, BackupVerifyOptions, create_backup, hash_bytes,
    restore_backup_to_side_path, restore_backup_to_side_path_with_verification_hook, verify_backup,
    work_history_error,
};
use crate::db::{StoredAgent, StoredCertificateRecord, StoredTrustQuarantine};
use crate::models::{MemoryId, MemorySeal, RedactionLevel, WorkspaceId, MEMORY_SEAL_PLACEHOLDER_CONTENT, memory_seal_commitment};
use uuid::Uuid;

type TestResult = Result<(), String>;
type TrustState = (Vec<MemorySeal>, Vec<StoredTrustQuarantine>, Vec<StoredCertificateRecord>, Vec<StoredAgent>);

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
        db.list_memory_seals_for_recovery(&ws).map_err(|e| e.to_string())?,
        db.list_trust_quarantine(&ws, false).map_err(|e| e.to_string())?,
        db.list_certificates_for_recovery(&ws).map_err(|e| e.to_string())?,
        db.list_agents_for_recovery(&ws).map_err(|e| e.to_string())?,
    ))
}

fn fixture(level: RedactionLevel) -> Result<Fixture, String> {
    let (root, workspace, database) = crate::core::backup::tests::fixture().map_err(|e| e.message())?;
    let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    let ws = workspace_id();
    let memory_id = MemoryId::from_uuid(Uuid::from_u128(2)).to_string();
    let time = "2026-09-01T00:00:00Z";
    db.execute_raw(&format!("UPDATE memories SET content = '{MEMORY_SEAL_PLACEHOLDER_CONTENT}' WHERE id = '{memory_id}'"))
        .map_err(|e| e.to_string())?;
    db.insert_memory_seal_for_recovery(&MemorySeal {
        memory_id,
        content_commitment: memory_seal_commitment(b"private unrevealed result"),
        sealed_at: time.to_owned(),
        revealed_at: None,
        reveal_verified: None,
    }).map_err(|e| e.to_string())?;
    db.insert_trust_quarantine_for_recovery(&StoredTrustQuarantine {
        workspace_id: ws.clone(), source_uri: "ee-test://backup".to_owned(),
        first_event_at: time.to_owned(), last_event_at: time.to_owned(),
        harmful_event_count: 7, quarantined_until: Some("2099-01-01T00:00:00Z".to_owned()),
        reason: "Repeated harmful observations".to_owned(), status: "active".to_owned(),
        created_at: time.to_owned(), updated_at: time.to_owned(),
    }).map_err(|e| e.to_string())?;
    db.insert_certificate_for_recovery(&StoredCertificateRecord {
        id: "cert_recovery_guard".to_owned(), workspace_id: ws.clone(),
        target_kind: "pack".to_owned(), target_id: "pack_historical".to_owned(),
        hash_algo: "blake3".to_owned(), content_hash: hash_bytes(b"historical certificate payload"),
        signature: None, signature_algorithm: None, signer: None, signed_at: None,
        verified_at: None, status: "pending".to_owned(), manifest_path: None,
        payload_path: None, metadata_json: "{}".to_owned(), created_at: time.to_owned(), updated_at: time.to_owned(),
    }).map_err(|e| e.to_string())?;
    for n in 0..2 {
        db.insert_agent_for_recovery(&StoredAgent {
            id: format!("agt_{n:026}"), workspace_id: ws.clone(), name: format!("HistoricalAgent{n}"),
            model: Some("historical-model".to_owned()), created_at: time.to_owned(), last_seen_at: time.to_owned(),
        }).map_err(|e| e.to_string())?;
    }
    let seeded = read_state(&db)?;
    assert_eq!((seeded.0.len(), seeded.1.len(), seeded.2.len(), seeded.3.len()), (1, 1, 1, 2));
    db.close().map_err(|e| e.to_string())?;
    let backup = create_backup(&BackupCreateOptions {
        workspace_path: workspace.clone(), database_path: Some(database.clone()),
        output_dir: None, label: None, redaction_level: level, include_derived: false,
        include_graph_cache: false, dry_run: false,
    }).map_err(|e| e.message())?;
    let options = BackupRestoreOptions {
        workspace_path: workspace, backup_path: PathBuf::from(backup.backup_path),
        side_path: root.path().join("restored"), restore_graph_cache: false, dry_run: false,
    };
    Ok(Fixture { root, database, options })
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
        let counts = tables.iter().map(|t| db.count_table_rows(t)).collect::<Result<Vec<_>, _>>().map_err(work_history_error)?;
        db.execute_raw(sql).map_err(work_history_error)?;
        let after = read_state(&db).map_err(work_history_error)?;
        assert_ne!(before, after, "mutation must really change well-formed trust rows");
        let after_counts = tables.iter().map(|t| db.count_table_rows(t)).collect::<Result<Vec<_>, _>>().map_err(work_history_error)?;
        assert_eq!(counts, after_counts, "every table's count must stay identical");
        db.close().map_err(work_history_error)
    }).err().ok_or("published altered trust state")?;
    assert!(error.message().contains(&format!("Restored durable content differs for {table}")), "{}", error.message());
    assert!(!error.message().contains("PRIVATE_SENTINEL"));
    assert!(!fixture.options.side_path.join(WORKSPACE_MARKER).exists());
    let source = DbConnection::open_file(&fixture.database).map_err(|e| e.to_string())?;
    assert_eq!(read_state(&source)?, original);
    source.close().map_err(|e| e.to_string())?;
    let verified = verify_backup(&BackupVerifyOptions {
        workspace_path: fixture.options.workspace_path, backup_path: fixture.options.backup_path,
    }).map_err(|e| e.message())?;
    assert_eq!(verified.status, "verified");
    Ok(())
}

#[test]
fn trust_fence_rejects_quarantine_release() -> TestResult {
    assert_refused("trust_quarantine", "UPDATE trust_quarantine SET status = 'released'")
}

#[test]
fn trust_fence_rejects_quarantine_expiry_rewrite() -> TestResult {
    assert_refused("trust_quarantine", "UPDATE trust_quarantine SET quarantined_until = '2020-01-01T00:00:00Z'")
}

#[test]
fn trust_fence_rejects_fabricated_reveal() -> TestResult {
    assert_refused("memory_seals", "UPDATE memory_seals SET revealed_at = '2026-09-02T00:00:00Z', reveal_verified = 1")
}

#[test]
fn trust_fence_rejects_substituted_commitment() -> TestResult {
    assert_refused("memory_seals", &format!("UPDATE memory_seals SET content_commitment = '{}'", memory_seal_commitment(b"substituted result")))
}

#[test]
fn trust_fence_rejects_fabricated_certificate_verification() -> TestResult {
    assert_refused("certificates", "UPDATE certificates SET status = 'valid', verified_at = '2026-09-02T00:00:00Z'")
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
        let db = DbConnection::open_file(&first.restored_database_path).map_err(|e| e.to_string())?;
        let expected = read_state(&db)?;
        assert_eq!((expected.0.len(), expected.1.len(), expected.2.len(), expected.3.len()), (1, 1, 1, 2));
        assert!(expected.0[0].is_sealed());
        assert_eq!(expected.1[0].status, "active");
        assert!(expected.2[0].verified_at.is_none());
        assert_eq!(expected.2[0].status, "pending");
        db.close().map_err(|e| e.to_string())?;
        let backup = create_backup(&BackupCreateOptions {
            workspace_path: fixture.options.side_path.clone(), database_path: Some(PathBuf::from(&first.restored_database_path)),
            output_dir: None, label: None, redaction_level: level, include_derived: false,
            include_graph_cache: false, dry_run: false,
        }).map_err(|e| e.message())?;
        let second = restore_backup_to_side_path(&BackupRestoreOptions {
            workspace_path: fixture.options.side_path, backup_path: PathBuf::from(backup.backup_path),
            side_path: fixture.root.path().join("second"), restore_graph_cache: false, dry_run: false,
        }).map_err(|e| e.message())?;
        let db = DbConnection::open_file(&second.restored_database_path).map_err(|e| e.to_string())?;
        assert_eq!(read_state(&db)?, expected, "restoring cannot release, reveal, verify or reattribute history");
        db.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}
