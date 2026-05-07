use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};

use crate::steward::{DaemonForegroundOptions, DaemonForegroundReport, JobRunResult, JobType};

pub const SUBSYSTEM: &str = "serve";
pub const DAEMON_JOB_TABLE_SCHEMA_V1: &str = "ee.steward.daemon_job_table.v1";
pub const DAEMON_JOB_ROW_SCHEMA_V1: &str = "ee.steward.daemon_job_row.v1";
pub const DAEMON_STATUS_SCHEMA_V1: &str = "ee.steward.daemon_status.v1";
pub const DAEMON_RECOVERY_SCHEMA_V1: &str = "ee.steward.daemon_recovery.v1";
pub const DAEMON_WRITE_OWNER_IDENTITY: &str = "ee-daemon-single-write-owner";

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

#[derive(Clone, Debug)]
pub struct DaemonRunPlan {
    pub run_id: String,
    pub table_path: PathBuf,
    pub rows: Vec<DaemonJobRow>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonJobRow {
    pub schema: String,
    pub row_id: String,
    pub run_id: String,
    pub daemon_job_key: String,
    pub runner_job_id: String,
    pub tick: u32,
    pub job_type: String,
    pub status: String,
    pub outcome: Option<String>,
    pub workspace: String,
    pub write_owner_id: String,
    pub reason: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub recorded_at: String,
    pub duration_ms: Option<u64>,
    pub items_processed: Option<u64>,
    pub error: Option<String>,
    pub dry_run: bool,
    pub durable_mutation: bool,
    pub recovered_from_orphan: bool,
    pub recovery_reason: Option<String>,
}

impl DaemonJobRow {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        serde_json::to_value(self).unwrap_or_else(|_| {
            json!({
                "schema": DAEMON_JOB_ROW_SCHEMA_V1,
                "rowId": self.row_id,
                "daemonJobKey": self.daemon_job_key,
                "status": self.status,
            })
        })
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        matches!(self.status.as_str(), "pending" | "running")
    }
}

#[derive(Clone, Debug)]
pub struct DaemonRecoveryReport {
    pub workspace: String,
    pub table_path: PathBuf,
    pub recovered_at: String,
    pub scanned_rows: usize,
    pub open_jobs_cancelled: usize,
    pub recovered_rows: Vec<DaemonJobRow>,
}

impl DaemonRecoveryReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": DAEMON_RECOVERY_SCHEMA_V1,
            "workspace": self.workspace,
            "tablePath": self.table_path.display().to_string(),
            "recoveredAt": self.recovered_at,
            "scannedRows": self.scanned_rows,
            "openJobsCancelled": self.open_jobs_cancelled,
            "recoveredRows": self
                .recovered_rows
                .iter()
                .map(DaemonJobRow::data_json)
                .collect::<Vec<_>>(),
        })
    }
}

#[derive(Clone, Debug)]
pub struct DaemonStatusReport {
    pub workspace: String,
    pub requested_job_types: Vec<JobType>,
    pub table_path: PathBuf,
    pub row_count: usize,
    pub open_job_count: usize,
    pub recent_outcomes: Vec<DaemonJobRow>,
}

impl DaemonStatusReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        let spool_config = crate::core::WriteSpoolConfig::default();
        json!({
            "schema": DAEMON_STATUS_SCHEMA_V1,
            "command": "daemon status",
            "workspace": self.workspace,
            "running": self.open_job_count > 0,
            "daemonized": false,
            "foregroundAvailable": true,
            "backgroundAvailable": false,
            "supervisor": "asupersync_foreground",
            "jobTypes": self
                .requested_job_types
                .iter()
                .map(|job_type| job_type.as_str())
                .collect::<Vec<_>>(),
            "writeOwner": {
                "schema": crate::core::WRITE_OWNER_STATUS_SCHEMA_V1,
                "identity": DAEMON_WRITE_OWNER_IDENTITY,
                "mode": "single_process_foreground",
                "spool": {
                    "schema": crate::core::WRITE_SPOOL_STATUS_SCHEMA_V1,
                    "backpressureSchema": crate::core::WRITE_SPOOL_BACKPRESSURE_SCHEMA_V1,
                    "backpressureCode": crate::core::WRITE_SPOOL_BACKPRESSURE_CODE,
                    "maxPending": spool_config.max_pending,
                    "maxBatchSize": spool_config.max_batch_size,
                    "maxPendingBytes": spool_config.max_pending_bytes,
                    "maxQueueAgeMs": spool_config.max_queue_age_ms,
                }
            },
            "durable": {
                "schema": DAEMON_JOB_TABLE_SCHEMA_V1,
                "tablePath": self.table_path.display().to_string(),
                "rowCount": self.row_count,
                "openJobCount": self.open_job_count,
                "recentOutcomeCount": self.recent_outcomes.len(),
            },
            "recentOutcomes": self
                .recent_outcomes
                .iter()
                .map(DaemonJobRow::data_json)
                .collect::<Vec<_>>(),
            "recovery": {
                "schema": DAEMON_RECOVERY_SCHEMA_V1,
                "openJobsEligibleForCancellation": self.open_job_count,
                "repair": "Start ee daemon --foreground --once --json to recover orphaned pending/running daemon jobs."
            },
            "degraded": [
                {
                    "code": "daemon_background_mode_unimplemented",
                    "severity": "low",
                    "message": "Only bounded foreground daemon mode is implemented.",
                    "repair": "ee daemon --foreground --once --json"
                }
            ],
        })
    }
}

#[must_use]
pub fn daemon_job_table_path(workspace_path: &Path) -> PathBuf {
    workspace_path.join(".ee").join("daemon-jobs.jsonl")
}

pub fn record_daemon_foreground_start(
    workspace_path: &Path,
    options: &DaemonForegroundOptions,
) -> Result<DaemonRunPlan, String> {
    let table_path = daemon_job_table_path(workspace_path);
    if options.dry_run {
        return Ok(DaemonRunPlan {
            run_id: "dry-run".to_owned(),
            table_path,
            rows: Vec::new(),
        });
    }

    let recorded_at = Utc::now().to_rfc3339();
    let run_id = daemon_run_id(workspace_path, &recorded_at);
    let mut rows = Vec::new();
    for tick in 1..=options.tick_limit {
        for (offset, job_type) in options.job_types.iter().enumerate() {
            let runner_job_id = runner_job_id(offset);
            rows.push(DaemonJobRow {
                schema: DAEMON_JOB_ROW_SCHEMA_V1.to_owned(),
                row_id: row_id(&run_id, tick, &runner_job_id, "planned"),
                run_id: run_id.clone(),
                daemon_job_key: daemon_job_key(&run_id, tick, &runner_job_id),
                runner_job_id,
                tick,
                job_type: job_type.as_str().to_owned(),
                status: if tick == 1 { "running" } else { "pending" }.to_owned(),
                outcome: None,
                workspace: workspace_path.to_string_lossy().into_owned(),
                write_owner_id: DAEMON_WRITE_OWNER_IDENTITY.to_owned(),
                reason: format!("daemon foreground tick {tick} planned"),
                started_at: Some(recorded_at.clone()),
                completed_at: None,
                recorded_at: recorded_at.clone(),
                duration_ms: None,
                items_processed: None,
                error: None,
                dry_run: false,
                durable_mutation: false,
                recovered_from_orphan: false,
                recovery_reason: None,
            });
        }
    }

    append_daemon_job_rows(&table_path, &rows)?;
    Ok(DaemonRunPlan {
        run_id,
        table_path,
        rows,
    })
}

pub fn record_daemon_foreground_report(
    workspace_path: &Path,
    report: &DaemonForegroundReport,
    run_id: &str,
) -> Result<Vec<DaemonJobRow>, String> {
    if report.dry_run || run_id == "dry-run" {
        return Ok(Vec::new());
    }

    let mut rows = Vec::new();
    for tick in &report.ticks {
        for result in &tick.report.results {
            rows.push(row_from_result(
                workspace_path,
                run_id,
                tick.tick,
                &tick.started_at,
                &tick.completed_at,
                result,
            ));
        }
    }

    append_daemon_job_rows(&daemon_job_table_path(workspace_path), &rows)?;
    Ok(rows)
}

pub fn recover_orphaned_daemon_jobs(
    workspace_path: &Path,
    reason: &str,
) -> Result<DaemonRecoveryReport, String> {
    let table_path = daemon_job_table_path(workspace_path);
    let rows = load_daemon_job_rows(workspace_path)?;
    let latest = latest_daemon_rows(&rows);
    let recovered_at = Utc::now().to_rfc3339();
    let mut recovered_rows = Vec::new();

    for row in latest.into_iter().filter(DaemonJobRow::is_open) {
        recovered_rows.push(DaemonJobRow {
            schema: DAEMON_JOB_ROW_SCHEMA_V1.to_owned(),
            row_id: row_id(
                &row.run_id,
                row.tick,
                &row.runner_job_id,
                "recovered-cancelled",
            ),
            run_id: row.run_id,
            daemon_job_key: row.daemon_job_key,
            runner_job_id: row.runner_job_id,
            tick: row.tick,
            job_type: row.job_type,
            status: "cancelled".to_owned(),
            outcome: Some("cancelled".to_owned()),
            workspace: row.workspace,
            write_owner_id: DAEMON_WRITE_OWNER_IDENTITY.to_owned(),
            reason: "daemon restart recovery".to_owned(),
            started_at: row.started_at,
            completed_at: Some(recovered_at.clone()),
            recorded_at: recovered_at.clone(),
            duration_ms: None,
            items_processed: None,
            error: Some(reason.to_owned()),
            dry_run: row.dry_run,
            durable_mutation: false,
            recovered_from_orphan: true,
            recovery_reason: Some(reason.to_owned()),
        });
    }

    if !recovered_rows.is_empty() {
        append_daemon_job_rows(&table_path, &recovered_rows)?;
    }

    Ok(DaemonRecoveryReport {
        workspace: workspace_path.to_string_lossy().into_owned(),
        table_path,
        recovered_at,
        scanned_rows: rows.len(),
        open_jobs_cancelled: recovered_rows.len(),
        recovered_rows,
    })
}

pub fn daemon_status_report(
    workspace_path: &Path,
    requested_job_types: &[JobType],
    recent_limit: usize,
) -> Result<DaemonStatusReport, String> {
    let rows = load_daemon_job_rows(workspace_path)?;
    let mut latest = latest_daemon_rows(&rows);
    latest.sort_by(|left, right| {
        right
            .recorded_at
            .cmp(&left.recorded_at)
            .then_with(|| left.daemon_job_key.cmp(&right.daemon_job_key))
    });
    let open_job_count = latest.iter().filter(|row| row.is_open()).count();
    latest.truncate(recent_limit);
    Ok(DaemonStatusReport {
        workspace: workspace_path.to_string_lossy().into_owned(),
        requested_job_types: requested_job_types.to_vec(),
        table_path: daemon_job_table_path(workspace_path),
        row_count: rows.len(),
        open_job_count,
        recent_outcomes: latest,
    })
}

pub fn load_daemon_job_rows(workspace_path: &Path) -> Result<Vec<DaemonJobRow>, String> {
    let table_path = daemon_job_table_path(workspace_path);
    if !table_path.exists() {
        return Ok(Vec::new());
    }
    let file = OpenOptions::new()
        .read(true)
        .open(&table_path)
        .map_err(|error| format!("Failed to open daemon job table: {error}"))?;
    let reader = BufReader::new(file);
    let mut rows = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line.map_err(|error| format!("Failed to read daemon job row: {error}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let row = serde_json::from_str::<DaemonJobRow>(&line).map_err(|error| {
            format!(
                "Failed to parse daemon job row {} in {}: {error}",
                index + 1,
                table_path.display()
            )
        })?;
        rows.push(row);
    }
    Ok(rows)
}

fn append_daemon_job_rows(table_path: &Path, rows: &[DaemonJobRow]) -> Result<(), String> {
    if rows.is_empty() {
        return Ok(());
    }
    let parent = table_path
        .parent()
        .ok_or_else(|| "Daemon job table path has no parent directory".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Failed to create daemon job table directory: {error}"))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(table_path)
        .map_err(|error| format!("Failed to open daemon job table for append: {error}"))?;
    for row in rows {
        serde_json::to_writer(&mut file, row)
            .map_err(|error| format!("Failed to serialize daemon job row: {error}"))?;
        file.write_all(b"\n")
            .map_err(|error| format!("Failed to write daemon job row: {error}"))?;
    }
    file.flush()
        .map_err(|error| format!("Failed to flush daemon job table: {error}"))
}

fn latest_daemon_rows(rows: &[DaemonJobRow]) -> Vec<DaemonJobRow> {
    let mut by_key = BTreeMap::new();
    for row in rows {
        by_key.insert(row.daemon_job_key.clone(), row.clone());
    }
    by_key.into_values().collect()
}

fn row_from_result(
    workspace_path: &Path,
    run_id: &str,
    tick: u32,
    tick_started_at: &str,
    tick_completed_at: &str,
    result: &JobRunResult,
) -> DaemonJobRow {
    let outcome = result.outcome.as_str();
    DaemonJobRow {
        schema: DAEMON_JOB_ROW_SCHEMA_V1.to_owned(),
        row_id: row_id(run_id, tick, &result.job_id, outcome),
        run_id: run_id.to_owned(),
        daemon_job_key: daemon_job_key(run_id, tick, &result.job_id),
        runner_job_id: result.job_id.clone(),
        tick,
        job_type: result.job_type.as_str().to_owned(),
        status: outcome.to_owned(),
        outcome: Some(outcome.to_owned()),
        workspace: workspace_path.to_string_lossy().into_owned(),
        write_owner_id: DAEMON_WRITE_OWNER_IDENTITY.to_owned(),
        reason: format!("daemon foreground tick {tick} completed"),
        started_at: Some(tick_started_at.to_owned()),
        completed_at: Some(tick_completed_at.to_owned()),
        recorded_at: Utc::now().to_rfc3339(),
        duration_ms: Some(result.duration_ms),
        items_processed: result.items_processed,
        error: result.error.clone(),
        dry_run: result.dry_run,
        durable_mutation: result
            .details
            .as_ref()
            .and_then(|details| details.get("durableMutation"))
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
        recovered_from_orphan: false,
        recovery_reason: None,
    }
}

fn daemon_run_id(workspace_path: &Path, recorded_at: &str) -> String {
    let input = format!("{}|{recorded_at}", workspace_path.display());
    let digest = blake3::hash(input.as_bytes()).to_hex().to_string();
    format!("daemon-run-{}", &digest[..16])
}

fn daemon_job_key(run_id: &str, tick: u32, runner_job_id: &str) -> String {
    format!("{run_id}:tick-{tick:06}:{runner_job_id}")
}

fn runner_job_id(offset: usize) -> String {
    format!("job-{:06}", offset.saturating_add(1))
}

fn row_id(run_id: &str, tick: u32, runner_job_id: &str, phase: &str) -> String {
    let input = format!(
        "{run_id}|{tick}|{runner_job_id}|{phase}|{}",
        Utc::now().to_rfc3339()
    );
    let digest = blake3::hash(input.as_bytes()).to_hex().to_string();
    format!("daemon-row-{}", &digest[..20])
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T>(actual: T, expected: T, label: &str) -> TestResult
    where
        T: std::fmt::Debug + PartialEq,
    {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{label}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn daemon_foreground_persists_rows_and_status_reports_write_owner() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let mut options = DaemonForegroundOptions::new(temp.path().to_string_lossy().into_owned());
        options.interval_ms = 0;
        options.job_types = vec![JobType::HealthCheck];

        let plan = record_daemon_foreground_start(temp.path(), &options)?;
        ensure(plan.rows.len(), 1, "planned rows")?;

        let report = crate::steward::run_daemon_foreground(&options)?;
        let terminal_rows = record_daemon_foreground_report(temp.path(), &report, &plan.run_id)?;
        ensure(terminal_rows.len(), 1, "terminal rows")?;

        let rows = load_daemon_job_rows(temp.path())?;
        ensure(rows.len(), 2, "persisted row count")?;

        let status = daemon_status_report(temp.path(), &[JobType::HealthCheck], 5)?;
        ensure(status.open_job_count, 0, "open jobs")?;
        ensure(status.row_count, 2, "status row count")?;
        let json = status.data_json();
        ensure(
            json["writeOwner"]["identity"].as_str(),
            Some(DAEMON_WRITE_OWNER_IDENTITY),
            "write owner identity",
        )?;
        ensure(
            json["writeOwner"]["spool"]["backpressureCode"].as_str(),
            Some(crate::core::WRITE_SPOOL_BACKPRESSURE_CODE),
            "backpressure code",
        )?;
        ensure(
            json["recentOutcomes"][0]["status"].as_str(),
            Some("success"),
            "recent terminal status",
        )
    }

    #[test]
    fn daemon_recovery_cancels_orphaned_planned_jobs() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let mut options = DaemonForegroundOptions::new(temp.path().to_string_lossy().into_owned());
        options.interval_ms = 0;
        options.job_types = vec![JobType::HealthCheck];

        let _plan = record_daemon_foreground_start(temp.path(), &options)?;
        let before = daemon_status_report(temp.path(), &[JobType::HealthCheck], 5)?;
        ensure(before.open_job_count, 1, "open before recovery")?;

        let recovery = recover_orphaned_daemon_jobs(temp.path(), "simulated daemon restart")?;
        ensure(recovery.open_jobs_cancelled, 1, "cancelled orphan count")?;
        ensure(recovery.scanned_rows, 1, "recovery scanned rows")?;

        let after = daemon_status_report(temp.path(), &[JobType::HealthCheck], 5)?;
        ensure(after.open_job_count, 0, "open after recovery")?;
        ensure(after.row_count, 2, "rows after recovery")?;
        let json = after.data_json();
        ensure(
            json["recentOutcomes"][0]["status"].as_str(),
            Some("cancelled"),
            "cancelled status",
        )?;
        ensure(
            json["recentOutcomes"][0]["recoveredFromOrphan"].as_bool(),
            Some(true),
            "recovered marker",
        )
    }

    #[test]
    fn daemon_status_handles_missing_table_without_mutation() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let status = daemon_status_report(temp.path(), &[JobType::DecaySweep], 5)?;
        ensure(status.row_count, 0, "row count")?;
        ensure(status.open_job_count, 0, "open job count")?;
        ensure(
            daemon_job_table_path(temp.path()).exists(),
            false,
            "status must not create table",
        )?;
        ensure(
            status.data_json()["running"].as_bool(),
            Some(false),
            "running flag",
        )
    }
}
