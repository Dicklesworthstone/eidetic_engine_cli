//! Executable SRR6.7 checks for the mesh anti-entropy protocol primitives.
//!
//! Imported by path while `src/mesh/mod.rs` is owned by adjacent mesh CLI work.

#[path = "../src/mesh/anti_entropy_protocol.rs"]
#[allow(dead_code)]
mod anti_entropy_protocol;

use std::collections::BTreeMap;

use anti_entropy_protocol::{
    ANTI_ENTROPY_PROTOCOL_DOC, ANTI_ENTROPY_PROTOCOL_SCENARIOS, MeshAntiEntropyRetryPolicy,
    MeshBlockedRange, MeshBlockedRangeReason, MeshOriginKey, MeshPeerCursor, MeshPeerTip,
    MeshRangeKey, MeshRangePlanner, MeshRangeRetryState, MeshReplayEvent, MeshRoundPeerOutcome,
    MeshSyncSummaryInput, build_sync_summary, cursor_after_durable_replay, degraded_codes,
    summarize_event_range,
};

type TestResult = Result<(), String>;

const EXPECTED_SCENARIOS: &[&str] = &[
    "tip_advertise_builds_bounded_range_requests",
    "cursor_advances_only_after_durable_contiguous_replay",
    "range_digest_is_order_independent",
    "bounded_retry_blocks_after_max_attempts",
    "sync_summary_is_redaction_safe",
    "two_peer_partition_rejoin_converges",
];

#[test]
fn protocol_scenario_catalog_is_stable_and_logged() -> TestResult {
    assert_eq!(ANTI_ENTROPY_PROTOCOL_SCENARIOS, EXPECTED_SCENARIOS);

    for scenario in ANTI_ENTROPY_PROTOCOL_SCENARIOS {
        println!("mesh_anti_entropy_protocol_scenario={scenario} result=covered");
    }

    let protocol_doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(ANTI_ENTROPY_PROTOCOL_DOC),
    )
    .map_err(|error| format!("read {ANTI_ENTROPY_PROTOCOL_DOC}: {error}"))?;
    for required in [
        "TipAdvertise",
        "RangeRequest",
        "EventBatch",
        "Bounded retry/backoff",
        "Sync summary surface",
    ] {
        if !protocol_doc.contains(required) {
            return Err(format!(
                "{ANTI_ENTROPY_PROTOCOL_DOC} must document {required}"
            ));
        }
    }

    Ok(())
}

#[test]
fn tip_advertise_builds_bounded_range_requests() -> TestResult {
    let peer_id = "node_key=raw-peer-identity@example.tailnet";
    let origin_a = MeshOriginKey::with_workspace("origin-a", "workspace-a");
    let origin_b = MeshOriginKey::new("origin-b");
    let planner = MeshRangePlanner {
        max_events_per_range: 2,
        ..MeshRangePlanner::default()
    };

    let plan = planner.plan(
        peer_id,
        &[MeshPeerCursor::new(peer_id, origin_a.clone(), 1)],
        &[
            MeshPeerTip::new(origin_b.clone(), 1),
            MeshPeerTip::new(origin_a.clone(), 5),
            MeshPeerTip::new(origin_a.clone(), 4),
        ],
        &[],
        10_000,
        "2026-05-19T22:25:30.000Z",
    );

    assert_eq!(plan.blocked_ranges, Vec::new());
    assert_eq!(plan.waiting_ranges, Vec::new());
    assert_eq!(plan.requests.len(), 2);
    assert_eq!(plan.requests[0].origin, origin_a);
    assert_eq!(plan.requests[0].start_seq, 2);
    assert_eq!(plan.requests[0].end_seq, 3);
    assert_eq!(plan.requests[0].attempt, 1);
    assert_eq!(plan.requests[1].origin, origin_b);
    assert_eq!(plan.requests[1].start_seq, 1);
    assert_eq!(plan.requests[1].end_seq, 1);

    Ok(())
}

#[test]
fn cursor_advances_only_after_durable_contiguous_replay() -> TestResult {
    assert_eq!(
        cursor_after_durable_replay(5, [8, 6, 6]),
        6,
        "cursor must not skip missing durable seq=7"
    );
    assert_eq!(
        cursor_after_durable_replay(6, [8, 7]),
        8,
        "cursor advances once the gap is durably replayed"
    );
    assert_eq!(
        cursor_after_durable_replay(8, [7, 9]),
        9,
        "events below the cursor are ignored; next contiguous seq advances"
    );

    Ok(())
}

#[test]
fn range_digest_is_order_independent_and_rejects_holes() -> TestResult {
    let origin = MeshOriginKey::new("origin-a");
    let canonical = summarize_event_range([
        MeshReplayEvent::new(origin.clone(), 1, "hash-1"),
        MeshReplayEvent::new(origin.clone(), 2, "hash-2"),
        MeshReplayEvent::new(origin.clone(), 3, "hash-3"),
    ])
    .map_err(|error| format!("canonical digest: {error:?}"))?;
    let shuffled = summarize_event_range([
        MeshReplayEvent::new(origin.clone(), 3, "hash-3"),
        MeshReplayEvent::new(origin.clone(), 1, "hash-1"),
        MeshReplayEvent::new(origin.clone(), 2, "hash-2"),
    ])
    .map_err(|error| format!("shuffled digest: {error:?}"))?;

    assert_eq!(canonical, shuffled);
    assert_eq!(canonical.start_seq, 1);
    assert_eq!(canonical.end_seq, 3);
    assert_eq!(canonical.event_count, 3);

    let hole = summarize_event_range([
        MeshReplayEvent::new(origin.clone(), 1, "hash-1"),
        MeshReplayEvent::new(origin, 3, "hash-3"),
    ]);
    assert!(hole.is_err(), "range batches with holes must be rejected");

    Ok(())
}

#[test]
fn range_digest_and_origin_alias_distinguish_field_boundaries() -> TestResult {
    let split_origin = MeshOriginKey::with_workspace("origin-a", "workspace-a");
    let joined_origin = MeshOriginKey::new("origin-a|workspace-a");

    assert_ne!(
        split_origin.redacted_alias(),
        joined_origin.redacted_alias(),
        "origin aliases must distinguish node/workspace boundaries"
    );

    let split_digest = summarize_event_range([MeshReplayEvent::new(split_origin, 1, "event-hash")])
        .map_err(|error| format!("split origin digest: {error:?}"))?;
    let joined_digest =
        summarize_event_range([MeshReplayEvent::new(joined_origin, 1, "event-hash")])
            .map_err(|error| format!("joined origin digest: {error:?}"))?;
    assert_ne!(
        split_digest.range_digest, joined_digest.range_digest,
        "range digests must not collapse workspace and origin delimiter boundaries"
    );

    let origin = MeshOriginKey::new("origin-a");
    let no_audit_digest =
        summarize_event_range([MeshReplayEvent::new(origin.clone(), 1, "event:audit")])
            .map_err(|error| format!("no audit digest: {error:?}"))?;
    let mut split_hash_event = MeshReplayEvent::new(origin, 1, "event");
    split_hash_event.audit_hash = Some("audit".to_owned());
    let with_audit_digest = summarize_event_range([split_hash_event])
        .map_err(|error| format!("with audit digest: {error:?}"))?;
    assert_ne!(
        no_audit_digest.range_digest, with_audit_digest.range_digest,
        "range digests must distinguish event-hash text from audit-hash boundaries"
    );

    Ok(())
}

#[test]
fn bounded_retry_blocks_after_max_attempts() -> TestResult {
    let peer_id = "peer-retry";
    let origin = MeshOriginKey::new("origin-retry");
    let policy = MeshAntiEntropyRetryPolicy::default();
    let planner = MeshRangePlanner {
        policy,
        max_events_per_range: 18,
    };
    let key = MeshRangeKey::new(peer_id, origin.clone(), 1, 18);
    let plan = planner.plan(
        peer_id,
        &[],
        &[MeshPeerTip::new(origin, 58)],
        &[MeshRangeRetryState {
            key,
            attempts: policy.max_attempts,
            next_retry_after_epoch_ms: Some(20_000),
        }],
        10_000,
        "2026-05-19T22:25:30.000Z",
    );

    assert_eq!(plan.requests, Vec::new());
    assert_eq!(plan.waiting_ranges, Vec::new());
    assert_eq!(plan.blocked_ranges.len(), 1);
    assert_eq!(
        plan.blocked_ranges[0].reason,
        MeshBlockedRangeReason::MaxAttemptsExceeded
    );
    assert_eq!(policy.next_delay_ms(1), 1_000);
    assert_eq!(policy.next_delay_ms(4), 8_000);

    Ok(())
}

#[test]
fn sync_summary_is_redaction_safe_and_status_ready() -> TestResult {
    let peer_id = "node_key=raw-peer-identity@example.tailnet";
    let origin = MeshOriginKey::new("100.64.1.7/raw-origin-node");
    let mut outcome = MeshRoundPeerOutcome::new(peer_id);
    outcome.events_accepted = 14;
    outcome.events_duplicate = 2;
    outcome.events_forked = 1;
    outcome.ranges_requested = 3;
    outcome.ranges_fulfilled = 2;

    let summary = build_sync_summary(MeshSyncSummaryInput {
        last_round_completed_at: Some("2026-05-19T22:10:30.000Z".to_owned()),
        origins_tracked: 3,
        peer_outcomes: vec![outcome],
        retry_policy: MeshAntiEntropyRetryPolicy::default(),
        current_attempts: 5,
        next_retry_after: Some("2026-05-19T22:25:30.000Z".to_owned()),
        blocked_ranges: vec![MeshBlockedRange {
            key: MeshRangeKey::new(peer_id, origin, 41, 58),
            retry_after: "2026-05-19T22:25:30.000Z".to_owned(),
            reason: MeshBlockedRangeReason::MaxAttemptsExceeded,
        }],
        degraded: vec![degraded_codes::FORK_OBSERVED.to_owned()],
    });

    let value = serde_json::to_value(&summary).map_err(|error| error.to_string())?;
    assert_eq!(
        value.pointer("/schema").and_then(serde_json::Value::as_str),
        Some("ee.mesh.anti_entropy.v1")
    );
    assert_eq!(
        value
            .pointer("/perPeerCounts/0/eventsAccepted")
            .and_then(serde_json::Value::as_u64),
        Some(14)
    );
    assert_eq!(
        value
            .pointer("/backoffPosture/maxAttempts")
            .and_then(serde_json::Value::as_u64),
        Some(5)
    );
    assert!(
        value
            .pointer("/perPeerCounts/0/peerAlias")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|alias| alias.starts_with("peer_"))
    );
    assert!(
        value
            .pointer("/blockedRanges/0/originAlias")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|alias| alias.starts_with("origin_"))
    );
    assert!(
        summary
            .degraded
            .contains(&degraded_codes::ROUND_BLOCKED.to_owned())
    );
    assert!(
        summary
            .degraded
            .contains(&degraded_codes::FORK_OBSERVED.to_owned())
    );

    let serialized = serde_json::to_string(&summary).map_err(|error| error.to_string())?;
    for forbidden in ["node_key=", "@example", "100.64.", "raw-origin-node"] {
        assert!(
            !serialized.contains(forbidden),
            "summary leaked raw identity fragment {forbidden}: {serialized}"
        );
    }

    Ok(())
}

#[test]
fn sync_summary_counts_blocked_only_peers_in_budget_window() -> TestResult {
    let peer_id = "node_key=blocked-peer@example.tailnet";
    let origin = MeshOriginKey::new("blocked-origin");

    let summary = build_sync_summary(MeshSyncSummaryInput {
        last_round_completed_at: Some("2026-05-19T22:10:30.000Z".to_owned()),
        origins_tracked: 1,
        peer_outcomes: Vec::new(),
        retry_policy: MeshAntiEntropyRetryPolicy::default(),
        current_attempts: 5,
        next_retry_after: Some("2026-05-19T22:25:30.000Z".to_owned()),
        blocked_ranges: vec![MeshBlockedRange {
            key: MeshRangeKey::new(peer_id, origin, 41, 58),
            retry_after: "2026-05-19T22:25:30.000Z".to_owned(),
            reason: MeshBlockedRangeReason::MaxAttemptsExceeded,
        }],
        degraded: Vec::new(),
    });

    assert_eq!(
        summary.peer_count, 1,
        "blocked-range peers are part of the current budget window even without a fulfilled outcome"
    );
    assert_eq!(summary.per_peer_counts, Vec::new());
    assert_eq!(summary.blocked_ranges.len(), 1);
    assert!(summary.blocked_ranges[0].peer_alias.starts_with("peer_"));
    assert!(
        summary
            .degraded
            .contains(&degraded_codes::ROUND_BLOCKED.to_owned())
    );

    let serialized = serde_json::to_string(&summary).map_err(|error| error.to_string())?;
    for forbidden in ["node_key=", "@example", "blocked-origin"] {
        assert!(
            !serialized.contains(forbidden),
            "summary leaked raw identity fragment {forbidden}: {serialized}"
        );
    }

    Ok(())
}

#[test]
fn two_peer_partition_rejoin_converges() -> TestResult {
    let peer_a = "peer-a";
    let peer_b = "peer-b";
    let origin_a = MeshOriginKey::new("origin-a");
    let origin_b = MeshOriginKey::new("origin-b");
    let events_a = vec![
        MeshReplayEvent::new(origin_a.clone(), 1, "a-1"),
        MeshReplayEvent::new(origin_a.clone(), 2, "a-2"),
        MeshReplayEvent::new(origin_a.clone(), 3, "a-3"),
    ];
    let events_b = vec![MeshReplayEvent::new(origin_b.clone(), 1, "b-1")];
    let planner = MeshRangePlanner {
        max_events_per_range: 2,
        ..MeshRangePlanner::default()
    };

    let mut durable_a: BTreeMap<MeshOriginKey, Vec<MeshReplayEvent>> =
        BTreeMap::from([(origin_a.clone(), events_a.clone())]);
    let mut durable_b: BTreeMap<MeshOriginKey, Vec<MeshReplayEvent>> =
        BTreeMap::from([(origin_b.clone(), events_b.clone())]);

    let first_plan = planner.plan(
        peer_a,
        &[MeshPeerCursor::new(peer_a, origin_a.clone(), 0)],
        &[MeshPeerTip::new(origin_a.clone(), 3)],
        &[],
        0,
        "2026-05-19T22:25:30.000Z",
    );
    assert_eq!(first_plan.requests.len(), 1);
    assert_eq!(first_plan.requests[0].start_seq, 1);
    assert_eq!(first_plan.requests[0].end_seq, 2);
    serve_range(&mut durable_b, &durable_a, &first_plan.requests[0])?;
    let cursor_b_after_first = cursor_for(&durable_b, &origin_a);
    assert_eq!(cursor_b_after_first, 2);

    let second_plan = planner.plan(
        peer_a,
        &[MeshPeerCursor::new(
            peer_a,
            origin_a.clone(),
            cursor_b_after_first,
        )],
        &[MeshPeerTip::new(origin_a.clone(), 3)],
        &[],
        0,
        "2026-05-19T22:25:30.000Z",
    );
    assert_eq!(second_plan.requests.len(), 1);
    assert_eq!(second_plan.requests[0].start_seq, 3);
    assert_eq!(second_plan.requests[0].end_seq, 3);
    serve_range(&mut durable_b, &durable_a, &second_plan.requests[0])?;

    let reverse_plan = planner.plan(
        peer_b,
        &[MeshPeerCursor::new(peer_b, origin_b.clone(), 0)],
        &[MeshPeerTip::new(origin_b.clone(), 1)],
        &[],
        0,
        "2026-05-19T22:25:30.000Z",
    );
    assert_eq!(reverse_plan.requests.len(), 1);
    serve_range(&mut durable_a, &durable_b, &reverse_plan.requests[0])?;

    assert_eq!(cursor_for(&durable_a, &origin_a), 3);
    assert_eq!(cursor_for(&durable_a, &origin_b), 1);
    assert_eq!(cursor_for(&durable_b, &origin_a), 3);
    assert_eq!(cursor_for(&durable_b, &origin_b), 1);
    assert_eq!(digest_for(&durable_a)?, digest_for(&durable_b)?);

    Ok(())
}

fn serve_range(
    destination: &mut BTreeMap<MeshOriginKey, Vec<MeshReplayEvent>>,
    source: &BTreeMap<MeshOriginKey, Vec<MeshReplayEvent>>,
    request: &anti_entropy_protocol::MeshRangeRequest,
) -> TestResult {
    let batch = source
        .get(&request.origin)
        .ok_or_else(|| format!("source missing origin {:?}", request.origin))?
        .iter()
        .filter(|event| event.seq >= request.start_seq && event.seq <= request.end_seq)
        .cloned()
        .collect::<Vec<_>>();
    summarize_event_range(batch.clone()).map_err(|error| format!("range validation: {error:?}"))?;
    destination
        .entry(request.origin.clone())
        .or_default()
        .extend(batch);
    let events = destination.entry(request.origin.clone()).or_default();
    events.sort_by(|left, right| {
        left.seq
            .cmp(&right.seq)
            .then_with(|| left.event_hash.cmp(&right.event_hash))
    });
    events.dedup_by(|left, right| left.seq == right.seq && left.event_hash == right.event_hash);
    Ok(())
}

fn cursor_for(node: &BTreeMap<MeshOriginKey, Vec<MeshReplayEvent>>, origin: &MeshOriginKey) -> u64 {
    cursor_after_durable_replay(
        0,
        node.get(origin)
            .into_iter()
            .flat_map(|events| events.iter().map(|event| event.seq)),
    )
}

fn digest_for(node: &BTreeMap<MeshOriginKey, Vec<MeshReplayEvent>>) -> Result<Vec<String>, String> {
    node.values()
        .map(|events| {
            summarize_event_range(events.clone())
                .map(|digest| digest.range_digest)
                .map_err(|error| format!("digest: {error:?}"))
        })
        .collect()
}
