use super::*;
use crate::serve::{
    DaemonExecution, daemon_job_table_path, daemon_status_report, load_daemon_job_rows,
};
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

struct QueuedIndexFixture {
    _temporary: tempfile::TempDir,
    workspace: PathBuf,
    database: PathBuf,
    workspace_id: String,
    memory_id: String,
    job_id: String,
    _embedder: crate::core::index::TestWorkspaceEmbedderStackGuard,
}

impl QueuedIndexFixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        use crate::db::{
            CreateMemoryInput, CreateSearchIndexJobInput, CreateWorkspaceInput, SearchIndexJobType,
        };

        let (temporary, workspace) = fixture()?;
        std::fs::create_dir_all(workspace.join(".ee"))?;
        let database = workspace.join(".ee/ee.db");
        // A rebound store keeps its identity, which need not match its path.
        let workspace_id = crate::models::WorkspaceId::from_uuid(uuid::Uuid::now_v7()).to_string();
        let memory_id = crate::models::MemoryId::from_uuid(uuid::Uuid::now_v7()).to_string();
        let job_id = "sidx_00000000000000000000000987".to_owned();
        let connection = DbConnection::open_file(&database)?;
        connection.migrate()?;
        connection.insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace.to_string_lossy().into_owned(),
                name: Some("durable index retry".to_owned()),
            },
        )?;
        connection.insert_memory(
            &memory_id,
            &CreateMemoryInput {
                workspace_id: workspace_id.clone(),
                level: "semantic".to_owned(),
                kind: "fact".to_owned(),
                content: "Tangerine compass publication recovery sentinel.".to_owned(),
                workflow_id: None,
                confidence: 0.8,
                utility: 0.5,
                importance: 0.5,
                provenance_uri: Some("manual://durable-index-retry".to_owned()),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
        )?;
        connection.insert_search_index_job(
            &job_id,
            &CreateSearchIndexJobInput {
                workspace_id: workspace_id.clone(),
                job_type: SearchIndexJobType::SingleDocument,
                document_source: Some("memory".to_owned()),
                document_id: Some(memory_id.clone()),
                documents_total: 1,
            },
        )?;
        connection.close()?;
        let embedder = crate::core::index::install_test_hash_workspace_embedder(&workspace_id);
        Ok(Self {
            _temporary: temporary,
            workspace,
            database,
            workspace_id,
            memory_id,
            job_id,
            _embedder: embedder,
        })
    }

    fn job(&self) -> Result<crate::db::StoredSearchIndexJob, Box<dyn std::error::Error>> {
        let connection = DbConnection::open_file_read_only(&self.database)?;
        let mut jobs = connection.list_search_index_jobs(&self.workspace_id, None)?;
        connection.close()?;
        assert_eq!(jobs.len(), 1, "retry must preserve the original queue row");
        let job = jobs.pop().ok_or("queued index job disappeared")?;
        assert_eq!(job.id, self.job_id);
        assert_eq!(job.document_id.as_deref(), Some(self.memory_id.as_str()));
        Ok(job)
    }

    fn index_path(&self) -> PathBuf {
        self.workspace.join(".ee/index")
    }
}

fn selected_index_options(options: &DaemonForegroundOptions) -> DaemonForegroundOptions {
    assert_eq!(
        options
            .job_types
            .iter()
            .filter(|job| **job == JobType::IndexCoalesce)
            .count(),
        1,
        "the live scheduler must select exactly one retry handler"
    );
    assert!(!options.job_types.contains(&JobType::IndexRebuild));
    let mut selected = options.clone();
    // Keep the actual scheduled handler and durable owner, without unrelated
    // decay/health/team maintenance obscuring the publication outcome.
    selected
        .job_types
        .retain(|job| *job == JobType::IndexCoalesce);
    selected
}

fn assert_index_tick_journal(
    workspace: &Path,
    reports: &[DaemonExecutionReport],
    outcomes: &[&str],
) -> TestResult {
    let rows = load_daemon_job_rows(workspace)?;
    assert_eq!(rows.len(), outcomes.len() * 2);
    assert_eq!(reports.len(), outcomes.len());
    for ((pair, report), outcome) in rows.chunks_exact(2).zip(reports).zip(outcomes) {
        assert_eq!(pair[0].job_type, "index_coalesce");
        assert_eq!(pair[0].status, "running");
        assert_eq!(pair[1].status, *outcome);
        assert_eq!(pair[0].daemon_job_key, pair[1].daemon_job_key);
        assert_eq!(pair[0].run_id, report.run_id);
        assert_eq!(pair[1].run_id, report.run_id);
        assert_eq!(report.completed_ticks, 1);
        assert_eq!(report.completed_jobs, 1);
        assert_eq!(report.recovered_orphans, 0);
    }
    for pair in reports.windows(2) {
        assert_ne!(pair[0].run_id, pair[1].run_id);
    }
    assert_eq!(daemon_status_report(workspace, &[], 0)?.open_job_count, 0);
    Ok(())
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
fn missing_store_planning_preserves_job_order_scope_and_shared_shutdown_without_writes()
-> TestResult {
    let (_temporary, workspace) = fixture()?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let options = tick_options(&workspace, Arc::clone(&shutdown))?;
    assert_eq!(options.job_types, BACKGROUND_JOBS);
    assert_eq!(options.workspace, workspace.to_str().unwrap());
    assert_eq!(
        options.runner_options.workspace_path.as_deref(),
        Some(workspace.as_path())
    );
    assert_eq!(options.tick_limit, 1);
    assert_eq!(options.interval_ms, 0);
    assert!(!options.dry_run);
    assert!(options.runner_options.continue_on_error);
    assert!(Arc::ptr_eq(
        options.runner_options.cancellation_flag.as_ref().unwrap(),
        &shutdown
    ));
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
    run_scheduler(
        workspace.to_str().unwrap(),
        Arc::clone(&shutdown),
        Duration::ZERO,
        |options| {
            let report = run_daemon_foreground_durable(&custom_options(options));
            shutdown.store(true, Ordering::Release);
            observed = Some(report.clone());
            report
        },
    );
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
    run_scheduler(
        workspace.to_str().unwrap(),
        Arc::clone(&shutdown),
        Duration::ZERO,
        |options| {
            calls += 1;
            if calls == 1 {
                return Err("temporary admission failure".to_owned());
            }
            let result = run_daemon_foreground_durable(&custom_options(options));
            shutdown.store(true, Ordering::Release);
            completed = Some(result.clone());
            result
        },
    );
    assert_eq!(calls, 2);
    assert_eq!(
        completed.ok_or("retry was not executed")??.completed_jobs,
        1
    );
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
    run_scheduler(
        workspace.to_str().unwrap(),
        Arc::clone(&shutdown),
        Duration::ZERO,
        |options| {
            let result = run_daemon_foreground_durable(&custom_options(options));
            shutdown.store(true, Ordering::Release);
            completed = Some(result.clone());
            result
        },
    );
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
    run_scheduler(
        workspace.to_str().unwrap(),
        Arc::clone(&shutdown),
        Duration::ZERO,
        |options| {
            let result = run_daemon_foreground_durable(&custom_options(options));
            shutdown.store(true, Ordering::Release);
            observed = Some(result.clone());
            result
        },
    );
    assert!(observed.ok_or("tick was not attempted")?.is_err());
    assert_eq!(std::fs::read(daemon_job_table_path(&workspace))?, prefix);
    assert_eq!(daemon_status_report(&workspace, &[], 0)?.open_job_count, 1);
    Ok(())
}

#[test]
fn failed_and_cancelled_index_jobs_remain_actionable_until_completed() -> TestResult {
    let fixture = QueuedIndexFixture::new()?;
    let connection = DbConnection::open_file(&fixture.database)?;
    assert!(index_jobs_need_processing(&fixture.workspace)?);

    assert!(connection.start_search_index_job(&fixture.job_id)?);
    assert!(connection.fail_search_index_job(&fixture.job_id, "fixture publication failure")?);
    connection.close()?;
    assert!(
        index_jobs_need_processing(&fixture.workspace)?,
        "failed work remains actionable because the processor requeues it"
    );

    let shutdown = Arc::new(AtomicBool::new(false));
    let options = tick_options(&fixture.workspace, shutdown)?;
    selected_index_options(&options);
    assert_eq!(fixture.job()?.status, "failed", "planning stays read-only");

    let connection = DbConnection::open_file(&fixture.database)?;
    assert!(connection.requeue_search_index_job_for_retry(&fixture.job_id)?);
    assert!(connection.start_search_index_job(&fixture.job_id)?);
    assert!(connection.cancel_running_search_index_job(&fixture.job_id)?);
    connection.close()?;
    assert!(
        index_jobs_need_processing(&fixture.workspace)?,
        "cancelled work remains actionable because the processor requeues it"
    );
    assert_eq!(fixture.job()?.status, "cancelled");

    let connection = DbConnection::open_file(&fixture.database)?;
    assert!(connection.requeue_search_index_job_for_retry(&fixture.job_id)?);
    assert!(connection.start_search_index_job(&fixture.job_id)?);
    assert!(connection.complete_search_index_job(&fixture.job_id, 1)?);
    connection.close()?;
    assert!(!index_jobs_need_processing(&fixture.workspace)?);
    Ok(())
}

#[test]
fn repaired_publication_is_retried_by_the_next_durable_tick_and_becomes_searchable() -> TestResult {
    use crate::core::index::{IndexHealth, IndexStatusOptions, get_index_status};
    use crate::core::search::{SearchDedupMode, SearchOptions, SearchSourceMode, run_search};

    let fixture = QueuedIndexFixture::new()?;
    std::fs::write(fixture.index_path(), b"publication obstruction")?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut observed = Vec::new();
    run_scheduler(
        fixture.workspace.to_str().unwrap(),
        Arc::clone(&shutdown),
        Duration::from_millis(20),
        |options| {
            let selected = selected_index_options(options);
            if observed.len() == 1 {
                let failed = fixture.job().expect("failed queue row remains readable");
                assert_eq!(failed.status, "failed");
                assert!(failed.error_message.is_some());
                assert_eq!(failed.documents_indexed, 0);
                assert_eq!(
                    std::fs::read(fixture.index_path()).expect("obstruction stays intact"),
                    b"publication obstruction"
                );
                std::fs::remove_file(fixture.index_path()).expect("repair fixture obstruction");
            }
            let result = run_daemon_foreground_durable(&selected);
            observed.push(result.clone());
            if observed.len() == 2 {
                shutdown.store(true, Ordering::Release);
            }
            result
        },
    );
    let reports = observed.into_iter().collect::<Result<Vec<_>, _>>()?;
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].failed, 1);
    assert_eq!(reports[0].succeeded, 0);
    assert_eq!(reports[1].succeeded, 1);
    assert_eq!(reports[1].failed, 0);
    assert_index_tick_journal(&fixture.workspace, &reports, &["failed", "success"])?;
    let job = fixture.job()?;
    assert_eq!(job.status, "completed");
    assert_eq!(job.documents_indexed, 1);
    assert!(job.error_message.is_none());
    assert!(!index_jobs_need_processing(&fixture.workspace)?);

    // Establish publication before a foreground search could repair anything.
    let status = get_index_status(&IndexStatusOptions {
        workspace_path: fixture.workspace.clone(),
        database_path: Some(fixture.database.clone()),
        index_dir: None,
    })?;
    assert_eq!(status.health, IndexHealth::Ready);
    assert!(status.db_generation.is_some());
    assert_eq!(status.db_generation, status.index_generation);
    assert_eq!(status.index_document_count, Some(1));
    let search = run_search(&SearchOptions {
        workspace_path: fixture.workspace.clone(),
        database_path: Some(fixture.database.clone()),
        index_dir: None,
        query: "tangerine compass".to_owned(),
        limit: 5,
        speed: crate::search::SpeedMode::Instant,
        explain: false,
        as_of: None,
        include_tombstoned: false,
        include_expired: false,
        include_future: false,
        include_stale: false,
        relevance_floor: Some(0.0),
        dedup_mode: SearchDedupMode::DocId,
        source_mode: SearchSourceMode::LexicalOnly,
        strict_source_mode: true,
        memory_scope: crate::models::MemoryScope::Workspace,
        strict_scope: false,
    })?;
    assert!(
        search
            .results
            .iter()
            .any(|hit| hit.doc_id == fixture.memory_id)
    );
    Ok(())
}

#[test]
fn unrepaired_publication_failure_retries_once_per_cadence_without_losing_job_evidence()
-> TestResult {
    let fixture = QueuedIndexFixture::new()?;
    std::fs::write(fixture.index_path(), b"persistent publication obstruction")?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let interval = Duration::from_millis(20);
    let mut last_completion: Option<Instant> = None;
    let mut observed = Vec::new();
    run_scheduler(
        fixture.workspace.to_str().unwrap(),
        Arc::clone(&shutdown),
        interval,
        |options| {
            if let Some(completed) = last_completion {
                assert!(
                    completed.elapsed() >= interval,
                    "a failed publication must not bypass the sleep-after-completion cadence"
                );
            }
            let result = run_daemon_foreground_durable(&selected_index_options(options));
            observed.push(result.clone());
            let job = fixture.job().expect("failed job remains readable");
            assert_eq!(job.status, "failed");
            assert!(job.error_message.is_some());
            assert_eq!(job.documents_indexed, 0);
            if observed.len() == 3 {
                shutdown.store(true, Ordering::Release);
            }
            last_completion = Some(Instant::now());
            result
        },
    );
    let reports = observed.into_iter().collect::<Result<Vec<_>, _>>()?;
    assert_eq!(reports.len(), 3);
    for report in &reports {
        assert_eq!(report.failed, 1);
        assert_eq!(report.succeeded, 0);
        assert_eq!(report.cancelled, 0);
        assert_eq!(report.timed_out, 0);
    }
    assert_index_tick_journal(
        &fixture.workspace,
        &reports,
        &["failed", "failed", "failed"],
    )?;
    assert_eq!(
        std::fs::read(fixture.index_path())?,
        b"persistent publication obstruction"
    );
    assert!(index_jobs_need_processing(&fixture.workspace)?);
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
        select_index_job(true, || panic!(
            "rebuild must not inspect the coalesce queue"
        ))
        .unwrap(),
        Some(JobType::IndexRebuild),
    );
}

#[test]
fn only_pending_queue_work_schedules_coalesce_and_never_an_unrequested_rebuild() {
    assert_eq!(
        select_index_job(false, || Ok(true)).unwrap(),
        Some(JobType::IndexCoalesce)
    );
    assert_eq!(select_index_job(false, || Ok(false)).unwrap(), None);
    assert_eq!(
        select_index_job(false, || Err("queue unavailable".to_owned())).unwrap_err(),
        "queue unavailable",
    );
}
