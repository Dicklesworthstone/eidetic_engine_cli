//! Golden artifacts for `CassImportReport::data_json` and
//! `CassImportReport::human_summary` (bd-308ge).
//!
//! Pins:
//! * the full-shape ee.import.cass.v1 `data` payload emitted by `data_json`,
//! * the single-line operator summary emitted by `human_summary`,
//! * the `[REDACTED_PATH]` substitution applied by
//!   `redact_import_report_source_ref` when source_id / session.source_path
//!   look path- or secret-shaped.
//!
//! Existing src/cass/import.rs has per-field unit checks but never byte-snapshots
//! the structural shape. A future agent renaming a key, swapping field order,
//! or relaxing redaction would slip past current coverage.

use std::env;
use std::fs;
use std::path::PathBuf;

use ee::cass::{CassImportReport, ImportSessionStatus, ImportedCassSession};
use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn golden_path(name: &str, extension: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join("cass")
        .join(format!("import_report_{name}.{extension}.golden"))
}

fn sort_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted: Vec<(String, Value)> = map
                .iter()
                .map(|(k, v)| (k.clone(), sort_value(v)))
                .collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            let mut out = serde_json::Map::new();
            for (k, v) in sorted {
                out.insert(k, v);
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(sort_value).collect()),
        other => other.clone(),
    }
}

fn canonicalize_json(value: &Value) -> Result<String, String> {
    let sorted = sort_value(value);
    let mut text =
        serde_json::to_string_pretty(&sorted).map_err(|error| format!("serialize: {error}"))?;
    text.push('\n');
    Ok(text)
}

fn assert_golden(name: &str, extension: &str, actual: &str) -> TestResult {
    let path = golden_path(name, extension);
    if env::var("UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&path, actual).map_err(|error| error.to_string())?;
        return Ok(());
    }
    let expected = fs::read_to_string(&path)
        .map_err(|error| format!("missing golden {}: {error}", path.display()))?;
    ensure(
        actual == expected,
        format!(
            "golden mismatch {}\n--- expected\n{expected}\n+++ actual\n{actual}",
            path.display()
        ),
    )
}

fn successful_two_session_report() -> CassImportReport {
    CassImportReport {
        schema: "ee.import.cass.v1",
        workspace_path: "ws-fixture-01".to_string(),
        database_path: Some("ws-fixture-01/ee.db".to_string()),
        source_id: "cass-fixture-source".to_string(),
        ledger_id: Some("ledger-fixture-01".to_string()),
        dry_run: false,
        since: None,
        sessions_discovered: 2,
        sessions_imported: 1,
        sessions_skipped: 1,
        spans_imported: 7,
        index_jobs_queued: 1,
        index_required_action: None,
        status: "ok".to_string(),
        sessions: vec![
            ImportedCassSession {
                source_path: "cass-fixture-source/session-aaa".to_string(),
                session_id: Some("session-aaa".to_string()),
                index_job_id: Some("idx-job-aaa".to_string()),
                status: ImportSessionStatus::Imported,
                spans_imported: 7,
                message_count: Some(42),
                missing_metadata: Vec::new(),
            },
            ImportedCassSession {
                source_path: "cass-fixture-source/session-bbb".to_string(),
                session_id: Some("session-bbb".to_string()),
                index_job_id: None,
                status: ImportSessionStatus::Skipped,
                spans_imported: 0,
                message_count: Some(9),
                missing_metadata: vec!["session_id_hint".to_string()],
            },
        ],
    }
}

fn dry_run_with_since_report() -> CassImportReport {
    CassImportReport {
        schema: "ee.import.cass.v1",
        workspace_path: "ws-fixture-02".to_string(),
        database_path: None,
        source_id: "cass-fixture-source".to_string(),
        ledger_id: None,
        dry_run: true,
        since: Some("90d".to_string()),
        sessions_discovered: 1,
        sessions_imported: 0,
        sessions_skipped: 0,
        spans_imported: 0,
        index_jobs_queued: 0,
        index_required_action: Some("would_enqueue".to_string()),
        status: "ok".to_string(),
        sessions: vec![ImportedCassSession {
            source_path: "cass-fixture-source/session-ccc".to_string(),
            session_id: Some("session-ccc".to_string()),
            index_job_id: None,
            status: ImportSessionStatus::WouldImport,
            spans_imported: 12,
            message_count: None,
            missing_metadata: Vec::new(),
        }],
    }
}

fn path_redacted_report() -> CassImportReport {
    CassImportReport {
        schema: "ee.import.cass.v1",
        workspace_path: "ws-fixture-03".to_string(),
        database_path: Some("ws-fixture-03/ee.db".to_string()),
        // Path-prefixed source_id must be substituted with [REDACTED_PATH].
        source_id: "/Users/test-fixture/private-source".to_string(),
        ledger_id: Some("ledger-fixture-03".to_string()),
        dry_run: false,
        since: None,
        sessions_discovered: 1,
        sessions_imported: 1,
        sessions_skipped: 0,
        spans_imported: 3,
        index_jobs_queued: 1,
        index_required_action: None,
        status: "ok".to_string(),
        sessions: vec![ImportedCassSession {
            // Path-prefixed session source_path must also be substituted.
            source_path: "/Volumes/external/cass/session-ddd".to_string(),
            session_id: Some("session-ddd".to_string()),
            index_job_id: Some("idx-job-ddd".to_string()),
            status: ImportSessionStatus::Imported,
            spans_imported: 3,
            message_count: Some(15),
            missing_metadata: Vec::new(),
        }],
    }
}

#[test]
fn cass_import_report_data_json_two_session_success_matches_golden() -> TestResult {
    let report = successful_two_session_report();
    let actual = canonicalize_json(&report.data_json())?;
    assert_golden("two_session_success", "json", &actual)
}

#[test]
fn cass_import_report_data_json_dry_run_with_since_matches_golden() -> TestResult {
    let report = dry_run_with_since_report();
    let actual = canonicalize_json(&report.data_json())?;
    assert_golden("dry_run_with_since", "json", &actual)
}

#[test]
fn cass_import_report_data_json_path_redacted_matches_golden() -> TestResult {
    let report = path_redacted_report();
    let actual = canonicalize_json(&report.data_json())?;
    assert_golden("path_redacted", "json", &actual)
}

#[test]
fn cass_import_report_human_summary_two_session_success_matches_golden() -> TestResult {
    let report = successful_two_session_report();
    let actual = report.human_summary();
    assert_golden("two_session_success", "summary", &actual)
}

#[test]
fn cass_import_report_human_summary_dry_run_with_since_matches_golden() -> TestResult {
    let report = dry_run_with_since_report();
    let actual = report.human_summary();
    assert_golden("dry_run_with_since", "summary", &actual)
}

#[test]
fn cass_import_report_human_summary_path_redacted_matches_golden() -> TestResult {
    // Redaction is only applied to source_id / session.source_path. The
    // human summary does not include those fields, but it must remain
    // stable across redaction-bearing reports.
    let report = path_redacted_report();
    let actual = report.human_summary();
    assert_golden("path_redacted", "summary", &actual)
}

#[test]
fn import_session_status_strings_remain_stable() -> TestResult {
    // The data_json snapshot already pins these values, but assert the
    // enum->str surface directly so a future agent renaming the enum
    // variant cannot accidentally change the wire string.
    ensure(
        ImportSessionStatus::Imported.as_str() == "imported",
        "Imported -> \"imported\"",
    )?;
    ensure(
        ImportSessionStatus::Skipped.as_str() == "skipped",
        "Skipped -> \"skipped\"",
    )?;
    ensure(
        ImportSessionStatus::WouldImport.as_str() == "would_import",
        "WouldImport -> \"would_import\"",
    )
}
