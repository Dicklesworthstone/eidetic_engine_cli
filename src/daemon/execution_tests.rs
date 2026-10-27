use super::*;
use crate::steward::RunOutcome;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture() -> Result<(tempfile::TempDir, DaemonForegroundOptions), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let mut options = DaemonForegroundOptions::new(root.to_string_lossy().into_owned());
    options.tick_limit = 3;
    options.interval_ms = 0;
    options.job_types = vec![JobType::HealthCheck, JobType::DecaySweep];
    Ok((temp, options))
}

fn result(invocation: &DaemonJobInvocation) -> JobRunResult {
    JobRunResult {
        job_id: invocation.runner_job_id.clone(),
        job_type: invocation.job_type,
        outcome: RunOutcome::Success,
        duration_ms: 1,
        items_processed: Some(1),
        error: None,
        budget_summary: None,
        details: Some(json!({"durableMutation": true})),
        dry_run: false,
    }
}

#[test]
fn only_the_current_job_is_started_and_each_finish_is_visible_before_next_admission() -> TestResult
{
    let (_temp, options) = fixture()?;
    let root = Path::new(&options.workspace);
    let mut execution = DaemonExecution::begin(&options)?;
    assert_eq!(execution.completed_jobs(), 0);
    assert_eq!(daemon_status_report(root, &[], 0)?.row_count, 0);
    assert!(!daemon_job_table_path(root).exists());
    for index in 0..6 {
        let invocation = execution.begin_next_job()?.unwrap();
        assert_eq!(invocation.tick, index / 2 + 1);
        assert_eq!(
            invocation.runner_job_id,
            format!("job-{:06}", index % 2 + 1)
        );
        let status = daemon_status_report(root, &[], 20)?;
        assert_eq!(status.row_count, (2 * index + 1) as usize);
        assert_eq!(status.open_job_count, 1);
        assert!(execution.begin_next_job().is_err());
        execution.finish_job(&invocation, &result(&invocation))?;
        let status = daemon_status_report(root, &[], 20)?;
        assert_eq!(status.row_count, (2 * index + 2) as usize);
        assert_eq!(status.open_job_count, 0);
        assert_eq!(execution.completed_ticks(), (index + 1) / 2);
    }
    assert_eq!(execution.completed_jobs(), 6);
    assert_eq!(execution.completed_ticks(), 3);
    assert!(execution.begin_next_job()?.is_none());
    assert!(execution.begin_next_job()?.is_none());
    let rows = load_daemon_job_rows(root)?;
    for pair in rows.as_chunks::<2>().0 {
        assert_eq!(pair[0].daemon_job_key, pair[1].daemon_job_key);
        assert_eq!(pair[0].started_at, pair[1].started_at);
        assert_ne!(pair[0].row_id, pair[1].row_id);
        assert!(pair[1].durable_mutation);
        assert_eq!(pair[1].items_processed, Some(1));
    }
    Ok(())
}

#[test]
fn restart_preserves_completed_ticks_and_recovers_only_the_inflight_job() -> TestResult {
    let (_temp, options) = fixture()?;
    let root = Path::new(&options.workspace);
    let mut execution = DaemonExecution::begin(&options)?;
    let old_run = execution.run_id().to_owned();
    for _ in 0..2 {
        let invocation = execution.begin_next_job()?.unwrap();
        execution.finish_job(&invocation, &result(&invocation))?;
    }
    let interrupted = execution.begin_next_job()?.unwrap();
    assert_eq!(interrupted.tick, 2);
    let prefix = fs::read(daemon_job_table_path(root))?;
    drop(execution); // Simulate an owner that stopped, not a live run.
    let mut restarted = DaemonExecution::begin(&options)?;
    assert_ne!(restarted.run_id(), old_run);
    assert_eq!(restarted.recovered_orphans(), 1);
    assert_eq!(daemon_status_report(root, &[], 20)?.open_job_count, 0);
    assert!(fs::read(daemon_job_table_path(root))?.starts_with(&prefix));
    let rows = load_daemon_job_rows(root)?;
    assert_eq!(rows.len(), 6);
    assert_eq!(rows.iter().filter(|row| row.status == "success").count(), 2);
    let recovered = rows.last().unwrap();
    assert!(recovered.recovered_from_orphan);
    assert_eq!(recovered.run_id, old_run);
    assert_eq!(recovered.tick, 2);
    assert_eq!(recovered.runner_job_id, interrupted.runner_job_id);
    assert!(rows.iter().all(|row| row.tick < 3));
    assert_eq!(restarted.begin_next_job()?.unwrap().tick, 1);
    Ok(())
}

#[test]
fn live_execution_excludes_recovery_and_both_old_and_new_execution_owners() -> TestResult {
    let (_temp, options) = fixture()?;
    let root = Path::new(&options.workspace);
    let mut execution = DaemonExecution::begin(&options)?;
    execution.begin_next_job()?;
    let before = fs::read(daemon_job_table_path(root))?;
    assert!(recover_orphaned_daemon_jobs(root, "must not cancel a live owner").is_err());
    assert!(DaemonExecution::begin(&options).is_err());
    assert!(record_daemon_foreground_start(root, &options).is_err());
    assert_eq!(fs::read(daemon_job_table_path(root))?, before);
    assert_eq!(daemon_status_report(root, &[], 0)?.open_job_count, 1);
    drop(execution);
    let replacement = DaemonExecution::begin(&options)?;
    assert_eq!(replacement.recovered_orphans(), 1);
    Ok(())
}

#[test]
fn mismatched_and_duplicate_finishes_cannot_advance_or_change_the_journal() -> TestResult {
    let (_temp, options) = fixture()?;
    let root = Path::new(&options.workspace);
    let mut execution = DaemonExecution::begin(&options)?;
    let invocation = execution.begin_next_job()?.unwrap();
    let before = fs::read(daemon_job_table_path(root))?;
    let good = result(&invocation);
    let mut wrong_id = good.clone();
    wrong_id.job_id = "another-job".to_owned();
    let mut wrong_type = good.clone();
    wrong_type.job_type = JobType::BackupExport;
    let mut dry_run = good.clone();
    dry_run.dry_run = true;
    for rejected in [wrong_id, wrong_type, dry_run] {
        assert!(execution.finish_job(&invocation, &rejected).is_err());
        assert_eq!(fs::read(daemon_job_table_path(root))?, before);
        assert_eq!(execution.completed_jobs(), 0);
    }
    execution.finish_job(&invocation, &good)?;
    let after = fs::read(daemon_job_table_path(root))?;
    assert!(execution.finish_job(&invocation, &good).is_err());
    assert_eq!(fs::read(daemon_job_table_path(root))?, after);
    assert_eq!(execution.completed_jobs(), 1);
    Ok(())
}

#[test]
fn oversized_terminal_record_stops_admission_and_remains_recoverable() -> TestResult {
    let (_temp, options) = fixture()?;
    let root = Path::new(&options.workspace);
    let mut execution = DaemonExecution::begin(&options)?;
    let invocation = execution.begin_next_job()?.unwrap();
    let before = fs::read(daemon_job_table_path(root))?;
    let mut oversized = result(&invocation);
    oversized.error = Some("\0".repeat(MAX_ROW_BYTES / 6));
    assert!(execution.finish_job(&invocation, &oversized).is_err());
    assert_eq!(fs::read(daemon_job_table_path(root))?, before);
    assert_eq!(execution.completed_jobs(), 0);
    assert!(execution.begin_next_job().is_err());
    assert!(
        execution
            .finish_job(&invocation, &result(&invocation))
            .is_err()
    );
    drop(execution);
    let restarted = DaemonExecution::begin(&options)?;
    assert_eq!(restarted.recovered_orphans(), 1);
    Ok(())
}

#[test]
fn stop_closes_only_the_active_job_and_does_not_claim_rollback() -> TestResult {
    let (_temp, options) = fixture()?;
    let root = Path::new(&options.workspace);
    let mut execution = DaemonExecution::begin(&options)?;
    execution.begin_next_job()?;
    execution.stop("supervisor cancellation")?;
    assert!(execution.begin_next_job().is_err());
    let rows = load_daemon_job_rows(root)?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].status, "cancelled");
    assert!(rows[1].reason.contains("outcome may be unknown"));
    assert!(!rows[1].recovered_from_orphan);
    assert_eq!(daemon_status_report(root, &[], 0)?.open_job_count, 0);
    drop(execution);
    let restarted = DaemonExecution::begin(&options)?;
    assert_eq!(restarted.recovered_orphans(), 0);
    Ok(())
}

#[test]
fn idle_stop_creates_no_phantom_jobs() -> TestResult {
    let (_temp, options) = fixture()?;
    let root = Path::new(&options.workspace);
    let mut execution = DaemonExecution::begin(&options)?;
    execution.stop("cancelled before scheduling")?;
    assert!(!daemon_job_table_path(root).exists());
    assert_eq!(execution.completed_jobs(), 0);
    assert!(execution.begin_next_job().is_err());
    Ok(())
}

#[test]
fn corrupt_complete_history_prevents_a_new_execution_without_mutating_evidence() -> TestResult {
    let (_temp, options) = fixture()?;
    let root = Path::new(&options.workspace);
    let mut execution = DaemonExecution::begin(&options)?;
    execution.begin_next_job()?;
    drop(execution);
    let path = daemon_job_table_path(root);
    OpenOptions::new()
        .append(true)
        .open(&path)?
        .write_all(b"{invalid}\n")?;
    let before = fs::read(&path)?;
    assert!(DaemonExecution::begin(&options).is_err());
    assert_eq!(fs::read(&path)?, before);
    Ok(())
}

#[test]
fn invalid_and_dry_run_requests_cannot_acquire_an_execution_lease() -> TestResult {
    let (_temp, mut options) = fixture()?;
    let root = PathBuf::from(&options.workspace);
    options.dry_run = true;
    assert!(DaemonExecution::begin(&options).is_err());
    assert!(!root.join(".ee").exists());
    options.dry_run = false;
    options.tick_limit = 0;
    assert!(DaemonExecution::begin(&options).is_err());
    assert!(!root.join(".ee").exists());
    options.tick_limit = 1;
    options.job_types = vec![JobType::HealthCheck; MAX_JOBS_PER_TICK + 1];
    assert!(DaemonExecution::begin(&options).is_err());
    assert!(!root.join(".ee").exists());
    Ok(())
}

#[test]
fn failed_start_stops_cursor_without_granting_handler_permission() -> TestResult {
    let (_temp, options) = fixture()?;
    let root = Path::new(&options.workspace);
    let mut execution = DaemonExecution::begin(&options)?;
    fs::create_dir(daemon_job_table_path(root))?;
    assert!(execution.begin_next_job().is_err());
    assert!(execution.active.is_none());
    assert_eq!(execution.completed_jobs(), 0);
    assert!(execution.begin_next_job().is_err());
    Ok(())
}

#[test]
fn stale_invocation_from_a_prior_tick_or_run_cannot_finish_current_work() -> TestResult {
    let (_temp, mut options) = fixture()?;
    options.job_types = vec![JobType::HealthCheck];
    let root = Path::new(&options.workspace);
    let mut execution = DaemonExecution::begin(&options)?;
    let first = execution.begin_next_job()?.unwrap();
    execution.finish_job(&first, &result(&first))?;
    let second = execution.begin_next_job()?.unwrap();
    assert_eq!(first.runner_job_id, second.runner_job_id);
    let before = fs::read(daemon_job_table_path(root))?;
    assert!(execution.finish_job(&first, &result(&second)).is_err());
    assert_eq!(fs::read(daemon_job_table_path(root))?, before);
    execution.finish_job(&second, &result(&second))?;
    drop(execution);
    let mut next_run = DaemonExecution::begin(&options)?;
    let current = next_run.begin_next_job()?.unwrap();
    assert_eq!(current.tick, first.tick);
    assert_eq!(current.runner_job_id, first.runner_job_id);
    let before = fs::read(daemon_job_table_path(root))?;
    assert!(next_run.finish_job(&first, &result(&current)).is_err());
    assert_eq!(fs::read(daemon_job_table_path(root))?, before);
    next_run.finish_job(&current, &result(&current))?;
    Ok(())
}
