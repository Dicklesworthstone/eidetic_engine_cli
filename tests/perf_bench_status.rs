use std::fs;
use std::sync::OnceLock;

use ee::core::status::{
    STATUS_BENCH_GROUP_NAME, STATUS_BENCH_HARD_CEILING_MS, StatusBenchReport,
    run_status_bench_quick,
};
use serde_json::Value;
use toml_edit::{DocumentMut, Item};

const BASELINE_PATH: &str = "benches/baselines/v0.1.json";
const BUDGETS_PATH: &str = "benches/budgets.toml";
const REGRESSION_TOLERANCE: f64 = 1.30;
// Debug `cargo test` runs include instrumentation and can execute this
// micro-benchmark under full-suite load. Release benchmarks still use the
// canonical 100ms ceiling; the debug gate only rejects pathological drift.
const DEBUG_CEILING_MULTIPLIER: f64 = 10.0;

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

fn budgets_manifest() -> Result<DocumentMut, String> {
    let content = fs::read_to_string(BUDGETS_PATH)
        .map_err(|error| format!("failed to read `{BUDGETS_PATH}`: {error}"))?;
    content
        .parse::<DocumentMut>()
        .map_err(|error| format!("invalid budgets toml: {error}"))
}

fn baseline_f64(value: &Value, key: &str) -> Result<f64, String> {
    value
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("baseline field `{key}` missing or not numeric"))
}

fn toml_field<'a>(value: &'a Item, key: &str) -> Result<&'a Item, String> {
    value
        .get(key)
        .ok_or_else(|| format!("missing TOML field `{key}`"))
}

fn toml_string(value: &Item, key: &str) -> Result<String, String> {
    toml_field(value, key)?
        .as_value()
        .and_then(|field| field.as_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("TOML field `{key}` must be a string"))
}

fn toml_bool(value: &Item, key: &str) -> Result<bool, String> {
    toml_field(value, key)?
        .as_value()
        .and_then(|field| field.as_bool())
        .ok_or_else(|| format!("TOML field `{key}` must be a boolean"))
}

fn toml_string_array(value: &Item, key: &str) -> Result<Vec<String>, String> {
    let array = toml_field(value, key)?
        .as_value()
        .and_then(|field| field.as_array())
        .ok_or_else(|| format!("TOML field `{key}` must be an array"))?;
    array
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("TOML field `{key}` must contain only strings"))
        })
        .collect()
}

fn effective_hard_ceiling_ms() -> f64 {
    if cfg!(debug_assertions) {
        STATUS_BENCH_HARD_CEILING_MS * DEBUG_CEILING_MULTIPLIER
    } else {
        STATUS_BENCH_HARD_CEILING_MS
    }
}

#[test]
fn bench_script_exposes_rch_safe_profiles_and_report_fields() -> TestResult {
    let source = fs::read_to_string("scripts/bench.sh")
        .map_err(|error| format!("failed to read scripts/bench.sh: {error}"))?;
    for expected in [
        "--profile",
        "ci-smoke",
        "nightly",
        "stress",
        "${TMPDIR:-/tmp}",
        "EE_BENCH_ARTIFACT_DIR",
        "ee.perf.v1",
        "workload_tier",
        "p95_ms",
        "p99_ms",
        "max_rss_kb",
        "allocation_count",
        "rows_per_sec",
    ] {
        if !source.contains(expected) {
            return Err(format!("scripts/bench.sh missing `{expected}`"));
        }
    }

    if source.contains("/tmp/bench_output") {
        return Err("scripts/bench.sh must not depend on hard-coded /tmp temp files".to_owned());
    }

    Ok(())
}

#[test]
fn benchmark_budget_profiles_are_explicit_and_advisory() -> TestResult {
    let manifest = budgets_manifest()?;
    let profiles = manifest
        .get("profiles")
        .ok_or_else(|| "missing TOML field `profiles`".to_owned())?
        .as_table()
        .ok_or_else(|| "`profiles` must be a TOML table".to_owned())?;

    for (name, tier, suitability) in [
        ("ci-smoke", "small", "normal_ci"),
        ("nightly", "medium", "nightly_ci"),
        ("stress", "stress", "local_256gb"),
    ] {
        let profile = profiles
            .get(name)
            .ok_or_else(|| format!("missing benchmark profile `{name}`"))?;
        if toml_string(profile, "workload_tier")? != tier {
            return Err(format!(
                "profile `{name}` must target workload tier `{tier}`"
            ));
        }
        if toml_string(profile, "ci_suitability")? != suitability {
            return Err(format!(
                "profile `{name}` must use CI suitability `{suitability}`"
            ));
        }
        if !toml_bool(profile, "advisory")? {
            return Err(format!("profile `{name}` must start as advisory"));
        }
        if toml_bool(profile, "release_blocking")? {
            return Err(format!(
                "profile `{name}` must not be release-blocking until baselines stabilize"
            ));
        }
        if toml_string_array(profile, "benches")?.is_empty() {
            return Err(format!("profile `{name}` must name at least one bench"));
        }
    }

    let smoke = profiles
        .get("ci-smoke")
        .ok_or_else(|| "missing ci-smoke profile".to_owned())?;
    let smoke_benches = toml_string_array(smoke, "benches")?;
    if smoke_benches != vec!["status".to_owned()] {
        return Err(format!(
            "ci-smoke must run only the status smoke benchmark, got {smoke_benches:?}"
        ));
    }

    Ok(())
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
