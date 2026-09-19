//! Real backup/recovery mutations that leave every durable row count unchanged.

use std::path::PathBuf;

use super::*;
use crate::config::WORKSPACE_MARKER;
use crate::core::backup::{
    BackupCreateOptions, BackupRestoreOptions, BackupVerifyOptions, create_backup,
    restore_backup_to_side_path, restore_backup_to_side_path_with_recovery_hooks, verify_backup,
    work_history_error,
};
use crate::db::{
    StoredCurationCandidate, StoredCurationTtlPolicy, StoredProcedure, StoredProcedureEvent,
};
use crate::models::{MemoryId, RedactionLevel, WorkspaceId};
use uuid::Uuid;

type TestResult = Result<(), String>;
type State = (
    Vec<StoredCurationCandidate>,
    Vec<StoredCurationTtlPolicy>,
    Vec<StoredProcedure>,
    Vec<StoredProcedureEvent>,
);

const TIME: &str = "2026-09-01T00:00:00Z";
const SECRET: &str = "api_key=lifecycle-private-canary";

struct Fixture {
    root: tempfile::TempDir,
    database: PathBuf,
    options: BackupRestoreOptions,
}

fn workspace_id() -> String {
    WorkspaceId::from_uuid(Uuid::from_u128(1)).to_string()
}

fn candidate_id(n: usize) -> String {
    format!("curate_{n:026}")
}

fn candidate(n: usize) -> StoredCurationCandidate {
    let (status, state) = [
        ("approved", "accepted"),
        ("rejected", "rejected"),
        ("applied", "applied"),
    ][n % 3];
    StoredCurationCandidate {
        id: candidate_id(n),
        workspace_id: workspace_id(),
        candidate_type: "promote".to_owned(),
        target_memory_id: Some(MemoryId::from_uuid(Uuid::from_u128(2)).to_string()),
        proposed_content: Some(SECRET.to_owned()),
        proposed_confidence: Some(0.75),
        proposed_trust_class: None,
        source_type: "human_request".to_owned(),
        source_id: None,
        reason: "Preserve explicit review decisions".to_owned(),
        confidence: 0.75,
        status: status.to_owned(),
        created_at: TIME.to_owned(),
        reviewed_at: Some(TIME.to_owned()),
        reviewed_by: Some("reviewer".to_owned()),
        applied_at: (status == "applied").then(|| TIME.to_owned()),
        ttl_expires_at: Some("2050-09-01T00:00:00Z".to_owned()),
        review_state: state.to_owned(),
        snoozed_until: None,
        merged_into_candidate_id: None,
        state_entered_at: Some(TIME.to_owned()),
        last_action_at: Some(TIME.to_owned()),
        ttl_policy_id: Some("curation.proposed.default".to_owned()),
        derivation_source_refs_json: None,
        derivation_metadata_json: None,
    }
}

fn procedure(n: usize) -> StoredProcedure {
    StoredProcedure {
        id: format!("proc_lifecycle_{n}"),
        workspace_id: workspace_id(),
        name: "Check release".to_owned(),
        body: if n < 2 {
            SECRET
        } else {
            "Check the signature."
        }
        .to_owned(),
        level: "procedural".to_owned(),
        maturity: if n == 1 { "retired" } else { "mature" }.to_owned(),
        confidence: 0.75,
        utility: 0.5,
        importance: 0.75,
        evidence_uris: vec![],
        helpful_count: 7,
        harmful_count: 2,
        created_at: TIME.to_owned(),
        updated_at: TIME.to_owned(),
        last_promoted_at: Some(TIME.to_owned()),
        last_validated_at: Some(TIME.to_owned()),
        retired_at: (n == 1).then(|| TIME.to_owned()),
        retire_reason: (n == 1).then(|| "Superseded release procedure".to_owned()),
    }
}

fn read_state(db: &DbConnection) -> Result<State, String> {
    let ws = workspace_id();
    Ok((
        db.list_curation_candidates(&ws, None, None, None)
            .map_err(|e| e.to_string())?,
        db.list_curation_ttl_policies().map_err(|e| e.to_string())?,
        db.list_procedures_for_recovery(&ws)
            .map_err(|e| e.to_string())?,
        db.list_procedure_events_for_recovery(&ws)
            .map_err(|e| e.to_string())?,
    ))
}

fn fixture(redaction: RedactionLevel) -> Result<Fixture, String> {
    let (root, workspace, database) =
        crate::core::backup::tests::fixture().map_err(|e| e.message())?;
    let db = DbConnection::open_file(&database).map_err(|e| e.to_string())?;
    for n in 0..3 {
        db.insert_curation_candidate_for_recovery(&candidate(n))
            .map_err(|e| e.to_string())?;
        db.insert_procedure_for_recovery(&procedure(n))
            .map_err(|e| e.to_string())?;
    }
    db.insert_procedure_event_for_recovery(&StoredProcedureEvent {
        id: "pevt_lifecycle_0".to_owned(),
        workspace_id: workspace_id(),
        procedure_id: procedure(0).id,
        event_type: "outcome_helpful".to_owned(),
        from_maturity: Some("mature".to_owned()),
        to_maturity: Some("mature".to_owned()),
        reason: Some("Observed release success".to_owned()),
        evidence_uris: vec![],
        actor: Some("reviewer".to_owned()),
        created_at: TIME.to_owned(),
    })
    .map_err(|e| e.to_string())?;
    let state = read_state(&db)?;
    assert_eq!((state.0.len(), state.2.len(), state.3.len()), (3, 3, 1));
    assert!(!state.1.is_empty());
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

fn corrupt(db_path: &std::path::Path, sql: &str) -> Result<(), DomainError> {
    let db = DbConnection::open_file(db_path).map_err(work_history_error)?;
    let original = read_state(&db).map_err(work_history_error)?;
    let tables = db.list_user_tables().map_err(work_history_error)?;
    let counts = tables
        .iter()
        .map(|t| db.count_table_rows(t))
        .collect::<Result<Vec<_>, _>>()
        .map_err(work_history_error)?;
    db.execute_raw(sql).map_err(work_history_error)?;
    assert_ne!(read_state(&db).map_err(work_history_error)?, original);
    let after = tables
        .iter()
        .map(|t| db.count_table_rows(t))
        .collect::<Result<Vec<_>, _>>()
        .map_err(work_history_error)?;
    assert_eq!(
        counts, after,
        "count-only verification cannot catch this change"
    );
    db.close().map_err(work_history_error)
}

fn assert_refused(table: &str, sql: &str, redaction: RedactionLevel, late: bool) -> TestResult {
    let fixture = fixture(redaction)?;
    let source = DbConnection::open_file(&fixture.database).map_err(|e| e.to_string())?;
    let original = read_state(&source)?;
    source.close().map_err(|e| e.to_string())?;
    let error = restore_backup_to_side_path_with_recovery_hooks(
        &fixture.options,
        |path| if late { Ok(()) } else { corrupt(path, sql) },
        |path| if late { corrupt(path, sql) } else { Ok(()) },
    )
    .err()
    .ok_or("published altered lifecycle state")?;
    assert!(
        error
            .message()
            .contains(&format!("Restored durable content differs for {table}")),
        "{}",
        error.message()
    );
    assert!(!error.message().contains("PRIVATE_MUTATION"));
    assert!(!error.message().contains("lifecycle-private-canary"));
    assert!(!fixture.options.side_path.join(WORKSPACE_MARKER).exists());
    let source = DbConnection::open_file(&fixture.database).map_err(|e| e.to_string())?;
    assert_eq!(read_state(&source)?, original);
    source.close().map_err(|e| e.to_string())?;
    assert_eq!(
        verify_backup(&BackupVerifyOptions {
            workspace_path: fixture.options.workspace_path,
            backup_path: fixture.options.backup_path,
        })
        .map_err(|e| e.message())?
        .status,
        "verified"
    );
    Ok(())
}

#[test]
fn restored_lifecycle_survives_rebackup_without_reviving_retirement_or_replaying_events()
-> TestResult {
    for redaction in [
        RedactionLevel::None,
        RedactionLevel::Standard,
        RedactionLevel::Full,
    ] {
        let fixture = fixture(redaction)?;
        let mut options = fixture.options;
        for generation in 0..2 {
            let restored = restore_backup_to_side_path(&options).map_err(|e| e.message())?;
            assert_eq!(restored.restored_curation_candidate_count, 3);
            assert_eq!(restored.restored_procedure_count, 3);
            assert_eq!(restored.restored_procedure_event_count, 1);
            let db = DbConnection::open_file(&restored.restored_database_path)
                .map_err(|e| e.to_string())?;
            let state = read_state(&db)?;
            assert_eq!((state.0.len(), state.2.len(), state.3.len()), (3, 3, 1));
            assert_eq!(state.3[0].procedure_id, procedure(0).id);
            for row in &state.2 {
                assert_eq!((row.helpful_count, row.harmful_count), (7, 2));
                if row.id == procedure(1).id {
                    assert_eq!(row.maturity, "retired");
                    assert_eq!(row.retired_at.as_deref(), Some(TIME));
                }
            }
            for row in &state.0 {
                let original = (0..3)
                    .map(candidate)
                    .find(|c| c.id == row.id)
                    .ok_or("unknown candidate")?;
                if redaction == RedactionLevel::None {
                    assert_eq!(*row, original);
                } else if original.status == "approved" {
                    assert_eq!(row.status, "pending");
                    assert_eq!(row.review_state, "needs_evidence");
                    assert!(row.ttl_policy_id.is_none());
                } else {
                    assert_eq!(row.status, original.status);
                }
            }
            if redaction != RedactionLevel::None {
                assert!(
                    !serde_json::to_string(&state)
                        .map_err(|e| e.to_string())?
                        .contains("lifecycle-private-canary")
                );
            }
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
                options.side_path = fixture.root.path().join("restored-again");
            }
        }
    }
    Ok(())
}

#[test]
fn lifecycle_fence_rejects_rejected_proposal_reapproval() -> TestResult {
    assert_refused(
        "curation_candidates",
        "UPDATE curation_candidates SET status = 'approved', review_state = 'accepted' WHERE status = 'rejected'",
        RedactionLevel::None,
        false,
    )
}

#[test]
fn lifecycle_fence_rejects_rewritten_proposal() -> TestResult {
    assert_refused(
        "curation_candidates",
        "UPDATE curation_candidates SET proposed_content = 'PRIVATE_MUTATION'",
        RedactionLevel::None,
        false,
    )
}

#[test]
fn lifecycle_fence_rejects_changed_automatic_promotion_policy() -> TestResult {
    assert_refused(
        "curation_ttl_policies",
        "UPDATE curation_ttl_policies SET requires_evidence_count = requires_evidence_count + 1",
        RedactionLevel::None,
        false,
    )
}

#[test]
fn lifecycle_fence_rejects_redaction_review_bypass() -> TestResult {
    assert_refused(
        "curation_candidates",
        "UPDATE curation_candidates SET status = 'approved', review_state = 'accepted' WHERE review_state = 'needs_evidence'",
        RedactionLevel::Standard,
        false,
    )
}

#[test]
fn lifecycle_fence_rejects_lost_redaction_review_clock() -> TestResult {
    assert_refused(
        "curation_candidates",
        "UPDATE curation_candidates SET last_action_at = NULL WHERE review_state = 'needs_evidence'",
        RedactionLevel::Standard,
        false,
    )
}

#[test]
fn lifecycle_fence_rejects_changed_procedure_instructions() -> TestResult {
    assert_refused(
        "procedures",
        "UPDATE procedures SET body = 'PRIVATE_MUTATION'",
        RedactionLevel::None,
        false,
    )
}

#[test]
fn lifecycle_fence_rejects_revived_retired_procedure_after_redaction() -> TestResult {
    assert_refused(
        "procedures",
        "UPDATE procedures SET maturity = 'mature' WHERE maturity = 'retired'",
        RedactionLevel::Standard,
        false,
    )
}

#[test]
fn lifecycle_fence_rejects_replayed_procedure_feedback() -> TestResult {
    assert_refused(
        "procedures",
        "UPDATE procedures SET helpful_count = helpful_count + 1",
        RedactionLevel::None,
        false,
    )
}

#[test]
fn lifecycle_fence_rejects_reparented_procedure_evidence() -> TestResult {
    assert_refused(
        "procedure_events",
        "UPDATE procedure_events SET procedure_id = 'proc_lifecycle_2'",
        RedactionLevel::None,
        false,
    )
}

#[test]
fn lifecycle_fence_rejects_reapproval_after_index_rebuilding() -> TestResult {
    assert_refused(
        "curation_candidates",
        "UPDATE curation_candidates SET status = 'approved', review_state = 'accepted' WHERE status = 'rejected'",
        RedactionLevel::None,
        true,
    )
}

#[test]
fn lifecycle_fence_rejects_retirement_loss_after_index_rebuilding() -> TestResult {
    assert_refused(
        "procedures",
        "UPDATE procedures SET maturity = 'mature' WHERE maturity = 'retired'",
        RedactionLevel::None,
        true,
    )
}
