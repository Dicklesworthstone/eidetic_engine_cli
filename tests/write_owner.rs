use std::sync::{
    Arc, Barrier, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::thread;
use std::time::Duration;

use ee::core::write_owner::{
    WriteSpool, WriteSpoolConfig, WriteSpoolIntent, WriteSpoolIntentKind, WriteSpoolRecordStatus,
};
use ee::db::{CreateWorkspaceInput, DbConnection};

type TestResult = Result<(), String>;

#[test]
fn file_backed_two_writers_are_serialized() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let database_path = tempdir.path().join("write-owner.db");
    let setup = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    setup.migrate().map_err(|error| error.to_string())?;
    setup.close().map_err(|error| error.to_string())?;

    let barrier = Arc::new(Barrier::new(2));
    let active_writers = Arc::new(AtomicUsize::new(0));
    let max_active_writers = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();

    for index in 0..2 {
        let database_path = database_path.clone();
        let barrier = Arc::clone(&barrier);
        let active_writers = Arc::clone(&active_writers);
        let max_active_writers = Arc::clone(&max_active_writers);

        handles.push(thread::spawn(move || -> TestResult {
            let connection =
                DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
            let workspace_id = format!("wsp_writeowner{index:016}");
            let input = CreateWorkspaceInput {
                path: format!("/tmp/write-owner-{index}"),
                name: Some(format!("Write Owner {index}")),
            };

            barrier.wait();
            connection
                .with_transaction(|| {
                    let active = active_writers.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active_writers.fetch_max(active, Ordering::SeqCst);

                    let result = connection.insert_workspace(&workspace_id, &input);
                    thread::sleep(Duration::from_millis(25));
                    active_writers.fetch_sub(1, Ordering::SeqCst);
                    result
                })
                .map_err(|error| error.to_string())?;
            connection.close().map_err(|error| error.to_string())
        }));
    }

    for handle in handles {
        handle
            .join()
            .map_err(|_| "writer thread panicked".to_string())??;
    }

    if max_active_writers.load(Ordering::SeqCst) != 1 {
        return Err("write owner allowed overlapping durable writers".to_string());
    }

    let check = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
    for index in 0..2 {
        let workspace_id = format!("wsp_writeowner{index:016}");
        let stored = check
            .get_workspace(&workspace_id)
            .map_err(|error| error.to_string())?;
        if stored.is_none() {
            return Err(format!("missing serialized workspace {workspace_id}"));
        }
    }
    check.close().map_err(|error| error.to_string())
}

#[test]
fn writer_spool_simulated_swarm_load_batches_all_agent_writes() -> TestResult {
    const AGENTS: usize = 8;
    const WRITES_PER_AGENT: usize = 16;
    const TOTAL_WRITES: usize = AGENTS * WRITES_PER_AGENT;

    let spool = Arc::new(Mutex::new(WriteSpool::new(
        WriteSpoolConfig::new(TOTAL_WRITES + 1, 8, 1024 * 1024, 30_000),
        0,
    )));
    let barrier = Arc::new(Barrier::new(AGENTS));
    let mut handles = Vec::new();

    for agent in 0..AGENTS {
        let spool = Arc::clone(&spool);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || -> TestResult {
            barrier.wait();
            for write in 0..WRITES_PER_AGENT {
                let idempotency_key = format!("agent-{agent}-write-{write}");
                let mut guard = spool
                    .lock()
                    .map_err(|_| "write spool mutex poisoned".to_string())?;
                guard
                    .enqueue(
                        WriteSpoolIntent::new(
                            WriteSpoolIntentKind::Remember,
                            "workspace",
                            idempotency_key,
                            128,
                        ),
                        (agent * WRITES_PER_AGENT + write) as u64,
                    )
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        }));
    }

    for handle in handles {
        handle
            .join()
            .map_err(|_| "writer thread panicked".to_string())??;
    }

    let mut guard = spool
        .lock()
        .map_err(|_| "write spool mutex poisoned".to_string())?;
    assert_eq!(guard.status(1_000).queue_depth, TOTAL_WRITES);

    let mut batches = 0usize;
    while let Some(batch) = guard.next_batch() {
        if batch.row_count() > 8 {
            return Err(format!(
                "batch exceeded configured size: {}",
                batch.row_count()
            ));
        }
        guard.mark_batch_committed(batch.batch_id, 2_000);
        batches += 1;
    }

    let status = guard.status(2_000);
    assert_eq!(status.queue_depth, 0);
    assert_eq!(status.total_enqueued, TOTAL_WRITES as u64);
    assert_eq!(status.total_committed, TOTAL_WRITES as u64);
    assert!(
        batches < TOTAL_WRITES,
        "writes should coalesce into batches"
    );
    assert_eq!(status.max_batch_size_observed, 8);
    assert!(
        guard
            .recovery_records()
            .iter()
            .all(|record| record.status == WriteSpoolRecordStatus::Committed),
        "all simulated swarm writes must recover as committed"
    );

    Ok(())
}
