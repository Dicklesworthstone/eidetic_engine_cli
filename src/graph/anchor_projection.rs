//! bd-1n0np.3.4 — graph projection from memories onto code surfaces.
//!
//! Projects `MemoryAnchor` rows (bd-1n0np.3.2) into a deterministic edge set:
//! - `memory -> anchor` (`MentionsSurface`): a memory references a code surface.
//! - `anchor <-> anchor` (`AnchorProximity`): two surfaces co-mentioned by the
//!   same memory are proximate (surfaces that appear together).
//!
//! The edge set feeds fnx graph analysis (articulation / proximity, reusing
//! `src/graph/decay.rs` + `gomory_hu`) for impact + bridge analysis, Pack DNA,
//! and blind-spot coverage. This builder is pure and deterministic; the caller
//! constructs the fnx graph and runs the analytics. Surface nodes are keyed on
//! the anchor value HASH (never the raw value), so the projection is
//! redaction-safe by construction.

use std::collections::{BTreeMap, BTreeSet};

use crate::models::StoredMemoryAnchor;

/// Relation kind of a projected edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum AnchorProjectionRelation {
    /// A memory references a code surface (`memory -> anchor`).
    MentionsSurface,
    /// Two surfaces co-mentioned by the same memory (`anchor <-> anchor`).
    AnchorProximity,
}

impl AnchorProjectionRelation {
    /// Stable string form for JSON output and graph edge labels.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MentionsSurface => "mentions_surface",
            Self::AnchorProximity => "anchor_proximity",
        }
    }
}

/// One projected edge between graph nodes (memory ids and surface keys).
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct AnchorProjectionEdge {
    pub source: String,
    pub target: String,
    pub relation: AnchorProjectionRelation,
}

/// A deterministic projection of memories onto code surfaces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnchorProjection {
    pub memory_nodes: Vec<String>,
    pub anchor_nodes: Vec<String>,
    pub edges: Vec<AnchorProjectionEdge>,
}

/// Stable surface key for an anchor node: `kind:value_hash`. Keyed on the hash
/// (never the raw value) so the node identifier stays redaction-safe.
#[must_use]
pub fn anchor_surface_key(anchor: &StoredMemoryAnchor) -> String {
    format!(
        "{}:{}",
        anchor.anchor_kind.as_str(),
        anchor.anchor_value_hash
    )
}

/// Build the memory→surface projection from anchor rows (bd-1n0np.3.4).
/// Deterministic: nodes and edges are sorted and deduplicated, and every
/// `AnchorProximity` edge is emitted in canonical (lexicographically ordered)
/// node order, so the projection is independent of input order.
#[must_use]
pub fn project_memory_anchor_graph(anchors: &[StoredMemoryAnchor]) -> AnchorProjection {
    let mut memory_nodes: BTreeSet<String> = BTreeSet::new();
    let mut anchor_nodes: BTreeSet<String> = BTreeSet::new();
    let mut edges: BTreeSet<AnchorProjectionEdge> = BTreeSet::new();
    // memory id -> its distinct surface keys, used to derive proximity pairs.
    let mut surfaces_by_memory: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for anchor in anchors {
        let memory = anchor.memory_id.clone();
        let surface = anchor_surface_key(anchor);
        memory_nodes.insert(memory.clone());
        anchor_nodes.insert(surface.clone());
        edges.insert(AnchorProjectionEdge {
            source: memory.clone(),
            target: surface.clone(),
            relation: AnchorProjectionRelation::MentionsSurface,
        });
        surfaces_by_memory
            .entry(memory)
            .or_default()
            .insert(surface);
    }

    for surfaces in surfaces_by_memory.values() {
        let ordered: Vec<&String> = surfaces.iter().collect();
        for (index, left) in ordered.iter().enumerate() {
            for right in ordered.iter().skip(index + 1) {
                edges.insert(AnchorProjectionEdge {
                    source: (*left).clone(),
                    target: (*right).clone(),
                    relation: AnchorProjectionRelation::AnchorProximity,
                });
            }
        }
    }

    AnchorProjection {
        memory_nodes: memory_nodes.into_iter().collect(),
        anchor_nodes: anchor_nodes.into_iter().collect(),
        edges: edges.into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::{AnchorProjectionRelation, project_memory_anchor_graph};
    use crate::models::{
        MemoryAnchorFreshnessState, MemoryAnchorKind, MemoryAnchorSource, StoredMemoryAnchor,
    };

    fn anchor(memory: &str, kind: MemoryAnchorKind, hash: &str) -> StoredMemoryAnchor {
        StoredMemoryAnchor {
            memory_id: memory.to_string(),
            anchor_kind: kind,
            anchor_value_hash: hash.to_string(),
            redacted_anchor_value: "redacted".to_string(),
            confidence: 1.0,
            source: MemoryAnchorSource::Explicit,
            provenance: "test".to_string(),
            captured_span_hash: "blake3:span".to_string(),
            freshness_state: MemoryAnchorFreshnessState::Current,
            generation: 0,
            created_at: "2026-06-07T00:00:00Z".to_string(),
            updated_at: "2026-06-07T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn projection_links_memories_to_shared_surface() {
        // Two memories anchor the SAME surface -> convergence on one anchor node.
        let anchors = vec![
            anchor("mem_a", MemoryAnchorKind::Path, "blake3:fileX"),
            anchor("mem_b", MemoryAnchorKind::Path, "blake3:fileX"),
        ];
        let projection = project_memory_anchor_graph(&anchors);

        assert_eq!(
            projection.memory_nodes,
            vec!["mem_a".to_string(), "mem_b".to_string()]
        );
        assert_eq!(
            projection.anchor_nodes,
            vec!["path:blake3:fileX".to_string()]
        );
        assert_eq!(projection.edges.len(), 2);
        assert!(
            projection
                .edges
                .iter()
                .all(|edge| edge.relation == AnchorProjectionRelation::MentionsSurface)
        );
    }

    #[test]
    fn projection_emits_proximity_for_co_mentioned_surfaces() {
        // One memory anchors two surfaces -> one proximity edge between them.
        let anchors = vec![
            anchor("mem_a", MemoryAnchorKind::Path, "blake3:fileX"),
            anchor("mem_a", MemoryAnchorKind::Symbol, "blake3:funcY"),
        ];
        let projection = project_memory_anchor_graph(&anchors);

        let proximity: Vec<_> = projection
            .edges
            .iter()
            .filter(|edge| edge.relation == AnchorProjectionRelation::AnchorProximity)
            .collect();
        assert_eq!(proximity.len(), 1);
        // Canonical order: "path:..." < "symbol:..." lexicographically.
        assert_eq!(proximity[0].source, "path:blake3:fileX");
        assert_eq!(proximity[0].target, "symbol:blake3:funcY");
    }

    #[test]
    fn projection_is_deterministic_and_deduped() {
        let forward = vec![
            anchor("mem_a", MemoryAnchorKind::Path, "blake3:f1"),
            anchor("mem_a", MemoryAnchorKind::Symbol, "blake3:s1"),
            anchor("mem_a", MemoryAnchorKind::Path, "blake3:f1"),
        ];
        let mut reversed = forward.clone();
        reversed.reverse();

        let first = project_memory_anchor_graph(&forward);
        let second = project_memory_anchor_graph(&reversed);
        assert_eq!(first, second, "projection is independent of input order");

        // The duplicate f1 collapses to one node and one mentions edge.
        assert_eq!(first.anchor_nodes.len(), 2);
        let mentions = first
            .edges
            .iter()
            .filter(|edge| edge.relation == AnchorProjectionRelation::MentionsSurface)
            .count();
        assert_eq!(mentions, 2);
    }
}
