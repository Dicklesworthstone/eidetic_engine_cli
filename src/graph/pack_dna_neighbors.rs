//! Positive-support PPR neighbors with bounded presentation working storage.
//!
//! The upstream result contains zero entries for nodes outside the supported
//! seed walk. Those are not neighbors. Keep only the requested best positive
//! non-seed entries, with the existing exact score and memory-ID ordering.
//! This selects a report prefix; it does not compute or alter PageRank.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use super::{Cx, GraphResult, MemoryId, PackDnaPprNeighbor, check_cancelled};
use crate::util::radix_ulid_sort::{
    compare_ulid_payload_or_lexical, sort_by_ulid_payload_or_lexical,
};

pub(super) fn select<Caps>(
    cx: &Cx<Caps>,
    scores: BTreeMap<MemoryId, f64>,
    seeds: &BTreeSet<MemoryId>,
    limit: usize,
) -> GraphResult<Vec<PackDnaPprNeighbor>> {
    select_with_checkpoint(scores, seeds, limit, |_| check_cancelled(cx, "pack_dna"))
}

fn select_with_checkpoint(
    scores: BTreeMap<MemoryId, f64>,
    seeds: &BTreeSet<MemoryId>,
    limit: usize,
    mut checkpoint: impl FnMut(usize) -> GraphResult<()>,
) -> GraphResult<Vec<PackDnaPprNeighbor>> {
    checkpoint(0)?;
    if limit == 0 {
        return Ok(Vec::new());
    }
    // Do not reserve a caller-provided limit (which may be usize::MAX).
    // The input score map is still O(graph nodes); only presentation scratch
    // and converted memory-ID strings are bounded by the requested prefix.
    let mut best = BinaryHeap::<RankedNeighbor>::new();
    for (index, (memory_id, score)) in scores.into_iter().enumerate() {
        if index != 0 && index % 256 == 0 {
            checkpoint(best.len())?;
        }
        if !score.is_finite() || score <= 0.0 || seeds.contains(&memory_id) {
            continue;
        }
        if best.len() == limit && best.peek().is_some_and(|worst| score < worst.0.score) {
            continue;
        }
        let candidate = RankedNeighbor(PackDnaPprNeighbor {
            memory_id: memory_id.to_string(),
            score,
        });
        if best.len() < limit {
            best.push(candidate);
        } else if let Some(mut worst) = best.peek_mut()
            && candidate < *worst
        {
            *worst = candidate;
        }
    }
    checkpoint(best.len())?;
    let mut neighbors = best.into_iter().map(|entry| entry.0).collect::<Vec<_>>();
    sort_by_ulid_payload_or_lexical(&mut neighbors, |neighbor| neighbor.memory_id.as_str());
    neighbors.sort_by(|left, right| right.score.total_cmp(&left.score));
    checkpoint(neighbors.len())?;
    Ok(neighbors)
}

/// A max-heap whose greatest entry is the worst retained neighbor. Equal
/// scores evict the greater canonical ID first, exactly as the old full sort.
struct RankedNeighbor(PackDnaPprNeighbor);

impl Ord for RankedNeighbor {
    fn cmp(&self, other: &Self) -> Ordering {
        other.0.score.total_cmp(&self.0.score).then_with(|| {
            compare_ulid_payload_or_lexical(&self.0.memory_id, &other.0.memory_id)
        })
    }
}

impl PartialOrd for RankedNeighbor {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for RankedNeighbor {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for RankedNeighbor {}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use crate::graph::pack_dna::{PackDnaInput, compute_pack_dna, current_or_testing_cx};
    use crate::graph::{AttrMap, DiGraph, GraphError, MemoryGraphProjection};
    use fnx_runtime::CgseValue;

    fn mem(number: u128) -> MemoryId {
        MemoryId::from_uuid(uuid::Uuid::from_u128(number))
    }

    fn reference(
        scores: &BTreeMap<MemoryId, f64>,
        seeds: &BTreeSet<MemoryId>,
        limit: usize,
    ) -> Vec<PackDnaPprNeighbor> {
        let mut all = scores
            .iter()
            .filter(|(id, score)| score.is_finite() && **score > 0.0 && !seeds.contains(*id))
            .map(|(id, score)| PackDnaPprNeighbor {
                memory_id: id.to_string(),
                score: *score,
            })
            .collect::<Vec<_>>();
        sort_by_ulid_payload_or_lexical(&mut all, |neighbor| neighbor.memory_id.as_str());
        all.sort_by(|left, right| right.score.total_cmp(&left.score));
        all.truncate(limit);
        all
    }

    #[test]
    fn zero_negative_nonfinite_and_seed_scores_cannot_pad_the_report() {
        let scores = BTreeMap::from([
            (mem(1), 0.0),
            (mem(2), -0.0),
            (mem(3), -1.0),
            (mem(4), f64::NAN),
            (mem(5), f64::INFINITY),
            (mem(6), f64::NEG_INFINITY),
            (mem(7), 100.0),
            (mem(8), 0.25),
            (mem(9), f64::from_bits(1)),
        ]);
        let seeds = BTreeSet::from([mem(7)]);
        let selected = select(&current_or_testing_cx(), scores.clone(), &seeds, 20).unwrap();
        assert_eq!(selected, reference(&scores, &seeds, 20));
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].memory_id, mem(8).to_string());
        assert_eq!(
            selected[1].score.to_bits(),
            1,
            "no arbitrary positive-score floor"
        );
    }

    #[test]
    fn bounded_selection_matches_full_sort_for_ties_and_every_limit() {
        for rotation in 0..17 {
            let scores: BTreeMap<_, _> = (0_u32..257)
                .map(|number| {
                    (mem(u128::from(number + 1)), f64::from((number + rotation) % 17))
                })
                .collect();
            let seeds = BTreeSet::from([mem(17), mem(34)]);
            for limit in [0, 1, 2, 10, 128, 257, usize::MAX] {
                assert_eq!(
                    select(&current_or_testing_cx(), scores.clone(), &seeds, limit).unwrap(),
                    reference(&scores, &seeds, limit),
                );
            }
        }
    }

    #[test]
    fn large_positive_pool_keeps_only_the_requested_presentation_prefix() {
        let scores: BTreeMap<_, _> = (1_u32..=20_000)
            .map(|number| (mem(u128::from(number)), f64::from(number % 31)))
            .collect();
        let expected = reference(&scores, &BTreeSet::new(), 10);
        let mut observations = 0;
        let selected = select_with_checkpoint(scores, &BTreeSet::new(), 10, |retained| {
            observations += 1;
            assert!(retained <= 10);
            Ok(())
        })
        .unwrap();
        assert!(observations > 70, "long walks remain cancellation-aware");
        assert_eq!(selected, expected);
    }

    #[test]
    fn cancellation_during_selection_returns_no_prefix() {
        let scores: BTreeMap<_, _> = (1_u32..=1024)
            .map(|number| (mem(u128::from(number)), 0.5))
            .collect();
        let mut checkpoints = 0;
        let error = select_with_checkpoint(scores, &BTreeSet::new(), 10, |_| {
            checkpoints += 1;
            if checkpoints == 3 {
                Err(GraphError::AlgorithmCancelled {
                    algorithm: "pack_dna".to_owned(),
                    reason: "stop neighbor selection".to_owned(),
                })
            } else {
                Ok(())
            }
        })
        .expect_err("cancelled selections cannot publish their retained prefix");
        assert!(matches!(error, GraphError::AlgorithmCancelled { .. }));
        assert_eq!(checkpoints, 3);
    }

    #[test]
    fn empty_scores_and_zero_cap_do_not_allocate_from_the_requested_limit() {
        assert!(select(
            &current_or_testing_cx(),
            BTreeMap::new(),
            &BTreeSet::new(),
            usize::MAX,
        )
        .unwrap()
        .is_empty());
        let mut checks = 0;
        let result = select_with_checkpoint(
            BTreeMap::from([(mem(1), 1.0)]),
            &BTreeSet::new(),
            0,
            |retained| {
                checks += 1;
                assert_eq!(retained, 0);
                Ok(())
            },
        )
        .unwrap();
        assert!(result.is_empty());
        assert_eq!(checks, 1);
    }

    fn edge(graph: &mut DiGraph, source: u128, target: u128, relation: &str) {
        let mut attrs = AttrMap::new();
        attrs.insert("weight".to_owned(), CgseValue::Float(1.0));
        attrs.insert("confidence".to_owned(), CgseValue::Float(1.0));
        attrs.insert("relation".to_owned(), CgseValue::String(relation.to_owned()));
        graph
            .add_edge_with_attrs(&mem(source).to_string(), &mem(target).to_string(), attrs)
            .unwrap();
    }

    fn public_neighbors(block_second_edge: bool) -> Vec<PackDnaPprNeighbor> {
        let mut graph = DiGraph::strict();
        edge(&mut graph, 1, 2, "supports");
        edge(
            &mut graph,
            2,
            3,
            if block_second_edge { "contradicts" } else { "supports" },
        );
        edge(&mut graph, 4, 5, "supports");
        graph.add_node(mem(6).to_string());
        let projection = MemoryGraphProjection {
            node_count: graph.node_count(),
            edge_count: graph.edge_count(),
            graph,
            build_ms: 0.0,
            snapshot_version: 73,
        };
        let input = PackDnaInput::new(vec![mem(2), mem(5)], vec![mem(1)], vec![mem(1), mem(4)]);
        let dna = compute_pack_dna(&projection, &input).unwrap();
        assert!(
            dna.degraded.is_empty(),
            "the real PPR computation must complete"
        );
        assert_eq!(dna.snapshot_version, 73);
        assert_eq!(projection.graph.node_count(), 6);
        dna.ppr_neighbors
    }

    #[test]
    fn public_pack_dna_does_not_call_disconnected_or_isolated_nodes_neighbors() {
        let neighbors = public_neighbors(false);
        let ids: BTreeSet<_> = neighbors
            .iter()
            .map(|neighbor| neighbor.memory_id.clone())
            .collect();
        assert_eq!(
            ids,
            BTreeSet::from([mem(2).to_string(), mem(3).to_string()])
        );
        assert!(neighbors.iter().all(|neighbor| neighbor.score > 0.0));
    }

    #[test]
    fn topological_adjacency_without_positive_ppr_support_is_not_a_neighbor() {
        let neighbors = public_neighbors(true);
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].memory_id, mem(2).to_string());
        assert!(neighbors[0].score > 0.0);
    }
}
