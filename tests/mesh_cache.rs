//! Executable SRR6.26 mesh cache retention checks.
//!
//! Imported by path while adjacent mesh module surfaces are actively owned by
//! other swarm lanes.

#[path = "../src/mesh/cache.rs"]
#[allow(dead_code)]
mod cache;

use cache::{
    MeshCacheBodyFetchDecision, MeshCacheEntry, MeshCacheEvictionReason, MeshCacheLane,
    MeshCacheQuotaKind, MeshCacheQuotaWarningSeverity, MeshCacheQuotas, MeshCacheRetentionInput,
    MeshCacheStatus, blake3_content_hash, decide_body_fetch_lifecycle, eager_replication_warnings,
    plan_mesh_cache_retention,
};

type TestResult = Result<(), String>;

#[test]
fn quota_pressure_evicts_low_score_lru_body_and_logs_audit_fields() -> TestResult {
    let input = MeshCacheRetentionInput {
        entries: vec![
            MeshCacheEntry::derived("body-old", "peer_alpha", MeshCacheLane::Body, 220)
                .with_retention_score(10)
                .with_last_access_seq(1),
            MeshCacheEntry::derived("body-new", "peer_alpha", MeshCacheLane::Body, 220)
                .with_retention_score(50)
                .with_last_access_seq(5),
            MeshCacheEntry::derived("meta-hot", "peer_alpha", MeshCacheLane::Metadata, 40)
                .with_retention_score(10)
                .with_last_access_seq(1),
        ],
        quotas: MeshCacheQuotas {
            global_bytes: Some(260),
            ..MeshCacheQuotas::unlimited()
        },
        now_epoch_ms: 10_000,
    };

    let plan = plan_mesh_cache_retention(&input);

    assert_eq!(plan.cache_bytes_before(), 480);
    assert_eq!(plan.cache_bytes_after(), 260);
    assert_eq!(plan.evicted_count(), 1);
    let eviction = &plan.evictions[0];
    assert_eq!(eviction.cache_key, "body-old");
    assert_eq!(eviction.peer_id, "peer_alpha");
    assert_eq!(
        eviction.reason,
        MeshCacheEvictionReason::GlobalQuotaExceeded
    );
    assert_eq!(eviction.cache_bytes_before, 480);
    assert_eq!(eviction.cache_bytes_after, 260);
    assert_eq!(eviction.evicted_count, 1);

    println!(
        "mesh_cache_retention cache_bytes_before={} cache_bytes_after={} evicted_count={} peer_id={} reason={}",
        eviction.cache_bytes_before,
        eviction.cache_bytes_after,
        eviction.evicted_count,
        eviction.peer_id,
        eviction.reason.as_str()
    );

    Ok(())
}

#[test]
fn per_peer_quota_only_evictions_that_peers_derived_cache() -> TestResult {
    let input = MeshCacheRetentionInput {
        entries: vec![
            MeshCacheEntry::derived("alpha-cold", "peer_alpha", MeshCacheLane::Embedding, 170)
                .with_retention_score(1)
                .with_last_access_seq(1),
            MeshCacheEntry::derived("alpha-hot", "peer_alpha", MeshCacheLane::Embedding, 170)
                .with_retention_score(900)
                .with_last_access_seq(9),
            MeshCacheEntry::derived("beta-cold", "peer_beta", MeshCacheLane::Embedding, 170)
                .with_retention_score(1)
                .with_last_access_seq(1),
        ],
        quotas: MeshCacheQuotas {
            per_peer_bytes: Some(200),
            ..MeshCacheQuotas::unlimited()
        },
        now_epoch_ms: 10_000,
    };

    let plan = plan_mesh_cache_retention(&input);

    assert_eq!(plan.evicted_count(), 1);
    assert_eq!(plan.evictions[0].cache_key, "alpha-cold");
    assert_eq!(
        plan.evictions[0].reason,
        MeshCacheEvictionReason::PeerQuotaExceeded
    );
    assert_eq!(
        plan.usage_after.by_peer_bytes.get("peer_alpha").copied(),
        Some(170)
    );
    assert_eq!(
        plan.usage_after.by_peer_bytes.get("peer_beta").copied(),
        Some(170)
    );

    Ok(())
}

#[test]
fn local_source_truth_is_never_counted_or_evicted() -> TestResult {
    let input = MeshCacheRetentionInput {
        entries: vec![
            MeshCacheEntry::local_source_truth("local-memory", MeshCacheLane::Body, 100_000),
            MeshCacheEntry::derived("peer-body", "peer_alpha", MeshCacheLane::Body, 100),
        ],
        quotas: MeshCacheQuotas {
            global_bytes: Some(50),
            ..MeshCacheQuotas::unlimited()
        },
        now_epoch_ms: 10_000,
    };

    let plan = plan_mesh_cache_retention(&input);

    assert_eq!(plan.protected_local_source_truth_count, 1);
    assert_eq!(plan.cache_bytes_before(), 100);
    assert_eq!(plan.cache_bytes_after(), 0);
    assert_eq!(plan.evictions.len(), 1);
    assert_eq!(plan.evictions[0].cache_key, "peer-body");

    Ok(())
}

#[test]
fn lane_quota_eviction_preserves_metadata_when_body_lane_is_over_budget() -> TestResult {
    let input = MeshCacheRetentionInput {
        entries: vec![
            MeshCacheEntry::derived("body-cold", "peer_alpha", MeshCacheLane::Body, 160)
                .with_retention_score(1),
            MeshCacheEntry::derived("body-warm", "peer_beta", MeshCacheLane::Body, 160)
                .with_retention_score(100),
            MeshCacheEntry::derived("meta", "peer_alpha", MeshCacheLane::Metadata, 60)
                .with_retention_score(0),
        ],
        quotas: MeshCacheQuotas {
            body_bytes: Some(200),
            ..MeshCacheQuotas::unlimited()
        },
        now_epoch_ms: 10_000,
    };

    let plan = plan_mesh_cache_retention(&input);

    assert_eq!(plan.evicted_count(), 1);
    assert_eq!(plan.evictions[0].cache_key, "body-cold");
    assert_eq!(
        plan.evictions[0].reason,
        MeshCacheEvictionReason::LaneQuotaExceeded
    );
    assert_eq!(plan.usage_after.metadata_bytes, 60);
    assert_eq!(plan.usage_after.body_bytes, 160);

    Ok(())
}

#[test]
fn expired_entries_are_evict_first_body_lifecycle_events() -> TestResult {
    let input = MeshCacheRetentionInput {
        entries: vec![
            MeshCacheEntry::derived("expired-high", "peer_alpha", MeshCacheLane::Body, 100)
                .with_retention_score(900)
                .with_expires_at_epoch_ms(9_000),
            MeshCacheEntry::derived("fresh-low", "peer_alpha", MeshCacheLane::Body, 100)
                .with_retention_score(1)
                .with_expires_at_epoch_ms(20_000),
        ],
        quotas: MeshCacheQuotas::unlimited(),
        now_epoch_ms: 10_000,
    };

    let plan = plan_mesh_cache_retention(&input);

    assert_eq!(plan.evicted_count(), 1);
    assert_eq!(plan.evictions[0].cache_key, "expired-high");
    assert_eq!(plan.evictions[0].reason, MeshCacheEvictionReason::Expired);
    assert_eq!(plan.cache_bytes_after(), 100);

    Ok(())
}

#[test]
fn fetched_body_hash_mismatch_quarantines_body_without_persisting() -> TestResult {
    let body = b"remote body v1";
    let expected = blake3_content_hash(body);
    let valid = decide_body_fetch_lifecycle(&expected, body);
    assert_fetch_decision(
        &valid,
        MeshCacheStatus::Available,
        true,
        None,
        "matching body",
    )?;

    let mismatch = decide_body_fetch_lifecycle(&expected, b"remote body v2");
    assert_fetch_decision(
        &mismatch,
        MeshCacheStatus::Quarantined,
        false,
        Some("content_hash_mismatch"),
        "mismatched body",
    )?;

    Ok(())
}

#[test]
fn eager_replication_warns_before_quota_pressure() -> TestResult {
    let existing = vec![
        MeshCacheEntry::derived("body-a", "peer_alpha", MeshCacheLane::Body, 700),
        MeshCacheEntry::derived("meta-a", "peer_alpha", MeshCacheLane::Metadata, 100),
    ];
    let candidate = MeshCacheEntry::derived("body-b", "peer_alpha", MeshCacheLane::Body, 150);
    let quotas = MeshCacheQuotas {
        global_bytes: Some(1_000),
        per_peer_bytes: Some(1_000),
        body_bytes: Some(900),
        ..MeshCacheQuotas::unlimited()
    };

    let warnings = eager_replication_warnings(&existing, &candidate, &quotas, 90);

    assert!(
        warnings.iter().any(|warning| {
            warning.kind == MeshCacheQuotaKind::Lane
                && warning.lane == Some(MeshCacheLane::Body)
                && warning.severity == MeshCacheQuotaWarningSeverity::NearLimit
        }),
        "expected body lane near-limit warning before eager replication, got {warnings:?}"
    );

    Ok(())
}

fn assert_fetch_decision(
    decision: &MeshCacheBodyFetchDecision,
    status: MeshCacheStatus,
    body_persist_allowed: bool,
    quarantine_reason: Option<&str>,
    label: &str,
) -> TestResult {
    if decision.status != status {
        return Err(format!(
            "{label}: expected status {}, got {}",
            status.as_str(),
            decision.status.as_str()
        ));
    }
    if decision.body_persist_allowed != body_persist_allowed {
        return Err(format!(
            "{label}: expected body_persist_allowed={body_persist_allowed}, got {}",
            decision.body_persist_allowed
        ));
    }
    if decision.quarantine_reason.as_deref() != quarantine_reason {
        return Err(format!(
            "{label}: expected quarantine_reason={quarantine_reason:?}, got {:?}",
            decision.quarantine_reason
        ));
    }
    Ok(())
}
