//! Criterion benchmark for tiered hot/warm/cold recall.
//!
//! Group name: `ee_tiered_recall`
//!
//! The fixture compares the same corpus with memory-tier admission disabled and
//! enabled. The enabled path must keep required cold evidence explainable while
//! allowing hot/warm candidates to receive deterministic advisory boosts.

#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use ee::core::context::{
    ContextPackOptions, ContextPackOutputOptions, run_context_pack_with_performance,
};
use ee::core::index::{IndexRebuildOptions, IndexRebuildStatus, rebuild_index};
use ee::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};
use ee::models::{MemoryScope, RedactionLevel, WorkspaceId};
use ee::search::SpeedMode;
use tempfile::TempDir;

const TIERED_RECALL_BENCH_GROUP: &str = "ee_tiered_recall";
const TIERED_RECALL_OPERATION: &str = "ee_tiered_recall_context";
const TIERED_RECALL_QUERY: &str = "tiered recall cold required sentinel cache prewarm outage";
const TIERED_RECALL_FILLER_COUNT: usize = 650;
const TIERED_RECALL_MEMORY_COUNT: usize = TIERED_RECALL_FILLER_COUNT + 1;
const TIERED_RECALL_CANDIDATE_POOL: u32 = 192;
const TIERED_RECALL_BUDGET_P50_MS: f64 = 140.0;
const TIERED_RECALL_BUDGET_P99_MS: f64 = 340.0;

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn ensure_workspace_row(connection: &DbConnection, workspace_path: &Path) {
    let workspace_path_string = workspace_path.to_string_lossy().into_owned();
    if connection
        .get_workspace_by_path(&workspace_path_string)
        .expect("query tiered recall benchmark workspace row")
        .is_some()
    {
        return;
    }

    connection
        .insert_workspace(
            &stable_workspace_id(workspace_path),
            &CreateWorkspaceInput {
                path: workspace_path_string,
                name: workspace_path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
            },
        )
        .expect("insert tiered recall benchmark workspace row");
}

fn write_pack_config(workspace_path: &Path, enabled: bool) {
    let config_path = workspace_path.join(".ee").join("config.toml");
    std::fs::write(
        config_path,
        format!("[pack]\nmemory_tier_admission = {enabled}\n"),
    )
    .expect("write tiered recall benchmark config");
}

fn insert_memory(
    connection: &DbConnection,
    workspace_id: &str,
    id: &str,
    kind: &str,
    content: &str,
    score: f32,
) {
    connection
        .insert_memory(
            id,
            &CreateMemoryInput {
                workspace_id: workspace_id.to_owned(),
                level: "semantic".to_owned(),
                kind: kind.to_owned(),
                content: content.to_owned(),
                workflow_id: None,
                confidence: score,
                utility: score,
                importance: score,
                provenance_uri: None,
                trust_class: "human_explicit".to_owned(),
                trust_subclass: Some("tiered-recall-bench".to_owned()),
                tags: vec!["bench".to_owned(), "tiered-recall".to_owned()],
                valid_from: None,
                valid_to: None,
            },
        )
        .expect("insert tiered recall benchmark memory");
}

fn seed_tiered_recall_database(temp_dir: &Path, memory_tier_admission: bool) -> (PathBuf, PathBuf) {
    let workspace_path = temp_dir.to_path_buf();
    let db_path = workspace_path.join(".ee").join("ee.db");
    std::fs::create_dir_all(db_path.parent().expect("db parent")).expect("create .ee dir");
    write_pack_config(&workspace_path, memory_tier_admission);

    let connection = DbConnection::open_file(&db_path).expect("open tiered recall benchmark db");
    connection.migrate().expect("migrate tiered recall db");
    ensure_workspace_row(&connection, &workspace_path);
    let workspace_id = stable_workspace_id(&workspace_path);

    for index in 0..TIERED_RECALL_FILLER_COUNT {
        insert_memory(
            &connection,
            &workspace_id,
            &format!("mem_{:026}", 121_000 + index),
            if index % 3 == 0 { "rule" } else { "fact" },
            &format!(
                "Tiered recall hot warm filler {index:03}: tiered recall cache prewarm \
                 outage evidence keeps the candidate pool above the hot and warm budgets."
            ),
            0.95,
        );
    }
    insert_memory(
        &connection,
        &workspace_id,
        "mem_00000000000000000000121651",
        "failure",
        "Tiered recall COLD REQUIRED SENTINEL cache prewarm outage evidence: cold required failure evidence must remain available and explainable.",
        0.02,
    );
    connection
        .close()
        .expect("close tiered recall benchmark db");

    let index_dir = workspace_path.join(".ee").join("index");
    let report = rebuild_index(&IndexRebuildOptions {
        workspace_path: workspace_path.clone(),
        database_path: Some(db_path),
        index_dir: Some(index_dir.clone()),
        dry_run: false,
    })
    .expect("rebuild tiered recall benchmark index");
    assert_eq!(
        report.status,
        IndexRebuildStatus::Success,
        "tiered recall benchmark index should rebuild successfully"
    );
    assert_eq!(report.memories_indexed, TIERED_RECALL_MEMORY_COUNT as u32);
    (workspace_path, index_dir)
}

fn context_options(workspace_path: &Path, index_dir: &Path) -> ContextPackOptions {
    ContextPackOptions {
        workspace_path: workspace_path.to_path_buf(),
        database_path: Some(workspace_path.join(".ee").join("ee.db")),
        index_dir: Some(index_dir.to_path_buf()),
        query: TIERED_RECALL_QUERY.to_owned(),
        speed: SpeedMode::Default,
        filters: Default::default(),
        profile: None,
        max_tokens: Some(20_000),
        candidate_pool: Some(TIERED_RECALL_CANDIDATE_POOL),
        max_results: None,
        include_tombstoned: false,
        as_of: None,
        include_expired: false,
        include_future: false,
        include_stale: false,
        relevance_floor: None,
        redaction_level: RedactionLevel::Minimal,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        ppr_weight: None,
        changed_symbols: Vec::new(),
        changed_symbols_from_git: false,
        pagination: None,
        coordination_snapshot_path: None,
        coordination_stale_after_ms: ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
        output_options: ContextPackOutputOptions::default(),
    }
}

fn tiered_recall_context_summary(workspace_path: &Path, index_dir: &Path) -> (usize, usize, usize) {
    let run =
        run_context_pack_with_performance(&context_options(workspace_path, index_dir), "context")
            .expect("tiered recall context pack");
    let items = &run.response.data.pack.items;
    let tier_admission_count = items
        .iter()
        .filter(|item| item.why.contains("tierAdmission"))
        .count();
    let cold_recall_count = items
        .iter()
        .filter(|item| item.why.contains("tierAdmission tier=cold"))
        .count();
    let required_cold_count = items
        .iter()
        .filter(|item| {
            item.why.contains("tierAdmission tier=cold")
                && item.why.contains("requiredEvidencePreserved=true")
        })
        .count();
    black_box((
        run.response.data.pack.hash,
        run.response.data.pack.items.len(),
        tier_admission_count,
        cold_recall_count,
        required_cold_count,
        run.performance
            .pointer("/data/timings")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len)
            .unwrap_or_default(),
    ));
    (tier_admission_count, cold_recall_count, required_cold_count)
}

fn bench_tiered_recall(c: &mut Criterion) {
    let mut group = c.benchmark_group(TIERED_RECALL_BENCH_GROUP);
    for (label, enabled) in [
        ("tiering_disabled_baseline", false),
        ("tiering_enabled_hot_cold", true),
    ] {
        let temp_dir = TempDir::new().expect("temp dir");
        let (workspace_path, index_dir) = seed_tiered_recall_database(temp_dir.path(), enabled);
        group.bench_function(BenchmarkId::new(TIERED_RECALL_OPERATION, label), |b| {
            b.iter(|| tiered_recall_context_summary(&workspace_path, &index_dir));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_tiered_recall);
criterion_main!(benches);

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::{
        TIERED_RECALL_BENCH_GROUP, TIERED_RECALL_BUDGET_P50_MS, TIERED_RECALL_BUDGET_P99_MS,
        TIERED_RECALL_CANDIDATE_POOL, TIERED_RECALL_MEMORY_COUNT, TIERED_RECALL_OPERATION,
        context_options, seed_tiered_recall_database, tiered_recall_context_summary,
    };

    #[test]
    fn tiered_recall_benchmark_contract_matches_swarmx_gate() {
        assert_eq!(TIERED_RECALL_BENCH_GROUP, "ee_tiered_recall");
        assert_eq!(TIERED_RECALL_OPERATION, "ee_tiered_recall_context");
        assert!(
            (TIERED_RECALL_CANDIDATE_POOL as usize) < TIERED_RECALL_MEMORY_COUNT,
            "tiered recall proof must not request the whole corpus"
        );
        assert!(
            TIERED_RECALL_CANDIDATE_POOL > 128,
            "bounded pool should still cross the hot tier budget and exercise warm admission"
        );
        assert!(
            TIERED_RECALL_MEMORY_COUNT > 640,
            "default hot+warm budgets are 640, so the fixture must force a cold tier"
        );
        assert!(
            TIERED_RECALL_BUDGET_P50_MS > 0.0
                && TIERED_RECALL_BUDGET_P99_MS >= TIERED_RECALL_BUDGET_P50_MS,
            "tiered recall benchmark budgets must be positive and monotonic"
        );
    }

    #[test]
    fn disabled_fixture_has_no_tier_admission_annotations() {
        let temp_dir = TempDir::new().expect("temp dir");
        let (workspace_path, index_dir) = seed_tiered_recall_database(temp_dir.path(), false);
        let run =
            ee::core::context::run_context_pack(&context_options(&workspace_path, &index_dir))
                .expect("disabled tiered recall fixture context");
        assert!(
            run.data
                .pack
                .items
                .iter()
                .all(|item| !item.why.contains("tierAdmission")),
            "disabled fixture must preserve default context output shape"
        );
    }

    #[test]
    fn enabled_fixture_explains_required_cold_recall() {
        let temp_dir = TempDir::new().expect("temp dir");
        let (workspace_path, index_dir) = seed_tiered_recall_database(temp_dir.path(), true);
        let (tiered_count, cold_count, required_cold_count) =
            tiered_recall_context_summary(&workspace_path, &index_dir);
        assert!(
            tiered_count > 0,
            "enabled fixture must exercise tier admission"
        );
        assert!(cold_count > 0, "enabled fixture must include cold recall");
        assert!(
            required_cold_count > 0,
            "enabled fixture must preserve required cold evidence"
        );
    }
}
