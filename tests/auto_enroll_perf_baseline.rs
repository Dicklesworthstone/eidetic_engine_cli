use serde_json::Value;
use std::fs;

type TestResult = Result<(), String>;

const BASELINE_PATH: &str = "benches/baselines/auto_enroll_perf_v0.json";

fn load_baseline() -> Result<Value, String> {
    let source = fs::read_to_string(BASELINE_PATH)
        .map_err(|error| format!("failed to read {BASELINE_PATH}: {error}"))?;
    serde_json::from_str(&source).map_err(|error| format!("invalid baseline JSON: {error}"))
}

fn require(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn number_at<'a>(value: &'a Value, pointer: &str) -> Result<f64, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("missing numeric field {pointer}"))
}

#[test]
fn baseline_pins_srr6_46_15_contract_and_margins() -> TestResult {
    let baseline = load_baseline()?;
    require(
        baseline.get("schema").and_then(Value::as_str) == Some("ee.perf.baseline.v1"),
        "baseline schema must be ee.perf.baseline.v1",
    )?;
    require(
        baseline.get("sourceBead").and_then(Value::as_str) == Some("bd-36bbk.1.15"),
        "baseline must cite bd-36bbk.1.15",
    )?;
    require(
        baseline.get("hardware_class").and_then(Value::as_str) == Some("mac-m3-pro"),
        "baseline hardware_class must be mac-m3-pro",
    )?;
    require(
        number_at(&baseline, "/regression_margin/active_p99_pct")? == 15.0,
        "active p99 regression margin must stay at 15 percent",
    )?;
    require(
        number_at(&baseline, "/regression_margin/idle_p99_pct")? == 20.0,
        "idle p99 regression margin must stay at 20 percent",
    )?;
    require(
        number_at(
            &baseline,
            "/regression_margin/idle_rss_slope_mb_per_hour_max",
        )? == 0.7,
        "24h idle RSS slope ceiling must stay at 0.7 MB/h",
    )
}

#[test]
fn baseline_covers_documented_active_idle_and_scale_rows() -> TestResult {
    let baseline = load_baseline()?;
    require(
        baseline
            .get("active_workload_rows")
            .and_then(Value::as_array)
            .map(Vec::len)
            == Some(15),
        "baseline must contain 15 active workload rows",
    )?;
    require(
        baseline
            .get("idle_workload_rows")
            .and_then(Value::as_array)
            .map(Vec::len)
            == Some(4),
        "baseline must contain 4 idle workload rows",
    )?;
    require(
        baseline
            .get("scale_workload_rows")
            .and_then(Value::as_array)
            .map(Vec::len)
            == Some(2),
        "baseline must contain 2 scale workload rows",
    )?;

    for operation in [
        "ee_status_cold_5_peers",
        "ee_status_cold_50_peers",
        "ee_status_warm_5_peers",
        "ee_status_warm_50_peers",
        "ee_mesh_status_cold_5_peers",
        "ee_mesh_status_cold_50_peers",
        "ee_mesh_status_warm_50_peers",
        "ee_mesh_auto_enroll_dry_run_5_peers",
        "ee_mesh_auto_enroll_dry_run_50_peers",
        "ee_mesh_auto_enroll_5_peers",
        "ee_mesh_auto_enroll_50_peers",
        "ee_mesh_status_warm_drift_50_peers",
        "ee_mesh_hello_roundtrip_cold_1_peer",
        "ee_steward_reconciliation_no_drift_50_peers",
        "ee_steward_reconciliation_drift_5_of_50_peers",
        "ee_daemon_idle_rss_1h",
        "ee_daemon_idle_rss_24h",
        "ee_daemon_idle_cpu_1h",
        "ee_daemon_idle_fd_count_1h",
        "ee_mesh_status_refresh_500_peers",
        "ee_mesh_status_cache_hit_500_peers",
    ] {
        let pointer = format!("/operations/{operation}");
        let row = baseline
            .pointer(&pointer)
            .ok_or_else(|| format!("baseline missing operation {operation}"))?;
        require(
            row.get("p50_ms").and_then(Value::as_f64).is_some(),
            format!("{operation} missing p50_ms"),
        )?;
        require(
            row.get("p99_ms").and_then(Value::as_f64).is_some(),
            format!("{operation} missing p99_ms"),
        )?;
    }
    Ok(())
}

#[test]
fn scripts_expose_read_only_auto_enroll_perf_gates() -> TestResult {
    let bench = fs::read_to_string("scripts/bench.sh")
        .map_err(|error| format!("failed to read scripts/bench.sh: {error}"))?;
    for expected in [
        "auto_enroll",
        "auto_enroll_idle_24h",
        "AUTO_ENROLL_BASELINE_ONLY",
        "append_auto_enroll_baseline_rows",
        "auto_enroll_perf_v0.json",
    ] {
        require(
            bench.contains(expected),
            format!("scripts/bench.sh missing {expected}"),
        )?;
    }

    let perf_gate = fs::read_to_string("scripts/e2e_overhaul/auto_enroll_perf_gate.sh")
        .map_err(|error| format!("failed to read auto_enroll_perf_gate.sh: {error}"))?;
    for expected in [
        "Cargo/Rust execution must happen through RCH",
        "EE_AUTO_ENROLL_PERF_REPORT",
        "active_workload_rows | length == 15",
        "idle_workload_rows | length == 4",
        "scale_workload_rows | length == 2",
    ] {
        require(
            perf_gate.contains(expected),
            format!("auto_enroll_perf_gate.sh missing {expected}"),
        )?;
    }

    let idle_gate = fs::read_to_string("scripts/e2e_overhaul/auto_enroll_idle_24h.sh")
        .map_err(|error| format!("failed to read auto_enroll_idle_24h.sh: {error}"))?;
    for expected in [
        "EE_E2E_NIGHTLY=1",
        "EE_AUTO_ENROLL_IDLE_REPORT",
        "exit 78",
        "rss_slope_mb_per_hour_max == 0.7",
    ] {
        require(
            idle_gate.contains(expected),
            format!("auto_enroll_idle_24h.sh missing {expected}"),
        )?;
    }

    Ok(())
}

#[test]
fn verify_contract_tracks_new_srr6_46_e2e_scripts_as_pending() -> TestResult {
    let contract = fs::read_to_string("tests/contracts/auto_enroll_verify_gate_coverage.rs")
        .map_err(|error| {
            format!("failed to read tests/contracts/auto_enroll_verify_gate_coverage.rs: {error}")
        })?;
    for expected in [
        "scripts/e2e_overhaul/auto_enroll_perf_gate.sh",
        "scripts/e2e_overhaul/auto_enroll_idle_24h.sh",
    ] {
        require(
            contract.contains(expected),
            format!("auto_enroll_verify_gate_coverage missing pending script {expected}"),
        )?;
    }
    Ok(())
}
