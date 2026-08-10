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
use crate::models::TrustClass;

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
    /// Canonical pairs a reviewed `both-valid` resolution marked as legitimate
    /// tension (`ee conflict resolve --verb both-valid` writes a `related` link
    /// with `resolution=both_valid` metadata): suppressed from the actionable
    /// pair surface, bd-3a1op.4.
    pub both_valid_resolved: std::collections::BTreeSet<(String, String)>,
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
                both_valid_resolved: std::collections::BTreeSet::new(),
            };
        }
    };

    let mut edges = Vec::new();
    let mut both_valid_resolved = std::collections::BTreeSet::new();
    for link in &links {
        let signal = match link.relation_enum() {
            Some(MemoryLinkRelation::Contradicts) => ExplicitConflictSignal::ContradictionLink,
            Some(MemoryLinkRelation::Supersedes) => ExplicitConflictSignal::Supersession,
            Some(MemoryLinkRelation::Related) => {
                // A reviewed both-valid resolution (bd-3a1op.4) marks the pair
                // as legitimate tension; record it so the surface suppresses
                // the pair instead of re-flagging a settled conflict.
                let resolved = link
                    .metadata_json
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                    .and_then(|meta| {
                        meta.get("resolution")
                            .and_then(serde_json::Value::as_str)
                            .map(|value| value == "both_valid")
                    })
                    .unwrap_or(false);
                if resolved {
                    let (a, b) = (link.src_memory_id.as_str(), link.dst_memory_id.as_str());
                    let pair = if a <= b { (a, b) } else { (b, a) };
                    both_valid_resolved.insert((pair.0.to_owned(), pair.1.to_owned()));
                }
                continue;
            }
            // Supports / DerivedFrom / CoTag / CoMention and any unparseable
            // relation are not explicit conflict evidence.
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
        both_valid_resolved,
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
/// memory trust-class vocabulary (the `memories.trust_class` CHECK set). Unknown
/// or corrupt classes rank below every known class so they never silently outrank
/// valid store data.
#[must_use]
pub fn trust_class_rank(trust_class: &str) -> u8 {
    match trust_class.parse::<TrustClass>() {
        Ok(TrustClass::HumanExplicit) => 6,
        Ok(TrustClass::PeerHumanAttested) => 5,
        Ok(TrustClass::AgentValidated) => 4,
        Ok(TrustClass::AgentAssertion) => 3,
        Ok(TrustClass::CassEvidence) => 2,
        Ok(TrustClass::LegacyImport) => 1,
        Err(_) => 0,
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
        // A reviewed both-valid resolution settled this tension: suppress.
        if gathered
            .both_valid_resolved
            .contains(&(low.clone(), high.clone()))
        {
            continue;
        }
        let (Ok(Some(a)), Ok(Some(b))) = (connection.get_memory(low), connection.get_memory(high))
        else {
            degraded.push(format!(
                "conflict pair {low}<->{high} skipped: a cited memory row could not be read"
            ));
            continue;
        };
        // A tombstoned side means the conflict was already resolved (superseded,
        // rejected, or expired): the pair is history, not an actionable conflict.
        // Deterministic — keyed on the persisted tombstone, never wall clock.
        if a.tombstoned_at.is_some() || b.tombstoned_at.is_some() {
            continue;
        }
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

// ---------------------------------------------------------------------------
// Conflict resolution planning (bd-3a1op.4, ADR 0066)
// ---------------------------------------------------------------------------

/// Wire schema id for the `ee conflict resolve` report.
pub const CONFLICT_RESOLVE_SCHEMA_V1: &str = "ee.conflict.resolve.v1";

/// Resolution verb vocabulary (ADR 0066 verb table).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResolveVerb {
    /// Keeper supersedes the loser: supersede link + validity close + decision.
    Supersede,
    /// Loser was simply wrong: expire it with the rationale on record.
    RejectOne,
    /// Both sides hold in different scopes: tag each side into its scope.
    ScopeSplit,
    /// The tension is legitimate: record a `related` link + the decision.
    BothValid,
}

impl ResolveVerb {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "supersede" => Some(Self::Supersede),
            "reject-one" => Some(Self::RejectOne),
            "scope-split" => Some(Self::ScopeSplit),
            "both-valid" => Some(Self::BothValid),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supersede => "supersede",
            Self::RejectOne => "reject-one",
            Self::ScopeSplit => "scope-split",
            Self::BothValid => "both-valid",
        }
    }
}

/// One planned mutation atom. Every atom maps 1:1 onto an EXISTING audited
/// core operation (`decide_record`, `expire_memory`, `update_memory_link`,
/// `update_memory_tags`) — the plan never introduces a novel mutation path.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum PlannedResolutionAction {
    /// `decide_record` — when `supersedes` is set the one atom also creates
    /// the supersede link and closes the loser's validity window.
    #[serde(rename_all = "camelCase")]
    RecordDecision {
        topic: String,
        chosen: String,
        alternatives: Vec<String>,
        supersedes: Option<String>,
    },
    /// `expire_memory` — audited soft expiration.
    #[serde(rename_all = "camelCase")]
    ExpireMemory { memory_id: String, reason: String },
    /// `update_memory_link` create — audited explicit typed link.
    #[serde(rename_all = "camelCase")]
    CreateLink {
        from: String,
        to: String,
        relation: String,
    },
    /// `update_memory_tags` patch(add) — audited scope tagging.
    #[serde(rename_all = "camelCase")]
    AddTags {
        memory_id: String,
        tags: Vec<String>,
    },
}

/// The dry-run-visible mutation plan for one conflict pair.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictResolutionPlan {
    pub conflict_id: String,
    pub verb: ResolveVerb,
    pub memory_a: String,
    pub memory_b: String,
    pub keep: Option<String>,
    pub lose: Option<String>,
    pub actions: Vec<PlannedResolutionAction>,
}

/// Inputs to planning, already parsed by the CLI layer.
#[derive(Clone, Debug)]
pub struct ConflictResolveRequest<'a> {
    pub memory_a: &'a str,
    pub memory_b: &'a str,
    pub verb: ResolveVerb,
    pub keep: Option<&'a str>,
    pub reason: &'a str,
    pub scope_a_tags: Vec<String>,
    pub scope_b_tags: Vec<String>,
}

/// Planning outcome. Refusals are data, not errors, so the CLI can emit the
/// honest degraded/policy envelope for each case.
#[derive(Clone, Debug, PartialEq)]
pub enum ConflictResolutionOutcome {
    Plan(ConflictResolutionPlan),
    /// The pair is not on the CURRENT conflict surface (state moved since the
    /// agent ran explain). Carries the focused live view for re-orientation.
    StaleSurface {
        current_pairs: Vec<ConflictPairView>,
    },
    /// Policy refuses the mutation (exit 7): destructive verb against a
    /// human-explicit rule memory.
    PolicyDenied {
        message: String,
        repair: String,
    },
    /// The request itself is malformed (missing/invalid --keep, scopes, ...).
    InvalidRequest {
        message: String,
        repair: String,
    },
}

fn decision_topic(pair: &ConflictPairView) -> String {
    format!("conflict:{}", pair.conflict_id)
}

fn head(content: &str) -> String {
    const MAX: usize = 96;
    let oneline = content.replace('\n', " ");
    if oneline.chars().count() <= MAX {
        oneline
    } else {
        let truncated: String = oneline.chars().take(MAX).collect();
        format!("{truncated}…")
    }
}

/// Pure planner: re-checks the pair against the live surface, enforces verb
/// argument rules and the destructive-verb policy, then maps the verb onto
/// existing audited atoms per the ADR 0066 verb table.
#[must_use]
pub fn plan_conflict_resolution(
    surface: &ConflictSurface,
    request: &ConflictResolveRequest<'_>,
) -> ConflictResolutionOutcome {
    let pair = surface.pairs.iter().find(|pair| {
        (pair.memory_a.id == request.memory_a && pair.memory_b.id == request.memory_b)
            || (pair.memory_a.id == request.memory_b && pair.memory_b.id == request.memory_a)
    });
    let Some(pair) = pair else {
        let mut current: Vec<ConflictPairView> = surface
            .pairs
            .iter()
            .filter(|pair| {
                [request.memory_a, request.memory_b]
                    .iter()
                    .any(|id| pair.memory_a.id == *id || pair.memory_b.id == *id)
            })
            .cloned()
            .collect();
        current.truncate(8);
        return ConflictResolutionOutcome::StaleSurface {
            current_pairs: current,
        };
    };

    let needs_keep = matches!(
        request.verb,
        ResolveVerb::Supersede | ResolveVerb::RejectOne
    );
    let (keep, lose) = if needs_keep {
        let Some(keep) = request.keep else {
            return ConflictResolutionOutcome::InvalidRequest {
                message: format!(
                    "--verb {} requires --keep <memory-id> naming the surviving side.",
                    request.verb.as_str()
                ),
                repair: format!(
                    "ee conflict resolve {} {} --verb {} --keep {} --reason \"...\" --json",
                    request.memory_a,
                    request.memory_b,
                    request.verb.as_str(),
                    request.memory_a
                ),
            };
        };
        if keep != pair.memory_a.id && keep != pair.memory_b.id {
            return ConflictResolutionOutcome::InvalidRequest {
                message: format!("--keep {keep} is neither side of this conflict pair."),
                repair: format!(
                    "Pass --keep {} or --keep {}.",
                    pair.memory_a.id, pair.memory_b.id
                ),
            };
        }
        let lose = if keep == pair.memory_a.id {
            pair.memory_b.id.clone()
        } else {
            pair.memory_a.id.clone()
        };
        (Some(keep.to_owned()), Some(lose))
    } else {
        (None, None)
    };

    if let Some(lose_id) = lose.as_deref() {
        let loser = if pair.memory_a.id == lose_id {
            &pair.memory_a
        } else {
            &pair.memory_b
        };
        if request.verb == ResolveVerb::RejectOne
            && loser.kind == "rule"
            && loser.trust_class == "human_explicit"
        {
            return ConflictResolutionOutcome::PolicyDenied {
                message: format!(
                    "reject-one refuses to expire {lose_id}: it is a human-explicit rule; \
                     rejecting it outright requires a human decision."
                ),
                repair: "Use --verb supersede (records provenance + the decision) or have a \
                         human run `ee memory expire` directly."
                    .to_owned(),
            };
        }
    }

    let loser_head = lose.as_deref().map(|id| {
        if pair.memory_a.id == id {
            head(&pair.memory_a.content)
        } else {
            head(&pair.memory_b.content)
        }
    });

    let actions = match request.verb {
        ResolveVerb::Supersede => {
            // One existing atom: decide_record(supersedes=loser) creates the
            // decision memory, the supersede link, AND closes the loser.
            vec![PlannedResolutionAction::RecordDecision {
                topic: decision_topic(pair),
                chosen: format!("keep {}", keep.as_deref().unwrap_or_default()),
                alternatives: loser_head.into_iter().collect(),
                supersedes: lose.clone(),
            }]
        }
        ResolveVerb::RejectOne => vec![
            PlannedResolutionAction::ExpireMemory {
                memory_id: lose.clone().unwrap_or_default(),
                reason: request.reason.to_owned(),
            },
            PlannedResolutionAction::RecordDecision {
                topic: decision_topic(pair),
                chosen: format!(
                    "keep {}; reject the other side",
                    keep.as_deref().unwrap_or_default()
                ),
                alternatives: loser_head.into_iter().collect(),
                supersedes: None,
            },
        ],
        ResolveVerb::ScopeSplit => {
            if request.scope_a_tags.is_empty() || request.scope_b_tags.is_empty() {
                return ConflictResolutionOutcome::InvalidRequest {
                    message: "--verb scope-split requires --scope-a-tags and --scope-b-tags \
                              (comma-separated, both non-empty)."
                        .to_owned(),
                    repair: format!(
                        "ee conflict resolve {} {} --verb scope-split --scope-a-tags rust \
                         --scope-b-tags python --reason \"...\" --json",
                        request.memory_a, request.memory_b
                    ),
                };
            }
            vec![
                PlannedResolutionAction::AddTags {
                    memory_id: pair.memory_a.id.clone(),
                    tags: request.scope_a_tags.clone(),
                },
                PlannedResolutionAction::AddTags {
                    memory_id: pair.memory_b.id.clone(),
                    tags: request.scope_b_tags.clone(),
                },
                PlannedResolutionAction::RecordDecision {
                    topic: decision_topic(pair),
                    chosen: format!(
                        "scope-split: {} → [{}]; {} → [{}]",
                        pair.memory_a.id,
                        request.scope_a_tags.join(","),
                        pair.memory_b.id,
                        request.scope_b_tags.join(",")
                    ),
                    alternatives: Vec::new(),
                    supersedes: None,
                },
            ]
        }
        ResolveVerb::BothValid => vec![
            PlannedResolutionAction::CreateLink {
                from: pair.memory_a.id.clone(),
                to: pair.memory_b.id.clone(),
                relation: "related".to_owned(),
            },
            PlannedResolutionAction::RecordDecision {
                topic: decision_topic(pair),
                chosen: "both-valid: the tension is legitimate; both memories stand".to_owned(),
                alternatives: Vec::new(),
                supersedes: None,
            },
        ],
    };

    ConflictResolutionOutcome::Plan(ConflictResolutionPlan {
        conflict_id: pair.conflict_id.clone(),
        verb: request.verb,
        memory_a: pair.memory_a.id.clone(),
        memory_b: pair.memory_b.id.clone(),
        keep,
        lose,
        actions,
    })
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
    fn trust_class_rank_follows_the_canonical_store_taxonomy() {
        assert!(trust_class_rank("human_explicit") > trust_class_rank("peer_human_attested"));
        assert!(trust_class_rank("peer_human_attested") > trust_class_rank("agent_validated"));
        assert!(trust_class_rank("agent_validated") > trust_class_rank("agent_assertion"));
        assert!(trust_class_rank("agent_assertion") > trust_class_rank("cass_evidence"));
        assert!(trust_class_rank("cass_evidence") > trust_class_rank("legacy_import"));
        // Unknown/corrupt classes rank below every valid DB trust class.
        assert!(trust_class_rank("legacy_import") > trust_class_rank("totally_unknown"));
        assert_eq!(trust_class_rank("external"), 0);
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
    fn surface_drops_pairs_with_a_tombstoned_side() {
        // A resolved conflict (loser expired/superseded → tombstoned) must
        // leave the actionable surface: this is what makes `ee conflict
        // resolve` terminal and its stale-surface re-run refusal real.
        let connection = open_seeded_db();
        seed_memory_trust(&connection, MEM_A, "human_explicit");
        seed_memory_trust(&connection, MEM_B, "agent_assertion");
        seed_link(
            &connection,
            LINK_1,
            MEM_A,
            MEM_B,
            MemoryLinkRelation::Contradicts,
        );
        let before =
            assemble_conflict_surface(&connection, ContradictionDetectionConfig::default());
        assert_eq!(before.pairs.len(), 1, "live pair surfaces first");

        assert!(connection.tombstone_memory(MEM_B).expect("tombstone loser"));
        let after = assemble_conflict_surface(&connection, ContradictionDetectionConfig::default());
        assert!(
            after.pairs.is_empty(),
            "tombstoned side must drop the pair from the actionable surface"
        );
    }

    #[test]
    fn surface_suppresses_both_valid_resolved_pairs() {
        // `ee conflict resolve --verb both-valid` records a related link with
        // resolution metadata; the settled pair must stop being re-flagged.
        let connection = open_seeded_db();
        seed_memory(&connection, MEM_A);
        seed_memory(&connection, MEM_B);
        seed_link(
            &connection,
            LINK_1,
            MEM_A,
            MEM_B,
            MemoryLinkRelation::Contradicts,
        );
        connection
            .insert_memory_link(
                LINK_2,
                &CreateMemoryLinkInput {
                    src_memory_id: MEM_A.to_owned(),
                    dst_memory_id: MEM_B.to_owned(),
                    relation: MemoryLinkRelation::Related,
                    weight: 1.0,
                    confidence: 1.0,
                    directed: false,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: MemoryLinkSource::Agent,
                    created_by: Some("conflict-resolve-test".to_owned()),
                    metadata_json: Some(
                        "{\"resolution\":\"both_valid\",\"conflictId\":\"cfl_x\"}".to_owned(),
                    ),
                },
            )
            .expect("insert resolution link");

        let surface =
            assemble_conflict_surface(&connection, ContradictionDetectionConfig::default());
        assert!(
            surface.pairs.is_empty(),
            "both-valid-resolved pair must be suppressed: {:?}",
            surface.pairs
        );

        // A plain related link WITHOUT the resolution marker must not suppress.
        let connection = open_seeded_db();
        seed_memory(&connection, MEM_A);
        seed_memory(&connection, MEM_B);
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
            MEM_A,
            MEM_B,
            MemoryLinkRelation::Related,
        );
        let surface =
            assemble_conflict_surface(&connection, ContradictionDetectionConfig::default());
        assert_eq!(
            surface.pairs.len(),
            1,
            "plain related link is not a resolution"
        );
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

    // ---- resolution planner (bd-3a1op.4) ---------------------------------

    use super::{
        ConflictMemberView, ConflictPairView, ConflictResolutionOutcome, ConflictResolveRequest,
        ConflictSurface, PlannedResolutionAction, ResolveVerb, plan_conflict_resolution,
    };

    fn member(id: &str, kind: &str, trust_class: &str) -> ConflictMemberView {
        ConflictMemberView {
            id: id.to_owned(),
            content: format!("content of {id}"),
            level: "semantic".to_owned(),
            kind: kind.to_owned(),
            trust_class: trust_class.to_owned(),
            trust_rank: 2,
            confidence: 0.8,
            importance: 0.5,
            valid_from: None,
            valid_to: None,
            updated_at: "2026-08-10T00:00:00Z".to_owned(),
            preferred: false,
        }
    }

    fn fixture_surface() -> ConflictSurface {
        ConflictSurface {
            schema: CONFLICT_SURFACE_SCHEMA_V1,
            pairs: vec![ConflictPairView {
                conflict_id: "cfl_fixture_pair".to_owned(),
                signal: "polarity_opposition".to_owned(),
                load_bearing_milli: 500,
                preferred_side: "a".to_owned(),
                preferred_reason: "higher_trust".to_owned(),
                memory_a: member(MEM_A, "fact", "agent_inferred"),
                memory_b: member(MEM_B, "fact", "agent_inferred"),
            }],
            clusters: Vec::new(),
            explicit_edge_count: 1,
            gathered_signals: Vec::new(),
            deferred_signals: Vec::new(),
            fuzzy_near_conflict_skipped: false,
            degraded: Vec::new(),
        }
    }

    fn request<'a>(verb: ResolveVerb, keep: Option<&'a str>) -> ConflictResolveRequest<'a> {
        ConflictResolveRequest {
            memory_a: MEM_A,
            memory_b: MEM_B,
            verb,
            keep,
            reason: "test rationale",
            scope_a_tags: Vec::new(),
            scope_b_tags: Vec::new(),
        }
    }

    #[test]
    fn supersede_plans_the_single_decide_record_atom() {
        let outcome = plan_conflict_resolution(
            &fixture_surface(),
            &request(ResolveVerb::Supersede, Some(MEM_A)),
        );
        let ConflictResolutionOutcome::Plan(plan) = outcome else {
            panic!("expected a plan, got {outcome:?}");
        };
        assert_eq!(plan.keep.as_deref(), Some(MEM_A));
        assert_eq!(plan.lose.as_deref(), Some(MEM_B));
        assert_eq!(plan.actions.len(), 1, "supersede is ONE decide_record atom");
        let PlannedResolutionAction::RecordDecision { supersedes, .. } = &plan.actions[0] else {
            panic!("expected RecordDecision, got {:?}", plan.actions[0]);
        };
        assert_eq!(supersedes.as_deref(), Some(MEM_B));
    }

    #[test]
    fn reject_one_plans_expire_then_decision_and_reversed_keep_resolves_loser() {
        // keep=b exercises the orientation-independent keep/lose resolution.
        let outcome = plan_conflict_resolution(
            &fixture_surface(),
            &request(ResolveVerb::RejectOne, Some(MEM_B)),
        );
        let ConflictResolutionOutcome::Plan(plan) = outcome else {
            panic!("expected a plan, got {outcome:?}");
        };
        assert_eq!(plan.lose.as_deref(), Some(MEM_A));
        assert!(matches!(
            &plan.actions[0],
            PlannedResolutionAction::ExpireMemory { memory_id, reason }
                if memory_id == MEM_A && reason == "test rationale"
        ));
        assert!(matches!(
            &plan.actions[1],
            PlannedResolutionAction::RecordDecision {
                supersedes: None,
                ..
            }
        ));
    }

    #[test]
    fn keep_required_verbs_refuse_without_keep() {
        for verb in [ResolveVerb::Supersede, ResolveVerb::RejectOne] {
            let outcome = plan_conflict_resolution(&fixture_surface(), &request(verb, None));
            assert!(
                matches!(outcome, ConflictResolutionOutcome::InvalidRequest { .. }),
                "{} without --keep must refuse",
                verb.as_str()
            );
        }
        let outcome = plan_conflict_resolution(
            &fixture_surface(),
            &request(ResolveVerb::Supersede, Some(MEM_C)),
        );
        assert!(
            matches!(outcome, ConflictResolutionOutcome::InvalidRequest { .. }),
            "--keep naming a non-member must refuse"
        );
    }

    #[test]
    fn stale_pair_refuses_with_focused_current_state() {
        let outcome = plan_conflict_resolution(
            &fixture_surface(),
            &ConflictResolveRequest {
                memory_a: MEM_A,
                memory_b: MEM_C, // (a,c) is NOT a live pair; (a,b) is
                verb: ResolveVerb::BothValid,
                keep: None,
                reason: "r",
                scope_a_tags: Vec::new(),
                scope_b_tags: Vec::new(),
            },
        );
        let ConflictResolutionOutcome::StaleSurface { current_pairs } = outcome else {
            panic!("expected StaleSurface, got {outcome:?}");
        };
        assert_eq!(
            current_pairs.len(),
            1,
            "focused view carries the live (a,b) pair"
        );
        assert_eq!(current_pairs[0].memory_a.id, MEM_A);
    }

    #[test]
    fn reject_one_of_human_explicit_rule_is_policy_denied() {
        let mut surface = fixture_surface();
        surface.pairs[0].memory_b = member(MEM_B, "rule", "human_explicit");
        let outcome =
            plan_conflict_resolution(&surface, &request(ResolveVerb::RejectOne, Some(MEM_A)));
        assert!(
            matches!(outcome, ConflictResolutionOutcome::PolicyDenied { .. }),
            "expiring a human-explicit rule via reject-one must be policy-denied, got {outcome:?}"
        );
        // supersede of the same memory stays allowed (records provenance).
        let outcome =
            plan_conflict_resolution(&surface, &request(ResolveVerb::Supersede, Some(MEM_A)));
        assert!(matches!(outcome, ConflictResolutionOutcome::Plan(_)));
    }

    #[test]
    fn scope_split_requires_both_scopes_and_plans_tags_then_decision() {
        let outcome =
            plan_conflict_resolution(&fixture_surface(), &request(ResolveVerb::ScopeSplit, None));
        assert!(matches!(
            outcome,
            ConflictResolutionOutcome::InvalidRequest { .. }
        ));

        let mut req = request(ResolveVerb::ScopeSplit, None);
        req.scope_a_tags = vec!["rust".to_owned()];
        req.scope_b_tags = vec!["python".to_owned()];
        let outcome = plan_conflict_resolution(&fixture_surface(), &req);
        let ConflictResolutionOutcome::Plan(plan) = outcome else {
            panic!("expected a plan, got {outcome:?}");
        };
        assert_eq!(plan.actions.len(), 3);
        assert!(matches!(
            &plan.actions[0],
            PlannedResolutionAction::AddTags { memory_id, tags }
                if memory_id == MEM_A && tags == &vec!["rust".to_owned()]
        ));
        assert!(matches!(
            &plan.actions[2],
            PlannedResolutionAction::RecordDecision { .. }
        ));
    }

    #[test]
    fn both_valid_plans_related_link_then_decision() {
        let outcome =
            plan_conflict_resolution(&fixture_surface(), &request(ResolveVerb::BothValid, None));
        let ConflictResolutionOutcome::Plan(plan) = outcome else {
            panic!("expected a plan, got {outcome:?}");
        };
        assert_eq!(plan.keep, None);
        assert!(matches!(
            &plan.actions[0],
            PlannedResolutionAction::CreateLink { relation, .. } if relation == "related"
        ));
        assert!(matches!(
            &plan.actions[1],
            PlannedResolutionAction::RecordDecision {
                supersedes: None,
                ..
            }
        ));
    }
}
