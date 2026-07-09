//! Bipartite HITS wrapper for the rule↔memory provenance projection
//! (bd-2jl2.1 / G9.a).
//!
//! The rule-provenance bipartite graph carries two partitions tagged
//! with the `bipartite_partition` node attribute: `rule` for rule nodes
//! and `memory` for memory nodes. Every edge crosses the partition by
//! construction (see `build_rule_provenance_bipartite_from_tables` in
//! `crate::graph`). This wrapper runs the underlying HITS power
//! iteration over the undirected projection and then partitions the
//! resulting scores so callers receive:
//!
//! - `authorities` keyed by memory ID — memories that many rules cite.
//! - `hubs` keyed by rule ID — rules that anchor against many memories.
//!
//! Determinism follows from the BTreeMap output; the same bipartite
//! graph yields byte-identical scores across repeated runs (J7).
//!
//! Snapshot persistence and downstream surfaces (G9.b `ee why
//! loadBearing` + `ee curate load-bearing protection`, G9.c `ee rule
//! provenance` + insights sections) consume this same `BipartiteHits`
//! shape.

use std::collections::{BTreeMap, BTreeSet};

use asupersync::Cx;
use fnx_algorithms::hits_centrality;
use fnx_runtime::CgseValue;
use serde::Serialize;

use crate::graph::Graph;
use crate::graph::GraphResult;
use crate::graph::algorithms::{DEFAULT_BACKGROUND_BUDGET, current_or_testing_cx, run_with_budget};
use crate::graph::hits::HITS_REPORT_SCHEMA_V1;

/// Node attribute key on the bipartite rule↔memory projection that
/// tags each node with its partition (`rule` or `memory`). Mirrors the
/// constant used inside `build_rule_provenance_bipartite_from_rows`.
pub const BIPARTITE_PARTITION_ATTR: &str = "bipartite_partition";
/// Partition value for rule nodes.
pub const BIPARTITE_PARTITION_RULE: &str = "rule";
/// Partition value for memory nodes.
pub const BIPARTITE_PARTITION_MEMORY: &str = "memory";

/// Partitioned HITS scores for a rule↔memory bipartite projection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BipartiteHits {
    /// Authority scores keyed by memory ID. Memories scored by how
    /// many rules anchor against them, weighted by the connecting
    /// rules' hub scores.
    pub authorities: BTreeMap<String, f64>,
    /// Hub scores keyed by rule ID. Rules scored by how many memories
    /// they cite, weighted by the connecting memories' authority
    /// scores.
    pub hubs: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadBearingMemoryItem {
    pub rank: usize,
    pub memory_id: String,
    pub load_bearing_score: f64,
    pub citing_rule_count: usize,
    pub interpretation: &'static str,
    pub evidence: LoadBearingMemoryEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadBearingMemoryEvidence {
    pub schema: &'static str,
    pub algorithm: &'static str,
    pub snapshot_version: u64,
}

/// Project bipartite authority scores into load-bearing memory items for
/// the `loadBearingMemories` insights surface.
///
/// Items are sorted by descending authority score and then memory ID so
/// equal scores remain deterministic across graph construction order.
/// Non-finite and zero scores are omitted because they carry no
/// actionable load-bearing signal for agents.
#[must_use]
pub fn load_bearing_memory_items(
    graph: &Graph,
    hits: &BipartiteHits,
    snapshot_version: u64,
) -> Vec<LoadBearingMemoryItem> {
    let mut scored_memories = hits
        .authorities
        .iter()
        .filter_map(|(memory_id, score)| {
            if score.is_finite()
                && *score > 0.0
                && matches!(
                    partition_for(graph, memory_id.as_str()),
                    Some(BIPARTITE_PARTITION_MEMORY)
                )
            {
                Some((memory_id.clone(), *score))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    // `total_cmp` over the previous `partial_cmp(...).unwrap_or(Equal)`.
    // The `filter(score.is_finite() && *score > 0.0)` above excludes
    // NaN today (`NaN > 0.0 == false`), and the `memory_id` tiebreaker
    // would stabilize any NaN-collapse anyway, so the two orderings are
    // observationally equivalent. But `load_bearing_memory_items`
    // feeds the `HITS_REPORT_SCHEMA_V1` envelope (G9.b
    // `ee why --load-bearing` + insights), which is a determinism
    // contract — same bipartite graph → byte-identical `rank` field —
    // and the `partial_cmp(...).unwrap_or(Equal)` pattern collapses
    // NaN onto an unspecified equivalence class. A future caller that
    // bypasses the upstream filter (refactor that feeds raw HITS
    // scores in, or a numerical edge case in `fnx_algorithms::
    // hits_centrality` that produces a non-finite authority value)
    // would silently break the envelope contract without tripping any
    // existing test. Mirrors the equivalent hardening of
    // `top_positive_influencers` / `top_negative_influencers` in
    // `src/core/influence.rs`.
    scored_memories.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });

    scored_memories
        .into_iter()
        .enumerate()
        .map(|(index, (memory_id, score))| LoadBearingMemoryItem {
            rank: index + 1,
            citing_rule_count: citing_rule_count(graph, &memory_id),
            memory_id,
            load_bearing_score: score,
            interpretation: "load_bearing",
            evidence: LoadBearingMemoryEvidence {
                schema: HITS_REPORT_SCHEMA_V1,
                algorithm: "bipartite_hits",
                snapshot_version,
            },
        })
        .collect()
}

fn citing_rule_count(graph: &Graph, memory_id: &str) -> usize {
    graph
        .neighbors_iter(memory_id)
        .map(|neighbors| {
            neighbors
                .filter(|node| matches!(partition_for(graph, node), Some(BIPARTITE_PARTITION_RULE)))
                .count()
        })
        .unwrap_or(0)
}

/// Compute partitioned HITS scores for the rule↔memory bipartite
/// projection.
///
/// Nodes without the `bipartite_partition` attribute are silently
/// dropped — the caller's responsibility is to pass the projection
/// built by `build_rule_provenance_bipartite_from_tables`, which tags
/// every node it adds.
pub fn compute_bipartite_hits(graph: &Graph) -> GraphResult<BipartiteHits> {
    let cx = current_or_testing_cx();
    compute_bipartite_hits_with_cx(&cx, graph)
}

pub fn compute_bipartite_hits_with_cx<Caps>(
    cx: &Cx<Caps>,
    graph: &Graph,
) -> GraphResult<BipartiteHits> {
    let graph = graph.clone();
    run_with_budget(cx, "bipartite_hits", DEFAULT_BACKGROUND_BUDGET, move || {
        let result = hits_centrality(&graph);
        let mut authorities: BTreeMap<String, f64> = BTreeMap::new();
        let mut hubs: BTreeMap<String, f64> = BTreeMap::new();
        for score in result.authorities {
            match partition_for(&graph, &score.node) {
                Some(BIPARTITE_PARTITION_MEMORY) => {
                    authorities.insert(score.node, score.score);
                }
                Some(BIPARTITE_PARTITION_RULE) => {
                    hubs.entry(score.node).or_insert(score.score);
                }
                _ => {}
            }
        }
        for score in result.hubs {
            match partition_for(&graph, &score.node) {
                Some(BIPARTITE_PARTITION_RULE) => {
                    hubs.insert(score.node, score.score);
                }
                Some(BIPARTITE_PARTITION_MEMORY) => {
                    authorities.entry(score.node).or_insert(score.score);
                }
                _ => {}
            }
        }
        BipartiteHits { authorities, hubs }
    })
}

fn partition_for<'a>(graph: &'a Graph, node: &str) -> Option<&'a str> {
    let attrs = graph.node_attrs(node)?;
    match attrs.get(BIPARTITE_PARTITION_ATTR)? {
        CgseValue::String(value) => Some(value.as_str()),
        _ => None,
    }
}

/// Stable schema tag for the rule-provenance ego-subgraph payload
/// surfaced by the upcoming `ee rule provenance` command (bd-2jl2.3).
pub const RULE_PROVENANCE_EGO_SCHEMA_V1: &str = "ee.graph.rule_provenance_ego.v1";

/// One memory cited by the center rule in a rule-provenance ego graph.
///
/// `other_rule_count` counts how many *other* rules also cite this
/// memory in the same bipartite projection (i.e. the size of the
/// memory's neighbor set on the rule partition, minus the center
/// rule). Memories with `other_rule_count == 0` are anchored solely by
/// the center rule.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleProvenanceCitedMemory {
    pub memory_id: String,
    pub other_rule_count: usize,
}

/// One peer rule that shares at least one cited memory with the center
/// rule. `shared_memory_ids` is the deterministic intersection of the
/// peer rule's memory set with the center rule's memory set.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleProvenanceCoCitingRule {
    pub rule_id: String,
    pub shared_memory_count: usize,
    pub shared_memory_ids: Vec<String>,
}

/// Bipartite ego subgraph for a single rule, formatted for the
/// `ee rule provenance <rule_id>` surface (bd-2jl2.3 / G9.c).
///
/// `cited_memories` is sorted by memory ID, `co_citing_rules` is sorted
/// by rule ID, and `shared_memory_ids` inside each peer is sorted by
/// memory ID. Combined with the BTreeSet-backed traversal, this yields
/// byte-identical JSON across runs (J7 determinism contract).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleProvenanceEgo {
    pub schema: &'static str,
    pub rule_id: String,
    pub status: RuleProvenanceEgoStatus,
    pub cited_memories: Vec<RuleProvenanceCitedMemory>,
    pub co_citing_rules: Vec<RuleProvenanceCoCitingRule>,
}

/// Honest enumeration of why an ego subgraph might be empty.
///
/// `Available` means the rule node was found and traversed (even if it
/// cited nothing). `RuleNotFound` separates "this rule has no
/// provenance edges yet" from "this rule isn't in the bipartite
/// projection at all" — agents need that distinction to decide whether
/// to retry after a steward refresh or to surface a structured error.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleProvenanceEgoStatus {
    #[default]
    Available,
    RuleNotFound,
    NotARuleNode,
}

/// Build the bipartite ego subgraph rooted at `rule_id`.
///
/// Walks the rule-provenance bipartite projection starting from
/// `rule_id`: collects the memories the rule cites (the 1-hop ring),
/// then walks each cited memory's other rule neighbors (the 2-hop
/// ring) to surface peer rules that overlap on at least one memory.
///
/// All output collections are deterministically sorted; repeated calls
/// against the same projection produce byte-identical JSON.
///
/// Returns a populated `RuleProvenanceEgo` even on the empty cases —
/// callers distinguish "rule not found" / "node exists but isn't a
/// rule" / "rule has zero citations" via `status` and the empty
/// `cited_memories` / `co_citing_rules` collections rather than via
/// `Result`.
#[must_use]
pub fn compute_rule_provenance_ego(graph: &Graph, rule_id: &str) -> RuleProvenanceEgo {
    let mut ego = RuleProvenanceEgo {
        schema: RULE_PROVENANCE_EGO_SCHEMA_V1,
        rule_id: rule_id.to_owned(),
        ..RuleProvenanceEgo::default()
    };

    match partition_for(graph, rule_id) {
        Some(BIPARTITE_PARTITION_RULE) => {}
        Some(_) => {
            ego.status = RuleProvenanceEgoStatus::NotARuleNode;
            return ego;
        }
        None => {
            ego.status = RuleProvenanceEgoStatus::RuleNotFound;
            return ego;
        }
    }

    // 1-hop: memories cited by this rule. BTreeSet keeps the
    // deterministic order independent of the underlying graph's
    // adjacency iteration order.
    let cited_memory_ids: BTreeSet<String> = match graph.neighbors_iter(rule_id) {
        Some(iter) => iter
            .filter(|node| matches!(partition_for(graph, node), Some(BIPARTITE_PARTITION_MEMORY)))
            .map(str::to_owned)
            .collect(),
        None => BTreeSet::new(),
    };

    // 2-hop: for each cited memory, enumerate the other rules that
    // also cite it. We track per-peer which memories overlap so the
    // CLI can surface "rule X co-cites memories [a, b]" without a
    // second pass.
    let mut peer_to_shared: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for memory_id in &cited_memory_ids {
        let memory_neighbor_iter = match graph.neighbors_iter(memory_id) {
            Some(iter) => iter,
            None => continue,
        };
        for peer in memory_neighbor_iter {
            if peer == rule_id {
                continue;
            }
            if !matches!(partition_for(graph, peer), Some(BIPARTITE_PARTITION_RULE)) {
                continue;
            }
            peer_to_shared
                .entry(peer.to_owned())
                .or_default()
                .insert(memory_id.clone());
        }
    }

    // Project the 1-hop memories with their `other_rule_count`. Walk
    // each memory's rule neighbors once more so we don't have to keep
    // a full inverse index in memory; ego subgraphs are small enough
    // that the second pass is cheap.
    ego.cited_memories = cited_memory_ids
        .iter()
        .map(|memory_id| {
            let other_rule_count = match graph.neighbors_iter(memory_id) {
                Some(iter) => iter
                    .filter(|peer| {
                        *peer != rule_id
                            && matches!(partition_for(graph, peer), Some(BIPARTITE_PARTITION_RULE))
                    })
                    .count(),
                None => 0,
            };
            RuleProvenanceCitedMemory {
                memory_id: memory_id.clone(),
                other_rule_count,
            }
        })
        .collect();

    ego.co_citing_rules = peer_to_shared
        .into_iter()
        .map(|(peer_rule_id, shared)| {
            let shared_memory_ids: Vec<String> = shared.into_iter().collect();
            RuleProvenanceCoCitingRule {
                rule_id: peer_rule_id,
                shared_memory_count: shared_memory_ids.len(),
                shared_memory_ids,
            }
        })
        .collect();

    ego
}

#[cfg(test)]
mod tests {
    use super::*;
    use fnx_classes::AttrMap;
    use fnx_runtime::CompatibilityMode;

    type TestResult = Result<(), String>;

    fn graph_result<T>(result: GraphResult<T>) -> Result<T, String> {
        result.map_err(|error| error.to_string())
    }

    fn partition_attrs(partition: &str) -> AttrMap {
        let mut attrs = AttrMap::new();
        attrs.insert(
            BIPARTITE_PARTITION_ATTR.to_owned(),
            CgseValue::String(partition.to_owned()),
        );
        attrs
    }

    fn non_string_partition_attrs() -> AttrMap {
        let mut attrs = AttrMap::new();
        attrs.insert(BIPARTITE_PARTITION_ATTR.to_owned(), CgseValue::Int(7));
        attrs
    }

    fn add_rule(graph: &mut Graph, rule: &str) {
        graph.add_node_with_attrs(rule, partition_attrs(BIPARTITE_PARTITION_RULE));
    }

    fn add_memory(graph: &mut Graph, memory: &str) {
        graph.add_node_with_attrs(memory, partition_attrs(BIPARTITE_PARTITION_MEMORY));
    }

    fn link(graph: &mut Graph, rule: &str, memory: &str) {
        graph
            .add_edge_with_attrs(rule, memory, AttrMap::new())
            .unwrap_or_else(|error| panic!("test edge {rule}→{memory} should add: {error:?}"));
    }

    #[test]
    fn bipartite_hits_empty_graph_returns_empty_partitions() -> TestResult {
        let graph = Graph::new(CompatibilityMode::Strict);

        let result = graph_result(compute_bipartite_hits(&graph))?;

        assert!(
            result.authorities.is_empty(),
            "empty bipartite must yield empty authorities"
        );
        assert!(
            result.hubs.is_empty(),
            "empty bipartite must yield empty hubs"
        );
        Ok(())
    }

    #[test]
    fn bipartite_hits_single_rule_with_one_source_memory() -> TestResult {
        let mut graph = Graph::new(CompatibilityMode::Strict);
        add_rule(&mut graph, "rule_001");
        add_memory(&mut graph, "mem_001");
        link(&mut graph, "rule_001", "mem_001");

        let result = graph_result(compute_bipartite_hits(&graph))?;

        assert_eq!(
            result.authorities.len(),
            1,
            "single memory landed in authorities"
        );
        assert_eq!(result.hubs.len(), 1, "single rule landed in hubs");
        assert!(result.authorities.get("mem_001").copied().unwrap_or(0.0) > 0.0);
        assert!(result.hubs.get("rule_001").copied().unwrap_or(0.0) > 0.0);
        Ok(())
    }

    #[test]
    fn bipartite_hits_multi_rule_shared_memory_lifts_shared_authority() -> TestResult {
        // Three rules all cite mem_shared. mem_solo is cited by only one
        // rule. mem_shared must score higher as an authority than mem_solo.
        let mut graph = Graph::new(CompatibilityMode::Strict);
        for rule in ["rule_a", "rule_b", "rule_c"] {
            add_rule(&mut graph, rule);
        }
        add_memory(&mut graph, "mem_shared");
        add_memory(&mut graph, "mem_solo");
        link(&mut graph, "rule_a", "mem_shared");
        link(&mut graph, "rule_b", "mem_shared");
        link(&mut graph, "rule_c", "mem_shared");
        link(&mut graph, "rule_a", "mem_solo");

        let result = graph_result(compute_bipartite_hits(&graph))?;

        let shared = result.authorities.get("mem_shared").copied().unwrap_or(0.0);
        let solo = result.authorities.get("mem_solo").copied().unwrap_or(0.0);
        assert!(
            shared > solo,
            "shared memory must out-score solo memory as authority ({shared} vs {solo})"
        );
        // rule_a cites both memories so it has more connective load than
        // rule_b or rule_c, but at minimum every rule must register as a hub.
        for rule in ["rule_a", "rule_b", "rule_c"] {
            assert!(
                result.hubs.get(rule).copied().unwrap_or(0.0) > 0.0,
                "rule {rule} must register a positive hub score"
            );
        }
        Ok(())
    }

    #[test]
    fn bipartite_hits_isolated_memory_scores_uniformly_with_other_isolates() -> TestResult {
        // Isolated nodes (no incident edges) cannot accumulate HITS
        // weight through power iteration; they fall to the floor score.
        let mut graph = Graph::new(CompatibilityMode::Strict);
        add_rule(&mut graph, "rule_main");
        add_memory(&mut graph, "mem_connected");
        add_memory(&mut graph, "mem_isolated");
        link(&mut graph, "rule_main", "mem_connected");

        let result = graph_result(compute_bipartite_hits(&graph))?;

        let connected = result
            .authorities
            .get("mem_connected")
            .copied()
            .unwrap_or(0.0);
        let isolated = result
            .authorities
            .get("mem_isolated")
            .copied()
            .unwrap_or(0.0);
        assert!(
            connected > isolated,
            "connected memory must out-score isolated memory ({connected} vs {isolated})"
        );
        Ok(())
    }

    #[test]
    fn bipartite_hits_isolated_rule_falls_below_connected_rules() -> TestResult {
        // Symmetric to the isolated-memory case: a rule with no incident
        // memory edges cannot accumulate hub weight.
        let mut graph = Graph::new(CompatibilityMode::Strict);
        add_rule(&mut graph, "rule_connected");
        add_rule(&mut graph, "rule_isolated");
        add_memory(&mut graph, "mem_anchor");
        link(&mut graph, "rule_connected", "mem_anchor");

        let result = graph_result(compute_bipartite_hits(&graph))?;

        let connected = result.hubs.get("rule_connected").copied().unwrap_or(0.0);
        let isolated = result.hubs.get("rule_isolated").copied().unwrap_or(0.0);
        assert!(
            connected > isolated,
            "connected rule must out-score isolated rule ({connected} vs {isolated})"
        );
        // Determinism sanity check: a second run produces the same result.
        let second = graph_result(compute_bipartite_hits(&graph))?;
        assert_eq!(result, second, "bipartite HITS must be deterministic");
        Ok(())
    }

    #[test]
    fn bipartite_hits_drops_unpartitioned_and_non_string_partition_nodes() -> TestResult {
        let mut graph = Graph::new(CompatibilityMode::Strict);
        add_rule(&mut graph, "rule_good");
        add_memory(&mut graph, "mem_good");
        graph.add_node_with_attrs("node_unpartitioned", AttrMap::new());
        graph.add_node_with_attrs("node_bad_partition", non_string_partition_attrs());
        link(&mut graph, "rule_good", "mem_good");
        link(&mut graph, "rule_good", "node_bad_partition");
        link(&mut graph, "node_unpartitioned", "mem_good");

        let result = graph_result(compute_bipartite_hits(&graph))?;

        assert_eq!(
            result.authorities.keys().cloned().collect::<Vec<_>>(),
            vec!["mem_good".to_owned()],
            "only memory-partition nodes may surface as authorities"
        );
        assert_eq!(
            result.hubs.keys().cloned().collect::<Vec<_>>(),
            vec!["rule_good".to_owned()],
            "only rule-partition nodes may surface as hubs"
        );
        Ok(())
    }

    #[test]
    fn load_bearing_memory_items_rank_shared_authorities_for_insights() -> TestResult {
        let mut graph = Graph::new(CompatibilityMode::Strict);
        for rule in ["rule_cornerstone_a", "rule_cornerstone_b"] {
            add_rule(&mut graph, rule);
        }
        add_memory(&mut graph, "mem_load_bearing");
        add_memory(&mut graph, "mem_solo_source");
        link(&mut graph, "rule_cornerstone_a", "mem_load_bearing");
        link(&mut graph, "rule_cornerstone_a", "mem_solo_source");
        link(&mut graph, "rule_cornerstone_b", "mem_load_bearing");

        let hits = graph_result(compute_bipartite_hits(&graph))?;
        let items = load_bearing_memory_items(&graph, &hits, 17);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].rank, 1);
        assert_eq!(items[0].memory_id, "mem_load_bearing");
        assert_eq!(items[0].citing_rule_count, 2);
        assert_eq!(items[0].interpretation, "load_bearing");
        assert_eq!(items[0].evidence.schema, HITS_REPORT_SCHEMA_V1);
        assert_eq!(items[0].evidence.algorithm, "bipartite_hits");
        assert_eq!(items[0].evidence.snapshot_version, 17);
        assert!(
            items[0].load_bearing_score > items[1].load_bearing_score,
            "memory cited by two cornerstone rules should outrank solo memory"
        );
        assert_eq!(items[1].rank, 2);
        assert_eq!(items[1].memory_id, "mem_solo_source");
        assert_eq!(items[1].citing_rule_count, 1);
        Ok(())
    }

    #[test]
    fn load_bearing_memory_items_filter_noise_and_tie_break_by_memory_id() {
        let mut graph = Graph::new(CompatibilityMode::Strict);
        add_rule(&mut graph, "rule_a");
        add_rule(&mut graph, "rule_b");
        for memory in ["mem_a", "mem_b", "mem_zero", "mem_nan"] {
            add_memory(&mut graph, memory);
            link(&mut graph, "rule_a", memory);
        }
        graph.add_node_with_attrs("node_unpartitioned", AttrMap::new());
        link(&mut graph, "rule_b", "mem_b");
        link(&mut graph, "rule_b", "node_unpartitioned");

        let hits = BipartiteHits {
            authorities: BTreeMap::from([
                ("mem_b".to_owned(), 0.5),
                ("mem_a".to_owned(), 0.5),
                ("mem_zero".to_owned(), 0.0),
                ("mem_nan".to_owned(), f64::NAN),
                ("node_unpartitioned".to_owned(), 0.9),
            ]),
            hubs: BTreeMap::new(),
        };

        let items = load_bearing_memory_items(&graph, &hits, 4);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].rank, 1);
        assert_eq!(items[0].memory_id, "mem_a");
        assert_eq!(items[0].citing_rule_count, 1);
        assert_eq!(items[1].rank, 2);
        assert_eq!(items[1].memory_id, "mem_b");
        assert_eq!(items[1].citing_rule_count, 2);
    }

    #[test]
    fn rule_provenance_ego_unknown_rule_returns_rule_not_found() {
        let graph = Graph::new(CompatibilityMode::Strict);

        let ego = compute_rule_provenance_ego(&graph, "rule_missing");

        assert_eq!(ego.schema, RULE_PROVENANCE_EGO_SCHEMA_V1);
        assert_eq!(ego.rule_id, "rule_missing");
        assert_eq!(ego.status, RuleProvenanceEgoStatus::RuleNotFound);
        assert!(ego.cited_memories.is_empty());
        assert!(ego.co_citing_rules.is_empty());
    }

    #[test]
    fn rule_provenance_ego_memory_node_passed_as_rule_returns_not_a_rule_node() {
        let mut graph = Graph::new(CompatibilityMode::Strict);
        add_memory(&mut graph, "mem_001");

        let ego = compute_rule_provenance_ego(&graph, "mem_001");

        assert_eq!(ego.status, RuleProvenanceEgoStatus::NotARuleNode);
        assert!(ego.cited_memories.is_empty());
        assert!(ego.co_citing_rules.is_empty());
    }

    #[test]
    fn rule_provenance_ego_ignores_unpartitioned_neighbors() {
        let mut graph = Graph::new(CompatibilityMode::Strict);
        add_rule(&mut graph, "rule_main");
        add_rule(&mut graph, "rule_peer");
        add_memory(&mut graph, "mem_shared");
        graph.add_node_with_attrs("rule_unpartitioned", AttrMap::new());
        graph.add_node_with_attrs("mem_bad_partition", non_string_partition_attrs());
        link(&mut graph, "rule_main", "mem_shared");
        link(&mut graph, "rule_peer", "mem_shared");
        link(&mut graph, "rule_unpartitioned", "mem_shared");
        link(&mut graph, "rule_main", "mem_bad_partition");

        let ego = compute_rule_provenance_ego(&graph, "rule_main");

        let cited: Vec<_> = ego
            .cited_memories
            .iter()
            .map(|memory| (memory.memory_id.as_str(), memory.other_rule_count))
            .collect();
        assert_eq!(
            cited,
            vec![("mem_shared", 1)],
            "ego traversal must ignore non-memory neighbors and unpartitioned peer rules"
        );
        assert_eq!(ego.co_citing_rules.len(), 1);
        assert_eq!(ego.co_citing_rules[0].rule_id, "rule_peer");
        assert_eq!(
            ego.co_citing_rules[0].shared_memory_ids,
            vec!["mem_shared".to_owned()]
        );
    }

    #[test]
    fn rule_provenance_ego_isolated_rule_yields_empty_rings() {
        let mut graph = Graph::new(CompatibilityMode::Strict);
        add_rule(&mut graph, "rule_lonely");

        let ego = compute_rule_provenance_ego(&graph, "rule_lonely");

        assert_eq!(ego.status, RuleProvenanceEgoStatus::Available);
        assert!(ego.cited_memories.is_empty());
        assert!(ego.co_citing_rules.is_empty());
    }

    #[test]
    fn rule_provenance_ego_collects_solo_and_shared_memory_citations() {
        // rule_main cites mem_solo (only it) and mem_shared (also rule_peer).
        // rule_peer also cites mem_isolated_to_peer (which the ego must NOT
        // surface, because the center is rule_main).
        let mut graph = Graph::new(CompatibilityMode::Strict);
        add_rule(&mut graph, "rule_main");
        add_rule(&mut graph, "rule_peer");
        add_memory(&mut graph, "mem_solo");
        add_memory(&mut graph, "mem_shared");
        add_memory(&mut graph, "mem_isolated_to_peer");
        link(&mut graph, "rule_main", "mem_solo");
        link(&mut graph, "rule_main", "mem_shared");
        link(&mut graph, "rule_peer", "mem_shared");
        link(&mut graph, "rule_peer", "mem_isolated_to_peer");

        let ego = compute_rule_provenance_ego(&graph, "rule_main");

        assert_eq!(ego.status, RuleProvenanceEgoStatus::Available);

        // Cited memories: sorted by memory ID, with correct other-rule counts.
        let cited: Vec<_> = ego
            .cited_memories
            .iter()
            .map(|m| (m.memory_id.as_str(), m.other_rule_count))
            .collect();
        assert_eq!(cited, vec![("mem_shared", 1), ("mem_solo", 0)]);

        // Co-citing rules: only rule_peer (sharing mem_shared); the
        // ego must NOT surface mem_isolated_to_peer because the center
        // rule does not cite it.
        assert_eq!(ego.co_citing_rules.len(), 1);
        let peer = &ego.co_citing_rules[0];
        assert_eq!(peer.rule_id, "rule_peer");
        assert_eq!(peer.shared_memory_count, 1);
        assert_eq!(peer.shared_memory_ids, vec!["mem_shared".to_string()]);
    }

    #[test]
    fn rule_provenance_ego_is_deterministic_across_runs() {
        // Same graph constructed twice in different add-order; the ego
        // output must be byte-identical (J7 determinism).
        fn build(order: &[(&str, &str, &str)]) -> Graph {
            let mut graph = Graph::new(CompatibilityMode::Strict);
            // Add all unique rules and memories first so partition tags
            // exist when the edges land.
            let mut seen_rules: BTreeSet<&str> = BTreeSet::new();
            let mut seen_memories: BTreeSet<&str> = BTreeSet::new();
            for (rule, memory, _) in order {
                if seen_rules.insert(rule) {
                    add_rule(&mut graph, rule);
                }
                if seen_memories.insert(memory) {
                    add_memory(&mut graph, memory);
                }
            }
            for (rule, memory, _) in order {
                link(&mut graph, rule, memory);
            }
            graph
        }

        let order_a = vec![
            ("rule_main", "mem_b", ""),
            ("rule_peer", "mem_b", ""),
            ("rule_main", "mem_a", ""),
            ("rule_peer", "mem_a", ""),
        ];
        let mut order_b = order_a.clone();
        order_b.reverse();

        let ego_a = compute_rule_provenance_ego(&build(&order_a), "rule_main");
        let ego_b = compute_rule_provenance_ego(&build(&order_b), "rule_main");

        let json_a = serde_json::to_string(&ego_a).expect("ego A serializes");
        let json_b = serde_json::to_string(&ego_b).expect("ego B serializes");
        assert_eq!(
            json_a, json_b,
            "ego output must be insertion-order-invariant"
        );

        // Spot-check the schema content too.
        let cited: Vec<_> = ego_a
            .cited_memories
            .iter()
            .map(|m| m.memory_id.as_str())
            .collect();
        assert_eq!(cited, vec!["mem_a", "mem_b"]);
        assert_eq!(ego_a.co_citing_rules.len(), 1);
        assert_eq!(
            ego_a.co_citing_rules[0].shared_memory_ids,
            vec!["mem_a".to_string(), "mem_b".to_string()]
        );
    }
}
