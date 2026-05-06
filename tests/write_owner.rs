use std::sync::{
    Arc, Barrier,
    atomic::{AtomicUsize, Ordering},
};
use std::thread;
use std::time::Duration;

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
