//! Executable SRR6.20 checks for two-tier latency, freshness, and budget proof.

#[path = "../src/mesh/anti_entropy_protocol.rs"]
mod anti_entropy_protocol;

use anti_entropy_protocol::{
    DEFAULT_BODY_CACHE_BUDGET_BYTES, DEFAULT_INDEX_JOB_AMPLIFICATION_BUDGET,
    DEFAULT_PEER_PROBE_FANOUT_BUDGET, DEFAULT_PEER_PROBE_TIMEOUT_BUDGET_MS,
    DEFAULT_STALE_READ_WINDOW_BUDGET_MS, DEFAULT_SYNC_BATCH_BUDGET_EVENTS,
    DEFAULT_TIER1_LOCAL_P50_BUDGET_MS, DEFAULT_TIER1_LOCAL_P99_BUDGET_MS,
    MESH_TWO_TIER_BUDGET_SUMMARY_SCHEMA_V1, MeshTwoTierBudgetInput, build_two_tier_budget_summary,
    degraded_codes,
};

type TestResult = Result<(), String>;

#[test]
fn tier1_budget_report_preserves_local_answer_and_cache_hit_path() -> TestResult {
    let summary = build_two_tier_budget_summary(MeshTwoTierBudgetInput {
        mesh_enabled: true,
        baseline_tier1_p50_ms: 38,
        observed_tier1_p50_ms: 42,
        baseline_tier1_p99_ms: 117,
        observed_tier1_p99_ms: 128,
        peer_timeout_ms: 150,
        max_peer_probe_elapsed_ms: 149,
        stale_read_window_ms: 3_000,
        peer_count: 12,
        body_cache_bytes: 128 * 1024,
        sync_batch_events: 128,
        index_jobs_enqueued: 4,
        cache_hit_path_observed: true,
        checked_at: Some("2026-05-20T15:00:00.000Z".to_owned()),
    });

    assert_eq!(summary.schema, MESH_TWO_TIER_BUDGET_SUMMARY_SCHEMA_V1);
    assert_eq!(summary.status, "within_budget");
    assert_eq!(summary.degraded, Vec::<String>::new());
    assert!(summary.mesh_enabled);
    assert!(!summary.local_answer_blocking);
    assert!(!summary.network_on_tier1);
    assert!(summary.cache_hit_path_observed);

    assert_eq!(
        summary.tier1_latency.p50_budget_ms,
        DEFAULT_TIER1_LOCAL_P50_BUDGET_MS
    );
    assert_eq!(
        summary.tier1_latency.p99_budget_ms,
        DEFAULT_TIER1_LOCAL_P99_BUDGET_MS
    );
    assert_eq!(summary.tier1_latency.p50_regression_ms, 4);
    assert_eq!(summary.tier1_latency.p99_regression_ms, 11);
    assert!(summary.tier1_latency.within_budget);

    assert_eq!(
        summary.freshness.probe_execution,
        "async_after_local_answer"
    );
    assert_eq!(
        summary.freshness.peer_timeout_budget_ms,
        DEFAULT_PEER_PROBE_TIMEOUT_BUDGET_MS
    );
    assert_eq!(
        summary.freshness.stale_read_window_budget_ms,
        DEFAULT_STALE_READ_WINDOW_BUDGET_MS
    );
    assert_eq!(
        summary.freshness.peer_count_budget,
        DEFAULT_PEER_PROBE_FANOUT_BUDGET
    );
    assert!(summary.freshness.within_budget);

    assert_eq!(
        summary.resources.body_cache_budget_bytes,
        DEFAULT_BODY_CACHE_BUDGET_BYTES
    );
    assert_eq!(
        summary.resources.sync_batch_budget_events,
        DEFAULT_SYNC_BATCH_BUDGET_EVENTS
    );
    assert_eq!(
        summary.resources.index_job_budget,
        DEFAULT_INDEX_JOB_AMPLIFICATION_BUDGET
    );
    assert!(!summary.resources.body_transfer_allowed_on_tier1);
    assert!(summary.resources.within_budget);

    let evidence = serde_json::to_string(&summary).map_err(|error| error.to_string())?;
    println!("{evidence}");

    Ok(())
}

#[test]
fn budget_report_flags_timeout_backpressure_and_resource_pressure() -> TestResult {
    let summary = build_two_tier_budget_summary(MeshTwoTierBudgetInput {
        mesh_enabled: true,
        baseline_tier1_p50_ms: 38,
        observed_tier1_p50_ms: DEFAULT_TIER1_LOCAL_P50_BUDGET_MS + 1,
        baseline_tier1_p99_ms: 117,
        observed_tier1_p99_ms: DEFAULT_TIER1_LOCAL_P99_BUDGET_MS + 1,
        peer_timeout_ms: DEFAULT_PEER_PROBE_TIMEOUT_BUDGET_MS + 1,
        max_peer_probe_elapsed_ms: DEFAULT_PEER_PROBE_TIMEOUT_BUDGET_MS + 2,
        stale_read_window_ms: DEFAULT_STALE_READ_WINDOW_BUDGET_MS + 1,
        peer_count: usize::try_from(DEFAULT_PEER_PROBE_FANOUT_BUDGET + 1)
            .map_err(|error| error.to_string())?,
        body_cache_bytes: DEFAULT_BODY_CACHE_BUDGET_BYTES + 1,
        sync_batch_events: DEFAULT_SYNC_BATCH_BUDGET_EVENTS + 1,
        index_jobs_enqueued: DEFAULT_INDEX_JOB_AMPLIFICATION_BUDGET + 1,
        cache_hit_path_observed: false,
        checked_at: None,
    });

    assert_eq!(summary.status, "degraded");
    assert_eq!(
        summary.degraded,
        vec![
            degraded_codes::SUPERVISOR_BUDGET_EXCEEDED.to_owned(),
            degraded_codes::FRESHNESS_PEER_TIMEOUT.to_owned(),
        ]
    );
    assert!(!summary.tier1_latency.within_budget);
    assert!(!summary.freshness.within_budget);
    assert!(!summary.resources.within_budget);
    assert!(!summary.cache_hit_path_observed);
    assert!(!summary.local_answer_blocking);
    assert!(!summary.network_on_tier1);
    assert!(!summary.resources.body_transfer_allowed_on_tier1);

    Ok(())
}

#[test]
fn mesh_disabled_budget_report_is_noop_for_remote_pressure() -> TestResult {
    let summary = build_two_tier_budget_summary(MeshTwoTierBudgetInput {
        mesh_enabled: false,
        baseline_tier1_p50_ms: 0,
        observed_tier1_p50_ms: u64::MAX,
        baseline_tier1_p99_ms: 0,
        observed_tier1_p99_ms: u64::MAX,
        peer_timeout_ms: u64::MAX,
        max_peer_probe_elapsed_ms: u64::MAX,
        stale_read_window_ms: u64::MAX,
        peer_count: usize::MAX,
        body_cache_bytes: u64::MAX,
        sync_batch_events: u64::MAX,
        index_jobs_enqueued: u64::MAX,
        cache_hit_path_observed: false,
        checked_at: None,
    });

    assert_eq!(summary.status, "disabled");
    assert_eq!(summary.degraded, Vec::<String>::new());
    assert!(!summary.mesh_enabled);
    assert!(!summary.local_answer_blocking);
    assert!(!summary.network_on_tier1);
    assert_eq!(summary.freshness.probe_execution, "mesh_disabled_noop");
    assert!(summary.tier1_latency.within_budget);
    assert!(summary.freshness.within_budget);
    assert!(summary.resources.within_budget);

    Ok(())
}
