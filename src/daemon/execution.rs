//! A lease-owned, incremental journal cursor for real daemon execution.
//!
//! Only the job about to run gets a start record. A successful finish is synced
//! before the cursor advances; dropping an unfinished cursor leaves one orphan
//! for the next owner's recovery. This is not an exactly-once side-effect log:
//! a handler interrupted after a mutation can still have an unknown outcome.

use super::*;

/// Identity supplied to a handler after its start record has become durable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonJobInvocation {
    pub tick: u32,
    pub runner_job_id: String,
    pub job_type: JobType,
    daemon_job_key: String,
}

/// One workspace execution owner. Intentionally not Clone: there is one cursor
/// and at most one in-flight job, even when a caller uses multiple threads.
#[derive(Debug)]
pub struct DaemonExecution {
    workspace: PathBuf,
    table_path: PathBuf,
    run_id: String,
    job_types: Vec<JobType>,
    tick_limit: u32,
    next_tick: u32,
    next_job: usize,
    active: Option<DaemonJobRow>,
    stopped: bool,
    completed_jobs: u64,
    completed_ticks: u32,
    recovered_orphans: usize,
    _lease: File,
}

impl DaemonExecution {
    /// Validate admission limits without acquiring a lease or writing state.
    pub(crate) fn validate_request(options: &DaemonForegroundOptions) -> Result<(), String> {
        validate_options(options)
    }

    /// Validate and recover the selected workspace while holding the SAME
    /// execution lease that will cover all subsequent jobs and their finishes.
    /// No new job is recorded or executed by this constructor.
    pub fn begin(options: &DaemonForegroundOptions) -> Result<Self, String> {
        validate_options(options)?;
        if options.dry_run {
            return Err("A dry run must not acquire a durable daemon execution cursor".to_owned());
        }
        let requested = Path::new(&options.workspace);
        check_path(requested)?;
        let workspace = fs::canonicalize(requested)
            .map_err(|error| format!("Resolve daemon execution workspace: {error}"))?;
        if !workspace.is_dir() {
            return Err("Daemon workspace must be an existing directory".to_owned());
        }
        let lease = execution_lease(&workspace)?;
        let recovery = recover_orphaned_daemon_jobs_with_lease(
            &workspace,
            "previous daemon execution owner stopped; outcome may be unknown",
            &lease,
        )?;
        Ok(Self {
            table_path: daemon_job_table_path(&workspace),
            workspace,
            run_id: format!("daemon-run-{}", uuid::Uuid::now_v7()),
            job_types: options.job_types.clone(),
            tick_limit: options.tick_limit,
            next_tick: 1,
            next_job: 0,
            active: None,
            stopped: false,
            completed_jobs: 0,
            completed_ticks: 0,
            recovered_orphans: recovery.open_jobs_cancelled,
            _lease: lease,
        })
    }

    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    #[must_use]
    pub fn completed_jobs(&self) -> u64 {
        self.completed_jobs
    }

    #[must_use]
    pub fn completed_ticks(&self) -> u32 {
        self.completed_ticks
    }

    #[must_use]
    pub fn recovered_orphans(&self) -> usize {
        self.recovered_orphans
    }

    /// Persist the next start before returning permission to execute a handler.
    /// Never advances over an unfinished job or preallocates later ticks.
    pub fn begin_next_job(&mut self) -> Result<Option<DaemonJobInvocation>, String> {
        if self.stopped {
            return Err("Daemon execution has stopped; acquire a new execution owner".to_owned());
        }
        if self.active.is_some() {
            return Err("Finish the active daemon job before starting another".to_owned());
        }
        if self.next_tick > self.tick_limit {
            return Ok(None);
        }
        let runner_job_id = format!("job-{:06}", self.next_job + 1);
        let invocation = DaemonJobInvocation {
            daemon_job_key: job_key(&self.run_id, self.next_tick, &runner_job_id),
            tick: self.next_tick,
            runner_job_id,
            job_type: self.job_types[self.next_job],
        };
        let recorded_at = Utc::now().to_rfc3339();
        let row = DaemonJobRow {
            schema: DAEMON_JOB_ROW_SCHEMA_V1.to_owned(),
            row_id: new_row_id(),
            run_id: self.run_id.clone(),
            daemon_job_key: invocation.daemon_job_key.clone(),
            runner_job_id: invocation.runner_job_id.clone(),
            tick: invocation.tick,
            job_type: invocation.job_type.as_str().to_owned(),
            status: "running".to_owned(),
            outcome: None,
            workspace: self.workspace.to_string_lossy().into_owned(),
            write_owner_id: DAEMON_WRITE_OWNER_IDENTITY.to_owned(),
            reason: format!("daemon foreground tick {} job starting", invocation.tick),
            started_at: Some(recorded_at.clone()),
            completed_at: None,
            recorded_at,
            duration_ms: None,
            items_processed: None,
            error: None,
            dry_run: false,
            durable_mutation: false,
            recovered_from_orphan: false,
            recovery_reason: None,
        };
        if let Err(error) = append_rows(&self.table_path, std::slice::from_ref(&row)) {
            // Even a reported sync failure can follow a partial or complete
            // append. Never hand a caller permission to execute after it.
            self.stopped = true;
            return Err(error);
        }
        self.active = Some(row);
        Ok(Some(invocation))
    }

    /// Persist the result for precisely the active invocation. The next job is
    /// not admitted until append AND sync succeed. A journal failure stops this
    /// owner; recovery, not an in-memory retry, resolves the uncertain boundary.
    pub fn finish_job(
        &mut self,
        invocation: &DaemonJobInvocation,
        result: &JobRunResult,
    ) -> Result<(), String> {
        if self.stopped {
            return Err("Daemon execution has stopped; cannot acknowledge a result".to_owned());
        }
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "No daemon job is active".to_owned())?;
        if invocation.daemon_job_key != active.daemon_job_key
            || invocation.tick != active.tick
            || invocation.runner_job_id != active.runner_job_id
            || invocation.job_type.as_str() != active.job_type
            || result.job_id != active.runner_job_id
            || result.job_type.as_str() != active.job_type
            || result.dry_run
        {
            return Err("Daemon result does not match the active durable invocation".to_owned());
        }
        let completed_at = Utc::now().to_rfc3339();
        let row = row_from_result(
            &self.workspace,
            &self.run_id,
            active.tick,
            active.started_at.as_deref().unwrap_or(&active.recorded_at),
            &completed_at,
            result,
        );
        if let Err(error) = append_rows(&self.table_path, std::slice::from_ref(&row)) {
            self.stopped = true;
            return Err(error);
        }
        self.active = None;
        self.completed_jobs += 1;
        self.next_job += 1;
        if self.next_job == self.job_types.len() {
            self.completed_ticks += 1;
            self.next_tick += 1;
            self.next_job = 0;
        }
        Ok(())
    }

    /// Stop admission and record interruption of the active job, if any.
    /// `cancelled` is NOT proof of rollback; a handler may already have mutated
    /// durable state. No unstarted job or later tick is invented as cancelled.
    pub fn stop(&mut self, reason: &str) -> Result<(), String> {
        if self.stopped {
            return Err("Daemon execution already stopped; inspect its durable journal".to_owned());
        }
        self.stopped = true;
        let Some(active) = self.active.as_ref() else {
            return Ok(());
        };
        let mut row = active.clone();
        let recorded_at = Utc::now().to_rfc3339();
        row.row_id = new_row_id();
        row.status = "cancelled".to_owned();
        row.outcome = Some("cancelled".to_owned());
        row.reason =
            "daemon execution interrupted; prior handler outcome may be unknown".to_owned();
        row.error = Some(reason.to_owned());
        row.completed_at = Some(recorded_at.clone());
        row.recorded_at = recorded_at;
        append_rows(&self.table_path, std::slice::from_ref(&row))?;
        self.active = None;
        Ok(())
    }
}

#[cfg(test)]
#[path = "execution_tests.rs"]
mod tests;
