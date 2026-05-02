use std::fs;
use std::sync::OnceLock;

use ee::core::status::{
    STATUS_BENCH_GROUP_NAME, STATUS_BENCH_HARD_CEILING_MS, StatusBenchReport,
    run_status_bench_quick,
};
use serde_json::Value;

const BASELINE_PATH: &str = "benches/baselines/v0.1.json";
const REGRESSION_TOLERANCE: f64 = 1.30;
const DEBUG_CEILING_MULTIPLIER: f64 = 3.0;

type TestResult<T = ()> = Result<T, String>;

fn format_scales(report: &StatusBenchReport) -> String {
    report
        .scales
        .iter()
        .map(|sample| {
            format!(
                "{}: memory_count={}, p50={:.3}ms, max={:.3}ms",
                sample.scale_name, sample.memory_count, sample.p50_ms, sample.max_ms
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn quick_report() -> TestResult<&'static StatusBenchReport> {
    static REPORT: OnceLock<Result<StatusBenchReport, String>> = OnceLock::new();
    match REPORT.get_or_init(run_status_bench_quick) {
        Ok(report) => Ok(report),
        Err(error) => Err(format!("quick status benchmark failed: {error}")),
    }
}

fn baseline_operation() -> Result<Value, String> {
    let content = fs::read_to_string(BASELINE_PATH)
        .map_err(|error| format!("failed to read `{BASELINE_PATH}`: {error}"))?;
    let payload: Value = serde_json::from_str(&content)
        .map_err(|error| format!("invalid baseline json: {error}"))?;
    payload
        .pointer("/operations/ee_status")
        .cloned()
        .ok_or_else(|| "baseline missing operations.ee_status".to_owned())
}

fn baseline_f64(value: &Value, key: &str) -> Result<f64, String> {
    value
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("baseline field `{key}` missing or not numeric"))
}

fn effective_hard_ceiling_ms() -> f64 {
    if cfg!(debug_assertions) {
        STATUS_BENCH_HARD_CEILING_MS * DEBUG_CEILING_MULTIPLIER
    } else {
        STATUS_BENCH_HARD_CEILING_MS
    }
}

#[test]
fn status_bench_group_name_is_canonical() -> TestResult {
    if STATUS_BENCH_GROUP_NAME == "ee_status" {
        Ok(())
    } else {
        Err(format!(
            "expected benchmark group name `ee_status`, got `{STATUS_BENCH_GROUP_NAME}`"
        ))
    }
}

#[test]
fn status_bench_source_references_canonical_group_constant() -> TestResult {
    let source = fs::read_to_string("benches/status.rs")
        .map_err(|error| format!("failed to read benches/status.rs: {error}"))?;
    if source.contains("STATUS_BENCH_GROUP_NAME") {
        Ok(())
    } else {
        Err(
            "benches/status.rs must use STATUS_BENCH_GROUP_NAME for criterion group naming"
                .to_owned(),
        )
    }
}

#[test]
fn status_quick_bench_p50_stays_under_hard_ceiling() -> TestResult {
    let report = quick_report()?;
    let hard_ceiling_ms = effective_hard_ceiling_ms();
    if report.aggregate_p50_ms > hard_ceiling_ms {
        return Err(format!(
            "aggregate p50 {:.3}ms exceeded hard ceiling {:.3}ms; scales: {}",
            report.aggregate_p50_ms,
            hard_ceiling_ms,
            format_scales(report)
        ));
    }

    for scale in &report.scales {
        let scale_hard_ceiling_ms = if cfg!(debug_assertions) {
            scale.hard_ceiling_ms * DEBUG_CEILING_MULTIPLIER
        } else {
            scale.hard_ceiling_ms
        };
        if scale.p50_ms > scale_hard_ceiling_ms {
            return Err(format!(
                "scale `{}` p50 {:.3}ms exceeded hard ceiling {:.3}ms",
                scale.scale_name, scale.p50_ms, scale_hard_ceiling_ms
            ));
        }
    }

    Ok(())
}

#[test]
fn status_quick_bench_compare_mode_regression_guard() -> TestResult {
    let report = quick_report()?;
    let operation = baseline_operation()?;

    let baseline_p50 = baseline_f64(&operation, "p50_ms")?;
    let mut allowed_p50 = baseline_p50 * REGRESSION_TOLERANCE;
    if cfg!(debug_assertions) {
        allowed_p50 = allowed_p50.max(effective_hard_ceiling_ms());
    }
    if report.aggregate_p50_ms > allowed_p50 {
        return Err(format!(
            "aggregate p50 regression: {:.3}ms > {:.3}ms (baseline {:.3}ms, tolerance {:.2}x); scales: {}",
            report.aggregate_p50_ms,
            allowed_p50,
            baseline_p50,
            REGRESSION_TOLERANCE,
            format_scales(report)
        ));
    }

    let scales = operation
        .get("scales")
        .and_then(Value::as_object)
        .ok_or_else(|| "baseline missing operations.ee_status.scales object".to_owned())?;
    for sample in &report.scales {
        let Some(scale_baseline) = scales.get(sample.scale_name) else {
            return Err(format!(
                "baseline missing scale `{}` for ee_status",
                sample.scale_name
            ));
        };
        let baseline_scale_p50 = baseline_f64(scale_baseline, "p50_ms")?;
        let mut allowed_scale = baseline_scale_p50 * REGRESSION_TOLERANCE;
        if cfg!(debug_assertions) {
            allowed_scale = allowed_scale.max(sample.hard_ceiling_ms * DEBUG_CEILING_MULTIPLIER);
        }
        if sample.p50_ms > allowed_scale {
            return Err(format!(
                "scale `{}` p50 regression: {:.3}ms > {:.3}ms (baseline {:.3}ms, tolerance {:.2}x)",
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
