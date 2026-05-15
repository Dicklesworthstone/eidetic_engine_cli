//! N15.3 WAL retention contract test (bd-17c65.14.15.4).
//!
//! Pins the two surfaces required by the N15.3 bead spec that the
//! foundation work has not yet shipped:
//!
//! 1. The `ee_wal_holds` table is declared in the canonical migration
//!    list (`ee::db::MIGRATIONS`) with the columns the lab capture
//!    path needs to pin a captured WAL LSN: workspace_id, episode_id,
//!    lsn, created_at, expires_at. Without this table the "PATH 1"
//!    WAL-hold path from the bead spec cannot persist a hold, and
//!    later replays cannot detect that a snapshot was vacuumed.
//!
//! 2. `CaptureReport` JSON exposes a `wal_retention_kind` field whose
//!    value is one of `hold` or `best_effort`. Per the bead spec, an
//!    agent inspecting an episode must be able to distinguish a
//!    capture whose snapshot is guaranteed reachable (PATH 1, hold
//!    persisted) from one whose snapshot may be lost on the next
//!    FrankenSQLite checkpoint (PATH 2, best-effort honest
//!    degradation).
//!
//! Both assertions are forward-looking pins: today they fail, marking
//! the implementation gap visibly. When N15.3 lands the schema +
//! report field, this file flips to passing without modification.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use ee::core::lab::{CaptureOptions, capture_episode};
use ee::db::DbConnection;
use tempfile::TempDir;

type TestResult = Result<(), String>;

fn migrated_db() -> Result<(TempDir, DbConnection), String> {
    let dir = tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))?;
    let db_path = dir.path().join(".ee").join("ee.db");
    std::fs::create_dir_all(db_path.parent().expect("db parent"))
        .map_err(|error| format!("mkdir: {error}"))?;
    let conn = DbConnection::open_file(&db_path).map_err(|error| format!("open db: {error}"))?;
    conn.migrate()
        .map_err(|error| format!("migrate: {error}"))?;
    Ok((dir, conn))
}

#[test]
fn ee_wal_holds_table_present_after_canonical_migrate() -> TestResult {
    let (_dir, conn) = migrated_db()?;
    let tables = conn
        .list_user_tables()
        .map_err(|error| format!("list_user_tables: {error}"))?;

    if !tables.iter().any(|name| name == "ee_wal_holds") {
        return Err(format!(
            "N15.3 WAL retention gap: ee_wal_holds is not declared in canonical \
             MIGRATIONS. The bead spec requires this table so `ee lab capture` can \
             persist a WAL hold (workspace_id, episode_id, lsn, created_at, \
             expires_at) and so the daemon decay sweep can age out expired holds. \
             Tables present: {tables:?}"
        ));
    }
    Ok(())
}

#[test]
fn ee_wal_holds_supports_required_columns() -> TestResult {
    let (_dir, conn) = migrated_db()?;
    let tables = conn
        .list_user_tables()
        .map_err(|error| format!("list_user_tables: {error}"))?;
    if !tables.iter().any(|name| name == "ee_wal_holds") {
        return Err(
            "N15.3 WAL retention gap: ee_wal_holds table missing; cannot verify columns. \
             See ee_wal_holds_table_present_after_canonical_migrate."
                .to_string(),
        );
    }

    conn.execute_raw(
        "INSERT INTO ee_wal_holds \
            (workspace_id, episode_id, lsn, created_at, expires_at) \
         VALUES \
            ('ws_contract_test', 'ep_contract_test', 'lsn-0001', \
             '2026-01-01T00:00:00Z', '2026-12-31T00:00:00Z')",
    )
    .map_err(|error| {
        format!(
            "N15.3 WAL retention gap: ee_wal_holds is missing one of the required \
             columns (workspace_id, episode_id, lsn, created_at, expires_at). \
             Insert failed with: {error}"
        )
    })?;

    let count = conn
        .count_table_rows("ee_wal_holds")
        .map_err(|error| format!("count_table_rows: {error}"))?;
    if count != 1 {
        return Err(format!(
            "expected exactly 1 row in ee_wal_holds after insert, got {count}"
        ));
    }
    Ok(())
}

#[test]
fn capture_report_serializes_wal_retention_kind() -> TestResult {
    let options = CaptureOptions {
        workspace: PathBuf::from("."),
        task_input: Some("wal-retention-kind contract".to_string()),
        dry_run: true,
        ..Default::default()
    };
    let report = capture_episode(&options).map_err(|error| error.message())?;
    let json = report.to_json();
    let value: serde_json::Value = serde_json::from_str(&json)
        .map_err(|error| format!("capture report JSON did not parse: {error}; json={json}"))?;

    let kind = value
        .get("wal_retention_kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            format!(
                "N15.3 WAL retention gap: CaptureReport JSON does not include \
                 `wal_retention_kind`. Per the bead spec, every captured episode \
                 must report whether its WAL snapshot is held (`hold`) or \
                 best-effort (`best_effort`) so agents can tell whether replay \
                 is guaranteed. Got JSON: {json}"
            )
        })?;

    if kind != "hold" && kind != "best_effort" {
        return Err(format!(
            "N15.3 WAL retention gap: wal_retention_kind must be `hold` or \
             `best_effort`, got `{kind}`"
        ));
    }
    Ok(())
}
