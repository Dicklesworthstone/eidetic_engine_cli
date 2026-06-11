//! bd-2pos6.7 — shared structural-graph adapter for insights sections.
//!
//! kCore and kTruss (and any future structural section) need the same
//! source facts: the workspace memory set, the memory-link projection
//! with the SAME edge conventions the `ee graph` command handlers use
//! (weight / confidence / relation / source / evidence_count attrs,
//! undirected membership semantics), and deterministic, support-bundle
//! safe metadata. This adapter builds that input ONCE so the section
//! builders never invent their own graph loaders.
//!
//! The adapter deliberately stops at data shaping: kCore still calls
//! `fnx_algorithms::k_core` and kTruss still calls
//! [`crate::graph::health::compute_k_truss`] on the
//! [`StructuralGraphInput::graph`] this returns. No algorithm runs here.

use fnx_classes::{AttrMap, Graph};
use fnx_runtime::CgseValue;
use serde::Serialize;

use crate::db::{StoredMemory, StoredMemoryLink};

/// Evidence schema id stamped on structural-section items built from
/// this adapter.
pub const STRUCTURAL_GRAPH_EVIDENCE_SCHEMA: &str = "ee.insights.structural_graph.v1";
/// The only graph type this adapter projects.
pub const STRUCTURAL_GRAPH_TYPE: &str = "memory_links";
/// Filter posture: the insights loader supplies workspace-scoped,
/// tombstone-filtered memories and links; the adapter additionally
/// canonicalizes undirected duplicates.
pub const STRUCTURAL_GRAPH_FILTER_POSTURE: &str =
    "workspace_scoped_tombstone_filtered_canonical_undirected";

/// One canonical undirected edge after duplicate/reverse collapsing.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuralGraphEdge {
    /// Lexically smaller endpoint (canonical order).
    pub a: String,
    /// Lexically larger endpoint.
    pub b: String,
    pub weight: f64,
    pub confidence: f64,
    pub relation: String,
    pub evidence_count: i64,
}

/// Deterministic structural-graph input for insights section builders.
pub struct StructuralGraphInput {
    /// Undirected projection with the shared edge-attr conventions; pass
    /// to `fnx_algorithms::k_core` / `compute_k_truss` directly.
    pub graph: Graph,
    /// Every workspace memory id is a node (isolated memories count:
    /// core/truss number 0 is a real finding, not missing data).
    pub node_count: usize,
    /// Canonical undirected edge count after dedup.
    pub edge_count: usize,
    /// Canonical edges in deterministic order (evidence + tests).
    pub edges: Vec<StructuralGraphEdge>,
    /// Latest memory_links graph snapshot version when one exists.
    pub snapshot_version: Option<u64>,
    pub graph_type: &'static str,
    pub filter_posture: &'static str,
    pub evidence_schema: &'static str,
}

impl StructuralGraphInput {
    /// True when there is nothing structural to report. Section builders
    /// must emit their precise no-data degradation instead of fabricating
    /// items.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.node_count == 0 || self.edge_count == 0
    }
}

/// Build the shared structural input from already-loaded workspace data.
///
/// Projection semantics match the `ee graph` command handlers: every
/// link contributes one undirected edge carrying weight / confidence /
/// relation / source / evidence_count attrs. On top of that, duplicate
/// and reversed links collapse onto one canonical `(min, max)` edge —
/// links are processed in a deterministic order (canonical pair, then
/// link id) and the first canonical occurrence wins, so the projection
/// is byte-stable regardless of input order.
#[must_use]
pub fn build_structural_graph_input(
    memories: &[StoredMemory],
    links: &[StoredMemoryLink],
    snapshot_version: Option<u64>,
) -> StructuralGraphInput {
    let mut graph = Graph::strict();
    let mut node_ids: Vec<&str> = memories.iter().map(|memory| memory.id.as_str()).collect();
    node_ids.sort_unstable();
    node_ids.dedup();
    for id in &node_ids {
        graph.add_node((*id).to_owned());
    }

    // Deterministic processing order: canonical endpoint pair, then link
    // id, so reversed/duplicate links always resolve identically.
    let mut ordered: Vec<&StoredMemoryLink> = links.iter().collect();
    ordered.sort_by(|left, right| {
        canonical_pair(left)
            .cmp(&canonical_pair(right))
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut edges: Vec<StructuralGraphEdge> = Vec::new();
    for link in ordered {
        let (a, b) = canonical_pair(link);
        if a == b {
            // Self-links carry no structural membership signal.
            continue;
        }
        if edges
            .last()
            .is_some_and(|edge| edge.a == a && edge.b == b)
        {
            // Duplicate or reversed of the previous canonical edge.
            continue;
        }
        let mut attrs = AttrMap::new();
        attrs.insert("weight", CgseValue::Float(f64::from(link.weight)));
        attrs.insert("confidence", CgseValue::Float(f64::from(link.confidence)));
        attrs.insert("relation", CgseValue::String(link.relation.clone()));
        attrs.insert("source", CgseValue::String(link.source.clone()));
        attrs.insert(
            "evidence_count",
            CgseValue::Int(i64::from(link.evidence_count)),
        );
        if graph
            .add_edge_with_attrs(a.to_owned(), b.to_owned(), attrs)
            .is_err()
        {
            // Strict graphs reject duplicate edges; the dedup above makes
            // this unreachable for same-pair repeats, but a rejected edge
            // must never abort the whole projection.
            continue;
        }
        edges.push(StructuralGraphEdge {
            a: a.to_owned(),
            b: b.to_owned(),
            weight: f64::from(link.weight),
            confidence: f64::from(link.confidence),
            relation: link.relation.clone(),
            evidence_count: i64::from(link.evidence_count),
        });
    }

    StructuralGraphInput {
        node_count: node_ids.len(),
        edge_count: edges.len(),
        edges,
        graph,
        snapshot_version,
        graph_type: STRUCTURAL_GRAPH_TYPE,
        filter_posture: STRUCTURAL_GRAPH_FILTER_POSTURE,
        evidence_schema: STRUCTURAL_GRAPH_EVIDENCE_SCHEMA,
    }
}

fn canonical_pair(link: &StoredMemoryLink) -> (&str, &str) {
    let src = link.src_memory_id.as_str();
    let dst = link.dst_memory_id.as_str();
    if src <= dst { (src, dst) } else { (dst, src) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(id: &str) -> StoredMemory {
        StoredMemory {
            id: id.to_owned(),
            workspace_id: "wsp_structural00000000000001".to_owned(),
            level: "semantic".to_owned(),
            kind: "fact".to_owned(),
            content: "structural fixture".to_owned(),
            workflow_id: None,
            confidence: 0.5,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: None,
            trust_class: "human_explicit".to_owned(),
            trust_subclass: None,
            provenance_chain_hash: None,
            provenance_chain_hash_version: "ee.memory.provenance_chain.v1".to_owned(),
            provenance_verification_status: "verified".to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            updated_at: "2026-01-01T00:00:00Z".to_owned(),
            tombstoned_at: None,
            valid_from: None,
            valid_to: None,
        }
    }

    fn link(id: &str, src: &str, dst: &str) -> StoredMemoryLink {
        StoredMemoryLink {
            id: id.to_owned(),
            src_memory_id: src.to_owned(),
            dst_memory_id: dst.to_owned(),
            relation: "supports".to_owned(),
            weight: 0.8,
            confidence: 0.7,
            directed: false,
            evidence_count: 2,
            last_reinforced_at: None,
            source: "auto".to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            created_by: None,
            metadata_json: None,
        }
    }

    /// Empty workspace: zero nodes/edges, deterministic metadata, no panic.
    #[test]
    fn empty_workspace_produces_empty_deterministic_input() {
        let input = build_structural_graph_input(&[], &[], None);
        assert_eq!(input.node_count, 0);
        assert_eq!(input.edge_count, 0);
        assert!(input.edges.is_empty());
        assert!(input.is_empty());
        assert_eq!(input.graph_type, STRUCTURAL_GRAPH_TYPE);
        assert_eq!(input.filter_posture, STRUCTURAL_GRAPH_FILTER_POSTURE);
        assert_eq!(input.evidence_schema, STRUCTURAL_GRAPH_EVIDENCE_SCHEMA);
        assert_eq!(input.snapshot_version, None);
    }

    /// Three-node triangle: stable node and edge ordering, metadata, and
    /// a graph the k-truss path accepts.
    #[test]
    fn triangle_projection_is_stable_and_truss_compatible() {
        let memories = [memory("mem_a"), memory("mem_c"), memory("mem_b")];
        let links = [
            link("lnk_3", "mem_c", "mem_a"),
            link("lnk_1", "mem_a", "mem_b"),
            link("lnk_2", "mem_b", "mem_c"),
        ];
        let input = build_structural_graph_input(&memories, &links, Some(7));
        assert_eq!(input.node_count, 3);
        assert_eq!(input.edge_count, 3);
        assert_eq!(input.snapshot_version, Some(7));
        let pairs: Vec<(&str, &str)> = input
            .edges
            .iter()
            .map(|edge| (edge.a.as_str(), edge.b.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![("mem_a", "mem_b"), ("mem_a", "mem_c"), ("mem_b", "mem_c")],
            "edges must emit in canonical deterministic order"
        );
        assert!(!input.is_empty());

        let truss = crate::graph::health::compute_k_truss(&input.graph);
        assert_eq!(truss.max_k, 3, "a triangle is exactly a 3-truss");
    }

    /// Duplicate and reversed links collapse onto one canonical edge,
    /// stable regardless of input order.
    #[test]
    fn duplicate_and_reversed_links_canonicalize_stably() {
        let memories = [memory("mem_a"), memory("mem_b")];
        let forward = [
            link("lnk_1", "mem_a", "mem_b"),
            link("lnk_2", "mem_b", "mem_a"),
            link("lnk_3", "mem_a", "mem_b"),
            link("lnk_4", "mem_a", "mem_a"),
        ];
        let reversed: Vec<StoredMemoryLink> = forward.iter().rev().cloned().collect();

        let one = build_structural_graph_input(&memories, &forward, None);
        let two = build_structural_graph_input(&memories, &reversed, None);
        assert_eq!(one.edge_count, 1, "dup/reverse/self links collapse to one edge");
        assert_eq!(one.edges, two.edges, "projection is input-order independent");
        assert_eq!(
            (one.edges[0].a.as_str(), one.edges[0].b.as_str()),
            ("mem_a", "mem_b")
        );
    }
}
