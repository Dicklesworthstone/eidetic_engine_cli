//! The public adapter must not enlarge or weaken its caller's source selection.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "fixture decoding and test-only invariant checks"
)]

use std::collections::BTreeSet;

use super::tests::{link, memory};
use super::{StructuralGraphInput, build_structural_graph_input};
use crate::db::{StoredMemory, StoredMemoryLink};
use fnx_runtime::CgseValue;
use serde_json::json;

fn assert_consistent(input: &StructuralGraphInput, nodes: &[&str], pairs: &[(&str, &str)]) {
    assert_eq!(input.node_count, nodes.len());
    assert_eq!(input.node_count, input.graph.node_count());
    assert_eq!(input.edge_count, pairs.len());
    assert_eq!(input.edge_count, input.graph.edge_count());
    let expected_nodes: BTreeSet<_> = nodes.iter().copied().collect();
    for id in nodes {
        assert!(input.graph.has_node(id), "missing admitted node {id}");
    }
    let actual: Vec<_> = input
        .edges
        .iter()
        .map(|edge| (edge.a.as_str(), edge.b.as_str()))
        .collect();
    assert_eq!(actual, pairs);
    for edge in &input.edges {
        assert!(expected_nodes.contains(edge.a.as_str()));
        assert!(expected_nodes.contains(edge.b.as_str()));
        assert!(input.graph.has_edge(&edge.a, &edge.b));
        let attrs = input
            .graph
            .edge_attrs(&edge.a, &edge.b)
            .expect("edge attributes");
        assert_eq!(attrs.get("weight"), Some(&CgseValue::Float(edge.weight)));
        assert_eq!(
            attrs.get("confidence"),
            Some(&CgseValue::Float(edge.confidence))
        );
        assert_eq!(
            attrs.get("relation"),
            Some(&CgseValue::String(edge.relation.clone()))
        );
        assert_eq!(
            attrs.get("evidence_count"),
            Some(&CgseValue::Int(edge.evidence_count))
        );
    }
}

fn admitted_mesh() -> String {
    json!({
        "cachedMaterialId": "cached-fixture",
        "originWorkspaceId": "peer-workspace",
        "producerPeerId": "peer-fixture",
        "materialLane": "graphSignal",
        "trustLane": "peer",
        "redactionPosture": "screened",
        "workspaceScopeDecision": "allow"
    })
    .to_string()
}

#[test]
fn dangling_links_cannot_add_unselected_nodes_or_disagree_with_reported_counts() {
    let memories = [memory("a"), memory("isolated")];
    let links = [
        link("1", "a", "outside"),
        link("2", "outside", "a"),
        link("3", "outside", "other-outside"),
        link("4", "a", "a"),
    ];
    let input = build_structural_graph_input(&memories, &links, Some(7));
    assert_consistent(&input, &["a", "isolated"], &[]);
    assert!(input.is_empty());
    assert!(!input.graph.has_node("outside"));
    assert_eq!(input.snapshot_version, Some(7));
    let empty = build_structural_graph_input(&[], &links, None);
    assert_consistent(&empty, &[], &[]);
}

#[test]
fn tombstoned_memories_and_their_incident_links_are_withheld() {
    let mut retired = memory("retired");
    retired.tombstoned_at = Some("2026-01-02T00:00:00Z".to_owned());
    let memories = [memory("a"), retired, memory("b")];
    let links = [
        link("1", "a", "retired"),
        link("2", "retired", "b"),
        link("3", "b", "a"),
    ];
    let input = build_structural_graph_input(&memories, &links, None);
    assert_consistent(&input, &["a", "b"], &[("a", "b")]);
    assert!(!input.graph.has_node("retired"));
}

#[test]
fn a_cross_workspace_link_cannot_join_otherwise_admitted_components() {
    let mut foreign_a = memory("foreign-a");
    foreign_a.workspace_id = "another-workspace".to_owned();
    let mut foreign_b = memory("foreign-b");
    foreign_b.workspace_id = foreign_a.workspace_id.clone();
    let memories = [memory("a"), memory("b"), foreign_a, foreign_b];
    let links = [
        link("1", "a", "b"),
        link("2", "b", "foreign-a"),
        link("3", "foreign-b", "foreign-a"),
    ];
    let input = build_structural_graph_input(&memories, &links, None);
    assert_consistent(
        &input,
        &["a", "b", "foreign-a", "foreign-b"],
        &[("a", "b"), ("foreign-a", "foreign-b")],
    );
    assert!(!input.graph.has_edge("b", "foreign-a"));
}

#[test]
fn mesh_visibility_matches_the_canonical_projection_before_edge_selection() {
    let memories = [memory("a"), memory("b")];
    let allowed = admitted_mesh();
    let mut denied: serde_json::Value = serde_json::from_str(&allowed).unwrap();
    denied["workspaceScopeDecision"] = json!("deny");
    let mut malformed_policy: serde_json::Value = serde_json::from_str(&allowed).unwrap();
    malformed_policy["policyDecision"] = json!({"action": "allow"});
    for (metadata, visible) in [
        (None, true),
        (Some("{}".to_owned()), true),
        (Some(allowed), true),
        (Some(denied.to_string()), false),
        (Some(malformed_policy.to_string()), false),
        (
            Some("{\"cachedMaterialId\":\"missing-scope\"}".to_owned()),
            false,
        ),
        (Some("not-json".to_owned()), false),
    ] {
        assert_eq!(
            crate::graph::memory_link_mesh_metadata_visible(metadata.as_deref()),
            visible,
            "fixture must exercise the shared admission policy"
        );
        let mut row = link("1", "a", "b");
        row.metadata_json = metadata;
        let input = build_structural_graph_input(&memories, &[row], None);
        assert_consistent(
            &input,
            &["a", "b"],
            if visible { &[("a", "b")] } else { &[] },
        );
    }
}

#[test]
fn a_denied_lower_id_cannot_mask_an_allowed_parallel_relationship() {
    let memories = [memory("a"), memory("b")];
    let mut denied = link("0-denied", "a", "b");
    denied.metadata_json = Some("{\"cachedMaterialId\":\"hidden\"}".to_owned());
    denied.relation = "contradicts".to_owned();
    denied.weight = 1.0;
    let mut allowed = link("1-allowed", "b", "a");
    allowed.weight = 0.25;
    allowed.evidence_count = 17;
    for links in [
        [denied.clone(), allowed.clone()],
        [allowed.clone(), denied.clone()],
    ] {
        let input = build_structural_graph_input(&memories, &links, None);
        assert_consistent(&input, &["a", "b"], &[("a", "b")]);
        assert_eq!(input.edges[0].weight, 0.25);
        assert_eq!(input.edges[0].relation, allowed.relation);
        assert_eq!(input.edges[0].evidence_count, 17);
    }
}

#[test]
fn conflicting_duplicate_scope_or_liveness_cannot_be_selected_by_input_order() {
    for conflict in 0..3 {
        let good = memory("ambiguous");
        let mut bad = good.clone();
        match conflict {
            0 => bad.workspace_id = "another-workspace".to_owned(),
            1 => bad.tombstoned_at = Some("2026-01-02T00:00:00Z".to_owned()),
            _ => bad.workspace_id = " \t".to_owned(),
        }
        let links = [link("1", "ambiguous", "a")];
        for rows in [
            vec![good.clone(), bad.clone(), good.clone(), memory("a")],
            vec![bad.clone(), good.clone(), memory("a"), good.clone()],
        ] {
            let input = build_structural_graph_input(&rows, &links, None);
            assert_consistent(&input, &["a"], &[]);
            assert!(!input.graph.has_node("ambiguous"));
        }
    }
}

#[test]
fn coherent_duplicates_and_isolated_memories_remain_real_nodes() {
    let rows = [memory("a"), memory("isolated"), memory("b"), memory("a")];
    let links = [link("1", "a", "b")];
    let input = build_structural_graph_input(&rows, &links, None);
    assert_consistent(&input, &["a", "b", "isolated"], &[("a", "b")]);
    assert!(!input.is_empty());
}

#[test]
fn blank_identities_do_not_become_graph_nodes_or_join_blank_scopes() {
    let mut blank_scope = memory("unbound");
    blank_scope.workspace_id.clear();
    let memories = [memory(""), memory(" \t"), blank_scope, memory("a")];
    let links = [link("1", "a", ""), link("2", "unbound", "a")];
    assert_consistent(
        &build_structural_graph_input(&memories, &links, None),
        &["a"],
        &[],
    );
}

#[test]
fn nonfinite_edge_values_never_enter_graph_attributes_or_serialized_evidence() {
    let memories = [memory("a"), memory("b")];
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        for weight in [true, false] {
            let mut invalid = link("0-invalid", "a", "b");
            if weight {
                invalid.weight = value;
            } else {
                invalid.confidence = value;
            }
            let valid = link("1-valid", "a", "b");
            let input = build_structural_graph_input(&memories, &[invalid, valid], None);
            assert_consistent(&input, &["a", "b"], &[("a", "b")]);
            let encoded = serde_json::to_string(&input.edges).unwrap();
            assert!(!encoded.contains("null"));
            assert!(input.edges[0].weight.is_finite() && input.edges[0].confidence.is_finite());
        }
    }
}

#[test]
fn denied_edges_cannot_invent_a_structural_triangle() {
    let memories = [memory("a"), memory("b"), memory("c")];
    let mut closing = link("3", "a", "c");
    let open = [link("1", "a", "b"), link("2", "b", "c")];
    let full = build_structural_graph_input(
        &memories,
        &[open[0].clone(), open[1].clone(), closing.clone()],
        None,
    );
    assert_eq!(crate::graph::health::compute_k_truss(&full.graph).max_k, 3);
    closing.metadata_json =
        Some("{\"workspaceScopeDecision\":\"deny\",\"cachedMaterialId\":\"hidden\"}".to_owned());
    let filtered = build_structural_graph_input(
        &memories,
        &[open[0].clone(), open[1].clone(), closing],
        None,
    );
    assert_consistent(&filtered, &["a", "b", "c"], &[("a", "b"), ("b", "c")]);
    // Assert on MEMBERSHIP, not on max_k.
    //
    // `compute_k_truss` searches k >= 3 and floors `max_k` at 3, the bottom of
    // that range; it signals "no truss" by returning EMPTY `member_counts` and
    // `top_memories_at_k`. Two tests in src/graph/health.rs pin that
    // deliberately -- `k_truss_report_handles_empty_graph` and
    // `k_truss_report_does_not_promote_path_without_triangles` both assert
    // max_k == 3 for graphs containing no triangle at all.
    //
    // So `max_k < 3` could never hold, and this test was born red against an
    // established contract rather than catching a defect. Changing the floor to
    // satisfy it reds both of those tests instead, which is how the contract
    // was confirmed.
    //
    // The claim this test exists to make is that a DENIED edge cannot invent a
    // triangle, and emptiness is exactly that claim. `assert_consistent` above
    // has already established the graph is the 2-edge path, so this is not
    // vacuous: if the denied edge were admitted, a-b-c-a would close and
    // member_counts would carry a k=3 entry.
    let truss = crate::graph::health::compute_k_truss(&filtered.graph);
    assert!(
        truss.member_counts.is_empty(),
        "a denied edge must not create any k-truss, got {:?}",
        truss.member_counts
    );
    assert!(
        truss.top_memories_at_k.is_empty(),
        "a denied edge must not promote any memory into a truss, got {:?}",
        truss.top_memories_at_k
    );
}

#[test]
fn permutations_preserve_admitted_topology_and_canonical_attributes() {
    let mut retired = memory("retired");
    retired.tombstoned_at = Some("2026-01-02T00:00:00Z".to_owned());
    let rows = [memory("a"), memory("b"), memory("c"), retired];
    let mut denied = link("0", "a", "b");
    denied.metadata_json = Some("{\"cachedMaterialId\":\"hidden\"}".to_owned());
    let links = [
        denied,
        link("3", "b", "a"),
        link("2", "a", "b"),
        link("4", "b", "c"),
        link("5", "a", "retired"),
        link("6", "c", "outside"),
    ];
    let expected = build_structural_graph_input(&rows, &links, Some(7));
    fn permutations<T: Clone>(values: &[T]) -> Vec<Vec<T>> {
        if values.is_empty() {
            return vec![Vec::new()];
        }
        let mut result = Vec::new();
        for index in 0..values.len() {
            let rest: Vec<_> = values
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, value)| value.clone())
                .collect();
            for mut tail in permutations(&rest) {
                tail.insert(0, values[index].clone());
                result.push(tail);
            }
        }
        result
    }
    for reordered in permutations(&links) {
        let input = build_structural_graph_input(&rows, &reordered, Some(7));
        assert_eq!(input.edges, expected.edges);
        assert_consistent(&input, &["a", "b", "c"], &[("a", "b"), ("b", "c")]);
    }
    for reordered in permutations(&rows) {
        let input = build_structural_graph_input(&reordered, &links, Some(7));
        assert_eq!(input.edges, expected.edges);
    }
}

#[test]
fn selection_is_read_only_and_does_not_rewrite_source_or_advisory_snapshot() {
    let memories = [memory("a"), memory("b")];
    let mut row = link("1", "b", "a");
    row.metadata_json = Some(admitted_mesh());
    let memory_state = |rows: &[StoredMemory]| {
        rows.iter()
            .map(|row| {
                (
                    row.id.clone(),
                    row.workspace_id.clone(),
                    row.content.clone(),
                    row.tombstoned_at.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    let link_state = |rows: &[StoredMemoryLink]| {
        rows.iter()
            .map(|row| {
                (
                    row.id.clone(),
                    row.src_memory_id.clone(),
                    row.dst_memory_id.clone(),
                    row.metadata_json.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    let links = [row];
    let before_memories = memory_state(&memories);
    let before_links = link_state(&links);
    let first = build_structural_graph_input(&memories, &links, Some(123));
    let second = build_structural_graph_input(&memories, &links, Some(123));
    assert_eq!(first.edges, second.edges);
    assert_eq!(first.snapshot_version, Some(123));
    assert_eq!(memory_state(&memories), before_memories);
    assert_eq!(link_state(&links), before_links);
}
