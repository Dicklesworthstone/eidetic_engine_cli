//! SRR6.7 mesh anti-entropy protocol primitives.
//!
//! This module is transport-independent and intentionally pure. It owns the
//! deterministic mechanics that a future asupersync supervisor can schedule:
//! peer tip comparison, bounded range planning, retry/backoff posture, durable
//! cursor advancement, event-range digests, and the redaction-safe sync summary
//! consumed by `ee status` / `ee doctor`.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

/// Protocol documentation for this module's wire contract.
pub const ANTI_ENTROPY_PROTOCOL_DOC: &str = "docs/mesh/anti_entropy.md";

/// Public schema emitted by [`build_sync_summary`].
pub const MESH_ANTI_ENTROPY_SYNC_SUMMARY_SCHEMA_V1: &str = "ee.mesh.anti_entropy.v1";

/// Default initial retry delay from `docs/mesh/anti_entropy.md`.
pub const DEFAULT_INITIAL_BACKOFF_MS: u64 = 1_000;

/// Default maximum retry delay from `docs/mesh/anti_entropy.md`.
pub const DEFAULT_MAX_BACKOFF_MS: u64 = 60_000;

/// Default maximum attempts per peer/range from `docs/mesh/anti_entropy.md`.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 5;

/// Stable degraded codes used by the sync-summary schema.
pub mod degraded_codes {
    pub const ROUND_BLOCKED: &str = "mesh_anti_entropy_round_blocked";
    pub const PARTITION_OBSERVED: &str = "mesh_anti_entropy_partition_observed";
    pub const FORK_OBSERVED: &str = "mesh_anti_entropy_fork_observed";
    pub const PROTOCOL_ERROR: &str = "mesh_anti_entropy_protocol_error";
    pub const SUPERVISOR_BUDGET_EXCEEDED: &str = "mesh_anti_entropy_supervisor_budget_exceeded";
    pub const PEER_POLICY_REFUSED: &str = "mesh_anti_entropy_peer_policy_refused";
    pub const TRANSPORT_UNAVAILABLE: &str = "mesh_anti_entropy_transport_unavailable";
}

/// Executable scenario names referenced by the SRR6.7 e2e wrapper.
pub const ANTI_ENTROPY_PROTOCOL_SCENARIOS: &[&str] = &[
    "tip_advertise_builds_bounded_range_requests",
    "cursor_advances_only_after_durable_contiguous_replay",
    "range_digest_is_order_independent",
    "bounded_retry_blocks_after_max_attempts",
    "sync_summary_is_redaction_safe",
    "two_peer_partition_rejoin_converges",
];

/// One append-only origin stream.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshOriginKey {
    pub origin_node_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_workspace_id: Option<String>,
}

impl MeshOriginKey {
    #[must_use]
    pub fn new(origin_node_id: impl Into<String>) -> Self {
        Self {
            origin_node_id: origin_node_id.into(),
            origin_workspace_id: None,
        }
    }

    #[must_use]
    pub fn with_workspace(
        origin_node_id: impl Into<String>,
        origin_workspace_id: impl Into<String>,
    ) -> Self {
        Self {
            origin_node_id: origin_node_id.into(),
            origin_workspace_id: Some(origin_workspace_id.into()),
        }
    }

    #[must_use]
    pub fn redacted_alias(&self) -> String {
        match &self.origin_workspace_id {
            Some(workspace_id) => {
                origin_alias(&format!("{}|{}", self.origin_node_id, workspace_id))
            }
            None => origin_alias(&self.origin_node_id),
        }
    }
}

/// A peer's advertised contiguous frontier for one origin.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshPeerTip {
    pub origin: MeshOriginKey,
    pub last_contiguous_seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tip_event_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tip_audit_hash: Option<String>,
}

impl MeshPeerTip {
    #[must_use]
    pub fn new(origin: MeshOriginKey, last_contiguous_seq: u64) -> Self {
        Self {
            origin,
            last_contiguous_seq,
            tip_event_hash: None,
            tip_audit_hash: None,
        }
    }
}

/// Locally known durable cursor for a peer/origin stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshPeerCursor {
    pub peer_id: String,
    pub origin: MeshOriginKey,
    pub last_durable_seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tip_event_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tip_audit_hash: Option<String>,
}

impl MeshPeerCursor {
    #[must_use]
    pub fn new(peer_id: impl Into<String>, origin: MeshOriginKey, last_durable_seq: u64) -> Self {
        Self {
            peer_id: peer_id.into(),
            origin,
            last_durable_seq,
            tip_event_hash: None,
            tip_audit_hash: None,
        }
    }
}

/// Stable key for tracking retry state per peer/range.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshRangeKey {
    pub peer_id: String,
    pub origin: MeshOriginKey,
    pub start_seq: u64,
    pub end_seq: u64,
}

impl MeshRangeKey {
    #[must_use]
    pub fn new(
        peer_id: impl Into<String>,
        origin: MeshOriginKey,
        start_seq: u64,
        end_seq: u64,
    ) -> Self {
        Self {
            peer_id: peer_id.into(),
            origin,
            start_seq,
            end_seq,
        }
    }
}

/// Retry state persisted by the future supervisor between budgeted rounds.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshRangeRetryState {
    pub key: MeshRangeKey,
    pub attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_retry_after_epoch_ms: Option<u64>,
}

impl MeshRangeRetryState {
    #[must_use]
    pub fn new(key: MeshRangeKey, attempts: u32) -> Self {
        Self {
            key,
            attempts,
            next_retry_after_epoch_ms: None,
        }
    }
}

/// Range request sent to a peer. The peer ID is internal; public summaries use
/// aliases produced by [`peer_alias`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshRangeRequest {
    pub peer_id: String,
    pub origin: MeshOriginKey,
    pub start_seq: u64,
    pub end_seq: u64,
    pub attempt: u32,
}

impl MeshRangeRequest {
    #[must_use]
    pub fn key(&self) -> MeshRangeKey {
        MeshRangeKey::new(
            self.peer_id.clone(),
            self.origin.clone(),
            self.start_seq,
            self.end_seq,
        )
    }
}

/// Reason a range is represented in `blockedRanges`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshBlockedRangeReason {
    MaxAttemptsExceeded,
    PeerPolicyRefused,
    TransportUnavailable,
    SupervisorBudgetExceeded,
}

impl MeshBlockedRangeReason {
    #[must_use]
    pub fn degraded_code(self) -> &'static str {
        match self {
            Self::MaxAttemptsExceeded => degraded_codes::ROUND_BLOCKED,
            Self::PeerPolicyRefused => degraded_codes::PEER_POLICY_REFUSED,
            Self::TransportUnavailable => degraded_codes::TRANSPORT_UNAVAILABLE,
            Self::SupervisorBudgetExceeded => degraded_codes::SUPERVISOR_BUDGET_EXCEEDED,
        }
    }
}

/// Internal blocked range with raw identifiers. Render public output through
/// [`build_sync_summary`] so aliases are applied.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshBlockedRange {
    pub key: MeshRangeKey,
    pub retry_after: String,
    pub reason: MeshBlockedRangeReason,
}

/// Range waiting for its retry budget.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshWaitingRange {
    pub key: MeshRangeKey,
    pub next_retry_after_epoch_ms: u64,
}

/// Deterministic range-planning output.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshRangePlan {
    pub requests: Vec<MeshRangeRequest>,
    pub waiting_ranges: Vec<MeshWaitingRange>,
    pub blocked_ranges: Vec<MeshBlockedRange>,
}

/// Bounded retry/backoff policy. Defaults match `docs/mesh/anti_entropy.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAntiEntropyRetryPolicy {
    pub initial_ms: u64,
    pub multiplier: u64,
    pub max_ms: u64,
    pub max_attempts: u32,
}

impl Default for MeshAntiEntropyRetryPolicy {
    fn default() -> Self {
        Self {
            initial_ms: DEFAULT_INITIAL_BACKOFF_MS,
            multiplier: 2,
            max_ms: DEFAULT_MAX_BACKOFF_MS,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
        }
    }
}

impl MeshAntiEntropyRetryPolicy {
    #[must_use]
    pub fn next_delay_ms(&self, attempts_after_failure: u32) -> u64 {
        let exponent = attempts_after_failure.saturating_sub(1);
        let mut delay = self.initial_ms.max(1);
        for _ in 0..exponent {
            delay = delay.saturating_mul(self.multiplier.max(1));
            if delay >= self.max_ms {
                return self.max_ms;
            }
        }
        delay.min(self.max_ms)
    }
}

/// Pure planner that emits at most one bounded range request per origin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeshRangePlanner {
    pub policy: MeshAntiEntropyRetryPolicy,
    pub max_events_per_range: u64,
}

impl Default for MeshRangePlanner {
    fn default() -> Self {
        Self {
            policy: MeshAntiEntropyRetryPolicy::default(),
            max_events_per_range: 512,
        }
    }
}

impl MeshRangePlanner {
    #[must_use]
    pub fn plan(
        &self,
        peer_id: &str,
        local_cursors: &[MeshPeerCursor],
        peer_tips: &[MeshPeerTip],
        retry_state: &[MeshRangeRetryState],
        now_epoch_ms: u64,
        retry_after_rfc3339: &str,
    ) -> MeshRangePlan {
        let local_by_origin = cursor_map_for_peer(peer_id, local_cursors);
        let retry_by_key = retry_state
            .iter()
            .map(|state| (state.key.clone(), state))
            .collect::<BTreeMap<_, _>>();
        let mut tips_by_origin: BTreeMap<MeshOriginKey, u64> = BTreeMap::new();
        for tip in peer_tips {
            tips_by_origin
                .entry(tip.origin.clone())
                .and_modify(|existing| *existing = (*existing).max(tip.last_contiguous_seq))
                .or_insert(tip.last_contiguous_seq);
        }

        let mut plan = MeshRangePlan::default();
        let max_events = self.max_events_per_range.max(1);

        for (origin, peer_tip) in tips_by_origin {
            let local_seq = local_by_origin.get(&origin).copied().unwrap_or(0);
            if peer_tip <= local_seq {
                continue;
            }

            let start_seq = local_seq.saturating_add(1);
            let end_seq = peer_tip.min(start_seq.saturating_add(max_events - 1));
            let key = MeshRangeKey::new(peer_id.to_owned(), origin.clone(), start_seq, end_seq);
            let retry = retry_by_key.get(&key).copied();
            let attempts = retry.map(|state| state.attempts).unwrap_or(0);

            if attempts >= self.policy.max_attempts {
                plan.blocked_ranges.push(MeshBlockedRange {
                    key,
                    retry_after: retry_after_rfc3339.to_owned(),
                    reason: MeshBlockedRangeReason::MaxAttemptsExceeded,
                });
                continue;
            }

            if let Some(next_retry_after_epoch_ms) =
                retry.and_then(|state| state.next_retry_after_epoch_ms)
            {
                if next_retry_after_epoch_ms > now_epoch_ms {
                    plan.waiting_ranges.push(MeshWaitingRange {
                        key,
                        next_retry_after_epoch_ms,
                    });
                    continue;
                }
            }

            plan.requests.push(MeshRangeRequest {
                peer_id: peer_id.to_owned(),
                origin,
                start_seq,
                end_seq,
                attempt: attempts.saturating_add(1),
            });
        }

        plan
    }
}

/// Event metadata needed to validate and digest one range batch.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshReplayEvent {
    pub origin: MeshOriginKey,
    pub seq: u64,
    pub event_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit_hash: Option<String>,
}

impl MeshReplayEvent {
    #[must_use]
    pub fn new(origin: MeshOriginKey, seq: u64, event_hash: impl Into<String>) -> Self {
        Self {
            origin,
            seq,
            event_hash: event_hash.into(),
            audit_hash: None,
        }
    }
}

/// Stable digest of a contiguous event range.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshRangeDigest {
    pub origin: MeshOriginKey,
    pub start_seq: u64,
    pub end_seq: u64,
    pub event_count: usize,
    pub first_event_hash: String,
    pub last_event_hash: String,
    pub range_digest: String,
}

/// Protocol validation error for an inbound range batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MeshProtocolError {
    EmptyRange,
    MixedOrigins {
        expected_origin: MeshOriginKey,
        actual_origin: MeshOriginKey,
    },
    DuplicateSeq {
        origin: MeshOriginKey,
        seq: u64,
    },
    NonContiguousRange {
        origin: MeshOriginKey,
        expected_seq: u64,
        actual_seq: u64,
    },
}

/// Build an order-independent digest after validating that the delivered batch
/// is one origin and one contiguous sequence span.
pub fn summarize_event_range(
    events: impl IntoIterator<Item = MeshReplayEvent>,
) -> Result<MeshRangeDigest, MeshProtocolError> {
    let mut events = events.into_iter().collect::<Vec<_>>();
    if events.is_empty() {
        return Err(MeshProtocolError::EmptyRange);
    }
    events.sort_by(|left, right| {
        left.origin
            .cmp(&right.origin)
            .then_with(|| left.seq.cmp(&right.seq))
            .then_with(|| left.event_hash.cmp(&right.event_hash))
    });

    let origin = events[0].origin.clone();
    let mut seen = BTreeSet::new();
    for event in &events {
        if event.origin != origin {
            return Err(MeshProtocolError::MixedOrigins {
                expected_origin: origin,
                actual_origin: event.origin.clone(),
            });
        }
        if !seen.insert(event.seq) {
            return Err(MeshProtocolError::DuplicateSeq {
                origin: event.origin.clone(),
                seq: event.seq,
            });
        }
    }

    let start_seq = events[0].seq;
    for (offset, event) in events.iter().enumerate() {
        let expected_seq = start_seq.saturating_add(offset as u64);
        if event.seq != expected_seq {
            return Err(MeshProtocolError::NonContiguousRange {
                origin: origin.clone(),
                expected_seq,
                actual_seq: event.seq,
            });
        }
    }

    let mut digest_input = String::new();
    digest_input.push_str(&origin.origin_node_id);
    digest_input.push('|');
    if let Some(workspace_id) = &origin.origin_workspace_id {
        digest_input.push_str(workspace_id);
    }
    for event in &events {
        digest_input.push('|');
        digest_input.push_str(&event.seq.to_string());
        digest_input.push(':');
        digest_input.push_str(&event.event_hash);
        if let Some(audit_hash) = &event.audit_hash {
            digest_input.push(':');
            digest_input.push_str(audit_hash);
        }
    }

    Ok(MeshRangeDigest {
        origin,
        start_seq,
        end_seq: events.last().map(|event| event.seq).unwrap_or(start_seq),
        event_count: events.len(),
        first_event_hash: events[0].event_hash.clone(),
        last_event_hash: events
            .last()
            .map(|event| event.event_hash.clone())
            .unwrap_or_default(),
        range_digest: stable_hash_hex(&digest_input, 16),
    })
}

/// Advance a durable cursor only across contiguous accepted event sequences.
/// The caller supplies the set that has already survived durable replay.
#[must_use]
pub fn cursor_after_durable_replay(
    previous_cursor: u64,
    durable_accepted_seqs: impl IntoIterator<Item = u64>,
) -> u64 {
    if previous_cursor == u64::MAX {
        return previous_cursor;
    }

    let durable = durable_accepted_seqs.into_iter().collect::<BTreeSet<_>>();
    let mut cursor = previous_cursor;
    let mut next_seq = cursor.saturating_add(1);
    while durable.contains(&next_seq) {
        cursor = next_seq;
        if cursor == u64::MAX {
            break;
        }
        next_seq = next_seq.saturating_add(1);
    }
    cursor
}

/// Replay counts for one peer in a completed round.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MeshRoundPeerOutcome {
    pub peer_id: String,
    pub events_accepted: u64,
    pub events_duplicate: u64,
    pub events_forked: u64,
    pub ranges_requested: u64,
    pub ranges_fulfilled: u64,
}

impl MeshRoundPeerOutcome {
    #[must_use]
    pub fn new(peer_id: impl Into<String>) -> Self {
        Self {
            peer_id: peer_id.into(),
            ..Self::default()
        }
    }
}

/// Inputs for rendering a redaction-safe sync summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshSyncSummaryInput {
    pub last_round_completed_at: Option<String>,
    pub origins_tracked: usize,
    pub peer_outcomes: Vec<MeshRoundPeerOutcome>,
    pub retry_policy: MeshAntiEntropyRetryPolicy,
    pub current_attempts: u64,
    pub next_retry_after: Option<String>,
    pub blocked_ranges: Vec<MeshBlockedRange>,
    pub degraded: Vec<String>,
}

/// Redaction-safe `ee.mesh.anti_entropy.v1` summary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAntiEntropySyncSummary {
    pub schema: &'static str,
    pub last_round_completed_at: Option<String>,
    pub origins_tracked: usize,
    pub peer_count: usize,
    pub per_peer_counts: Vec<MeshPeerSyncCounts>,
    pub backoff_posture: MeshBackoffPosture,
    pub blocked_ranges: Vec<MeshBlockedRangeSummary>,
    pub degraded: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshPeerSyncCounts {
    pub peer_alias: String,
    pub events_accepted: u64,
    pub events_duplicate: u64,
    pub events_forked: u64,
    pub ranges_requested: u64,
    pub ranges_fulfilled: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshBackoffPosture {
    pub initial_ms: u64,
    pub max_ms: u64,
    pub max_attempts: u32,
    pub current_attempts: u64,
    pub next_retry_after: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshBlockedRangeSummary {
    pub peer_alias: String,
    pub origin_alias: String,
    pub start_seq: u64,
    pub end_seq: u64,
    pub retry_after: String,
    pub reason: MeshBlockedRangeReason,
}

/// Render status/doctor-ready summary output without leaking raw peer/origin
/// identities.
#[must_use]
pub fn build_sync_summary(input: MeshSyncSummaryInput) -> MeshAntiEntropySyncSummary {
    let mut counts_by_peer: BTreeMap<String, MeshPeerSyncCounts> = BTreeMap::new();
    for outcome in input.peer_outcomes {
        let alias = peer_alias(&outcome.peer_id);
        let entry = counts_by_peer
            .entry(alias.clone())
            .or_insert_with(|| MeshPeerSyncCounts {
                peer_alias: alias,
                events_accepted: 0,
                events_duplicate: 0,
                events_forked: 0,
                ranges_requested: 0,
                ranges_fulfilled: 0,
            });
        entry.events_accepted = entry
            .events_accepted
            .saturating_add(outcome.events_accepted);
        entry.events_duplicate = entry
            .events_duplicate
            .saturating_add(outcome.events_duplicate);
        entry.events_forked = entry.events_forked.saturating_add(outcome.events_forked);
        entry.ranges_requested = entry
            .ranges_requested
            .saturating_add(outcome.ranges_requested);
        entry.ranges_fulfilled = entry
            .ranges_fulfilled
            .saturating_add(outcome.ranges_fulfilled);
    }

    let mut degraded = input.degraded.into_iter().collect::<BTreeSet<_>>();
    let mut blocked_ranges = input
        .blocked_ranges
        .into_iter()
        .map(|range| {
            degraded.insert(range.reason.degraded_code().to_owned());
            MeshBlockedRangeSummary {
                peer_alias: peer_alias(&range.key.peer_id),
                origin_alias: range.key.origin.redacted_alias(),
                start_seq: range.key.start_seq,
                end_seq: range.key.end_seq,
                retry_after: range.retry_after,
                reason: range.reason,
            }
        })
        .collect::<Vec<_>>();

    blocked_ranges.sort_by(|left, right| {
        left.peer_alias
            .cmp(&right.peer_alias)
            .then_with(|| left.origin_alias.cmp(&right.origin_alias))
            .then_with(|| left.start_seq.cmp(&right.start_seq))
            .then_with(|| left.end_seq.cmp(&right.end_seq))
            .then_with(|| left.reason.cmp(&right.reason))
    });

    MeshAntiEntropySyncSummary {
        schema: MESH_ANTI_ENTROPY_SYNC_SUMMARY_SCHEMA_V1,
        last_round_completed_at: input.last_round_completed_at,
        origins_tracked: input.origins_tracked,
        peer_count: counts_by_peer.len(),
        per_peer_counts: counts_by_peer.into_values().collect(),
        backoff_posture: MeshBackoffPosture {
            initial_ms: input.retry_policy.initial_ms,
            max_ms: input.retry_policy.max_ms,
            max_attempts: input.retry_policy.max_attempts,
            current_attempts: input.current_attempts,
            next_retry_after: input.next_retry_after,
        },
        blocked_ranges,
        degraded: degraded.into_iter().collect(),
    }
}

#[must_use]
pub fn peer_alias(peer_id: &str) -> String {
    format!("peer_{}", stable_hash_hex(peer_id, 12))
}

#[must_use]
pub fn origin_alias(origin_node_id: &str) -> String {
    format!("origin_{}", stable_hash_hex(origin_node_id, 12))
}

fn cursor_map_for_peer(
    peer_id: &str,
    local_cursors: &[MeshPeerCursor],
) -> BTreeMap<MeshOriginKey, u64> {
    let mut by_origin = BTreeMap::new();
    for cursor in local_cursors {
        if cursor.peer_id == peer_id {
            by_origin
                .entry(cursor.origin.clone())
                .and_modify(|existing: &mut u64| {
                    *existing = (*existing).max(cursor.last_durable_seq);
                })
                .or_insert(cursor.last_durable_seq);
        }
    }
    by_origin
}

fn stable_hash_hex(value: &str, width: usize) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = FNV_OFFSET;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }

    let hex = format!("{hash:016x}");
    hex.chars().take(width.min(hex.len())).collect()
}
