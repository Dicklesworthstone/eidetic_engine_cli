#![no_main]

//! Fuzz target for
//! `ee::graph::bipartite_provenance::compute_rule_provenance_ego`.
//!
//! Constructs an arbitrary bipartite rule-memory graph from
//! the input bytes and drives it through the ego-subgraph helper
//! shipped in commit `fe9310b`. Asserts every invariant the
//! function promises:
//!
//! 1. **Panic-freedom.** No graph topology may cause a panic,
//!    including center-rule-absent, center-rule-is-actually-a-
//!    memory, isolated rule, empty graph, and dense fan-out cases.
//! 2. **Status correctness.** When the center id is not a node,
//!    `status = RuleNotFound`. When it is a memory node,
//!    `status = NotARuleNode`. Otherwise `status = Available`.
//! 3. **Deterministic ordering.** `cited_memories` is sorted by
//!    `memory_id`; `co_citing_rules` is sorted by `rule_id`;
//!    `shared_memory_ids` inside each co-citing peer is sorted
//!    and deduped.
//! 4. **`other_rule_count` agreement.** For each cited memory
//!    surfaced by the ego, `other_rule_count` must equal the
//!    number of rules in the input graph that also cite that
//!    memory, excluding the center rule.
//! 5. **Center rule never echoes itself.** No `co_citing_rules`
//!    entry has `rule_id == center_rule_id`.
//! 6. **Determinism.** Two calls on the same graph produce
//!    byte-identical JSON serialization (J7 contract).
//!
//! Inputs are interpreted as 3-byte edge tuples. The first byte
//! picks the center rule by index; subsequent triples each add
//! one (rule_idx, memory_idx) edge. A small fixed table of rule
//! and memory ids keeps the corpus structurally sensible while
//! still exploring every degenerate topology.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use libfuzzer_sys::fuzz_target;

use ee::graph::bipartite_provenance::{
    BIPARTITE_PARTITION_ATTR, BIPARTITE_PARTITION_MEMORY, BIPARTITE_PARTITION_RULE,
    RULE_PROVENANCE_EGO_SCHEMA_V1, RuleProvenanceEgoStatus, compute_rule_provenance_ego,
};
use ee::graph::{AttrMap, Graph};
use fnx_runtime::{CgseValue, CompatibilityMode};

const MAX_INPUT_BYTES: usize = 4 * 1024;
const MAX_EDGES: usize = 128;

const RULE_IDS: &[&str] = &[
    "rule_a",
    "rule_b",
    "rule_c",
    "rule_d",
    "rule_e",
    "rule_main",
    "rule_peer",
    "rule_lonely",
];

const MEMORY_IDS: &[&str] = &[
    "mem_alpha",
    "mem_beta",
    "mem_gamma",
    "mem_delta",
    "mem_epsilon",
    "mem_shared",
    "mem_solo",
    "mem_orphan",
];

/// Center options: a rule from the table, a memory (exercising
/// `NotARuleNode`), and a non-node string (exercising
/// `RuleNotFound`). One byte from the input chooses among them.
fn pick_center(byte: u8) -> &'static str {
    let total = RULE_IDS.len() + MEMORY_IDS.len() + 1;
    let idx = byte as usize % total;
    if idx < RULE_IDS.len() {
        RULE_IDS[idx]
    } else if idx < RULE_IDS.len() + MEMORY_IDS.len() {
        MEMORY_IDS[idx - RULE_IDS.len()]
    } else {
        "rule_never_present_in_graph"
    }
}

fn partition_attrs(partition: &str) -> AttrMap {
    let mut attrs = AttrMap::new();
    attrs.insert(
        BIPARTITE_PARTITION_ATTR.to_owned(),
        CgseValue::String(partition.to_owned()),
    );
    attrs
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    if data.is_empty() {
        return;
    }

    let center = pick_center(data[0]);
    let edges: Vec<(&'static str, &'static str)> = data[1..]
        .chunks_exact(2)
        .take(MAX_EDGES)
        .map(|chunk| {
            (
                RULE_IDS[chunk[0] as usize % RULE_IDS.len()],
                MEMORY_IDS[chunk[1] as usize % MEMORY_IDS.len()],
            )
        })
        .collect();

    let mut graph = Graph::new(CompatibilityMode::Strict);

    // Track which nodes we have already inserted so we don't
    // double-add (Graph::new + Strict refuses duplicate inserts
    // in some implementations; either way the partition tag must
    // be applied once).
    let mut rule_nodes: BTreeSet<&'static str> = BTreeSet::new();
    let mut memory_nodes: BTreeSet<&'static str> = BTreeSet::new();
    for (rule, memory) in &edges {
        if rule_nodes.insert(rule) {
            graph.add_node_with_attrs(*rule, partition_attrs(BIPARTITE_PARTITION_RULE));
        }
        if memory_nodes.insert(memory) {
            graph.add_node_with_attrs(*memory, partition_attrs(BIPARTITE_PARTITION_MEMORY));
        }
    }

    // Add edges; drop any that fail (e.g. duplicate insertion in
    // strict mode) so the fuzzer can keep exploring without
    // bailing on every duplicate.
    let mut declared_edges: BTreeSet<(&'static str, &'static str)> = BTreeSet::new();
    for (rule, memory) in &edges {
        if declared_edges.insert((*rule, *memory)) {
            let _ = graph.add_edge_with_attrs(*rule, *memory, AttrMap::new());
        }
    }

    let ego = compute_rule_provenance_ego(&graph, center);

    // Invariant 1: panic-freedom — implicit if we got here.

    // Invariant 2: status correctness.
    let expected_status = if rule_nodes.contains(center) {
        RuleProvenanceEgoStatus::Available
    } else if memory_nodes.contains(center) {
        RuleProvenanceEgoStatus::NotARuleNode
    } else {
        RuleProvenanceEgoStatus::RuleNotFound
    };
    assert_eq!(
        ego.status, expected_status,
        "status mismatch for center {center:?}: expected {expected_status:?}, got {:?}",
        ego.status
    );
    assert_eq!(ego.schema, RULE_PROVENANCE_EGO_SCHEMA_V1);
    assert_eq!(ego.rule_id, center);

    // Invariants 3-5 only meaningful when the center is a real
    // rule. The other two states must yield empty rings.
    if expected_status != RuleProvenanceEgoStatus::Available {
        assert!(ego.cited_memories.is_empty());
        assert!(ego.co_citing_rules.is_empty());
    } else {
        // Build the ground-truth index from the input edge set.
        let mut rule_to_memories: BTreeMap<&'static str, BTreeSet<&'static str>> = BTreeMap::new();
        let mut memory_to_rules: BTreeMap<&'static str, BTreeSet<&'static str>> = BTreeMap::new();
        for (rule, memory) in &declared_edges {
            rule_to_memories.entry(*rule).or_default().insert(*memory);
            memory_to_rules.entry(*memory).or_default().insert(*rule);
        }
        let center_memories: BTreeSet<&'static str> =
            rule_to_memories.get(center).cloned().unwrap_or_default();

        // Invariant 3a: cited_memories sorted by memory_id, and
        // its set matches the center's outgoing memory neighbors.
        let cited: Vec<&str> = ego
            .cited_memories
            .iter()
            .map(|c| c.memory_id.as_str())
            .collect();
        let mut cited_sorted = cited.clone();
        cited_sorted.sort_unstable();
        assert_eq!(cited, cited_sorted, "cited_memories not sorted");
        let cited_set: BTreeSet<&str> = cited.into_iter().collect();
        let expected_cited_set: BTreeSet<&str> = center_memories.iter().copied().collect();
        assert_eq!(cited_set, expected_cited_set, "cited_memories set mismatch");

        // Invariant 4: other_rule_count for each cited memory.
        for cited_memory in &ego.cited_memories {
            let other_rules: BTreeSet<&str> = memory_to_rules
                .get(cited_memory.memory_id.as_str())
                .map(|rules| {
                    rules
                        .iter()
                        .filter(|peer| **peer != center)
                        .copied()
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                cited_memory.other_rule_count,
                other_rules.len(),
                "other_rule_count mismatch for memory {}",
                cited_memory.memory_id
            );
        }

        // Invariant 3b: co_citing_rules sorted by rule_id with
        // sorted+deduped shared_memory_ids; invariant 5: center
        // rule never appears.
        let peer_ids: Vec<&str> = ego
            .co_citing_rules
            .iter()
            .map(|peer| peer.rule_id.as_str())
            .collect();
        let mut peer_sorted = peer_ids.clone();
        peer_sorted.sort_unstable();
        assert_eq!(peer_ids, peer_sorted, "co_citing_rules not sorted");
        for peer in &ego.co_citing_rules {
            assert_ne!(
                peer.rule_id.as_str(),
                center,
                "co_citing_rules echoes center rule"
            );
            let shared_ids = &peer.shared_memory_ids;
            let mut shared_sorted = shared_ids.clone();
            shared_sorted.sort_unstable();
            assert_eq!(
                shared_ids, &shared_sorted,
                "shared_memory_ids not sorted for peer {}",
                peer.rule_id
            );
            // Deduped.
            let unique: BTreeSet<&String> = shared_ids.iter().collect();
            assert_eq!(
                unique.len(),
                shared_ids.len(),
                "shared_memory_ids has duplicates"
            );
            // Non-empty: a peer only appears because it shares
            // at least one memory with the center.
            assert!(
                !shared_ids.is_empty(),
                "co_citing_rules entry with empty shared_memory_ids"
            );
            // shared_memory_count must equal the vec length.
            assert_eq!(peer.shared_memory_count, shared_ids.len());

            // Each shared memory is in the center's cited set
            // AND in the peer's memory set.
            let peer_memories: &BTreeSet<&'static str> =
                match rule_to_memories.get(peer.rule_id.as_str()) {
                    Some(memories) => memories,
                    None => panic!(
                        "co_citing_rules entry {} has no memories in input graph",
                        peer.rule_id
                    ),
                };
            for shared in shared_ids {
                assert!(
                    center_memories.contains(shared.as_str()),
                    "shared memory {shared} not cited by center"
                );
                assert!(
                    peer_memories.contains(shared.as_str()),
                    "shared memory {shared} not cited by peer {}",
                    peer.rule_id
                );
            }
        }

        // Invariant 3c: every peer in the ground truth must appear
        // in co_citing_rules with exactly the right shared set.
        for (peer, peer_memories) in &rule_to_memories {
            if *peer == center {
                continue;
            }
            let shared: BTreeSet<&'static str> = peer_memories
                .intersection(&center_memories)
                .copied()
                .collect();
            if shared.is_empty() {
                // Peer shares nothing with center; should not
                // appear in co_citing_rules.
                let observed = ego
                    .co_citing_rules
                    .iter()
                    .any(|entry| entry.rule_id.as_str() == *peer);
                assert!(
                    !observed,
                    "peer {peer} with no shared memories surfaced in co_citing_rules"
                );
            } else {
                let entry = ego
                    .co_citing_rules
                    .iter()
                    .find(|entry| entry.rule_id.as_str() == *peer)
                    .unwrap_or_else(|| panic!("peer {peer} missing from co_citing_rules"));
                let observed_shared: BTreeSet<&str> =
                    entry.shared_memory_ids.iter().map(String::as_str).collect();
                let expected_shared: BTreeSet<&str> = shared.iter().copied().collect();
                assert_eq!(
                    observed_shared, expected_shared,
                    "shared_memory_ids mismatch for peer {peer}"
                );
            }
        }
    }

    // Invariant 6: determinism. Two calls on the same graph
    // produce byte-identical JSON.
    let ego_again = compute_rule_provenance_ego(&graph, center);
    let json_first = serde_json::to_string(&ego).expect("first serialize");
    let json_second = serde_json::to_string(&ego_again).expect("second serialize");
    assert_eq!(
        json_first, json_second,
        "compute_rule_provenance_ego is not deterministic"
    );
});
