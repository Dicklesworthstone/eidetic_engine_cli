//! Criterion benchmark for `ee why` (EE-PERF-BENCH-why).
//!
//! Group name: `ee_why`
//!
//! Input scales:
//! - empty: no stored memories (not-found lookup path)
//! - 100: database seeded with 100 memories
//! - 5000: database seeded with 5000 memories
//!
//! Quick/compare controls:
//! - `EE_BENCH_QUICK=1` runs deterministic preflight latency checks.
//! - `EE_BENCH_COMPARE_ONLY=1` additionally enforces baseline regression
//!   (`p50 <= baseline * (1 + 30%)`) using `benches/baselines/v0.1.json`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use asupersync::lab::{LabConfig, LabRuntime};
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::core::why::{WhyOptions, explain_memory};
use ee::search::HashEmbedder;
use tempfile::TempDir;

const GROUP_NAME: &str = "ee_why";
const TARGET_P50_MS: f64 = 25.0;
const HARD_CEILING_MS: f64 = 100.0;
const REGRESSION_THRESHOLD: f64 = 0.30;
const BASELINE_FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/benches/baselines/v0.1.json");
const QUICK_ITERATIONS: usize = 24;
const DEFAULT_ITERATIONS: usize = 120;
const MISSING_MEMORY_ID: &str = "mem_missing_benchmark_target";

#[derive(Debug)]
struct Fixture {
    _temp_dir: TempDir,
    db_path: PathBuf,
    target_memory_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Baseline {
    p50_ms: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LatencyStats {
    p50_ms: f64,
    p99_ms: f64,
}

fn quick_mode_enabled() -> bool {
    std::env::var("EE_BENCH_QUICK")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn compare_only_enabled() -> bool {
    std::env::var("EE_BENCH_COMPARE_ONLY")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn bind_deterministic_harness() {
    let _runtime = LabRuntime::new(LabConfig::new(42));
    let _hash_embedder = HashEmbedder::default_256();
}

fn seed_fixture(memory_count: usize) -> Fixture {
    let temp_dir = TempDir::new().unwrap_or_else(|error| {
        panic!("failed to create tempdir for why benchmark fixture: {error}")
    });
    let workspace_path = temp_dir.path().to_path_buf();
    let db_path = workspace_path.join(".ee").join("ee.db");

    let db_parent = db_path
        .parent()
        .unwrap_or_else(|| panic!("database path has no parent: {}", db_path.display()));
    if let Err(error) = std::fs::create_dir_all(db_parent) {
        panic!(
            "failed to create benchmark workspace directory {}: {error}",
            db_parent.display()
        );
    }
    bind_deterministic_harness();

    let mut target_memory_id = MISSING_MEMORY_ID.to_owned();
    let target_index = memory_count / 2;
    for index in 0..memory_count {
        let report = remember_memory(&RememberMemoryOptions {
            workspace_path: &workspace_path,
            database_path: Some(&db_path),
            content: &format!(
                "Seed memory {index}: benchmark fixture for ee why latency and retrieval explainability."
            ),
            level: "semantic",
            kind: "fact",
            tags: Some("bench,why"),
            confidence: 0.7,
            source: None,
            valid_from: None,
            valid_to: None,
            dry_run: false,
        })
        .unwrap_or_else(|error| panic!("failed to seed benchmark memory {index}: {error:?}"));

        if index == target_index {
            target_memory_id = report.memory_id.to_string();
        }
    }

    Fixture {
        _temp_dir: temp_dir,
        db_path,
        target_memory_id,
    }
}

fn single_why_latency_ms(db_path: &Path, memory_id: &str) -> f64 {
    let start = Instant::now();
    let report = explain_memory(&WhyOptions {
        database_path: db_path,
        memory_id,
        confidence_threshold: WhyOptions::DEFAULT_CONFIDENCE_THRESHOLD,
    });
    black_box(report);
    start.elapsed().as_secs_f64() * 1_000.0
}

fn percentile_ms(samples: &[f64], quantile: f64) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(|left, right| left.total_cmp(right));
    let len = sorted.len();
    let index = ((len.saturating_sub(1) as f64) * quantile).round() as usize;
    sorted[index.min(len.saturating_sub(1))]
}

fn load_why_baseline() -> Baseline {
    let raw = std::fs::read_to_string(BASELINE_FILE)
        .unwrap_or_else(|error| panic!("failed reading baseline file `{BASELINE_FILE}`: {error}"));
    let json: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("failed parsing baseline JSON `{BASELINE_FILE}`: {error}"));
    let p50_ms = json
        .get("operations")
        .and_then(|value| value.get("ee_why"))
        .and_then(|value| value.get("p50_ms"))
        .and_then(serde_json::Value::as_f64)
        .unwrap_or_else(|| panic!("missing baseline value operations.ee_why.p50_ms"));

    Baseline { p50_ms }
}

fn gather_latency_stats(memory_count: usize, iterations: usize) -> LatencyStats {
    let fixture = seed_fixture(memory_count);
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        samples.push(single_why_latency_ms(
            &fixture.db_path,
            &fixture.target_memory_id,
        ));
    }

    LatencyStats {
        p50_ms: percentile_ms(&samples, 0.50),
        p99_ms: percentile_ms(&samples, 0.99),
    }
}

fn assert_latency_within_budget(scale: &str, stats: LatencyStats, compare_baseline: bool) {
    assert!(
        stats.p50_ms <= HARD_CEILING_MS,
        "{GROUP_NAME} {scale}: p50 {:.3}ms exceeded hard ceiling {:.3}ms",
        stats.p50_ms,
        HARD_CEILING_MS
    );

    if compare_baseline {
        let baseline = load_why_baseline();
        assert!(
            baseline.p50_ms <= TARGET_P50_MS,
            "{GROUP_NAME} baseline p50 {:.3}ms exceeds target p50 {:.3}ms",
            baseline.p50_ms,
            TARGET_P50_MS
        );
        let limit = baseline.p50_ms * (1.0 + REGRESSION_THRESHOLD);
        assert!(
            stats.p50_ms <= limit,
            "{GROUP_NAME} {scale}: p50 {:.3}ms exceeded regression threshold (baseline {:.3}ms, limit {:.3}ms)",
            stats.p50_ms,
            baseline.p50_ms,
            limit
        );
    }
}

fn run_preflight_checks_if_requested() {
    let quick = quick_mode_enabled();
    let compare = compare_only_enabled();
    if !quick && !compare {
        return;
    }

    let iterations = if quick {
        QUICK_ITERATIONS
    } else {
        DEFAULT_ITERATIONS
    };

    for (scale, memory_count) in [
        ("empty", 0usize),
        ("100_memories", 100),
        ("5000_memories", 5000),
    ] {
        let stats = gather_latency_stats(memory_count, iterations);
        assert_latency_within_budget(scale, stats, compare);
    }
}

fn bench_why(c: &mut Criterion) {
    run_preflight_checks_if_requested();

    let mut group = c.benchmark_group(GROUP_NAME);
    if quick_mode_enabled() {
        group.measurement_time(Duration::from_secs(1));
        group.sample_size(10);
    }

    for &(memory_count, label) in &[
        (0usize, "empty"),
        (100, "100_memories"),
        (5000, "5000_memories"),
    ] {
        group.bench_with_input(
            BenchmarkId::new("why", label),
            &memory_count,
            |b, &count| {
                let fixture = seed_fixture(count);
                b.iter(|| {
                    let report = explain_memory(&WhyOptions {
                        database_path: &fixture.db_path,
                        memory_id: &fixture.target_memory_id,
                        confidence_threshold: WhyOptions::DEFAULT_CONFIDENCE_THRESHOLD,
                    });
                    black_box(report);
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_why);
criterion_main!(benches);

#[cfg(test)]
mod tests {
    #[test]
    fn benchmark_group_name_is_canonical() {
        assert_eq!(super::GROUP_NAME, "ee_why", "canonical group name");
    }

    #[test]
    fn quick_regression_check_stays_under_hard_ceiling() {
        let stats = super::gather_latency_stats(100, super::QUICK_ITERATIONS);
        assert!(
            stats.p50_ms <= super::HARD_CEILING_MS,
            "quick mode p50 {:.3}ms exceeded hard ceiling {:.3}ms",
            stats.p50_ms,
            super::HARD_CEILING_MS
        );
        assert!(
            stats.p99_ms <= super::HARD_CEILING_MS * 2.0,
            "quick mode p99 {:.3}ms exceeded sanity ceiling {:.3}ms",
            stats.p99_ms,
            super::HARD_CEILING_MS * 2.0
        );
    }
}
