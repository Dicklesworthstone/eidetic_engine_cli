//! Source identity and deterministic representative selection through the live adapter.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test-only graph invariant checks"
)]

use std::collections::BTreeMap;

use super::tests::{link, memory};
use super::{StructuralGraphInput, build_structural_graph_input};
use crate::db::StoredMemoryLink;
use fnx_runtime::CgseValue;

fn project(links: &[StoredMemoryLink]) -> StructuralGraphInput {
    let input =
        build_structural_graph_input(&[memory("a"), memory("b"), memory("c")], links, Some(19));
    assert_eq!(input.node_count, 3);
    assert_eq!(input.node_count, input.graph.node_count());
    assert_eq!(input.edge_count, input.graph.edge_count());
    assert_eq!(input.edge_count, input.edges.len());
    assert_eq!(input.snapshot_version, Some(19));
    input
}

fn assert_refused(original: &StoredMemoryLink, conflicting: &StoredMemoryLink) {
    for links in [
        vec![original.clone(), conflicting.clone()],
        vec![conflicting.clone(), original.clone()],
        vec![original.clone(), conflicting.clone(), original.clone()],
        vec![conflicting.clone(), original.clone(), conflicting.clone()],
    ] {
        let input = project(&links);
        assert!(input.edges.is_empty());
        assert!(input.is_empty());
    }
}

#[test]
fn equivalent_repeated_source_rows_retain_one_unchanged_relationship() {
    let mut original = link("source", "b", "a");
    original.weight = 0.25;
    original.evidence_count = 37;
    original.metadata_json = Some("{}".to_owned());
    let expected = project(std::slice::from_ref(&original));
    let repeated = vec![original.clone(); 20_000];
    let input = project(&repeated);
    assert_eq!(input.edges, expected.edges);
    assert_eq!(input.edges.len(), 1);
    assert_eq!(input.edges[0].evidence_count, 37);
    assert_eq!(input.edges[0].weight, 0.25);
    assert_eq!(repeated[0].metadata_json, original.metadata_json);
}

#[test]
fn every_persisted_source_field_participates_in_duplicate_identity_checks() {
    let original = link("source", "a", "b");
    for field in 0..12 {
        let mut changed = original.clone();
        match field {
            0 => changed.src_memory_id = "c".to_owned(),
            1 => changed.dst_memory_id = "c".to_owned(),
            2 => changed.relation = "contradicts".to_owned(),
            3 => changed.weight = 0.25,
            4 => changed.confidence = 0.5,
            5 => changed.directed = !changed.directed,
            6 => changed.evidence_count += 1,
            7 => changed.last_reinforced_at = Some("2026-01-02T00:00:00Z".to_owned()),
            8 => changed.source = "agent".to_owned(),
            9 => changed.created_at = "2026-01-02T00:00:00Z".to_owned(),
            10 => changed.created_by = Some("another-agent".to_owned()),
            _ => changed.metadata_json = Some("{}".to_owned()),
        }
        assert_refused(&original, &changed);
    }
}

#[test]
fn a_single_source_identity_cannot_claim_two_pairs_or_reverse_its_endpoints() {
    let original = link("source", "a", "b");
    assert_refused(&original, &link("source", "a", "c"));
    assert_refused(&original, &link("source", "b", "a"));
    assert_refused(&original, &link("source", "outside", "b"));
}

#[test]
fn a_visible_twin_cannot_launder_a_denied_version_of_the_same_source() {
    let original = link("source", "a", "b");
    let mut denied = original.clone();
    denied.metadata_json = Some("{\"cachedMaterialId\":\"missing-scope\"}".to_owned());
    assert_refused(&original, &denied);
    // Another admitted SOURCE ID remains independent, even for the same pair.
    let mut alternative = link("z-alternative", "b", "a");
    alternative.weight = 0.125;
    let input = project(&[original, denied, alternative]);
    assert_eq!(input.edges.len(), 1);
    assert_eq!(input.edges[0].weight, 0.125);
}

#[test]
fn canonical_pair_winners_keep_lexical_source_id_order_not_arrival_order() {
    let mut first = link("10", "a", "b");
    first.evidence_count = 10;
    let mut second = link("2", "b", "a");
    second.evidence_count = 2;
    let mut third = link("1", "b", "c");
    third.evidence_count = 1;
    for links in [
        vec![first.clone(), second.clone(), third.clone()],
        vec![third.clone(), second.clone(), first.clone()],
        vec![second.clone(), first.clone(), third.clone(), first.clone()],
    ] {
        let input = project(&links);
        let observed: Vec<_> = input
            .edges
            .iter()
            .map(|edge| (edge.a.as_str(), edge.b.as_str(), edge.evidence_count))
            .collect();
        assert_eq!(observed, vec![("a", "b", 10), ("b", "c", 1)]);
        let attrs = input.graph.edge_attrs("a", "b").unwrap();
        assert_eq!(attrs.get("evidence_count"), Some(&CgseValue::Int(10)));
    }
}

#[test]
fn blank_source_ids_cannot_supply_structural_edges() {
    for id in ["", " ", "\t\n"] {
        assert!(project(&[link(id, "a", "b")]).edges.is_empty());
    }
}

#[test]
fn identical_nan_records_and_conflicting_float_bits_never_supply_edges() {
    let original = link("source", "a", "b");
    let mut nan = original.clone();
    nan.weight = f32::NAN;
    assert!(project(&[nan.clone(), nan.clone()]).edges.is_empty());
    assert_refused(&original, &nan);
    let mut positive_zero = original.clone();
    positive_zero.weight = 0.0;
    let mut negative_zero = positive_zero.clone();
    negative_zero.weight = -0.0;
    assert_refused(&positive_zero, &negative_zero);
    assert_eq!(
        project(&[positive_zero.clone(), positive_zero]).edges.len(),
        1
    );
}

#[test]
fn metadata_bytes_and_opaque_source_identity_are_not_normalized_into_agreement() {
    let mut first = link("source", "a", "b");
    first.metadata_json = Some("{}".to_owned());
    let mut different = first.clone();
    different.metadata_json = Some("{ }".to_owned());
    assert_refused(&first, &different);
    // Case is part of a source identity, not an alias or a disagreement.
    different.id = "Source".to_owned();
    different.evidence_count = 7;
    let input = project(&[first, different]);
    assert_eq!(input.edges.len(), 1);
    assert_eq!(input.edges[0].evidence_count, 7); // "Source" sorts before "source".
}

#[test]
fn all_four_source_state_combinations_match_an_independent_admission_oracle() {
    // Each source is absent, singly present, repeated coherently, conflicting,
    // denied, or conflicting across endpoint pairs. Only cases 1/2 qualify.
    let pairs = [("a", "b"), ("b", "c"), ("a", "c"), ("b", "a")];
    for case in 0..6_usize.pow(4) {
        let mut code = case;
        let mut links = Vec::new();
        let mut expected = BTreeMap::new();
        for (index, (src, dst)) in pairs.iter().enumerate() {
            let state = code % 6;
            code /= 6;
            let mut original = link(&format!("source-{index}"), src, dst);
            original.evidence_count = u32::try_from(index + 1).unwrap();
            if matches!(state, 1 | 2) {
                let pair = if src < dst {
                    (*src, *dst)
                } else {
                    (*dst, *src)
                };
                expected.entry(pair).or_insert(original.evidence_count);
            }
            match state {
                0 => {}
                1 => links.push(original),
                2 => links.extend([original.clone(), original]),
                3 => {
                    let mut different = original.clone();
                    different.weight = 0.25;
                    links.extend([original.clone(), different, original]);
                }
                4 => {
                    original.metadata_json = Some("{\"cachedMaterialId\":\"hidden\"}".to_owned());
                    links.push(original);
                }
                _ => {
                    let mut different = original.clone();
                    different.src_memory_id = "outside".to_owned();
                    links.extend([different, original]);
                }
            }
        }
        for _ in 0..2 {
            let input = project(&links);
            let actual: BTreeMap<_, _> = input
                .edges
                .iter()
                .map(|edge| ((edge.a.as_str(), edge.b.as_str()), edge.evidence_count))
                .collect();
            let expected_i64: BTreeMap<_, _> = expected
                .iter()
                .map(|(&pair, &count)| (pair, i64::from(count)))
                .collect();
            assert_eq!(actual, expected_i64, "source state {case}");
            links.reverse();
        }
    }
}
