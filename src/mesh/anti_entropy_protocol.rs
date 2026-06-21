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

/// Public schema emitted by [`build_freshness_probe_summary`].
pub const MESH_FRESHNESS_PROBE_SUMMARY_SCHEMA_V1: &str = "ee.mesh.freshness_probe.v1";

/// Public schema emitted by [`build_two_tier_budget_summary`].
pub const MESH_TWO_TIER_BUDGET_SUMMARY_SCHEMA_V1: &str = "ee.mesh.two_tier_budget.v1";

/// Default initial retry delay from `docs/mesh/anti_entropy.md`.
pub const DEFAULT_INITIAL_BACKOFF_MS: u64 = 1_000;

/// Default maximum retry delay from `docs/mesh/anti_entropy.md`.
pub const DEFAULT_MAX_BACKOFF_MS: u64 = 60_000;

/// Default maximum attempts per peer/range from `docs/mesh/anti_entropy.md`.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 5;

/// Default Tier-1 local-answer p50 latency budget for mesh-aware reads.
pub const DEFAULT_TIER1_LOCAL_P50_BUDGET_MS: u64 = 75;

/// Default Tier-1 local-answer p99 latency budget for mesh-aware reads.
pub const DEFAULT_TIER1_LOCAL_P99_BUDGET_MS: u64 = 250;

/// Default async peer freshness timeout budget.
pub const DEFAULT_PEER_PROBE_TIMEOUT_BUDGET_MS: u64 = 750;

/// Default maximum stale-read window under normal mesh sync cadence.
pub const DEFAULT_STALE_READ_WINDOW_BUDGET_MS: u64 = 5_000;

/// Default maximum number of events accepted in one sync batch.
pub const DEFAULT_SYNC_BATCH_BUDGET_EVENTS: u64 = 512;

/// Default lazy body-cache growth budget for one foreground read.
pub const DEFAULT_BODY_CACHE_BUDGET_BYTES: u64 = 512 * 1024;

/// Default index-job amplification budget for one peer sync round.
pub const DEFAULT_INDEX_JOB_AMPLIFICATION_BUDGET: u64 = 16;

/// Default peer fanout budget for async freshness probes.
pub const DEFAULT_PEER_PROBE_FANOUT_BUDGET: u64 = 32;

/// Stable degraded codes used by the sync-summary schema.
pub mod degraded_codes {
    pub const ROUND_BLOCKED: &str = "mesh_anti_entropy_round_blocked";
    pub const PARTITION_OBSERVED: &str = "mesh_anti_entropy_partition_observed";
    pub const FORK_OBSERVED: &str = "mesh_anti_entropy_fork_observed";
    pub const PROTOCOL_ERROR: &str = "mesh_anti_entropy_protocol_error";
    pub const SUPERVISOR_BUDGET_EXCEEDED: &str = "mesh_anti_entropy_supervisor_budget_exceeded";
    pub const PEER_POLICY_REFUSED: &str = "mesh_anti_entropy_peer_policy_refused";
    pub const TRANSPORT_UNAVAILABLE: &str = "mesh_anti_entropy_transport_unavailable";
    pub const FRESHNESS_PEER_TIMEOUT: &str = "mesh_freshness_peer_timeout";
    pub const FRESHNESS_PEER_POLICY_REFUSED: &str = "mesh_freshness_peer_policy_refused";
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
        origin_alias(&origin_identity_hash_input(self))
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
            max_events_per_range: DEFAULT_SYNC_BATCH_BUDGET_EVENTS,
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

    let mut digest_input = origin_identity_hash_input(&origin);
    for event in &events {
        push_hash_field(&mut digest_input, "seq", &event.seq.to_string());
        push_hash_field(&mut digest_input, "event", &event.event_hash);
        match &event.audit_hash {
            Some(audit_hash) => push_hash_field(&mut digest_input, "audit", audit_hash),
            None => push_hash_marker(&mut digest_input, "audit_none"),
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

/// Redaction-safe query summary used by the async freshness probe.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshFreshnessQuerySummary {
    pub query_fingerprint: String,
    pub summary_hash: String,
}

impl MeshFreshnessQuerySummary {
    #[must_use]
    pub fn new(query_fingerprint: impl Into<String>) -> Self {
        let raw_query_fingerprint = query_fingerprint.into();
        let summary_hash = format!("query_{}", stable_hash_hex(&raw_query_fingerprint, 16));
        Self {
            query_fingerprint: summary_hash.clone(),
            summary_hash,
        }
    }
}

/// One peer's non-blocking freshness probe result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshPeerFreshnessProbe {
    pub peer_id: String,
    pub policy_allowed: bool,
    pub timed_out: bool,
    pub peer_tips: Vec<MeshPeerTip>,
    pub query_summary: Option<MeshFreshnessQuerySummary>,
}

impl MeshPeerFreshnessProbe {
    #[must_use]
    pub fn allowed(peer_id: impl Into<String>, peer_tips: Vec<MeshPeerTip>) -> Self {
        Self {
            peer_id: peer_id.into(),
            policy_allowed: true,
            timed_out: false,
            peer_tips,
            query_summary: None,
        }
    }

    #[must_use]
    pub fn denied(peer_id: impl Into<String>) -> Self {
        Self {
            peer_id: peer_id.into(),
            policy_allowed: false,
            timed_out: false,
            peer_tips: Vec::new(),
            query_summary: None,
        }
    }

    #[must_use]
    pub fn timeout(peer_id: impl Into<String>) -> Self {
        Self {
            peer_id: peer_id.into(),
            policy_allowed: true,
            timed_out: true,
            peer_tips: Vec::new(),
            query_summary: None,
        }
    }

    #[must_use]
    pub fn with_query_summary(mut self, summary: MeshFreshnessQuerySummary) -> Self {
        self.query_summary = Some(summary);
        self
    }
}

/// Inputs for rendering an async peer freshness summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshFreshnessProbeInput {
    pub mesh_enabled: bool,
    pub local_query_summary: Option<MeshFreshnessQuerySummary>,
    pub local_cursors: Vec<MeshPeerCursor>,
    pub peer_probes: Vec<MeshPeerFreshnessProbe>,
    pub peer_timeout_ms: u64,
    pub checked_at: Option<String>,
}

/// Redaction-safe result from the non-blocking freshness probe path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshFreshnessProbeSummary {
    pub schema: &'static str,
    pub status: String,
    pub checked_at: Option<String>,
    pub local_answer_blocking: bool,
    pub probe_execution: &'static str,
    pub body_transfer_allowed: bool,
    pub peer_timeout_ms: u64,
    pub peer_count: usize,
    pub peer_probes_scheduled: usize,
    pub query_summary: Option<MeshFreshnessQuerySummary>,
    pub revision_availability: Vec<MeshRevisionAvailabilitySignal>,
    pub per_peer: Vec<MeshFreshnessPeerSummary>,
    pub degraded: Vec<String>,
}

/// Signal that a peer may hold newer relevant material for the answered query.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshRevisionAvailabilitySignal {
    pub peer_alias: String,
    pub origin_alias: String,
    pub local_last_durable_seq: u64,
    pub peer_last_contiguous_seq: u64,
    pub missing_event_count: u64,
    pub relevance_basis: String,
    pub evidence_id: String,
}

/// Per-peer redaction-safe probe status.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshFreshnessPeerSummary {
    pub peer_alias: String,
    pub status: String,
    pub advertised_origin_count: usize,
    pub max_missing_event_count: u64,
    pub query_summary_matched: Option<bool>,
    pub body_transfer_allowed: bool,
}

/// Inputs for a status/doctor-ready SRR6.20 two-tier budget report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshTwoTierBudgetInput {
    pub mesh_enabled: bool,
    pub baseline_tier1_p50_ms: u64,
    pub observed_tier1_p50_ms: u64,
    pub baseline_tier1_p99_ms: u64,
    pub observed_tier1_p99_ms: u64,
    pub peer_timeout_ms: u64,
    pub max_peer_probe_elapsed_ms: u64,
    pub stale_read_window_ms: u64,
    pub peer_count: usize,
    pub body_cache_bytes: u64,
    pub sync_batch_events: u64,
    pub index_jobs_enqueued: u64,
    pub cache_hit_path_observed: bool,
    pub checked_at: Option<String>,
}

/// Redaction-safe two-tier latency/freshness/resource budget summary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshTwoTierBudgetSummary {
    pub schema: &'static str,
    pub status: String,
    pub checked_at: Option<String>,
    pub mesh_enabled: bool,
    pub local_answer_blocking: bool,
    pub network_on_tier1: bool,
    pub cache_hit_path_observed: bool,
    pub tier1_latency: MeshTier1LatencyBudget,
    pub freshness: MeshFreshnessBudget,
    pub resources: MeshResourceBudget,
    pub degraded: Vec<String>,
}

/// Foreground local-read latency budget, before and after mesh is enabled.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshTier1LatencyBudget {
    pub baseline_p50_ms: u64,
    pub observed_p50_ms: u64,
    pub p50_budget_ms: u64,
    pub p50_regression_ms: u64,
    pub baseline_p99_ms: u64,
    pub observed_p99_ms: u64,
    pub p99_budget_ms: u64,
    pub p99_regression_ms: u64,
    pub within_budget: bool,
}

/// Async freshness budget status for non-blocking peer probes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshFreshnessBudget {
    pub peer_timeout_ms: u64,
    pub peer_timeout_budget_ms: u64,
    pub max_peer_probe_elapsed_ms: u64,
    pub stale_read_window_ms: u64,
    pub stale_read_window_budget_ms: u64,
    pub peer_count: usize,
    pub peer_count_budget: u64,
    pub probe_execution: &'static str,
    pub within_budget: bool,
}

/// Resource budget status for background sync and lazy body/cache work.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshResourceBudget {
    pub body_cache_bytes: u64,
    pub body_cache_budget_bytes: u64,
    pub sync_batch_events: u64,
    pub sync_batch_budget_events: u64,
    pub index_jobs_enqueued: u64,
    pub index_job_budget: u64,
    pub body_transfer_allowed_on_tier1: bool,
    pub within_budget: bool,
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

    let mut peer_aliases = counts_by_peer.keys().cloned().collect::<BTreeSet<_>>();
    let mut degraded = input.degraded.into_iter().collect::<BTreeSet<_>>();
    let mut blocked_ranges = input
        .blocked_ranges
        .into_iter()
        .map(|range| {
            let peer_alias = peer_alias(&range.key.peer_id);
            peer_aliases.insert(peer_alias.clone());
            degraded.insert(range.reason.degraded_code().to_owned());
            MeshBlockedRangeSummary {
                peer_alias,
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
        peer_count: peer_aliases.len(),
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

/// Build the SRR6.20 proof summary consumed by status/doctor and structured
/// tests. The summary is pure and redaction-safe: it carries budgets and
/// observed counters only, never peer identities, paths, queries, or bodies.
#[must_use]
pub fn build_two_tier_budget_summary(input: MeshTwoTierBudgetInput) -> MeshTwoTierBudgetSummary {
    let peer_count = u64::try_from(input.peer_count).unwrap_or(u64::MAX);
    let p50_regression_ms = input
        .observed_tier1_p50_ms
        .saturating_sub(input.baseline_tier1_p50_ms);
    let p99_regression_ms = input
        .observed_tier1_p99_ms
        .saturating_sub(input.baseline_tier1_p99_ms);

    let latency_within_budget = !input.mesh_enabled
        || (input.observed_tier1_p50_ms <= DEFAULT_TIER1_LOCAL_P50_BUDGET_MS
            && input.observed_tier1_p99_ms <= DEFAULT_TIER1_LOCAL_P99_BUDGET_MS);
    let freshness_within_budget = !input.mesh_enabled
        || (input.peer_timeout_ms <= DEFAULT_PEER_PROBE_TIMEOUT_BUDGET_MS
            && input.max_peer_probe_elapsed_ms <= input.peer_timeout_ms
            && input.stale_read_window_ms <= DEFAULT_STALE_READ_WINDOW_BUDGET_MS
            && peer_count <= DEFAULT_PEER_PROBE_FANOUT_BUDGET);
    let resources_within_budget = !input.mesh_enabled
        || (input.body_cache_bytes <= DEFAULT_BODY_CACHE_BUDGET_BYTES
            && input.sync_batch_events <= DEFAULT_SYNC_BATCH_BUDGET_EVENTS
            && input.index_jobs_enqueued <= DEFAULT_INDEX_JOB_AMPLIFICATION_BUDGET);

    let mut degraded = BTreeSet::new();
    if input.mesh_enabled {
        if !latency_within_budget
            || input.stale_read_window_ms > DEFAULT_STALE_READ_WINDOW_BUDGET_MS
            || peer_count > DEFAULT_PEER_PROBE_FANOUT_BUDGET
            || !resources_within_budget
            || !input.cache_hit_path_observed
        {
            degraded.insert(degraded_codes::SUPERVISOR_BUDGET_EXCEEDED.to_owned());
        }
        if input.peer_timeout_ms > DEFAULT_PEER_PROBE_TIMEOUT_BUDGET_MS
            || input.max_peer_probe_elapsed_ms > input.peer_timeout_ms
        {
            degraded.insert(degraded_codes::FRESHNESS_PEER_TIMEOUT.to_owned());
        }
    }

    let status = if !input.mesh_enabled {
        "disabled"
    } else if degraded.is_empty() {
        "within_budget"
    } else {
        "degraded"
    };

    MeshTwoTierBudgetSummary {
        schema: MESH_TWO_TIER_BUDGET_SUMMARY_SCHEMA_V1,
        status: status.to_owned(),
        checked_at: input.checked_at,
        mesh_enabled: input.mesh_enabled,
        local_answer_blocking: false,
        network_on_tier1: false,
        cache_hit_path_observed: input.cache_hit_path_observed,
        tier1_latency: MeshTier1LatencyBudget {
            baseline_p50_ms: input.baseline_tier1_p50_ms,
            observed_p50_ms: input.observed_tier1_p50_ms,
            p50_budget_ms: DEFAULT_TIER1_LOCAL_P50_BUDGET_MS,
            p50_regression_ms,
            baseline_p99_ms: input.baseline_tier1_p99_ms,
            observed_p99_ms: input.observed_tier1_p99_ms,
            p99_budget_ms: DEFAULT_TIER1_LOCAL_P99_BUDGET_MS,
            p99_regression_ms,
            within_budget: latency_within_budget,
        },
        freshness: MeshFreshnessBudget {
            peer_timeout_ms: input.peer_timeout_ms,
            peer_timeout_budget_ms: DEFAULT_PEER_PROBE_TIMEOUT_BUDGET_MS,
            max_peer_probe_elapsed_ms: input.max_peer_probe_elapsed_ms,
            stale_read_window_ms: input.stale_read_window_ms,
            stale_read_window_budget_ms: DEFAULT_STALE_READ_WINDOW_BUDGET_MS,
            peer_count: input.peer_count,
            peer_count_budget: DEFAULT_PEER_PROBE_FANOUT_BUDGET,
            probe_execution: if input.mesh_enabled {
                "async_after_local_answer"
            } else {
                "mesh_disabled_noop"
            },
            within_budget: freshness_within_budget,
        },
        resources: MeshResourceBudget {
            body_cache_bytes: input.body_cache_bytes,
            body_cache_budget_bytes: DEFAULT_BODY_CACHE_BUDGET_BYTES,
            sync_batch_events: input.sync_batch_events,
            sync_batch_budget_events: DEFAULT_SYNC_BATCH_BUDGET_EVENTS,
            index_jobs_enqueued: input.index_jobs_enqueued,
            index_job_budget: DEFAULT_INDEX_JOB_AMPLIFICATION_BUDGET,
            body_transfer_allowed_on_tier1: false,
            within_budget: resources_within_budget,
        },
        degraded: degraded.into_iter().collect(),
    }
}

/// Build the non-blocking peer freshness result consumed after a local answer
/// has already returned. The probe compares tips and optional query summaries
/// only; it never schedules body transfer.
#[must_use]
pub fn build_freshness_probe_summary(input: MeshFreshnessProbeInput) -> MeshFreshnessProbeSummary {
    if !input.mesh_enabled {
        return MeshFreshnessProbeSummary {
            schema: MESH_FRESHNESS_PROBE_SUMMARY_SCHEMA_V1,
            status: "disabled".to_owned(),
            checked_at: input.checked_at,
            local_answer_blocking: false,
            probe_execution: "mesh_disabled_noop",
            body_transfer_allowed: false,
            peer_timeout_ms: input.peer_timeout_ms,
            peer_count: 0,
            peer_probes_scheduled: 0,
            query_summary: input.local_query_summary,
            revision_availability: Vec::new(),
            per_peer: Vec::new(),
            degraded: Vec::new(),
        };
    }

    let mut degraded = BTreeSet::new();
    let mut per_peer = Vec::new();
    let mut revision_availability = Vec::new();
    let local_query_summary = input.local_query_summary.clone();

    for probe in input.peer_probes {
        let peer_alias = peer_alias(&probe.peer_id);
        if !probe.policy_allowed {
            degraded.insert(degraded_codes::FRESHNESS_PEER_POLICY_REFUSED.to_owned());
            per_peer.push(MeshFreshnessPeerSummary {
                peer_alias,
                status: "denied".to_owned(),
                advertised_origin_count: 0,
                max_missing_event_count: 0,
                query_summary_matched: None,
                body_transfer_allowed: false,
            });
            continue;
        }

        if probe.timed_out {
            degraded.insert(degraded_codes::FRESHNESS_PEER_TIMEOUT.to_owned());
            per_peer.push(MeshFreshnessPeerSummary {
                peer_alias,
                status: "timeout".to_owned(),
                advertised_origin_count: 0,
                max_missing_event_count: 0,
                query_summary_matched: None,
                body_transfer_allowed: false,
            });
            continue;
        }

        let query_summary_matched = freshness_query_summary_matches(
            local_query_summary.as_ref(),
            probe.query_summary.as_ref(),
        );
        let tips_by_origin = freshness_tips_by_origin(&probe.peer_tips);
        let advertised_origin_count = tips_by_origin.len();
        if query_summary_matched == Some(false) {
            per_peer.push(MeshFreshnessPeerSummary {
                peer_alias,
                status: "query_summary_miss".to_owned(),
                advertised_origin_count,
                max_missing_event_count: 0,
                query_summary_matched,
                body_transfer_allowed: false,
            });
            continue;
        }

        let local_by_origin = cursor_map_for_peer(&probe.peer_id, &input.local_cursors);
        let mut max_missing_event_count = 0u64;
        let mut peer_signal_count = 0usize;

        for (origin, peer_seq) in tips_by_origin {
            let local_seq = local_by_origin.get(&origin).copied().unwrap_or(0);
            if peer_seq <= local_seq {
                continue;
            }
            let missing_event_count = peer_seq.saturating_sub(local_seq);
            max_missing_event_count = max_missing_event_count.max(missing_event_count);
            peer_signal_count = peer_signal_count.saturating_add(1);
            let origin_alias = origin.redacted_alias();
            let relevance_basis = if query_summary_matched == Some(true) {
                "query_summary_match"
            } else {
                "peer_tip_advanced"
            }
            .to_owned();
            let evidence_id = freshness_evidence_id(
                &peer_alias,
                &origin_alias,
                local_query_summary
                    .as_ref()
                    .map(|summary| summary.summary_hash.as_str()),
                local_seq,
                peer_seq,
            );
            revision_availability.push(MeshRevisionAvailabilitySignal {
                peer_alias: peer_alias.clone(),
                origin_alias,
                local_last_durable_seq: local_seq,
                peer_last_contiguous_seq: peer_seq,
                missing_event_count,
                relevance_basis,
                evidence_id,
            });
        }

        per_peer.push(MeshFreshnessPeerSummary {
            peer_alias,
            status: if peer_signal_count == 0 {
                "stale_or_current".to_owned()
            } else {
                "fresher".to_owned()
            },
            advertised_origin_count,
            max_missing_event_count,
            query_summary_matched,
            body_transfer_allowed: false,
        });
    }

    revision_availability.sort_by(|left, right| {
        left.peer_alias
            .cmp(&right.peer_alias)
            .then_with(|| left.origin_alias.cmp(&right.origin_alias))
            .then_with(|| {
                left.local_last_durable_seq
                    .cmp(&right.local_last_durable_seq)
            })
            .then_with(|| {
                left.peer_last_contiguous_seq
                    .cmp(&right.peer_last_contiguous_seq)
            })
    });
    revision_availability.dedup_by(|left, right| {
        left.peer_alias == right.peer_alias
            && left.origin_alias == right.origin_alias
            && left.peer_last_contiguous_seq == right.peer_last_contiguous_seq
    });
    per_peer.sort_by(|left, right| left.peer_alias.cmp(&right.peer_alias));

    let status = if !revision_availability.is_empty() {
        "revision_available"
    } else if !degraded.is_empty() {
        "degraded"
    } else {
        "current"
    };

    MeshFreshnessProbeSummary {
        schema: MESH_FRESHNESS_PROBE_SUMMARY_SCHEMA_V1,
        status: status.to_owned(),
        checked_at: input.checked_at,
        local_answer_blocking: false,
        probe_execution: "async_after_local_answer",
        body_transfer_allowed: false,
        peer_timeout_ms: input.peer_timeout_ms,
        peer_count: per_peer.len(),
        peer_probes_scheduled: per_peer.len(),
        query_summary: local_query_summary,
        revision_availability,
        per_peer,
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

fn origin_identity_hash_input(origin: &MeshOriginKey) -> String {
    let mut input = String::new();
    push_hash_field(&mut input, "origin_node", &origin.origin_node_id);
    match &origin.origin_workspace_id {
        Some(workspace_id) => push_hash_field(&mut input, "origin_workspace", workspace_id),
        None => push_hash_marker(&mut input, "origin_workspace_none"),
    }
    input
}

fn push_hash_field(input: &mut String, label: &str, value: &str) {
    input.push_str(label);
    input.push('#');
    input.push_str(&value.len().to_string());
    input.push(':');
    input.push_str(value);
    input.push(';');
}

fn push_hash_marker(input: &mut String, label: &str) {
    input.push_str(label);
    input.push_str("#0:;");
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

fn freshness_tips_by_origin(peer_tips: &[MeshPeerTip]) -> BTreeMap<MeshOriginKey, u64> {
    let mut tips_by_origin = BTreeMap::new();
    for tip in peer_tips {
        tips_by_origin
            .entry(tip.origin.clone())
            .and_modify(|existing: &mut u64| {
                *existing = (*existing).max(tip.last_contiguous_seq);
            })
            .or_insert(tip.last_contiguous_seq);
    }
    tips_by_origin
}

fn freshness_query_summary_matches(
    local: Option<&MeshFreshnessQuerySummary>,
    peer: Option<&MeshFreshnessQuerySummary>,
) -> Option<bool> {
    match (local, peer) {
        (Some(local), Some(peer)) => Some(local.summary_hash == peer.summary_hash),
        _ => None,
    }
}

fn freshness_evidence_id(
    peer_alias: &str,
    origin_alias: &str,
    query_summary_hash: Option<&str>,
    local_seq: u64,
    peer_seq: u64,
) -> String {
    let query = query_summary_hash.unwrap_or("query_unspecified");
    format!(
        "freshness_{}",
        stable_hash_hex(
            &format!("{peer_alias}|{origin_alias}|{query}|{local_seq}|{peer_seq}"),
            16
        )
    )
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
