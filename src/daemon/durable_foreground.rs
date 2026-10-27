//! Execute real steward handlers with a durable boundary around each job.
//!
//! Unlike collecting the entire foreground run and journaling it afterwards,
//! this entry point holds one execution lease across recovery, handlers and
//! cancellable waits. It retains one runner result, not every tick's payload.
//! Call this instead of the legacy start/run/report triplet: that triplet owns
//! a separate lease and must not be mixed with this self-contained executor.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use asupersync::runtime::yield_now::yield_now;
use asupersync::time::sleep;
use asupersync::{CancelReason, Cx, Outcome};
use chrono::Utc;

use super::{DaemonExecution, DaemonJobInvocation};
use crate::steward::{
    DaemonForegroundOptions, JobPriority, JobRunResult, ManualRunner, RunOutcome, RunnerReport,
    try_acquire_maintenance_job_lock,
};

const SLEEP_SLICE_MS: u64 = 250;

/// Fixed-size counters plus run identity; full job evidence is in the journal.
/// A failed/cancelled handler can stop admission before all requested jobs run.
/// Dry runs are plans only: they acquire no lease and invoke no handlers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonExecutionReport {
    pub run_id: String,
    pub workspace: PathBuf,
    pub started_at: String,
    pub completed_at: String,
    pub dry_run: bool,
    pub requested_ticks: u32,
    pub requested_jobs: u64,
    pub completed_ticks: u32,
    pub completed_jobs: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub cancelled: u64,
    pub skipped: u64,
    pub timed_out: u64,
    pub was_cancelled: bool,
    pub stopped_early: bool,
    pub recovered_orphans: usize,
}

impl DaemonExecutionReport {
    fn planned(options: &DaemonForegroundOptions) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            run_id: "dry-run".to_owned(),
            workspace: PathBuf::from(&options.workspace),
            started_at: now.clone(),
            completed_at: now,
            dry_run: options.dry_run,
            requested_ticks: options.tick_limit,
            requested_jobs: u64::from(options.tick_limit) * options.job_types.len() as u64,
            completed_ticks: 0,
            completed_jobs: 0,
            succeeded: 0,
            failed: 0,
            cancelled: 0,
            skipped: 0,
            timed_out: 0,
            was_cancelled: false,
            stopped_early: false,
            recovered_orphans: 0,
        }
    }

    fn record(&mut self, result: &JobRunResult, execution: &DaemonExecution) {
        match result.outcome {
            RunOutcome::Success => self.succeeded += 1,
            RunOutcome::Failed => self.failed += 1,
            RunOutcome::Cancelled => self.cancelled += 1,
            RunOutcome::Skipped => self.skipped += 1,
            RunOutcome::TimedOut => self.timed_out += 1,
        }
        self.completed_jobs = execution.completed_jobs();
        self.completed_ticks = execution.completed_ticks();
    }
}

/// Run the durable executor on the production Asupersync runtime.
///
/// Failed job outcomes remain explicit in the returned counters. Cancellation
/// of the supervisor and storage failures return errors; already synced job
/// evidence remains available through daemon status and recovery.
pub fn run_daemon_foreground_durable(
    options: &DaemonForegroundOptions,
) -> Result<DaemonExecutionReport, String> {
    validate_request(options)?;
    if external_cancellation_requested(options) {
        return Err("Daemon supervisor cancelled before execution".to_owned());
    }
    if options.dry_run {
        return Ok(DaemonExecutionReport::planned(options));
    }
    let runtime = crate::core::build_cli_runtime()
        .map_err(|error| format!("Failed to build Asupersync daemon runtime: {error}"))?;
    let task_options = options.clone();
    let join = runtime
        .handle()
        .try_spawn(async move {
            let Some(cx) = Cx::current() else {
                return Outcome::Err(
                    "Asupersync daemon task started without an ambient Cx".to_owned(),
                );
            };
            run_daemon_foreground_durable_supervised(&cx, &task_options).await
        })
        .map_err(|error| format!("Failed to spawn Asupersync daemon supervisor: {error}"))?;
    match runtime.block_on(join) {
        Outcome::Ok(report) => Ok(report),
        Outcome::Err(message) => Err(message),
        Outcome::Cancelled(reason) => Err(format!(
            "Daemon supervisor cancelled: {}",
            crate::core::outcome::cancel_message(&reason),
        )),
        Outcome::Panicked(payload) => Err(format!("Daemon supervisor panicked: {payload}")),
    }
}

/// Execute the real ManualRunner handlers under an explicit supervisor Cx.
///
/// A failed job stops the run unless continue_on_error is enabled. A cancelled
/// runner always stops it. No unstarted remainder is recorded as having run.
/// Completion is synced BEFORE the next cancellation checkpoint, so a handler
/// that returns a result just as cancellation arrives does not lose that result.
pub async fn run_daemon_foreground_durable_supervised(
    cx: &Cx,
    options: &DaemonForegroundOptions,
) -> Outcome<DaemonExecutionReport, String> {
    if let Err(error) = validate_request(options) {
        return Outcome::Err(error);
    }
    if let Some(reason) = cancellation_reason(cx, options) {
        return Outcome::Cancelled(reason);
    }
    let mut summary = DaemonExecutionReport::planned(options);
    if options.dry_run {
        return Outcome::Ok(summary);
    }
    let mut execution = match DaemonExecution::begin(options) {
        Ok(execution) => execution,
        Err(error) => return Outcome::Err(error),
    };
    summary.run_id = execution.run_id().to_owned();
    summary.workspace = execution.workspace().to_path_buf();
    summary.recovered_orphans = execution.recovered_orphans();

    loop {
        if let Some(reason) = cancellation_reason(cx, options) {
            return stop_with(
                &mut execution,
                "supervisor cancellation",
                Outcome::Cancelled(reason),
            );
        }
        // Share the existing manual/daemon maintenance exclusion contract.
        // Hold this only through the handler and terminal sync, never while
        // waiting for the next tick or yielding to another runtime task.
        let maintenance =
            match try_acquire_maintenance_job_lock(execution.workspace(), execution.run_id()) {
                Ok(guard) => guard,
                Err(error) => {
                    return Outcome::Err(format!(
                        "Cannot acquire daemon maintenance ownership: {}",
                        error.message(),
                    ));
                }
            };
        let invocation = match execution.begin_next_job() {
            Ok(Some(invocation)) => invocation,
            Ok(None) => break,
            Err(error) => return Outcome::Err(error),
        };
        let mut runner_options = options.runner_options.clone().with_cancellation_cx(cx);
        runner_options.dry_run = false;
        runner_options.workspace_path = Some(execution.workspace().to_path_buf());
        let mut runner = ManualRunner::new(runner_options);
        let runner_id = runner.schedule(
            invocation.job_type,
            JobPriority::Normal,
            Some(format!("daemon foreground tick {}", invocation.tick)),
        );
        // This is the production handler dispatch, not a synthetic result or
        // second runtime. Its own budget, QoS, store and cancellation guards run.
        let report = runner.run_pending();
        let runner_cancelled = report.was_cancelled;
        let result = match take_result(report, &runner_id, &invocation) {
            Ok(result) => result,
            Err(error) => {
                if let Some(reason) = cancellation_reason(cx, options) {
                    return stop_with(
                        &mut execution,
                        "handler interrupted",
                        Outcome::Cancelled(reason),
                    );
                }
                return stop_with(&mut execution, &error, Outcome::Err(error.clone()));
            }
        };
        // Do not checkpoint between the handler's returned result and its
        // terminal journal append. If this append fails, no later job runs.
        if let Err(error) = execution.finish_job(&invocation, &result) {
            return Outcome::Err(error);
        }
        drop(runner);
        drop(maintenance);
        summary.record(&result, &execution);
        summary.was_cancelled |= runner_cancelled || result.outcome == RunOutcome::Cancelled;
        let stop_after_result = summary.was_cancelled
            || (result.outcome == RunOutcome::Failed && !options.runner_options.continue_on_error);
        if stop_after_result {
            summary.stopped_early = summary.completed_jobs < summary.requested_jobs;
            if let Err(error) = execution.stop("runner requested stop after durable completion") {
                return Outcome::Err(error);
            }
            if let Some(reason) = cancellation_reason(cx, options) {
                return Outcome::Cancelled(reason);
            }
            break;
        }
        if summary.completed_jobs == summary.requested_jobs {
            break;
        }
        let last_job_in_tick = summary
            .completed_jobs
            .is_multiple_of(options.job_types.len() as u64);
        if last_job_in_tick {
            if let Some(reason) = wait_interval(cx, options).await {
                return stop_with(
                    &mut execution,
                    "cancelled between daemon ticks",
                    Outcome::Cancelled(reason),
                );
            }
        } else {
            // Also permit fair scheduling and cancellation between jobs when
            // an operator requests a zero-delay, multi-job run.
            yield_now().await;
        }
    }
    summary.completed_at = Utc::now().to_rfc3339();
    Outcome::Ok(summary)
}

fn validate_request(options: &DaemonForegroundOptions) -> Result<(), String> {
    DaemonExecution::validate_request(options)?;
    if let Some(handler_workspace) = &options.runner_options.workspace_path {
        let journal_workspace = fs::canonicalize(Path::new(&options.workspace))
            .map_err(|error| format!("Resolve daemon journal workspace: {error}"))?;
        let handler_workspace = fs::canonicalize(handler_workspace)
            .map_err(|error| format!("Resolve daemon handler workspace: {error}"))?;
        if journal_workspace != handler_workspace {
            return Err("Daemon handler workspace does not match its journal workspace".to_owned());
        }
    }
    Ok(())
}

fn external_cancellation_requested(options: &DaemonForegroundOptions) -> bool {
    options
        .runner_options
        .cancellation_flag
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::Acquire))
        || options
            .runner_options
            .cancellation_cx
            .as_ref()
            .is_some_and(Cx::is_cancel_requested)
}

fn cancellation_reason(cx: &Cx, options: &DaemonForegroundOptions) -> Option<CancelReason> {
    if cx.checkpoint().is_err() {
        Some(
            cx.cancel_reason()
                .unwrap_or_else(CancelReason::parent_cancelled),
        )
    } else if external_cancellation_requested(options) {
        Some(CancelReason::parent_cancelled())
    } else {
        None
    }
}

async fn wait_interval(cx: &Cx, options: &DaemonForegroundOptions) -> Option<CancelReason> {
    let mut remaining = options.interval_ms;
    if remaining == 0 {
        yield_now().await;
    }
    while remaining > 0 {
        if let Some(reason) = cancellation_reason(cx, options) {
            return Some(reason);
        }
        let slice = remaining.min(SLEEP_SLICE_MS);
        sleep(cx.now(), Duration::from_millis(slice)).await;
        remaining -= slice;
    }
    cancellation_reason(cx, options)
}

fn take_result(
    mut report: RunnerReport,
    runner_id: &str,
    invocation: &DaemonJobInvocation,
) -> Result<JobRunResult, String> {
    if report.results.len() != 1 {
        return Err(
            "Single-job daemon handler did not return exactly one terminal result".to_owned(),
        );
    }
    let mut result = report
        .results
        .pop()
        .ok_or_else(|| "Daemon handler result disappeared".to_owned())?;
    if result.job_id != runner_id || result.job_type != invocation.job_type || result.dry_run {
        return Err("Daemon handler returned a result for a different invocation".to_owned());
    }
    // ManualRunner owns a fresh local ledger per handler; the cursor owns the
    // durable run/tick/job identity. Remap only after checking the local result.
    result.job_id.clone_from(&invocation.runner_job_id);
    Ok(result)
}

fn stop_with<T>(
    execution: &mut DaemonExecution,
    reason: &str,
    outcome: Outcome<T, String>,
) -> Outcome<T, String> {
    match execution.stop(reason) {
        Ok(()) => outcome,
        Err(error) => Outcome::Err(format!(
            "Daemon interrupted ({reason}); terminal journal recording failed: {error}",
        )),
    }
}

#[cfg(test)]
#[path = "durable_foreground_tests.rs"]
mod tests;
