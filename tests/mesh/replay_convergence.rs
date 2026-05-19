use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

type TestResult = Result<(), String>;

const SURFACE: &str = "mesh_replay_convergence";

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct EventKey {
    origin_node_id: String,
    seq: u64,
}

impl EventKey {
    fn new(origin_node_id: &str, seq: u64) -> Self {
        Self {
            origin_node_id: origin_node_id.to_owned(),
            seq,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EventKind {
    Create,
    Revise,
    Tombstone,
    Validity,
}

impl EventKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Revise => "revise",
            Self::Tombstone => "tombstone",
            Self::Validity => "validity",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MeshReplayEvent {
    key: EventKey,
    event_kind: EventKind,
    logical_memory_id: String,
    base_event_hash: Option<String>,
    content_hash: String,
    valid_from_seq: u64,
    valid_to_seq: Option<u64>,
    event_hash: String,
}

impl MeshReplayEvent {
    fn new(
        origin_node_id: &str,
        seq: u64,
        event_kind: EventKind,
        logical_memory_id: &str,
        base_event_hash: Option<&str>,
        content_hash: &str,
    ) -> Self {
        Self::with_validity(
            origin_node_id,
            seq,
            event_kind,
            logical_memory_id,
            base_event_hash,
            content_hash,
            0,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn with_validity(
        origin_node_id: &str,
        seq: u64,
        event_kind: EventKind,
        logical_memory_id: &str,
        base_event_hash: Option<&str>,
        content_hash: &str,
        valid_from_seq: u64,
        valid_to_seq: Option<u64>,
    ) -> Self {
        let key = EventKey::new(origin_node_id, seq);
        let base_event_hash = base_event_hash.map(str::to_owned);
        let event_hash = event_hash_for(
            &key,
            event_kind,
            logical_memory_id,
            base_event_hash.as_deref(),
            content_hash,
            valid_from_seq,
            valid_to_seq,
        );
        Self {
            key,
            event_kind,
            logical_memory_id: logical_memory_id.to_owned(),
            base_event_hash,
            content_hash: content_hash.to_owned(),
            valid_from_seq,
            valid_to_seq,
            event_hash,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ReplayOutcome {
    Accepted,
    Duplicate,
    RejectedForkedStream,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EventRange {
    origin_node_id: String,
    start_seq: u64,
    end_seq: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RangeSummary {
    origin_node_id: String,
    start_seq: u64,
    end_seq: u64,
    event_count: usize,
    event_hashes: Vec<String>,
    summary_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MemoryProjection {
    logical_memory_id: String,
    visible_heads: Vec<String>,
    contradiction_evidence: Vec<String>,
    status: ProjectionStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProjectionStatus {
    Active,
    Conflict,
    Tombstoned,
    Expired,
}

impl ProjectionStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Conflict => "conflict",
            Self::Tombstoned => "tombstoned",
            Self::Expired => "expired",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct IndexRow {
    logical_memory_id: String,
    status: ProjectionStatus,
    head_hashes: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NodeOutputs {
    db_projection: Vec<MemoryProjection>,
    index_projection: Vec<IndexRow>,
    search_results: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReplayNode {
    node_id: String,
    durable_events: BTreeMap<EventKey, MeshReplayEvent>,
    frontier: BTreeMap<String, u64>,
}

impl ReplayNode {
    fn new(node_id: &str) -> Self {
        Self {
            node_id: node_id.to_owned(),
            durable_events: BTreeMap::new(),
            frontier: BTreeMap::new(),
        }
    }

    fn from_durable_log(node_id: &str, events: impl IntoIterator<Item = MeshReplayEvent>) -> Self {
        let mut node = Self::new(node_id);
        let mut events = events.into_iter().collect::<Vec<_>>();
        events.sort_by(|left, right| {
            left.key
                .cmp(&right.key)
                .then_with(|| left.event_hash.cmp(&right.event_hash))
        });
        for event in events {
            let _ = node.replay(event);
        }
        node
    }

    fn replay(&mut self, event: MeshReplayEvent) -> ReplayOutcome {
        match self.durable_events.get(&event.key) {
            Some(existing) if existing.event_hash == event.event_hash => ReplayOutcome::Duplicate,
            Some(_existing) => ReplayOutcome::RejectedForkedStream,
            None => {
                let origin_node_id = event.key.origin_node_id.clone();
                self.durable_events.insert(event.key.clone(), event);
                self.advance_frontier_for(&origin_node_id);
                ReplayOutcome::Accepted
            }
        }
    }

    fn replay_batch(&mut self, events: impl IntoIterator<Item = MeshReplayEvent>) {
        for event in events {
            let _ = self.replay(event);
        }
    }

    fn cursor_for(&self, origin_node_id: &str) -> u64 {
        self.frontier.get(origin_node_id).copied().unwrap_or(0)
    }

    fn ranges_to_request(&self, peer_frontier: &BTreeMap<String, u64>) -> Vec<EventRange> {
        peer_frontier
            .iter()
            .filter_map(|(origin_node_id, peer_tip)| {
                let local_tip = self.cursor_for(origin_node_id);
                (*peer_tip > local_tip).then(|| EventRange {
                    origin_node_id: origin_node_id.clone(),
                    start_seq: local_tip + 1,
                    end_seq: *peer_tip,
                })
            })
            .collect()
    }

    fn events_for_range(&self, range: &EventRange) -> Vec<MeshReplayEvent> {
        (range.start_seq..=range.end_seq)
            .filter_map(|seq| {
                self.durable_events
                    .get(&EventKey::new(&range.origin_node_id, seq))
                    .cloned()
            })
            .collect()
    }

    fn sync_from(&mut self, peer: &Self, reverse_delivery: bool) {
        for range in self.ranges_to_request(&peer.frontier) {
            let mut events = peer.events_for_range(&range);
            if reverse_delivery {
                events.reverse();
            }
            self.replay_batch(events);
        }
    }

    fn durable_log(&self) -> Vec<MeshReplayEvent> {
        self.durable_events.values().cloned().collect()
    }

    fn convergence_digest(&self) -> String {
        let event_hashes = self
            .durable_events
            .values()
            .map(|event| event.event_hash.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let frontier = self
            .frontier
            .iter()
            .map(|(origin, seq)| format!("{origin}:{seq}"))
            .collect::<Vec<_>>()
            .join(",");
        let index = self
            .index_projection(50)
            .into_iter()
            .map(|row| {
                format!(
                    "{}:{}:{}",
                    row.logical_memory_id,
                    row.status.as_str(),
                    row.head_hashes.join("+")
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "node={};events=[{event_hashes}];frontier=[{frontier}];index=[{index}]",
            self.node_id
        )
    }

    fn db_projection(&self, as_of_seq: u64) -> Vec<MemoryProjection> {
        let mut by_memory: BTreeMap<String, Vec<&MeshReplayEvent>> = BTreeMap::new();
        let mut referenced_bases = BTreeSet::new();
        for event in self.durable_events.values() {
            by_memory
                .entry(event.logical_memory_id.clone())
                .or_default()
                .push(event);
            if let Some(base_event_hash) = &event.base_event_hash {
                referenced_bases.insert(base_event_hash.clone());
            }
        }

        by_memory
            .into_iter()
            .map(|(logical_memory_id, events)| {
                let tombstoned = events
                    .iter()
                    .any(|event| event.event_kind == EventKind::Tombstone);
                let expired = events
                    .iter()
                    .filter_map(|event| event.valid_to_seq)
                    .min()
                    .is_some_and(|valid_to_seq| as_of_seq > valid_to_seq);
                let mut visible_heads = events
                    .iter()
                    .filter(|event| !referenced_bases.contains(&event.event_hash))
                    .map(|event| event.event_hash.clone())
                    .collect::<Vec<_>>();
                visible_heads.sort();
                let status = if tombstoned {
                    ProjectionStatus::Tombstoned
                } else if expired {
                    ProjectionStatus::Expired
                } else if visible_heads.len() > 1 {
                    ProjectionStatus::Conflict
                } else {
                    ProjectionStatus::Active
                };
                let contradiction_evidence = if status == ProjectionStatus::Conflict {
                    visible_heads.clone()
                } else {
                    Vec::new()
                };
                MemoryProjection {
                    logical_memory_id,
                    visible_heads,
                    contradiction_evidence,
                    status,
                }
            })
            .collect()
    }

    fn index_projection(&self, as_of_seq: u64) -> Vec<IndexRow> {
        self.db_projection(as_of_seq)
            .into_iter()
            .map(|projection| IndexRow {
                logical_memory_id: projection.logical_memory_id,
                status: projection.status,
                head_hashes: projection.visible_heads,
            })
            .collect()
    }

    fn search_results(&self, query: &str, as_of_seq: u64) -> Vec<String> {
        self.index_projection(as_of_seq)
            .into_iter()
            .filter(|row| {
                row.logical_memory_id.contains(query)
                    && matches!(
                        row.status,
                        ProjectionStatus::Active | ProjectionStatus::Conflict
                    )
            })
            .map(|row| format!("{}:{}", row.logical_memory_id, row.status.as_str()))
            .collect()
    }

    fn outputs(&self, query: &str, as_of_seq: u64) -> NodeOutputs {
        NodeOutputs {
            db_projection: self.db_projection(as_of_seq),
            index_projection: self.index_projection(as_of_seq),
            search_results: self.search_results(query, as_of_seq),
        }
    }

    fn advance_frontier_for(&mut self, origin_node_id: &str) {
        let mut next_seq = self.cursor_for(origin_node_id) + 1;
        while self
            .durable_events
            .contains_key(&EventKey::new(origin_node_id, next_seq))
        {
            self.frontier.insert(origin_node_id.to_owned(), next_seq);
            next_seq += 1;
        }
    }
}

fn event_hash_for(
    key: &EventKey,
    event_kind: EventKind,
    logical_memory_id: &str,
    base_event_hash: Option<&str>,
    content_hash: &str,
    valid_from_seq: u64,
    valid_to_seq: Option<u64>,
) -> String {
    let canonical = format!(
        "schema=ee.mesh.replay_event.test.v1\norigin={}\nseq={}\nkind={}\nlogical={}\nbase={}\ncontent={}\nvalid_from={}\nvalid_to={}\n",
        key.origin_node_id,
        key.seq,
        event_kind.as_str(),
        logical_memory_id,
        base_event_hash.unwrap_or(""),
        content_hash,
        valid_from_seq,
        valid_to_seq.map_or_else(String::new, |seq| seq.to_string())
    );
    format!("blake3:{}", blake3::hash(canonical.as_bytes()).to_hex())
}

fn range_summary(
    origin_node_id: &str,
    start_seq: u64,
    end_seq: u64,
    events: &[MeshReplayEvent],
) -> RangeSummary {
    let mut event_hashes = events
        .iter()
        .filter(|event| {
            event.key.origin_node_id == origin_node_id
                && event.key.seq >= start_seq
                && event.key.seq <= end_seq
        })
        .map(|event| event.event_hash.clone())
        .collect::<Vec<_>>();
    event_hashes.sort();
    let canonical = format!(
        "{origin_node_id}:{start_seq}:{end_seq}:{}",
        event_hashes.join(",")
    );
    RangeSummary {
        origin_node_id: origin_node_id.to_owned(),
        start_seq,
        end_seq,
        event_count: event_hashes.len(),
        event_hashes,
        summary_hash: format!("blake3:{}", blake3::hash(canonical.as_bytes()).to_hex()),
    }
}

fn mesh_event(scenario: &str, phase: &str, node_id: &str, message: &str) {
    println!(
        "{}",
        json!({
            "schema": "ee.test_event.v1",
            "surface": SURFACE,
            "scenario": scenario,
            "phase": phase,
            "meshNode": node_id,
            "message": message,
        })
    );
}

fn assert_equal<T: Eq + std::fmt::Debug>(actual: T, expected: T, label: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{label}: expected {expected:?}, got {actual:?}"))
    }
}

#[test]
fn event_hash_and_range_summary_are_deterministic() -> TestResult {
    let scenario = "event_hash_and_range_summary_are_deterministic";
    let first = MeshReplayEvent::new(
        "node01",
        1,
        EventKind::Create,
        "mem_rule_alpha",
        None,
        "hash_a1",
    );
    let second = MeshReplayEvent::new(
        "node01",
        2,
        EventKind::Revise,
        "mem_rule_alpha",
        Some(first.event_hash.as_str()),
        "hash_a2",
    );
    let repeated = MeshReplayEvent::new(
        "node01",
        1,
        EventKind::Create,
        "mem_rule_alpha",
        None,
        "hash_a1",
    );

    assert_equal(
        repeated.event_hash,
        first.event_hash.clone(),
        "stable event hash",
    )?;
    let shuffled = vec![second.clone(), first.clone()];
    let ordered = vec![first.clone(), second.clone()];
    let left = range_summary("node01", 1, 2, &shuffled);
    let right = range_summary("node01", 1, 2, &ordered);

    assert_equal(left, right, "range summary order independence")?;
    let summary = range_summary("node01", 1, 2, &ordered);
    assert_equal(summary.event_count, 2, "range event count")?;
    assert!(summary.summary_hash.starts_with("blake3:"));
    mesh_event(
        scenario,
        "assert",
        "node01",
        "event_hash_and_range_summary_stable",
    );
    Ok(())
}

#[test]
fn missed_ranges_and_out_of_order_batches_do_not_advance_cursor_past_gaps() -> TestResult {
    let scenario = "missed_ranges_and_out_of_order_batches";
    let first = MeshReplayEvent::new(
        "node01",
        1,
        EventKind::Create,
        "mem_rule_alpha",
        None,
        "hash_a1",
    );
    let second = MeshReplayEvent::new(
        "node01",
        2,
        EventKind::Revise,
        "mem_rule_alpha",
        Some(first.event_hash.as_str()),
        "hash_a2",
    );
    let third = MeshReplayEvent::new(
        "node01",
        3,
        EventKind::Revise,
        "mem_rule_alpha",
        Some(second.event_hash.as_str()),
        "hash_a3",
    );
    let mut peer = ReplayNode::new("node01");
    assert_equal(
        peer.replay(first.clone()),
        ReplayOutcome::Accepted,
        "peer seq1",
    )?;
    assert_equal(
        peer.replay(third.clone()),
        ReplayOutcome::Accepted,
        "peer seq3",
    )?;
    assert_equal(
        peer.cursor_for("node01"),
        1,
        "peer cursor stops at missing seq2",
    )?;

    let mut receiver = ReplayNode::new("node02");
    assert_equal(
        receiver.replay(third.clone()),
        ReplayOutcome::Accepted,
        "receiver out-of-order seq3",
    )?;
    assert_equal(
        receiver.cursor_for("node01"),
        0,
        "receiver cursor cannot skip seq1",
    )?;
    receiver.sync_from(&peer, false);
    assert_equal(
        receiver.cursor_for("node01"),
        1,
        "receiver reaches seq1 only",
    )?;
    assert_equal(
        receiver.replay(third),
        ReplayOutcome::Duplicate,
        "duplicate seq3",
    )?;

    assert_equal(
        peer.replay(second.clone()),
        ReplayOutcome::Accepted,
        "peer fills seq2",
    )?;
    assert_equal(
        peer.cursor_for("node01"),
        3,
        "peer cursor advances through seq3",
    )?;
    receiver.sync_from(&peer, true);
    assert_equal(
        receiver.cursor_for("node01"),
        3,
        "receiver advances after durable seq2",
    )?;
    mesh_event(
        scenario,
        "assert",
        "node02",
        "cursor_advanced_only_after_contiguous_replay",
    );
    Ok(())
}

#[test]
fn partition_then_rejoin_converges_db_index_and_search_outputs() -> TestResult {
    let scenario = "partition_then_rejoin_converges";
    let a1 = MeshReplayEvent::new(
        "node01",
        1,
        EventKind::Create,
        "mem_release_rule",
        None,
        "hash_a1",
    );
    let a2 = MeshReplayEvent::new(
        "node01",
        2,
        EventKind::Revise,
        "mem_release_rule",
        Some(a1.event_hash.as_str()),
        "hash_a2",
    );
    let b1 = MeshReplayEvent::new(
        "node02",
        1,
        EventKind::Create,
        "mem_review_rule",
        None,
        "hash_b1",
    );
    let b2 = MeshReplayEvent::new(
        "node02",
        2,
        EventKind::Revise,
        "mem_review_rule",
        Some(b1.event_hash.as_str()),
        "hash_b2",
    );

    let mut node_a = ReplayNode::from_durable_log("node01", [a1.clone(), a2.clone()]);
    let mut node_b = ReplayNode::from_durable_log("node02", [b1.clone(), b2.clone()]);
    mesh_event(
        scenario,
        "action",
        "node01",
        "partitioned_local_events_recorded",
    );
    mesh_event(
        scenario,
        "action",
        "node02",
        "partitioned_local_events_recorded",
    );

    node_a.sync_from(&node_b, true);
    node_b.sync_from(&node_a, true);
    node_a.sync_from(&node_b, false);
    node_b.sync_from(&node_a, false);
    let oracle = ReplayNode::from_durable_log("oracle", [a1, a2, b1, b2]);

    assert_equal(
        node_a.outputs("rule", 50),
        oracle.outputs("rule", 50),
        "node_a converged outputs",
    )?;
    assert_equal(
        node_b.outputs("rule", 50),
        oracle.outputs("rule", 50),
        "node_b converged outputs",
    )?;
    assert_equal(
        node_a.convergence_digest().replace("node=node01;", ""),
        node_b.convergence_digest().replace("node=node02;", ""),
        "node digests match after rejoin",
    )?;
    mesh_event(
        scenario,
        "assert",
        "scenario",
        "db_index_search_outputs_match_oracle",
    );
    Ok(())
}

#[test]
fn conflicting_revisions_remain_explicit_contradiction_evidence() -> TestResult {
    let scenario = "conflicting_revisions_are_explicit";
    let left = MeshReplayEvent::new(
        "node01",
        1,
        EventKind::Create,
        "mem_shared_policy",
        None,
        "hash_left",
    );
    let right = MeshReplayEvent::new(
        "node02",
        1,
        EventKind::Create,
        "mem_shared_policy",
        None,
        "hash_right",
    );
    let node = ReplayNode::from_durable_log("node03", [left.clone(), right.clone()]);
    let projection = node.db_projection(50);

    assert_equal(projection.len(), 1, "one logical projection")?;
    assert_equal(
        projection[0].status,
        ProjectionStatus::Conflict,
        "conflict status",
    )?;
    assert_equal(
        projection[0].contradiction_evidence.clone(),
        vec![left.event_hash, right.event_hash],
        "contradiction evidence keeps both heads",
    )?;
    assert_equal(
        node.search_results("shared", 50),
        vec!["mem_shared_policy:conflict".to_owned()],
        "search returns conflict-labelled row",
    )?;
    mesh_event(
        scenario,
        "assert",
        "node03",
        "conflict_heads_visible_not_overwritten",
    );
    Ok(())
}

#[test]
fn tombstone_and_validity_propagate_to_converged_search_projection() -> TestResult {
    let scenario = "tombstone_and_validity_propagate";
    let create_deleted = MeshReplayEvent::new(
        "node01",
        1,
        EventKind::Create,
        "mem_deleted_rule",
        None,
        "hash_d1",
    );
    let tombstone = MeshReplayEvent::new(
        "node01",
        2,
        EventKind::Tombstone,
        "mem_deleted_rule",
        Some(create_deleted.event_hash.as_str()),
        "hash_d2",
    );
    let create_short = MeshReplayEvent::new(
        "node02",
        1,
        EventKind::Create,
        "mem_short_rule",
        None,
        "hash_s1",
    );
    let validity = MeshReplayEvent::with_validity(
        "node02",
        2,
        EventKind::Validity,
        "mem_short_rule",
        Some(create_short.event_hash.as_str()),
        "hash_s2",
        0,
        Some(25),
    );
    let mut node_a =
        ReplayNode::from_durable_log("node01", [create_deleted.clone(), tombstone.clone()]);
    let mut node_b =
        ReplayNode::from_durable_log("node02", [create_short.clone(), validity.clone()]);
    node_a.sync_from(&node_b, true);
    node_b.sync_from(&node_a, true);

    assert_equal(
        node_a.outputs("rule", 50),
        node_b.outputs("rule", 50),
        "converged expired outputs",
    )?;
    assert_equal(
        node_a.search_results("rule", 50),
        Vec::<String>::new(),
        "expired/tombstoned excluded",
    )?;
    assert_equal(
        node_a.search_results("rule", 20),
        vec!["mem_short_rule:active".to_owned()],
        "valid memory remains searchable before valid_to",
    )?;
    mesh_event(
        scenario,
        "assert",
        "scenario",
        "tombstone_validity_search_projection_converged",
    );
    Ok(())
}

#[test]
fn peer_restart_rehydrates_durable_log_without_cursor_regression() -> TestResult {
    let scenario = "peer_restart_rehydrates_durable_log";
    let first = MeshReplayEvent::new(
        "node01",
        1,
        EventKind::Create,
        "mem_release_rule",
        None,
        "hash_a1",
    );
    let second = MeshReplayEvent::new(
        "node01",
        2,
        EventKind::Revise,
        "mem_release_rule",
        Some(first.event_hash.as_str()),
        "hash_a2",
    );
    let peer_event = MeshReplayEvent::new(
        "node02",
        1,
        EventKind::Create,
        "mem_review_rule",
        None,
        "hash_b1",
    );
    let node = ReplayNode::from_durable_log(
        "node01",
        [first.clone(), second.clone(), peer_event.clone()],
    );
    let restarted = ReplayNode::from_durable_log("node01", node.durable_log());

    assert_equal(restarted.frontier, node.frontier, "restart keeps frontiers")?;
    assert_equal(
        restarted.outputs("rule", 50),
        node.outputs("rule", 50),
        "restart keeps projections",
    )?;
    let mut restarted_again = restarted.clone();
    assert_equal(
        restarted_again.replay(second),
        ReplayOutcome::Duplicate,
        "post-restart duplicate event is idempotent",
    )?;
    mesh_event(
        scenario,
        "assert",
        "node01",
        "restart_rehydrated_durable_log",
    );
    Ok(())
}
