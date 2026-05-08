//! Golden tests for perf-forensics compare output (mwjq.5).
//!
//! These tests freeze the JSON contract for compare reports using synthetic
//! fixtures. All timing values use fixed synthetic values (not live measurements)
//! so diffs are deterministic.
//!
//! Update workflow: run this test with `INSTA_UPDATE=always`, review every
//! `tests/snapshots/perf_compare_golden__*.snap` diff, then commit only the
//! accepted `.snap` files with the test change.

use ee::core::perf_forensics::{
    ArtifactKind, ArtifactSide, ArtifactSummary, CompareResult, MetricValue, RedactionPosture,
    Severity, SummaryDegradation, check_perf_budget_summary, compare_artifacts,
};
use ee::models::{
    ArtifactKind as ModelArtifactKind, ArtifactSummary as ModelArtifactSummary,
    MetricValue as ModelMetricValue, ProfileReference,
};
use insta::assert_json_snapshot;

fn benchmark_baseline() -> ArtifactSummary {
    ArtifactSummary::new("baseline-bench-001", ArtifactKind::BenchmarkReport)
        .with_metric("search_elapsed_ms", MetricValue::timing(100.0))
        .with_metric("pack_tokens", MetricValue::stable(4000.0))
        .with_metric("candidate_count", MetricValue::stable(50.0))
        .with_metric("cache_hit_rate", MetricValue::stable(0.85))
        .with_metric("memory_rss_mb", MetricValue::resource(256.0, "MB"))
}

fn benchmark_candidate_unchanged() -> ArtifactSummary {
    ArtifactSummary::new("candidate-bench-001", ArtifactKind::BenchmarkReport)
        .with_metric("search_elapsed_ms", MetricValue::timing(102.0))
        .with_metric("pack_tokens", MetricValue::stable(4000.0))
        .with_metric("candidate_count", MetricValue::stable(50.0))
        .with_metric("cache_hit_rate", MetricValue::stable(0.85))
        .with_metric("memory_rss_mb", MetricValue::resource(258.0, "MB"))
}

fn benchmark_candidate_latency_regression() -> ArtifactSummary {
    ArtifactSummary::new("candidate-bench-latency-regression", ArtifactKind::BenchmarkReport)
        .with_metric("search_elapsed_ms", MetricValue::timing(280.0)) // 180% of baseline
        .with_metric("pack_tokens", MetricValue::stable(4000.0))
        .with_metric("candidate_count", MetricValue::stable(50.0))
        .with_metric("cache_hit_rate", MetricValue::stable(0.85))
        .with_metric("memory_rss_mb", MetricValue::resource(260.0, "MB"))
}

fn benchmark_candidate_memory_regression() -> ArtifactSummary {
    ArtifactSummary::new(
        "candidate-bench-memory-regression",
        ArtifactKind::BenchmarkReport,
    )
    .with_metric("search_elapsed_ms", MetricValue::timing(105.0))
    .with_metric("pack_tokens", MetricValue::stable(4000.0))
    .with_metric("candidate_count", MetricValue::stable(50.0))
    .with_metric("cache_hit_rate", MetricValue::stable(0.85))
    .with_metric("memory_rss_mb", MetricValue::resource(768.0, "MB")) // 3x baseline
}

fn benchmark_candidate_cache_regression() -> ArtifactSummary {
    ArtifactSummary::new("candidate-bench-cache-regression", ArtifactKind::BenchmarkReport)
        .with_metric("search_elapsed_ms", MetricValue::timing(150.0))
        .with_metric("pack_tokens", MetricValue::stable(4000.0))
        .with_metric("candidate_count", MetricValue::stable(50.0))
        .with_metric("cache_hit_rate", MetricValue::stable(0.35)) // dropped from 0.85
        .with_metric("memory_rss_mb", MetricValue::resource(256.0, "MB"))
}

fn benchmark_candidate_improved() -> ArtifactSummary {
    ArtifactSummary::new("candidate-bench-improved", ArtifactKind::BenchmarkReport)
        .with_metric("search_elapsed_ms", MetricValue::timing(40.0)) // 60% faster
        .with_metric("pack_tokens", MetricValue::stable(3500.0)) // fewer tokens
        .with_metric("candidate_count", MetricValue::stable(50.0))
        .with_metric("cache_hit_rate", MetricValue::stable(0.92)) // better hit rate
        .with_metric("memory_rss_mb", MetricValue::resource(200.0, "MB")) // less memory
}

fn benchmark_candidate_missing_metric() -> ArtifactSummary {
    ArtifactSummary::new("candidate-bench-missing", ArtifactKind::BenchmarkReport)
        .with_metric("search_elapsed_ms", MetricValue::timing(105.0))
        .with_metric("pack_tokens", MetricValue::stable(4000.0))
        // cache_hit_rate missing
        // memory_rss_mb missing
        .with_metric("candidate_count", MetricValue::stable(50.0))
}

fn benchmark_baseline_smoke_tier() -> ArtifactSummary {
    let mut summary = benchmark_baseline();
    summary.fixture_tier = Some("smoke".to_string());
    summary
}

fn benchmark_candidate_stress_tier() -> ArtifactSummary {
    let mut summary = benchmark_candidate_unchanged();
    summary.fixture_tier = Some("stress".to_string());
    summary
}

fn benchmark_candidate_redaction_uncertain() -> ArtifactSummary {
    let mut summary = benchmark_candidate_unchanged()
        .with_degradation(SummaryDegradation::redaction_uncertain("request.query"));
    summary.redaction = RedactionPosture {
        applied: true,
        uncertain_fields: vec!["request.query".to_string()],
        secret_indicators: 2,
    };
    summary
}

fn benchmark_candidate_unsupported_artifact() -> ArtifactSummary {
    benchmark_candidate_unchanged()
        .with_degradation(SummaryDegradation::unsupported_kind("legacy_profiler_dump"))
}

fn profile_baseline() -> ArtifactSummary {
    let mut summary = ArtifactSummary::new("baseline-profile-001", ArtifactKind::ProfileEvidence);
    summary.profile = Some("workstation".to_string());
    summary
        .with_metric(
            "profile.budgets.search_candidate_limit",
            MetricValue::stable(5000.0),
        )
        .with_metric(
            "profile.budgets.pack_max_tokens",
            MetricValue::stable(8000.0),
        )
        .with_metric(
            "profile.budgets.cache_memory_cap_mb",
            MetricValue::stable(256.0),
        )
}

fn profile_candidate_mismatch() -> ArtifactSummary {
    let mut summary =
        ArtifactSummary::new("candidate-profile-mismatch", ArtifactKind::ProfileEvidence);
    summary.profile = Some("swarm".to_string()); // different profile
    summary
        .with_metric(
            "profile.budgets.search_candidate_limit",
            MetricValue::stable(50000.0),
        )
        .with_metric(
            "profile.budgets.pack_max_tokens",
            MetricValue::stable(32000.0),
        )
        .with_metric(
            "profile.budgets.cache_memory_cap_mb",
            MetricValue::stable(2048.0),
        )
}

fn bundle_baseline() -> ArtifactSummary {
    let mut summary =
        ArtifactSummary::new("baseline-bundle-001", ArtifactKind::SupportBundleManifest);
    summary.content_hash = Some("sha256:abc123def456".to_string());
    summary.observed_hash = Some("sha256:abc123def456".to_string());
    summary
        .with_metric("artifact_count", MetricValue::stable(12.0))
        .with_metric("total_size_bytes", MetricValue::stable(524288.0))
}

fn bundle_tampered() -> ArtifactSummary {
    let mut summary = ArtifactSummary::new(
        "candidate-bundle-tampered",
        ArtifactKind::SupportBundleManifest,
    );
    summary.content_hash = Some("sha256:abc123def456".to_string());
    summary.observed_hash = Some("sha256:999888777666".to_string()); // tampered
    summary
        .with_metric("artifact_count", MetricValue::stable(12.0))
        .with_metric("total_size_bytes", MetricValue::stable(524288.0))
}

fn write_queue_baseline() -> ArtifactSummary {
    ArtifactSummary::new("baseline-wq-001", ArtifactKind::WriteQueueReport)
        .with_metric("queue_depth", MetricValue::stable(5.0))
        .with_metric("batch_size", MetricValue::stable(10.0))
        .with_metric("backpressure_events", MetricValue::stable(0.0))
}

fn write_queue_regression() -> ArtifactSummary {
    ArtifactSummary::new("candidate-wq-regression", ArtifactKind::WriteQueueReport)
        .with_metric("queue_depth", MetricValue::stable(85.0)) // high queue depth
        .with_metric("batch_size", MetricValue::stable(10.0))
        .with_metric("backpressure_events", MetricValue::stable(12.0)) // backpressure hit
}

fn budget_artifact_workstation() -> ModelArtifactSummary {
    let mut artifact = ModelArtifactSummary::new(
        "budget-candidate-workstation",
        ModelArtifactKind::BenchmarkReport,
        "ee.benchmark.report.v1",
    )
    .with_profile(ProfileReference {
        profile_name: "workstation".to_string(),
        confidence: Some("high".to_string()),
        override_source: None,
    })
    .with_fixture_tier("smoke")
    .with_command_family("perf-compare");
    artifact.add_metric("search_elapsed_ms", ModelMetricValue::measured(96.0, "ms"));
    artifact.add_metric("pack_tokens", ModelMetricValue::measured(3800.0, "tokens"));
    artifact.add_metric("cache_hit_rate", ModelMetricValue::measured(0.86, "ratio"));
    artifact
}

#[test]
fn perf_compare_unchanged_golden() {
    let baseline = benchmark_baseline();
    let candidate = benchmark_candidate_unchanged();
    let report = compare_artifacts(&baseline, &candidate);

    assert_eq!(report.summary.result, CompareResult::Unchanged);
    assert_json_snapshot!("perf_compare_unchanged", report);
}

#[test]
fn perf_compare_latency_regression_golden() {
    let baseline = benchmark_baseline();
    let candidate = benchmark_candidate_latency_regression();
    let report = compare_artifacts(&baseline, &candidate);

    assert_eq!(report.summary.result, CompareResult::Regressed);
    assert_json_snapshot!("perf_compare_latency_regression", report);
}

#[test]
fn perf_compare_memory_regression_golden() {
    let baseline = benchmark_baseline();
    let candidate = benchmark_candidate_memory_regression();
    let report = compare_artifacts(&baseline, &candidate);

    assert_eq!(report.summary.result, CompareResult::Regressed);
    assert_json_snapshot!("perf_compare_memory_regression", report);
}

#[test]
fn perf_compare_cache_regression_golden() {
    let baseline = benchmark_baseline();
    let candidate = benchmark_candidate_cache_regression();
    let report = compare_artifacts(&baseline, &candidate);

    assert_eq!(report.summary.result, CompareResult::Regressed);
    assert_json_snapshot!("perf_compare_cache_regression", report);
}

#[test]
fn perf_compare_improved_golden() {
    let baseline = benchmark_baseline();
    let candidate = benchmark_candidate_improved();
    let report = compare_artifacts(&baseline, &candidate);

    assert_eq!(report.summary.result, CompareResult::Improved);
    assert_json_snapshot!("perf_compare_improved", report);
}

#[test]
fn perf_compare_missing_metric_golden() {
    let baseline = benchmark_baseline();
    let candidate = benchmark_candidate_missing_metric();
    let report = compare_artifacts(&baseline, &candidate);

    assert!(report.degraded.iter().any(|d| d.code == "metric_missing"));
    assert_json_snapshot!("perf_compare_missing_metric", report);
}

#[test]
fn perf_compare_fixture_tier_mismatch_golden() {
    let baseline = benchmark_baseline_smoke_tier();
    let candidate = benchmark_candidate_stress_tier();
    let report = compare_artifacts(&baseline, &candidate);

    assert!(
        report.degraded.iter().any(|d| {
            d.code == "fixture_tier_mismatch" && d.artifact_side == ArtifactSide::Both
        })
    );
    assert_json_snapshot!("perf_compare_fixture_tier_mismatch", report);
}

#[test]
fn perf_compare_unsupported_artifact_golden() {
    let baseline = benchmark_baseline();
    let candidate = benchmark_candidate_unsupported_artifact();
    let report = compare_artifacts(&baseline, &candidate);

    assert!(report.degraded.iter().any(|d| {
        d.code == "unsupported_artifact_kind"
            && d.severity == Severity::High
            && d.artifact_side == ArtifactSide::Candidate
    }));
    assert_json_snapshot!("perf_compare_unsupported_artifact", report);
}

#[test]
fn perf_compare_redaction_uncertainty_golden() {
    let synthetic_secret = "sk_live_deterministic_fixture_secret";
    let baseline = benchmark_baseline();
    let candidate = benchmark_candidate_redaction_uncertain();
    let report = compare_artifacts(&baseline, &candidate);
    let json = serde_json::to_string(&report).expect("compare report should serialize");

    assert!(report.degraded.iter().any(|d| {
        d.code == "redaction_uncertain" && d.artifact_side == ArtifactSide::Candidate
    }));
    assert!(
        !json.contains(synthetic_secret),
        "raw synthetic secret must never appear in golden compare output"
    );
    assert_json_snapshot!("perf_compare_redaction_uncertainty", report);
}

#[test]
fn perf_compare_profile_mismatch_golden() {
    let baseline = profile_baseline();
    let candidate = profile_candidate_mismatch();
    let report = compare_artifacts(&baseline, &candidate);

    // Profile mismatch should produce degradation
    assert!(report.degraded.iter().any(|d| d.code == "profile_mismatch"));
    assert_json_snapshot!("perf_compare_profile_mismatch", report);
}

#[test]
fn perf_compare_tampered_hash_golden() {
    let baseline = bundle_baseline();
    let candidate = bundle_tampered();
    let report = compare_artifacts(&baseline, &candidate);

    assert_eq!(report.summary.result, CompareResult::Inconclusive);
    assert!(report.degraded.iter().any(|d| d.code == "tampered_hash"));
    assert_json_snapshot!("perf_compare_tampered_hash", report);
}

#[test]
fn perf_compare_write_queue_regression_golden() {
    let baseline = write_queue_baseline();
    let candidate = write_queue_regression();
    let report = compare_artifacts(&baseline, &candidate);

    assert_eq!(report.summary.result, CompareResult::Regressed);
    // Should hint at write_spool subsystem
    assert!(
        report
            .owner_hints
            .iter()
            .any(|h| { h.owner == ee::core::perf_forensics::SubsystemOwner::WriteSpool })
    );
    assert_json_snapshot!("perf_compare_write_queue_regression", report);
}

#[test]
fn perf_budget_check_workstation_golden() {
    let report = check_perf_budget_summary("workstation", &budget_artifact_workstation());

    assert_eq!(
        report.summary.result,
        ee::core::perf_forensics::BudgetCheckResult::Passed
    );
    assert_json_snapshot!("perf_budget_check_workstation", report);
}
