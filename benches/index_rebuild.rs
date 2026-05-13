//! Criterion benchmark for full search-index rebuild latency (J9).
//!
//! Group name: `ee_index_rebuild`

#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use tempfile::TempDir;

use ee::core::index::{IndexRebuildOptions, IndexRebuildStatus, rebuild_index};
use ee::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};
use ee::models::WorkspaceId;

const INDEX_REBUILD_SCALES: &[usize] = &[100, 1_000];
const BUDGET_P50_MS: f64 = 18_000.0;
const BUDGET_P99_MS: f64 = 41_000.0;
const REGRESSION_THRESHOLD_P50_PCT: f64 = 30.0;
const REGRESSION_THRESHOLD_P99_PCT: f64 = 50.0;

struct IndexFixture {
    workspace_path: PathBuf,
    db_path: PathBuf,
}

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn seed_fixture(root: &Path, memory_count: usize) -> IndexFixture {
    let workspace_path = root.join(format!("index-rebuild-{memory_count}"));
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
                name: Some("index rebuild benchmark".to_owned()),
            },
        )
        .expect("insert workspace");

    for index in 0..memory_count {
        connection
            .insert_memory(
                &format!("mem_index_rebuild_bench_{index:010}"),
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "semantic".to_owned(),
                    kind: "fact".to_owned(),
                    content: format!(
                        "Index rebuild benchmark memory {index}: deterministic searchable content."
                    ),
                    workflow_id: None,
                    confidence: 0.75,
                    utility: 0.5,
                    importance: 0.5,
                    provenance_uri: None,
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: Some("index-rebuild-bench".to_owned()),
                    tags: vec!["bench".to_owned(), "index".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("insert memory");
    }

    IndexFixture {
        workspace_path,
        db_path,
    }
}

fn bench_index_rebuild(c: &mut Criterion) {
    black_box((
        BUDGET_P50_MS,
        BUDGET_P99_MS,
        REGRESSION_THRESHOLD_P50_PCT,
        REGRESSION_THRESHOLD_P99_PCT,
    ));
    let temp_dir = TempDir::new().expect("temp dir");
    let fixtures = INDEX_REBUILD_SCALES
        .iter()
        .map(|count| (*count, seed_fixture(temp_dir.path(), *count)))
        .collect::<Vec<_>>();
    let mut group = c.benchmark_group("ee_index_rebuild");

    for (memory_count, fixture) in &fixtures {
        let mut iteration = 0_u64;
        group.bench_with_input(
            BenchmarkId::new("full_rebuild", memory_count),
            fixture,
            |bench, fixture| {
                bench.iter(|| {
                    iteration = iteration.saturating_add(1);
                    let index_dir = fixture
                        .workspace_path
                        .join(".ee")
                        .join(format!("index_bench_{memory_count}_{iteration:06}"));
                    let report = rebuild_index(&IndexRebuildOptions {
                        workspace_path: fixture.workspace_path.clone(),
                        database_path: Some(fixture.db_path.clone()),
                        index_dir: Some(index_dir),
                        dry_run: false,
                    })
                    .expect("rebuild index");
                    assert_eq!(
                        report.status,
                        IndexRebuildStatus::Success,
                        "index rebuild should succeed"
                    );
                    assert_eq!(report.memories_indexed as usize, *memory_count);
                    black_box(report.documents_total);
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_index_rebuild);
criterion_main!(benches);
