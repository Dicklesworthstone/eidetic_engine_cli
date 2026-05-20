//! Executable SRR6.14 checks for async peer freshness probes.

#[path = "../src/mesh/anti_entropy_protocol.rs"]
mod anti_entropy_protocol;

use anti_entropy_protocol::{
    MESH_FRESHNESS_PROBE_SUMMARY_SCHEMA_V1, MeshFreshnessProbeInput, MeshFreshnessQuerySummary,
    MeshOriginKey, MeshPeerCursor, MeshPeerFreshnessProbe, MeshPeerTip,
    build_freshness_probe_summary, degraded_codes,
};

type TestResult = Result<(), String>;

#[test]
fn fresher_peer_emits_revision_evidence_without_body_transfer() -> TestResult {
    let peer_id = "node_key=raw-peer-identity@example.tailnet";
    let origin = MeshOriginKey::with_workspace("origin-alpha-raw", "workspace-secret");
    let query = MeshFreshnessQuerySummary::new("secret release query text");

    assert_eq!(query.query_fingerprint, query.summary_hash);
    assert!(query.summary_hash.starts_with("query_"));
    assert!(
        !query.summary_hash.contains("secret"),
        "query summary must not expose raw query text"
    );

    let summary = build_freshness_probe_summary(MeshFreshnessProbeInput {
        mesh_enabled: true,
        local_query_summary: Some(query.clone()),
        local_cursors: vec![MeshPeerCursor::new(peer_id, origin.clone(), 3)],
        peer_probes: vec![
            MeshPeerFreshnessProbe::allowed(
                peer_id,
                vec![
                    MeshPeerTip::new(origin.clone(), 7),
                    MeshPeerTip::new(origin.clone(), 6),
                ],
            )
            .with_query_summary(query),
        ],
        peer_timeout_ms: 150,
        checked_at: Some("2026-05-20T04:50:00.000Z".to_owned()),
    });

    assert_eq!(summary.schema, MESH_FRESHNESS_PROBE_SUMMARY_SCHEMA_V1);
    assert_eq!(summary.status, "revision_available");
    assert_eq!(summary.probe_execution, "async_after_local_answer");
    assert!(!summary.local_answer_blocking);
    assert!(!summary.body_transfer_allowed);
    assert_eq!(summary.peer_timeout_ms, 150);
    assert_eq!(summary.peer_count, 1);
    assert_eq!(summary.peer_probes_scheduled, 1);
    assert_eq!(summary.revision_availability.len(), 1);

    let signal = &summary.revision_availability[0];
    assert!(signal.peer_alias.starts_with("peer_"));
    assert!(signal.origin_alias.starts_with("origin_"));
    assert_eq!(signal.local_last_durable_seq, 3);
    assert_eq!(signal.peer_last_contiguous_seq, 7);
    assert_eq!(signal.missing_event_count, 4);
    assert_eq!(signal.relevance_basis, "query_summary_match");
    assert!(signal.evidence_id.starts_with("freshness_"));

    let peer = &summary.per_peer[0];
    assert_eq!(peer.status, "fresher");
    assert_eq!(peer.advertised_origin_count, 1);
    assert_eq!(peer.max_missing_event_count, 4);
    assert_eq!(peer.query_summary_matched, Some(true));
    assert!(!peer.body_transfer_allowed);

    let serialized = serde_json::to_string(&summary).map_err(|error| error.to_string())?;
    for forbidden in [
        "node_key=",
        "@example",
        "origin-alpha-raw",
        "workspace-secret",
        "secret release query text",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "freshness summary leaked raw fragment {forbidden}: {serialized}"
        );
    }

    Ok(())
}

#[test]
fn stale_peer_is_current_and_emits_no_revision_signal() -> TestResult {
    let peer_id = "peer-stale";
    let origin = MeshOriginKey::new("origin-stale");

    let summary = build_freshness_probe_summary(MeshFreshnessProbeInput {
        mesh_enabled: true,
        local_query_summary: None,
        local_cursors: vec![MeshPeerCursor::new(peer_id, origin.clone(), 7)],
        peer_probes: vec![MeshPeerFreshnessProbe::allowed(
            peer_id,
            vec![
                MeshPeerTip::new(origin.clone(), 5),
                MeshPeerTip::new(origin, 7),
            ],
        )],
        peer_timeout_ms: 150,
        checked_at: None,
    });

    assert_eq!(summary.status, "current");
    assert_eq!(summary.degraded, Vec::<String>::new());
    assert_eq!(summary.revision_availability, Vec::new());
    assert_eq!(summary.per_peer.len(), 1);
    assert_eq!(summary.per_peer[0].status, "stale_or_current");
    assert_eq!(summary.per_peer[0].max_missing_event_count, 0);
    assert_eq!(summary.per_peer[0].query_summary_matched, None);
    assert!(!summary.local_answer_blocking);
    assert!(!summary.body_transfer_allowed);

    Ok(())
}

#[test]
fn denied_peer_degrades_without_transfer_or_revision_signal() -> TestResult {
    let summary = build_freshness_probe_summary(MeshFreshnessProbeInput {
        mesh_enabled: true,
        local_query_summary: Some(MeshFreshnessQuerySummary::new("denied query")),
        local_cursors: Vec::new(),
        peer_probes: vec![MeshPeerFreshnessProbe::denied("peer-denied")],
        peer_timeout_ms: 150,
        checked_at: None,
    });

    assert_eq!(summary.status, "degraded");
    assert_eq!(
        summary.degraded,
        vec![degraded_codes::FRESHNESS_PEER_POLICY_REFUSED.to_owned()]
    );
    assert_eq!(summary.revision_availability, Vec::new());
    assert_eq!(summary.per_peer.len(), 1);
    assert_eq!(summary.per_peer[0].status, "denied");
    assert!(!summary.per_peer[0].body_transfer_allowed);
    assert!(!summary.local_answer_blocking);
    assert!(!summary.body_transfer_allowed);

    Ok(())
}

#[test]
fn timed_out_peer_respects_budget_and_does_not_block() -> TestResult {
    let summary = build_freshness_probe_summary(MeshFreshnessProbeInput {
        mesh_enabled: true,
        local_query_summary: None,
        local_cursors: Vec::new(),
        peer_probes: vec![MeshPeerFreshnessProbe::timeout("peer-timeout")],
        peer_timeout_ms: 25,
        checked_at: Some("2026-05-20T04:51:00.000Z".to_owned()),
    });

    assert_eq!(summary.status, "degraded");
    assert_eq!(summary.peer_timeout_ms, 25);
    assert_eq!(
        summary.degraded,
        vec![degraded_codes::FRESHNESS_PEER_TIMEOUT.to_owned()]
    );
    assert_eq!(summary.per_peer.len(), 1);
    assert_eq!(summary.per_peer[0].status, "timeout");
    assert_eq!(summary.revision_availability, Vec::new());
    assert!(!summary.local_answer_blocking);
    assert!(!summary.body_transfer_allowed);

    Ok(())
}

#[test]
fn mesh_disabled_is_noop_even_when_peer_tips_are_fresher() -> TestResult {
    let peer_id = "peer-disabled";
    let origin = MeshOriginKey::new("origin-disabled");
    let query = MeshFreshnessQuerySummary::new("disabled query");

    let summary = build_freshness_probe_summary(MeshFreshnessProbeInput {
        mesh_enabled: false,
        local_query_summary: Some(query.clone()),
        local_cursors: vec![MeshPeerCursor::new(peer_id, origin.clone(), 0)],
        peer_probes: vec![
            MeshPeerFreshnessProbe::allowed(peer_id, vec![MeshPeerTip::new(origin, 99)])
                .with_query_summary(query),
        ],
        peer_timeout_ms: 150,
        checked_at: None,
    });

    assert_eq!(summary.status, "disabled");
    assert_eq!(summary.probe_execution, "mesh_disabled_noop");
    assert_eq!(summary.peer_count, 0);
    assert_eq!(summary.peer_probes_scheduled, 0);
    assert_eq!(summary.revision_availability, Vec::new());
    assert_eq!(summary.per_peer, Vec::new());
    assert_eq!(summary.degraded, Vec::<String>::new());
    assert!(!summary.local_answer_blocking);
    assert!(!summary.body_transfer_allowed);

    Ok(())
}

#[test]
fn mismatched_query_summary_suppresses_freshness_signal() -> TestResult {
    let peer_id = "peer-query-miss";
    let origin = MeshOriginKey::new("origin-query-miss");

    let summary = build_freshness_probe_summary(MeshFreshnessProbeInput {
        mesh_enabled: true,
        local_query_summary: Some(MeshFreshnessQuerySummary::new("local query")),
        local_cursors: vec![MeshPeerCursor::new(peer_id, origin.clone(), 1)],
        peer_probes: vec![
            MeshPeerFreshnessProbe::allowed(peer_id, vec![MeshPeerTip::new(origin, 9)])
                .with_query_summary(MeshFreshnessQuerySummary::new("different peer query")),
        ],
        peer_timeout_ms: 150,
        checked_at: None,
    });

    assert_eq!(summary.status, "current");
    assert_eq!(summary.revision_availability, Vec::new());
    assert_eq!(summary.degraded, Vec::<String>::new());
    assert_eq!(summary.per_peer.len(), 1);
    assert_eq!(summary.per_peer[0].status, "query_summary_miss");
    assert_eq!(summary.per_peer[0].query_summary_matched, Some(false));
    assert!(!summary.local_answer_blocking);
    assert!(!summary.body_transfer_allowed);

    Ok(())
}
