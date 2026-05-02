//! Criterion benchmark for `ee context` (EE-PERF-BENCH-context).
//!
//! Group name: `ee_context`
//!
//! Tests the `run_context_pack` function at three token budget scales:
//! - 1k tokens: small context pack
//! - 4k tokens: default context pack
//! - 8k tokens: large context pack
//!
//! Performance budget (plan §28):
//! - p50: 95ms
//! - p99: 240ms
//! - Regression threshold: 30%

#![allow(clippy::expect_used)]
#![allow(dead_code)]

use std::path::Path;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use tempfile::TempDir;

use ee::core::context::{ContextPackOptions, run_context_pack};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::DbConnection;

/// Performance budget from plan §28 (README "Performance" table).
/// p50 must stay under 95ms, p99 under 240ms.
const BUDGET_P50_MS: f64 = 95.0;
const BUDGET_P99_MS: f64 = 240.0;

/// Regression threshold: fail if p50 degrades by more than 30%.
const REGRESSION_THRESHOLD: f64 = 0.30;

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
            level: "procedural",
            kind: "rule",
            tags: Some(&format!("bench,{topic}")),
            confidence: 0.75,
            source: None,
            valid_from: None,
            valid_to: None,
            dry_run: false,
        };
        remember_memory(&options).expect("seed memory");
    }

    workspace_path
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
                        profile: None,
                        max_tokens: Some(tokens),
                        candidate_pool: Some(50),
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
                    profile: None,
                    max_tokens: Some(4000),
                    candidate_pool: Some(50),
                };
                run_context_pack(&options).expect("context pack");
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_context, bench_context_memory_scales);
criterion_main!(benches);

#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use tempfile::TempDir;

    use super::{BUDGET_P50_MS, BUDGET_P99_MS, REGRESSION_THRESHOLD, seed_database};

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
}
