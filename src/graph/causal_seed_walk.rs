//! Exact nearest-ancestor selection for bounded causal PPR pre-warming.
//!
//! Complete ancestry and cycle diagnostics belong to the explanation API. A
//! top-k boost request must not build that complete graph just to discard all
//! but its nearest k ancestors.

use std::collections::BTreeSet;

use super::{CausalAncestor, DiGraph, compare_ulid_payload_or_lexical};

pub(super) fn nearest_ancestors(
    graph: &DiGraph,
    source: &str,
    limit: usize,
) -> Vec<CausalAncestor> {
    if limit == 0 || !graph.has_node(source) {
        return Vec::new();
    }
    walk(source, limit, |id| graph.successors(id).unwrap_or_default())
}

fn walk<'a>(
    source: &str,
    limit: usize,
    mut successors: impl FnMut(&str) -> Vec<&'a str>,
) -> Vec<CausalAncestor> {
    let mut ancestors = Vec::new();
    let mut visited = BTreeSet::new();
    visited.insert(source.to_owned());
    let mut frontier = vec![source.to_owned()];
    let mut depth = 0_usize;

    while ancestors.len() < limit && !frontier.is_empty() {
        depth = depth.saturating_add(1);
        let remaining = limit - ancestors.len();
        let mut next: Vec<String> = Vec::new();
        for current in frontier {
            for successor in successors(&current) {
                if visited.contains(successor) {
                    continue;
                }
                // Keep the best remaining IDs across the ENTIRE level, not
                // merely the first parent's children. This is the same total
                // ULID/legacy-ID ordering as complete causal ancestry.
                if let Err(index) = next.binary_search_by(|candidate| {
                    compare_ulid_payload_or_lexical(candidate, successor)
                }) {
                    if index < remaining {
                        next.insert(index, successor.to_owned());
                        if next.len() > remaining {
                            let _ = next.pop();
                        }
                    }
                }
            }
        }
        for id in &next {
            visited.insert(id.clone());
            ancestors.push(CausalAncestor {
                memory_id: id.clone(),
                path_length: depth,
            });
        }
        // A truncated level fills the result, so discarded nodes are never
        // needed at a later depth. Otherwise this is the complete next level.
        frontier = next;
    }

    ancestors
}

#[cfg(test)]
mod tests {
    use super::*;
    use fnx_runtime::CompatibilityMode;

    fn graph() -> DiGraph {
        DiGraph::new(CompatibilityMode::Strict)
    }

    fn edge(graph: &mut DiGraph, source: &str, target: &str) {
        graph
            .add_edge_with_attrs(source, target, Default::default())
            .expect("valid test edge");
    }

    #[test]
    fn bounded_walk_matches_complete_ancestry_for_every_three_node_graph() {
        let ids = ["a", "b", "c"];
        for mask in 0_u16..512 {
            let mut graph = graph();
            for id in ids {
                graph.add_node(id);
            }
            for (left, source) in ids.iter().enumerate() {
                for (right, target) in ids.iter().enumerate() {
                    if mask & (1 << (left * ids.len() + right)) != 0 {
                        edge(&mut graph, source, target);
                    }
                }
            }
            for source in ids {
                let complete =
                    super::super::compute_causal_ancestry_unbudgeted(&graph, source).ancestors;
                for limit in 0..=4 {
                    let expected = complete.iter().take(limit).cloned().collect::<Vec<_>>();
                    assert_eq!(
                        nearest_ancestors(&graph, source, limit),
                        expected,
                        "mask={mask}, source={source}, limit={limit}"
                    );
                }
            }
        }
    }

    #[test]
    fn last_level_is_ranked_globally_across_parents() {
        let mut graph = graph();
        for (source, target) in [
            ("failure", "a"),
            ("failure", "b"),
            ("a", "z"),
            ("b", "c"),
            ("b", "z"),
        ] {
            edge(&mut graph, source, target);
        }
        let nearest = nearest_ancestors(&graph, "failure", 3);
        let ids = nearest
            .iter()
            .map(|entry| entry.memory_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!(nearest[2].path_length, 2);
    }

    #[test]
    fn canonical_and_legacy_ties_match_the_public_ordering() {
        let mut graph = graph();
        for target in [
            "legacy",
            "rule_01J0000000000000000000000C",
            "note_01J0000000000000000000000A",
            "mem_01J0000000000000000000000A",
            "mem_01J0000000000000000000000B",
        ] {
            edge(&mut graph, "failure", target);
        }
        let complete =
            super::super::compute_causal_ancestry_unbudgeted(&graph, "failure").ancestors;
        for limit in 0..=6 {
            assert_eq!(
                nearest_ancestors(&graph, "failure", limit),
                complete.iter().take(limit).cloned().collect::<Vec<_>>()
            );
        }
        assert_eq!(complete[0].memory_id, "mem_01J0000000000000000000000A");
        assert_eq!(complete[1].memory_id, "note_01J0000000000000000000000A");
    }

    #[test]
    fn long_chain_stops_reading_successors_at_the_requested_cap() {
        let ids = (0..10_000)
            .map(|index| format!("node_{index}"))
            .collect::<Vec<_>>();
        let mut reads = 0;
        let nearest = walk(&ids[0], 16, |id| {
            reads += 1;
            let index = id
                .strip_prefix("node_")
                .expect("fixture ID")
                .parse::<usize>()
                .expect("fixture index");
            ids.get(index + 1)
                .map(|next| vec![next.as_str()])
                .unwrap_or_default()
        });
        assert_eq!(reads, 16, "must not scan the other 9,983 nodes");
        assert_eq!(nearest.len(), 16);
        assert_eq!(nearest[15].memory_id, "node_16");
        assert_eq!(nearest[15].path_length, 16);
    }

    #[test]
    fn wide_frontier_does_not_expand_nodes_beyond_the_cutoff() {
        let children = (0..128)
            .map(|index| format!("child_{index:03}"))
            .collect::<Vec<_>>();
        let mut reads = 0;
        let nearest = walk("failure", 4, |id| {
            reads += 1;
            assert_eq!(id, "failure", "a full last level must not be expanded");
            children.iter().rev().map(String::as_str).collect()
        });
        assert_eq!(reads, 1);
        assert_eq!(nearest.len(), 4);
        assert_eq!(nearest[0].memory_id, "child_000");
        assert_eq!(nearest[3].memory_id, "child_003");
    }

    #[test]
    fn zero_cap_missing_source_and_self_loop_are_empty() {
        assert!(walk("failure", 0, |_| panic!("zero cap must not read adjacency")).is_empty());
        let mut graph = graph();
        edge(&mut graph, "failure", "failure");
        assert!(nearest_ancestors(&graph, "failure", 16).is_empty());
        assert!(nearest_ancestors(&graph, "missing", 16).is_empty());
    }
}
