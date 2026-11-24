//! Explicit, bounded CASS refreshes for the maintenance runner.
//!
//! Importing external session history is opt-in: this job is not part of the
//! unconditional background scheduler. The existing importer owns source
//! verification, per-session transactions, resumable import ledgers, and the
//! durable index queue. Publication remains a separate index_coalesce job.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use asupersync::{CancelKind, CancelReason, Cx};
use serde_json::{Value as JsonValue, json};

use super::{
    BudgetExceedAction, JobBudgetState, JobWorkResult, ManualRunner, ResourceType, RunOutcome,
    RunnerOptions, millis_to_u64, steward_job_failure,
};
use crate::cass::{
    CassClient, CassImportError, CassImportOptions, CassImportReport, discover_import_binary,
    import_cass_sessions_with_cx,
};
use crate::db::DbConnection;

const SCHEMA: &str = "ee.steward.cass_import.v1";
const DEFAULT_TIME_LIMIT_MS: u64 = 120_000;
const DEFAULT_ITEM_LIMIT: u64 = 10;
const REPAIR: &str = "ee job run cass_import --workspace . --json";

struct ImportStore {
    workspace_path: PathBuf,
    database_path: PathBuf,
    workspace_id: String,
}

/// Check an explicitly addressed CASS refresh target without creating a store.
///
/// Command and durable-daemon admission call this before acquiring their
/// maintenance lock or writing their job journal. The handler repeats the
/// check, so library callers receive the same workspace-binding protection.
///
/// # Errors
///
/// Refuses a missing workspace/store, a store needing migration, and a store
/// without a workspace row bound to the addressed root.
pub fn validate_cass_import_store(
    workspace_path: &Path,
    database_path: Option<&Path>,
) -> Result<(), String> {
    admit_store(workspace_path, database_path).map(|_| ())
}

fn admit_store(workspace_path: &Path, database_path: Option<&Path>) -> Result<ImportStore, String> {
    let workspace_path = workspace_path
        .canonicalize()
        .map_err(|error| format!("CASS import workspace is unavailable: {error}"))?;
    if !workspace_path.is_dir() {
        return Err("CASS import workspace must be an existing directory.".to_owned());
    }
    let database_path = database_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    crate::core::ensure_addressed_database_exists(&database_path)
        .map_err(|error| format!("CASS import store is unavailable: {}", error.message()))?;
    let connection = DbConnection::open_file_read_only(&database_path)
        .map_err(|error| format!("CASS import store is unavailable: {error}"))?;
    if connection
        .needs_migration()
        .map_err(|error| format!("CASS import schema inspection failed: {error}"))?
    {
        return Err(
            "CASS import store needs migration; run ee migrate run --workspace . --json."
                .to_owned(),
        );
    }
    let workspace = crate::core::workspace::addressed_workspace_row(
        &connection,
        &workspace_path,
        &database_path,
    )
    .map_err(|error| format!("CASS import workspace binding failed: {}", error.message()))?
    .ok_or_else(|| {
        "CASS import requires an existing workspace row bound to the addressed root.".to_owned()
    })?;
    Ok(ImportStore {
        workspace_path,
        database_path,
        workspace_id: workspace.id,
    })
}

fn configured_client(workspace_path: &Path) -> Result<CassClient, String> {
    let config = crate::core::config_surface::merged_workspace_config(workspace_path)
        .map_err(|error| format!("CASS import configuration is unavailable: {error}"))?;
    // This resolver deliberately ignores executable overrides from workspace
    // configuration. Only the established user/environment authority may
    // choose a binary; do not read config.values.cass.binary here.
    let override_path = crate::core::config_surface::cass_import_binary_override(workspace_path);
    let discovered = discover_import_binary(override_path.as_deref())
        .map_err(|error| format!("CASS import executable is unavailable: {error}"))?;
    let mut client = CassClient::from_discovered(discovered);
    if let Some(seconds) = config.values.cass.subprocess_timeout_secs {
        client = client.with_timeout(Duration::from_secs(seconds.max(1)));
    }
    Ok(client)
}

impl ManualRunner {
    pub(super) fn execute_cass_import(&self, budget: &mut JobBudgetState) -> JobWorkResult {
        self.execute_cass_import_with_client(budget, configured_client)
    }

    fn execute_cass_import_with_client<F>(
        &self,
        budget: &mut JobBudgetState,
        client_factory: F,
    ) -> JobWorkResult
    where
        F: FnOnce(&Path) -> Result<CassClient, String>,
    {
        let started = Instant::now();
        let result = self.execute_cass_import_inner(budget, started, client_factory);
        budget.record(ResourceType::TimeMs, millis_to_u64(started.elapsed()));
        if let Some(items) = result.1 {
            budget.record(ResourceType::Items, items);
        }
        result
    }

    fn execute_cass_import_inner<F>(
        &self,
        budget: &JobBudgetState,
        started: Instant,
        client_factory: F,
    ) -> JobWorkResult
    where
        F: FnOnce(&Path) -> Result<CassClient, String>,
    {
        if let Some(reason) = parent_cancellation(&self.options) {
            return admission_cancelled(self.options.dry_run, Some(reason), false);
        }
        if shutdown_requested(&self.options) {
            return admission_cancelled(self.options.dry_run, None, false);
        }
        let item_limit = hard_budget_remaining(budget, ResourceType::Items, DEFAULT_ITEM_LIMIT);
        if item_limit == 0 {
            return admission_cancelled(self.options.dry_run, None, false);
        }
        let limit = match u32::try_from(item_limit) {
            Ok(limit) => limit,
            Err(_) => {
                return failure(
                    "cass_import_item_limit_invalid",
                    "CASS import item limit must fit a positive 32-bit session count.".to_owned(),
                    self.options.dry_run,
                    None,
                );
            }
        };
        let time_limit = Duration::from_millis(hard_budget_remaining(
            budget,
            ResourceType::TimeMs,
            DEFAULT_TIME_LIMIT_MS,
        ));
        if time_limit.is_zero() {
            return admission_cancelled(self.options.dry_run, None, true);
        }
        let Some(workspace_path) = self.options.workspace_path.as_deref() else {
            return failure(
                "cass_import_workspace_required",
                "CASS import requires a workspace path; pass --workspace <path>.".to_owned(),
                self.options.dry_run,
                None,
            );
        };
        let store = match admit_store(workspace_path, self.options.database_path.as_deref()) {
            Ok(store) => store,
            Err(message) => {
                return failure(
                    "cass_import_store_unavailable",
                    message,
                    self.options.dry_run,
                    self.options.database_path.as_deref(),
                );
            }
        };
        if self
            .options
            .workspace_id
            .as_ref()
            .is_some_and(|workspace_id| workspace_id != &store.workspace_id)
        {
            return failure(
                "cass_import_workspace_mismatch",
                "CASS import workspace ID does not match the addressed store.".to_owned(),
                self.options.dry_run,
                Some(&store.database_path),
            );
        }
        let timeout = time_limit.saturating_sub(started.elapsed());
        let timeout = self
            .options
            .cancellation_cx
            .as_ref()
            .and_then(|parent| parent.remaining_budget().deadline)
            .map_or(timeout, |remaining| timeout.min(remaining));
        if let Some(reason) = parent_cancellation(&self.options) {
            return admission_cancelled(self.options.dry_run, Some(reason), false);
        }
        if timeout.is_zero() {
            return admission_cancelled(self.options.dry_run, None, true);
        }
        let options = CassImportOptions {
            workspace_path: store.workspace_path.clone(),
            database_path: Some(store.database_path.clone()),
            limit,
            since: None,
            dry_run: self.options.dry_run,
            include_spans: true,
        };
        let ledger_id = options.ledger_id();
        let run = crate::core::run_cli_with_cx(timeout, |cx| async move {
            if let Some(reason) = synchronize_cancellation(&cx, &self.options) {
                return admission_cancelled(options.dry_run, Some(reason), false);
            }
            let before = match observe_progress(&store, &ledger_id) {
                Ok(progress) => progress,
                Err(message) => {
                    if let Some(reason) = synchronize_cancellation(&cx, &self.options) {
                        return admission_cancelled(options.dry_run, Some(reason), false);
                    }
                    return failure(
                        "cass_import_progress_unavailable",
                        message,
                        options.dry_run,
                        Some(&store.database_path),
                    );
                }
            };
            let client = match client_factory(&store.workspace_path) {
                Ok(client) => client,
                Err(message) => {
                    if let Some(reason) = synchronize_cancellation(&cx, &self.options) {
                        return admission_cancelled(options.dry_run, Some(reason), false);
                    }
                    return failure(
                        "cass_import_executable_unavailable",
                        message,
                        options.dry_run,
                        Some(&store.database_path),
                    );
                }
            };
            // The importer is synchronous, but its subprocess supervisor and
            // transaction checkpoints consume this runtime-owned context.
            // Relay the caller's live cancellation even while it is blocked
            // reading a CASS child; never detach the importer itself.
            std::thread::scope(|scope| {
                let (stop, stopped) = mpsc::channel::<()>();
                let relay_cx = cx.clone();
                let runner_options = &self.options;
                let relay = std::thread::Builder::new()
                    .name("ee-cass-import-cancel".to_owned())
                    .spawn_scoped(scope, move || {
                        loop {
                            if synchronize_cancellation(&relay_cx, runner_options).is_some() {
                                return;
                            }
                            match stopped.recv_timeout(Duration::from_millis(10)) {
                                Err(RecvTimeoutError::Timeout) => {}
                                Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                            }
                        }
                    });
                let relay = match relay {
                    Ok(relay) => relay,
                    Err(error) => {
                        return failure(
                            "cass_import_cancellation_monitor_failed",
                            format!("CASS import cancellation monitor could not start: {error}"),
                            options.dry_run,
                            Some(&store.database_path),
                        );
                    }
                };
                let mut invoked = false;
                let result = match synchronize_cancellation(&cx, &self.options) {
                    Some(reason) => Err(CassImportError::Cancelled(reason)),
                    None => {
                        invoked = true;
                        import_cass_sessions_with_cx(&cx, &client, &options)
                    }
                };
                // Disconnect also stops the relay during unwinding; scope
                // joins it before the request can return in every case.
                drop(stop);
                let monitor_failed = relay.join().is_err();
                let cancelled = synchronize_cancellation(&cx, &self.options).or_else(|| {
                    result
                        .as_ref()
                        .err()
                        .and_then(CassImportError::cancellation_reason)
                        .cloned()
                });
                // Observe retained ledger progress even after cancellation.
                // Only this bounded read is masked; imports remain cancellable.
                let after = cx.masked(|| {
                    let _ambient = Cx::set_current(Some(cx.clone()));
                    observe_progress(&store, &ledger_id)
                });
                finish_import(
                    &options,
                    &ledger_id,
                    before,
                    after,
                    invoked,
                    result,
                    cancelled,
                    monitor_failed,
                )
            })
        });
        match run {
            Ok(result) => result,
            Err(error) => failure(
                "cass_import_runtime_unavailable",
                format!("CASS import runtime could not start: {error}"),
                self.options.dry_run,
                self.options.database_path.as_deref(),
            ),
        }
    }
}

fn hard_budget_remaining(
    budget: &JobBudgetState,
    resource: ResourceType,
    default_limit: u64,
) -> u64 {
    let consumed = budget
        .consumption
        .get(&resource)
        .map_or(0, |consumption| consumption.consumed);
    budget
        .budgets
        .iter()
        .filter(|limit| limit.resource == resource && limit.on_exceed == BudgetExceedAction::Cancel)
        .map(|limit| limit.limit.saturating_sub(consumed))
        .min()
        .unwrap_or_else(|| default_limit.saturating_sub(consumed))
}

fn shutdown_requested(options: &RunnerOptions) -> bool {
    options
        .cancellation_flag
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::SeqCst))
}

fn parent_cancellation(options: &RunnerOptions) -> Option<CancelReason> {
    let parent = options.cancellation_cx.as_ref()?;
    if parent.checkpoint().is_ok() && !parent.is_cancel_requested() {
        return None;
    }
    Some(parent.cancel_reason().unwrap_or_else(|| {
        crate::core::outcome::attributed_cancel_reason(
            parent,
            CancelKind::ParentCancelled,
            "CASS import parent cancelled without a recorded reason",
        )
    }))
}

fn synchronize_cancellation(cx: &Cx, options: &RunnerOptions) -> Option<CancelReason> {
    if !cx.is_cancel_requested() {
        if let Some(reason) = parent_cancellation(options) {
            cx.set_cancel_reason(reason);
        } else if shutdown_requested(options) {
            cx.set_cancel_reason(crate::core::outcome::attributed_cancel_reason(
                cx,
                CancelKind::Shutdown,
                "CASS import maintenance shutdown requested",
            ));
        }
    }
    if cx.checkpoint().is_ok() && !cx.is_cancel_requested() {
        return None;
    }
    Some(cx.cancel_reason().unwrap_or_else(|| {
        crate::core::outcome::attributed_cancel_reason(
            cx,
            CancelKind::ParentCancelled,
            "CASS import cancelled without a recorded reason",
        )
    }))
}

fn is_timeout(reason: &CancelReason) -> bool {
    reason
        .chain()
        .any(|cause| matches!(cause.kind, CancelKind::Timeout | CancelKind::Deadline))
}

fn admission_cancelled(
    dry_run: bool,
    reason: Option<CancelReason>,
    timed_out: bool,
) -> JobWorkResult {
    let timed_out = timed_out || reason.as_ref().is_some_and(is_timeout);
    let message = if timed_out {
        "CASS import time budget expired before source work."
    } else {
        "CASS import cancelled before source work."
    };
    (
        if timed_out {
            RunOutcome::TimedOut
        } else {
            RunOutcome::Cancelled
        },
        Some(0),
        Some(message.to_owned()),
        Some(json!({
            "schema": SCHEMA,
            "jobType": "cass_import",
            "code": if timed_out { "cass_import_timed_out" } else { "cass_import_cancelled" },
            "dryRun": dry_run,
            "durableMutation": false,
            "mutationStatus": "unchanged",
            "mutationMayHaveOccurred": false,
            "indexPublished": false,
            "cancelReason": reason,
        })),
    )
}

fn failure(code: &str, message: String, dry_run: bool, database: Option<&Path>) -> JobWorkResult {
    steward_job_failure(SCHEMA, code, message, dry_run, database, REPAIR)
}

fn index_repair(options: &CassImportOptions) -> String {
    // Always quote complete arguments. Workspace/database paths can contain
    // whitespace, quotes, or shell syntax and must retain their exact target.
    let quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    let workspace = quote(&options.workspace_path);
    let database = options.database_path.as_deref().map(quote);
    match database {
        Some(database) => format!(
            "ee job run index_coalesce --workspace {workspace} --database {database} --json"
        ),
        None => format!("ee job run index_coalesce --workspace {workspace} --json"),
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ImportProgress {
    ledger_exists: bool,
    attempts: u64,
    imported_sessions: u64,
    imported_spans: u64,
}

impl ImportProgress {
    fn data_json(self) -> JsonValue {
        json!({
            "ledgerExists": self.ledger_exists,
            "attempts": self.attempts,
            "cumulativeImportedSessions": self.imported_sessions,
            "cumulativeImportedSpans": self.imported_spans,
        })
    }
}

fn observe_progress(store: &ImportStore, ledger_id: &str) -> Result<ImportProgress, String> {
    let connection = DbConnection::open_file_read_only(&store.database_path)
        .map_err(|error| format!("CASS import progress store is unavailable: {error}"))?;
    let rows = connection
        .query(
            "SELECT attempt_count, imported_session_count, imported_span_count FROM import_ledger WHERE workspace_id = ?1 AND id = ?2 AND source_kind = 'cass'",
            &[
                sqlmodel_core::Value::Text(store.workspace_id.clone()),
                sqlmodel_core::Value::Text(ledger_id.to_owned()),
            ],
        )
        .map_err(|error| format!("CASS import progress read failed: {error}"))?;
    let Some(row) = rows.first() else {
        return Ok(ImportProgress::default());
    };
    let count = |index: usize| {
        row.get(index)
            .and_then(|value| value.as_i64())
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| "CASS import ledger contains an invalid progress counter.".to_owned())
    };
    Ok(ImportProgress {
        ledger_exists: true,
        attempts: count(0)?,
        imported_sessions: count(1)?,
        imported_spans: count(2)?,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "one terminal result keeps import and observation facts together"
)]
fn finish_import(
    options: &CassImportOptions,
    ledger_id: &str,
    before: ImportProgress,
    after: Result<ImportProgress, String>,
    invoked: bool,
    result: Result<CassImportReport, CassImportError>,
    cancelled: Option<CancelReason>,
    monitor_failed: bool,
) -> JobWorkResult {
    let report = result.as_ref().ok();
    // Successful non-preview imports have a durable ledger even for an empty
    // selection. For failures, the ledger's attempt counter is written before
    // source transactions, so a readable unchanged ledger proves no import
    // mutation. An unreadable post-admission outcome remains explicitly unknown.
    let durable_mutation = if options.dry_run || !invoked {
        Some(false)
    } else if report.is_some_and(|report| report.ledger_id.is_some()) {
        Some(true)
    } else {
        after.as_ref().ok().map(|progress| *progress != before)
    };
    let items = report
        .map(|report| u64::from(report.sessions_discovered))
        .or_else(|| {
            after.as_ref().ok().map(|progress| {
                progress
                    .imported_sessions
                    .saturating_sub(before.imported_sessions)
            })
        });
    let mut details = json!({
        "schema": SCHEMA,
        "jobType": "cass_import",
        "dryRun": options.dry_run,
        "selectionLimit": options.limit,
        "ledgerId": ledger_id,
        "durableMutation": durable_mutation,
        "mutationStatus": match durable_mutation {
            Some(true) => "changed",
            Some(false) => "unchanged",
            None => "unknown",
        },
        "mutationMayHaveOccurred": durable_mutation.is_none(),
        "indexPublished": false,
        "indexRequiredAction": if options.dry_run { None } else { Some(index_repair(options)) },
        "retainedProgress": after.as_ref().ok().map(|progress| progress.data_json()),
        "progressReadError": after.as_ref().err(),
        "import": report.map(CassImportReport::data_json),
    });
    if let Some(reason) = cancelled {
        let outcome = if is_timeout(&reason) {
            RunOutcome::TimedOut
        } else {
            RunOutcome::Cancelled
        };
        details["code"] = json!(if outcome == RunOutcome::TimedOut {
            "cass_import_timed_out"
        } else {
            "cass_import_cancelled"
        });
        details["cancelReason"] = json!(reason);
        return (
            outcome,
            items,
            Some("CASS import stopped; any committed session progress is retained.".to_owned()),
            Some(details),
        );
    }
    if monitor_failed {
        details["code"] = json!("cass_import_cancellation_monitor_failed");
        return (
            RunOutcome::Failed,
            items,
            Some("CASS import cancellation monitor failed; inspect retained progress.".to_owned()),
            Some(details),
        );
    }
    match result {
        Err(error) => {
            details["code"] = json!("cass_import_failed");
            details["repair"] = json!(
                error
                    .resolved_repair_hint()
                    .unwrap_or_else(|| REPAIR.to_owned())
            );
            details["subprocess"] = error
                .subprocess_diagnostics_json()
                .unwrap_or(JsonValue::Null);
            (
                RunOutcome::Failed,
                items,
                Some(format!("CASS import failed: {error}")),
                Some(details),
            )
        }
        Ok(report) => {
            if let Some(degradation) = report.history_degradation() {
                details["code"] = json!(degradation.code);
                details["degraded"] = json!([degradation.data_json()]);
                return (
                    RunOutcome::Failed,
                    items,
                    Some(degradation.message),
                    Some(details),
                );
            }
            if let Err(error) = after {
                details["code"] = json!("cass_import_progress_unavailable");
                return (RunOutcome::Failed, items, Some(error), Some(details));
            }
            (RunOutcome::Success, items, None, Some(details))
        }
    }
}

#[cfg(test)]
#[path = "cass_import_tests.rs"]
mod tests;
