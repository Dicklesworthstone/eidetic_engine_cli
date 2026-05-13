//! Criterion benchmark for audit timeline query latency (J9).
//!
//! Group name: `ee_audit_query`

#![allow(clippy::expect_used)]

use std::path::Path;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use tempfile::TempDir;

use ee::db::{CreateAuditInput, CreateWorkspaceInput, DbConnection};
use ee::models::WorkspaceId;

const AUDIT_ROW_COUNT: usize = 1_000;
const BUDGET_P50_MS: f64 = 35.0;
const BUDGET_P99_MS: f64 = 100.0;
const REGRESSION_THRESHOLD_P50_PCT: f64 = 30.0;
const REGRESSION_THRESHOLD_P99_PCT: f64 = 50.0;

struct AuditFixture {
    connection: DbConnection,
    workspace_id: String,
}

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn seed_audit_fixture(root: &Path) -> AuditFixture {
    let workspace_path = root.join("audit-query-workspace");
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
                name: Some("audit query benchmark".to_owned()),
            },
        )
        .expect("insert workspace");

    for index in 0..AUDIT_ROW_COUNT {
        connection
            .insert_audit(
                &format!("audit_query_bench_{index:010}"),
                &CreateAuditInput {
                    workspace_id: Some(workspace_id.clone()),
                    actor: Some("bench".to_owned()),
                    action: "memory.create".to_owned(),
                    target_type: Some("memory".to_owned()),
                    target_id: Some(format!("mem_audit_query_bench_{index:010}")),
                    details: Some(format!(r#"{{"index":{index},"surface":"audit_query"}}"#)),
                },
            )
            .expect("insert audit row");
    }

    AuditFixture {
        connection,
        workspace_id,
    }
}

fn bench_audit_query(c: &mut Criterion) {
    black_box((
        BUDGET_P50_MS,
        BUDGET_P99_MS,
        REGRESSION_THRESHOLD_P50_PCT,
        REGRESSION_THRESHOLD_P99_PCT,
    ));
    let temp_dir = TempDir::new().expect("temp dir");
    let fixture = seed_audit_fixture(temp_dir.path());
    let mut group = c.benchmark_group("ee_audit_query");

    group.bench_function("timeline_1k_limit_1k", |bench| {
        bench.iter(|| {
            let entries = fixture
                .connection
                .list_audit_entries(Some(&fixture.workspace_id), Some(AUDIT_ROW_COUNT as u32))
                .expect("list audit entries");
            assert_eq!(entries.len(), AUDIT_ROW_COUNT);
            black_box(entries);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_audit_query);
criterion_main!(benches);
