use std::collections::BTreeSet;

use super::{
    GuardedMemory, SuppressionBasis, decide_contradiction_suppressions,
    decide_contradiction_survivor,
};

fn memory(id: &str, trust: i64, freshness: i64) -> GuardedMemory {
    GuardedMemory {
        memory_id: id.to_owned(),
        trust_milli: trust,
        freshness_epoch: freshness,
    }
}

fn pairs(edges: &[(&str, &str)]) -> Vec<(String, String)> {
    edges
        .iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

#[test]
fn a_suppressed_middle_node_cannot_suppress_compatible_evidence() {
    let members = vec![memory("a", 900, 1), memory("b", 500, 1), memory("c", 100, 1)];
    let edges = pairs(&[("b", "c"), ("a", "b")]);
    let decisions = decide_contradiction_suppressions(&members, &edges);
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0], decide_contradiction_survivor(&members[0], &members[1]));

    for order in [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
        let reordered: Vec<_> = order.iter().map(|&index| members[index].clone()).collect();
        for reverse_edges in [false, true] {
            for endpoint_mask in 0..4 {
                let mut variant = edges.clone();
                for (index, edge) in variant.iter_mut().enumerate() {
                    if endpoint_mask & (1 << index) != 0 {
                        std::mem::swap(&mut edge.0, &mut edge.1);
                    }
                }
                if reverse_edges {
                    variant.reverse();
                }
                assert_eq!(decide_contradiction_suppressions(&reordered, &variant), decisions);
            }
        }
    }
}

#[test]
fn retained_witness_is_the_strongest_retained_neighbor() {
    let members = vec![memory("a", 900, 1), memory("b", 700, 1), memory("c", 100, 1)];
    let edges = pairs(&[("b", "c"), ("a", "c")]);
    let decisions = decide_contradiction_suppressions(&members, &edges);
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].kept_memory_id, "a");
    assert_eq!(decisions[0].suppressed_memory_id, "c");
}

#[test]
fn graph_priority_preserves_pairwise_trust_freshness_and_id_semantics() {
    for members in [
        vec![memory("a", i64::MAX, i64::MIN), memory("b", i64::MIN, i64::MAX)],
        vec![memory("a", 500, 20), memory("b", 500, 10)],
        vec![memory("a", 500, 10), memory("b", 500, 10)],
    ] {
        let expected = decide_contradiction_survivor(&members[0], &members[1]);
        assert_eq!(
            decide_contradiction_suppressions(&members, &pairs(&[("b", "a")])),
            vec![expected]
        );
    }
    let tied = vec![memory("b", 500, 10), memory("a", 500, 10), memory("c", 500, 10)];
    let decisions = decide_contradiction_suppressions(
        &tied,
        &pairs(&[("b", "c"), ("a", "c"), ("b", "a")]),
    );
    assert_eq!(decisions.len(), 2);
    assert!(decisions.iter().all(|decision| {
        decision.kept_memory_id == "a" && decision.basis == SuppressionBasis::DeterministicTieBreak
    }));
}

#[test]
fn duplicate_and_invalid_pairs_do_not_invent_suppressions() {
    let members = vec![memory("a", 900, 1), memory("b", 100, 1)];
    let expected = decide_contradiction_suppressions(&members, &pairs(&[("a", "b")]));
    let noisy = pairs(&[
        ("a", "a"),
        ("a", "unknown"),
        ("", "b"),
        ("  ", "a"),
        ("b", "a"),
        (" a ", " b "),
        ("a", "b"),
    ]);
    assert_eq!(decide_contradiction_suppressions(&members, &noisy), expected);
    assert!(decide_contradiction_suppressions(&members, &pairs(&[("a", "a")])).is_empty());
    assert!(decide_contradiction_suppressions(&[], &noisy).is_empty());
    assert!(decide_contradiction_suppressions(&members, &[]).is_empty());
}

#[test]
fn repeated_member_ids_use_their_strongest_standing_independent_of_order() {
    let mut members = vec![memory("a", 100, 1), memory("b", 500, 1), memory("a", 900, 1)];
    let edges = pairs(&[("b", "a")]);
    let expected = decide_contradiction_suppressions(&members, &edges);
    assert_eq!(expected.len(), 1);
    assert_eq!(expected[0].kept_memory_id, "a");
    members.reverse();
    assert_eq!(decide_contradiction_suppressions(&members, &edges), expected);
}

#[test]
fn disconnected_components_do_not_drop_isolated_or_compatible_members() {
    let members = vec![
        memory("a", 900, 1),
        memory("b", 500, 1),
        memory("c", 100, 1),
        memory("d", 900, 1),
        memory("e", 100, 1),
        memory("isolated", 0, 1),
    ];
    let decisions = decide_contradiction_suppressions(
        &members,
        &pairs(&[("b", "c"), ("d", "e"), ("a", "b")]),
    );
    let suppressed: Vec<_> = decisions.iter().map(|d| d.suppressed_memory_id.as_str()).collect();
    assert_eq!(suppressed, vec!["b", "e"]);
}

#[test]
fn all_five_member_graphs_match_an_independent_subset_oracle() {
    const N: usize = 5;
    let members: Vec<_> = (0..N)
        .map(|index| memory(&format!("m{index}"), i64::try_from(N - index).unwrap(), 1))
        .collect();
    let possible_edges: Vec<_> = (0..N)
        .flat_map(|a| ((a + 1)..N).map(move |b| (a, b)))
        .collect();
    for graph in 0..(1_usize << possible_edges.len()) {
        let edges: Vec<_> = possible_edges
            .iter()
            .enumerate()
            .filter(|(bit, _)| graph & (1 << bit) != 0)
            .map(|(_, &(a, b))| (members[a].memory_id.clone(), members[b].memory_id.clone()))
            .collect();
        let decisions = decide_contradiction_suppressions(&members, &edges);
        let suppressed: BTreeSet<_> = decisions
            .iter()
            .map(|decision| decision.suppressed_memory_id.as_str())
            .collect();
        let retained: Vec<_> = members
            .iter()
            .filter(|member| !suppressed.contains(member.memory_id.as_str()))
            .cloned()
            .collect();
        let retained_ids: BTreeSet<_> = retained.iter().map(|m| m.memory_id.as_str()).collect();
        for decision in &decisions {
            assert!(retained_ids.contains(decision.kept_memory_id.as_str()));
            assert!(edges.iter().any(|(a, b)| {
                (a == &decision.kept_memory_id && b == &decision.suppressed_memory_id)
                    || (b == &decision.kept_memory_id && a == &decision.suppressed_memory_id)
            }));
        }
        assert!(decide_contradiction_suppressions(&retained, &edges).is_empty());

        // Highest bit represents the strongest memory. Exhaustively maximizing
        // this mask implements the priority contract, not maximum item count.
        let best_mask = (0..(1_usize << N))
            .filter(|subset| {
                possible_edges.iter().enumerate().all(|(bit, &(a, b))| {
                    graph & (1 << bit) == 0
                        || subset & (1 << (N - 1 - a)) == 0
                        || subset & (1 << (N - 1 - b)) == 0
                })
            })
            .max()
            .unwrap();
        let actual_mask = members.iter().enumerate().fold(0_usize, |mask, (index, member)| {
            if retained_ids.contains(member.memory_id.as_str()) {
                mask | (1 << (N - 1 - index))
            } else {
                mask
            }
        });
        assert_eq!(actual_mask, best_mask, "graph mask {graph}");

        let reversed_members: Vec<_> = members.iter().rev().cloned().collect();
        let reversed_edges: Vec<_> = edges
            .iter()
            .rev()
            .map(|(a, b)| (b.clone(), a.clone()))
            .collect();
        assert_eq!(
            decide_contradiction_suppressions(&reversed_members, &reversed_edges),
            decisions,
            "graph mask {graph}"
        );
    }
}

#[test]
fn canonical_edge_order_must_not_override_trust_priority() {
    // This is already the sorted detector order, not malformed input. The old
    // pair loop let B suppress A before C suppressed B, discarding valid A.
    let members = vec![memory("a", 100, 1), memory("b", 500, 1), memory("c", 900, 1)];
    let edges = pairs(&[("a", "b"), ("b", "c")]);
    let decisions = decide_contradiction_suppressions(&members, &edges);
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].kept_memory_id, "c");
    assert_eq!(decisions[0].suppressed_memory_id, "b");
}
