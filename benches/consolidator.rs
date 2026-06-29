//! Criterion benchmark for steward consolidation via Sieve-Streaming.
//!
//! Group name: `bench_consolidator_sieve_streaming`
//!
//! Bench scale:
//! - 10000_memories: 2k duplicate groups, cardinality bound k=64

#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use tempfile::TempDir;

use ee::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};
use ee::models::WorkspaceId;
use ee::steward::{JobType, ManualRunner, RunOutcome, RunnerOptions};

const BENCH_GROUP_NAME: &str = "bench_consolidator_sieve_streaming";
const BENCH_OPERATION_NAME: &str = "consolidation_pass";
const SCALE_LABEL_10K: &str = "10000_memories";
const MEMORY_COUNT_10K: usize = 10_000;
const DUPLICATE_GROUP_SIZE: usize = 5;
const SELECTOR_ITEM_LIMIT: u64 = 64;
const BUDGET_P50_MS: f64 = 200.0;
const QUICK_WARMUP_ITERS: usize = 3;
const QUICK_MEASURE_ITERS: usize = 21;

struct ConsolidatorFixture {
    _temp_dir: TempDir,
    workspace_path: PathBuf,
    db_path: PathBuf,
    candidate_count: usize,
}

#[derive(Clone, Debug)]
struct QuickStats {
    p50_ms: f64,
}

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn ensure_workspace_row(connection: &DbConnection, workspace_path: &Path) -> String {
    let workspace_path_string = workspace_path.to_string_lossy().into_owned();
    let workspace_id = stable_workspace_id(workspace_path);
    connection
        .insert_workspace(
            &workspace_id,
            &CreateWorkspaceInput {
                path: workspace_path_string,
                name: Some("consolidator-bench".to_owned()),
            },
        )
        .expect("insert consolidator benchmark workspace");
    workspace_id
}

fn duplicate_content(group_index: usize, slot: usize) -> String {
    let shard = group_index % 32;
    match slot % DUPLICATE_GROUP_SIZE {
        0 => format!("Consolidator duplicate group {group_index:05} shard {shard}"),
        1 => format!(" consolidator   duplicate   group {group_index:05} shard {shard} "),
        2 => format!("CONSOLIDATOR DUPLICATE GROUP {group_index:05} SHARD {shard}"),
        3 => format!("\nConsolidator duplicate group {group_index:05} shard {shard}\n"),
        _ => format!("consolidator duplicate group {group_index:05} shard {shard}"),
    }
}

fn seed_fixture(memory_count: usize) -> ConsolidatorFixture {
    assert!(
        memory_count % DUPLICATE_GROUP_SIZE == 0,
        "benchmark scale must divide evenly into duplicate groups"
    );
    let temp_dir = TempDir::new().expect("create consolidator benchmark tempdir");
    let workspace_path = temp_dir.path().to_path_buf();
    let db_path = workspace_path.join(".ee").join("ee.db");
    std::fs::create_dir_all(db_path.parent().expect("db parent")).expect("create .ee dir");

    let connection = DbConnection::open_file(&db_path).expect("open consolidator benchmark db");
    connection
        .migrate()
        .expect("migrate consolidator benchmark db");
    let workspace_id = ensure_workspace_row(&connection, &workspace_path);

    let levels = ["procedural", "episodic", "decision", "evidence"];
    let kinds = ["rule", "fact", "note", "failure"];
    for index in 0..memory_count {
        let group_index = index / DUPLICATE_GROUP_SIZE;
        let slot = index % DUPLICATE_GROUP_SIZE;
        let quality = 1.0_f32 - ((slot as f32) * 0.12);
        let input = CreateMemoryInput {
            workspace_id: workspace_id.clone(),
            level: levels[group_index % levels.len()].to_owned(),
            kind: kinds[group_index % kinds.len()].to_owned(),
            content: duplicate_content(group_index, slot),
            workflow_id: None,
            confidence: quality,
            utility: quality,
            importance: quality,
            provenance_uri: None,
            trust_class: "human_explicit".to_owned(),
            trust_subclass: Some("consolidator-bench".to_owned()),
            tags: vec!["bench".to_owned(), "consolidator".to_owned()],
            valid_from: None,
            valid_to: None,
        };
        let memory_id = format!("mem_consolidator_{index:06}");
        connection
            .insert_memory(&memory_id, &input)
            .expect("insert consolidator benchmark memory");
    }

    let duplicate_group_count = memory_count / DUPLICATE_GROUP_SIZE;
    ConsolidatorFixture {
        _temp_dir: temp_dir,
        workspace_path,
        db_path,
        candidate_count: memory_count - duplicate_group_count,
    }
}

fn run_consolidation_once(fixture: &ConsolidatorFixture) -> f64 {
    let options = RunnerOptions::new()
        .with_workspace_path(&fixture.workspace_path)
        .with_database_path(&fixture.db_path)
        .with_item_limit(SELECTOR_ITEM_LIMIT)
        .with_as_of("2026-05-21T00:00:00Z")
        .with_actor("bench-consolidator")
        .with_dry_run(true);
    let mut runner = ManualRunner::new(options);

    let started = Instant::now();
    let result = runner.run_job_type(
        JobType::ConsolidationPass,
        Some("bench_consolidator_sieve_streaming".to_owned()),
    );
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;

    assert_eq!(
        result.outcome,
        RunOutcome::Success,
        "consolidation succeeds"
    );
    let details = result.details.as_ref().expect("consolidation details");
    assert_eq!(
        details["selector"]["algorithm"].as_str(),
        Some("sieve_streaming_greedy_v1"),
        "selector algorithm is surfaced"
    );
    assert_eq!(
        details["selector"]["consideredCandidates"].as_u64(),
        Some(fixture.candidate_count as u64),
        "all duplicate candidates are considered"
    );
    assert_eq!(
        details["selector"]["selectedCandidates"].as_u64(),
        Some(SELECTOR_ITEM_LIMIT),
        "selector applies the k bound"
    );
    assert_eq!(
        details["dryRun"].as_bool(),
        Some(true),
        "benchmark is dry-run"
    );
    assert_eq!(
        details["durableMutation"].as_bool(),
        Some(false),
        "benchmark does not mutate curation state"
    );

    black_box(details);
    black_box(elapsed_ms);
    elapsed_ms
}

fn percentile(sorted_samples: &[f64], percentile: f64) -> f64 {
    assert!(
        !sorted_samples.is_empty(),
        "percentile requires at least one sample"
    );
    let last_index = sorted_samples.len() - 1;
    let raw = (percentile * last_index as f64).round();
    let index = raw.clamp(0.0, last_index as f64) as usize;
    sorted_samples[index]
}

fn quick_stats_for_scale(memory_count: usize) -> QuickStats {
    let fixture = seed_fixture(memory_count);
    for _ in 0..QUICK_WARMUP_ITERS {
        let _ = run_consolidation_once(&fixture);
    }

    let mut samples = Vec::with_capacity(QUICK_MEASURE_ITERS);
    for _ in 0..QUICK_MEASURE_ITERS {
        samples.push(run_consolidation_once(&fixture));
    }
    samples.sort_by(|left, right| left.total_cmp(right));

    QuickStats {
        p50_ms: percentile(&samples, 0.50),
    }
}

fn compare_only_mode_enabled() -> bool {
    std::env::var("EE_BENCH_COMPARE_ONLY")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn assert_p50_budget(stats: &QuickStats) {
    assert!(
        stats.p50_ms <= BUDGET_P50_MS,
        "p50 budget exceeded for {SCALE_LABEL_10K}: current {:.3}ms > {:.3}ms",
        stats.p50_ms,
        BUDGET_P50_MS
    );
}

fn bench_consolidator_sieve_streaming(c: &mut Criterion) {
    if compare_only_mode_enabled() {
        let stats = quick_stats_for_scale(MEMORY_COUNT_10K);
        assert_p50_budget(&stats);
        return;
    }

    let fixture = seed_fixture(MEMORY_COUNT_10K);
    let mut group = c.benchmark_group(BENCH_GROUP_NAME);
    group.bench_function(
        BenchmarkId::new(BENCH_OPERATION_NAME, SCALE_LABEL_10K),
        |bench| {
            bench.iter(|| {
                let elapsed_ms = run_consolidation_once(&fixture);
                black_box(elapsed_ms);
            });
        },
    );
    group.finish();
}

criterion_group!(benches, bench_consolidator_sieve_streaming);
criterion_main!(benches);

#[cfg(test)]
mod tests {
    #[test]
    fn benchmark_group_name_is_canonical() {
        assert_eq!(
            super::BENCH_GROUP_NAME,
            "bench_consolidator_sieve_streaming"
        );
    }

    #[test]
    fn p50_budget_matches_bead_acceptance() {
        assert!(
            (super::BUDGET_P50_MS - 200.0).abs() < f64::EPSILON,
            "p50 budget matches bd-3usjw.45"
        );
    }

    #[test]
    fn ten_k_scale_produces_expected_candidate_count() {
        let groups = super::MEMORY_COUNT_10K / super::DUPLICATE_GROUP_SIZE;
        assert_eq!(
            super::MEMORY_COUNT_10K - groups,
            8_000,
            "10k fixture should create 8k duplicate candidates"
        );
    }
}
