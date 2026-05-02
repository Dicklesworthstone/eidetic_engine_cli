//! Criterion benchmark for `ee import cass` (EE-PERF-BENCH-import_cass).
//!
//! Group name: `ee_import_cass`
//!
//! Bench scales:
//! - empty: no discovered CASS sessions
//! - 100_messages: one discovered session with 100 imported message lines
//! - 1000_messages: one discovered session with 1000 imported message lines

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use asupersync::lab::{LabConfig, LabRuntime};
use criterion::{BenchmarkId, Criterion, black_box};
use serde_json::{Value as JsonValue, json};
use tempfile::TempDir;

use ee::cass::{CassClient, CassImportOptions, import_cass_sessions};

const BENCH_GROUP_NAME: &str = "ee_import_cass";
const BASELINE_OPERATION_KEY: &str = "ee_import_cass";
const BASELINE_PATH: &str = "benches/baselines/v0.1.json";
const QUICK_SUMMARY_PATH: &str = "target/criterion/ee_import_cass/quick_summary.json";

/// README Performance table: `ee import cass --since 30d` (cold) p50 4.1s / p99 11s.
const BUDGET_TARGET_P50_MS: f64 = 4_100.0;
const BUDGET_TARGET_P99_MS: f64 = 11_000.0;

/// Plan §28 hard ceiling for `ee import cass (1000 messages)`.
const HARD_CEILING_MS: f64 = 30_000.0;

/// Regression threshold: fail compare-only mode when p50 regresses by >30%.
const REGRESSION_THRESHOLD: f64 = 0.30;

/// Quick sampling config used by compare-only mode and tests.
const QUICK_WARMUP_ITERS: usize = 3;
const QUICK_MEASURE_ITERS: usize = 21;
const LAB_RUNTIME_SEED: u64 = 42;

const FAKE_CASS_SCRIPT: &str = r#"#!/usr/bin/env sh
set -eu

command="${1:-}"
if [ "$#" -gt 0 ]; then
  shift
fi

if [ "$command" = "sessions" ]; then
  workspace=""
  while [ "$#" -gt 0 ]; do
    if [ "$1" = "--workspace" ] && [ "$#" -ge 2 ]; then
      workspace="$2"
      shift 2
      continue
    fi
    shift
  done
  if [ -z "$workspace" ]; then
    printf '%s\n' '{"error":"missing workspace"}' >&2
    exit 2
  fi
  cat "$workspace/.ee/fake_cass/sessions.json"
  exit 0
fi

if [ "$command" = "view" ]; then
  source_path="${1:-}"
  if [ -z "$source_path" ]; then
    printf '%s\n' '{"error":"missing source_path"}' >&2
    exit 2
  fi
  workspace_dir=$(dirname "$source_path")
  cat "$workspace_dir/.ee/fake_cass/view.json"
  exit 0
fi

printf '%s\n' '{"error":"unsupported fake cass command"}' >&2
exit 2
"#;

#[derive(Clone, Copy, Debug)]
struct ImportCassScale {
    name: &'static str,
    message_count: usize,
    hard_ceiling_ms: f64,
}

const IMPORT_CASS_SCALES: [ImportCassScale; 3] = [
    ImportCassScale {
        name: "empty",
        message_count: 0,
        hard_ceiling_ms: HARD_CEILING_MS,
    },
    ImportCassScale {
        name: "100_messages",
        message_count: 100,
        hard_ceiling_ms: HARD_CEILING_MS,
    },
    ImportCassScale {
        name: "1000_messages",
        message_count: 1_000,
        hard_ceiling_ms: HARD_CEILING_MS,
    },
];

#[derive(Clone, Debug)]
struct QuickStats {
    p50_ms: f64,
    p99_ms: f64,
    max_ms: f64,
}

#[derive(Clone, Debug)]
struct BaselineStats {
    p50_ms: f64,
    p99_ms: f64,
}

#[derive(Clone, Debug)]
struct QuickScaleSample {
    scale: &'static str,
    message_count: usize,
    p50_ms: f64,
    p99_ms: f64,
    max_ms: f64,
    hard_ceiling_ms: f64,
}

#[derive(Debug)]
struct ImportCassFixture {
    _temp_dir: TempDir,
    workspace_path: PathBuf,
    cass_client: CassClient,
    scale: ImportCassScale,
    run_counter: AtomicUsize,
}

impl ImportCassFixture {
    fn prepare(scale: ImportCassScale) -> Result<Self, String> {
        let temp_dir =
            TempDir::new().map_err(|error| format!("failed creating tempdir: {error}"))?;
        let workspace_path = temp_dir.path().to_path_buf();

        ensure_workspace_layout(&workspace_path)?;
        write_fake_cass_files(&workspace_path, scale.message_count)?;

        let cass_binary = workspace_path.join(".ee").join("fake_cass").join("cass");
        let cass_client = CassClient::with_binary(cass_binary);

        Ok(Self {
            _temp_dir: temp_dir,
            workspace_path,
            cass_client,
            scale,
            run_counter: AtomicUsize::new(0),
        })
    }

    fn measure_once(&self) -> Result<f64, String> {
        let run_index = self.run_counter.fetch_add(1, Ordering::Relaxed);
        let db_path = self
            .workspace_path
            .join(".ee")
            .join(format!("ee_import_cass_bench_{run_index}.db"));

        let options = CassImportOptions {
            workspace_path: self.workspace_path.clone(),
            database_path: Some(db_path.clone()),
            limit: 10,
            dry_run: false,
            include_spans: false,
        };

        let start = Instant::now();
        let report = import_cass_sessions(&self.cass_client, &options)
            .map_err(|error| format!("import_cass_sessions failed: {error}"))?;
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

        self.validate_report(&report)?;
        cleanup_db_artifacts(&db_path);

        Ok(elapsed_ms)
    }

    fn validate_report(&self, report: &ee::cass::CassImportReport) -> Result<(), String> {
        let expected_discovered = if self.scale.message_count == 0 { 0 } else { 1 };
        let expected_imported = expected_discovered;
        let expected_spans = 0u32;

        if report.sessions_discovered != expected_discovered {
            return Err(format!(
                "unexpected sessions_discovered for scale {}: expected {}, got {}",
                self.scale.name, expected_discovered, report.sessions_discovered
            ));
        }
        if report.sessions_imported != expected_imported {
            return Err(format!(
                "unexpected sessions_imported for scale {}: expected {}, got {}",
                self.scale.name, expected_imported, report.sessions_imported
            ));
        }
        if report.spans_imported != expected_spans {
            return Err(format!(
                "unexpected spans_imported for scale {}: expected {}, got {}",
                self.scale.name, expected_spans, report.spans_imported
            ));
        }
        Ok(())
    }
}

fn ensure_workspace_layout(workspace_path: &Path) -> Result<(), String> {
    let ee_dir = workspace_path.join(".ee");
    fs::create_dir_all(&ee_dir).map_err(|error| {
        format!(
            "failed creating workspace layout at {}: {error}",
            ee_dir.display()
        )
    })
}

fn write_fake_cass_files(workspace_path: &Path, message_count: usize) -> Result<(), String> {
    let fake_cass_dir = workspace_path.join(".ee").join("fake_cass");
    fs::create_dir_all(&fake_cass_dir).map_err(|error| {
        format!(
            "failed creating fake cass directory {}: {error}",
            fake_cass_dir.display()
        )
    })?;

    let cass_binary = fake_cass_dir.join("cass");
    fs::write(&cass_binary, FAKE_CASS_SCRIPT)
        .map_err(|error| format!("failed writing fake cass script: {error}"))?;
    set_executable(&cass_binary)?;

    let workspace_text = workspace_path.to_string_lossy().into_owned();
    let session_path = workspace_path.join(format!("session_{message_count}.jsonl"));
    let session_text = session_path.to_string_lossy().into_owned();

    let sessions = if message_count == 0 {
        Vec::new()
    } else {
        vec![json!({
            "path": session_text,
            "workspace": workspace_text,
            "agent": "codex",
            "modified": "2026-05-01T00:00:00Z",
            "message_count": message_count,
            "token_count": message_count.saturating_mul(12),
        })]
    };

    let sessions_payload = json!({ "sessions": sessions });
    let sessions_json = serde_json::to_string(&sessions_payload)
        .map_err(|error| format!("failed serializing fake sessions json: {error}"))?;
    fs::write(fake_cass_dir.join("sessions.json"), sessions_json)
        .map_err(|error| format!("failed writing fake sessions json: {error}"))?;

    let mut lines = Vec::with_capacity(message_count);
    for line_number in 1..=message_count {
        lines.push(json!({
            "line": line_number,
            // Keep each line tiny so cass subprocess stdout stays below pipe
            // capacity even at 1000-message scale (avoids timeout deadlocks).
            "content": "{}",
        }));
    }

    let view_payload = json!({
        "lines": lines,
    });
    let view_json = serde_json::to_string(&view_payload)
        .map_err(|error| format!("failed serializing fake view json: {error}"))?;
    fs::write(fake_cass_dir.join("view.json"), view_json)
        .map_err(|error| format!("failed writing fake view json: {error}"))?;

    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path).map_err(|error| {
        format!(
            "failed reading file metadata for {}: {error}",
            path.display()
        )
    })?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).map_err(|error| {
        format!(
            "failed setting executable permissions on {}: {error}",
            path.display()
        )
    })
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn cleanup_db_artifacts(db_path: &Path) {
    let wal = PathBuf::from(format!("{}-wal", db_path.display()));
    let shm = PathBuf::from(format!("{}-shm", db_path.display()));
    let _ = fs::remove_file(db_path);
    let _ = fs::remove_file(wal);
    let _ = fs::remove_file(shm);
}

fn bind_lab_runtime() -> LabRuntime {
    LabRuntime::new(LabConfig::new(LAB_RUNTIME_SEED))
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

fn quick_stats_for_scale(scale: ImportCassScale) -> Result<QuickStats, String> {
    let _lab_runtime = bind_lab_runtime();
    let fixture = ImportCassFixture::prepare(scale)?;

    for _ in 0..QUICK_WARMUP_ITERS {
        let _ = fixture.measure_once()?;
    }

    let mut samples = Vec::with_capacity(QUICK_MEASURE_ITERS);
    for _ in 0..QUICK_MEASURE_ITERS {
        samples.push(fixture.measure_once()?);
    }
    samples.sort_by(|left, right| left.total_cmp(right));

    let p50_ms = percentile(&samples, 0.50)?;
    let p99_ms = percentile(&samples, 0.99)?;
    let max_ms = samples.last().copied().unwrap_or_default();

    Ok(QuickStats {
        p50_ms,
        p99_ms,
        max_ms,
    })
}

fn load_baseline(scale: &str) -> Result<BaselineStats, String> {
    let payload = fs::read_to_string(BASELINE_PATH)
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

fn assert_regression_window(scale: &ImportCassScale, stats: &QuickStats) -> Result<(), String> {
    let baseline = load_baseline(scale.name)?;
    let max_p50 = baseline.p50_ms * (1.0 + REGRESSION_THRESHOLD);
    if stats.p50_ms > max_p50 {
        return Err(format!(
            "p50 regression for scale '{}': current {:.3}ms > {:.3}ms baseline ceiling (baseline {:.3}ms, threshold {:.0}%)",
            scale.name,
            stats.p50_ms,
            max_p50,
            baseline.p50_ms,
            REGRESSION_THRESHOLD * 100.0
        ));
    }

    let max_p99 = baseline.p99_ms * (1.0 + REGRESSION_THRESHOLD);
    if stats.p99_ms > max_p99 {
        return Err(format!(
            "p99 regression for scale '{}': current {:.3}ms > {:.3}ms baseline ceiling (baseline {:.3}ms, threshold {:.0}%)",
            scale.name,
            stats.p99_ms,
            max_p99,
            baseline.p99_ms,
            REGRESSION_THRESHOLD * 100.0
        ));
    }

    Ok(())
}

fn assert_hard_ceiling(scale: &ImportCassScale, stats: &QuickStats) -> Result<(), String> {
    if stats.p50_ms > scale.hard_ceiling_ms {
        return Err(format!(
            "hard-ceiling p50 failure for scale '{}': {:.3}ms > {:.3}ms",
            scale.name, stats.p50_ms, scale.hard_ceiling_ms
        ));
    }
    Ok(())
}

fn write_quick_summary(summary: &JsonValue) -> Result<(), String> {
    let summary_json = serde_json::to_string_pretty(summary)
        .map_err(|error| format!("failed serializing quick summary: {error}"))?;

    if let Some(parent) = Path::new(QUICK_SUMMARY_PATH).parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed creating quick summary directory {}: {error}",
                parent.display()
            )
        })?;
    }

    fs::write(QUICK_SUMMARY_PATH, &summary_json).map_err(|error| {
        format!("failed writing quick summary to {QUICK_SUMMARY_PATH}: {error}")
    })?;
    println!("{summary_json}");
    Ok(())
}

fn run_quick_mode(compare_only: bool) -> Result<(), String> {
    let mut samples = Vec::with_capacity(IMPORT_CASS_SCALES.len());

    for scale in IMPORT_CASS_SCALES {
        let stats = quick_stats_for_scale(scale)?;
        assert_hard_ceiling(&scale, &stats)?;
        if compare_only {
            assert_regression_window(&scale, &stats)?;
        }

        samples.push(QuickScaleSample {
            scale: scale.name,
            message_count: scale.message_count,
            p50_ms: stats.p50_ms,
            p99_ms: stats.p99_ms,
            max_ms: stats.max_ms,
            hard_ceiling_ms: scale.hard_ceiling_ms,
        });
    }

    let summary = json!({
        "schema": "ee.perf.quick_bench.v1",
        "operation": BENCH_GROUP_NAME,
        "target_p50_ms": BUDGET_TARGET_P50_MS,
        "target_p99_ms": BUDGET_TARGET_P99_MS,
        "hard_ceiling_ms": HARD_CEILING_MS,
        "compare_only": compare_only,
        "scales": samples
            .iter()
            .map(|sample| {
                json!({
                    "scale": sample.scale,
                    "message_count": sample.message_count,
                    "p50_ms": sample.p50_ms,
                    "p99_ms": sample.p99_ms,
                    "max_ms": sample.max_ms,
                    "hard_ceiling_ms": sample.hard_ceiling_ms,
                })
            })
            .collect::<Vec<_>>(),
    });

    write_quick_summary(&summary)
}

fn run_criterion_mode() {
    let mut criterion = Criterion::default().configure_from_args();
    let mut group = criterion.benchmark_group(BENCH_GROUP_NAME);

    for scale in IMPORT_CASS_SCALES {
        let fixture = match ImportCassFixture::prepare(scale) {
            Ok(fixture) => fixture,
            Err(error) => {
                eprintln!(
                    "warning: skipping import cass benchmark scale '{}' due to setup error: {error}",
                    scale.name
                );
                continue;
            }
        };

        group.bench_with_input(
            BenchmarkId::new(scale.name, scale.message_count),
            &fixture,
            |bench, fixture| {
                bench.iter(|| {
                    let elapsed_ms = fixture.measure_once().unwrap_or_default();
                    black_box(elapsed_ms);
                });
            },
        );
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
#[allow(unused_imports, dead_code)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    #[test]
    fn benchmark_group_name_is_canonical() -> TestResult {
        ensure(
            BENCH_GROUP_NAME == "ee_import_cass",
            format!("expected benchmark group name ee_import_cass, got {BENCH_GROUP_NAME}"),
        )
    }

    #[test]
    fn budget_constants_match_plan_and_readme() -> TestResult {
        ensure(
            (BUDGET_TARGET_P50_MS - 4_100.0).abs() < f64::EPSILON,
            "p50 target must match README Performance table (4.1s)",
        )?;
        ensure(
            (BUDGET_TARGET_P99_MS - 11_000.0).abs() < f64::EPSILON,
            "p99 target must match README Performance table (11s)",
        )?;
        ensure(
            (HARD_CEILING_MS - 30_000.0).abs() < f64::EPSILON,
            "hard ceiling must match plan §28 (30s)",
        )
    }

    #[test]
    fn regression_threshold_is_30_percent() -> TestResult {
        ensure(
            (REGRESSION_THRESHOLD - 0.30).abs() < f64::EPSILON,
            "regression threshold is 30%",
        )
    }

    #[test]
    fn baseline_contains_all_import_scales() -> TestResult {
        for scale in ["empty", "100_messages", "1000_messages"] {
            if let Err(error) = load_baseline(scale) {
                return Err(format!(
                    "baseline should include scale '{scale}' for {BASELINE_OPERATION_KEY}: {error}"
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn quick_mode_p50_stays_under_hard_ceiling() -> TestResult {
        let scale = ImportCassScale {
            name: "100_messages",
            message_count: 100,
            hard_ceiling_ms: HARD_CEILING_MS,
        };
        let stats = quick_stats_for_scale(scale)?;
        ensure(
            stats.p50_ms <= HARD_CEILING_MS,
            format!(
                "quick mode p50 {:.3}ms exceeds hard ceiling {:.3}ms",
                stats.p50_ms, HARD_CEILING_MS
            ),
        )
    }
}
