use super::*;
use crate::serve::{
    daemon_job_table_path, daemon_status_report, load_daemon_job_rows, recover_orphaned_daemon_jobs,
};
use crate::steward::JobType;
use std::sync::{Arc, atomic::AtomicBool};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture() -> Result<(tempfile::TempDir, DaemonForegroundOptions), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let mut options = DaemonForegroundOptions::new(root.to_string_lossy().into_owned());
    options.tick_limit = 3;
    options.interval_ms = 0;
    // These invoke the real missing-store team handler and explicit custom-job
    // abstention path. No peer, model, service manager or existing store is used.
    options.job_types = vec![JobType::TeamSteward, JobType::Custom];
    Ok((temp, options))
}

#[test]
fn dry_run_is_an_explicit_plan_without_workspace_or_handler_side_effects() -> TestResult {
    let (temp, mut options) = fixture()?;
    options.workspace = temp.path().join("absent").to_string_lossy().into_owned();
    options.dry_run = true;
    let report = run_daemon_foreground_durable(&options)?;
    assert!(report.dry_run);
    assert_eq!(report.run_id, "dry-run");
    assert_eq!(report.requested_ticks, 3);
    assert_eq!(report.requested_jobs, 6);
    assert_eq!(report.completed_ticks, 0);
    assert_eq!(report.completed_jobs, 0);
    assert_eq!(report.recovered_orphans, 0);
    assert!(!Path::new(&options.workspace).exists());
    Ok(())
}

#[test]
fn invalid_admission_limits_fail_before_any_durable_state() -> TestResult {
    for dry_run in [false, true] {
        let (_temp, mut options) = fixture()?;
        options.dry_run = dry_run;
        let root = PathBuf::from(&options.workspace);
        for tick_limit in [0, 1_001, u32::MAX] {
            options.tick_limit = tick_limit;
            assert!(run_daemon_foreground_durable(&options).is_err());
            assert!(!root.join(".ee").exists());
        }
        options.tick_limit = 1;
        options.interval_ms = 60_001;
        assert!(run_daemon_foreground_durable(&options).is_err());
        options.interval_ms = 0;
        options.job_types = vec![JobType::Custom; 257];
        assert!(run_daemon_foreground_durable(&options).is_err());
        assert!(!root.join(".ee").exists());
    }
    Ok(())
}

#[test]
fn conflicting_handler_workspace_is_refused_before_either_workspace_changes() -> TestResult {
    for dry_run in [false, true] {
        let (_temp, mut options) = fixture()?;
        let other = tempfile::tempdir()?;
        let other_root = other.path().canonicalize()?;
        options.dry_run = dry_run;
        options.runner_options.workspace_path = Some(other_root.clone());
        let error = run_daemon_foreground_durable(&options).unwrap_err();
        assert!(error.contains("does not match"));
        assert!(!Path::new(&options.workspace).join(".ee").exists());
        assert!(!other_root.join(".ee").exists());
    }
    Ok(())
}

#[test]
fn matching_handler_workspace_is_accepted_without_dry_run_writes() -> TestResult {
    let (_temp, mut options) = fixture()?;
    options.dry_run = true;
    options.runner_options.workspace_path = Some(PathBuf::from(&options.workspace).join("."));
    assert_eq!(run_daemon_foreground_durable(&options)?.completed_jobs, 0);
    assert!(!Path::new(&options.workspace).join(".ee").exists());
    Ok(())
}

#[test]
fn pre_cancelled_request_creates_no_lease_or_job_rows() -> TestResult {
    let (_temp, mut options) = fixture()?;
    options.runner_options.cancellation_flag = Some(Arc::new(AtomicBool::new(true)));
    let error = run_daemon_foreground_durable(&options).unwrap_err();
    assert!(error.contains("cancelled"));
    assert!(!Path::new(&options.workspace).join(".ee").exists());
    Ok(())
}

#[test]
fn pre_cancelled_request_does_not_even_recover_an_old_orphan() -> TestResult {
    let (_temp, mut options) = fixture()?;
    let root = PathBuf::from(&options.workspace);
    let mut prior = DaemonExecution::begin(&options)?;
    prior.begin_next_job()?;
    drop(prior);
    let path = daemon_job_table_path(&root);
    let before = fs::read(&path)?;
    options.runner_options.cancellation_flag = Some(Arc::new(AtomicBool::new(true)));
    assert!(run_daemon_foreground_durable(&options).is_err());
    assert_eq!(fs::read(&path)?, before);
    assert_eq!(daemon_status_report(&root, &[], 0)?.open_job_count, 1);
    Ok(())
}

#[test]
fn production_runner_journals_every_requested_job_without_accumulating_tick_reports() -> TestResult {
    let (_temp, options) = fixture()?;
    let root = Path::new(&options.workspace);
    let report = run_daemon_foreground_durable(&options)?;
    assert_eq!(report.completed_ticks, 3);
    assert_eq!(report.completed_jobs, 6);
    assert_eq!(report.succeeded, 3);
    assert_eq!(report.skipped, 3);
    assert_eq!(report.failed + report.cancelled + report.timed_out, 0);
    assert!(!report.stopped_early);
    assert!(!report.was_cancelled);
    assert!(!report.dry_run);
    assert_ne!(report.run_id, "dry-run");
    let rows = load_daemon_job_rows(root)?;
    assert_eq!(rows.len(), 12);
    for (index, pair) in rows.chunks_exact(2).enumerate() {
        assert_eq!(pair[0].tick, index as u32 / 2 + 1);
        assert_eq!(pair[0].runner_job_id, format!("job-{:06}", index % 2 + 1));
        assert_eq!(pair[0].status, "running");
        assert_eq!(pair[1].status, if index % 2 == 0 { "success" } else { "skipped" });
        assert_eq!(pair[0].daemon_job_key, pair[1].daemon_job_key);
        assert_eq!(pair[1].run_id, report.run_id);
        assert!(!pair[1].durable_mutation);
        assert_eq!(pair[1].items_processed, Some(0));
    }
    assert_eq!(daemon_status_report(root, &[], 0)?.open_job_count, 0);
    assert_eq!(recover_orphaned_daemon_jobs(root, "completed run")?.open_jobs_cancelled, 0);
    assert!(!root.join(".ee/ee.db").exists());
    Ok(())
}

#[test]
fn real_handler_failure_respects_continue_on_error_and_does_not_invent_future_jobs() -> TestResult {
    for continue_on_error in [false, true] {
        let (_temp, mut options) = fixture()?;
        let root = PathBuf::from(&options.workspace);
        options.job_types = vec![JobType::IndexRebuild, JobType::Custom];
        options.runner_options.continue_on_error = continue_on_error;
        // IndexRebuild's real preflight explicitly fails on a missing store.
        let report = run_daemon_foreground_durable(&options)?;
        assert_eq!(report.failed, if continue_on_error { 3 } else { 1 });
        assert_eq!(report.skipped, if continue_on_error { 3 } else { 0 });
        assert_eq!(report.completed_jobs, if continue_on_error { 6 } else { 1 });
        assert_eq!(report.completed_ticks, if continue_on_error { 3 } else { 0 });
        assert_eq!(report.stopped_early, !continue_on_error);
        let rows = load_daemon_job_rows(&root)?;
        assert_eq!(rows.len() as u64, report.completed_jobs * 2);
        assert_eq!(rows[1].status, "failed");
        assert!(rows[1].error.as_deref().unwrap_or("").contains("does not exist"));
        assert_eq!(daemon_status_report(&root, &[], 0)?.open_job_count, 0);
        assert!(!root.join(".ee/ee.db").exists());
    }
    Ok(())
}

#[test]
fn manual_maintenance_owner_prevents_daemon_handler_admission() -> TestResult {
    let (_temp, options) = fixture()?;
    let root = Path::new(&options.workspace);
    let _manual = try_acquire_maintenance_job_lock(root, "manual-test")
        .map_err(|error| error.message().to_owned())?;
    let error = run_daemon_foreground_durable(&options).unwrap_err();
    assert!(error.contains("maintenance ownership"));
    assert!(!daemon_job_table_path(root).exists());
    Ok(())
}

#[test]
fn live_daemon_owner_prevents_production_executor_from_recovering_it() -> TestResult {
    let (_temp, options) = fixture()?;
    let root = Path::new(&options.workspace);
    let mut existing = DaemonExecution::begin(&options)?;
    existing.begin_next_job()?;
    let before = fs::read(daemon_job_table_path(root))?;
    assert!(run_daemon_foreground_durable(&options).is_err());
    assert_eq!(fs::read(daemon_job_table_path(root))?, before);
    assert_eq!(daemon_status_report(root, &[], 0)?.open_job_count, 1);
    Ok(())
}

fn report_for(results: Vec<JobRunResult>) -> RunnerReport {
    RunnerReport {
        results, total_duration_ms: 0, succeeded: 0, failed: 0, skipped: 0,
        was_cancelled: false, started_at: "start".to_owned(), completed_at: "finish".to_owned(),
    }
}

fn result_for(invocation: &DaemonJobInvocation) -> JobRunResult {
    JobRunResult {
        job_id: "local-job".to_owned(), job_type: invocation.job_type,
        outcome: RunOutcome::Success, duration_ms: 12, items_processed: Some(34),
        error: None, budget_summary: None,
        details: Some(serde_json::json!({"durableMutation": true})), dry_run: false,
    }
}

#[test]
fn invalid_runner_result_shape_or_identity_cannot_be_attached_to_the_journal() -> TestResult {
    let (_temp, options) = fixture()?;
    let root = Path::new(&options.workspace);
    let mut execution = DaemonExecution::begin(&options)?;
    let invocation = execution.begin_next_job()?.unwrap();
    let before = fs::read(daemon_job_table_path(root))?;
    let good = result_for(&invocation);
    assert!(take_result(report_for(Vec::new()), "local-job", &invocation).is_err());
    assert!(take_result(report_for(vec![good.clone(), good.clone()]), "local-job", &invocation).is_err());
    let mut wrong_id = good.clone();
    wrong_id.job_id = "unrelated".to_owned();
    let mut wrong_type = good.clone();
    wrong_type.job_type = JobType::IndexRebuild;
    let mut dry = good;
    dry.dry_run = true;
    for bad in [wrong_id, wrong_type, dry] {
        assert!(take_result(report_for(vec![bad]), "local-job", &invocation).is_err());
    }
    assert_eq!(fs::read(daemon_job_table_path(root))?, before);
    Ok(())
}

#[test]
fn checked_local_identity_remapping_preserves_the_actual_handler_result() -> TestResult {
    let (_temp, options) = fixture()?;
    let root = Path::new(&options.workspace);
    let mut execution = DaemonExecution::begin(&options)?;
    let invocation = execution.begin_next_job()?.unwrap();
    let original = result_for(&invocation);
    let adapted = take_result(report_for(vec![original.clone()]), "local-job", &invocation)?;
    assert_eq!(adapted.job_id, invocation.runner_job_id);
    assert_eq!(adapted.outcome, original.outcome);
    assert_eq!(adapted.duration_ms, original.duration_ms);
    assert_eq!(adapted.items_processed, original.items_processed);
    assert_eq!(adapted.details, original.details);
    execution.finish_job(&invocation, &adapted)?;
    let rows = load_daemon_job_rows(root)?;
    assert_eq!(rows[1].duration_ms, Some(12));
    assert_eq!(rows[1].items_processed, Some(34));
    assert!(rows[1].durable_mutation);
    Ok(())
}

#[test]
fn cancellation_during_interval_preserves_completed_tick_and_releases_ownership() -> TestResult {
    use std::sync::mpsc;
    use std::time::Instant;

    let (_temp, mut options) = fixture()?;
    let root = PathBuf::from(&options.workspace);
    options.job_types = vec![JobType::Custom];
    options.tick_limit = 2;
    options.interval_ms = 60_000;
    let flag = Arc::new(AtomicBool::new(false));
    options.runner_options.cancellation_flag = Some(Arc::clone(&flag));
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _ = sender.send(run_daemon_foreground_durable(&options));
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_completed_tick = false;
    while Instant::now() < deadline {
        if let Ok(status) = daemon_status_report(&root, &[], 0) {
            if status.row_count == 2 && status.open_job_count == 0 {
                saw_completed_tick = true;
                break;
            }
        }
        if let Ok(result) = receiver.try_recv() {
            flag.store(true, Ordering::Release);
            return Err(format!("executor returned before first durable tick was observed: {result:?}").into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Always cancel the worker, including the failure path. Do not leave a
    // sixty-second test task running when the first-tick assertion fails.
    flag.store(true, Ordering::Release);
    let result = receiver.recv_timeout(Duration::from_secs(10))?;
    worker.join().map_err(|_| "daemon executor test worker panicked")?;
    assert!(saw_completed_tick, "first tick must be durable before the next one runs");
    assert!(result.unwrap_err().contains("cancelled"));
    let rows = load_daemon_job_rows(&root)?;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.tick == 1));
    assert_eq!(rows[1].status, "skipped");
    assert_eq!(daemon_status_report(&root, &[], 0)?.open_job_count, 0);
    assert_eq!(recover_orphaned_daemon_jobs(&root, "after cancellation")?.open_jobs_cancelled, 0);
    let _next_manual = try_acquire_maintenance_job_lock(&root, "next-manual")
        .map_err(|error| error.message().to_owned())?;
    Ok(())
}
