use ee::graph::scale_policy::{
    SCALE_LOCALITY_ADVISOR_SCHEMA_V1, SCALE_LOCALITY_REDACTION_STATUS, ScaleLocalityAdvisorInput,
    ScaleLocalityAffinity, ScaleLocalityContention, ScaleLocalityPlanAction,
    ScaleLocalityQueueState, ScaleLocalityReadPoolState, ScaleLocalityShardStrategy,
    ScaleLocalitySurface, ScaleLocalityTopologyKind, advise_scale_locality,
};
use serde::Serialize;
use serde_json::Value;

type TestResult = Result<(), String>;

fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn ensure_equal<T: std::fmt::Debug + PartialEq>(
    actual: T,
    expected: T,
    context: &str,
) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{context}: expected {expected:?}, got {actual:?}"))
    }
}

fn to_value<T: Serialize>(value: &T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

fn has_degraded_code(json: &Value, code: &str) -> bool {
    json.pointer("/degraded")
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry.pointer("/code").and_then(Value::as_str) == Some(code))
        })
}

#[test]
fn high_core_numa_fixture_recommends_locality_aware_shard_groups() -> TestResult {
    let report = advise_scale_locality(&ScaleLocalityAdvisorInput::high_core_numa_fixture());

    ensure_equal(report.schema, SCALE_LOCALITY_ADVISOR_SCHEMA_V1, "schema id")?;
    ensure_equal(
        report.redaction_status,
        SCALE_LOCALITY_REDACTION_STATUS,
        "redaction status",
    )?;
    ensure_equal(
        report.topology.kind,
        ScaleLocalityTopologyKind::MultiSocketNuma,
        "topology kind",
    )?;
    ensure(
        report.topology.measured,
        "high-core Linux fixture should use measured NUMA topology",
    )?;
    ensure_equal(
        report.shard_placement.strategy,
        ScaleLocalityShardStrategy::NumaShardGroups,
        "shard strategy",
    )?;
    ensure_equal(
        report.shard_placement.recommended_shard_groups,
        2,
        "recommended shard groups",
    )?;
    ensure_equal(
        report.shard_placement.shards_per_group,
        4,
        "shards per group",
    )?;
    ensure_equal(
        report.read_pool.state,
        ScaleLocalityReadPoolState::Undersized,
        "read pool state",
    )?;
    ensure_equal(
        report.read_pool.recommended_size,
        16,
        "recommended read pool size",
    )?;
    ensure_equal(
        report.queue_pressure.state,
        ScaleLocalityQueueState::Moderate,
        "queue pressure state",
    )?;
    ensure_equal(
        report.cache_affinity.affinity,
        ScaleLocalityAffinity::Strong,
        "cache affinity",
    )?;
    ensure_equal(
        report.cross_pool_contention.state,
        ScaleLocalityContention::Moderate,
        "cross-pool contention",
    )?;
    ensure(
        report.execution_plan.iter().any(|step| {
            step.surface == ScaleLocalitySurface::Search
                && step.action == ScaleLocalityPlanAction::IncreaseReadPool
        }),
        "search plan should recommend increasing the read pool first",
    )?;
    ensure(
        report.execution_plan.iter().any(|step| {
            step.surface == ScaleLocalitySurface::GraphProjection
                && step.action == ScaleLocalityPlanAction::UseNumaShardGroups
        }),
        "graph plan should stay NUMA-local",
    )?;

    let json = to_value(&report)?;
    ensure(
        has_degraded_code(&json, "read_pool_undersized"),
        "undersized high-core fixture should emit read_pool_undersized",
    )?;
    ensure(
        !report.to_json().contains("raw")
            && !report.to_json().contains("memory body")
            && !report.to_json().contains("query text"),
        "advisor JSON must stay redaction-safe",
    )
}

#[test]
fn single_socket_fixture_co_locates_without_fake_numa() -> TestResult {
    let report = advise_scale_locality(&ScaleLocalityAdvisorInput::single_socket_fixture());

    ensure_equal(
        report.topology.kind,
        ScaleLocalityTopologyKind::SingleSocket,
        "single-socket topology kind",
    )?;
    ensure_equal(
        report.shard_placement.strategy,
        ScaleLocalityShardStrategy::SingleSocketCoLocated,
        "single-socket shard strategy",
    )?;
    ensure_equal(
        report.read_pool.state,
        ScaleLocalityReadPoolState::Adequate,
        "single-socket read pool",
    )?;
    ensure_equal(
        report.cache_affinity.affinity,
        ScaleLocalityAffinity::Acceptable,
        "single-socket cache affinity",
    )?;
    ensure(
        report.degraded.is_empty(),
        format!(
            "single-socket fixture should not degrade: {:?}",
            report.degraded
        ),
    )?;
    ensure(
        report.execution_plan.iter().all(|step| {
            step.action != ScaleLocalityPlanAction::UseNumaShardGroups
                && step.action != ScaleLocalityPlanAction::UsePlatformFallback
        }),
        "single-socket plan should neither invent NUMA groups nor platform fallback",
    )
}

#[test]
fn macos_fixture_reports_platform_fallback_instead_of_measured_numa() -> TestResult {
    let report =
        advise_scale_locality(&ScaleLocalityAdvisorInput::macos_platform_fallback_fixture());
    let json = to_value(&report)?;

    ensure_equal(
        report.topology.kind,
        ScaleLocalityTopologyKind::PlatformFallback,
        "macOS topology kind",
    )?;
    ensure(
        !report.topology.measured,
        "macOS fallback must not claim measured NUMA locality",
    )?;
    ensure_equal(
        report.shard_placement.strategy,
        ScaleLocalityShardStrategy::PlatformFallbackRoundRobin,
        "macOS shard strategy",
    )?;
    ensure(
        report
            .execution_plan
            .iter()
            .any(|step| step.action == ScaleLocalityPlanAction::UsePlatformFallback),
        "macOS execution plan should explicitly name platform fallback",
    )?;
    ensure(
        has_degraded_code(&json, "scale_locality_platform_fallback"),
        "macOS fallback should emit the platform fallback degraded code",
    )
}

#[test]
fn unknown_topology_fixture_stays_conservative_and_requests_hotset_signals() -> TestResult {
    let report = advise_scale_locality(&ScaleLocalityAdvisorInput::unknown_topology_fixture());
    let json = to_value(&report)?;

    ensure_equal(
        report.topology.kind,
        ScaleLocalityTopologyKind::Unknown,
        "unknown topology kind",
    )?;
    ensure_equal(
        report.queue_pressure.state,
        ScaleLocalityQueueState::Unknown,
        "unknown queue pressure",
    )?;
    ensure_equal(
        report.shard_placement.strategy,
        ScaleLocalityShardStrategy::ConservativeSingleShard,
        "unknown shard strategy",
    )?;
    ensure_equal(
        report.cache_affinity.affinity,
        ScaleLocalityAffinity::Weak,
        "unknown cache affinity",
    )?;
    ensure(
        has_degraded_code(&json, "scale_locality_topology_unknown"),
        "unknown topology should emit topology degraded code",
    )?;
    ensure(
        has_degraded_code(&json, "hotset_prewarm_no_signals"),
        "unknown topology should ask for hotset signals",
    )?;
    ensure(
        report
            .provenance
            .iter()
            .any(|entry| entry.kind == "resource_admission")
            && report.provenance.iter().any(|entry| entry.kind == "hotset"),
        "advisor should keep resource-admission and hotset provenance",
    )
}
