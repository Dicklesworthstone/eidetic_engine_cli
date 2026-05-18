//! Integer-only minhash rank centrality for deterministic top-K graph reads.
//!
//! This module intentionally does not replace PageRank. It supplies the
//! deterministic primitive for `bd-3usjw.46`: rank nodes from incoming edge
//! sets using stable minhash signatures and integer score keys, so the same
//! graph produces byte-identical output across CPU and platform targets.

use asupersync::Cx;
use fnx_algorithms::ComplexityWitness;
use serde::Serialize;

use crate::graph::DiGraph;
use crate::graph::GraphResult;
use crate::graph::algorithms::{check_cancelled, current_or_testing_cx};

pub const MINHASH_RANK_SCHEMA_V1: &str = "ee.graph.minhash_rank.v1";
pub const DEFAULT_MINHASH_SIGNATURE_COUNT: usize = 64;
pub const DEFAULT_MINHASH_TOP_K: usize = 100;

const MINHASH_SIGNATURE_COUNT_MAX: usize = 256;
const DENSITY_EXACT_SHIFT: u32 = 48;
const DENSITY_SKETCH_MASK: u64 = (1_u64 << DENSITY_EXACT_SHIFT) - 1;

// Pre-folded FNV1a state for the per-node domain separator. Computed at
// compile time so each `stable_node_hash` call only feeds the node bytes
// through the hasher; bd-3usjw.46 top-K phase visits this thousands of
// times, so removing the per-call 32-byte FNV prefix is load-bearing.
const NODE_HASH_SEED: u64 =
    fnv1a_update_const(0xcbf2_9ce4_8422_2325_u64, b"ee.graph.minhash_rank.node.v1");

fn trace_minhash_rank_checkpoint(
    phase: &'static str,
    elapsed_ms: u64,
    candidate_count: usize,
    degraded_codes: &[&str],
) {
    tracing::info!(
        workspace_id = "graph",
        request_id = "minhash_rank_centrality",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-3usjw.46"),
        surface = "minhash_rank_centrality",
        phase,
        elapsed_ms,
        candidate_count,
        degraded_codes = ?degraded_codes,
        "minhash rank centrality checkpoint"
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MinHashRankPolicy {
    pub signature_count: usize,
    pub top_k: usize,
}

impl Default for MinHashRankPolicy {
    fn default() -> Self {
        Self {
            signature_count: DEFAULT_MINHASH_SIGNATURE_COUNT,
            top_k: DEFAULT_MINHASH_TOP_K,
        }
    }
}

impl MinHashRankPolicy {
    #[must_use]
    pub fn normalized(self) -> Self {
        Self {
            signature_count: self.signature_count.clamp(1, MINHASH_SIGNATURE_COUNT_MAX),
            top_k: self.top_k.max(1),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MinHashRankScore {
    pub rank: usize,
    pub node: String,
    pub signature_density: u64,
    pub incoming_edge_count: usize,
    pub outgoing_edge_count: usize,
    pub signature: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MinHashRankResult {
    pub schema: &'static str,
    pub policy: MinHashRankPolicy,
    pub scores: Vec<MinHashRankScore>,
    pub witness: ComplexityWitness,
}

pub fn compute_minhash_rank(graph: &DiGraph) -> GraphResult<MinHashRankResult> {
    compute_minhash_rank_with_policy(graph, MinHashRankPolicy::default())
}

pub fn compute_minhash_rank_with_policy(
    graph: &DiGraph,
    policy: MinHashRankPolicy,
) -> GraphResult<MinHashRankResult> {
    let cx = current_or_testing_cx();
    compute_minhash_rank_with_cx(&cx, graph, policy)
}

pub fn compute_minhash_rank_with_cx(
    cx: &Cx,
    graph: &DiGraph,
    policy: MinHashRankPolicy,
) -> GraphResult<MinHashRankResult> {
    const ALGORITHM_NAME: &str = "minhash_rank_centrality";

    trace_minhash_rank_checkpoint("input", 0, graph.node_count(), &[]);
    check_cancelled(cx, ALGORITHM_NAME)?;
    trace_minhash_rank_checkpoint("dependency_check", 0, graph.node_count(), &[]);
    let input = MinHashRankInput::from_graph(graph);
    let result = compute_minhash_rank_unbudgeted(graph, input, policy.normalized());
    check_cancelled(cx, ALGORITHM_NAME)?;
    trace_minhash_rank_checkpoint("response", 0, result.scores.len(), &[]);
    Ok(result)
}

// Borrowed view of the graph used for ranking. We deliberately keep this
// shape minimal: ranking is determined entirely by `incoming_counts` (via
// `exact_density_key`), with edge-hash work deferred to the top-K phase
// so cold nodes never pay for signatures they don't survive into.
struct MinHashRankInput<'g> {
    nodes: Vec<&'g str>,
    incoming_counts: Vec<usize>,
    outgoing_counts: Vec<usize>,
    edge_count: usize,
}

struct MinHashRankCandidate {
    node_index: usize,
    signature_density: u64,
    incoming_edge_count: usize,
    outgoing_edge_count: usize,
}

impl<'g> MinHashRankInput<'g> {
    fn from_graph(graph: &'g DiGraph) -> Self {
        let mut nodes: Vec<&str> = graph.nodes_ordered();
        nodes.sort_unstable();

        let incoming_counts: Vec<usize> = nodes.iter().map(|node| graph.in_degree(node)).collect();
        let outgoing_counts: Vec<usize> = nodes.iter().map(|node| graph.out_degree(node)).collect();
        let edge_count: usize = incoming_counts.iter().sum();

        Self {
            nodes,
            incoming_counts,
            outgoing_counts,
            edge_count,
        }
    }
}

fn compute_minhash_rank_unbudgeted(
    graph: &DiGraph,
    input: MinHashRankInput<'_>,
    policy: MinHashRankPolicy,
) -> MinHashRankResult {
    let MinHashRankInput {
        nodes,
        incoming_counts,
        outgoing_counts,
        edge_count,
    } = input;
    let node_count = nodes.len();

    let mut candidates: Vec<MinHashRankCandidate> = (0..node_count)
        .map(|index| MinHashRankCandidate {
            node_index: index,
            signature_density: exact_density_key(incoming_counts[index]),
            incoming_edge_count: incoming_counts[index],
            outgoing_edge_count: outgoing_counts[index],
        })
        .collect();

    // Correct top-K selection for composite key (primary=exact density + secondary=sketch).
    // We partition cheaply on primary only, then identify the marginal density at position top_k.
    // Non-zero marginal groups need sketches for every node in the group because the sketch
    // decides which tied nodes survive. A zero-incoming marginal group has no sketch entropy;
    // it can be trimmed by outgoing degree and node id before materializing rows.
    let top_k = policy.top_k;
    let candidates = if candidates.len() <= top_k {
        candidates
    } else {
        candidates.select_nth_unstable_by(top_k - 1, |left, right| {
            candidate_primary_order(left, right, &nodes)
        });
        let marginal_density = candidates[top_k - 1].signature_density;

        let mut strict = Vec::with_capacity(top_k);
        let mut marginal = Vec::new();
        for candidate in candidates {
            if candidate.signature_density > marginal_density {
                strict.push(candidate);
            } else if candidate.signature_density == marginal_density {
                marginal.push(candidate);
            }
        }

        let needed_from_marginal = top_k.saturating_sub(strict.len());
        if marginal_density == 0 && marginal.len() > needed_from_marginal {
            marginal.select_nth_unstable_by(needed_from_marginal, |left, right| {
                candidate_zero_marginal_order(left, right, &nodes)
            });
            marginal.truncate(needed_from_marginal);
        }
        strict.extend(marginal);
        strict
    };

    let mut signature_edges_scanned = 0usize;
    let mut rows: Vec<MinHashRankScore> = candidates
        .into_iter()
        .map(|candidate| {
            let target_name = nodes[candidate.node_index];
            let target_hash = stable_node_hash(target_name);
            let mut signature = vec![u64::MAX; policy.signature_count];
            let mut scanned = 0usize;

            if let Some(predecessors) = graph.predecessors_iter(target_name) {
                for source_name in predecessors {
                    let source_hash = stable_node_hash(source_name);
                    let edge_hash = stable_edge_hash(source_hash, target_hash);
                    scanned = scanned.saturating_add(1);
                    for (seed, slot) in signature.iter_mut().enumerate() {
                        let value = seeded_minhash_value(seed, edge_hash);
                        if value < *slot {
                            *slot = value;
                        }
                    }
                }
            }

            signature_edges_scanned = signature_edges_scanned
                .saturating_add(scanned.saturating_mul(policy.signature_count));

            MinHashRankScore {
                rank: 0,
                node: target_name.to_owned(),
                signature_density: signature_density_key(
                    candidate.incoming_edge_count,
                    signature.first().copied().unwrap_or(u64::MAX),
                ),
                incoming_edge_count: candidate.incoming_edge_count,
                outgoing_edge_count: candidate.outgoing_edge_count,
                signature,
            }
        })
        .collect();
    rows.sort_by(|left, right| {
        right
            .signature_density
            .cmp(&left.signature_density)
            .then_with(|| right.incoming_edge_count.cmp(&left.incoming_edge_count))
            .then_with(|| right.outgoing_edge_count.cmp(&left.outgoing_edge_count))
            .then_with(|| left.node.cmp(&right.node))
    });
    rows.truncate(top_k);
    for (index, row) in rows.iter_mut().enumerate() {
        row.rank = index + 1;
    }

    MinHashRankResult {
        schema: MINHASH_RANK_SCHEMA_V1,
        policy,
        witness: ComplexityWitness {
            algorithm: "minhash_rank_centrality".to_owned(),
            complexity_claim: "O(|V| + m * |E_marginal|) integer minhash top-K; zero-incoming marginal groups are trimmed before sketch materialization"
                .to_owned(),
            nodes_touched: node_count,
            edges_scanned: edge_count.saturating_add(signature_edges_scanned),
            queue_peak: rows.len(),
        },
        scores: rows,
    }
}

fn candidate_primary_order(
    left: &MinHashRankCandidate,
    right: &MinHashRankCandidate,
    nodes: &[&str],
) -> std::cmp::Ordering {
    right
        .signature_density
        .cmp(&left.signature_density)
        .then_with(|| right.incoming_edge_count.cmp(&left.incoming_edge_count))
        .then_with(|| right.outgoing_edge_count.cmp(&left.outgoing_edge_count))
        .then_with(|| nodes[left.node_index].cmp(nodes[right.node_index]))
}

fn candidate_zero_marginal_order(
    left: &MinHashRankCandidate,
    right: &MinHashRankCandidate,
    nodes: &[&str],
) -> std::cmp::Ordering {
    right
        .outgoing_edge_count
        .cmp(&left.outgoing_edge_count)
        .then_with(|| nodes[left.node_index].cmp(nodes[right.node_index]))
}

fn stable_node_hash(node: &str) -> u64 {
    splitmix64(fnv1a_update(NODE_HASH_SEED, node.as_bytes()))
}

fn stable_edge_hash(source_hash: u64, target_hash: u64) -> u64 {
    splitmix64(source_hash ^ target_hash.rotate_left(31) ^ 0x4f1b_5d9a_b715_6a37)
}

fn fnv1a_update(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

const fn fnv1a_update_const(mut hash: u64, bytes: &[u8]) -> u64 {
    let mut index = 0;
    while index < bytes.len() {
        hash ^= bytes[index] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        index += 1;
    }
    hash
}

fn seeded_minhash_value(seed: usize, edge_hash: u64) -> u64 {
    let seed = u64::try_from(seed).unwrap_or(u64::MAX);
    splitmix64(edge_hash ^ seed.wrapping_mul(0x9e37_79b9_7f4a_7c15))
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn exact_density_key(incoming_edge_count: usize) -> u64 {
    u64::try_from(incoming_edge_count)
        .unwrap_or(u64::MAX)
        .min(u64::MAX >> DENSITY_EXACT_SHIFT)
        << DENSITY_EXACT_SHIFT
}

fn signature_density_key(incoming_edge_count: usize, sketch_min: u64) -> u64 {
    let exact = exact_density_key(incoming_edge_count);
    let sketch = if incoming_edge_count == 0 {
        0
    } else {
        (u64::MAX.saturating_sub(sketch_min) >> 16) & DENSITY_SKETCH_MASK
    };
    exact | sketch
}

#[cfg(test)]
mod tests {
    use super::*;
    use fnx_runtime::CompatibilityMode;

    type TestResult = Result<(), String>;

    fn graph_result<T>(result: GraphResult<T>) -> Result<T, String> {
        result.map_err(|error| error.to_string())
    }

    fn graph() -> DiGraph {
        DiGraph::new(CompatibilityMode::Strict)
    }

    fn add_edge(graph: &mut DiGraph, source: &str, target: &str) -> TestResult {
        graph
            .add_edge(source, target)
            .map_err(|error| format!("add edge {source}->{target}: {error}"))
    }

    #[test]
    fn minhash_rank_empty_graph_returns_empty_result() -> TestResult {
        let result = graph_result(compute_minhash_rank(&graph()))?;

        assert_eq!(result.schema, MINHASH_RANK_SCHEMA_V1);
        assert!(result.scores.is_empty());
        assert_eq!(result.witness.nodes_touched, 0);
        Ok(())
    }

    #[test]
    fn minhash_rank_prefers_dense_incoming_edge_sets() -> TestResult {
        let mut graph = graph();
        add_edge(&mut graph, "a", "hub")?;
        add_edge(&mut graph, "b", "hub")?;
        add_edge(&mut graph, "c", "hub")?;
        add_edge(&mut graph, "hub", "leaf")?;

        let result = graph_result(compute_minhash_rank_with_policy(
            &graph,
            MinHashRankPolicy {
                signature_count: 16,
                top_k: 4,
            },
        ))?;

        assert_eq!(result.scores[0].node, "hub");
        assert_eq!(result.scores[0].rank, 1);
        assert_eq!(result.scores[0].incoming_edge_count, 3);
        assert_eq!(result.scores[1].node, "leaf");
        assert_eq!(result.scores[1].incoming_edge_count, 1);
        assert!(
            result.scores[0].signature_density > result.scores[1].signature_density,
            "hub density should exceed leaf density"
        );
        Ok(())
    }

    #[test]
    fn minhash_rank_is_byte_stable() -> TestResult {
        let mut graph = graph();
        for (source, target) in [
            ("n4", "n1"),
            ("n3", "n1"),
            ("n2", "n1"),
            ("n4", "n2"),
            ("n3", "n2"),
            ("n4", "n3"),
        ] {
            add_edge(&mut graph, source, target)?;
        }
        let policy = MinHashRankPolicy {
            signature_count: 32,
            top_k: 4,
        };

        let first = graph_result(compute_minhash_rank_with_policy(&graph, policy))?;
        let second = graph_result(compute_minhash_rank_with_policy(&graph, policy))?;

        let first_bytes = serde_json::to_vec(&first).map_err(|error| error.to_string())?;
        let second_bytes = serde_json::to_vec(&second).map_err(|error| error.to_string())?;
        assert_eq!(first_bytes, second_bytes);
        assert_eq!(first, second);
        Ok(())
    }

    #[test]
    fn minhash_rank_trims_zero_incoming_marginal_group_before_scoring() -> TestResult {
        let mut graph = graph();
        add_edge(&mut graph, "src_1", "hot")?;
        add_edge(&mut graph, "src_2", "hot")?;
        for index in 0..16 {
            graph.add_node(format!("cold_{index:02}"));
        }

        let result = graph_result(compute_minhash_rank_with_policy(
            &graph,
            MinHashRankPolicy {
                signature_count: 8,
                top_k: 5,
            },
        ))?;

        assert_eq!(result.scores.len(), 5);
        assert_eq!(result.witness.queue_peak, 5);
        assert_eq!(result.scores[0].node, "hot");
        assert!(
            result.scores[1..]
                .iter()
                .all(|score| score.incoming_edge_count == 0)
        );
        Ok(())
    }

    #[test]
    fn minhash_policy_normalization_bounds_signature_count_and_top_k() {
        assert_eq!(
            MinHashRankPolicy {
                signature_count: 0,
                top_k: 0
            }
            .normalized(),
            MinHashRankPolicy {
                signature_count: 1,
                top_k: 1
            }
        );
        assert_eq!(
            MinHashRankPolicy {
                signature_count: usize::MAX,
                top_k: 7
            }
            .normalized()
            .signature_count,
            MINHASH_SIGNATURE_COUNT_MAX
        );
    }

    #[test]
    fn node_hash_seed_matches_runtime_fnv1a_fold() {
        // bd-3usjw.46: NODE_HASH_SEED is the const-folded FNV1a state after
        // feeding the domain separator. Confirms the const path matches the
        // runtime path so we don't accidentally change every node hash by
        // editing one or the other.
        let runtime = fnv1a_update(0xcbf2_9ce4_8422_2325_u64, b"ee.graph.minhash_rank.node.v1");
        assert_eq!(NODE_HASH_SEED, runtime);
    }
}
