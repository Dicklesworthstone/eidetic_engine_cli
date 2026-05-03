//! Criterion benchmark for `ee curate candidates` (EE-PERF-BENCH-curate_candidates).
//!
//! Group name: `ee_curate_candidates`
//!
//! Bench scales:
//! - empty: no curation candidates
//! - 50_candidates: representative queue size for one review pass
//! - 5000_candidates: large queue stress case

use std::path::{Path, PathBuf};
use std::time::Instant;

use asupersync::lab::{LabConfig, LabRuntime};
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use serde_json::Value as JsonValue;
use tempfile::TempDir;

use ee::core::curate::{CurateCandidatesOptions, list_curation_candidates};
use ee::core::memory::{RememberMemoryOptions, remember_memory};
use ee::db::{CreateCurationCandidateInput, DbConnection};

const BENCH_GROUP_NAME: &str = "ee_curate_candidates";
const BASELINE_OPERATION_KEY: &str = "ee_curate_candidates";
const BASELINE_PATH: &str = "benches/baselines/v0.1.json";

/// Performance budget from plan §28.
/// Curate candidates (50 episodic) target p50: 800ms, hard ceiling: 5000ms.
const BUDGET_P50_MS: f64 = 800.0;
const BUDGET_P99_MS: f64 = 5000.0;

/// Regression threshold: fail compare-only mode when p50 regresses by >30%.
const REGRESSION_THRESHOLD: f64 = 0.30;

/// Quick sampling config used by compare-only mode.
const QUICK_WARMUP_ITERS: usize = 5;
const QUICK_MEASURE_ITERS: usize = 31;
const LAB_RUNTIME_SEED: u64 = 42;

#[derive(Clone, Debug)]
struct CurateFixture {
    workspace_path: PathBuf,
    db_path: PathBuf,
    candidate_count: usize,
}

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
        50 => "50_candidates",
        5000 => "5000_candidates",
        _ => "unknown",
    }
}

fn candidate_id(index: usize) -> String {
    format!("curate_{index:026}")
}

fn ensure_workspace_layout(workspace_path: &Path) {
    let ee_dir = workspace_path.join(".ee");
    if let Err(error) = std::fs::create_dir_all(&ee_dir) {
        panic!("failed to create workspace .ee directory: {error}");
    }
}

fn seed_fixture(workspace_path: &Path, candidate_count: usize) -> CurateFixture {
    ensure_workspace_layout(workspace_path);
    let workspace_path = workspace_path.to_path_buf();
    let db_path = workspace_path.join(".ee").join("ee.db");

    let memory_report = remember_memory(&RememberMemoryOptions {
        workspace_path: &workspace_path,
        database_path: Some(&db_path),
        content: "Curate benchmark target memory.",
        level: "episodic",
        kind: "fact",
        tags: Some("bench,curate,target"),
        confidence: 0.72,
        source: None,
        valid_from: None,
        valid_to: None,
        dry_run: false,
    })
    .unwrap_or_else(|error| panic!("failed creating curate benchmark target memory: {error:?}"));

    let target_memory_id = memory_report.memory_id.to_string();
    let connection = DbConnection::open_file(&db_path)
        .unwrap_or_else(|error| panic!("failed opening curate benchmark db: {error}"));
    if let Err(error) = connection.migrate() {
        panic!("failed migrating curate benchmark db: {error}");
    }

    let workspace_id = connection
        .list_workspaces()
        .unwrap_or_else(|error| panic!("failed listing workspaces for benchmark fixture: {error}"))
        .first()
        .map(|workspace| workspace.id.clone())
        .unwrap_or_else(|| panic!("benchmark fixture workspace row missing"));

    for index in 0..candidate_count {
        let input = CreateCurationCandidateInput {
            workspace_id: workspace_id.clone(),
            candidate_type: "promote".to_owned(),
            target_memory_id: target_memory_id.clone(),
            proposed_content: Some(format!("Proposed curated content #{index}.")),
            proposed_confidence: Some(0.73),
            proposed_trust_class: Some("medium".to_owned()),
            source_type: "agent_inference".to_owned(),
            source_id: Some(format!("sess_{index:08}")),
            reason: "benchmark-seed".to_owned(),
            confidence: 0.66,
            status: Some("pending".to_owned()),
            created_at: None,
            ttl_expires_at: None,
        };

        let id = candidate_id(index);
        if let Err(error) = connection.insert_curation_candidate(&id, &input) {
            panic!("failed inserting benchmark curation candidate {id}: {error}");
        }
    }

    CurateFixture {
        workspace_path,
        db_path,
        candidate_count,
    }
}

fn bind_lab_runtime() -> LabRuntime {
    LabRuntime::new(LabConfig::new(LAB_RUNTIME_SEED))
}

fn run_curate_candidates_once(fixture: &CurateFixture) -> f64 {
    let start = Instant::now();

    let options = CurateCandidatesOptions {
        workspace_path: &fixture.workspace_path,
        database_path: Some(&fixture.db_path),
        candidate_type: None,
        status: Some("pending"),
        target_memory_id: None,
        limit: 1000,
        offset: 0,
        sort: "review_state",
        group_duplicates: false,
    };

    let report = list_curation_candidates(&options)
        .unwrap_or_else(|error| panic!("curate candidates benchmark call failed: {error:?}"));
    assert_eq!(
        report.total_count, fixture.candidate_count,
        "candidate count must match seeded fixture"
    );

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

fn quick_stats_for_scale(candidate_count: usize) -> QuickStats {
    let temp_dir = TempDir::new()
        .unwrap_or_else(|error| panic!("failed to create tempdir for curate benchmark: {error}"));
    let workspace_path = temp_dir.path().to_path_buf();

    let _lab_runtime = bind_lab_runtime();
    let fixture = seed_fixture(&workspace_path, candidate_count);

    for _ in 0..QUICK_WARMUP_ITERS {
        let _ = run_curate_candidates_once(&fixture);
    }

    let mut samples = Vec::with_capacity(QUICK_MEASURE_ITERS);
    for _ in 0..QUICK_MEASURE_ITERS {
        samples.push(run_curate_candidates_once(&fixture));
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

fn bench_curate_candidates(c: &mut Criterion) {
    if compare_only_mode_enabled() {
        for &count in &[0usize, 50, 5000] {
            let label = scale_label(count);
            let stats = quick_stats_for_scale(count);
            assert_regression_window(label, &stats);
        }
        return;
    }

    let mut group = c.benchmark_group(BENCH_GROUP_NAME);
    for &count in &[0usize, 50, 5000] {
        let label = scale_label(count);
        group.bench_with_input(
            BenchmarkId::new("curate_candidates", label),
            &count,
            |b, &n| {
                let temp_dir = TempDir::new()
                    .unwrap_or_else(|error| panic!("failed to create benchmark tempdir: {error}"));
                let workspace_path = temp_dir.path().to_path_buf();
                let _lab_runtime = bind_lab_runtime();
                let fixture = seed_fixture(&workspace_path, n);

                b.iter(|| {
                    let elapsed_ms = run_curate_candidates_once(&fixture);
                    black_box(elapsed_ms);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_curate_candidates);
criterion_main!(benches);

#[cfg(test)]
mod tests {
    #[test]
    fn benchmark_group_name_is_canonical() {
        assert_eq!(
            super::BENCH_GROUP_NAME,
            "ee_curate_candidates",
            "canonical group name"
        );
    }

    #[test]
    fn budget_constants_match_plan() {
        assert!(
            (super::BUDGET_P50_MS - 800.0).abs() < f64::EPSILON,
            "p50 budget matches plan §28"
        );
        assert!(
            (super::BUDGET_P99_MS - 5000.0).abs() < f64::EPSILON,
            "p99 budget matches plan §28"
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
    fn seeded_candidate_ids_match_database_constraint_shape() {
        let sample = super::candidate_id(42);
        assert!(
            sample.starts_with("curate_"),
            "candidate id must use curate_ prefix"
        );
        assert_eq!(sample.len(), 33, "candidate id length");
    }
}
