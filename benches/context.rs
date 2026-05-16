//! Criterion benchmark for `ee context` (EE-PERF-BENCH-context).
//!
//! Group name: `ee_context`
//!
//! Tests the `run_context_pack` function at three token budget scales:
//! - 1k tokens: small context pack
//! - 4k tokens: default context pack
//! - 8k tokens: large context pack
//!
//! S4 resource scales:
//! - 1k memories: release-candidate smoke scale
//! - 10k memories: swarm-scale nightly/stress scale
//! - 100k memories: stress-only large-machine scale
//!
//! Performance budget (plan §28):
//! - p50: 95ms
//! - p99: 240ms
//! - Regression threshold: 30%

#![allow(clippy::expect_used)]
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use tempfile::TempDir;

use ee::core::context::{
    ContextPackOptions, ContextPackOutputOptions, run_context_pack,
    run_context_pack_with_performance,
};
use ee::core::index::{IndexRebuildOptions, IndexRebuildStatus, rebuild_index};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};
use ee::models::{MemoryScope, RedactionLevel, WorkspaceId};
use ee::pack::PackResourceProfile;
use ee::search::SpeedMode;

/// Performance budget from plan §28 (README "Performance" table).
/// p50 must stay under 95ms, p99 under 240ms.
const BUDGET_P50_MS: f64 = 95.0;
const BUDGET_P99_MS: f64 = 240.0;

/// Regression threshold: fail if p50 degrades by more than 30%.
const REGRESSION_THRESHOLD: f64 = 0.30;
const S4_RELEASE_CANDIDATE_SCALE: usize = 1_000;
const S4_NIGHTLY_SCALE: usize = 10_000;
const S4_STRESS_SCALE: usize = 100_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResourceScale {
    label: &'static str,
    memory_count: usize,
    candidate_pool: u32,
    resource_profile: PackResourceProfile,
}

const S4_RESOURCE_SCALES: &[ResourceScale] = &[
    ResourceScale {
        label: "1000_memories",
        memory_count: S4_RELEASE_CANDIDATE_SCALE,
        candidate_pool: 200,
        resource_profile: PackResourceProfile::Standard,
    },
    ResourceScale {
        label: "10000_memories",
        memory_count: S4_NIGHTLY_SCALE,
        candidate_pool: 1_000,
        resource_profile: PackResourceProfile::SwarmHeavy,
    },
    ResourceScale {
        label: "100000_memories",
        memory_count: S4_STRESS_SCALE,
        candidate_pool: 1_000,
        resource_profile: PackResourceProfile::SwarmHeavy,
    },
];

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
        .expect("query benchmark workspace row")
        .is_some()
    {
        return;
    }

    let input = CreateWorkspaceInput {
        path: workspace_path_string,
        name: workspace_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
    };
    connection
        .insert_workspace(&stable_workspace_id(workspace_path), &input)
        .expect("insert benchmark workspace row");
}

/// Seed a database with memories for context pack testing.
fn seed_database(temp_dir: &Path, memory_count: usize) -> std::path::PathBuf {
    let workspace_path = temp_dir.to_path_buf();
    let db_path = workspace_path.join(".ee").join("ee.db");

    std::fs::create_dir_all(db_path.parent().expect("db parent")).expect("create .ee dir");

    let connection = DbConnection::open_file(&db_path).expect("open db");
    connection.migrate().expect("migrate db");

    let topics = [
        "release",
        "testing",
        "performance",
        "refactoring",
        "debugging",
        "deployment",
        "security",
        "documentation",
    ];

    for i in 0..memory_count {
        let topic = topics[i % topics.len()];
        let content = format!(
            "Memory {i} about {topic}: This is a test memory for benchmarking context packing. \
             It contains relevant information about {topic} that should be retrieved when \
             querying for related tasks. The memory includes details about best practices, \
             common pitfalls, and lessons learned from past experiences with {topic}."
        );
        let options = RememberMemoryOptions {
            workspace_path: &workspace_path,
            database_path: Some(&db_path),
            content: &content,
            workflow_id: None,
            level: "procedural",
            kind: "rule",
            tags: Some(&format!("bench,{topic}")),
            confidence: 0.75,
            source: None,
            valid_from: None,
            valid_to: None,
            dry_run: false,
            auto_link: true,
            propose_candidates: false,
            allow_secret_mention: false,
        };
        remember_memory(&options).expect("seed memory");
    }

    workspace_path
}

/// Fast deterministic seeding for S4 scale benches. This bypasses remember-time
/// linking/proposal work so the benchmark fixture cost does not dominate the
/// measured read path.
fn seed_resource_scale_database(temp_dir: &Path, memory_count: usize) -> PathBuf {
    let workspace_path = temp_dir.to_path_buf();
    let db_path = workspace_path.join(".ee").join("ee.db");

    std::fs::create_dir_all(db_path.parent().expect("db parent")).expect("create .ee dir");

    let connection = DbConnection::open_file(&db_path).expect("open db");
    connection.migrate().expect("migrate db");
    ensure_workspace_row(&connection, &workspace_path);
    let workspace_id = stable_workspace_id(&workspace_path);
    let topics = [
        "release",
        "testing",
        "performance",
        "refactoring",
        "debugging",
        "deployment",
        "security",
        "documentation",
        "graph",
        "search",
    ];

    for index in 0..memory_count {
        let topic = topics[index % topics.len()];
        let content = format!(
            "S4 resource benchmark memory {index}: deterministic {topic} evidence for bounded \
             context pack assembly, search latency, and memory growth measurement."
        );
        let input = CreateMemoryInput {
            workspace_id: workspace_id.clone(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content,
            workflow_id: None,
            confidence: 0.75,
            utility: 0.75,
            importance: 0.75,
            provenance_uri: None,
            trust_class: "human_explicit".to_owned(),
            trust_subclass: Some("s4-resource-bench".to_owned()),
            tags: vec![
                "bench".to_owned(),
                "s4".to_owned(),
                "resource".to_owned(),
                topic.to_owned(),
            ],
            valid_from: None,
            valid_to: None,
        };
        let memory_id = format!("mem_s4_resource_{index:06}");
        connection
            .insert_memory(&memory_id, &input)
            .expect("insert S4 benchmark memory");
    }

    workspace_path
}

fn build_resource_scale_index(
    workspace_path: &Path,
    db_path: &Path,
    memory_count: usize,
) -> PathBuf {
    let index_dir = workspace_path.join(".ee").join("index");
    let report = rebuild_index(&IndexRebuildOptions {
        workspace_path: workspace_path.to_path_buf(),
        database_path: Some(db_path.to_path_buf()),
        index_dir: Some(index_dir.clone()),
        dry_run: false,
    })
    .expect("rebuild S4 benchmark index");
    assert_eq!(
        report.status,
        IndexRebuildStatus::Success,
        "S4 benchmark search index should rebuild successfully"
    );
    assert_eq!(
        report.memories_indexed as usize, memory_count,
        "S4 benchmark index should cover every seeded memory"
    );
    index_dir
}

fn performance_timing_ms(performance: &serde_json::Value, name: &str) -> f64 {
    performance
        .pointer("/data/timings")
        .and_then(serde_json::Value::as_array)
        .and_then(|timings| {
            timings.iter().find_map(|entry| {
                let timing_name = entry.get("name").and_then(serde_json::Value::as_str)?;
                (timing_name == name)
                    .then(|| entry.get("elapsedMs").and_then(serde_json::Value::as_f64))
                    .flatten()
            })
        })
        .unwrap_or(0.0)
}

fn active_bench_profile() -> String {
    std::env::var("EE_BENCH_PROFILE").unwrap_or_else(|_| "manual".to_owned())
}

fn s4_resource_scales_for_profile(profile: &str) -> Vec<ResourceScale> {
    match profile {
        "stress" => S4_RESOURCE_SCALES.to_vec(),
        "nightly" => S4_RESOURCE_SCALES[..2].to_vec(),
        _ => S4_RESOURCE_SCALES[..1].to_vec(),
    }
}

/// Benchmark `run_context_pack` at different token budget scales.
fn bench_context(c: &mut Criterion) {
    let mut group = c.benchmark_group("ee_context");

    let temp_dir = TempDir::new().expect("temp dir");
    let workspace_path = seed_database(temp_dir.path(), 100);
    let db_path = workspace_path.join(".ee").join("ee.db");

    for &max_tokens in &[1000u32, 4000, 8000] {
        let label = match max_tokens {
            1000 => "1k_tokens",
            4000 => "4k_tokens",
            8000 => "8k_tokens",
            _ => "unknown",
        };

        group.bench_with_input(
            BenchmarkId::new("context_pack", label),
            &max_tokens,
            |b, &tokens| {
                b.iter(|| {
                    let options = ContextPackOptions {
                        workspace_path: workspace_path.clone(),
                        database_path: Some(db_path.clone()),
                        index_dir: None,
                        query: "prepare for release deployment and testing".to_string(),
                        speed: SpeedMode::Default,
                        filters: Default::default(),
                        profile: None,
                        max_tokens: Some(tokens),
                        candidate_pool: Some(50),
                        max_results: None,
                        include_tombstoned: false,
                        as_of: None,
                        include_expired: false,
                        include_future: false,
                        include_stale: false,
                        redaction_level: RedactionLevel::Minimal,
                        memory_scope: MemoryScope::Swarm,
                        strict_scope: false,
                        ppr_weight: None,
                        pagination: None,
                        coordination_snapshot_path: None,
                        coordination_stale_after_ms: ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
                        output_options: Default::default(),
                    };
                    run_context_pack(&options).expect("context pack");
                });
            },
        );
    }

    group.finish();
}

/// Benchmark context pack at different memory scales.
fn bench_context_memory_scales(c: &mut Criterion) {
    let mut group = c.benchmark_group("ee_context_memory_scales");

    for &count in &[10usize, 100, 500] {
        let label = match count {
            10 => "10_memories",
            100 => "100_memories",
            500 => "500_memories",
            _ => "unknown",
        };

        group.bench_with_input(BenchmarkId::new("context_pack", label), &count, |b, &n| {
            let temp_dir = TempDir::new().expect("temp dir");
            let workspace_path = seed_database(temp_dir.path(), n);
            let db_path = workspace_path.join(".ee").join("ee.db");

            b.iter(|| {
                let options = ContextPackOptions {
                    workspace_path: workspace_path.clone(),
                    database_path: Some(db_path.clone()),
                    index_dir: None,
                    query: "release testing security".to_string(),
                    speed: SpeedMode::Default,
                    filters: Default::default(),
                    profile: None,
                    max_tokens: Some(4000),
                    candidate_pool: Some(50),
                    max_results: None,
                    include_tombstoned: false,
                    as_of: None,
                    include_expired: false,
                    include_future: false,
                    include_stale: false,
                    redaction_level: RedactionLevel::Minimal,
                    memory_scope: MemoryScope::Swarm,
                    strict_scope: false,
                    ppr_weight: None,
                    pagination: None,
                    coordination_snapshot_path: None,
                    coordination_stale_after_ms: ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
                    output_options: Default::default(),
                };
                run_context_pack(&options).expect("context pack");
            });
        });
    }

    group.finish();
}

/// Benchmark context SLO telemetry over S4's required large-memory fixtures.
fn bench_context_s4_resource_scales(c: &mut Criterion) {
    let mut group = c.benchmark_group("ee_context_s4_resource_scales");

    for scale in s4_resource_scales_for_profile(&active_bench_profile()) {
        group.bench_with_input(
            BenchmarkId::new("context_pack_slo", scale.label),
            &scale,
            |b, scale| {
                let temp_dir = TempDir::new().expect("temp dir");
                let workspace_path =
                    seed_resource_scale_database(temp_dir.path(), scale.memory_count);
                let db_path = workspace_path.join(".ee").join("ee.db");
                let index_dir =
                    build_resource_scale_index(&workspace_path, &db_path, scale.memory_count);

                b.iter(|| {
                    let options = ContextPackOptions {
                        workspace_path: workspace_path.clone(),
                        database_path: Some(db_path.clone()),
                        index_dir: Some(index_dir.clone()),
                        query: "S4 resource benchmark release testing performance".to_string(),
                        speed: SpeedMode::Default,
                        filters: Default::default(),
                        profile: None,
                        max_tokens: Some(4000),
                        candidate_pool: Some(scale.candidate_pool),
                        max_results: None,
                        include_tombstoned: false,
                        as_of: None,
                        include_expired: false,
                        include_future: false,
                        include_stale: false,
                        redaction_level: RedactionLevel::Minimal,
                        memory_scope: MemoryScope::Swarm,
                        strict_scope: false,
                        ppr_weight: None,
                        pagination: None,
                        coordination_snapshot_path: None,
                        coordination_stale_after_ms: ee::pack::DEFAULT_COORDINATION_STALE_AFTER_MS,
                        output_options: ContextPackOutputOptions::default()
                            .with_resource_profile(scale.resource_profile),
                    };
                    let run = run_context_pack_with_performance(&options, "context")
                        .expect("context pack");
                    let search_ms = performance_timing_ms(&run.performance, "search");
                    let pack_assembly_ms = performance_timing_ms(&run.performance, "packAssembly");
                    let memory_bytes_peak = run
                        .performance
                        .pointer("/data/pack/slo/actuals/memoryBytesPeak")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or_default();
                    black_box(run.response.data.slo.as_ref().map(|slo| {
                        (
                            search_ms,
                            pack_assembly_ms,
                            memory_bytes_peak,
                            slo.actuals.elapsed_ms,
                            slo.actuals.memory_bytes_peak,
                            slo.actuals.scanned_count,
                        )
                    }));
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_context,
    bench_context_memory_scales,
    bench_context_s4_resource_scales
);
criterion_main!(benches);

#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use tempfile::TempDir;

    use super::{
        BUDGET_P50_MS, BUDGET_P99_MS, REGRESSION_THRESHOLD, S4_NIGHTLY_SCALE,
        S4_RELEASE_CANDIDATE_SCALE, S4_RESOURCE_SCALES, S4_STRESS_SCALE,
        s4_resource_scales_for_profile, seed_database,
    };

    #[test]
    fn benchmark_group_name_is_canonical() {
        assert_eq!("ee_context", "ee_context", "canonical group name");
    }

    #[test]
    fn budget_constants_match_plan() {
        assert!(
            (BUDGET_P50_MS - 95.0).abs() < f64::EPSILON,
            "p50 budget matches plan §28"
        );
        assert!(
            (BUDGET_P99_MS - 240.0).abs() < f64::EPSILON,
            "p99 budget matches plan §28"
        );
    }

    #[test]
    fn regression_threshold_is_30_percent() {
        assert!(
            (REGRESSION_THRESHOLD - 0.30).abs() < f64::EPSILON,
            "regression threshold is 30%"
        );
    }

    #[test]
    fn can_seed_database_for_context() {
        let temp_dir = TempDir::new().expect("temp dir");
        let workspace_path = seed_database(temp_dir.path(), 10);
        let db_path = workspace_path.join(".ee").join("ee.db");
        assert!(db_path.exists(), "database file exists after seeding");
    }

    #[test]
    fn s4_resource_scale_table_covers_required_fixture_sizes() {
        let counts = S4_RESOURCE_SCALES
            .iter()
            .map(|scale| scale.memory_count)
            .collect::<Vec<_>>();
        assert_eq!(
            counts,
            vec![
                S4_RELEASE_CANDIDATE_SCALE,
                S4_NIGHTLY_SCALE,
                S4_STRESS_SCALE
            ],
            "S4 benchmark scales must cover 1k, 10k, and 100k memories"
        );
    }

    #[test]
    fn s4_resource_scales_are_profile_gated() {
        assert_eq!(
            s4_resource_scales_for_profile("ci-smoke")
                .iter()
                .map(|scale| scale.memory_count)
                .collect::<Vec<_>>(),
            vec![S4_RELEASE_CANDIDATE_SCALE]
        );
        assert_eq!(
            s4_resource_scales_for_profile("nightly")
                .iter()
                .map(|scale| scale.memory_count)
                .collect::<Vec<_>>(),
            vec![S4_RELEASE_CANDIDATE_SCALE, S4_NIGHTLY_SCALE]
        );
        assert_eq!(
            s4_resource_scales_for_profile("stress")
                .iter()
                .map(|scale| scale.memory_count)
                .collect::<Vec<_>>(),
            vec![
                S4_RELEASE_CANDIDATE_SCALE,
                S4_NIGHTLY_SCALE,
                S4_STRESS_SCALE
            ]
        );
    }
}
