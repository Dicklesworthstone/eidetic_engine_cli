//! bd-1clqr.2: public recorder path tests for a targeted non-lib RCH proof.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};
use ee::core::session_budget::{
    SESSION_BUDGET_REDACTION_STATUS, SessionBudgetCommand, SessionBudgetCommandClass,
    SessionBudgetCommandSurface, SessionBudgetCorrelation, SessionBudgetCost, SessionBudgetDbCost,
    SessionBudgetDerivedAssetCost, SessionBudgetEvidenceKind, SessionBudgetEvidenceRef,
    SessionBudgetNormalizedCommand, SessionBudgetOptInSource, SessionBudgetRchCost,
    SessionBudgetRecordOutcome, SessionBudgetRecorder, SessionBudgetRecorderConfig,
    SessionBudgetStaleSource, session_budget_hash,
};
use ee::models::SESSION_BUDGET_SCHEMA_V1;
use serde_json::Value;

static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

type TestResult = Result<(), String>;

fn ledger_path(name: &str) -> PathBuf {
    let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "ee-session-budget-integration-{name}-{}-{id}.jsonl",
        std::process::id()
    ))
}

fn config(path: PathBuf, max_rows: usize, max_age_days: u32) -> SessionBudgetRecorderConfig {
    SessionBudgetRecorderConfig::new(
        path,
        NonZeroUsize::new(max_rows).expect("max rows"),
        NonZeroU32::new(max_age_days).expect("max age days"),
        SessionBudgetOptInSource::TestFixture,
        1.0,
    )
    .expect("valid config")
}

fn observation(
    sequence: u32,
    recorded_at: DateTime<Utc>,
) -> ee::core::session_budget::SessionBudgetObservation {
    ee::core::session_budget::SessionBudgetObservation {
        recorded_at,
        workspace_fingerprint: "a1b2c3d4e5f6".to_owned(),
        correlation: SessionBudgetCorrelation {
            session_id: "sess_public_recorder".to_owned(),
            command_id: format!("cmd_public_recorder_{sequence:04}"),
            parent_command_id: None,
            task_hash: session_budget_hash(format!("task-{sequence}")),
            pack_id: None,
            rch_job_id: None,
            agent_mail_thread_id: Some("bd-1clqr.2".to_owned()),
            bead_id: Some("bd-1clqr.2".to_owned()),
        },
        command: SessionBudgetCommand {
            surface: SessionBudgetCommandSurface::Recall,
            command_class: SessionBudgetCommandClass::ReadOnly,
            read_only: true,
            durable_mutation: false,
            normalized_command: SessionBudgetNormalizedCommand::EeRecall,
        },
        cost: SessionBudgetCost {
            wall_clock_ms: u64::from(sequence) * 7,
            output_tokens_estimated: 12,
            output_tokens_returned: 10,
            output_bytes: 128,
            pack_tokens_requested: 0,
            pack_tokens_used: 0,
            rch: SessionBudgetRchCost::default(),
            db: SessionBudgetDbCost {
                lock_wait_ms: 0,
                read_pool_acquire_ms: 0,
                write_attempt_count: 1,
            },
            derived_assets: SessionBudgetDerivedAssetCost {
                freshness_penalty_ms: 0,
                stale_sources: vec![SessionBudgetStaleSource::None],
            },
        },
        degraded_groups: Vec::new(),
        evidence: vec![SessionBudgetEvidenceRef {
            kind: SessionBudgetEvidenceKind::Timer,
            r#ref: Some(format!("timer-{sequence}")),
            hash: Some(session_budget_hash(format!("timer-{sequence}"))),
        }],
    }
}

fn rows(path: &Path) -> Result<Vec<Value>, String> {
    fs::read_to_string(path)
        .map_err(|error| error.to_string())?
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).map_err(|error| error.to_string()))
        .collect()
}

#[test]
fn disabled_public_recorder_skips_estimator_and_file_work() -> TestResult {
    let path = ledger_path("disabled");
    let recorder = SessionBudgetRecorder::disabled();
    let mut estimator_called = false;

    let outcome = recorder
        .record_with(|| {
            estimator_called = true;
            Ok(observation(
                1,
                Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap(),
            ))
        })
        .map_err(|error| error.to_string())?;

    assert_eq!(outcome, SessionBudgetRecordOutcome::disabled());
    assert!(!estimator_called);
    assert!(!path.exists());
    Ok(())
}

#[test]
fn enabled_public_recorder_writes_bounded_redacted_rows() -> TestResult {
    let path = ledger_path("bounded");
    let recorder = SessionBudgetRecorder::enabled(config(path.clone(), 2, 30));
    let base = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();

    recorder
        .record_with(|| Ok(observation(1, base)))
        .map_err(|error| error.to_string())?;
    recorder
        .record_with(|| Ok(observation(2, base + ChronoDuration::seconds(1))))
        .map_err(|error| error.to_string())?;
    let outcome = recorder
        .record_with(|| Ok(observation(3, base + ChronoDuration::seconds(2))))
        .map_err(|error| error.to_string())?;

    assert_eq!(outcome.rows_after, 2);
    assert_eq!(outcome.evicted_rows, 1);
    let rows = rows(&path)?;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0]["correlation"]["commandId"],
        "cmd_public_recorder_0002"
    );
    assert_eq!(
        rows[1]["correlation"]["commandId"],
        "cmd_public_recorder_0003"
    );
    assert_eq!(rows[1]["schema"], SESSION_BUDGET_SCHEMA_V1);
    assert_eq!(
        rows[1]["privacy"]["redactionStatus"],
        SESSION_BUDGET_REDACTION_STATUS
    );
    assert_eq!(rows[1]["privacy"]["rawCommandStored"], false);
    assert_eq!(rows[1]["privacy"]["rawOutputStored"], false);
    assert_eq!(rows[1]["privacy"]["contentStored"], false);
    assert_eq!(rows[1]["retention"]["maxRowsPerWorkspace"], 2);
    Ok(())
}

#[test]
fn public_recorder_prunes_expired_rows() -> TestResult {
    let path = ledger_path("age");
    let recorder = SessionBudgetRecorder::enabled(config(path.clone(), 8, 1));
    let old = Utc.with_ymd_and_hms(2026, 6, 10, 12, 0, 0).unwrap();
    let fresh = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();

    recorder
        .record_with(|| Ok(observation(1, old)))
        .map_err(|error| error.to_string())?;
    let outcome = recorder
        .record_with(|| Ok(observation(2, fresh)))
        .map_err(|error| error.to_string())?;

    assert_eq!(outcome.evicted_rows, 1);
    let rows = rows(&path)?;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]["correlation"]["commandId"],
        "cmd_public_recorder_0002"
    );
    assert_eq!(rows[0]["retention"]["maxAgeDays"], 1);
    Ok(())
}
