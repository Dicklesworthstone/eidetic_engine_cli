//! Durable daemon job journal, separated from the HTTP transport.
//!
//! The public `serve` facade routes daemon journal operations here. The
//! transport and its wire types stay unchanged. A workspace execution lease
//! outlives the returned run plan; journal locks cover repair plus append on
//! one descriptor. Status is read-only and uses a bounded, spilling index.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use fs4::FileExt as Fs4FileExt;
use serde_json::{Value, json};

use super::transport::{
    DAEMON_JOB_ROW_SCHEMA_V1, DAEMON_RECOVERY_SCHEMA_V1, DAEMON_WRITE_OWNER_IDENTITY,
    DaemonJobRow, DaemonStatusReport, daemon_job_table_path,
};
use crate::steward::{DaemonForegroundOptions, DaemonForegroundReport, JobRunResult, JobType};

pub(super) const MAX_ROW_BYTES: usize = 1024 * 1024;
const MAX_HISTORY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RECENT_ROWS: usize = 256;
const MAX_RECOVERY_DETAILS: usize = 256;
const MAX_JOBS_PER_TICK: usize = 256;
const RECOVERY_SYNC_ROWS: usize = 64;

#[path = "journal_index.rs"]
mod journal_index;
use journal_index::{LatestIndex, LatestKeys};

#[path = "detail_budget.rs"]
mod detail_budget;
use detail_budget::{MAX_DETAIL_BYTES, RecentRows, retained_bytes};

#[path = "execution.rs"]
mod execution;
pub use execution::{DaemonExecution, DaemonJobInvocation};

/// Keep this plan alive until execution and terminal recording have finished.
/// Cloning shares the lease rather than opening another lock descriptor.
#[derive(Clone, Debug)]
pub struct DaemonRunPlan {
    pub run_id: String,
    pub table_path: PathBuf,
    pub rows: Vec<DaemonJobRow>,
    _lease: Option<Arc<File>>,
}

#[derive(Clone, Debug)]
pub struct DaemonRecoveryReport {
    pub workspace: String,
    pub table_path: PathBuf,
    pub recovered_at: String,
    pub scanned_rows: usize,
    pub open_jobs_cancelled: usize,
    pub recovered_rows: Vec<DaemonJobRow>,
    pub recovered_rows_truncated: bool,
}

impl DaemonRecoveryReport {
    #[must_use]
    pub fn data_json(&self) -> Value {
        json!({
            "schema": DAEMON_RECOVERY_SCHEMA_V1,
            "workspace": self.workspace,
            "tablePath": self.table_path.display().to_string(),
            "recoveredAt": self.recovered_at,
            "scannedRows": self.scanned_rows,
            "openJobsCancelled": self.open_jobs_cancelled,
            "recoveredRows": self.recovered_rows,
            "recoveredRowsTruncated": self.recovered_rows_truncated,
        })
    }
}

pub fn record_daemon_foreground_start(
    workspace: &Path,
    options: &DaemonForegroundOptions,
) -> Result<DaemonRunPlan, String> {
    validate_options(options)?;
    let table_path = daemon_job_table_path(workspace);
    if options.dry_run {
        return Ok(DaemonRunPlan {
            run_id: "dry-run".to_owned(), table_path, rows: Vec::new(), _lease: None,
        });
    }
    check_path(workspace)?;
    if !workspace.is_dir() {
        return Err("Daemon workspace must be an existing directory".to_owned());
    }
    let requested = fs::canonicalize(&options.workspace)
        .map_err(|e| format!("Resolve daemon execution workspace: {e}"))?;
    let actual = fs::canonicalize(workspace)
        .map_err(|e| format!("Resolve daemon journal workspace: {e}"))?;
    if requested != actual {
        return Err("Daemon execution workspace does not match its journal workspace".to_owned());
    }
    let lease = Arc::new(execution_lease(workspace)?);
    let run_id = format!("daemon-run-{}", uuid::Uuid::now_v7());
    let recorded_at = Utc::now().to_rfc3339();
    let rows = options.job_types.iter().enumerate().map(|(offset, job)| {
        let runner_job_id = format!("job-{:06}", offset + 1);
        DaemonJobRow {
            schema: DAEMON_JOB_ROW_SCHEMA_V1.to_owned(),
            row_id: new_row_id(),
            run_id: run_id.clone(),
            daemon_job_key: job_key(&run_id, 1, &runner_job_id),
            runner_job_id,
            tick: 1,
            job_type: job.as_str().to_owned(),
            status: "running".to_owned(),
            outcome: None,
            workspace: workspace.to_string_lossy().into_owned(),
            write_owner_id: DAEMON_WRITE_OWNER_IDENTITY.to_owned(),
            reason: "daemon foreground tick 1 planned".to_owned(),
            started_at: Some(recorded_at.clone()),
            completed_at: None,
            recorded_at: recorded_at.clone(),
            duration_ms: None, items_processed: None, error: None,
            dry_run: false, durable_mutation: false,
            recovered_from_orphan: false, recovery_reason: None,
        }
    }).collect::<Vec<_>>();
    append_rows(&table_path, &rows)?;
    Ok(DaemonRunPlan { run_id, table_path, rows, _lease: Some(lease) })
}

fn validate_options(options: &DaemonForegroundOptions) -> Result<(), String> {
    if options.workspace.trim().is_empty() {
        return Err("Daemon workspace must not be empty".to_owned());
    }
    if options.tick_limit == 0
        || options.tick_limit > crate::steward::MAX_DAEMON_FOREGROUND_TICK_LIMIT
    {
        return Err(format!("Daemon foreground tick limit must be between 1 and {}",
            crate::steward::MAX_DAEMON_FOREGROUND_TICK_LIMIT));
    }
    if options.interval_ms > crate::steward::MAX_DAEMON_FOREGROUND_INTERVAL_MS {
        return Err(format!("Daemon foreground interval must be no greater than {} ms",
            crate::steward::MAX_DAEMON_FOREGROUND_INTERVAL_MS));
    }
    if options.job_types.is_empty() {
        return Err("Daemon foreground mode requires at least one steward job type".to_owned());
    }
    if options.job_types.len() > MAX_JOBS_PER_TICK {
        return Err(format!("Daemon foreground mode permits at most {MAX_JOBS_PER_TICK} jobs per tick"));
    }
    Ok(())
}

pub fn record_daemon_foreground_report(
    workspace: &Path,
    report: &DaemonForegroundReport,
    run_id: &str,
) -> Result<Vec<DaemonJobRow>, String> {
    if report.dry_run || run_id == "dry-run" {
        return Ok(Vec::new());
    }
    let mut rows = Vec::new();
    for tick in &report.ticks {
        for result in &tick.report.results {
            rows.push(row_from_result(workspace, run_id, tick.tick,
                &tick.started_at, &tick.completed_at, result));
        }
    }
    append_rows(&daemon_job_table_path(workspace), &rows)?;
    Ok(rows)
}

/// Compatibility API for callers explicitly requesting full history. It
/// errors at its allocation budget rather than pretending a prefix is the
/// complete journal. Status and recovery do not use this collecting API.
pub fn load_daemon_job_rows(workspace: &Path) -> Result<Vec<DaemonJobRow>, String> {
    let path = daemon_job_table_path(workspace);
    let Some(mut file) = open_existing(&path, false)? else { return Ok(Vec::new()); };
    Fs4FileExt::lock_shared(&file).map_err(|e| format!("Lock daemon journal: {e}"))?;
    let mut rows = Vec::new();
    let mut retained = 0_usize;
    scan(&mut file, &path, |row, offset, length| {
        if offset.saturating_add(length as u64) > MAX_HISTORY_BYTES {
            return Err("Daemon full-history read exceeds 16 MiB; use daemon status or recovery for a bounded complete snapshot".to_owned());
        }
        let bytes = retained_bytes(&row);
        if bytes > (MAX_HISTORY_BYTES as usize).saturating_sub(retained) {
            return Err("Daemon full-history payload exceeds 16 MiB; use daemon status or recovery for a bounded complete snapshot".to_owned());
        }
        retained += bytes;
        rows.push(row);
        Ok(())
    })?;
    Ok(rows)
}

/// Stream a complete snapshot, retaining the newest prefix of at most 256
/// rows and 4 MiB of row payloads. Counts always cover the complete history;
/// only diagnostic details are capped. The existing recentOutcomeCount reports
/// the number actually returned. The history index spills larger histories to
/// temporary files outside the workspace. No journal bytes are changed.
pub fn daemon_status_report(
    workspace: &Path,
    requested_job_types: &[JobType],
    recent_limit: usize,
) -> Result<DaemonStatusReport, String> {
    let path = daemon_job_table_path(workspace);
    let mut report = DaemonStatusReport {
        workspace: workspace.to_string_lossy().into_owned(),
        requested_job_types: requested_job_types.to_vec(),
        table_path: path.clone(), row_count: 0, open_job_count: 0,
        recent_outcomes: Vec::new(),
    };
    let Some(mut file) = open_existing(&path, false)? else { return Ok(report); };
    Fs4FileExt::lock_shared(&file).map_err(|e| format!("Lock daemon journal: {e}"))?;
    let (summary, keys) = build_index(&mut file, &path)?;
    report.row_count = summary.rows;
    let limit = recent_limit.min(MAX_RECENT_ROWS);
    let mut recent = RecentRows::new(limit, MAX_DETAIL_BYTES);
    for entry in keys {
        let (_, offset) = entry.map_err(|e| format!("Read daemon index: {e}"))?;
        let row = row_at(&mut file, &path, offset)?;
        report.open_job_count += usize::from(row.is_open());
        recent.push(row);
    }
    report.recent_outcomes = recent.into_rows();
    Ok(report)
}

pub fn recover_orphaned_daemon_jobs(
    workspace: &Path,
    reason: &str,
) -> Result<DaemonRecoveryReport, String> {
    // Read-only absence must not create the execution lock or .ee directory.
    let lease = if regular_file_exists(&daemon_job_table_path(workspace))? {
        Some(execution_lease(workspace)?)
    } else {
        None
    };
    recover_orphaned_daemon_jobs_inner(workspace, reason, lease.as_ref())
}

fn recover_orphaned_daemon_jobs_with_lease(
    workspace: &Path,
    reason: &str,
    lease: &File,
) -> Result<DaemonRecoveryReport, String> {
    recover_orphaned_daemon_jobs_inner(workspace, reason, Some(lease))
}

fn recover_orphaned_daemon_jobs_inner(
    workspace: &Path,
    reason: &str,
    lease: Option<&File>,
) -> Result<DaemonRecoveryReport, String> {
    let path = daemon_job_table_path(workspace);
    let mut report = DaemonRecoveryReport {
        workspace: workspace.to_string_lossy().into_owned(), table_path: path.clone(),
        recovered_at: Utc::now().to_rfc3339(), scanned_rows: 0,
        open_jobs_cancelled: 0, recovered_rows: Vec::new(), recovered_rows_truncated: false,
    };
    // Do not create a workspace merely to discover there is no journal.
    if !regular_file_exists(&path)? { return Ok(report); }
    // The journal may have appeared between the caller's absence check and
    // this read. Refuse rather than recovering without an execution owner.
    if lease.is_none() {
        return Err("Daemon journal appeared before recovery acquired its execution lease; retry".to_owned());
    }
    let Some(mut file) = open_existing(&path, true)? else { return Ok(report); };
    Fs4FileExt::lock(&file).map_err(|e| format!("Lock daemon journal: {e}"))?;
    // Validate every complete row before performing any recovery mutation.
    let (summary, keys) = build_index(&mut file, &path)?;
    report.scanned_rows = summary.rows;
    repair_tail(&mut file, &path)?;
    let mut detail_bytes = 0_usize;
    for entry in keys {
        let (_, offset) = entry.map_err(|e| format!("Read daemon recovery index: {e}"))?;
        let mut row = row_at(&mut file, &path, offset)?;
        if !row.is_open() { continue; }
        row.row_id = new_row_id();
        row.status = "cancelled".to_owned();
        row.outcome = Some("cancelled".to_owned());
        row.reason = "daemon restart recovery; prior execution outcome may be unknown".to_owned();
        row.completed_at = Some(report.recovered_at.clone());
        row.recorded_at = report.recovered_at.clone();
        row.duration_ms = None;
        row.items_processed = None;
        row.error = Some(reason.to_owned());
        row.durable_mutation = false;
        row.recovered_from_orphan = true;
        row.recovery_reason = Some(reason.to_owned());
        write_row(&mut file, &row)?;
        report.open_jobs_cancelled += 1;
        let bytes = retained_bytes(&row);
        if !report.recovered_rows_truncated
            && report.recovered_rows.len() < MAX_RECOVERY_DETAILS
            && bytes <= MAX_DETAIL_BYTES.saturating_sub(detail_bytes)
        {
            detail_bytes += bytes;
            report.recovered_rows.push(row);
        } else {
            // This caps only the response, never the number of cancellations.
            report.recovered_rows_truncated = true;
        }
        if report.open_jobs_cancelled % RECOVERY_SYNC_ROWS == 0 {
            file.sync_all().map_err(|e| format!("Sync daemon recovery: {e}"))?;
        }
    }
    file.sync_all().map_err(|e| format!("Sync daemon recovery: {e}"))?;
    Ok(report)
}

#[derive(Default)]
struct ScanSummary { rows: usize }

fn build_index(file: &mut File, path: &Path) -> Result<(ScanSummary, LatestKeys), String> {
    let mut index = LatestIndex::default();
    let summary = scan(file, path, |row, offset, _| {
        index.insert(row.daemon_job_key, offset).map_err(|e| format!("Index daemon journal: {e}"))
    })?;
    let keys = index.finish().map_err(|e| format!("Finish daemon journal index: {e}"))?;
    Ok((summary, keys))
}

fn scan(
    file: &mut File,
    path: &Path,
    mut visit: impl FnMut(DaemonJobRow, u64, usize) -> Result<(), String>,
) -> Result<ScanSummary, String> {
    let size = file.metadata().map_err(|e| format!("Stat daemon journal: {e}"))?.len();
    file.seek(SeekFrom::Start(0)).map_err(|e| format!("Seek daemon journal: {e}"))?;
    let mut summary = ScanSummary::default();
    {
        // This bound is the actual locked file length, not a history quota.
        let mut reader = BufReader::new((&mut *file).take(size));
        let mut offset = 0_u64;
        let mut line_number = 0_u64;
        let mut bytes = Vec::new();
        loop {
            let count = read_line(&mut reader, &mut bytes)?;
            if count == 0 { break; }
            line_number += 1;
            let start = offset;
            offset += count as u64;
            if bytes.iter().all(u8::is_ascii_whitespace) { continue; }
            match serde_json::from_slice::<DaemonJobRow>(&bytes) {
                Ok(row) => {
                    summary.rows = summary.rows.checked_add(1)
                        .ok_or_else(|| "Daemon journal row count overflow".to_owned())?;
                    visit(row, start, count)?;
                }
                Err(error) if offset == size && !bytes.ends_with(b"\n") && torn_json(&bytes, &error) => {
                    tracing::warn!(target: "ee::daemon_journal", incomplete_tail_bytes = count,
                        "Ignoring interrupted final daemon journal append; preceding records remain available");
                    break;
                }
                Err(error) => return Err(format!("Failed to parse daemon job row {line_number} in {}: {error}", path.display())),
            }
        }
    }
    if file.metadata().map_err(|e| format!("Stat daemon journal: {e}"))?.len() != size {
        return Err("Daemon journal changed during a locked snapshot; an uncoordinated writer may be active".to_owned());
    }
    Ok(summary)
}

fn read_line(reader: &mut impl BufRead, bytes: &mut Vec<u8>) -> Result<usize, String> {
    bytes.clear();
    let count = reader.take((MAX_ROW_BYTES + 1) as u64).read_until(b'\n', bytes)
        .map_err(|e| format!("Read daemon job row: {e}"))?;
    if count > MAX_ROW_BYTES {
        return Err(format!("Daemon job row exceeds the {MAX_ROW_BYTES}-byte recovery limit"));
    }
    Ok(count)
}

fn row_at(file: &mut File, path: &Path, offset: u64) -> Result<DaemonJobRow, String> {
    file.seek(SeekFrom::Start(offset)).map_err(|e| format!("Seek daemon row: {e}"))?;
    let mut reader = BufReader::new(file);
    let mut bytes = Vec::new();
    read_line(&mut reader, &mut bytes)?;
    serde_json::from_slice(&bytes).map_err(|e| format!("Read indexed daemon row in {} at {offset}: {e}", path.display()))
}

fn torn_json(bytes: &[u8], error: &serde_json::Error) -> bool {
    if error.is_eof() { return true; }
    // A signal may also interrupt a UTF-8 code point inside a JSON string.
    if let Err(utf8) = std::str::from_utf8(bytes) {
        if utf8.error_len().is_none() {
            return serde_json::from_slice::<DaemonJobRow>(&bytes[..utf8.valid_up_to()])
                .is_err_and(|prefix_error| prefix_error.is_eof());
        }
    }
    false
}

fn append_rows(path: &Path, rows: &[DaemonJobRow]) -> Result<(), String> {
    if rows.is_empty() { return Ok(()); }
    // An invalid later row must not append an earlier valid prefix or repair
    // evidence before the request fails. Check wire sizes without allocating
    // encoded rows, including JSON escape expansion and the final newline.
    for row in rows {
        serde_json::to_writer(RowSize(0), row)
            .map_err(|e| format!("Encode daemon job row: {e}"))?;
    }
    let mut file = open_append(path)?;
    Fs4FileExt::lock(&file).map_err(|e| format!("Lock daemon journal: {e}"))?;
    repair_tail(&mut file, path)?;
    for row in rows { write_row(&mut file, row)?; }
    file.sync_all().map_err(|e| format!("Sync daemon journal: {e}"))?;
    sync_parent(path)
}

struct RowSize(usize);
impl Write for RowSize {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_ROW_BYTES.saturating_sub(1).saturating_sub(self.0) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "daemon job row exceeds its byte budget"));
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

struct RowBuffer(Vec<u8>);
impl Write for RowBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_ROW_BYTES.saturating_sub(1).saturating_sub(self.0.len()) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "daemon job row exceeds its byte budget"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

fn write_row(file: &mut File, row: &DaemonJobRow) -> Result<(), String> {
    // Encode completely before touching the file, but never allocate a whole
    // transaction's worth of JSON or an unbounded escaped string.
    let mut buffer = RowBuffer(Vec::new());
    serde_json::to_writer(&mut buffer, row).map_err(|e| format!("Encode daemon job row: {e}"))?;
    buffer.0.push(b'\n');
    file.write_all(&buffer.0).map_err(|e| format!("Append daemon job row: {e}"))
}

fn repair_tail(file: &mut File, path: &Path) -> Result<(), String> {
    let size = file.metadata().map_err(|e| format!("Stat daemon journal: {e}"))?.len();
    if size == 0 { return Ok(()); }
    file.seek(SeekFrom::Start(size - 1)).map_err(|e| format!("Seek daemon tail: {e}"))?;
    let mut last = [0];
    file.read_exact(&mut last).map_err(|e| format!("Read daemon tail: {e}"))?;
    if last[0] == b'\n' { return Ok(()); }
    let start = size.saturating_sub((MAX_ROW_BYTES + 1) as u64);
    file.seek(SeekFrom::Start(start)).map_err(|e| format!("Seek daemon tail: {e}"))?;
    let mut window = vec![0; (size - start) as usize];
    file.read_exact(&mut window).map_err(|e| format!("Read daemon tail: {e}"))?;
    let boundary = window.iter().rposition(|byte| *byte == b'\n').map_or(0, |pos| pos + 1);
    if boundary == 0 && start != 0 {
        return Err("Daemon journal tail exceeds safe recovery limit".to_owned());
    }
    let tail = &window[boundary..];
    if tail.len() >= MAX_ROW_BYTES {
        return Err("Daemon journal tail exceeds safe recovery limit".to_owned());
    }
    if tail.iter().all(u8::is_ascii_whitespace)
        || serde_json::from_slice::<DaemonJobRow>(tail).is_ok()
    {
        // A complete object whose newline was interrupted is valid history.
        file.write_all(b"\n").map_err(|e| format!("Complete daemon row delimiter: {e}"))?;
        return file.sync_all().map_err(|e| format!("Sync daemon row delimiter: {e}"));
    }
    let error = serde_json::from_slice::<DaemonJobRow>(tail)
        .err().ok_or_else(|| "Daemon tail classification failed".to_owned())?;
    if !torn_json(tail, &error) {
        return Err(format!("Malformed daemon journal tail: {error}"));
    }
    preserve_tail(path, tail)?;
    file.set_len(start + boundary as u64).map_err(|e| format!("Repair interrupted daemon tail: {e}"))?;
    file.sync_all().map_err(|e| format!("Sync repaired daemon journal: {e}"))
}

fn preserve_tail(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| "Daemon journal has no parent".to_owned())?;
    let digest = blake3::hash(bytes).to_hex();
    let backup = parent.join(format!("daemon-jobs.torn-{digest}.partial"));
    check_path(&backup)?;
    let mut temporary = tempfile::Builder::new().prefix(".daemon-tail-").tempfile_in(parent)
        .map_err(|e| format!("Create daemon tail evidence: {e}"))?;
    temporary.write_all(bytes).map_err(|e| format!("Write daemon tail evidence: {e}"))?;
    temporary.as_file().sync_all().map_err(|e| format!("Sync daemon tail evidence: {e}"))?;
    match temporary.persist_noclobber(&backup) {
        Ok(_) => {},
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            let Some(mut existing) = open_existing(&backup, false)? else {
                return Err("Daemon tail evidence disappeared".to_owned());
            };
            let mut prior = Vec::new();
            (&mut existing).take((MAX_ROW_BYTES + 1) as u64).read_to_end(&mut prior)
                .map_err(|e| format!("Read daemon tail evidence: {e}"))?;
            if prior != bytes { return Err("Existing daemon tail evidence does not match; refusing to overwrite it".to_owned()); }
        }
        Err(error) => return Err(format!("Preserve daemon tail evidence: {}", error.error)),
    }
    // Evidence must be durable before removing even an uncommitted suffix.
    sync_parent(&backup)
}

fn execution_lease(workspace: &Path) -> Result<File, String> {
    let path = workspace.join(".ee").join("daemon-run.lock");
    let file = open_append(&path)?;
    Fs4FileExt::try_lock(&file).map_err(|error| match error {
        fs4::TryLockError::WouldBlock => "A daemon run owns this workspace; refusing concurrent execution or orphan recovery".to_owned(),
        fs4::TryLockError::Error(error) => format!("Acquire daemon execution lease: {error}"),
    })?;
    Ok(file)
}

fn regular_file_exists(path: &Path) -> Result<bool, String> {
    check_path(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!("Refusing daemon journal path '{}': not a regular file", path.display())),
        Err(error) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => Ok(false),
        Err(error) => Err(format!("Inspect daemon journal path '{}': {error}", path.display())),
    }
}

fn open_existing(path: &Path, append: bool) -> Result<Option<File>, String> {
    if !regular_file_exists(path)? { return Ok(None); }
    let mut options = OpenOptions::new();
    options.read(true).append(append);
    configure(&mut options);
    match options.open(path) {
        Ok(file) => { validate_handle(&file, path)?; Ok(Some(file)) },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("Open daemon journal '{}': {error}", path.display())),
    }
}

fn open_append(path: &Path) -> Result<File, String> {
    check_path(path)?;
    let parent = path.parent().ok_or_else(|| "Daemon journal path has no parent".to_owned())?;
    let parent_existed = parent.is_dir();
    fs::create_dir_all(parent).map_err(|e| format!("Create daemon journal directory: {e}"))?;
    check_path(path)?;
    if !parent_existed { sync_parent(parent)?; }
    if path.exists() && !regular_file_exists(path)? {
        return Err("Invalid daemon journal path".to_owned());
    }
    let mut options = OpenOptions::new();
    options.read(true).append(true).create(true);
    configure(&mut options);
    let file = options.open(path).map_err(|e| format!("Open daemon journal for append: {e}"))?;
    validate_handle(&file, path)?;
    Ok(file)
}

fn validate_handle(file: &File, path: &Path) -> Result<(), String> {
    if !file.metadata().map_err(|e| format!("Inspect open daemon journal: {e}"))?.is_file() {
        return Err("Open daemon journal is not a regular file".to_owned());
    }
    check_path(path)
}

fn check_path(path: &Path) -> Result<(), String> {
    if let Some(link) = crate::core::path_safety::first_existing_symlink_component(path)
        .map_err(|e| format!("Inspect daemon journal path: {e}"))?
    {
        return Err(format!("Refusing daemon journal '{}': symbolic link '{}'", path.display(), link.display()));
    }
    Ok(())
}

#[cfg(unix)]
fn configure(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    // NONBLOCK prevents a concurrently substituted FIFO from hanging open;
    // regular files ignore it and are still checked on the opened handle.
    options.mode(0o600).custom_flags(
        (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
    );
}
#[cfg(not(unix))]
fn configure(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| "Daemon journal has no parent".to_owned())?;
    let mut options = OpenOptions::new();
    options.read(true);
    configure(&mut options);
    options.open(parent).and_then(|directory| directory.sync_all())
        .map_err(|e| format!("Sync daemon journal directory: {e}"))
}
#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<(), String> { Ok(()) }

fn new_row_id() -> String { format!("daemon-row-{}", uuid::Uuid::now_v7()) }
fn job_key(run: &str, tick: u32, job: &str) -> String { format!("{run}:tick-{tick:06}:{job}") }

fn row_from_result(
    workspace: &Path, run_id: &str, tick: u32, started_at: &str, completed_at: &str,
    result: &JobRunResult,
) -> DaemonJobRow {
    let outcome = result.outcome.as_str();
    DaemonJobRow {
        schema: DAEMON_JOB_ROW_SCHEMA_V1.to_owned(), row_id: new_row_id(),
        run_id: run_id.to_owned(), daemon_job_key: job_key(run_id, tick, &result.job_id),
        runner_job_id: result.job_id.clone(), tick, job_type: result.job_type.as_str().to_owned(),
        status: outcome.to_owned(), outcome: Some(outcome.to_owned()),
        workspace: workspace.to_string_lossy().into_owned(),
        write_owner_id: DAEMON_WRITE_OWNER_IDENTITY.to_owned(),
        reason: format!("daemon foreground tick {tick} completed"),
        started_at: Some(started_at.to_owned()), completed_at: Some(completed_at.to_owned()),
        recorded_at: Utc::now().to_rfc3339(), duration_ms: Some(result.duration_ms),
        items_processed: result.items_processed, error: result.error.clone(), dry_run: result.dry_run,
        durable_mutation: result.details.as_ref().and_then(|details| details.get("durableMutation"))
            .and_then(Value::as_bool).unwrap_or(false),
        recovered_from_orphan: false, recovery_reason: None,
    }
}

#[cfg(test)]
#[path = "job_journal_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "job_journal_budget_tests.rs"]
mod budget_tests;
