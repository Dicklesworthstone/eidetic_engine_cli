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

#[test]
fn graph_hits_benchmark_is_registered_and_budgeted() -> TestResult {
    let cargo_toml = fs::read_to_string("Cargo.toml")
        .map_err(|error| format!("failed to read Cargo.toml: {error}"))?;
    let bench_script = fs::read_to_string("scripts/bench.sh")
        .map_err(|error| format!("failed to read scripts/bench.sh: {error}"))?;
    let budgets = fs::read_to_string("benches/budgets.toml")
        .map_err(|error| format!("failed to read benches/budgets.toml: {error}"))?;
    let bench_source = fs::read_to_string("benches/graph_hits.rs")
        .map_err(|error| format!("failed to read benches/graph_hits.rs: {error}"))?;

    for expected in [
        "[[bench]]\nname = \"graph_hits\"\nharness = false",
        "BENCHMARKS=\"remember search context pack_size why outcome status workspace_init audit_query index_rebuild concurrent_writes import_cass link graph_pagerank graph_ppr graph_louvain graph_ktruss graph_gomory_hu graph_hits curate_candidates\"",
        "[operations.ee_graph_hits]",
        "p50_ms_max = 100.0",
        "p99_ms_max = 400.0",
        "10/100/1000 memory-link graphs",
        "bd-jy4w.4",
        "const BENCH_GROUP_NAME: &str = \"graph_hits\";",
        "const BUDGET_P50_MS: f64 = 100.0;",
        "const BUDGET_P99_MS: f64 = 400.0;",
        "const SCALES: &[usize] = &[10, 100, 1000];",
        "std::env::var(\"EE_BENCH_COMPARE_ONLY\")",
        "assert_budget(scale, quick_stats(scale));",
        "compute_hits(&graph)",
    ] {
        let present = cargo_toml.contains(expected)
            || bench_script.contains(expected)
            || budgets.contains(expected)
            || bench_source.contains(expected);
        if !present {
            return Err(format!("HITS benchmark perf contract missing `{expected}`"));
        }
    }

    Ok(())
}

#[test]
fn adaptive_budget_benchmark_reports_percentiles_and_valid_scenarios() -> TestResult {
    let bench_source = fs::read_to_string("benches/adaptive_budget.rs")
        .map_err(|error| format!("failed to read benches/adaptive_budget.rs: {error}"))?;

    for expected in [
        "const BENCH_GROUP_NAME: &str = \"adaptive_budget\";",
        "const PERCENTILE_SUMMARY_RELATIVE_PATH: &str = \"criterion/adaptive_budget/percentiles.json\";",
        "const PERCENTILE_MEASURE_ITERS: usize = 401;",
        "const PERCENTILE_BATCH_ITERS: usize = 128;",
        "const P50_BUDGET_MS: f64 = 2.0;",
        "const P99_TO_P50_MAX_RATIO: f64 = 2.0;",
        "fanout_just_below_cap",
        "fanout_at_cap",
        "fanout_just_above_cap",
        "retrieval_scores_at_1000_elements",
        "scores: skewed_scores(1_000)",
        "let inputs = scenarios",
        "classify_prebuilt_input(black_box(input))",
        "EE_ADAPTIVE_BUDGET_PERCENTILES",
    ] {
        if !bench_source.contains(expected) {
            return Err(format!(
                "adaptive budget benchmark contract missing `{expected}`"
            ));
        }
    }

    for forbidden in [
        "group.sample_size(50)",
        "complex_skewed_high_fanout",
        "fanout: 12.0",
    ] {
        if bench_source.contains(forbidden) {
            return Err(format!(
                "adaptive budget benchmark retained invalid fragment `{forbidden}`"
            ));
        }
    }

    Ok(())
}

#[test]
fn host_calibration_harness_is_invoked_only_and_perf_v1_shaped() -> TestResult {
    let source = fs::read_to_string("scripts/e2e_overhaul/host_calibration.sh")
        .map_err(|error| format!("failed to read host calibration harness: {error}"))?;

    for expected in [
        "bd-1zb7k.12.2",
        "ee.perf.v1",
        "host_profile",
        "index_rebuild",
        "search_json",
        "context_json",
        "pack_query_file",
        "graph_snapshot_dry_run",
        "renderer_markdown",
        "EE_HOST_CALIBRATION_ALLOW_STRESS",
        "EE_HOST_CALIBRATION_REQUIRE_RCH",
        "deterministic synthetic fixture only",
        "samples_count",
        "baseline_ref",
    ] {
        if !source.contains(expected) {
            return Err(format!("host calibration harness missing `{expected}`"));
        }
    }

    Ok(())
}
