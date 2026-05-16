//! Criterion benchmark for `ee context --ppr-weight` (bd-ov09.4).
//!
//! Group name: `ee_context_with_ppr`
//!
//! Measures the same 1k-memory fixture with PPR disabled and enabled so the
//! bench job can track the PPR rerank overhead budget separately from the base
//! context-pack SLO.

#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use tempfile::TempDir;

use ee::core::context::{ContextPackOptions, ContextPackOutputOptions, run_context_pack};
use ee::core::index::{IndexRebuildOptions, IndexRebuildStatus, rebuild_index};
use ee::db::{
    CreateMemoryInput, CreateMemoryLinkInput, CreateWorkspaceInput, DbConnection,
    MemoryLinkRelation, MemoryLinkSource,
};
use ee::graph::{CentralityRefreshOptions, CentralityRefreshStatus, refresh_graph_snapshot};
use ee::models::{MemoryScope, WorkspaceId};
use ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS;
use ee::search::SpeedMode;

const BENCH_GROUP_NAME: &str = "ee_context_with_ppr";
const MEMORY_COUNT: usize = 1_000;
#[allow(dead_code)]
const PPR_OVERHEAD_P50_BUDGET_MS: f64 = 30.0;

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn ensure_workspace_row(connection: &DbConnection, workspace_path: &Path) {
    let input = CreateWorkspaceInput {
        path: workspace_path.to_string_lossy().into_owned(),
        name: Some("context-with-ppr-bench".to_owned()),
    };
    connection
        .insert_workspace(&stable_workspace_id(workspace_path), &input)
        .expect("insert benchmark workspace row");
}

fn seed_benchmark_fixture(temp_dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let workspace_path = temp_dir.to_path_buf();
    let db_path = workspace_path.join(".ee").join("ee.db");
    let index_dir = workspace_path.join(".ee").join("index");
    std::fs::create_dir_all(db_path.parent().expect("db parent")).expect("create .ee dir");
    std::fs::write(
        workspace_path.join(".ee").join("config.toml"),
        "[graph.feature.ppr]\nenabled = true\n",
    )
    .expect("enable PPR benchmark feature flag");

    let connection = DbConnection::open_file(&db_path).expect("open benchmark db");
    connection.migrate().expect("migrate benchmark db");
    ensure_workspace_row(&connection, &workspace_path);
    let workspace_id = stable_workspace_id(&workspace_path);

    for index in 0..MEMORY_COUNT {
        let content = if index < 200 {
            format!(
                "PPR context benchmark primary memory {index:04}: structural reranking release seed evidence."
            )
        } else {
            format!(
                "PPR context benchmark background memory {index:04}: linked graph filler outside the query cohort."
            )
        };
        connection
            .insert_memory(
                &format!("mem_ppr_context_bench_{index:08}"),
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content,
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.8,
                    importance: 0.8,
                    provenance_uri: None,
                    trust_class: "agent_assertion".to_owned(),
                    trust_subclass: Some("context-ppr-bench".to_owned()),
                    tags: vec!["bench".to_owned(), "context".to_owned(), "ppr".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("insert benchmark memory");
    }

    for index in 0..(MEMORY_COUNT - 1) {
        connection
            .insert_memory_link(
                &format!("link_{index:026}"),
                &CreateMemoryLinkInput {
                    src_memory_id: format!("mem_ppr_context_bench_{index:08}"),
                    dst_memory_id: format!("mem_ppr_context_bench_{:08}", index + 1),
                    relation: MemoryLinkRelation::Supports,
                    weight: 1.0,
                    confidence: 1.0,
                    directed: true,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: MemoryLinkSource::Agent,
                    created_by: Some("context-with-ppr-bench".to_owned()),
                    metadata_json: None,
                },
            )
            .expect("insert benchmark memory link");
    }

    let centrality = refresh_graph_snapshot(
        &connection,
        &workspace_id,
        &CentralityRefreshOptions::default(),
    )
    .expect("refresh benchmark centrality snapshot");
    assert_eq!(
        centrality.centrality.status,
        CentralityRefreshStatus::Refreshed
    );
    assert!(
        centrality.snapshot.is_some(),
        "benchmark fixture should persist memory_links graph snapshot"
    );

    let rebuild = rebuild_index(&IndexRebuildOptions {
        workspace_path: workspace_path.clone(),
        database_path: Some(db_path.clone()),
        index_dir: Some(index_dir.clone()),
        dry_run: false,
    })
    .expect("rebuild benchmark index");
    assert_eq!(rebuild.status, IndexRebuildStatus::Success);
    assert_eq!(rebuild.memories_indexed as usize, MEMORY_COUNT);

    (workspace_path, db_path, index_dir)
}

fn options(
    workspace_path: &Path,
    db_path: &Path,
    index_dir: &Path,
    ppr_weight: Option<f32>,
) -> ContextPackOptions {
    ContextPackOptions {
        workspace_path: workspace_path.to_path_buf(),
        database_path: Some(db_path.to_path_buf()),
        index_dir: Some(index_dir.to_path_buf()),
        query: "structural reranking release seed".to_owned(),
        speed: SpeedMode::Default,
        filters: Default::default(),
        profile: None,
        max_tokens: Some(4000),
        candidate_pool: Some(200),
        max_results: None,
        include_tombstoned: false,
        as_of: None,
        include_expired: false,
        include_future: false,
        include_stale: false,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        ppr_weight,
        pagination: None,
        coordination_snapshot_path: None,
        coordination_stale_after_ms: DEFAULT_COORDINATION_STALE_AFTER_MS,
        output_options: ContextPackOutputOptions::default(),
    }
}

fn bench_context_with_ppr(c: &mut Criterion) {
    let mut group = c.benchmark_group(BENCH_GROUP_NAME);
    let temp_dir = TempDir::new().expect("temp dir");
    let (workspace_path, db_path, index_dir) = seed_benchmark_fixture(temp_dir.path());

    for (label, ppr_weight) in [
        ("base_1000_memories", None),
        ("ppr_1000_memories", Some(0.5)),
    ] {
        group.bench_with_input(
            BenchmarkId::new("context_pack", label),
            &ppr_weight,
            |b, weight| {
                b.iter(|| {
                    let response =
                        run_context_pack(&options(&workspace_path, &db_path, &index_dir, *weight))
                            .expect("context pack");
                    black_box(response.data.pack.hash);
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_context_with_ppr);
criterion_main!(benches);

#[cfg(test)]
mod tests {
    #[test]
    fn benchmark_group_name_is_canonical() {
        assert_eq!(super::BENCH_GROUP_NAME, "ee_context_with_ppr");
        assert_eq!(super::MEMORY_COUNT, 1_000);
        assert_eq!(super::PPR_OVERHEAD_P50_BUDGET_MS, 30.0);
    }
}
