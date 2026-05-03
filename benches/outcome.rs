//! Criterion benchmark for `ee outcome` (EE-PERF-BENCH-outcome).
//!
//! Group name: `ee_outcome`
//!
//! Bench scales:
//! - empty: fresh database with only the target memory
//! - 25_memories: database seeded with 25 additional memories
//! - 100_memories: database seeded with 100 additional memories
#![allow(clippy::expect_used, clippy::manual_is_multiple_of)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use asupersync::lab::{LabConfig, LabRuntime};
use criterion::{BenchmarkId, Criterion, black_box};
use serde_json::{Value as JsonValue, json};
use tempfile::TempDir;

use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::core::outcome::{
    DEFAULT_HARMFUL_BURST_WINDOW_SECONDS, DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR,
    OutcomeRecordOptions, record_outcome,
};
use ee::db::DbConnection;

const BENCH_GROUP_NAME: &str = "ee_outcome";
const BASELINE_OPERATION_KEY: &str = "ee_outcome";
const BASELINE_PATH: &str = "benches/baselines/v0.1.json";
const QUICK_SUMMARY_PATH: &str = "target/criterion/ee_outcome/quick_summary.json";

/// Plan §28 target p50 for `ee outcome`.
const TARGET_P50_MS: f64 = 8.0;

/// Plan §28 hard ceiling for `ee outcome` quick-mode guardrails.
const HARD_CEILING_MS: f64 = 30.0;

/// Regression threshold: fail compare-only mode when p50/p99 regresses by >30%.
const REGRESSION_THRESHOLD: f64 = 0.30;

/// Quick sampling config used by compare-only mode and tests.
const QUICK_WARMUP_ITERS: usize = 5;
const QUICK_MEASURE_ITERS: usize = 31;
const LAB_RUNTIME_SEED: u64 = 42;
const OUTCOME_SCALES: &[usize] = &[0, 25, 100];

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

#[derive(Clone, Debug)]
struct QuickScaleSample {
    scale: &'static str,
    memory_count: usize,
    p50_ms: f64,
    p99_ms: f64,
}

struct OutcomeFixture {
    db_path: PathBuf,
    target_memory_id: String,
}

fn scale_label(count: usize) -> &'static str {
    match count {
        0 => "empty",
        25 => "25_memories",
        100 => "100_memories",
        _ => "unknown",
    }
}

fn remember_seed_memory(workspace_path: &Path, db_path: &Path, index: usize) {
    let content = format!("Seed memory {index}: benchmark fixture content.");
    let options = RememberMemoryOptions {
        workspace_path,
        database_path: Some(db_path),
        content: &content,
        level: "semantic",
        kind: "fact",
        tags: Some("bench,seed"),
        confidence: 0.65,
        source: None,
        valid_from: None,
        valid_to: None,
        dry_run: false,
    };
    remember_memory(&options).expect("seed memory");
}

fn seed_fixture(temp_dir: &Path, memory_count: usize) -> OutcomeFixture {
    let workspace_path = temp_dir.to_path_buf();
    let db_path = workspace_path.join(".ee").join("ee.db");
    let db_parent = db_path.parent().expect("db parent path");
    std::fs::create_dir_all(db_parent).expect("create benchmark .ee directory");

    let connection = DbConnection::open_file(&db_path).expect("open benchmark db");
    connection.migrate().expect("migrate benchmark db");

    for i in 0..memory_count {
        remember_seed_memory(&workspace_path, &db_path, i);
    }

    let target = remember_memory(&RememberMemoryOptions {
        workspace_path: &workspace_path,
        database_path: Some(&db_path),
        content: "Outcome benchmark target memory.",
        level: "episodic",
        kind: "fact",
        tags: Some("bench,target"),
        confidence: 0.7,
        source: None,
        valid_from: None,
        valid_to: None,
        dry_run: false,
    })
    .expect("create target memory");

    OutcomeFixture {
        db_path,
        target_memory_id: target.memory_id.to_string(),
    }
}

fn bind_lab_runtime() -> LabRuntime {
    LabRuntime::new(LabConfig::new(LAB_RUNTIME_SEED))
}

fn record_once(fixture: &OutcomeFixture, counter: usize, dry_run: bool) -> Result<f64, String> {
    let signal = if counter % 2 == 0 {
        "helpful"
    } else {
        "harmful"
    };
    let reason = format!("benchmark outcome event {counter}");

    let start = Instant::now();
    let report = record_outcome(&OutcomeRecordOptions {
        database_path: fixture.db_path.as_path(),
        target_type: "memory".to_string(),
        target_id: fixture.target_memory_id.clone(),
        workspace_id: None,
        signal: signal.to_string(),
        weight: None,
        source_type: "outcome_observed".to_string(),
        source_id: None,
        reason: Some(reason),
        evidence_json: None,
        session_id: None,
        event_id: None,
        actor: Some("benchmark".to_string()),
        dry_run,
        harmful_per_source_per_hour: DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR,
        harmful_burst_window_seconds: DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
    })
    .map_err(|error| format!("record_outcome failed: {error:?}"))?;
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    let expected_status = if dry_run { "dry_run" } else { "recorded" };
    if report.status.as_str() != expected_status {
        return Err(format!(
            "unexpected outcome status: expected `{expected_status}`, got `{:?}`",
            report.status
        ));
    }

    Ok(elapsed_ms)
}

fn percentile(sorted_samples: &[f64], percentile: f64) -> Result<f64, String> {
    if sorted_samples.is_empty() {
        return Err("percentile requires at least one sample".to_owned());
    }
    let last_index = sorted_samples.len() - 1;
    let raw = (percentile * last_index as f64).round();
    let index = raw.clamp(0.0, last_index as f64) as usize;
    Ok(sorted_samples[index])
}

fn quick_stats_for_scale(memory_count: usize) -> Result<QuickStats, String> {
    let temp_dir = TempDir::new().map_err(|error| format!("temp dir: {error}"))?;
    let _lab_runtime = bind_lab_runtime();
    let fixture = seed_fixture(temp_dir.path(), memory_count);

    // Quick mode uses dry-run feedback events so the budget check stays stable
    // across debug/release profiles and hosts while still exercising validation,
    // target resolution, and aggregate score computation.
    for warmup in 0..QUICK_WARMUP_ITERS {
        let _ = record_once(&fixture, warmup + 1, true)?;
    }

    let mut samples = Vec::with_capacity(QUICK_MEASURE_ITERS);
    for i in 0..QUICK_MEASURE_ITERS {
        samples.push(record_once(&fixture, QUICK_WARMUP_ITERS + i + 1, true)?);
    }
    samples.sort_by(|left, right| left.total_cmp(right));

    Ok(QuickStats {
        p50_ms: percentile(&samples, 0.50)?,
        p99_ms: percentile(&samples, 0.99)?,
    })
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

fn assert_regression_window(scale: &str, stats: &QuickStats) -> Result<(), String> {
    let baseline = load_baseline(scale)?;
    if baseline.p50_ms > TARGET_P50_MS {
        return Err(format!(
            "baseline p50 for scale '{scale}' exceeds target: {:.3}ms > {:.3}ms",
            baseline.p50_ms, TARGET_P50_MS
        ));
    }
    let max_p50 = baseline.p50_ms * (1.0 + REGRESSION_THRESHOLD);
    if stats.p50_ms > max_p50 {
        return Err(format!(
            "p50 regression for scale '{scale}': current {:.3}ms > {:.3}ms baseline ceiling (baseline {:.3}ms, threshold {:.0}%)",
            stats.p50_ms,
            max_p50,
            baseline.p50_ms,
            REGRESSION_THRESHOLD * 100.0
        ));
    }
    let max_p99 = baseline.p99_ms * (1.0 + REGRESSION_THRESHOLD);
    if stats.p99_ms > max_p99 {
        return Err(format!(
            "p99 regression for scale '{scale}': current {:.3}ms > {:.3}ms baseline ceiling (baseline {:.3}ms, threshold {:.0}%)",
            stats.p99_ms,
            max_p99,
            baseline.p99_ms,
            REGRESSION_THRESHOLD * 100.0
        ));
    }
    Ok(())
}

fn assert_hard_budget(scale: &str, stats: &QuickStats) -> Result<(), String> {
    if stats.p50_ms > HARD_CEILING_MS {
        return Err(format!(
            "p50 hard-budget failure for scale '{scale}': {:.3}ms > {:.3}ms",
            stats.p50_ms, HARD_CEILING_MS
        ));
    }
    Ok(())
}

fn run_quick_mode(compare_only: bool) -> Result<(), String> {
    let mut samples = Vec::new();
    for &count in OUTCOME_SCALES {
        let label = scale_label(count);
        let stats = quick_stats_for_scale(count)?;
        assert_hard_budget(label, &stats)?;
        if compare_only {
            assert_regression_window(label, &stats)?;
        }
        samples.push(QuickScaleSample {
            scale: label,
            memory_count: count,
            p50_ms: stats.p50_ms,
            p99_ms: stats.p99_ms,
        });
    }

    let summary = json!({
        "schema": "ee.perf.quick_bench.v1",
        "operation": BENCH_GROUP_NAME,
        "target_p50_ms": TARGET_P50_MS,
        "hard_ceiling_ms": HARD_CEILING_MS,
        "compare_only": compare_only,
        "scales": samples
            .iter()
            .map(|sample| {
                json!({
                    "scale": sample.scale,
                    "memory_count": sample.memory_count,
                    "p50_ms": sample.p50_ms,
                    "p99_ms": sample.p99_ms,
                })
            })
            .collect::<Vec<_>>(),
    });

    let summary_json = serde_json::to_string_pretty(&summary)
        .map_err(|error| format!("failed serializing quick outcome bench summary: {error}"))?;
    if let Some(parent) = std::path::Path::new(QUICK_SUMMARY_PATH).parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed creating quick summary directory {}: {error}",
                parent.display()
            )
        })?;
    }
    std::fs::write(QUICK_SUMMARY_PATH, &summary_json).map_err(|error| {
        format!("failed writing quick summary to {QUICK_SUMMARY_PATH}: {error}")
    })?;
    println!("{summary_json}");
    Ok(())
}

fn run_criterion_mode() {
    let mut criterion = Criterion::default().configure_from_args();

    {
        let mut group = criterion.benchmark_group(BENCH_GROUP_NAME);
        for &count in OUTCOME_SCALES {
            let label = scale_label(count);
            group.bench_with_input(BenchmarkId::new("record", label), &count, |b, &n| {
                let temp_dir = TempDir::new().expect("temp dir");
                let fixture = seed_fixture(temp_dir.path(), n);
                let _lab_runtime = bind_lab_runtime();

                let mut counter = 0usize;
                b.iter(|| {
                    counter += 1;
                    let elapsed_ms = record_once(&fixture, counter, false).unwrap_or_default();
                    black_box(elapsed_ms);
                });
            });
        }
        group.finish();
    }

    {
        let mut group = criterion.benchmark_group("ee_outcome_dry_run");
        let temp_dir = TempDir::new().expect("temp dir");
        let fixture = seed_fixture(temp_dir.path(), 0);
        let _lab_runtime = bind_lab_runtime();
        let mut counter = 0usize;

        group.bench_function("dry_run", |b| {
            b.iter(|| {
                counter += 1;
                let elapsed_ms = record_once(&fixture, counter, true).unwrap_or_default();
                black_box(elapsed_ms);
            });
        });
        group.finish();
    }

    criterion.final_summary();
}

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let quick_mode = args.iter().any(|arg| arg == "--quick");
    let compare_only =
        args.iter().any(|arg| arg == "--compare-only") || compare_only_mode_enabled();

    if quick_mode || compare_only {
        return match run_quick_mode(compare_only) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::from(1)
            }
        };
    }

    run_criterion_mode();
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn benchmark_group_name_is_canonical() {
        assert_eq!(BENCH_GROUP_NAME, "ee_outcome", "canonical group name");
    }

    #[test]
    fn budget_constants_match_plan() {
        assert!(
            (TARGET_P50_MS - 8.0).abs() < f64::EPSILON,
            "p50 target matches plan §28"
        );
        assert!(
            (HARD_CEILING_MS - 30.0).abs() < f64::EPSILON,
            "hard ceiling matches plan §28"
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
    fn baseline_contains_all_outcome_scales() {
        for scale in ["empty", "25_memories", "100_memories"] {
            let baseline = load_baseline(scale);
            assert!(
                baseline.is_ok(),
                "baseline should include scale '{scale}': {baseline:?}"
            );
        }
    }

    #[test]
    fn fixture_seeding_creates_target_memory() {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let fixture = super::seed_fixture(temp_dir.path(), 5);
        assert!(fixture.db_path.exists(), "benchmark db should exist");
        assert!(
            fixture.target_memory_id.starts_with("mem_"),
            "target memory id should use mem_ prefix"
        );
        assert!(temp_dir.path().exists(), "workspace path should exist");
    }

    #[test]
    fn record_outcome_succeeds_on_seeded_fixture() {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let fixture = super::seed_fixture(temp_dir.path(), 1);
        let report = super::record_outcome(&super::OutcomeRecordOptions {
            database_path: fixture.db_path.as_path(),
            target_type: "memory".to_string(),
            target_id: fixture.target_memory_id.clone(),
            workspace_id: None,
            signal: "helpful".to_string(),
            weight: None,
            source_type: "outcome_observed".to_string(),
            source_id: None,
            reason: Some("unit test feedback".to_string()),
            evidence_json: None,
            session_id: None,
            event_id: None,
            actor: Some("unit-test".to_string()),
            dry_run: false,
            harmful_per_source_per_hour: super::DEFAULT_HARMFUL_PER_SOURCE_PER_HOUR,
            harmful_burst_window_seconds: super::DEFAULT_HARMFUL_BURST_WINDOW_SECONDS,
        })
        .expect("record outcome");

        assert_eq!(report.status.as_str(), "recorded");
        assert_eq!(report.target_type, "memory");
        assert_eq!(report.target_id, fixture.target_memory_id);
    }

    #[test]
    fn quick_mode_p50_stays_under_hard_ceiling() {
        let stats = quick_stats_for_scale(100).expect("quick benchmark failed");
        assert!(
            stats.p50_ms <= HARD_CEILING_MS,
            "quick mode p50 {:.3}ms exceeds hard ceiling {:.3}ms",
            stats.p50_ms,
            HARD_CEILING_MS
        );
    }
}
