//! bd-1n0np.7.2 — contradiction detection from explicit DB evidence.
//!
//! Detects contradiction clusters from *explicit* signals only — the discipline
//! both wizards agreed on (explicit-evidence-FIRST). A [`ConflictEdge`] is an
//! already-extracted relationship between two memories that the store records
//! durably: a contradiction/supersession link, an overlapping validity window, a
//! duplicate-but-divergent pair, a trust/outcome split, or repeated co-selection.
//! The caller gathers these from the DB; this module is the pure detector.
//!
//! The explicit edges form a contradiction graph, and we **reuse**
//! `crate::graph::health` (k-truss + Louvain via
//! [`detect_contradiction_clusters_with_policy`]) — the same machinery
//! structural health uses — to find the clusters. Each cluster is then ranked by
//! *centrality* (conflict-edge degree over its members) and *load-bearing*
//! weight (the strength of the signals implicating it), so the most urgent,
//! most-connected contradictions sort first.
//!
//! The fuzzy near-conflict detector (embedding opposition) is the
//! false-positive-prone part; it stays **opt-in** behind
//! [`ContradictionDetectionConfig::include_fuzzy_near_conflict`] and is *not*
//! implemented in v1 — when requested, the report flags it as skipped (no silent
//! cap) rather than silently widening to fuzzy matches. The explicit graph is the
//! gate.

use std::collections::{BTreeMap, BTreeSet};

use fnx_classes::Graph;
use fnx_runtime::CompatibilityMode;

use crate::graph::health::{
    ContradictionCluster, ContradictionClusterPolicy, detect_contradiction_clusters_with_policy,
};

/// An explicit, DB-recorded conflict signal between two memories. Each variant is
/// evidence the store already holds — never an inferred/fuzzy guess.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ExplicitConflictSignal {
    /// A direct `contradicts` link between the two memories.
    ContradictionLink,
    /// One memory supersedes the other (supersession link).
    Supersession,
    /// Their validity windows overlap while asserting different things.
    ValidityWindowOverlap,
    /// Near-duplicate content that nonetheless diverges.
    DuplicateDivergent,
    /// Their trust / outcome evidence points in opposite directions.
    TrustOutcomeSplit,
    /// They are repeatedly co-selected into the same packs (co-occurrence).
    RepeatedCoSelection,
}

impl ExplicitConflictSignal {
    /// Stable snake_case form for JSON / edge labels.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ContradictionLink => "contradiction_link",
            Self::Supersession => "supersession",
            Self::ValidityWindowOverlap => "validity_window_overlap",
            Self::DuplicateDivergent => "duplicate_divergent",
            Self::TrustOutcomeSplit => "trust_outcome_split",
            Self::RepeatedCoSelection => "repeated_co_selection",
        }
    }

    /// Load-bearing weight (milli-units): how strongly this signal implicates a
    /// genuine contradiction. A direct contradiction link is the heaviest; mere
    /// repeated co-selection is the lightest explicit signal.
    #[must_use]
    pub const fn weight_milli(self) -> u64 {
        match self {
            Self::ContradictionLink => 1000,
            Self::Supersession => 900,
            Self::DuplicateDivergent => 700,
            Self::ValidityWindowOverlap => 600,
            Self::TrustOutcomeSplit => 500,
            Self::RepeatedCoSelection => 300,
        }
    }
}

/// One explicit conflict relationship between two memories (the detector input).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConflictEdge {
    pub memory_a: String,
    pub memory_b: String,
    pub signal: ExplicitConflictSignal,
}

impl ConflictEdge {
    #[must_use]
    pub fn new(memory_a: &str, memory_b: &str, signal: ExplicitConflictSignal) -> Self {
        Self {
            memory_a: memory_a.to_string(),
            memory_b: memory_b.to_string(),
            signal,
        }
    }
}

/// Detector configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContradictionDetectionConfig {
    /// Optional Louvain density threshold override (forwarded to
    /// [`ContradictionClusterPolicy`]). `None` uses the health default.
    pub density_threshold: Option<f64>,
    /// Opt-in for the fuzzy embedding-opposition detector. Deferred in v1: when
    /// `true`, the report records the fuzzy pass as *skipped* rather than running
    /// the false-positive-prone path.
    pub include_fuzzy_near_conflict: bool,
}

impl Default for ContradictionDetectionConfig {
    fn default() -> Self {
        Self {
            density_threshold: None,
            include_fuzzy_near_conflict: false,
        }
    }
}

/// A contradiction cluster (from health.rs) plus its explicit-evidence ranking.
#[derive(Clone, Debug, PartialEq)]
pub struct RankedContradictionCluster {
    /// The underlying cluster as detected by `graph::health` (k-truss + Louvain).
    pub cluster: ContradictionCluster,
    /// Conflict-edge degree summed over the cluster's exemplar members
    /// (a centrality proxy: how connected the cluster is in the conflict graph).
    pub centrality: u32,
    /// Sum of signal weights (milli) of conflict edges incident to the cluster's
    /// exemplar members — the cluster's load-bearing mass.
    pub load_bearing_milli: u64,
    /// Deterministic composite urgency score; higher sorts first.
    pub rank_score: f64,
}

/// Result of explicit-evidence contradiction detection.
#[derive(Clone, Debug, PartialEq)]
pub struct ContradictionDetectionReport {
    /// Detected clusters, ranked most-urgent first.
    pub clusters: Vec<RankedContradictionCluster>,
    /// Number of distinct (canonicalized) explicit conflict edges considered.
    pub explicit_edge_count: usize,
    /// `true` when the caller requested the fuzzy near-conflict pass but it was
    /// skipped (v1 defers it). Surfaced so the omission is never silent.
    pub fuzzy_near_conflict_skipped: bool,
}

/// Canonicalize an edge to an unordered, trimmed `(low, high)` pair, dropping
/// blanks and self-loops. Returns `None` if the edge is unusable.
fn canonical_pair(edge: &ConflictEdge) -> Option<(String, String)> {
    let a = edge.memory_a.trim();
    let b = edge.memory_b.trim();
    if a.is_empty() || b.is_empty() || a == b {
        return None;
    }
    if a <= b {
        Some((a.to_string(), b.to_string()))
    } else {
        Some((b.to_string(), a.to_string()))
    }
}

/// Detect contradiction clusters from explicit conflict evidence (bd-1n0np.7.2).
///
/// Builds a contradiction graph from the (deduplicated) explicit edges, reuses
/// `graph::health` Louvain/k-truss clustering, then ranks each cluster by
/// centrality + load-bearing weight. Deterministic: edges are canonicalized and
/// deduplicated, and ties break on `louvain_id`.
#[must_use]
pub fn detect_explicit_contradictions(
    edges: &[ConflictEdge],
    config: ContradictionDetectionConfig,
) -> ContradictionDetectionReport {
    // Deduplicate edges to canonical unordered pairs, keeping the heaviest signal
    // weight seen for each pair (a pair backed by multiple signals is stronger).
    let mut pair_weight: BTreeMap<(String, String), u64> = BTreeMap::new();
    for edge in edges {
        if let Some(pair) = canonical_pair(edge) {
            let weight = edge.signal.weight_milli();
            pair_weight
                .entry(pair)
                .and_modify(|w| *w = (*w).max(weight))
                .or_insert(weight);
        }
    }

    // Per-memory conflict degree (centrality proxy) over the deduped edge set.
    let mut degree: BTreeMap<String, u32> = BTreeMap::new();
    for (a, b) in pair_weight.keys() {
        *degree.entry(a.clone()).or_insert(0) += 1;
        *degree.entry(b.clone()).or_insert(0) += 1;
    }

    // Build the contradiction graph (same construction health.rs uses for its
    // `Contradicts` relation graph) and reuse the proven cluster detector.
    let mut graph = Graph::new(CompatibilityMode::Strict);
    for (a, b) in pair_weight.keys() {
        graph.add_node(a);
        graph.add_node(b);
        let _ = graph.extend_edges_unrecorded([(a.as_str(), b.as_str())]);
    }
    let policy = ContradictionClusterPolicy::from_optional_config(config.density_threshold);
    let clusters = detect_contradiction_clusters_with_policy(&graph, policy);

    let mut ranked: Vec<RankedContradictionCluster> = clusters
        .into_iter()
        .map(|cluster| rank_cluster(cluster, &pair_weight, &degree))
        .collect();

    // Most urgent first; deterministic tie-break on louvain_id.
    ranked.sort_by(|left, right| {
        right
            .rank_score
            .partial_cmp(&left.rank_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(left.cluster.louvain_id.cmp(&right.cluster.louvain_id))
    });

    ContradictionDetectionReport {
        clusters: ranked,
        explicit_edge_count: pair_weight.len(),
        fuzzy_near_conflict_skipped: config.include_fuzzy_near_conflict,
    }
}

/// Rank one detected cluster by centrality + load-bearing weight.
fn rank_cluster(
    cluster: ContradictionCluster,
    pair_weight: &BTreeMap<(String, String), u64>,
    degree: &BTreeMap<String, u32>,
) -> RankedContradictionCluster {
    let members: BTreeSet<&String> = cluster.exemplar_memory_ids.iter().collect();

    let centrality: u32 = cluster
        .exemplar_memory_ids
        .iter()
        .map(|id| degree.get(id).copied().unwrap_or(0))
        .sum();

    // Load-bearing mass: each edge incident to a member contributes its weight
    // once (a member set is small, so a linear scan over deduped edges is fine).
    let load_bearing_milli: u64 = pair_weight
        .iter()
        .filter(|((a, b), _)| members.contains(a) || members.contains(b))
        .map(|(_, weight)| *weight)
        .sum();

    // Composite: severity multiplies, density and centrality scale, load-bearing
    // weight (in whole units) lifts. All inputs are deterministic.
    let severity_factor = match cluster.severity {
        crate::graph::health::ContradictionSeverity::Incoherent => 2.0,
        crate::graph::health::ContradictionSeverity::Inconsistent => 1.0,
    };
    let rank_score = severity_factor
        * cluster.density
        * (f64::from(centrality) + 1.0)
        * (1.0 + (load_bearing_milli as f64) / 1000.0);

    RankedContradictionCluster {
        cluster,
        centrality,
        load_bearing_milli,
        rank_score,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConflictEdge, ContradictionDetectionConfig, ExplicitConflictSignal, canonical_pair,
        detect_explicit_contradictions,
    };

    #[test]
    fn signal_weights_are_ordered_explicit_first() {
        // Direct contradiction links must outweigh weaker co-selection evidence.
        assert!(
            ExplicitConflictSignal::ContradictionLink.weight_milli()
                > ExplicitConflictSignal::RepeatedCoSelection.weight_milli()
        );
        assert!(
            ExplicitConflictSignal::Supersession.weight_milli()
                > ExplicitConflictSignal::TrustOutcomeSplit.weight_milli()
        );
    }

    #[test]
    fn canonical_pair_is_unordered_and_drops_blanks_and_self_loops() {
        let forward =
            ConflictEdge::new("mem_b", "mem_a", ExplicitConflictSignal::ContradictionLink);
        let reversed =
            ConflictEdge::new("  mem_a  ", "mem_b", ExplicitConflictSignal::Supersession);
        assert_eq!(canonical_pair(&forward), canonical_pair(&reversed));
        assert_eq!(
            canonical_pair(&forward),
            Some(("mem_a".to_string(), "mem_b".to_string()))
        );
        // Self-loops and blanks are unusable.
        assert_eq!(
            canonical_pair(&ConflictEdge::new(
                "x",
                "x",
                ExplicitConflictSignal::ContradictionLink
            )),
            None
        );
        assert_eq!(
            canonical_pair(&ConflictEdge::new(
                "   ",
                "y",
                ExplicitConflictSignal::ContradictionLink
            )),
            None
        );
    }

    #[test]
    fn no_edges_yields_no_clusters() {
        let report = detect_explicit_contradictions(&[], ContradictionDetectionConfig::default());
        assert!(report.clusters.is_empty());
        assert_eq!(report.explicit_edge_count, 0);
        assert!(!report.fuzzy_near_conflict_skipped);
    }

    #[test]
    fn duplicate_edges_are_canonicalized_before_counting() {
        // Same pair via two directions / two signals counts as ONE explicit edge.
        let edges = vec![
            ConflictEdge::new("mem_a", "mem_b", ExplicitConflictSignal::ContradictionLink),
            ConflictEdge::new(
                "mem_b",
                "mem_a",
                ExplicitConflictSignal::RepeatedCoSelection,
            ),
        ];
        let report =
            detect_explicit_contradictions(&edges, ContradictionDetectionConfig::default());
        assert_eq!(report.explicit_edge_count, 1);
    }

    #[test]
    fn requested_fuzzy_pass_is_reported_skipped_not_silently_run() {
        let config = ContradictionDetectionConfig {
            density_threshold: None,
            include_fuzzy_near_conflict: true,
        };
        let report = detect_explicit_contradictions(&[], config);
        // No silent widening: the deferred fuzzy pass is flagged, not performed.
        assert!(report.fuzzy_near_conflict_skipped);
    }

    #[test]
    fn dense_contradiction_clique_is_detected_and_ranked() {
        // A 3-memory contradiction triangle is a clear, dense conflict cluster.
        let edges = vec![
            ConflictEdge::new("mem_a", "mem_b", ExplicitConflictSignal::ContradictionLink),
            ConflictEdge::new("mem_b", "mem_c", ExplicitConflictSignal::ContradictionLink),
            ConflictEdge::new("mem_a", "mem_c", ExplicitConflictSignal::Supersession),
        ];
        let report =
            detect_explicit_contradictions(&edges, ContradictionDetectionConfig::default());
        assert_eq!(report.explicit_edge_count, 3);
        assert!(
            !report.clusters.is_empty(),
            "a dense contradiction triangle should surface at least one cluster"
        );
        let top = &report.clusters[0];
        assert!(top.rank_score > 0.0);
        assert!(top.load_bearing_milli > 0);
        assert!(top.centrality > 0);
    }

    #[test]
    fn ranking_is_deterministic_across_input_order() {
        let edges = vec![
            ConflictEdge::new("mem_a", "mem_b", ExplicitConflictSignal::ContradictionLink),
            ConflictEdge::new("mem_b", "mem_c", ExplicitConflictSignal::ContradictionLink),
            ConflictEdge::new("mem_a", "mem_c", ExplicitConflictSignal::ContradictionLink),
        ];
        let mut reversed = edges.clone();
        reversed.reverse();
        let first = detect_explicit_contradictions(&edges, ContradictionDetectionConfig::default());
        let second =
            detect_explicit_contradictions(&reversed, ContradictionDetectionConfig::default());
        assert_eq!(first, second, "detection is independent of input order");
    }
}
