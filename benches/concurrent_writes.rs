//! Criterion benchmark for concurrent memory writes (J9).
//!
//! Group name: `ee_concurrent_writes`

#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use tempfile::TempDir;

use ee::db::{AuditedMemoryInput, CreateMemoryInput, CreateWorkspaceInput, DbConnection};
use ee::models::WorkspaceId;

const WRITER_COUNT: usize = 4;
const WRITES_PER_WRITER: usize = 8;
const BUDGET_P50_MS: f64 = 120.0;
const BUDGET_P99_MS: f64 = 350.0;
const REGRESSION_THRESHOLD_P50_PCT: f64 = 30.0;
const REGRESSION_THRESHOLD_P99_PCT: f64 = 50.0;

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn prepare_workspace(root: &Path, iteration: u64) -> (PathBuf, String) {
    let workspace_path = root.join(format!("concurrent-writes-{iteration:06}"));
    std::fs::create_dir_all(workspace_path.join(".ee")).expect("create .ee dir");
    let db_path = workspace_path.join(".ee").join("ee.db");
    let connection = DbConnection::open_file(&db_path).expect("open db");
    connection.migrate().expect("migrate db");
    let workspace_id = stable_workspace_id(&workspace_path);
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace_path.to_string_lossy().into_owned(),
                name: Some("concurrent writes benchmark".to_owned()),
            },
        )
        .expect("insert workspace");
    (db_path, workspace_id)
}

fn write_memory_input(
    workspace_id: &str,
    writer_index: usize,
    write_index: usize,
) -> CreateMemoryInput {
    CreateMemoryInput {
        workspace_id: workspace_id.to_owned(),
        level: "episodic".to_owned(),
        kind: "fact".to_owned(),
        content: format!(
            "Concurrent write benchmark memory from writer {writer_index}, write {write_index}."
        ),
        workflow_id: None,
        confidence: 0.7,
        utility: 0.5,
        importance: 0.5,
        provenance_uri: None,
        trust_class: "agent_assertion".to_owned(),
        trust_subclass: Some("concurrent-writes-bench".to_owned()),
        tags: vec!["bench".to_owned(), "concurrent".to_owned()],
        valid_from: None,
        valid_to: None,
    }
}

fn run_concurrent_write_batch(root: &Path, iteration: u64) -> usize {
    let (db_path, workspace_id) = prepare_workspace(root, iteration);
    let barrier = Arc::new(Barrier::new(WRITER_COUNT));
    let handles = (0..WRITER_COUNT)
        .map(|writer_index| {
            let db_path = db_path.clone();
            let workspace_id = workspace_id.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let connection = DbConnection::open_file(&db_path).expect("open writer db");
                barrier.wait();
                for write_index in 0..WRITES_PER_WRITER {
                    let memory_id = format!(
                        "mem_concurrent_bench_{iteration:06}_{writer_index:02}_{write_index:02}"
                    );
                    connection
                        .insert_memory_audited(
                            &memory_id,
                            &AuditedMemoryInput {
                                memory: write_memory_input(
                                    &workspace_id,
                                    writer_index,
                                    write_index,
                                ),
                                actor: Some(format!("bench-writer-{writer_index}")),
                                details: Some(format!(
                                    r#"{{"writer":{writer_index},"write":{write_index}}}"#
                                )),
                            },
                        )
                        .expect("insert audited memory");
                }
            })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        handle.join().expect("writer thread should not panic");
    }

    WRITER_COUNT * WRITES_PER_WRITER
}

fn bench_concurrent_writes(c: &mut Criterion) {
    black_box((
        BUDGET_P50_MS,
        BUDGET_P99_MS,
        REGRESSION_THRESHOLD_P50_PCT,
        REGRESSION_THRESHOLD_P99_PCT,
    ));
    let mut group = c.benchmark_group("ee_concurrent_writes");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    group.bench_function("4_writers_x_8_audited_memories", |bench| {
        let root = TempDir::new().expect("temp dir");
        let mut iteration = 0_u64;
        bench.iter(|| {
            iteration = iteration.saturating_add(1);
            let writes = run_concurrent_write_batch(root.path(), iteration);
            black_box(writes);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_concurrent_writes);
criterion_main!(benches);
