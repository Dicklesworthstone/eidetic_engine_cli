//! Cross-feature graph context benchmark (bd-bife.16).
//!
//! Group name: `graph_full_stack`
//!
//! The workload seeds a 1k-memory graph workspace, enables all ten graph
//! feature flags, refreshes the memory-links snapshot, and runs 100 varied
//! context requests. `EE_BENCH_COMPARE_ONLY=1` executes the budget gate without
//! Criterion sampling so CI can fail fast on regressions.

#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use ee::core::context::{
    ContextPackOptions, ContextPackOutputOptions, run_context_pack_with_performance,
};
use ee::core::index::{IndexRebuildOptions, IndexRebuildStatus, rebuild_index};
use ee::core::search::SearchSourceMode;
use ee::db::{
    CreateMemoryInput, CreateMemoryLinkInput, CreateWorkspaceInput, DbConnection,
    MemoryLinkRelation, MemoryLinkSource,
};
use ee::graph::{CentralityRefreshOptions, CentralityRefreshStatus, refresh_graph_snapshot};
use ee::models::{MemoryScope, RedactionLevel, WorkspaceId};
use ee::pack::{DEFAULT_COORDINATION_STALE_AFTER_MS, PackResourceProfile};
use ee::search::SpeedMode;
use tempfile::TempDir;

const BENCH_GROUP_NAME: &str = "graph_full_stack";
const WORKSPACE_MEMORY_COUNT: usize = 1_000;
const WORKLOAD_QUERY_COUNT: usize = 100;
const CACHE_ON_P50_BUDGET_MS: f64 = 100.0;
const CACHE_ON_P99_BUDGET_MS: f64 = 500.0;
const CACHE_OFF_P50_BUDGET_MS: f64 = 350.0;
const CACHE_OFF_P99_BUDGET_MS: f64 = 1_000.0;
#[cfg(test)]
const DELIBERATE_REGRESSION_MS: f64 = 100.0;

const ALL_GRAPH_FEATURES: &[&str] = &[
    "ppr",
    "pack_dna",
    "causal_explain",
    "structural_health",
    "structural_decay",
    "proximity",
    "revision_dominance",
    "skyline",
    "load_bearing",
    "hits_profiles",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CacheMode {
    CacheOn,
    CacheOff,
}

impl CacheMode {
    const fn label(self) -> &'static str {
        match self {
            Self::CacheOn => "cache_on",
            Self::CacheOff => "cache_off",
        }
    }

    const fn p50_budget_ms(self) -> f64 {
        match self {
            Self::CacheOn => CACHE_ON_P50_BUDGET_MS,
            Self::CacheOff => CACHE_OFF_P50_BUDGET_MS,
        }
    }

    const fn p99_budget_ms(self) -> f64 {
        match self {
            Self::CacheOn => CACHE_ON_P99_BUDGET_MS,
            Self::CacheOff => CACHE_OFF_P99_BUDGET_MS,
        }
    }

    const fn cache_json_response(self) -> bool {
        matches!(self, Self::CacheOn)
    }

    const fn warm_first(self) -> bool {
        matches!(self, Self::CacheOn)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WorkloadStats {
    p50_ms: f64,
    p99_ms: f64,
}

#[derive(Debug)]
struct BenchFixture {
    _temp_dir: TempDir,
    workspace_path: PathBuf,
    db_path: PathBuf,
    index_dir: PathBuf,
}

fn graph_feature_config() -> String {
    let mut config = String::from("[graph.ppr]\nalpha = 0.50\n\n");
    for feature in ALL_GRAPH_FEATURES {
        config.push_str("[graph.feature.");
        config.push_str(feature);
        config.push_str("]\nenabled = true\n\n");
    }
    config
}

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn ensure_workspace_row(connection: &DbConnection, workspace_path: &Path) -> String {
    let workspace_id = stable_workspace_id(workspace_path);
    let input = CreateWorkspaceInput {
        path: workspace_path.to_string_lossy().into_owned(),
        name: Some("graph-full-stack-bench".to_owned()),
    };
    connection
        .insert_workspace(&workspace_id, &input)
        .expect("insert benchmark workspace row");
    workspace_id
}

fn seed_fixture() -> BenchFixture {
    let temp_dir = TempDir::new().expect("temp dir");
    let workspace_path = temp_dir.path().to_path_buf();
    let ee_dir = workspace_path.join(".ee");
    let db_path = ee_dir.join("ee.db");
    let index_dir = ee_dir.join("index");
    std::fs::create_dir_all(&ee_dir).expect("create .ee dir");
    std::fs::write(ee_dir.join("config.toml"), graph_feature_config())
        .expect("write graph feature benchmark config");

    let connection = DbConnection::open_file(&db_path).expect("open benchmark db");
    connection.migrate().expect("migrate benchmark db");
    let workspace_id = ensure_workspace_row(&connection, &workspace_path);
    let topics = [
        "release",
        "retrieval",
        "graph",
        "curation",
        "causal",
        "dominance",
        "skyline",
        "proximity",
    ];

    for index in 0..WORKSPACE_MEMORY_COUNT {
        let topic = topics[index % topics.len()];
        connection
            .insert_memory(
                &memory_id(index),
                &CreateMemoryInput {
                    workspace_id: workspace_id.clone(),
                    level: "semantic".to_owned(),
                    kind: "note".to_owned(),
                    content: format!(
                        "Graph full-stack benchmark memory {index:04}: {topic} evidence for context packing, structural signals, and cache behavior."
                    ),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.8,
                    importance: 0.8,
                    provenance_uri: None,
                    trust_class: "agent_assertion".to_owned(),
                    trust_subclass: Some("graph-full-stack-bench".to_owned()),
                    tags: vec!["bench".to_owned(), "graph-full-stack".to_owned(), topic.to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("insert benchmark memory");
    }

    for index in 0..WORKSPACE_MEMORY_COUNT {
        insert_link(
            &connection,
            index,
            (index + 1) % WORKSPACE_MEMORY_COUNT,
            "supports",
        );
        if index % 5 == 0 {
            insert_link(
                &connection,
                index,
                (index + 17) % WORKSPACE_MEMORY_COUNT,
                "related",
            );
        }
    }

    let refresh = refresh_graph_snapshot(
        &connection,
        &workspace_id,
        &CentralityRefreshOptions::default(),
    )
    .expect("refresh full-stack graph snapshot");
    assert_eq!(
        refresh.centrality.status,
        CentralityRefreshStatus::Refreshed
    );
    assert!(
        refresh.snapshot.is_some(),
        "full-stack fixture should persist a fresh memory-links snapshot"
    );

    let rebuild = rebuild_index(&IndexRebuildOptions {
        workspace_path: workspace_path.clone(),
        database_path: Some(db_path.clone()),
        index_dir: Some(index_dir.clone()),
        dry_run: false,
    })
    .expect("rebuild benchmark index");
    assert_eq!(rebuild.status, IndexRebuildStatus::Success);
    assert_eq!(rebuild.memories_indexed as usize, WORKSPACE_MEMORY_COUNT);

    BenchFixture {
        _temp_dir: temp_dir,
        workspace_path,
        db_path,
        index_dir,
    }
}

fn memory_id(index: usize) -> String {
    format!("mem_graph_full_stack_{index:08}")
}

fn insert_link(connection: &DbConnection, src: usize, dst: usize, relation: &str) {
    let relation = match relation {
        "supports" => MemoryLinkRelation::Supports,
        _ => MemoryLinkRelation::Related,
    };
    connection
        .insert_memory_link(
            &format!("link_graph_full_stack_{src:08}_{dst:08}"),
            &CreateMemoryLinkInput {
                src_memory_id: memory_id(src),
                dst_memory_id: memory_id(dst),
                relation,
                weight: 1.0,
                confidence: 1.0,
                directed: true,
                evidence_count: 1,
                last_reinforced_at: None,
                source: MemoryLinkSource::Agent,
                created_by: Some("graph-full-stack-bench".to_owned()),
                metadata_json: None,
            },
        )
        .expect("insert benchmark memory link");
}

fn workload_queries() -> Vec<String> {
    let stems = [
        "release graph skyline",
        "retrieval proximity evidence",
        "curation structural decay",
        "causal explanation flow",
        "dominance frontier edit impact",
        "load-bearing provenance",
        "hits authority grounding",
        "pack dna community",
        "ppr rerank seed",
        "gomory hu proximity",
    ];
    (0..WORKLOAD_QUERY_COUNT)
        .map(|index| format!("{} workload {index:03}", stems[index % stems.len()]))
        .collect()
}

fn context_options(fixture: &BenchFixture, query: &str, mode: CacheMode) -> ContextPackOptions {
    ContextPackOptions {
        workspace_path: fixture.workspace_path.clone(),
        database_path: Some(fixture.db_path.clone()),
        index_dir: Some(fixture.index_dir.clone()),
        query: query.to_owned(),
        speed: SpeedMode::Default,
        source_mode: SearchSourceMode::Hybrid,
        strict_source_mode: false,
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
        relevance_floor: None,
        redaction_level: RedactionLevel::Minimal,
        memory_scope: MemoryScope::Swarm,
        strict_scope: false,
        ppr_weight: Some(0.5),
        changed_symbols: Vec::new(),
        changed_symbols_from_git: false,
        pagination: None,
        coordination_snapshot_path: None,
        coordination_stale_after_ms: DEFAULT_COORDINATION_STALE_AFTER_MS,
        output_options: ContextPackOutputOptions::default()
            .with_resource_profile(PackResourceProfile::SwarmHeavy)
            .with_cache_json_response(mode.cache_json_response()),
        persist_pack: true,
    }
}

fn timing_ms(performance: &serde_json::Value, name: &str) -> Option<f64> {
    performance["data"]["timings"]
        .as_array()?
        .iter()
        .find(|timing| timing["name"].as_str() == Some(name))?
        .get("elapsedMs")?
        .as_f64()
}

fn run_one_context(fixture: &BenchFixture, query: &str, mode: CacheMode) -> f64 {
    let started = Instant::now();
    let run = run_context_pack_with_performance(&context_options(fixture, query, mode), "context")
        .expect("full-stack context pack");
    black_box((
        &run.response.data.pack.hash,
        run.response.data.pack.items.len(),
        run.response.data.degraded.len(),
    ));
    timing_ms(&run.performance, "total").unwrap_or_else(|| started.elapsed().as_secs_f64() * 1000.0)
}

fn percentile(sorted_samples: &[f64], percentile: f64) -> f64 {
    let last_index = sorted_samples.len() - 1;
    sorted_samples[(percentile * last_index as f64).round() as usize]
}

fn run_workload(fixture: &BenchFixture, mode: CacheMode) -> WorkloadStats {
    let queries = workload_queries();
    if mode.warm_first() {
        for query in &queries {
            let _ = run_one_context(fixture, query, mode);
        }
    }

    let mut samples = Vec::with_capacity(queries.len());
    for query in &queries {
        samples.push(run_one_context(fixture, query, mode));
    }
    samples.sort_by(|left, right| left.total_cmp(right));
    WorkloadStats {
        p50_ms: percentile(&samples, 0.50),
        p99_ms: percentile(&samples, 0.99),
    }
}

fn budget_error(mode: CacheMode, stats: WorkloadStats) -> Option<String> {
    if stats.p50_ms > mode.p50_budget_ms() {
        return Some(format!(
            "{} p50 exceeded: {:.3}ms > {:.3}ms",
            mode.label(),
            stats.p50_ms,
            mode.p50_budget_ms()
        ));
    }
    if stats.p99_ms > mode.p99_budget_ms() {
        return Some(format!(
            "{} p99 exceeded: {:.3}ms > {:.3}ms",
            mode.label(),
            stats.p99_ms,
            mode.p99_budget_ms()
        ));
    }
    None
}

fn assert_workload_budget(mode: CacheMode, stats: WorkloadStats) {
    if let Some(error) = budget_error(mode, stats) {
        panic!("{error}");
    }
}

#[cfg(test)]
fn deliberate_regression_is_rejected(mode: CacheMode) -> bool {
    let stats = WorkloadStats {
        p50_ms: mode.p50_budget_ms() + DELIBERATE_REGRESSION_MS,
        p99_ms: mode.p99_budget_ms(),
    };
    budget_error(mode, stats).is_some()
}

fn compare_only_mode_enabled() -> bool {
    std::env::var("EE_BENCH_COMPARE_ONLY")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn bench_graph_full_stack(c: &mut Criterion) {
    if compare_only_mode_enabled() {
        let fixture = seed_fixture();
        for mode in [CacheMode::CacheOn, CacheMode::CacheOff] {
            let stats = run_workload(&fixture, mode);
            println!(
                "graph_full_stack_budget mode={} query_count={} memory_count={} p50_ms={:.3} p99_ms={:.3} p50_budget_ms={:.3} p99_budget_ms={:.3}",
                mode.label(),
                WORKLOAD_QUERY_COUNT,
                WORKSPACE_MEMORY_COUNT,
                stats.p50_ms,
                stats.p99_ms,
                mode.p50_budget_ms(),
                mode.p99_budget_ms(),
            );
            assert_workload_budget(mode, stats);
        }
        return;
    }

    let fixture = seed_fixture();
    let mut group = c.benchmark_group(BENCH_GROUP_NAME);
    group.sample_size(10);
    for mode in [CacheMode::CacheOn, CacheMode::CacheOff] {
        group.bench_with_input(
            BenchmarkId::new("context_100_queries", mode.label()),
            &mode,
            |b, mode| {
                b.iter(|| black_box(run_workload(&fixture, *mode)));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_graph_full_stack);
criterion_main!(benches);

#[cfg(test)]
mod tests {
    #[test]
    fn benchmark_contract_names_all_ten_graph_features() {
        assert_eq!(super::BENCH_GROUP_NAME, "graph_full_stack");
        assert_eq!(super::WORKSPACE_MEMORY_COUNT, 1_000);
        assert_eq!(super::WORKLOAD_QUERY_COUNT, 100);
        assert_eq!(super::ALL_GRAPH_FEATURES.len(), 10);
        assert!(super::ALL_GRAPH_FEATURES.contains(&"ppr"));
        assert!(super::ALL_GRAPH_FEATURES.contains(&"hits_profiles"));
    }

    #[test]
    fn cache_mode_budgets_match_bead_acceptance() {
        assert_eq!(
            super::CacheMode::CacheOn.p50_budget_ms(),
            super::CACHE_ON_P50_BUDGET_MS
        );
        assert_eq!(
            super::CacheMode::CacheOn.p99_budget_ms(),
            super::CACHE_ON_P99_BUDGET_MS
        );
        assert_eq!(
            super::CacheMode::CacheOff.p50_budget_ms(),
            super::CACHE_OFF_P50_BUDGET_MS
        );
        assert_eq!(
            super::CacheMode::CacheOff.p99_budget_ms(),
            super::CACHE_OFF_P99_BUDGET_MS
        );
    }

    #[test]
    fn deliberate_100ms_regression_is_rejected() {
        assert!(super::deliberate_regression_is_rejected(
            super::CacheMode::CacheOn
        ));
        assert!(super::deliberate_regression_is_rejected(
            super::CacheMode::CacheOff
        ));
    }

    #[test]
    fn workload_queries_are_varied_and_pinned() {
        let queries = super::workload_queries();
        assert_eq!(queries.len(), super::WORKLOAD_QUERY_COUNT);
        assert_ne!(queries[0], queries[1]);
        assert!(queries[0].contains("workload 000"));
        assert!(queries[99].contains("workload 099"));
    }
}
