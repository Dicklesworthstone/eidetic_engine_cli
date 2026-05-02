//! Criterion benchmark for `ee search` (EE-PERF-BENCH-search).
//!
//! Group name: `ee_search`
//!
//! Bench scales:
//! - empty: fresh workspace without persisted memories
//! - 100_memories: small realistic workspace
//! - 5000_memories: plan-scale workspace

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use asupersync::lab::{LabConfig, LabRuntime};
use criterion::{BenchmarkId, Criterion, black_box};
use serde_json::{Value as JsonValue, json};
use tempfile::TempDir;

use ee::core::index::{IndexRebuildOptions, IndexRebuildStatus, rebuild_index};
use ee::core::search::{SearchOptions, run_search};
use ee::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};
use ee::models::WorkspaceId;

const BENCH_GROUP_NAME: &str = "ee_search";
const BASELINE_OPERATION_KEY: &str = "ee_search";
const BASELINE_PATH: &str = "benches/baselines/v0.1.json";
const QUICK_SUMMARY_PATH: &str = "target/criterion/ee_search/quick_summary.json";

/// Performance budget from plan §28 / README Performance table.
/// Search p50 target: 38ms, p99 target: 110ms.
const BUDGET_P50_MS: f64 = 38.0;
const BUDGET_P99_MS: f64 = 110.0;

/// Regression threshold: fail compare-only mode when p50 regresses by >30%.
const REGRESSION_THRESHOLD: f64 = 0.30;

/// Quick sampling config used by compare-only mode and tests.
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

#[derive(Clone, Debug)]
struct QuickScaleSample {
    scale: &'static str,
    memory_count: usize,
    p50_ms: f64,
    p99_ms: f64,
}

fn scale_label(count: usize) -> &'static str {
    match count {
        0 => "empty",
        100 => "100_memories",
        5000 => "5000_memories",
        _ => "unknown",
    }
}

fn query_for_scale(count: usize) -> &'static str {
    match count {
        0 => "search benchmark empty workspace",
        100 => "search benchmark seeded memory",
        5000 => "search benchmark large workspace retrieval",
        _ => "search benchmark",
    }
}

fn ensure_workspace_layout(workspace_path: &Path) {
    let ee_dir = workspace_path.join(".ee");
    if let Err(error) = std::fs::create_dir_all(&ee_dir) {
        panic!("failed to create workspace .ee directory: {error}");
    }
}

fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn ensure_workspace_row(connection: &DbConnection, workspace_path: &Path) {
    let workspace_path_string = workspace_path.to_string_lossy().into_owned();
    let workspace_exists = match connection.get_workspace_by_path(&workspace_path_string) {
        Ok(record) => record.is_some(),
        Err(error) => panic!("failed to query benchmark workspace row: {error}"),
    };

    if workspace_exists {
        return;
    }

    let workspace_id = stable_workspace_id(workspace_path);
    let input = CreateWorkspaceInput {
        path: workspace_path_string,
        name: workspace_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
    };
    if let Err(error) = connection.insert_workspace(&workspace_id, &input) {
        panic!("failed to insert benchmark workspace row: {error}");
    }
}

fn seed_database(workspace_path: &Path, count: usize) -> PathBuf {
    ensure_workspace_layout(workspace_path);
    let db_path = workspace_path.join(".ee").join("ee.db");
    let connection = match DbConnection::open_file(&db_path) {
        Ok(connection) => connection,
        Err(error) => panic!("failed opening benchmark database: {error}"),
    };
    if let Err(error) = connection.migrate() {
        panic!("failed migrating benchmark database: {error}");
    }
    ensure_workspace_row(&connection, workspace_path);
    let workspace_id = stable_workspace_id(workspace_path);

    for index in 0..count {
        let content = format!(
            "Search benchmark memory {index}: deterministic lexical seed for query coverage."
        );
        let input = CreateMemoryInput {
            workspace_id: workspace_id.clone(),
            level: "semantic".to_owned(),
            kind: "fact".to_owned(),
            content,
            confidence: 0.7,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "human_explicit".to_owned(),
            trust_subclass: Some("search-bench".to_owned()),
            tags: vec!["bench".to_owned(), "search".to_owned(), "seed".to_owned()],
            valid_from: None,
            valid_to: None,
        };
        let memory_id = format!("mem_search_bench_{index:05}");
        if let Err(error) = connection.insert_memory(&memory_id, &input) {
            panic!("failed seeding benchmark memory {index}: {error}");
        }
    }

    db_path
}

fn build_index(workspace_path: &Path, db_path: &Path, count: usize) -> PathBuf {
    let index_dir = workspace_path.join(".ee").join("index");
    let options = IndexRebuildOptions {
        workspace_path: workspace_path.to_path_buf(),
        database_path: Some(db_path.to_path_buf()),
        index_dir: Some(index_dir.clone()),
        dry_run: false,
    };

    let report = match rebuild_index(&options) {
        Ok(report) => report,
        Err(error) => panic!("search benchmark index rebuild failed: {error}"),
    };

    if count == 0 {
        assert!(
            matches!(
                report.status,
                IndexRebuildStatus::NoDocuments | IndexRebuildStatus::Success
            ),
            "empty workspace index rebuild should be no_documents/success, got {}",
            report.status.as_str()
        );
    } else {
        assert_eq!(
            report.status,
            IndexRebuildStatus::Success,
            "expected successful rebuild for seeded workspace, got {} with errors {:?}",
            report.status.as_str(),
            report.errors
        );
    }

    index_dir
}

fn bind_lab_runtime() -> LabRuntime {
    LabRuntime::new(LabConfig::new(LAB_RUNTIME_SEED))
}

fn run_search_once(options: &SearchOptions) -> f64 {
    let start = Instant::now();
    let result = run_search(options);
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    match result {
        Ok(report) => {
            black_box(report.results.len());
            black_box(report.status.as_str());
            black_box(report.elapsed_ms);
        }
        Err(error) => {
            black_box(error.to_string());
        }
    }

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

fn quick_stats_for_scale(count: usize) -> QuickStats {
    let temp_dir = match TempDir::new() {
        Ok(dir) => dir,
        Err(error) => panic!("failed to create tempdir for search bench: {error}"),
    };
    let workspace_path = temp_dir.path().to_path_buf();

    // Bind LabRuntime per bead requirement; runtime-owned flows remain deterministic.
    let _lab_runtime = bind_lab_runtime();

    let db_path = seed_database(&workspace_path, count);
    let index_dir = build_index(&workspace_path, &db_path, count);
    let options = SearchOptions {
        workspace_path: workspace_path.clone(),
        database_path: Some(db_path),
        index_dir: Some(index_dir),
        query: query_for_scale(count).to_owned(),
        limit: 20,
        explain: false,
    };

    for _ in 0..QUICK_WARMUP_ITERS {
        let _ = run_search_once(&options);
    }

    let mut samples = Vec::with_capacity(QUICK_MEASURE_ITERS);
    for _ in 0..QUICK_MEASURE_ITERS {
        samples.push(run_search_once(&options));
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

fn assert_regression_window(scale: &str, stats: &QuickStats) -> Result<(), String> {
    let baseline = load_baseline(scale)?;
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
    if stats.p50_ms > BUDGET_P50_MS {
        return Err(format!(
            "p50 hard-budget failure for scale '{scale}': {:.3}ms > {:.3}ms",
            stats.p50_ms, BUDGET_P50_MS
        ));
    }
    if stats.p99_ms > BUDGET_P99_MS {
        return Err(format!(
            "p99 hard-budget failure for scale '{scale}': {:.3}ms > {:.3}ms",
            stats.p99_ms, BUDGET_P99_MS
        ));
    }
    Ok(())
}

fn run_quick_mode(compare_only: bool) -> Result<(), String> {
    let mut samples = Vec::new();
    for &count in &[0usize, 100, 5000] {
        let label = scale_label(count);
        let stats = quick_stats_for_scale(count);
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
        "hard_ceiling_p50_ms": BUDGET_P50_MS,
        "hard_ceiling_p99_ms": BUDGET_P99_MS,
        "compare_only": compare_only,
        "scales": samples.iter().map(|sample| json!({
            "scale": sample.scale,
            "memory_count": sample.memory_count,
            "p50_ms": sample.p50_ms,
            "p99_ms": sample.p99_ms,
        })).collect::<Vec<_>>()
    });

    let summary_json = serde_json::to_string_pretty(&summary)
        .map_err(|error| format!("failed serializing quick search bench summary: {error}"))?;
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
    let mut group = criterion.benchmark_group(BENCH_GROUP_NAME);
    for &count in &[0usize, 100, 5000] {
        let label = scale_label(count);
        group.bench_with_input(BenchmarkId::new("search", label), &count, |b, &n| {
            let temp_dir = match TempDir::new() {
                Ok(dir) => dir,
                Err(error) => panic!("failed to create benchmark tempdir: {error}"),
            };
            let workspace_path = temp_dir.path().to_path_buf();
            let _lab_runtime = bind_lab_runtime();
            let db_path = seed_database(&workspace_path, n);
            let index_dir = build_index(&workspace_path, &db_path, n);

            let options = SearchOptions {
                workspace_path: workspace_path.clone(),
                database_path: Some(db_path),
                index_dir: Some(index_dir),
                query: query_for_scale(n).to_owned(),
                limit: 20,
                explain: false,
            };

            b.iter(|| {
                let elapsed_ms = run_search_once(&options);
                black_box(elapsed_ms);
            });
        });
    }
    group.finish();
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
#[allow(unused_imports)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_group_name_is_canonical() {
        assert_eq!(BENCH_GROUP_NAME, "ee_search", "canonical group name");
    }

    #[test]
    fn budget_constants_match_plan() {
        assert!(
            (BUDGET_P50_MS - 38.0).abs() < f64::EPSILON,
            "p50 budget matches plan §28/README"
        );
        assert!(
            (BUDGET_P99_MS - 110.0).abs() < f64::EPSILON,
            "p99 budget matches plan §28/README"
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
    fn baseline_contains_all_search_scales() {
        for scale in ["empty", "100_memories", "5000_memories"] {
            let baseline = load_baseline(scale);
            assert!(
                baseline.is_ok(),
                "baseline should include scale '{scale}': {baseline:?}"
            );
        }
    }

    #[test]
    fn quick_mode_p50_stays_under_hard_ceiling() {
        let stats = quick_stats_for_scale(100);
        assert!(
            stats.p50_ms <= BUDGET_P50_MS,
            "quick mode p50 {:.3}ms exceeds hard ceiling {:.3}ms",
            stats.p50_ms,
            BUDGET_P50_MS
        );
    }
}
