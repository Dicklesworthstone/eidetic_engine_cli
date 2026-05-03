//! Criterion benchmark for `ee graph pagerank` (EE-PERF-BENCH-graph_pagerank).
//!
//! Group name: `ee_graph_pagerank`
//!
//! Bench scales:
//! - empty: no pre-existing memory links
//! - 100_links: medium-sized memory-link graph
//! - 5000_links: large memory-link graph

use std::path::{Path, PathBuf};
use std::time::Instant;

use asupersync::lab::{LabConfig, LabRuntime};
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use serde_json::Value as JsonValue;
use tempfile::TempDir;

use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::{CreateMemoryLinkInput, DbConnection, MemoryLinkRelation, MemoryLinkSource};
use ee::graph::{CentralityRefreshOptions, refresh_centrality};

const BENCH_GROUP_NAME: &str = "ee_graph_pagerank";
const BASELINE_OPERATION_KEY: &str = "ee_graph_pagerank";
const BASELINE_PATH: &str = "benches/baselines/v0.1.json";

/// Performance budget from plan §28 / README performance table.
/// Graph PageRank cold path (5k links): p50 target 350ms, p99 hard ceiling 2000ms.
const BUDGET_P50_MS: f64 = 350.0;
const BUDGET_P99_MS: f64 = 2000.0;

/// Regression threshold: fail compare-only mode when p50 regresses by >30%.
const REGRESSION_THRESHOLD: f64 = 0.30;

/// Quick sampling config used by compare-only mode.
const QUICK_WARMUP_ITERS: usize = 5;
const QUICK_MEASURE_ITERS: usize = 31;
const LAB_RUNTIME_SEED: u64 = 42;

#[derive(Clone, Debug)]
struct QuickStats {
    p50_ms: f64,
    p99_ms: f64,
}

#[derive(Clone, Debug)]
struct BaselineStats {
    p50_ms: f64,
    p99_ms: f64,
}

fn scale_label(count: usize) -> &'static str {
    match count {
        0 => "empty",
        100 => "100_links",
        5000 => "5000_links",
        _ => "unknown",
    }
}

fn seed_link_id(index: usize) -> String {
    format!("link_{index:026}")
}

fn ensure_workspace_layout(workspace_path: &Path) {
    let ee_dir = workspace_path.join(".ee");
    if let Err(error) = std::fs::create_dir_all(&ee_dir) {
        panic!("failed to create workspace .ee directory: {error}");
    }
}

fn seed_database(workspace_path: &Path, link_count: usize) -> PathBuf {
    ensure_workspace_layout(workspace_path);
    let db_path = workspace_path.join(".ee").join("ee.db");
    let memory_total = link_count.saturating_add(2).max(2);

    let mut memory_ids = Vec::with_capacity(memory_total);
    for index in 0..memory_total {
        let content =
            format!("Graph pagerank benchmark memory {index}: deterministic seed for centrality.");
        let options = RememberMemoryOptions {
            workspace_path,
            database_path: Some(&db_path),
            content: &content,
            level: "semantic",
            kind: "fact",
            tags: Some("bench,graph,pagerank,seed"),
            confidence: 0.7,
            source: None,
            valid_from: None,
            valid_to: None,
            dry_run: false,
        };

        let report = match remember_memory(&options) {
            Ok(report) => report,
            Err(error) => panic!("failed seeding benchmark memory {index}: {error:?}"),
        };
        memory_ids.push(report.memory_id.to_string());
    }

    let seed_pool_len = memory_ids.len().saturating_sub(2);
    if link_count == 0 || seed_pool_len < 2 {
        return db_path;
    }

    let connection = match DbConnection::open_file(&db_path) {
        Ok(connection) => connection,
        Err(error) => panic!("failed opening benchmark db for link seeding: {error}"),
    };
    if let Err(error) = connection.migrate() {
        panic!("failed migrating benchmark db for link seeding: {error}");
    }

    for index in 0..link_count {
        let src_memory_id = memory_ids[index % seed_pool_len].clone();
        let dst_memory_id = memory_ids[(index + 1) % seed_pool_len].clone();
        if src_memory_id == dst_memory_id {
            continue;
        }
        let input = CreateMemoryLinkInput {
            src_memory_id,
            dst_memory_id,
            relation: MemoryLinkRelation::Related,
            weight: 0.8,
            confidence: 0.7,
            directed: true,
            evidence_count: 1,
            last_reinforced_at: None,
            source: MemoryLinkSource::Agent,
            created_by: Some("bench-seed".to_owned()),
            metadata_json: None,
        };
        let link_id = seed_link_id(index);
        if let Err(error) = connection.insert_memory_link(&link_id, &input) {
            panic!("failed inserting benchmark seed link {link_id}: {error}");
        }
    }

    db_path
}

fn bind_lab_runtime() -> LabRuntime {
    LabRuntime::new(LabConfig::new(LAB_RUNTIME_SEED))
}

fn run_pagerank_once(connection: &DbConnection) -> f64 {
    let start = Instant::now();

    let options = CentralityRefreshOptions {
        dry_run: false,
        min_weight: None,
        min_confidence: None,
        link_limit: None,
    };
    if let Err(error) = refresh_centrality(connection, &options) {
        panic!("graph pagerank benchmark refresh failed: {error}");
    }

    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
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

fn quick_stats_for_scale(link_count: usize) -> QuickStats {
    let temp_dir = match TempDir::new() {
        Ok(dir) => dir,
        Err(error) => panic!("failed to create tempdir for graph pagerank bench: {error}"),
    };
    let workspace_path = temp_dir.path().to_path_buf();

    let _lab_runtime = bind_lab_runtime();
    let db_path = seed_database(&workspace_path, link_count);
    let connection = match DbConnection::open_file(&db_path) {
        Ok(connection) => connection,
        Err(error) => panic!("failed opening benchmark db: {error}"),
    };

    for _ in 0..QUICK_WARMUP_ITERS {
        let _ = run_pagerank_once(&connection);
    }

    let mut samples = Vec::with_capacity(QUICK_MEASURE_ITERS);
    for _ in 0..QUICK_MEASURE_ITERS {
        samples.push(run_pagerank_once(&connection));
    }
    samples.sort_by(|left, right| left.total_cmp(right));

    QuickStats {
        p50_ms: percentile(&samples, 0.50),
        p99_ms: percentile(&samples, 0.99),
    }
}

fn load_baseline(scale: &str) -> Result<BaselineStats, String> {
    let payload = std::fs::read_to_string(BASELINE_PATH)
        .map_err(|error| format!("failed reading baseline file {BASELINE_PATH}: {error}"))?;
    let json: JsonValue = serde_json::from_str(&payload)
        .map_err(|error| format!("invalid baseline JSON: {error}"))?;

    let operation = json
        .get("operations")
        .and_then(|ops| ops.get(BASELINE_OPERATION_KEY))
        .ok_or_else(|| {
            format!("baseline missing operations.{BASELINE_OPERATION_KEY} in {BASELINE_PATH}")
        })?;
    let scale_node = operation
        .get("scales")
        .and_then(|scales| scales.get(scale))
        .ok_or_else(|| format!("baseline missing scale '{scale}'"))?;

    let p50_ms = scale_node
        .get("p50_ms")
        .and_then(JsonValue::as_f64)
        .ok_or_else(|| format!("baseline scale '{scale}' missing p50_ms"))?;
    let p99_ms = scale_node
        .get("p99_ms")
        .and_then(JsonValue::as_f64)
        .ok_or_else(|| format!("baseline scale '{scale}' missing p99_ms"))?;

    Ok(BaselineStats { p50_ms, p99_ms })
}

fn compare_only_mode_enabled() -> bool {
    std::env::var("EE_BENCH_COMPARE_ONLY")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn assert_regression_window(scale: &str, stats: &QuickStats) {
    let baseline = load_baseline(scale).unwrap_or_else(|error| panic!("{error}"));
    assert!(
        stats.p50_ms <= BUDGET_P50_MS,
        "p50 budget exceeded for scale '{scale}': current {:.3}ms > {:.3}ms",
        stats.p50_ms,
        BUDGET_P50_MS
    );
    let max_p50 = baseline.p50_ms * (1.0 + REGRESSION_THRESHOLD);
    assert!(
        stats.p50_ms <= max_p50,
        "p50 regression for scale '{scale}': current {:.3}ms > {:.3}ms baseline ceiling (baseline {:.3}ms, threshold {:.0}%)",
        stats.p50_ms,
        max_p50,
        baseline.p50_ms,
        REGRESSION_THRESHOLD * 100.0
    );
    assert!(
        stats.p99_ms <= BUDGET_P99_MS,
        "p99 budget exceeded for scale '{scale}': current {:.3}ms > {:.3}ms",
        stats.p99_ms,
        BUDGET_P99_MS
    );
    let max_p99 = baseline.p99_ms * (1.0 + REGRESSION_THRESHOLD);
    assert!(
        stats.p99_ms <= max_p99,
        "p99 regression for scale '{scale}': current {:.3}ms > {:.3}ms baseline ceiling (baseline {:.3}ms, threshold {:.0}%)",
        stats.p99_ms,
        max_p99,
        baseline.p99_ms,
        REGRESSION_THRESHOLD * 100.0
    );
}

fn bench_graph_pagerank(c: &mut Criterion) {
    if compare_only_mode_enabled() {
        for &count in &[0usize, 100, 5000] {
            let label = scale_label(count);
            let stats = quick_stats_for_scale(count);
            assert_regression_window(label, &stats);
        }
        return;
    }

    let mut group = c.benchmark_group(BENCH_GROUP_NAME);
    for &count in &[0usize, 100, 5000] {
        let label = scale_label(count);
        group.bench_with_input(
            BenchmarkId::new("graph_pagerank_refresh", label),
            &count,
            |b, &n| {
                let temp_dir = match TempDir::new() {
                    Ok(dir) => dir,
                    Err(error) => panic!("failed to create benchmark tempdir: {error}"),
                };
                let workspace_path = temp_dir.path().to_path_buf();
                let _lab_runtime = bind_lab_runtime();
                let db_path = seed_database(&workspace_path, n);
                let connection = match DbConnection::open_file(&db_path) {
                    Ok(connection) => connection,
                    Err(error) => panic!("failed opening benchmark db: {error}"),
                };

                b.iter(|| {
                    let elapsed_ms = run_pagerank_once(&connection);
                    black_box(elapsed_ms);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_graph_pagerank);
criterion_main!(benches);

#[cfg(test)]
mod tests {
    #[test]
    fn benchmark_group_name_is_canonical() {
        assert_eq!(
            super::BENCH_GROUP_NAME,
            "ee_graph_pagerank",
            "canonical group name"
        );
    }

    #[test]
    fn budget_constants_match_plan() {
        assert!(
            (super::BUDGET_P50_MS - 350.0).abs() < f64::EPSILON,
            "p50 budget matches plan §28/README"
        );
        assert!(
            (super::BUDGET_P99_MS - 2000.0).abs() < f64::EPSILON,
            "p99 hard ceiling matches plan §28/README"
        );
    }

    #[test]
    fn regression_threshold_is_30_percent() {
        assert!(
            (super::REGRESSION_THRESHOLD - 0.30).abs() < f64::EPSILON,
            "regression threshold is 30%"
        );
    }

    #[test]
    fn seeded_link_ids_match_database_constraint_shape() {
        let sample = super::seed_link_id(42);
        assert!(
            sample.starts_with("link_"),
            "seed link id must use link_ prefix"
        );
        assert_eq!(sample.len(), 31, "seed link id length");
    }
}
