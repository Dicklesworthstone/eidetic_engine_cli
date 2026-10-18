//! Background maintenance dispatched through the durable execution owner.
//!
//! The RPC server calls the public steward scheduler. Every tick here uses the
//! same lease, orphan recovery and per-handler terminal sync as an explicit
//! durable run. No separate start/run/report triplet or outer maintenance lock
//! may surround it: the durable executor owns those boundaries itself.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::db::DbConnection;
use crate::serve::{DaemonExecutionReport, run_daemon_foreground_durable};
use crate::steward::{
    BACKGROUND_SCHEDULER_INTERVAL_MS, DaemonForegroundOptions, JobType, RunnerOptions,
};

const SLEEP_SLICE: Duration = Duration::from_millis(500);
const BACKGROUND_JOBS: &[JobType] = &[
    JobType::DecaySweep,
    JobType::HealthCheck,
    JobType::TeamSteward,
];

/// Run sleep-first maintenance for the RPC daemon's bound workspace.
///
/// Transient tick failures do not terminate the RPC server. They are reported,
/// and later ticks retry without overwriting earlier journal evidence. The
/// executor releases its ownership before the next sleep. The daemon handle's
/// existing shutdown flag and thread unpark still interrupt an idle scheduler.
pub fn run_daemon_background_scheduler(workspace: &str, shutdown: Arc<AtomicBool>) {
    run_scheduler(
        workspace,
        shutdown,
        Duration::from_millis(BACKGROUND_SCHEDULER_INTERVAL_MS),
        run_daemon_foreground_durable,
    );
}

fn run_scheduler(
    workspace: &str,
    shutdown: Arc<AtomicBool>,
    interval: Duration,
    mut execute: impl FnMut(&DaemonForegroundOptions) -> Result<DaemonExecutionReport, String>,
) {
    // Resolve only at the first admitted tick. A short-lived daemon should not
    // even probe the store. Once bound, never resolve a relative path again
    // against a potentially changed process working directory.
    let mut pinned_workspace: Option<PathBuf> = None;
    loop {
        let Some(deadline) = Instant::now().checked_add(interval) else {
            tracing::error!(target: "ee::steward", "Background scheduler interval overflow");
            return;
        };
        if !wait_until(&shutdown, deadline) {
            return;
        }
        let workspace = match &pinned_workspace {
            Some(path) => path.clone(),
            None => match canonical_workspace(Path::new(workspace)) {
                Ok(path) => {
                    pinned_workspace = Some(path.clone());
                    path
                }
                Err(error) => {
                    tracing::warn!(target: "ee::steward", %error,
                        "Background workspace unavailable; retrying at the next tick");
                    continue;
                }
            },
        };
        let options = match tick_options(&workspace, Arc::clone(&shutdown)) {
            Ok(options) => options,
            Err(error) => {
                tracing::warn!(target: "ee::steward", %error,
                    "Background tick planning failed; no handler was admitted");
                continue;
            }
        };
        // Queue inspection can be slow. Recheck before constructing a runtime
        // or recovering any old job. The executor also checks this same flag.
        if shutdown.load(Ordering::Acquire) {
            return;
        }
        match execute(&options) {
            Ok(report) => {
                tracing::debug!(target: "ee::steward",
                    run_id = %report.run_id,
                    completed_jobs = report.completed_jobs,
                    failed = report.failed,
                    cancelled = report.cancelled,
                    timed_out = report.timed_out,
                    recovered_orphans = report.recovered_orphans,
                    "Background tick finished with durable job outcomes");
            }
            Err(error) => {
                if !shutdown.load(Ordering::Acquire) {
                    tracing::warn!(target: "ee::steward", %error,
                        "Background tick stopped; durable journal retains completed work");
                }
            }
        }
    }
}

fn canonical_workspace(workspace: &Path) -> Result<PathBuf, String> {
    let path = workspace.canonicalize()
        .map_err(|error| format!("Resolve background workspace: {error}"))?;
    if !path.is_dir() {
        return Err("Background workspace is not a directory".to_owned());
    }
    if path.to_str().is_none() {
        return Err("Background workspace cannot be represented without losing path bytes".to_owned());
    }
    Ok(path)
}

fn tick_options(
    workspace: &Path,
    shutdown: Arc<AtomicBool>,
) -> Result<DaemonForegroundOptions, String> {
    let workspace_text = workspace.to_str()
        .ok_or_else(|| "Background workspace is not UTF-8".to_owned())?;
    let mut jobs = BACKGROUND_JOBS.to_vec();
    // Preserve rebuild-on-fallback and deferred queue processing. Do not
    // rebuild every minute or schedule two publications in the same tick.
    match select_index_job(
        crate::core::index::pending_index_rebuild_request(workspace).is_some(),
        || index_jobs_need_processing(workspace),
    ) {
        Ok(Some(job)) => jobs.push(job),
        Ok(None) => {}
        Err(error) => tracing::warn!(target: "ee::steward", %error,
            "Background index queue inspection failed; retrying on the next tick"),
    }
    Ok(DaemonForegroundOptions {
        workspace: workspace_text.to_owned(),
        tick_limit: 1,
        interval_ms: 0,
        dry_run: false,
        job_types: jobs,
        runner_options: RunnerOptions {
            // Independent maintenance failures must not starve pending index
            // work. Cancellation and persistence failures still stop admission.
            continue_on_error: true,
            workspace_path: Some(workspace.to_path_buf()),
            ..RunnerOptions::new().with_cancellation_flag(shutdown)
        },
    })
}

fn select_index_job(
    rebuild_requested: bool,
    inspect_queue: impl FnOnce() -> Result<bool, String>,
) -> Result<Option<JobType>, String> {
    if rebuild_requested {
        return Ok(Some(JobType::IndexRebuild));
    }
    inspect_queue().map(|pending| pending.then_some(JobType::IndexCoalesce))
}

fn index_jobs_need_processing(workspace: &Path) -> Result<bool, String> {
    let database_path = workspace.join(".ee").join("ee.db");
    match std::fs::symlink_metadata(&database_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("Inspect background index store: {error}")),
        Ok(_) => {}
    }
    let connection = DbConnection::open_file_read_only(&database_path)
        .map_err(|error| format!("Open background index store: {error}"))?;
    let workspace = crate::core::workspace::addressed_workspace_row(
        &connection, workspace, &database_path,
    ).map_err(|error| error.message().to_owned())?;
    let Some(workspace) = workspace else { return Ok(false); };
    connection.query(
        "SELECT id FROM search_index_jobs WHERE workspace_id = ?1 AND status IN ('pending', 'running', 'failed', 'cancelled') LIMIT 1",
        &[sqlmodel_core::Value::Text(workspace.id)],
    ).map(|jobs| !jobs.is_empty())
        .map_err(|error| format!("Inspect pending background index jobs: {error}"))
}

fn wait_until(shutdown: &AtomicBool, deadline: Instant) -> bool {
    loop {
        if shutdown.load(Ordering::Acquire) {
            return false;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return true;
        }
        // An unpark token sent before this call is retained. Spurious wakeups
        // recheck the deadline; flag-only callers have a bounded fallback.
        std::thread::park_timeout(remaining.min(SLEEP_SLICE));
    }
}

#[cfg(test)]
#[path = "durable_scheduler_tests.rs"]
mod tests;
