//! CLI boundary regressions for explicit CASS import-to-review proposals.

use std::path::Path;

use clap::Parser;

use super::{
    CassImportReport, Cli, Command, ImportCommand, cass_import_human_summary,
    cass_import_response_v2, reconcile_cass_import_index,
};
use crate::cass::import::{CassHistoryRefusal, ImportSessionStatus, ImportedCassSession};
use crate::core::cass_review::review_imported_sessions;
use crate::db::{CreateWorkspaceInput, DbConnection, EvidenceAdmissionTally};
use crate::models::SessionId;
use crate::output::{FieldSelector, apply_field_selector_to_json};

type TestResult = Result<(), String>;

fn import_report(workspace: &Path, dry_run: bool) -> CassImportReport {
    CassImportReport {
        schema: crate::models::IMPORT_CASS_SCHEMA_V1,
        workspace_path: workspace.display().to_string(),
        database_path: Some(workspace.join("custom-import.db").display().to_string()),
        source_id: "cli-proposal-test".to_owned(),
        ledger_id: None,
        dry_run,
        since: None,
        sessions_discovered: 0,
        sessions_imported: 0,
        sessions_skipped: 0,
        spans_imported: 0,
        index_jobs_queued: 0,
        index_required_action: None,
        status: "completed".to_owned(),
        sessions: Vec::new(),
        evidence_admission: EvidenceAdmissionTally::default(),
    }
}

fn session(number: u128, status: ImportSessionStatus) -> ImportedCassSession {
    ImportedCassSession {
        source_path: format!("source-{number}.jsonl"),
        session_id: Some(SessionId::from_uuid(uuid::Uuid::from_u128(number)).to_string()),
        index_job_id: None,
        status,
        spans_imported: 0,
        message_count: Some(1),
        missing_metadata: Vec::new(),
    }
}

#[test]
fn cass_import_proposals_are_explicit_and_require_captured_spans() -> TestResult {
    for (argv, propose, dry_run) in [
        (vec!["ee", "import", "cass"], false, false),
        (vec!["ee", "import", "cass", "--propose"], true, false),
        (
            vec!["ee", "import", "cass", "--propose", "--dry-run"],
            true,
            true,
        ),
    ] {
        let cli = Cli::try_parse_from(argv).map_err(|error| error.to_string())?;
        let Some(Command::Import(ImportCommand::Cass(options))) = cli.command else {
            return Err("expected CASS import arguments".to_owned());
        };
        assert_eq!(options.propose, propose);
        assert_eq!(options.dry_run, dry_run);
    }
    assert!(Cli::try_parse_from(["ee", "import", "cass", "--propose", "--no-spans"]).is_err());
    Ok(())
}

#[test]
fn cass_import_proposal_preview_preserves_absent_store_and_standard_receipt() -> TestResult {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = directory.path().join("absent-workspace");
    let mut report = import_report(&workspace, true);
    report.sessions_discovered = 1;
    report
        .sessions
        .push(session(1, ImportSessionStatus::WouldImport));
    let review = review_imported_sessions(&report);
    assert_eq!(review.sessions_planned, 1);
    assert!(!review.durable_mutation);
    // Even a caller claiming proposal mutation cannot open a store in dry-run.
    assert!(reconcile_cass_import_index(&mut report, true).is_none());
    let response = cass_import_response_v2(&report, None, Some(&review));
    let standard = apply_field_selector_to_json(&response.to_string(), &FieldSelector::standard())
        .map_err(|error| error.to_string())?;
    let standard: serde_json::Value =
        serde_json::from_str(&standard).map_err(|error| error.to_string())?;
    assert_eq!(standard["data"]["review"]["status"], "planned");
    assert_eq!(standard["data"]["review"]["sessionsPlanned"], 1);
    assert_eq!(standard["data"]["review"]["candidatesPersisted"], 0);
    assert!(
        cass_import_response_v2(&report, None, None)["data"]
            .get("review")
            .is_none()
    );
    assert!(cass_import_human_summary(&report, None, Some(&review)).contains("review planned"));
    assert!(!workspace.exists());
    Ok(())
}

#[test]
fn cass_import_keeps_history_index_and_review_failures_in_one_success_receipt() -> TestResult {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = directory.path().join("missing-after-import");
    let mut report = import_report(&workspace, false);
    report.sessions_discovered = 2;
    report.sessions_imported = 1;
    report
        .sessions
        .push(session(1, ImportSessionStatus::Imported));
    report.sessions.push(session(
        2,
        ImportSessionStatus::Refused(CassHistoryRefusal::Changed),
    ));
    let review = review_imported_sessions(&report);
    assert_eq!(review.sessions_failed, 1);
    assert_eq!(review.sessions_refused, 1);
    let index = report.record_index_publish_failure("the committed generation is not published");
    let response = cass_import_response_v2(&report, Some(&index), Some(&review));
    assert_eq!(response["success"], true);
    assert_eq!(response["data"]["sessionsImported"], 1);
    assert_eq!(response["data"]["review"]["partialFailure"], true);
    let degraded = response["degraded"]
        .as_array()
        .ok_or_else(|| "missing degradation array".to_owned())?;
    for expected in [
        "cass_import_history_refused",
        "cass_import_index_publish_failed",
        "cass_import_review_partial",
    ] {
        assert!(degraded.iter().any(|entry| entry["code"] == expected));
    }
    let human = cass_import_human_summary(&report, Some(&index), Some(&review));
    assert!(human.contains("review partial"));
    assert!(human.contains("cass_import_review_partial"));
    assert!(human.contains("--database"));
    assert!(!workspace.exists());
    Ok(())
}

#[test]
fn proposal_backfill_does_not_skip_index_verification_without_import_jobs() -> TestResult {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let workspace = directory
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let mut report = import_report(&workspace, false);
    let database_path = report
        .database_path
        .as_deref()
        .ok_or("database binding missing")?;
    let connection = DbConnection::open_file(database_path).map_err(|error| error.to_string())?;
    connection.migrate().map_err(|error| error.to_string())?;
    connection
        .insert_workspace(
            &crate::core::workspace::stable_workspace_id(&workspace),
            &CreateWorkspaceInput {
                path: workspace.display().to_string(),
                name: Some("proposal-backfill".to_owned()),
            },
        )
        .map_err(|error| error.to_string())?;
    connection.close().map_err(|error| error.to_string())?;

    assert!(reconcile_cass_import_index(&mut report, false).is_none());
    let degradation = reconcile_cass_import_index(&mut report, true)
        .ok_or_else(|| "a proposal-only backfill must verify its unbuilt index".to_owned())?;
    assert_eq!(degradation.code, "cass_import_index_publish_failed");
    assert_eq!(report.index_jobs_queued, 0);
    let repair = report
        .index_required_action
        .as_deref()
        .ok_or("pending index repair missing")?;
    assert!(repair.contains("ee index rebuild"));
    assert!(repair.contains("--database"));
    assert!(repair.contains("custom-import.db"));
    assert_eq!(repair, degradation.repair);
    Ok(())
}
