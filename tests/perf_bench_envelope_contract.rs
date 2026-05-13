use std::fs;

use serde_json::{Value, json};

type TestResult<T = ()> = Result<T, String>;

const GOLDEN: &str = include_str!("fixtures/golden/perf_artifact/bench_envelope_v1.golden");

fn assert_operation_shape(operation: &Value, name: &str) -> TestResult {
    for field in [
        "status",
        "profile",
        "workload_tier",
        "p50_ms",
        "p99_ms",
        "samples_count",
        "regression_status",
        "baseline_ref",
    ] {
        if operation.get(field).is_none() {
            return Err(format!("operation `{name}` missing `{field}`"));
        }
    }

    let baseline_ref = operation
        .get("baseline_ref")
        .ok_or_else(|| format!("operation `{name}` missing baseline_ref"))?;
    for field in ["file", "operation"] {
        if baseline_ref.get(field).and_then(Value::as_str).is_none() {
            return Err(format!("operation `{name}` baseline_ref missing `{field}`"));
        }
    }

    Ok(())
}

#[test]
fn perf_v1_envelope_golden_shape_is_stable() -> TestResult {
    let envelope = json!({
      "schema": "ee.perf.v1",
      "profile": "ci-smoke",
      "profile_class": "normal_ci",
      "timestamp": "2026-05-13T00:00:00Z",
      "version": "0.1.0",
      "git_sha": "0000000",
      "target_dir": "/Volumes/USBNVME16TB/temp_agent_space/cargo-target",
      "criterion_dir": "/Volumes/USBNVME16TB/temp_agent_space/cargo-target/criterion",
      "artifact_dir": "/Volumes/USBNVME16TB/temp_agent_space/cargo-target/ee-bench",
      "budget_mode": "advisory",
      "release_blocking": false,
      "artifact_redaction": {
        "status": "redaction_safe",
        "raw_secret_material": "not_used",
        "policy": "synthetic placeholders only; command artifacts are JSON/stderr files under artifact_dir"
      },
      "workload": {
        "schema": "ee.perf.workload_ref.v1",
        "manifest": "tests/fixtures/swarm_scale/workloads.json",
        "tier": "small"
      },
      "operations": {
        "ee_status": {
          "status": "measured",
          "profile": "ci-smoke",
          "workload_tier": "small",
          "p50_ms": 12.5,
          "p95_ms": 20.0,
          "p99_ms": 25.0,
          "samples_count": 10,
          "max_ms": 30.0,
          "max_rss_kb": null,
          "allocation_count": null,
          "db_size_bytes": null,
          "index_size_bytes": null,
          "rows_per_sec": null,
          "regression_status": "within_budget",
          "baseline_ref": {
            "file": "benches/baselines/perf_v0_2.json",
            "operation": "ee_status"
          },
          "budget_mode": "advisory"
        }
      },
      "budgets_file": "benches/budgets.toml",
      "baseline_file": "benches/baselines/perf_v0_2.json"
    });

    assert_eq!(
        serde_json::to_string_pretty(&envelope).map_err(|error| error.to_string())?,
        GOLDEN.trim(),
        "ee.perf.v1 envelope shape changed; update the golden with an intentional schema change"
    );
    assert_operation_shape(&envelope["operations"]["ee_status"], "ee_status")?;
    Ok(())
}

#[test]
fn bench_script_emits_perf_v1_operation_contract_fields() -> TestResult {
    let source = fs::read_to_string("scripts/bench.sh")
        .map_err(|error| format!("failed to read scripts/bench.sh: {error}"))?;

    for expected in [
        "ee.perf.v1",
        "workload",
        "operations",
        "p50_ms",
        "p99_ms",
        "samples_count",
        "regression_status",
        "baseline_ref",
        "baseline_file",
    ] {
        if !source.contains(expected) {
            return Err(format!("scripts/bench.sh missing `{expected}`"));
        }
    }

    Ok(())
}
