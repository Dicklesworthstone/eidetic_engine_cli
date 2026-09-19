use std::path::PathBuf;

use super::*;
use crate::config::WORKSPACE_MARKER;
use crate::core::backup::{
    BackupCreateOptions, BackupRestoreOptions, BackupVerifyOptions, create_backup, hash_bytes,
    restore_backup_to_side_path, restore_backup_to_side_path_with_verification_hook, verify_backup,
    work_history_error,
};
use crate::db::{OutcomeEvidenceSource, StoredFeedbackQuarantine, StoredLearningObservation};
use crate::models::{MemoryId, RedactionLevel, WorkspaceId};
use uuid::Uuid;

type TestResult = Result<(), String>;
type SignalState = (Vec<StoredLearningObservation>, Vec<StoredFeedbackQuarantine>, Vec<StoredOutcomeEvidence>);

struct Fixture {
    root: tempfile::TempDir,
    database: PathBuf,
    options: BackupRestoreOptions,
}

fn workspace_id() -> String { WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string() }
fn quarantine_id(n: usize) -> String { format!("fq_{n:026}") }

fn read_state(db: &DbConnection) -> Result<SignalState, String> {
    let ws = workspace_id();
    Ok((
        db.list_learning_observations(&ws, None).map_err(|e| e.to_string())?,
        db.list_feedback_quarantine(&ws, None).map_err(|e| e.to_string())?,
        db.list_outcome_evidence_for_recovery(&ws).map_err(|e| e.to_string())?,
    ))
}

fn fixture(level: RedactionLevel) -> Result<Fixture, String> {
    let (root, workspace, database) = crate::core::backup::tests::fixture().map_err(|e| e.message())?;
    let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    let ws = workspace_id();
    let memory_id = MemoryId::from_uuid(Uuid::from_u128(2)).to_string();
    let time = "2026-09-01T00:00:00Z";
    db.insert_learning_observation_for_recovery(&StoredLearningObservation {
        id: "lobs_recovery_signal".to_owned(), workspace_id: ws.clone(),
        observation_kind: "experiment_observe".to_owned(), source_type: "experiment".to_owned(),
        source_id: Some("api_key=signal-source-private-canary".to_owned()),
        target_type: "memory".to_owned(), target_id: memory_id.clone(), topic: Some("release".to_owned()),
        signal: "helpful".to_owned(), evidence_json: Some("{\"note\":\"measured result\"}".to_owned()),
        observed_at: time.to_owned(), created_at: time.to_owned(),
    }).map_err(|e| e.to_string())?;
    for n in 0..2 {
        let mut row = StoredFeedbackQuarantine {
            id: quarantine_id(n), workspace_id: ws.clone(),
            source_id: "api_key=signal-quarantine-private-canary".to_owned(),
            target_type: "memory".to_owned(), target_id: memory_id.clone(), signal: "harmful".to_owned(),
            weight: 0.5, source_type: "outcome_observed".to_owned(),
            proposed_event_id: Some(format!("fb_{:026}", 100 + n)), recorded_at: time.to_owned(),
            reason: "Requires explicit review".to_owned(), event_reason: Some("Observed failure".to_owned()),
            evidence_json: None, session_id: None, raw_event_hash: String::new(), status: "pending".to_owned(),
            reviewed_at: None, reviewed_by: None, released_feedback_event_id: None,
        };
        row.raw_event_hash = if n == 0 {
            quarantine_payload_hash(&row).map_err(|e| e.message())?.ok_or("missing event id")?
        } else {
            hash_bytes(b"invalid original payload hash")
        };
        db.insert_feedback_quarantine_for_recovery(&row).map_err(|e| e.to_string())?;
    }
    for (n, source) in [OutcomeEvidenceSource::VerifierSuccess, OutcomeEvidenceSource::ExplicitHuman].into_iter().enumerate() {
        let mut row = StoredOutcomeEvidence {
            workspace_id: ws.clone(), source, evidence_family: source.evidence_family().to_owned(),
            signal_direction: source.default_direction().unwrap_or("negative").to_owned(),
            base_weight_milli: source.base_weight_milli(), evidence_ref: format!("untracked-observation-{n}"),
            agent_id: Some("historical-agent".to_owned()), task_id: Some("historical-task".to_owned()),
            run_id: Some("historical-run".to_owned()), observed_at: time.to_owned(),
            provenance_hash: String::new(), created_at: time.to_owned(),
        };
        row.provenance_hash = row.computed_provenance_hash();
        db.insert_outcome_evidence_for_recovery(&row).map_err(|e| e.to_string())?;
    }
    let state = read_state(&db)?;
    assert_eq!((state.0.len(), state.1.len(), state.2.len()), (1, 2, 2));
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

fn execute(db: &DbConnection, sql: &str) -> TestResult {
    db.execute_raw(sql).map(|_| ()).map_err(|e| e.to_string())
}

fn assert_refused(table: &str, mutate: impl FnOnce(&DbConnection) -> TestResult) -> TestResult {
    let fixture = fixture(RedactionLevel::None)?;
    let source = DbConnection::open_file(&fixture.database).map_err(|e| e.to_string())?;
    let original = read_state(&source)?;
    source.close().map_err(|e| e.to_string())?;
    let error = restore_backup_to_side_path_with_verification_hook(&fixture.options, |path| {
        let db = DbConnection::open_file(path).map_err(work_history_error)?;
        let before = read_state(&db).map_err(work_history_error)?;
        let tables = db.list_user_tables().map_err(work_history_error)?;
        let counts = tables.iter().map(|t| db.count_table_rows(t)).collect::<Result<Vec<_>, _>>().map_err(work_history_error)?;
        mutate(&db).map_err(work_history_error)?;
        assert_ne!(read_state(&db).map_err(work_history_error)?, before, "mutation must change actual readable rows");
        let after_counts = tables.iter().map(|t| db.count_table_rows(t)).collect::<Result<Vec<_>, _>>().map_err(work_history_error)?;
        assert_eq!(counts, after_counts, "a count-only fence must not detect this mutation");
        db.close().map_err(work_history_error)
    }).err().ok_or("published altered learning evidence")?;
    assert!(error.message().contains(&format!("Restored durable content differs for {table}")), "{}", error.message());
    assert!(!error.message().contains("PRIVATE_SENTINEL"));
    assert!(!fixture.options.side_path.join(WORKSPACE_MARKER).exists());
    let source = DbConnection::open_file(&fixture.database).map_err(|e| e.to_string())?;
    assert_eq!(read_state(&source)?, original);
    assert_eq!(source.count_table_rows("feedback_events").map_err(|e| e.to_string())?, 0);
    source.close().map_err(|e| e.to_string())?;
    assert_eq!(verify_backup(&BackupVerifyOptions {
        workspace_path: fixture.options.workspace_path, backup_path: fixture.options.backup_path,
    }).map_err(|e| e.message())?.status, "verified");
    Ok(())
}

fn rewrite_quarantine_payload(db: &DbConnection, invalid_source: bool) -> TestResult {
    let id = quarantine_id(usize::from(invalid_source));
    let mut row = db.list_feedback_quarantine(&workspace_id(), None).map_err(|e| e.to_string())?
        .into_iter().find(|row| row.id == id).ok_or("missing quarantine")?;
    if !invalid_source { row.weight = 0.75; }
    let hash = quarantine_payload_hash(&row).map_err(|e| e.message())?.ok_or("missing event")?;
    execute(db, &format!("UPDATE feedback_quarantine SET weight = {}, raw_event_hash = '{hash}' WHERE id = '{id}'", row.weight))?;
    let actual = db.list_feedback_quarantine(&workspace_id(), None).map_err(|e| e.to_string())?
        .into_iter().find(|row| row.id == id).ok_or("missing changed quarantine")?;
    assert_eq!(quarantine_payload_hash(&actual).map_err(|e| e.message())?.as_deref(), Some(actual.raw_event_hash.as_str()));
    Ok(())
}

fn rewrite_outcome(db: &DbConnection, change: impl FnOnce(&mut StoredOutcomeEvidence)) -> TestResult {
    let mut row = db.list_outcome_evidence_for_recovery(&workspace_id()).map_err(|e| e.to_string())?
        .into_iter().find(|row| row.source == OutcomeEvidenceSource::VerifierSuccess).ok_or("missing verifier result")?;
    let original_ref = row.evidence_ref.clone();
    let original_source = row.source.as_str();
    change(&mut row);
    row.provenance_hash = row.computed_provenance_hash();
    execute(db, &format!("UPDATE outcome_evidence_rows SET source_kind = '{}', evidence_family = '{}', base_weight_milli = {}, evidence_ref = '{}', observed_at = '{}', provenance_hash = '{}' WHERE source_kind = '{original_source}' AND evidence_ref = '{original_ref}'", row.source.as_str(), row.evidence_family, row.base_weight_milli, row.evidence_ref, row.observed_at, row.provenance_hash))?;
    let actual = db.list_outcome_evidence_for_recovery(&workspace_id()).map_err(|e| e.to_string())?
        .into_iter().find(|actual| actual.evidence_ref == row.evidence_ref && actual.source == row.source).ok_or("missing changed outcome")?;
    assert_eq!(actual, row);
    assert_eq!(actual.provenance_hash, actual.computed_provenance_hash());
    assert_eq!(actual.base_weight_milli, actual.source.base_weight_milli());
    Ok(())
}

#[test]
fn signal_fence_rejects_observation_direction_change() -> TestResult {
    assert_refused("learning_observations", |db| execute(db, "UPDATE learning_observations SET signal = 'harmful'"))
}

#[test]
fn signal_fence_rejects_fabricated_feedback_review() -> TestResult {
    assert_refused("feedback_quarantine", |db| execute(db, "UPDATE feedback_quarantine SET status = 'rejected', reviewed_at = '2026-09-02T00:00:00Z', reviewed_by = 'PRIVATE_SENTINEL'"))
}

#[test]
fn signal_fence_rejects_self_consistent_feedback_weight_change() -> TestResult {
    assert_refused("feedback_quarantine", |db| rewrite_quarantine_payload(db, false))
}

#[test]
fn signal_fence_rejects_repairing_invalid_source_into_releasable_feedback() -> TestResult {
    assert_refused("feedback_quarantine", |db| rewrite_quarantine_payload(db, true))
}

#[test]
fn signal_fence_rejects_self_consistent_escalation_to_human_authority() -> TestResult {
    assert_refused("outcome_evidence_rows", |db| rewrite_outcome(db, |row| {
        row.source = OutcomeEvidenceSource::ExplicitHuman;
        row.evidence_family = row.source.evidence_family().to_owned();
        row.base_weight_milli = row.source.base_weight_milli();
    }))
}

#[test]
fn signal_fence_rejects_self_consistent_evidence_reattribution() -> TestResult {
    assert_refused("outcome_evidence_rows", |db| rewrite_outcome(db, |row| row.evidence_ref = "PRIVATE_SENTINEL".to_owned()))
}

#[test]
fn signal_fence_rejects_self_consistent_outcome_chronology_change() -> TestResult {
    assert_refused("outcome_evidence_rows", |db| rewrite_outcome(db, |row| row.observed_at = "2026-08-31T00:00:00Z".to_owned()))
}

#[test]
fn learning_signals_survive_rebackup_without_reapplying_or_validating_feedback() -> TestResult {
    for level in [RedactionLevel::None, RedactionLevel::Full] {
        let fixture = fixture(level)?;
        let first = restore_backup_to_side_path(&fixture.options).map_err(|e| e.message())?;
        let db = DbConnection::open_file(&first.restored_database_path).map_err(|e| e.to_string())?;
        let expected = read_state(&db)?;
        assert_eq!((expected.0.len(), expected.1.len(), expected.2.len()), (1, 2, 2));
        for row in &expected.1 {
            assert_eq!(row.status, "pending");
            assert!(row.reviewed_at.is_none());
            assert!(row.released_feedback_event_id.is_none());
            assert_eq!(quarantine_payload_hash(row).map_err(|e| e.message())?.as_deref() == Some(row.raw_event_hash.as_str()), row.id == quarantine_id(0));
        }
        assert_eq!(db.count_table_rows("feedback_events").map_err(|e| e.to_string())?, 0);
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
        assert_eq!(read_state(&db)?, expected, "observation identity, invalid source hashes, authority and chronology must remain exact");
        assert_eq!(db.count_table_rows("feedback_events").map_err(|e| e.to_string())?, 0);
        db.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}
