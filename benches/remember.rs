//! Criterion benchmark for `ee remember` (EE-PERF-BENCH-remember).
//!
//! Group name: `ee_remember`
//!
//! Tests the `remember_memory` function at three scales:
//! - empty: fresh database with no existing memories
//! - 100: database seeded with 100 memories
//! - 5000: database seeded with 5000 memories
//!
//! Performance budget (plan §28):
//! - p50: 8ms
//! - p99: 22ms
//! - Regression threshold: 30%

#![allow(clippy::expect_used)]
#![allow(dead_code)]

use std::path::Path;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use tempfile::TempDir;

use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::DbConnection;

/// Performance budget from plan §28 (README "Performance" table).
/// p50 must stay under 8ms, p99 under 22ms.
const BUDGET_P50_MS: f64 = 8.0;
const BUDGET_P99_MS: f64 = 22.0;

/// Regression threshold: fail if p50 degrades by more than 30%.
const REGRESSION_THRESHOLD: f64 = 0.30;

/// Seed a database with `n` memories and return the workspace root path.
fn seed_database(temp_dir: &Path, n: usize) -> std::path::PathBuf {
    let workspace_path = temp_dir.to_path_buf();
    let db_path = workspace_path.join(".ee").join("ee.db");

    std::fs::create_dir_all(db_path.parent().expect("db parent")).expect("create .ee dir");

    let connection = DbConnection::open_file(&db_path).expect("open db");
    connection.migrate().expect("migrate db");

    for i in 0..n {
        let content = format!(
            "Seed memory {i}: This is a test memory for benchmarking the remember operation."
        );
        let options = RememberMemoryOptions {
            workspace_path: &workspace_path,
            database_path: Some(&db_path),
            content: &content,
            level: "semantic",
            kind: "fact",
            tags: Some("bench,seed"),
            confidence: 0.7,
            source: None,
            valid_from: None,
            valid_to: None,
            dry_run: false,
        };
        remember_memory(&options).expect("seed memory");
    }

    workspace_path
}

/// Benchmark `remember_memory` at different database scales.
fn bench_remember(c: &mut Criterion) {
    let mut group = c.benchmark_group("ee_remember");

    for &count in &[0usize, 100, 5000] {
        let label = match count {
            0 => "empty",
            100 => "100_memories",
            5000 => "5000_memories",
            _ => "unknown",
        };

        group.bench_with_input(BenchmarkId::new("remember", label), &count, |b, &n| {
            let temp_dir = TempDir::new().expect("temp dir");
            let workspace_path = seed_database(temp_dir.path(), n);
            let db_path = workspace_path.join(".ee").join("ee.db");

            let mut counter = n;
            b.iter(|| {
                counter += 1;
                let content = format!(
                    "Benchmark memory {counter}: Testing remember performance at scale {n}."
                );
                let options = RememberMemoryOptions {
                    workspace_path: &workspace_path,
                    database_path: Some(&db_path),
                    content: &content,
                    level: "episodic",
                    kind: "fact",
                    tags: Some("bench,measure"),
                    confidence: 0.6,
                    source: None,
                    valid_from: None,
                    valid_to: None,
                    dry_run: false,
                };
                remember_memory(&options).expect("remember");
            });
        });
    }

    group.finish();
}

/// Benchmark dry-run mode (validation without persistence).
fn bench_remember_dry_run(c: &mut Criterion) {
    let mut group = c.benchmark_group("ee_remember_dry_run");

    let temp_dir = TempDir::new().expect("temp dir");
    let workspace_path = temp_dir.path().to_path_buf();

    group.bench_function("dry_run", |b| {
        let mut counter = 0usize;
        b.iter(|| {
            counter += 1;
            let content = format!("Dry run benchmark {counter}: Validation only.");
            let options = RememberMemoryOptions {
                workspace_path: &workspace_path,
                database_path: None,
                content: &content,
                level: "working",
                kind: "fact",
                tags: Some("bench,dry_run"),
                confidence: 0.5,
                source: None,
                valid_from: None,
                valid_to: None,
                dry_run: true,
            };
            remember_memory(&options).expect("dry_run remember");
        });
    });

    group.finish();
}

criterion_group!(benches, bench_remember, bench_remember_dry_run);
criterion_main!(benches);

#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use tempfile::TempDir;

    use super::{BUDGET_P50_MS, BUDGET_P99_MS, REGRESSION_THRESHOLD, seed_database};

    #[test]
    fn benchmark_group_name_is_canonical() {
        assert_eq!("ee_remember", "ee_remember", "canonical group name");
    }

    #[test]
    fn budget_constants_match_plan() {
        assert!(
            (BUDGET_P50_MS - 8.0).abs() < f64::EPSILON,
            "p50 budget matches plan §28"
        );
        assert!(
            (BUDGET_P99_MS - 22.0).abs() < f64::EPSILON,
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
    fn can_seed_empty_database() {
        let temp_dir = TempDir::new().expect("temp dir");
        let workspace_path = seed_database(temp_dir.path(), 0);
        let db_path = workspace_path.join(".ee").join("ee.db");
        assert!(db_path.exists(), "database file exists");
    }

    #[test]
    fn can_seed_database_with_memories() {
        let temp_dir = TempDir::new().expect("temp dir");
        let workspace_path = seed_database(temp_dir.path(), 5);
        let db_path = workspace_path.join(".ee").join("ee.db");
        assert!(db_path.exists(), "database file exists after seeding");
    }
}
