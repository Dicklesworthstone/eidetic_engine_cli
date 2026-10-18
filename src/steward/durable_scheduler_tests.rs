use super::*;
use crate::serve::{DaemonExecution, daemon_job_table_path, daemon_status_report, load_daemon_job_rows};
use std::sync::mpsc;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixture() -> Result<(tempfile::TempDir, PathBuf), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().canonicalize()?;
    Ok((temporary, workspace))
}

fn custom_options(options: &DaemonForegroundOptions) -> DaemonForegroundOptions {
    let mut options = options.clone();
    // Exercise a real built-in abstention handler without opening a database,
    // contacting peers, or loading a model. The scheduler and executor are real.
    options.job_types = vec![JobType::Custom];
    options
}

#[test]
fn public_scheduler_route_selects_the_durable_entrypoint() {
    // Source contract, not a function-pointer identity check (the compiler
    // may duplicate or merge function bodies across codegen units).
    let library = include_str!("../lib.rs");
    let facade = include_str!("../steward_api.rs");
    let server = include_str!("../daemon/server.rs");
    assert!(library.contains("#[path = \"steward_api.rs\"]\npub mod steward;"));
    assert!(facade.contains("pub use durable_scheduler::run_daemon_background_scheduler;"));
    assert!(server.contains("crate::steward::run_daemon_background_scheduler("));
}

#[test]
fn missing_store_planning_preserves_job_order_scope_and_shared_shutdown_without_writes() -> TestResult {
    let (_temporary, workspace) = fixture()?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let options = tick_options(&workspace, Arc::clone(&shutdown))?;
    assert_eq!(options.job_types, BACKGROUND_JOBS);
    assert_eq!(options.workspace, workspace.to_str().unwrap());
    assert_eq!(options.runner_options.workspace_path.as_deref(), Some(workspace.as_path()));
    assert_eq!(options.tick_limit, 1);
    assert_eq!(options.interval_ms, 0);
    assert!(!options.dry_run);
    assert!(options.runner_options.continue_on_error);
    assert!(Arc::ptr_eq(options.runner_options.cancellation_flag.as_ref().unwrap(), &shutdown));
    assert!(!workspace.join(".ee").exists());
    assert!(!index_jobs_need_processing(&workspace)?);
    Ok(())
}

#[test]
fn precancelled_scheduler_does_not_resolve_workspace_or_invoke_executor() -> TestResult {
    let (temporary, _) = fixture()?;
    let missing = temporary.path().join("absent");
    let shutdown = Arc::new(AtomicBool::new(true));
    run_scheduler(missing.to_str().unwrap(), shutdown, Duration::ZERO, |_| {
        panic!("pre-cancelled scheduler must not execute");
    });
    assert!(!missing.exists());
    Ok(())
}

#[test]
fn shutdown_wakes_sleep_first_scheduler_without_a_handler_or_journal() -> TestResult {
    let (_temporary, workspace) = fixture()?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let worker_shutdown = Arc::clone(&shutdown);
    let worker_workspace = workspace.to_str().unwrap().to_owned();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        crate::steward::run_daemon_background_scheduler(&worker_workspace, worker_shutdown);
        done_tx.send(()).unwrap();
    });
    let ready = ready_rx.recv_timeout(Duration::from_secs(5));
    shutdown.store(true, Ordering::Release);
    worker.thread().unpark();
    let finished = done_rx.recv_timeout(Duration::from_secs(5));
    worker.join().map_err(|_| "scheduler worker panicked")?;
    ready?;
    finished?;
    assert!(!workspace.join(".ee").exists());
    Ok(())
}

#[test]
fn spurious_unpark_does_not_admit_a_tick_before_the_deadline() -> TestResult {
    let shutdown = AtomicBool::new(false);
    std::thread::current().unpark();
    let deadline = Instant::now() + Duration::from_millis(20);
    assert!(wait_until(&shutdown, deadline));
    assert!(Instant::now() >= deadline);
    shutdown.store(true, Ordering::Release);
    assert!(!wait_until(&shutdown, Instant::now()));
    Ok(())
}

#[test]
fn real_scheduler_tick_syncs_start_and_completion_and_releases_execution_ownership() -> TestResult {
    let (_temporary, workspace) = fixture()?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut observed = None;
    run_scheduler(workspace.to_str().unwrap(), Arc::clone(&shutdown), Duration::ZERO, |options| {
        let report = run_daemon_foreground_durable(&custom_options(options));
        shutdown.store(true, Ordering::Release);
        observed = Some(report.clone());
        report
    });
    let report = observed.ok_or("executor was not called")??;
    assert_eq!(report.completed_ticks, 1);
    assert_eq!(report.completed_jobs, 1);
    assert_eq!(report.skipped, 1);
    let rows = load_daemon_job_rows(&workspace)?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].status, "running");
    assert_eq!(rows[1].status, "skipped");
    assert_eq!(rows[0].daemon_job_key, rows[1].daemon_job_key);
    assert_eq!(rows[1].run_id, report.run_id);
    assert_eq!(daemon_status_report(&workspace, &[], 0)?.open_job_count, 0);
    let next_options = custom_options(&tick_options(&workspace, Arc::new(AtomicBool::new(false)))?);
    let next_owner = DaemonExecution::begin(&next_options)?;
    assert_eq!(next_owner.recovered_orphans(), 0);
    assert!(!workspace.join(".ee/ee.db").exists());
    Ok(())
}

#[test]
fn transient_tick_failure_is_retried_without_stopping_the_scheduler() -> TestResult {
    let (_temporary, workspace) = fixture()?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut calls = 0;
    let mut completed = None;
    run_scheduler(workspace.to_str().unwrap(), Arc::clone(&shutdown), Duration::ZERO, |options| {
        calls += 1;
        if calls == 1 {
            return Err("temporary admission failure".to_owned());
        }
        let result = run_daemon_foreground_durable(&custom_options(options));
        shutdown.store(true, Ordering::Release);
        completed = Some(result.clone());
        result
    });
    assert_eq!(calls, 2);
    assert_eq!(completed.ok_or("retry was not executed")??.completed_jobs, 1);
    assert_eq!(load_daemon_job_rows(&workspace)?.len(), 2);
    Ok(())
}

#[test]
fn next_background_tick_recovers_only_abandoned_work_before_new_handlers() -> TestResult {
    let (_temporary, workspace) = fixture()?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let options = custom_options(&tick_options(&workspace, Arc::clone(&shutdown))?);
    let mut prior = DaemonExecution::begin(&options)?;
    prior.begin_next_job()?;
    let old_run = prior.run_id().to_owned();
    drop(prior);
    let prefix = std::fs::read(daemon_job_table_path(&workspace))?;
    let mut completed = None;
    run_scheduler(workspace.to_str().unwrap(), Arc::clone(&shutdown), Duration::ZERO, |options| {
        let result = run_daemon_foreground_durable(&custom_options(options));
        shutdown.store(true, Ordering::Release);
        completed = Some(result.clone());
        result
    });
    let report = completed.ok_or("tick did not execute")??;
    assert_eq!(report.recovered_orphans, 1);
    assert_ne!(report.run_id, old_run);
    let rows = load_daemon_job_rows(&workspace)?;
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[1].run_id, old_run);
    assert!(rows[1].recovered_from_orphan);
    assert_eq!(rows[3].status, "skipped");
    assert!(std::fs::read(daemon_job_table_path(&workspace))?.starts_with(&prefix));
    assert_eq!(daemon_status_report(&workspace, &[], 0)?.open_job_count, 0);
    Ok(())
}

#[test]
fn live_execution_owner_excludes_background_recovery_and_handler_admission() -> TestResult {
    let (_temporary, workspace) = fixture()?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let options = custom_options(&tick_options(&workspace, Arc::clone(&shutdown))?);
    let mut live = DaemonExecution::begin(&options)?;
    live.begin_next_job()?;
    let prefix = std::fs::read(daemon_job_table_path(&workspace))?;
    let mut observed = None;
    run_scheduler(workspace.to_str().unwrap(), Arc::clone(&shutdown), Duration::ZERO, |options| {
        let result = run_daemon_foreground_durable(&custom_options(options));
        shutdown.store(true, Ordering::Release);
        observed = Some(result.clone());
        result
    });
    assert!(observed.ok_or("tick was not attempted")?.is_err());
    assert_eq!(std::fs::read(daemon_job_table_path(&workspace))?, prefix);
    assert_eq!(daemon_status_report(&workspace, &[], 0)?.open_job_count, 1);
    Ok(())
}

#[test]
fn canonical_workspace_rejects_file_and_missing_paths_without_creating_state() -> TestResult {
    let (temporary, workspace) = fixture()?;
    let file = temporary.path().join("not-a-workspace");
    std::fs::write(&file, b"unchanged")?;
    assert!(canonical_workspace(&file).is_err());
    assert!(canonical_workspace(&workspace.join("absent")).is_err());
    assert_eq!(canonical_workspace(&workspace.join("."))?, workspace);
    assert_eq!(std::fs::read(file)?, b"unchanged");
    Ok(())
}

#[test]
fn explicit_rebuild_takes_precedence_without_probing_or_publishing_the_queue() {
    assert_eq!(
        select_index_job(true, || panic!("rebuild must not inspect the coalesce queue")).unwrap(),
        Some(JobType::IndexRebuild),
    );
}

#[test]
fn only_pending_queue_work_schedules_coalesce_and_never_an_unrequested_rebuild() {
    assert_eq!(select_index_job(false, || Ok(true)).unwrap(), Some(JobType::IndexCoalesce));
    assert_eq!(select_index_job(false, || Ok(false)).unwrap(), None);
    assert_eq!(
        select_index_job(false, || Err("queue unavailable".to_owned())).unwrap_err(),
        "queue unavailable",
    );
}
