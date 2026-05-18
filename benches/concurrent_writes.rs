//! Criterion benchmark for concurrent memory writes (J9).
//!
//! Group name: `ee_concurrent_writes`

#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use tempfile::TempDir;

use ee::core::audit_lane::{
    AuditEnqueueResult, AuditEvent, AuditLane, AuditLaneConfig, insert_audit_event_batch,
};
use ee::db::{AuditedMemoryInput, CreateMemoryInput, CreateWorkspaceInput, DbConnection};
use ee::models::WorkspaceId;

const WRITER_COUNT: usize = 4;
const WRITES_PER_WRITER: usize = 8;
const BUDGET_P50_MS: f64 = 120.0;
const BUDGET_P99_MS: f64 = 350.0;
const REGRESSION_THRESHOLD_P50_PCT: f64 = 30.0;
const REGRESSION_THRESHOLD_P99_PCT: f64 = 50.0;
const AUDIT_LANE_PRODUCER_COUNT: usize = 64;
const AUDIT_LANE_EVENTS_PER_PRODUCER: usize = 2;
const AUDIT_LANE_EVENT_COUNT: usize = AUDIT_LANE_PRODUCER_COUNT * AUDIT_LANE_EVENTS_PER_PRODUCER;
const AUDIT_LANE_ENQUEUE_BUDGET_P50_MS: f64 = 0.5;
const AUDIT_LANE_ENQUEUE_BUDGET_P99_MS: f64 = 2.0;
const AUDIT_LANE_BATCH_COMMIT_BUDGET_P50_MS: f64 = 20.0;
const AUDIT_LANE_BATCH_COMMIT_BUDGET_P99_MS: f64 = 100.0;

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

fn audit_lane_event(
    workspace_id: &str,
    producer_index: usize,
    event_index: usize,
    seq: u64,
) -> AuditEvent {
    let input = ee::db::CreateAuditInput {
        workspace_id: Some(workspace_id.to_owned()),
        actor: Some(format!("audit-lane-bench-producer-{producer_index}")),
        action: "memory.create".to_owned(),
        target_type: Some("memory".to_owned()),
        target_id: Some(format!("mem_audit_lane_bench_{seq:014}")),
        details: Some(format!(
            r#"{{"producer":{producer_index},"event":{event_index}}}"#
        )),
    };
    AuditEvent::from_audit_input(format!("audit_{seq:032x}"), seq, &input)
}

fn audit_lane_producer_events(workspace_id: &str) -> Vec<Vec<AuditEvent>> {
    (0..AUDIT_LANE_PRODUCER_COUNT)
        .map(|producer_index| {
            (0..AUDIT_LANE_EVENTS_PER_PRODUCER)
                .map(|event_index| {
                    let seq =
                        (producer_index * AUDIT_LANE_EVENTS_PER_PRODUCER + event_index + 1) as u64;
                    audit_lane_event(workspace_id, producer_index, event_index, seq)
                })
                .collect()
        })
        .collect()
}

fn run_audit_lane_enqueue_drain(producer_events: Vec<Vec<AuditEvent>>) -> usize {
    let config = AuditLaneConfig {
        capacity: AUDIT_LANE_EVENT_COUNT,
        batch_size: 64,
        shutdown_event_limit: AUDIT_LANE_EVENT_COUNT,
    };
    let (handle, mut lane) = AuditLane::new(config);
    let barrier = Arc::new(Barrier::new(AUDIT_LANE_PRODUCER_COUNT));
    let handles = producer_events
        .into_iter()
        .enumerate()
        .map(|(producer_index, events)| {
            let producer_handle = handle.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                for event in events {
                    match producer_handle.enqueue(event) {
                        AuditEnqueueResult::Enqueued { .. } => {}
                        other => {
                            panic!("audit lane producer {producer_index} enqueue failed: {other:?}")
                        }
                    }
                }
            })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        handle.join().expect("audit lane producer should not panic");
    }

    let mut drained = Vec::with_capacity(AUDIT_LANE_EVENT_COUNT);
    let report = lane.shutdown_drain(|batch| drained.extend_from_slice(batch));
    assert_eq!(report.drained_events, AUDIT_LANE_EVENT_COUNT as u64);
    assert_eq!(report.pending_events, 0);
    assert!(report.degraded_codes.is_empty());
    drained.len()
}

fn prepare_audit_lane_batch_commit(root: &Path, iteration: u64) -> (DbConnection, Vec<AuditEvent>) {
    let workspace_path = root.join(format!("audit-lane-batch-{iteration:06}"));
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
                name: Some("audit lane batch commit benchmark".to_owned()),
            },
        )
        .expect("insert workspace");

    let events = audit_lane_producer_events(&workspace_id)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    (connection, events)
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

fn bench_audit_lane(c: &mut Criterion) {
    black_box((
        AUDIT_LANE_ENQUEUE_BUDGET_P50_MS,
        AUDIT_LANE_ENQUEUE_BUDGET_P99_MS,
        AUDIT_LANE_BATCH_COMMIT_BUDGET_P50_MS,
        AUDIT_LANE_BATCH_COMMIT_BUDGET_P99_MS,
        REGRESSION_THRESHOLD_P50_PCT,
        REGRESSION_THRESHOLD_P99_PCT,
    ));
    let mut group = c.benchmark_group("ee_audit_lane");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    group.bench_function("64_producers_x_2_enqueue_and_drain", |bench| {
        bench.iter_batched(
            || audit_lane_producer_events("wsp_audit_lane_bench"),
            |events| black_box(run_audit_lane_enqueue_drain(events)),
            BatchSize::SmallInput,
        );
    });

    group.bench_function("128_event_batch_commit", |bench| {
        let root = TempDir::new().expect("temp dir");
        let mut iteration = 0_u64;
        bench.iter_custom(|iterations| {
            let mut elapsed = Duration::ZERO;
            for _ in 0..iterations {
                iteration = iteration.saturating_add(1);
                let (connection, events) = prepare_audit_lane_batch_commit(root.path(), iteration);
                let start = Instant::now();
                insert_audit_event_batch(&connection, &events).expect("insert audit event batch");
                elapsed += start.elapsed();
                let committed = events.len();
                connection.close().expect("close db");
                black_box(committed);
            }
            elapsed
        });
    });

    group.finish();
}

criterion_group!(benches, bench_concurrent_writes, bench_audit_lane);
criterion_main!(benches);
