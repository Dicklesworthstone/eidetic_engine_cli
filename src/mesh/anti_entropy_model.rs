//! SRR6.25 executable model for mesh anti-entropy and stale-read semantics.
//!
//! This is deliberately not transport code. It is a small, pure model used to
//! pin the invariants ADR 0041 requires future SRR6 anti-entropy, replay, and
//! revision-notice implementations to preserve:
//!
//! - per-origin streams are append-only by `(origin_node_id, seq)`;
//! - duplicate and out-of-order delivery are idempotent;
//! - cursors advance only after durable, contiguous replay;
//! - Tier 1 reads return local state immediately and can later be revised;
//! - conflicting logical revisions remain visible instead of overwriting.

use std::collections::{BTreeMap, BTreeSet};

/// ADR that owns this model's assumptions.
pub const ANTI_ENTROPY_MODEL_ADR: &str = "docs/adr/0041-mesh-anti-entropy-model.md";

/// Stable scenario names logged by tests and referenced by ADR 0041.
pub const ANTI_ENTROPY_MODEL_SCENARIOS: &[&str] = &[
    "cursor_advances_only_after_contiguous_replay",
    "partition_rejoin_duplicate_out_of_order_delivery",
    "conflicting_revisions_are_visible",
    "stale_tier1_read_gets_revision_notice",
    "deterministic_replay_order_independent",
    "withdrawal_propagates_as_provenance_tombstone",
    "validity_expiry_filters_without_peer_cache_purge",
    "tombstone_hides_from_search_without_body_purge",
];

/// Stream position for one origin node.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct EventKey {
    pub origin_node_id: String,
    pub seq: u64,
}

impl EventKey {
    #[must_use]
    pub fn new(origin_node_id: impl Into<String>, seq: u64) -> Self {
        Self {
            origin_node_id: origin_node_id.into(),
            seq,
        }
    }
}

/// Mesh event kinds that affect deterministic replay visibility.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelEventKind {
    Create,
    Revise,
    Tombstone,
    Trust,
    Validity,
    BodyAvailable,
    ShareWithdraw,
}

impl ModelEventKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Revise => "revise",
            Self::Tombstone => "tombstone",
            Self::Trust => "trust",
            Self::Validity => "validity",
            Self::BodyAvailable => "bodyAvailable",
            Self::ShareWithdraw => "shareWithdraw",
        }
    }
}

/// Append-only mesh event facts relevant to the SRR6.25 model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelEvent {
    pub key: EventKey,
    pub event_id: String,
    pub kind: ModelEventKind,
    pub logical_memory_id: String,
    pub base_event_id: Option<String>,
    pub content_hash: String,
    pub valid_until_epoch_ms: Option<u64>,
}

impl ModelEvent {
    #[must_use]
    pub fn new(
        origin_node_id: impl Into<String>,
        seq: u64,
        logical_memory_id: impl Into<String>,
        base_event_id: Option<impl Into<String>>,
        content_hash: impl Into<String>,
    ) -> Self {
        Self::new_with_kind(
            origin_node_id,
            seq,
            logical_memory_id,
            base_event_id,
            content_hash,
            ModelEventKind::Create,
        )
    }

    #[must_use]
    pub fn new_with_kind(
        origin_node_id: impl Into<String>,
        seq: u64,
        logical_memory_id: impl Into<String>,
        base_event_id: Option<impl Into<String>>,
        content_hash: impl Into<String>,
        kind: ModelEventKind,
    ) -> Self {
        let origin_node_id = origin_node_id.into();
        let logical_memory_id = logical_memory_id.into();
        let content_hash = content_hash.into();
        let event_id = format!(
            "evt:{origin_node_id}:{seq}:{logical_memory_id}:{}:{content_hash}",
            kind.as_str()
        );
        Self {
            key: EventKey::new(origin_node_id, seq),
            event_id,
            kind,
            logical_memory_id,
            base_event_id: base_event_id.map(Into::into),
            content_hash,
            valid_until_epoch_ms: None,
        }
    }

    #[must_use]
    pub fn with_valid_until_epoch_ms(mut self, valid_until_epoch_ms: u64) -> Self {
        self.valid_until_epoch_ms = Some(valid_until_epoch_ms);
        self
    }
}

/// Effective visibility of one logical memory after replay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogicalVisibilityStatus {
    Active,
    Withdrawn,
    Tombstoned,
    Expired,
}

impl LogicalVisibilityStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Withdrawn => "withdrawn",
            Self::Tombstoned => "tombstoned",
            Self::Expired => "expired",
        }
    }
}

/// Context/search/why rendering contract for replayed mesh material.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogicalVisibility {
    pub logical_memory_id: String,
    pub status: LogicalVisibilityStatus,
    pub active_head_event_ids: Vec<String>,
    pub provenance_event_ids: Vec<String>,
    pub search_visible: bool,
    pub context_visible: bool,
    pub why_provenance_visible: bool,
    pub body_cache_purge_required: bool,
    pub residual_metadata_reason: Option<&'static str>,
}

/// Replay result for one delivered event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplayOutcome {
    Accepted,
    Duplicate,
    RejectedForkedStream {
        origin_node_id: String,
        seq: u64,
        existing_event_id: String,
        incoming_event_id: String,
    },
}

/// Missing contiguous range requested from a peer during anti-entropy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventRange {
    pub origin_node_id: String,
    pub start_seq: u64,
    pub end_seq: u64,
}

/// A logical memory has multiple visible heads. This is conflict evidence, not
/// a resolver that hides one branch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogicalConflict {
    pub logical_memory_id: String,
    pub head_event_ids: Vec<String>,
}

/// Snapshot returned by the local Tier 1 read path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tier1Read {
    pub frontier: BTreeMap<String, u64>,
    pub logical_heads: BTreeMap<String, Vec<String>>,
}

/// Explicit notice that a previously returned Tier 1 read is stale relative to
/// later durable replay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevisionNotice {
    pub advanced_origins: Vec<FrontierAdvance>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontierAdvance {
    pub origin_node_id: String,
    pub from_seq: u64,
    pub to_seq: u64,
}

/// Pure node-local model. `accepted` is the durable event set; `frontier` is
/// derived from contiguous accepted events and is what peers may safely use as
/// an import cursor.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelNode {
    accepted: BTreeMap<EventKey, ModelEvent>,
    frontier: BTreeMap<String, u64>,
}

impl ModelNode {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn accepted_event_ids(&self) -> Vec<String> {
        self.accepted
            .values()
            .map(|event| event.event_id.clone())
            .collect()
    }

    #[must_use]
    pub fn frontier(&self) -> BTreeMap<String, u64> {
        self.frontier.clone()
    }

    #[must_use]
    pub fn cursor_for(&self, origin_node_id: &str) -> u64 {
        self.frontier.get(origin_node_id).copied().unwrap_or(0)
    }

    pub fn replay(&mut self, event: ModelEvent) -> ReplayOutcome {
        match self.accepted.get(&event.key) {
            Some(existing) if existing.event_id == event.event_id => ReplayOutcome::Duplicate,
            Some(existing) => ReplayOutcome::RejectedForkedStream {
                origin_node_id: event.key.origin_node_id,
                seq: event.key.seq,
                existing_event_id: existing.event_id.clone(),
                incoming_event_id: event.event_id,
            },
            None => {
                let origin_node_id = event.key.origin_node_id.clone();
                self.accepted.insert(event.key.clone(), event);
                self.advance_frontier_for(&origin_node_id);
                ReplayOutcome::Accepted
            }
        }
    }

    /// Return the ranges this node should request from a peer that advertises
    /// `peer_frontier`. Each range starts after this node's durable cursor for
    /// that origin, so gaps cannot be skipped silently.
    #[must_use]
    pub fn ranges_to_request(&self, peer_frontier: &BTreeMap<String, u64>) -> Vec<EventRange> {
        let mut ranges = Vec::new();
        for (origin_node_id, peer_tip) in peer_frontier {
            let local_tip = self.cursor_for(origin_node_id);
            if *peer_tip > local_tip {
                ranges.push(EventRange {
                    origin_node_id: origin_node_id.clone(),
                    start_seq: local_tip + 1,
                    end_seq: *peer_tip,
                });
            }
        }
        ranges
    }

    #[must_use]
    pub fn events_for_range(&self, range: &EventRange) -> Vec<ModelEvent> {
        (range.start_seq..=range.end_seq)
            .filter_map(|seq| {
                self.accepted
                    .get(&EventKey::new(range.origin_node_id.clone(), seq))
                    .cloned()
            })
            .collect()
    }

    #[must_use]
    pub fn tier1_read(&self) -> Tier1Read {
        Tier1Read {
            frontier: self.frontier(),
            logical_heads: self.logical_heads(),
        }
    }

    #[must_use]
    pub fn revision_notice_since(&self, read: &Tier1Read) -> Option<RevisionNotice> {
        let mut advanced_origins = Vec::new();
        for (origin_node_id, to_seq) in &self.frontier {
            let from_seq = read.frontier.get(origin_node_id).copied().unwrap_or(0);
            if *to_seq > from_seq {
                advanced_origins.push(FrontierAdvance {
                    origin_node_id: origin_node_id.clone(),
                    from_seq,
                    to_seq: *to_seq,
                });
            }
        }

        if advanced_origins.is_empty() {
            None
        } else {
            Some(RevisionNotice { advanced_origins })
        }
    }

    #[must_use]
    pub fn logical_conflicts(&self) -> Vec<LogicalConflict> {
        self.logical_heads()
            .into_iter()
            .filter_map(|(logical_memory_id, head_event_ids)| {
                if head_event_ids.len() > 1 {
                    Some(LogicalConflict {
                        logical_memory_id,
                        head_event_ids,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    #[must_use]
    pub fn logical_visibility_at(&self, now_epoch_ms: u64) -> Vec<LogicalVisibility> {
        let mut events_by_logical: BTreeMap<String, Vec<&ModelEvent>> = BTreeMap::new();
        for event in self.accepted.values() {
            events_by_logical
                .entry(event.logical_memory_id.clone())
                .or_default()
                .push(event);
        }

        let heads = self.logical_heads();
        events_by_logical
            .into_iter()
            .map(|(logical_memory_id, mut events)| {
                events.sort_by(|left, right| {
                    left.key
                        .cmp(&right.key)
                        .then_with(|| left.event_id.cmp(&right.event_id))
                });
                let withdrawn = events
                    .iter()
                    .filter(|event| event.kind == ModelEventKind::ShareWithdraw)
                    .map(|event| event.event_id.clone())
                    .collect::<Vec<_>>();
                let tombstoned = events
                    .iter()
                    .filter(|event| event.kind == ModelEventKind::Tombstone)
                    .map(|event| event.event_id.clone())
                    .collect::<Vec<_>>();
                let expired = events
                    .iter()
                    .filter(|event| {
                        event
                            .valid_until_epoch_ms
                            .is_some_and(|valid_until| valid_until <= now_epoch_ms)
                    })
                    .map(|event| event.event_id.clone())
                    .collect::<Vec<_>>();

                if !withdrawn.is_empty() {
                    return LogicalVisibility {
                        logical_memory_id,
                        status: LogicalVisibilityStatus::Withdrawn,
                        active_head_event_ids: Vec::new(),
                        provenance_event_ids: withdrawn,
                        search_visible: false,
                        context_visible: false,
                        why_provenance_visible: true,
                        body_cache_purge_required: true,
                        residual_metadata_reason: Some(
                            "withdrawal_preserves_metadata_tombstone_and_requests_peer_cache_purge",
                        ),
                    };
                }
                if !tombstoned.is_empty() {
                    return LogicalVisibility {
                        logical_memory_id,
                        status: LogicalVisibilityStatus::Tombstoned,
                        active_head_event_ids: Vec::new(),
                        provenance_event_ids: tombstoned,
                        search_visible: false,
                        context_visible: false,
                        why_provenance_visible: true,
                        body_cache_purge_required: false,
                        residual_metadata_reason: Some(
                            "tombstone_preserves_provenance_without_peer_cache_purge",
                        ),
                    };
                }
                if !expired.is_empty() {
                    return LogicalVisibility {
                        logical_memory_id,
                        status: LogicalVisibilityStatus::Expired,
                        active_head_event_ids: Vec::new(),
                        provenance_event_ids: expired,
                        search_visible: false,
                        context_visible: false,
                        why_provenance_visible: true,
                        body_cache_purge_required: false,
                        residual_metadata_reason: Some(
                            "validity_expiry_filters_reads_without_peer_cache_purge",
                        ),
                    };
                }

                LogicalVisibility {
                    logical_memory_id: logical_memory_id.clone(),
                    status: LogicalVisibilityStatus::Active,
                    active_head_event_ids: heads
                        .get(&logical_memory_id)
                        .cloned()
                        .unwrap_or_default(),
                    provenance_event_ids: Vec::new(),
                    search_visible: true,
                    context_visible: true,
                    why_provenance_visible: true,
                    body_cache_purge_required: false,
                    residual_metadata_reason: None,
                }
            })
            .collect()
    }

    #[must_use]
    pub fn convergence_digest(&self) -> String {
        let events = self.accepted_event_ids().join(",");
        let frontier = self
            .frontier
            .iter()
            .map(|(origin, seq)| format!("{origin}:{seq}"))
            .collect::<Vec<_>>()
            .join(",");
        let conflicts = self
            .logical_conflicts()
            .into_iter()
            .map(|conflict| {
                format!(
                    "{}:{}",
                    conflict.logical_memory_id,
                    conflict.head_event_ids.join("+")
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!("events=[{events}];frontier=[{frontier}];conflicts=[{conflicts}]")
    }

    #[must_use]
    pub fn replay_deterministically(events: impl IntoIterator<Item = ModelEvent>) -> Self {
        let mut events = events.into_iter().collect::<Vec<_>>();
        events.sort_by(|left, right| {
            left.key
                .cmp(&right.key)
                .then_with(|| left.event_id.cmp(&right.event_id))
        });
        let mut node = Self::new();
        for event in events {
            let _ = node.replay(event);
        }
        node
    }

    fn advance_frontier_for(&mut self, origin_node_id: &str) {
        let mut next_seq = self.cursor_for(origin_node_id) + 1;
        while self
            .accepted
            .contains_key(&EventKey::new(origin_node_id.to_owned(), next_seq))
        {
            self.frontier.insert(origin_node_id.to_owned(), next_seq);
            next_seq += 1;
        }
    }

    fn logical_heads(&self) -> BTreeMap<String, Vec<String>> {
        let mut events_by_logical: BTreeMap<String, Vec<&ModelEvent>> = BTreeMap::new();
        let mut referenced_bases: BTreeSet<String> = BTreeSet::new();

        for event in self.accepted.values() {
            events_by_logical
                .entry(event.logical_memory_id.clone())
                .or_default()
                .push(event);
            if let Some(base_event_id) = &event.base_event_id {
                referenced_bases.insert(base_event_id.clone());
            }
        }

        events_by_logical
            .into_iter()
            .map(|(logical_memory_id, events)| {
                let mut heads = events
                    .into_iter()
                    .filter(|event| !referenced_bases.contains(&event.event_id))
                    .map(|event| event.event_id.clone())
                    .collect::<Vec<_>>();
                heads.sort();
                (logical_memory_id, heads)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ANTI_ENTROPY_MODEL_SCENARIOS, EventRange, LogicalVisibilityStatus, ModelEvent,
        ModelEventKind, ModelNode, ReplayOutcome,
    };

    fn event(
        origin: &str,
        seq: u64,
        logical_memory_id: &str,
        base_event_id: Option<&str>,
        content_hash: &str,
    ) -> ModelEvent {
        ModelEvent::new(origin, seq, logical_memory_id, base_event_id, content_hash)
    }

    fn event_kind(
        origin: &str,
        seq: u64,
        logical_memory_id: &str,
        base_event_id: Option<&str>,
        content_hash: &str,
        kind: ModelEventKind,
    ) -> ModelEvent {
        ModelEvent::new_with_kind(
            origin,
            seq,
            logical_memory_id,
            base_event_id,
            content_hash,
            kind,
        )
    }

    #[test]
    fn cursor_advances_only_after_contiguous_replay() {
        let scenario = "cursor_advances_only_after_contiguous_replay";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let mut node = ModelNode::new();
        let first = event("node_a", 1, "mem_release_rule", None, "hash_a1");
        let second = event(
            "node_a",
            2,
            "mem_release_rule",
            Some(first.event_id.as_str()),
            "hash_a2",
        );

        assert_eq!(node.replay(second.clone()), ReplayOutcome::Accepted);
        assert_eq!(
            node.cursor_for("node_a"),
            0,
            "{scenario}: cursor cannot skip missing seq=1"
        );

        assert_eq!(node.replay(first.clone()), ReplayOutcome::Accepted);
        assert_eq!(node.cursor_for("node_a"), 2);
        assert_eq!(node.replay(first), ReplayOutcome::Duplicate);
        assert_eq!(node.accepted_event_ids().len(), 2);
    }

    #[test]
    fn partition_rejoin_duplicate_out_of_order_delivery_converges() {
        let scenario = "partition_rejoin_duplicate_out_of_order_delivery";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let a1 = event("node_a", 1, "mem_release_rule", None, "hash_a1");
        let a2 = event(
            "node_a",
            2,
            "mem_release_rule",
            Some(a1.event_id.as_str()),
            "hash_a2",
        );
        let b1 = event("node_b", 1, "mem_review_rule", None, "hash_b1");

        let mut node_a = ModelNode::new();
        let mut node_b = ModelNode::new();
        assert_eq!(node_a.replay(a1.clone()), ReplayOutcome::Accepted);
        assert_eq!(node_a.replay(a2.clone()), ReplayOutcome::Accepted);
        assert_eq!(node_b.replay(b1.clone()), ReplayOutcome::Accepted);

        let ranges = node_b.ranges_to_request(&node_a.frontier());
        assert_eq!(
            ranges,
            vec![EventRange {
                origin_node_id: "node_a".to_owned(),
                start_seq: 1,
                end_seq: 2
            }]
        );

        assert_eq!(node_b.replay(a2.clone()), ReplayOutcome::Accepted);
        assert_eq!(node_b.cursor_for("node_a"), 0);
        assert_eq!(node_b.replay(a1.clone()), ReplayOutcome::Accepted);
        assert_eq!(node_b.cursor_for("node_a"), 2);
        assert_eq!(node_b.replay(a1.clone()), ReplayOutcome::Duplicate);

        let oracle = ModelNode::replay_deterministically([a1, a2, b1]);
        assert_eq!(
            node_b.convergence_digest(),
            oracle.convergence_digest(),
            "{scenario}: fixed accepted event set must converge"
        );
    }

    #[test]
    fn conflicting_revisions_are_visible_not_overwritten() {
        let scenario = "conflicting_revisions_are_visible";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let left = event("node_a", 1, "mem_shared_policy", None, "hash_left");
        let right = event("node_b", 1, "mem_shared_policy", None, "hash_right");
        let mut node = ModelNode::new();

        assert_eq!(node.replay(left.clone()), ReplayOutcome::Accepted);
        assert_eq!(node.replay(right.clone()), ReplayOutcome::Accepted);

        let conflicts = node.logical_conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].logical_memory_id, "mem_shared_policy");
        assert_eq!(
            conflicts[0].head_event_ids,
            vec![left.event_id.clone(), right.event_id.clone()]
        );
        assert_eq!(
            node.tier1_read().logical_heads["mem_shared_policy"],
            vec![left.event_id, right.event_id],
            "{scenario}: both heads remain visible to callers"
        );
    }

    #[test]
    fn stale_tier1_read_gets_revision_notice_after_import() {
        let scenario = "stale_tier1_read_gets_revision_notice";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let first = event("node_a", 1, "mem_release_rule", None, "hash_a1");
        let second = event(
            "node_a",
            2,
            "mem_release_rule",
            Some(first.event_id.as_str()),
            "hash_a2",
        );
        let mut node = ModelNode::new();
        assert_eq!(node.replay(first), ReplayOutcome::Accepted);

        let read = node.tier1_read();
        assert_eq!(read.frontier["node_a"], 1);
        assert_eq!(node.revision_notice_since(&read), None);

        assert_eq!(node.replay(second), ReplayOutcome::Accepted);
        let notice = node
            .revision_notice_since(&read)
            .expect("revision notice after newer durable replay");
        assert_eq!(notice.advanced_origins.len(), 1);
        assert_eq!(notice.advanced_origins[0].origin_node_id, "node_a");
        assert_eq!(notice.advanced_origins[0].from_seq, 1);
        assert_eq!(notice.advanced_origins[0].to_seq, 2);
        assert_eq!(
            read.frontier["node_a"], 1,
            "{scenario}: returned Tier 1 read is not mutated silently"
        );
    }

    #[test]
    fn deterministic_replay_is_order_independent() {
        let scenario = "deterministic_replay_order_independent";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let a1 = event("node_a", 1, "mem_release_rule", None, "hash_a1");
        let a2 = event(
            "node_a",
            2,
            "mem_release_rule",
            Some(a1.event_id.as_str()),
            "hash_a2",
        );
        let b1 = event("node_b", 1, "mem_shared_policy", None, "hash_b1");
        let c1 = event("node_c", 1, "mem_shared_policy", None, "hash_c1");

        let canonical =
            ModelNode::replay_deterministically([a1.clone(), a2.clone(), b1.clone(), c1.clone()]);
        let shuffled = ModelNode::replay_deterministically([c1, a2, b1, a1]);

        assert_eq!(
            canonical.convergence_digest(),
            shuffled.convergence_digest()
        );
        assert_eq!(canonical.logical_conflicts().len(), 1);
    }

    #[test]
    fn withdrawal_propagates_as_provenance_tombstone() {
        let scenario = "withdrawal_propagates_as_provenance_tombstone";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let create = event("node_a", 1, "mem_shared_body", None, "hash_body");
        let withdraw = event_kind(
            "node_a",
            2,
            "mem_shared_body",
            Some(create.event_id.as_str()),
            "hash_withdraw",
            ModelEventKind::ShareWithdraw,
        );
        let node = ModelNode::replay_deterministically([withdraw.clone(), create]);

        let visibility = node.logical_visibility_at(10_000);
        assert_eq!(visibility.len(), 1);
        let decision = &visibility[0];
        assert_eq!(decision.status, LogicalVisibilityStatus::Withdrawn);
        assert_eq!(decision.status.as_str(), "withdrawn");
        assert!(decision.active_head_event_ids.is_empty());
        assert_eq!(decision.provenance_event_ids, vec![withdraw.event_id]);
        assert!(!decision.search_visible);
        assert!(!decision.context_visible);
        assert!(decision.why_provenance_visible);
        assert!(decision.body_cache_purge_required);
        assert_eq!(
            decision.residual_metadata_reason,
            Some("withdrawal_preserves_metadata_tombstone_and_requests_peer_cache_purge")
        );
    }

    #[test]
    fn validity_expiry_filters_without_peer_cache_purge() {
        let scenario = "validity_expiry_filters_without_peer_cache_purge";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let validity = event_kind(
            "node_a",
            1,
            "mem_timeboxed",
            None,
            "hash_validity",
            ModelEventKind::Validity,
        )
        .with_valid_until_epoch_ms(5_000);
        let node = ModelNode::replay_deterministically([validity.clone()]);

        let active = node.logical_visibility_at(4_999);
        assert_eq!(active[0].status, LogicalVisibilityStatus::Active);
        assert!(active[0].search_visible);
        assert!(!active[0].body_cache_purge_required);

        let expired = node.logical_visibility_at(5_000);
        assert_eq!(expired[0].status, LogicalVisibilityStatus::Expired);
        assert_eq!(expired[0].provenance_event_ids, vec![validity.event_id]);
        assert!(!expired[0].search_visible);
        assert!(!expired[0].context_visible);
        assert!(expired[0].why_provenance_visible);
        assert!(!expired[0].body_cache_purge_required);
        assert_eq!(
            expired[0].residual_metadata_reason,
            Some("validity_expiry_filters_reads_without_peer_cache_purge")
        );
    }

    #[test]
    fn tombstone_hides_from_search_without_body_purge() {
        let scenario = "tombstone_hides_from_search_without_body_purge";
        assert!(ANTI_ENTROPY_MODEL_SCENARIOS.contains(&scenario));

        let create = event("node_a", 1, "mem_tombstoned", None, "hash_body");
        let tombstone = event_kind(
            "node_a",
            2,
            "mem_tombstoned",
            Some(create.event_id.as_str()),
            "hash_tombstone",
            ModelEventKind::Tombstone,
        );
        let node = ModelNode::replay_deterministically([create, tombstone.clone()]);

        let visibility = node.logical_visibility_at(10_000);
        assert_eq!(visibility[0].status, LogicalVisibilityStatus::Tombstoned);
        assert_eq!(visibility[0].provenance_event_ids, vec![tombstone.event_id]);
        assert!(!visibility[0].search_visible);
        assert!(!visibility[0].context_visible);
        assert!(visibility[0].why_provenance_visible);
        assert!(!visibility[0].body_cache_purge_required);
        assert_eq!(
            visibility[0].residual_metadata_reason,
            Some("tombstone_preserves_provenance_without_peer_cache_purge")
        );
    }
}
