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

use serde::Serialize;

use crate::db::{DbConnection, MemoryLinkRelation};
use crate::graph::health::{
    ContradictionCluster, ContradictionClusterPolicy, ContradictionSeverity,
    detect_contradiction_clusters_with_policy,
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

/// Explicit conflict edges gathered from the database, with an honest record of
/// which signal kinds were covered (bd-1n0np.7.2 DB-gather).
///
/// The `deferred` list is surfaced so a not-yet-gathered signal kind is never
/// silently treated as "no conflict" — the same no-silent-cap discipline the
/// detector uses for its deferred fuzzy pass.
#[derive(Clone, Debug, PartialEq)]
pub struct GatheredConflictEdges {
    /// The explicit conflict edges, ready to feed [`detect_explicit_contradictions`].
    pub edges: Vec<ConflictEdge>,
    /// Explicit signal kinds this gather covered.
    pub gathered: Vec<ExplicitConflictSignal>,
    /// Explicit signal kinds deferred to a later DB-gather slice (reported, not
    /// silently dropped).
    pub deferred: Vec<ExplicitConflictSignal>,
    /// Set when the link read failed: the gather degrades to no edges rather than
    /// panicking, and the read failure is reported instead of being swallowed.
    pub read_error: Option<String>,
}

/// Explicit signal kinds the v1 DB-gather covers (the link-based, least-ambiguous
/// evidence the store records directly).
const GATHERED_SIGNAL_KINDS: [ExplicitConflictSignal; 2] = [
    ExplicitConflictSignal::ContradictionLink,
    ExplicitConflictSignal::Supersession,
];

/// Explicit signal kinds deferred to later DB-gather slices. These require
/// cross-referencing memory rows / feedback events (and are more
/// false-positive-prone), so v1 reports them as not-yet-gathered.
const DEFERRED_SIGNAL_KINDS: [ExplicitConflictSignal; 4] = [
    ExplicitConflictSignal::DuplicateDivergent,
    ExplicitConflictSignal::ValidityWindowOverlap,
    ExplicitConflictSignal::TrustOutcomeSplit,
    ExplicitConflictSignal::RepeatedCoSelection,
];

/// Gather explicit conflict edges from the database (bd-1n0np.7.2 DB-gather).
///
/// v1 gathers the **link-based** explicit signals — the heaviest, least-ambiguous
/// evidence the store records directly: `contradicts` links
/// ([`ExplicitConflictSignal::ContradictionLink`]) and `supersedes` links
/// ([`ExplicitConflictSignal::Supersession`]). It reuses the exact same
/// [`DbConnection::list_all_memory_links`] load that `graph::health` uses, so the
/// contradiction graph stays consistent with structural health.
///
/// The remaining explicit signals (validity-window overlap, duplicate-divergent,
/// trust/outcome split, repeated co-selection) require cross-referencing memory
/// rows and feedback events; they are gathered in later slices and reported via
/// [`GatheredConflictEdges::deferred`] so an un-gathered kind is never silently
/// treated as absent. The fuzzy embedding-opposition detector remains opt-in and
/// out of scope here (the explicit graph is the gate).
///
/// Deterministic: links are loaded in the connection's deterministic order and
/// mapped 1:1; canonicalization/dedup happens downstream in
/// [`detect_explicit_contradictions`].
#[must_use]
pub fn gather_explicit_conflict_edges(connection: &DbConnection) -> GatheredConflictEdges {
    let gathered = GATHERED_SIGNAL_KINDS.to_vec();
    let deferred = DEFERRED_SIGNAL_KINDS.to_vec();

    let links = match connection.list_all_memory_links(None) {
        Ok(links) => links,
        Err(error) => {
            return GatheredConflictEdges {
                edges: Vec::new(),
                gathered,
                deferred,
                read_error: Some(format!("memory links could not be read: {error}")),
            };
        }
    };

    let mut edges = Vec::new();
    for link in &links {
        let signal = match link.relation_enum() {
            Some(MemoryLinkRelation::Contradicts) => ExplicitConflictSignal::ContradictionLink,
            Some(MemoryLinkRelation::Supersedes) => ExplicitConflictSignal::Supersession,
            // Supports / Related / DerivedFrom / CoTag / CoMention and any
            // unparseable relation are not explicit conflict evidence.
            _ => continue,
        };
        edges.push(ConflictEdge::new(
            &link.src_memory_id,
            &link.dst_memory_id,
            signal,
        ));
    }

    GatheredConflictEdges {
        edges,
        gathered,
        deferred,
        read_error: None,
    }
}

/// Convenience: gather explicit conflict edges from the database and run the
/// detector in one call (bd-1n0np.7.2). Returns the detection report alongside
/// the gather's coverage record so callers (the `ee conflict` surface,
/// bd-1n0np.7.3) can report both the clusters and which explicit signals were
/// considered vs deferred.
#[must_use]
pub fn detect_explicit_contradictions_from_connection(
    connection: &DbConnection,
    config: ContradictionDetectionConfig,
) -> (ContradictionDetectionReport, GatheredConflictEdges) {
    let gathered = gather_explicit_conflict_edges(connection);
    let report = detect_explicit_contradictions(&gathered.edges, config);
    (report, gathered)
}

// ---------------------------------------------------------------------------
// Read-only conflict surface (bd-1n0np.7.3): ee conflict list/explain/cluster.
//
// Joins the explicit-evidence detector output with the memory rows it implicates
// so an agent sees the ranked conflicting pairs WITH both bodies, which side is
// higher-trust/fresher, and the load-bearing status — without any mutation.
// ---------------------------------------------------------------------------

/// Schema id for the read-only conflict surface JSON.
pub const CONFLICT_SURFACE_SCHEMA_V1: &str = "ee.conflict.v1";

/// Trust ranking for a memory's `trust_class` (higher = more trusted). Used only
/// to pick the "higher-trust side" of a conflicting pair. Ranks the canonical
/// memory trust-class vocabulary (the `memories.trust_class` CHECK set); unknown
/// classes rank low-but-nonzero so they never silently outrank a known class.
#[must_use]
pub fn trust_class_rank(trust_class: &str) -> u8 {
    match trust_class {
        "human_explicit" => 5,
        "agent_validated" => 4,
        "cass_evidence" => 3,
        "agent_assertion" => 2,
        "legacy_import" => 1,
        "external" => 0,
        _ => 1,
    }
}

/// Stable id for a canonical conflicting pair (order-independent).
fn conflict_pair_id(low: &str, high: &str) -> String {
    let digest = blake3::hash(format!("{low}\u{0}{high}").as_bytes()).to_hex();
    format!("cf_{}", &digest[..16])
}

/// One memory's read-only projection inside a conflicting pair.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictMemberView {
    pub id: String,
    pub content: String,
    pub level: String,
    pub kind: String,
    pub trust_class: String,
    pub trust_rank: u8,
    pub confidence: f32,
    pub importance: f32,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    pub updated_at: String,
    /// `true` when this side is the higher-trust / fresher side of the pair.
    pub preferred: bool,
}

/// One ranked conflicting pair with both bodies and the preferred side.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictPairView {
    pub conflict_id: String,
    /// The heaviest explicit signal implicating this pair (snake_case).
    pub signal: String,
    pub load_bearing_milli: u64,
    /// `"a"`, `"b"`, or `"tie"` — which member is the higher-trust/fresher side.
    pub preferred_side: String,
    /// Why that side was preferred: `higher_trust`, `fresher`, or `tie_no_signal`.
    pub preferred_reason: String,
    pub memory_a: ConflictMemberView,
    pub memory_b: ConflictMemberView,
}

/// A detected contradiction cluster projected for the surface.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictClusterView {
    pub louvain_id: usize,
    pub size: usize,
    pub density: f64,
    pub severity: ContradictionSeverity,
    pub member_ids: Vec<String>,
    pub centrality: u32,
    pub load_bearing_milli: u64,
    pub rank_score: f64,
    pub suggested_action: String,
}

/// The full read-only conflict surface (`ee.conflict.v1`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictSurface {
    pub schema: &'static str,
    /// Ranked conflicting pairs, heaviest load-bearing first (deterministic).
    pub pairs: Vec<ConflictPairView>,
    /// Detected contradiction clusters, most-urgent first.
    pub clusters: Vec<ConflictClusterView>,
    pub explicit_edge_count: usize,
    /// Explicit signal kinds the gather covered (snake_case).
    pub gathered_signals: Vec<String>,
    /// Explicit signal kinds deferred to later slices (reported, not silent).
    pub deferred_signals: Vec<String>,
    pub fuzzy_near_conflict_skipped: bool,
    /// Non-fatal degradations (e.g. a link-read failure); never silent loss.
    pub degraded: Vec<String>,
}

impl ConflictSurface {
    /// Pairs/clusters that implicate `memory_id` (for `ee conflict explain`).
    #[must_use]
    pub fn focused_on(&self, memory_id: &str) -> ConflictSurface {
        let pairs: Vec<ConflictPairView> = self
            .pairs
            .iter()
            .filter(|p| p.memory_a.id == memory_id || p.memory_b.id == memory_id)
            .cloned()
            .collect();
        let clusters: Vec<ConflictClusterView> = self
            .clusters
            .iter()
            .filter(|c| c.member_ids.iter().any(|id| id == memory_id))
            .cloned()
            .collect();
        ConflictSurface {
            schema: self.schema,
            explicit_edge_count: pairs.len(),
            pairs,
            clusters,
            gathered_signals: self.gathered_signals.clone(),
            deferred_signals: self.deferred_signals.clone(),
            fuzzy_near_conflict_skipped: self.fuzzy_near_conflict_skipped,
            degraded: self.degraded.clone(),
        }
    }
}

/// Build a member view, marking `preferred` per the pair decision.
fn member_view(memory: &crate::db::StoredMemory, preferred: bool) -> ConflictMemberView {
    ConflictMemberView {
        id: memory.id.clone(),
        content: memory.content.clone(),
        level: memory.level.clone(),
        kind: memory.kind.clone(),
        trust_rank: trust_class_rank(&memory.trust_class),
        trust_class: memory.trust_class.clone(),
        confidence: memory.confidence,
        importance: memory.importance,
        valid_from: memory.valid_from.clone(),
        valid_to: memory.valid_to.clone(),
        updated_at: memory.updated_at.clone(),
        preferred,
    }
}

/// Decide the preferred (higher-trust, else fresher) side of a pair.
/// Returns `("a"|"b"|"tie", reason, a_preferred, b_preferred)`.
fn preferred_side(
    a: &crate::db::StoredMemory,
    b: &crate::db::StoredMemory,
) -> (&'static str, &'static str, bool, bool) {
    let (ra, rb) = (
        trust_class_rank(&a.trust_class),
        trust_class_rank(&b.trust_class),
    );
    if ra > rb {
        ("a", "higher_trust", true, false)
    } else if rb > ra {
        ("b", "higher_trust", false, true)
    } else if a.updated_at > b.updated_at {
        ("a", "fresher", true, false)
    } else if b.updated_at > a.updated_at {
        ("b", "fresher", false, true)
    } else {
        ("tie", "tie_no_signal", false, false)
    }
}

/// Assemble the read-only conflict surface from the database (bd-1n0np.7.3).
///
/// Reuses [`detect_explicit_contradictions_from_connection`] (the 7.2 gather +
/// detector), then joins each canonical conflicting pair with both memory rows so
/// the surface can report both bodies, the higher-trust/fresher side, and the
/// load-bearing weight. A pair whose memory rows cannot be read is dropped with a
/// visible `degraded` note (never silent). Deterministic: pairs sort by
/// load-bearing weight desc, then `conflict_id`.
#[must_use]
pub fn assemble_conflict_surface(
    connection: &DbConnection,
    config: ContradictionDetectionConfig,
) -> ConflictSurface {
    let (report, gathered) = detect_explicit_contradictions_from_connection(connection, config);

    let mut degraded: Vec<String> = Vec::new();
    if let Some(error) = &gathered.read_error {
        degraded.push(error.clone());
    }

    // Deduplicate to canonical pairs, keeping the heaviest signal per pair.
    let mut pair_signal: BTreeMap<(String, String), ExplicitConflictSignal> = BTreeMap::new();
    for edge in &gathered.edges {
        if let Some(pair) = canonical_pair(edge) {
            pair_signal
                .entry(pair)
                .and_modify(|s| {
                    if edge.signal.weight_milli() > s.weight_milli() {
                        *s = edge.signal;
                    }
                })
                .or_insert(edge.signal);
        }
    }

    let mut pairs: Vec<ConflictPairView> = Vec::new();
    for ((low, high), signal) in &pair_signal {
        let (Ok(Some(a)), Ok(Some(b))) = (connection.get_memory(low), connection.get_memory(high))
        else {
            degraded.push(format!(
                "conflict pair {low}<->{high} skipped: a cited memory row could not be read"
            ));
            continue;
        };
        let (preferred, reason, a_pref, b_pref) = preferred_side(&a, &b);
        pairs.push(ConflictPairView {
            conflict_id: conflict_pair_id(low, high),
            signal: signal.as_str().to_owned(),
            load_bearing_milli: signal.weight_milli(),
            preferred_side: preferred.to_owned(),
            preferred_reason: reason.to_owned(),
            memory_a: member_view(&a, a_pref),
            memory_b: member_view(&b, b_pref),
        });
    }

    // Deterministic: heaviest load-bearing first, then stable conflict_id.
    pairs.sort_by(|left, right| {
        right
            .load_bearing_milli
            .cmp(&left.load_bearing_milli)
            .then_with(|| left.conflict_id.cmp(&right.conflict_id))
    });

    let clusters: Vec<ConflictClusterView> = report
        .clusters
        .iter()
        .map(|ranked| ConflictClusterView {
            louvain_id: ranked.cluster.louvain_id,
            size: ranked.cluster.size,
            density: ranked.cluster.density,
            severity: ranked.cluster.severity,
            member_ids: ranked.cluster.exemplar_memory_ids.clone(),
            centrality: ranked.centrality,
            load_bearing_milli: ranked.load_bearing_milli,
            rank_score: ranked.rank_score,
            suggested_action: ranked.cluster.suggested_action.to_owned(),
        })
        .collect();

    ConflictSurface {
        schema: CONFLICT_SURFACE_SCHEMA_V1,
        pairs,
        clusters,
        explicit_edge_count: report.explicit_edge_count,
        gathered_signals: gathered
            .gathered
            .iter()
            .map(|s| s.as_str().to_owned())
            .collect(),
        deferred_signals: gathered
            .deferred
            .iter()
            .map(|s| s.as_str().to_owned())
            .collect(),
        fuzzy_near_conflict_skipped: report.fuzzy_near_conflict_skipped,
        degraded,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CONFLICT_SURFACE_SCHEMA_V1, ConflictEdge, ContradictionDetectionConfig,
        ExplicitConflictSignal, assemble_conflict_surface, canonical_pair,
        detect_explicit_contradictions, detect_explicit_contradictions_from_connection,
        gather_explicit_conflict_edges, trust_class_rank,
    };
    use crate::db::{
        CreateMemoryInput, CreateMemoryLinkInput, CreateWorkspaceInput, DbConnection,
        MemoryLinkRelation, MemoryLinkSource,
    };

    // ---- DB-gather test scaffolding (mirrors src/core/health.rs) ----------
    // IDs must satisfy the schema CHECK constraints: wsp_/mem_ are length 30,
    // link_ is length 31 (see src/db/mod.rs).
    const WS_ID: &str = "wsp_00000000000000000000000072";
    const MEM_A: &str = "mem_00000000000000000000000001";
    const MEM_B: &str = "mem_00000000000000000000000002";
    const MEM_C: &str = "mem_00000000000000000000000003";
    const LINK_1: &str = "link_00000000000000000000000001";
    const LINK_2: &str = "link_00000000000000000000000002";
    const LINK_3: &str = "link_00000000000000000000000003";

    fn open_seeded_db() -> DbConnection {
        let connection = DbConnection::open_memory().expect("open in-memory db");
        connection.migrate().expect("migrate schema");
        connection
            .insert_workspace(
                WS_ID,
                &CreateWorkspaceInput {
                    path: "/tmp/ee-contradiction-gather-fixture".to_owned(),
                    name: Some("contradiction gather".to_owned()),
                },
            )
            .expect("insert workspace");
        connection
    }

    fn seed_memory(connection: &DbConnection, memory_id: &str) {
        seed_memory_trust(connection, memory_id, "agent_assertion");
    }

    fn seed_memory_trust(connection: &DbConnection, memory_id: &str, trust_class: &str) {
        connection
            .insert_memory(
                memory_id,
                &CreateMemoryInput {
                    workspace_id: WS_ID.to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: format!("fixture {memory_id}"),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: None,
                    trust_class: trust_class.to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: None,
                    valid_to: None,
                },
            )
            .expect("insert memory");
    }

    fn seed_link(
        connection: &DbConnection,
        link_id: &str,
        src: &str,
        dst: &str,
        relation: MemoryLinkRelation,
    ) {
        connection
            .insert_memory_link(
                link_id,
                &CreateMemoryLinkInput {
                    src_memory_id: src.to_owned(),
                    dst_memory_id: dst.to_owned(),
                    relation,
                    weight: 1.0,
                    confidence: 1.0,
                    directed: false,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: MemoryLinkSource::Agent,
                    created_by: Some("contradiction-gather-test".to_owned()),
                    metadata_json: None,
                },
            )
            .expect("insert link");
    }

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

    #[test]
    fn gather_maps_contradicts_and_supersedes_links_to_explicit_signals() {
        let connection = open_seeded_db();
        for memory_id in [MEM_A, MEM_B, MEM_C] {
            seed_memory(&connection, memory_id);
        }
        seed_link(
            &connection,
            LINK_1,
            MEM_A,
            MEM_B,
            MemoryLinkRelation::Contradicts,
        );
        seed_link(
            &connection,
            LINK_2,
            MEM_B,
            MEM_C,
            MemoryLinkRelation::Supersedes,
        );

        let gathered = gather_explicit_conflict_edges(&connection);
        assert!(gathered.read_error.is_none(), "links read cleanly");
        assert_eq!(gathered.edges.len(), 2, "one edge per conflict link");
        assert!(gathered.edges.contains(&ConflictEdge::new(
            MEM_A,
            MEM_B,
            ExplicitConflictSignal::ContradictionLink
        )));
        assert!(gathered.edges.contains(&ConflictEdge::new(
            MEM_B,
            MEM_C,
            ExplicitConflictSignal::Supersession
        )));
    }

    #[test]
    fn gather_ignores_non_conflict_relations() {
        let connection = open_seeded_db();
        for memory_id in [MEM_A, MEM_B] {
            seed_memory(&connection, memory_id);
        }
        // Supports / Related are NOT explicit conflict evidence.
        seed_link(
            &connection,
            LINK_1,
            MEM_A,
            MEM_B,
            MemoryLinkRelation::Supports,
        );
        seed_link(
            &connection,
            LINK_2,
            MEM_A,
            MEM_B,
            MemoryLinkRelation::Related,
        );

        let gathered = gather_explicit_conflict_edges(&connection);
        assert!(
            gathered.edges.is_empty(),
            "non-conflict relations produce no conflict edges"
        );
    }

    #[test]
    fn gather_reports_deferred_signal_kinds_no_silent_omission() {
        let connection = open_seeded_db();
        let gathered = gather_explicit_conflict_edges(&connection);
        // v1 covers the link-based kinds and explicitly reports the rest as
        // deferred rather than pretending they were considered.
        assert!(
            gathered
                .gathered
                .contains(&ExplicitConflictSignal::ContradictionLink)
        );
        assert!(
            gathered
                .gathered
                .contains(&ExplicitConflictSignal::Supersession)
        );
        assert!(
            gathered
                .deferred
                .contains(&ExplicitConflictSignal::ValidityWindowOverlap),
            "an un-gathered signal kind is surfaced, never silently absent"
        );
        // Gathered and deferred kinds are disjoint and cover all six signals.
        assert_eq!(gathered.gathered.len() + gathered.deferred.len(), 6);
        for kind in &gathered.gathered {
            assert!(!gathered.deferred.contains(kind), "kinds are disjoint");
        }
    }

    #[test]
    fn gather_then_detect_surfaces_a_contradiction_cluster_end_to_end() {
        let connection = open_seeded_db();
        for memory_id in [MEM_A, MEM_B, MEM_C] {
            seed_memory(&connection, memory_id);
        }
        // A dense contradiction triangle of explicit links.
        seed_link(
            &connection,
            LINK_1,
            MEM_A,
            MEM_B,
            MemoryLinkRelation::Contradicts,
        );
        seed_link(
            &connection,
            LINK_2,
            MEM_B,
            MEM_C,
            MemoryLinkRelation::Contradicts,
        );
        seed_link(
            &connection,
            LINK_3,
            MEM_A,
            MEM_C,
            MemoryLinkRelation::Contradicts,
        );

        let (report, gathered) = detect_explicit_contradictions_from_connection(
            &connection,
            ContradictionDetectionConfig::default(),
        );
        assert_eq!(gathered.edges.len(), 3, "three explicit conflict links");
        assert_eq!(report.explicit_edge_count, 3);
        assert!(
            !report.clusters.is_empty(),
            "a dense explicit contradiction triangle surfaces a cluster"
        );
    }

    #[test]
    fn gather_on_empty_db_yields_no_edges_without_error() {
        let connection = open_seeded_db();
        let gathered = gather_explicit_conflict_edges(&connection);
        assert!(gathered.read_error.is_none());
        assert!(gathered.edges.is_empty(), "no links -> no conflict edges");
    }

    #[test]
    fn trust_class_rank_orders_human_above_agent_above_external() {
        assert!(trust_class_rank("human_explicit") > trust_class_rank("agent_validated"));
        assert!(trust_class_rank("agent_validated") > trust_class_rank("external"));
        // An unknown class ranks low-but-nonzero so it never outranks a known one.
        assert!(trust_class_rank("totally_unknown") < trust_class_rank("human_explicit"));
        assert!(trust_class_rank("totally_unknown") >= trust_class_rank("external"));
    }

    #[test]
    fn surface_pair_carries_both_bodies_and_prefers_higher_trust_side() {
        let connection = open_seeded_db();
        // MEM_A is human_explicit (higher trust), MEM_B is agent_assertion.
        seed_memory_trust(&connection, MEM_A, "human_explicit");
        seed_memory_trust(&connection, MEM_B, "agent_assertion");
        seed_link(
            &connection,
            LINK_1,
            MEM_A,
            MEM_B,
            MemoryLinkRelation::Contradicts,
        );

        let surface =
            assemble_conflict_surface(&connection, ContradictionDetectionConfig::default());
        assert_eq!(surface.schema, CONFLICT_SURFACE_SCHEMA_V1);
        assert_eq!(surface.pairs.len(), 1, "one conflicting pair");
        let pair = &surface.pairs[0];
        // Both bodies present.
        assert!(pair.memory_a.content.contains(MEM_A));
        assert!(pair.memory_b.content.contains(MEM_B));
        // The higher-trust side (canonical ordering puts MEM_A first) is preferred.
        assert_eq!(pair.preferred_side, "a");
        assert_eq!(pair.preferred_reason, "higher_trust");
        assert!(pair.memory_a.preferred && !pair.memory_b.preferred);
        assert_eq!(pair.signal, "contradiction_link");
        assert!(pair.load_bearing_milli > 0);
        assert!(surface.degraded.is_empty());
    }

    #[test]
    fn surface_reports_clusters_and_deferred_signals() {
        let connection = open_seeded_db();
        for memory_id in [MEM_A, MEM_B, MEM_C] {
            seed_memory(&connection, memory_id);
        }
        seed_link(
            &connection,
            LINK_1,
            MEM_A,
            MEM_B,
            MemoryLinkRelation::Contradicts,
        );
        seed_link(
            &connection,
            LINK_2,
            MEM_B,
            MEM_C,
            MemoryLinkRelation::Contradicts,
        );
        seed_link(
            &connection,
            LINK_3,
            MEM_A,
            MEM_C,
            MemoryLinkRelation::Contradicts,
        );

        let surface =
            assemble_conflict_surface(&connection, ContradictionDetectionConfig::default());
        assert_eq!(surface.pairs.len(), 3);
        assert!(
            !surface.clusters.is_empty(),
            "dense triangle surfaces a cluster"
        );
        // Deferred signal kinds are reported, never silently absent.
        assert!(
            surface
                .deferred_signals
                .iter()
                .any(|s| s == "validity_window_overlap")
        );
        assert!(
            surface
                .gathered_signals
                .iter()
                .any(|s| s == "contradiction_link")
        );
    }

    #[test]
    fn surface_focused_on_filters_to_the_named_memory() {
        let connection = open_seeded_db();
        for memory_id in [MEM_A, MEM_B, MEM_C] {
            seed_memory(&connection, memory_id);
        }
        // MEM_A<->MEM_B conflict; MEM_C is unrelated.
        seed_link(
            &connection,
            LINK_1,
            MEM_A,
            MEM_B,
            MemoryLinkRelation::Contradicts,
        );

        let surface =
            assemble_conflict_surface(&connection, ContradictionDetectionConfig::default());
        let focused = surface.focused_on(MEM_C);
        assert!(
            focused.pairs.is_empty(),
            "MEM_C participates in no conflict pair"
        );
        let focused_a = surface.focused_on(MEM_A);
        assert_eq!(focused_a.pairs.len(), 1, "MEM_A is in exactly one pair");
    }

    #[test]
    fn surface_is_deterministic_across_runs() {
        let connection = open_seeded_db();
        for memory_id in [MEM_A, MEM_B, MEM_C] {
            seed_memory(&connection, memory_id);
        }
        seed_link(
            &connection,
            LINK_1,
            MEM_A,
            MEM_B,
            MemoryLinkRelation::Contradicts,
        );
        seed_link(
            &connection,
            LINK_2,
            MEM_B,
            MEM_C,
            MemoryLinkRelation::Supersedes,
        );

        let first = assemble_conflict_surface(&connection, ContradictionDetectionConfig::default());
        let second =
            assemble_conflict_surface(&connection, ContradictionDetectionConfig::default());
        assert_eq!(first, second, "conflict surface is deterministic");
    }
}
