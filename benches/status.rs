use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use criterion::{BenchmarkId, Criterion, black_box};
use serde_json::{Value, json};

use ee::core::status::{
    STATUS_BENCH_GROUP_NAME, STATUS_BENCH_SCALES, StatusBenchFixture, StatusBenchReport,
    run_status_bench_quick, status_bench_exceeds_hard_ceiling,
};

const BASELINE_PATH: &str = "benches/baselines/v0.1.json";
const QUICK_SUMMARY_RELATIVE_PATH: &str = "criterion/ee_status/quick_summary.json";
const REGRESSION_TOLERANCE: f64 = 1.30;

fn main() -> ExitCode {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let quick = args.iter().any(|arg| arg == "--quick");
    let compare_only = args.iter().any(|arg| arg == "--compare-only");
    let advisory = args.iter().any(|arg| arg == "--advisory");

    if quick || compare_only {
        return match run_quick_mode(compare_only, advisory) {
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

fn run_quick_mode(compare_only: bool, advisory: bool) -> Result<(), String> {
    let report = run_status_bench_quick()?;
    let exceeds_hard_ceiling = status_bench_exceeds_hard_ceiling(&report);

    if compare_only {
        compare_against_baseline(&report)?;
    }

    let summary_path = quick_summary_path();
    let summary = quick_summary_json(&report, &summary_path, advisory, exceeds_hard_ceiling);
    write_quick_summary(&summary)?;
    println!("{summary}");

    if exceeds_hard_ceiling && !advisory {
        return Err(format!(
            "status benchmark exceeded hard ceiling of {:.2}ms (aggregate p50 {:.2}ms; failing scales: {})",
            report.hard_ceiling_ms,
            report.aggregate_p50_ms,
            failing_scales(&report)
        ));
    }

    Ok(())
}

fn run_criterion_mode() {
    let mut criterion = Criterion::default().configure_from_args();
    bench_status(&mut criterion);
    criterion.final_summary();
}

fn bench_status(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group(STATUS_BENCH_GROUP_NAME);

    for scale in STATUS_BENCH_SCALES {
        let fixture = match StatusBenchFixture::prepare(scale) {
            Ok(fixture) => fixture,
            Err(error) => {
                eprintln!(
                    "warning: skipping status benchmark scale `{}`: {error}",
                    scale.name
                );
                continue;
            }
        };

        group.bench_with_input(
            BenchmarkId::new(scale.name, scale.memory_count),
            &fixture,
            |bench, fixture| {
                bench.iter(|| {
                    let elapsed = fixture.measure_once().unwrap_or_default();
                    black_box(elapsed);
                });
            },
        );
    }

    group.finish();
}

fn compare_against_baseline(report: &StatusBenchReport) -> Result<(), String> {
    let content = fs::read_to_string(BASELINE_PATH)
        .map_err(|error| format!("failed to read baseline file `{BASELINE_PATH}`: {error}"))?;
    let payload: Value = serde_json::from_str(&content)
        .map_err(|error| format!("invalid baseline JSON in `{BASELINE_PATH}`: {error}"))?;

    let operation = payload
        .pointer("/operations/ee_status")
        .ok_or_else(|| "baseline file missing operations.ee_status".to_owned())?;

    let baseline_p50 = value_f64(operation, "p50_ms")?;
    let allowed_p50 = baseline_p50 * REGRESSION_TOLERANCE;
    if report.aggregate_p50_ms > allowed_p50 {
        return Err(format!(
            "aggregate p50 regression: {:.3}ms exceeds allowed {:.3}ms (baseline {:.3}ms, tolerance {:.2}x)",
            report.aggregate_p50_ms, allowed_p50, baseline_p50, REGRESSION_TOLERANCE
        ));
    }

    let scales = operation
        .get("scales")
        .and_then(Value::as_object)
        .ok_or_else(|| "baseline file missing operations.ee_status.scales object".to_owned())?;

    for sample in &report.scales {
        let Some(baseline_scale) = scales.get(sample.scale_name) else {
            return Err(format!(
                "baseline file missing scale `{}` for ee_status",
                sample.scale_name
            ));
        };
        let baseline_scale_p50 = value_f64(baseline_scale, "p50_ms")?;
        let allowed_scale = baseline_scale_p50 * REGRESSION_TOLERANCE;
        if sample.p50_ms > allowed_scale {
            return Err(format!(
                "scale `{}` p50 regression: {:.3}ms exceeds allowed {:.3}ms (baseline {:.3}ms, tolerance {:.2}x)",
                sample.scale_name,
                sample.p50_ms,
                allowed_scale,
                baseline_scale_p50,
                REGRESSION_TOLERANCE
            ));
        }
    }

    Ok(())
}

fn value_f64(object: &Value, key: &str) -> Result<f64, String> {
    object
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("baseline field `{key}` missing or not a number"))
}

fn quick_summary_json(
    report: &StatusBenchReport,
    summary_path: &Path,
    advisory: bool,
    exceeds_hard_ceiling: bool,
) -> String {
    let scales = report
        .scales
        .iter()
        .map(|sample| {
            let regression_status = if sample.p50_ms > sample.hard_ceiling_ms {
                "exceeded_hard_ceiling"
            } else {
                "within_budget"
            };
            json!({
                "scale": sample.scale_name,
                "memory_count": sample.memory_count,
                "iterations": sample.iterations,
                "p50_ms": sample.p50_ms,
                "max_ms": sample.max_ms,
                "hard_ceiling_ms": sample.hard_ceiling_ms,
                "regression_status": regression_status,
            })
        })
        .collect::<Vec<_>>();

    let regression_status = if exceeds_hard_ceiling {
        "exceeded_hard_ceiling"
    } else {
        "within_budget"
    };
    let budget_mode = if advisory { "advisory" } else { "blocking" };
    let payload = json!({
        "schema": "ee.perf.quick_bench.v1",
        "operation": report.operation,
        "iterations_per_scale": report.iterations_per_scale,
        "aggregate_p50_ms": report.aggregate_p50_ms,
        "hard_ceiling_ms": report.hard_ceiling_ms,
        "regression": {
            "status": regression_status,
            "budget_mode": budget_mode,
            "hard_ceiling_exceeded": exceeds_hard_ceiling,
        },
        "scales": scales,
        "quick_summary_path": summary_path.display().to_string(),
    });

    serde_json::to_string_pretty(&payload).unwrap_or_else(|error| {
        format!(
            "{{\"schema\":\"ee.error.v1\",\"error\":\"failed to serialize quick summary: {}\"}}",
            error
        )
    })
}

fn quick_summary_path() -> PathBuf {
    env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"))
        .join(QUICK_SUMMARY_RELATIVE_PATH)
}

fn write_quick_summary(summary_json: &str) -> Result<(), String> {
    let path = quick_summary_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create quick summary directory `{}`: {error}",
                parent.display()
            )
        })?;
    }
    fs::write(&path, summary_json).map_err(|error| {
        format!(
            "failed to write quick summary file `{}`: {error}",
            path.display()
        )
    })
}

fn failing_scales(report: &StatusBenchReport) -> String {
    let scales = report
        .scales
        .iter()
        .filter(|sample| sample.p50_ms > sample.hard_ceiling_ms)
        .map(|sample| {
            format!(
                "{} {:.2}ms>{:.2}ms",
                sample.scale_name, sample.p50_ms, sample.hard_ceiling_ms
            )
        })
        .collect::<Vec<_>>();

    if scales.is_empty() {
        return "aggregate".to_owned();
    }

    scales.join(", ")
}
