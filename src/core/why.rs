//! Memory selection explanation (EE-150).
//!
//! Provides the `ee why <memory-id>` command which explains:
//! - How a memory was stored (provenance, trust class)
//! - How it would be retrieved (scoring factors)
//! - How it would be selected for packs (relevance, utility, importance)
//! - Related memory links (supports, contradicts, derived_from, etc.)
//!
//! This makes the system explainable and auditable.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::Instant,
};

use crate::config::GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY;
use crate::core::degraded_aggregation::{DegradationAggregationInput, aggregate_degraded_entries};
use crate::core::memory::{
    EvidenceFreshness, EvidenceFreshnessStatus, assess_memory_evidence_freshness, memory_validity,
};
use crate::db::{DbConnection, generate_audit_id, generate_audit_id_seeded};
use crate::models::{
    AGENT_CONTEXT_PROFILE_SCHEMA_V1, AGENT_PROFILE_BIAS_CAP, AGENT_PROFILE_COLD_START_OUTCOMES,
    AgentContextProfileCounts, RationaleTrace, RationaleTraceVisibility,
    VerificationEvidenceRecord,
};
use crate::runtime::determinism::{Deterministic, Seed};
use sqlmodel_core::{Row, Value};

/// Why a memory was stored with certain characteristics.
#[derive(Clone, Debug, PartialEq)]
pub struct StorageExplanation {
    /// How the memory was created (import, remember, curate).
    pub origin: String,
    /// Trust class assigned at creation.
    pub trust_class: String,
    /// Trust subclass if applicable.
    pub trust_subclass: Option<String>,
    /// Original provenance URI.
    pub provenance_uri: Option<String>,
    /// Optional workflow lifecycle group.
    pub workflow_id: Option<String>,
    /// When the memory was created.
    pub created_at: String,
    /// RFC3339 timestamp when this memory becomes applicable.
    pub valid_from: Option<String>,
    /// RFC3339 timestamp when this memory stops being applicable.
    pub valid_to: Option<String>,
    /// Current validity status computed from the stored validity window.
    pub validity_status: String,
    /// Stable shape of the validity window.
    pub validity_window_kind: String,
}

/// Why a memory would be retrieved by search.
#[derive(Clone, Debug, PartialEq)]
pub struct RetrievalExplanation {
    /// Base confidence score (0.0-1.0).
    pub confidence: f32,
    /// Utility score for retrieval ranking.
    pub utility: f32,
    /// Importance score for priority.
    pub importance: f32,
    /// Tags that improve retrieval.
    pub tags: Vec<String>,
    /// Memory level (procedural, episodic, semantic).
    pub level: String,
    /// Memory kind (rule, decision, failure, etc.).
    pub kind: String,
}

/// Graph-derived retrieval features used to explain why a memory may be ranked.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphRetrievalExplanation {
    /// Availability status for graph-derived features.
    pub status: String,
    /// Pinned source for the graph feature state.
    pub source: GraphRetrievalSourceExplanation,
    /// Combined graph centrality score used as the retrieval feature.
    pub centrality_score: f64,
    /// Authority proxy. Uses normalized PageRank until HITS authority is available.
    pub authority_score: f64,
    /// Hub proxy. Uses normalized PageRank until HITS hub scores are available.
    pub hub_score: f64,
    /// Optional graph community identifier when community detection is available.
    pub community_id: Option<String>,
    /// Distance from this memory to the query seed, when query-seed expansion is available.
    pub distance_to_query_seed: Option<u32>,
    /// Whether this memory is in the same cluster as the top result.
    pub same_cluster_as_top_result: Option<bool>,
    /// Count of supporting evidence edges incident to this memory.
    pub evidence_support_count: u32,
    /// Count of contradiction edges or feedback events incident to this memory.
    pub contradiction_count: u32,
    /// Penalty applied when a memory is graph-isolated.
    pub orphan_penalty: f64,
    /// Penalty applied when an expired memory is a bridge in the graph.
    pub stale_bridge_penalty: f64,
    /// Raw PageRank score from the graph snapshot.
    pub pagerank: GraphMetricExplanation,
    /// Raw betweenness score from the graph snapshot.
    pub betweenness: GraphMetricExplanation,
    /// Human-readable graph labels.
    pub labels: Vec<String>,
    /// Human-readable graph reasons.
    pub reasons: Vec<String>,
    /// Stable formula for centrality_score.
    pub centrality_formula: String,
    /// Stable formula for orphan_penalty.
    pub orphan_penalty_formula: String,
    /// Stable formula for stale_bridge_penalty.
    pub stale_bridge_penalty_formula: String,
    /// Graph-specific degradations. These do not make `ee why` fail.
    pub degraded: Vec<WhyDegradation>,
}

/// Source metadata for graph retrieval features.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphRetrievalSourceExplanation {
    /// Source kind: graph_snapshot, live_centrality, or unavailable.
    pub kind: String,
    /// Workspace used for graph feature lookup.
    pub workspace_id: Option<String>,
    /// Graph type used for the feature lookup.
    pub graph_type: Option<String>,
    /// Snapshot witness when graph features came from persisted graph state.
    pub snapshot: Option<GraphRetrievalSnapshotExplanation>,
}

/// Snapshot witness for graph retrieval features.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphRetrievalSnapshotExplanation {
    /// Snapshot ID.
    pub id: String,
    /// Snapshot schema version.
    pub schema_version: String,
    /// Monotonic snapshot version.
    pub snapshot_version: u32,
    /// Source generation captured by the snapshot.
    pub source_generation: u32,
    /// Snapshot status.
    pub status: String,
    /// Snapshot content hash.
    pub content_hash: String,
    /// Snapshot creation timestamp.
    pub created_at: String,
}

/// One graph metric used by retrieval explanation.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphMetricExplanation {
    /// Raw metric value.
    pub raw: f64,
    /// Normalized metric value.
    pub normalized: f64,
    /// One-based rank when available.
    pub rank: Option<usize>,
    /// Metric weight in the centrality formula.
    pub weight: f64,
    /// Weighted contribution.
    pub contribution: f64,
    /// Stable formula for this contribution.
    pub formula: String,
}

/// Why a memory would be selected for a context pack.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectionExplanation {
    /// Combined selection score.
    pub selection_score: f32,
    /// Whether this memory would pass the confidence threshold.
    pub above_confidence_threshold: bool,
    /// Whether the memory is active (not tombstoned).
    pub is_active: bool,
    /// Explanation of how scores combine.
    pub score_breakdown: String,
    /// Most recent persisted context-pack selection for this memory, if any.
    pub latest_pack_selection: Option<PackSelectionExplanation>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentProfileSelectionExplanation {
    pub schema: &'static str,
    pub agent_name: String,
    pub agent_name_hash: String,
    pub helpful_count: u32,
    pub harmful_count: u32,
    pub ignored_count: u32,
    pub observed_outcomes: u32,
    pub bias: f64,
    pub max_bias_magnitude: f64,
    pub cold_start: bool,
    pub cold_start_threshold: u32,
    pub last_seen_at: String,
}

/// Memory lifecycle state surfaced by `ee why`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LifecycleExplanation {
    /// Stable lifecycle status.
    pub status: &'static str,
    /// Tombstone timestamp when the memory was hard-tombstoned.
    pub tombstoned_at: Option<String>,
    /// Operator-supplied tombstone reason when present in audit history.
    pub tombstoned_reason: Option<String>,
}

/// A persisted context-pack selection involving the memory.
#[derive(Clone, Debug, PartialEq)]
pub struct PackSelectionExplanation {
    /// Pack record ID.
    pub pack_id: String,
    /// Query that produced the pack.
    pub query: String,
    /// Pack profile.
    pub profile: String,
    /// One-based rank inside the pack.
    pub rank: u32,
    /// Context pack section.
    pub section: String,
    /// Estimated token cost recorded for the item.
    pub estimated_tokens: u32,
    /// Relevance score recorded when the pack was assembled.
    pub relevance: f32,
    /// Utility score recorded when the pack was assembled.
    pub utility: f32,
    /// Pack item's persisted why text.
    pub why: String,
    /// Persisted pack hash.
    pub pack_hash: String,
    /// Pack creation timestamp.
    pub selected_at: String,
}

/// Contradiction feedback recorded against this memory (EE-263).
#[derive(Clone, Debug, PartialEq)]
pub struct ContradictionMetadata {
    /// Feedback event ID.
    pub event_id: String,
    /// Weight of the contradiction signal.
    pub weight: f32,
    /// Source type (agent_inference, human_request, etc.).
    pub source_type: String,
    /// Reason for the contradiction.
    pub reason: Option<String>,
    /// When the contradiction was recorded.
    pub created_at: String,
    /// Whether the contradiction has been applied to scores.
    pub applied: bool,
}

/// Summary of a memory link for why output (EE-LINK-USAGE-001).
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryLinkSummary {
    /// Link ID.
    pub link_id: String,
    /// The related memory ID (the "other" side of the link).
    pub linked_memory_id: String,
    /// Relation type (supports, contradicts, derived_from, etc.).
    pub relation: String,
    /// Direction relative to the queried memory: "outgoing" (this memory -> linked),
    /// "incoming" (linked -> this memory), or "undirected".
    pub direction: String,
    /// Confidence score for this link (0.0-1.0).
    pub confidence: f32,
    /// Weight of the link edge.
    pub weight: f32,
    /// Number of evidence instances supporting this link.
    pub evidence_count: u32,
    /// Source that created the link (agent, auto, import, human).
    pub source: String,
    /// When the link was created.
    pub created_at: String,
}

/// A single audit timeline entry included in why output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryHistorySummaryEntry {
    /// Audit entry ID.
    pub audit_id: String,
    /// Timestamp of the event.
    pub timestamp: String,
    /// Actor who performed the action, if known.
    pub actor: Option<String>,
    /// Action recorded in the audit log.
    pub action: String,
    /// Audit details payload, usually JSON.
    pub details: Option<String>,
}

/// Memory history projection bundled into why output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryHistorySummary {
    /// History entries ordered newest first.
    pub entries: Vec<MemoryHistorySummaryEntry>,
    /// Total number of audit entries for this memory before truncation.
    pub total_count: u32,
    /// Whether entries were truncated by the why history limit.
    pub truncated: bool,
}

/// Visible rationale trace evidence linked to a why report.
#[derive(Clone, Debug, PartialEq)]
pub struct RationaleTraceSummary {
    /// Rationale trace schema.
    pub schema: &'static str,
    /// Stable rationale trace ID.
    pub trace_id: String,
    /// Visible rationale kind: hypothesis, decision, question, etc.
    pub kind: String,
    /// Evidence posture: asserted, supported, contradicted, or unresolved.
    pub posture: String,
    /// Visibility/redaction class.
    pub visibility: String,
    /// Author or source label for the visible rationale summary.
    pub author: String,
    /// Concise user/agent-visible rationale summary.
    pub summary: String,
    /// Confidence in basis points, 0..=10000.
    pub confidence_basis_points: u16,
    /// Evidence URIs supporting the rationale.
    pub evidence_uris: Vec<String>,
    /// Memory IDs linked to the rationale.
    pub linked_memory_ids: Vec<String>,
    /// Context pack IDs linked to the rationale.
    pub linked_context_pack_ids: Vec<String>,
    /// Recorder run IDs linked to the rationale.
    pub linked_recorder_run_ids: Vec<String>,
    /// Recorder event IDs linked to the rationale.
    pub linked_recorder_event_ids: Vec<String>,
    /// Causal trace IDs that reuse this rationale.
    pub linked_causal_trace_ids: Vec<String>,
    /// Prior rationale trace IDs superseded by this trace.
    pub supersedes_trace_ids: Vec<String>,
    /// Rationale trace IDs that contradict this trace.
    pub contradicted_by_trace_ids: Vec<String>,
    /// Creation timestamp.
    pub created_at: String,
}

impl RationaleTraceSummary {
    /// Build a why-safe summary from a persisted rationale trace.
    #[must_use]
    pub fn from_trace(trace: &RationaleTrace) -> Option<Self> {
        if !trace.visibility.is_storable() {
            return None;
        }

        Some(Self {
            schema: trace.schema,
            trace_id: trace.trace_id.clone(),
            kind: trace.kind.as_str().to_string(),
            posture: trace.posture.as_str().to_string(),
            visibility: trace.visibility.as_str().to_string(),
            author: trace.author.clone(),
            summary: trace.summary.clone(),
            confidence_basis_points: trace.confidence_basis_points,
            evidence_uris: trace.evidence_uris.clone(),
            linked_memory_ids: trace.linked_memory_ids.clone(),
            linked_context_pack_ids: trace.linked_context_pack_ids.clone(),
            linked_recorder_run_ids: trace.linked_recorder_run_ids.clone(),
            linked_recorder_event_ids: trace.linked_recorder_event_ids.clone(),
            linked_causal_trace_ids: trace.linked_causal_trace_ids.clone(),
            supersedes_trace_ids: trace.supersedes_trace_ids.clone(),
            contradicted_by_trace_ids: trace.contradicted_by_trace_ids.clone(),
            created_at: trace.created_at.clone(),
        })
    }

    fn is_visible_for_report(&self) -> bool {
        let Ok(visibility) = self.visibility.parse::<RationaleTraceVisibility>() else {
            return false;
        };

        visibility.is_storable()
            && !self.trace_id.trim().is_empty()
            && !self.summary.trim().is_empty()
    }
}

/// Non-fatal limitations in the why explanation.
/// Bayesian (alpha, beta) posterior summary for `ee why <memory-id>`
/// output (N7.1 / ADR 0032).
///
/// Mirrors the runtime [`crate::core::bayes::BetaPosterior`] state plus
/// derived fields (mean, credible intervals, effective sample size).
/// Renderers project this struct into JSON / markdown / TOON without
/// re-computing the math — the renderer is a thin formatting layer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BayesPosteriorSummary {
    /// Alpha (helpful-side pseudo-count).
    pub alpha: f64,
    /// Beta (harmful-side pseudo-count).
    pub beta: f64,
    /// Posterior mean = alpha / (alpha + beta). Equals the legacy
    /// `confidence` field's derived view for backward compatibility.
    pub mean: f64,
    /// Effective sample size (alpha + beta). Trust-class transitions
    /// gate on this per ADR 0032.
    pub effective_sample_size: f64,
    /// 90% equal-tailed credible interval `(lo, hi)`. `None` if the
    /// inverse-CDF iteration failed to converge.
    pub credible_interval_90: Option<(f64, f64)>,
    /// 50% equal-tailed credible interval `(lo, hi)`. Useful for
    /// compact-format rendering that drops the wider 90% interval.
    pub credible_interval_50: Option<(f64, f64)>,
}

impl BayesPosteriorSummary {
    /// Compute a summary from a runtime [`BetaPosterior`].
    #[must_use]
    pub fn from_posterior(posterior: &crate::core::bayes::BetaPosterior) -> Self {
        Self {
            alpha: posterior.alpha(),
            beta: posterior.beta(),
            mean: posterior.mean(),
            effective_sample_size: posterior.effective_sample_size(),
            credible_interval_90: posterior.credible_interval(0.90),
            credible_interval_50: posterior.credible_interval(0.50),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WhyDegradation {
    /// Stable degradation code.
    pub code: &'static str,
    /// Stable severity.
    pub severity: &'static str,
    /// Human-readable message.
    pub message: String,
    /// Suggested repair command when available.
    pub repair: Option<String>,
}

/// Complete why report for a memory.
#[derive(Clone, Debug)]
pub struct WhyReport {
    /// Package version for stable output.
    pub version: &'static str,
    /// Memory ID that was queried.
    pub memory_id: String,
    /// Whether the memory was found.
    pub found: bool,
    /// Full memory body text when the memory was found. `None` when the memory
    /// is not found or an error occurred. The why surface returns the full
    /// body (no truncation) so an agent does not need to chain a separate
    /// `ee show` call to read it.
    pub content: Option<String>,
    /// Storage explanation.
    pub storage: Option<StorageExplanation>,
    /// Retrieval explanation.
    pub retrieval: Option<RetrievalExplanation>,
    /// Graph-derived retrieval features and gaps.
    pub graph_retrieval: Option<GraphRetrievalExplanation>,
    /// Selection explanation.
    pub selection: Option<SelectionExplanation>,
    /// Per-agent outcome counts and capped selection bias for this memory.
    pub agent_profile: Option<AgentProfileSelectionExplanation>,
    /// Lifecycle explanation.
    pub lifecycle: Option<LifecycleExplanation>,
    /// Bayesian (alpha, beta) posterior over the memory's latent
    /// helpful-rate (N7.1 / ADR 0032). `None` when the memory does
    /// not exist OR when the schema migration has not been applied
    /// against an older workspace database.
    pub bayes_posterior: Option<BayesPosteriorSummary>,
    /// Optional causal explanation block requested by `ee why --causal-explain`.
    pub causal_explanation: Option<serde_json::Value>,
    /// Optional revision lineage block derived from the revision DAG.
    pub revision_lineage: Option<serde_json::Value>,
    /// Contradiction feedback recorded against this memory (EE-263).
    pub contradictions: Vec<ContradictionMetadata>,
    /// Memory links: supports, contradicts, derived_from, etc. (EE-LINK-USAGE-001).
    pub links: Vec<MemoryLinkSummary>,
    /// Audit history timeline for the memory.
    pub history: Option<MemoryHistorySummary>,
    /// Safe visible rationale traces linked to this memory or latest pack.
    pub rationale_traces: Vec<RationaleTraceSummary>,
    /// Verification evidence linked by the caller or future ledger lookup.
    pub verification_evidence: Vec<VerificationEvidenceRecord>,
    /// Non-fatal degradation notices.
    pub degraded: Vec<WhyDegradation>,
    /// Error message if query failed.
    pub error: Option<String>,
}

impl WhyReport {
    /// Create a report for a found memory.
    #[must_use]
    pub fn found(
        memory_id: String,
        storage: StorageExplanation,
        retrieval: RetrievalExplanation,
        selection: SelectionExplanation,
    ) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memory_id,
            found: true,
            content: None,
            storage: Some(storage),
            retrieval: Some(retrieval),
            graph_retrieval: None,
            selection: Some(selection),
            agent_profile: None,
            lifecycle: None,
            bayes_posterior: None,
            causal_explanation: None,
            revision_lineage: None,
            contradictions: Vec::new(),
            links: Vec::new(),
            history: None,
            rationale_traces: Vec::new(),
            verification_evidence: Vec::new(),
            degraded: Vec::new(),
            error: None,
        }
    }

    /// Attach the full memory body to the report. Returns `self` to allow
    /// builder-style chaining at the construction site.
    #[must_use]
    pub fn with_content(mut self, content: String) -> Self {
        self.content = Some(content);
        self
    }

    /// Create a report for a not-found memory.
    #[must_use]
    pub fn not_found(memory_id: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memory_id,
            found: false,
            content: None,
            storage: None,
            retrieval: None,
            graph_retrieval: None,
            selection: None,
            agent_profile: None,
            lifecycle: None,
            bayes_posterior: None,
            causal_explanation: None,
            revision_lineage: None,
            contradictions: Vec::new(),
            links: Vec::new(),
            history: None,
            rationale_traces: Vec::new(),
            verification_evidence: Vec::new(),
            degraded: Vec::new(),
            error: None,
        }
    }

    /// Create a report for an error condition.
    #[must_use]
    pub fn error(memory_id: String, message: String) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            memory_id,
            found: false,
            content: None,
            storage: None,
            retrieval: None,
            graph_retrieval: None,
            selection: None,
            agent_profile: None,
            lifecycle: None,
            bayes_posterior: None,
            causal_explanation: None,
            revision_lineage: None,
            contradictions: Vec::new(),
            links: Vec::new(),
            history: None,
            rationale_traces: Vec::new(),
            verification_evidence: Vec::new(),
            degraded: Vec::new(),
            error: Some(message),
        }
    }

    /// Create a successful report for a search result target that is not a memory.
    ///
    /// The CLI currently treats `found=false` reports as memory not-found errors, so
    /// unsupported non-memory result targets remain renderable and carry a stable
    /// degradation explaining why full memory provenance is unavailable.
    #[must_use]
    fn unsupported_result_target(
        document_id: String,
        source: WhyResultDocumentSource,
        conn: &DbConnection,
    ) -> Self {
        let storage = unsupported_result_target_storage(&document_id, source, conn);
        let retrieval = unsupported_result_target_retrieval(source);
        let selection = SelectionExplanation {
            selection_score: 0.0,
            above_confidence_threshold: false,
            is_active: false,
            score_breakdown:
                "non-memory search result targets are not eligible for memory pack selection"
                    .to_owned(),
            latest_pack_selection: None,
        };

        Self::found(document_id.clone(), storage, retrieval, selection).with_degradation(
            WhyDegradation {
                code: "why_result_target_unsupported_source",
                severity: "medium",
                message: format!(
                    "`result:{document_id}` targets a {} document, not a memory. `ee why` currently explains memory result targets and returns this source-level explanation instead of a memory not_found error.",
                    source.human_label()
                ),
                repair: Some(source.repair().to_owned()),
            },
        )
    }

    /// Add a non-fatal degradation notice to the report.
    #[must_use]
    pub fn with_degradation(mut self, degraded: WhyDegradation) -> Self {
        self.degraded.push(degraded);
        self
    }

    /// Add multiple non-fatal degradation notices to the report.
    #[must_use]
    pub fn with_degradations(mut self, degraded: Vec<WhyDegradation>) -> Self {
        self.degraded.extend(degraded);
        self
    }

    /// Add memory lifecycle metadata to the report.
    #[must_use]
    pub fn with_lifecycle(mut self, lifecycle: LifecycleExplanation) -> Self {
        self.lifecycle = Some(lifecycle);
        self
    }

    /// Add contradiction metadata to the report (EE-263).
    #[must_use]
    pub fn with_contradictions(mut self, contradictions: Vec<ContradictionMetadata>) -> Self {
        self.contradictions = contradictions;
        self
    }

    /// Add memory link summaries to the report (EE-LINK-USAGE-001).
    #[must_use]
    pub fn with_links(mut self, links: Vec<MemoryLinkSummary>) -> Self {
        self.links = links;
        self
    }

    /// Add memory audit history to the report.
    #[must_use]
    pub fn with_history(mut self, history: MemoryHistorySummary) -> Self {
        self.history = Some(history);
        self
    }

    /// Add optional memory audit history to the report.
    #[must_use]
    pub fn with_optional_history(mut self, history: Option<MemoryHistorySummary>) -> Self {
        self.history = history;
        self
    }

    /// Attach a Bayesian (alpha, beta) posterior summary to the
    /// report (N7.1 / ADR 0032). `None` is the no-op variant (memory
    /// not found, schema migration not applied, or pre-V041 row).
    #[must_use]
    pub fn with_bayes_posterior(mut self, summary: Option<BayesPosteriorSummary>) -> Self {
        self.bayes_posterior = summary;
        self
    }

    /// Attach an `ee.why.causal.v1` causal explanation block to the report.
    #[must_use]
    pub fn with_causal_explanation(mut self, causal_explanation: serde_json::Value) -> Self {
        self.causal_explanation = Some(causal_explanation);
        self
    }

    /// Attach a graph-derived revision lineage block.
    #[must_use]
    pub fn with_revision_lineage(mut self, revision_lineage: serde_json::Value) -> Self {
        self.revision_lineage = Some(revision_lineage);
        self
    }

    /// Add graph-derived retrieval feature explanation to the report.
    #[must_use]
    pub fn with_graph_retrieval(mut self, graph_retrieval: GraphRetrievalExplanation) -> Self {
        self.graph_retrieval = Some(graph_retrieval);
        self
    }

    /// Add per-agent profile counts and bias explanation to the report.
    #[must_use]
    pub fn with_agent_profile(
        mut self,
        agent_profile: Option<AgentProfileSelectionExplanation>,
    ) -> Self {
        self.agent_profile = agent_profile;
        self
    }

    /// Add safe visible rationale traces to the report.
    #[must_use]
    pub fn with_rationale_traces(
        mut self,
        mut rationale_traces: Vec<RationaleTraceSummary>,
    ) -> Self {
        rationale_traces.retain(RationaleTraceSummary::is_visible_for_report);
        rationale_traces.sort_by(|left, right| left.trace_id.cmp(&right.trace_id));
        rationale_traces.dedup_by(|left, right| left.trace_id == right.trace_id);
        self.rationale_traces = rationale_traces;
        self
    }

    /// Add verification evidence already linked by the caller.
    #[must_use]
    pub fn with_verification_evidence(
        mut self,
        mut verification_evidence: Vec<VerificationEvidenceRecord>,
    ) -> Self {
        verification_evidence
            .sort_by(|left, right| left.verification_id.cmp(&right.verification_id));
        verification_evidence.dedup_by(|left, right| left.verification_id == right.verification_id);
        self.verification_evidence = verification_evidence;
        self
    }
}

/// Options for the why query.
#[derive(Clone, Debug)]
pub struct WhyOptions<'a> {
    /// Database path.
    pub database_path: &'a Path,
    /// Memory ID or `result:<doc-id>` search result target to explain.
    pub memory_id: &'a str,
    /// Confidence threshold for selection (default 0.5).
    pub confidence_threshold: f32,
}

impl<'a> WhyOptions<'a> {
    /// Default confidence threshold for pack selection.
    pub const DEFAULT_CONFIDENCE_THRESHOLD: f32 = 0.5;
}

fn why_trace_elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn trace_why_math_checkpoint(
    workspace_id: &str,
    memory_id: &str,
    bead_id: &'static str,
    surface: &'static str,
    phase: &'static str,
    started: Instant,
    degraded_codes: &[&str],
) {
    tracing::info!(
        workspace_id = %workspace_id,
        request_id = %memory_id,
        bead_id,
        surface,
        phase,
        elapsed_ms = why_trace_elapsed_ms(started),
        degraded_codes = ?degraded_codes,
        "why math surface checkpoint"
    );
}

fn trace_why_math_surfaces(
    workspace_id: &str,
    memory_id: &str,
    phase: &'static str,
    started: Instant,
    degraded_codes: &[&str],
) {
    trace_why_math_checkpoint(
        workspace_id,
        memory_id,
        "bd-3usjw.44",
        "conformal_prediction_sets",
        phase,
        started,
        degraded_codes,
    );
    trace_why_math_checkpoint(
        workspace_id,
        memory_id,
        "bd-3usjw.48",
        "influence_function_why",
        phase,
        started,
        degraded_codes,
    );
}

/// Get a why explanation for a memory.
///
/// Explains why a memory was stored, how it would be retrieved,
/// and how it would be selected for context packs.
pub fn explain_memory(options: &WhyOptions<'_>) -> WhyReport {
    let mut audit_ids = WhyAuditIdSource::Ambient;
    explain_memory_inner(options, &mut audit_ids)
}

pub fn explain_memory_seeded(
    options: &WhyOptions<'_>,
    determinism: &mut Deterministic<Seed>,
) -> WhyReport {
    let mut audit_ids = WhyAuditIdSource::Seeded(determinism);
    explain_memory_inner(options, &mut audit_ids)
}

enum WhyAuditIdSource<'a> {
    Ambient,
    Seeded(&'a mut Deterministic<Seed>),
}

impl WhyAuditIdSource<'_> {
    fn next_audit_id(&mut self) -> String {
        match self {
            Self::Ambient => generate_audit_id(),
            Self::Seeded(determinism) => generate_audit_id_seeded(determinism),
        }
    }
}

fn explain_memory_inner(
    options: &WhyOptions<'_>,
    audit_ids: &mut WhyAuditIdSource<'_>,
) -> WhyReport {
    let started = Instant::now();
    let target = resolve_why_target(options.memory_id);
    let memory_id = target.document_id;

    let conn = match DbConnection::open_file(options.database_path) {
        Ok(c) => c,
        Err(e) => {
            return WhyReport::error(
                memory_id.to_string(),
                format!("Failed to open database: {e}"),
            );
        }
    };
    if let Err(error) = conn.migrate() {
        return WhyReport::error(
            memory_id.to_string(),
            format!("Failed to migrate database before why query: {error}"),
        );
    }

    if let Some(source) = target.unsupported_result_source() {
        return WhyReport::unsupported_result_target(memory_id.to_string(), source, &conn);
    }

    let memory = match conn.get_memory(memory_id) {
        Ok(Some(m)) => m,
        Ok(None) => return WhyReport::not_found(memory_id.to_string()),
        Err(e) => {
            return WhyReport::error(
                memory_id.to_string(),
                format!("Failed to query memory: {e}"),
            );
        }
    };

    trace_why_math_surfaces(&memory.workspace_id, memory_id, "input", started, &[]);

    let tags = match conn.get_memory_tags(memory_id) {
        Ok(t) => t,
        Err(e) => {
            return WhyReport::error(memory_id.to_string(), format!("Failed to query tags: {e}"));
        }
    };

    // Fetch contradiction feedback events (EE-263)
    let contradiction_fetch = fetch_contradictions(&conn, memory_id);

    // Fetch memory links (EE-LINK-USAGE-001)
    let link_fetch = fetch_links(&conn, memory_id);

    // Fetch memory audit history for the triad `ee why` surface.
    let history_fetch = fetch_history(&conn, memory_id);

    // Fetch rationale traces (EE-RATIONALE-TRACE-001)
    let rationale_trace_fetch = fetch_rationale_traces(&conn, &memory.workspace_id, memory_id);
    let verification_fetch = fetch_verification_evidence(&conn, "memory", memory_id);
    let mut evidence_degradations = Vec::new();
    if let Some(degradation) = contradiction_fetch.degradation {
        evidence_degradations.push(degradation);
    }
    if let Some(degradation) = link_fetch.degradation {
        evidence_degradations.push(degradation);
    }
    if let Some(degradation) = history_fetch.degradation {
        evidence_degradations.push(degradation);
    }
    if let Some(degradation) = rationale_trace_fetch.degradation {
        evidence_degradations.push(degradation);
    }
    if let Some(degradation) = verification_fetch.degradation {
        evidence_degradations.push(degradation);
    }
    if verification_fetch.items.is_empty()
        && tags.iter().any(|tag| {
            tag == "verification" || tag == "verification-required" || tag == "bead-closure"
        })
    {
        evidence_degradations.push(WhyDegradation {
            code: "verification_evidence_not_found",
            severity: "low",
            message: "No verification evidence ledger row is linked to this memory; verification-sensitive claims are reported as unverified rather than silently absent."
                .to_owned(),
            repair: Some(
                "ee verification ingest --file <verification-evidence.json> --target-type memory --target-id <memory-id>"
                    .to_owned(),
            ),
        });
    }
    let workspace_path = workspace_path_for_memory(&conn, &memory.workspace_id);
    let freshness = assess_memory_evidence_freshness(&memory, workspace_path.as_deref());
    if let Some(degradation) = why_evidence_freshness_degradation(memory_id, &freshness) {
        evidence_degradations.push(degradation);
    }
    let contradictions = contradiction_fetch.items;
    let links = link_fetch.items;
    let history = history_fetch.items.into_iter().next();
    let lifecycle = lifecycle_for_memory(&memory, history.as_ref());
    let rationale_traces = rationale_trace_fetch.items;
    let verification_evidence = verification_fetch.items;

    let validity = memory_validity(&memory.valid_from, &memory.valid_to);
    let graph_retrieval = build_graph_retrieval_explanation(
        &conn,
        &memory.workspace_id,
        memory_id,
        &links,
        &contradictions,
        &validity.status,
    );
    let storage = StorageExplanation {
        origin: determine_origin(&memory.trust_class),
        trust_class: memory.trust_class.clone(),
        trust_subclass: memory.trust_subclass.clone(),
        provenance_uri: memory
            .provenance_uri
            .clone()
            .map(redact_why_search_result_provenance_uri),
        workflow_id: memory.workflow_id.clone(),
        created_at: memory.created_at.clone(),
        valid_from: validity.valid_from,
        valid_to: validity.valid_to,
        validity_status: validity.status,
        validity_window_kind: validity.window_kind,
    };

    let retrieval = RetrievalExplanation {
        confidence: memory.confidence,
        utility: memory.utility,
        importance: memory.importance,
        tags,
        level: memory.level.clone(),
        kind: memory.kind.clone(),
    };

    let is_active = memory.tombstoned_at.is_none();
    let selection_score =
        compute_selection_score(memory.confidence, memory.utility, memory.importance);
    let above_threshold = memory.confidence >= options.confidence_threshold;
    let agent_profile =
        fetch_agent_profile_selection_explanation(&conn, &memory.workspace_id, memory_id);

    let latest_pack_selection = match latest_pack_selection(&conn, memory_id) {
        Ok(selection) => selection,
        Err(message) => {
            let report = build_report(
                memory_id,
                storage,
                retrieval,
                ReportSelectionInputs {
                    is_active,
                    selection_score,
                    above_threshold,
                    latest_pack_selection: None,
                    lifecycle,
                    contradictions,
                    links,
                    history,
                    rationale_traces,
                    verification_evidence: Vec::new(),
                    graph_retrieval,
                    degraded: evidence_degradations,
                    agent_profile,
                },
            )
            .with_content(memory.content.clone());
            trace_why_math_surfaces(
                &memory.workspace_id,
                memory_id,
                "response",
                started,
                &["why_pack_selection_unavailable"],
            );
            return report.with_degradation(WhyDegradation {
                code: "why_pack_selection_unavailable",
                severity: "low",
                message,
                repair: Some("ee db migrate".to_string()),
            });
        }
    };

    let report = build_report(
        memory_id,
        storage,
        retrieval,
        ReportSelectionInputs {
            is_active,
            selection_score,
            above_threshold,
            latest_pack_selection,
            lifecycle,
            contradictions,
            links,
            history,
            rationale_traces,
            verification_evidence,
            graph_retrieval,
            degraded: evidence_degradations,
            agent_profile,
        },
    )
    .with_content(memory.content.clone());

    // N7.1 (bd-17c65.14.7.2 / ADR 0032): attach the Bayesian
    // (alpha, beta) posterior summary so an agent reading `ee why`
    // can see the credible interval and effective sample size rather
    // than just the legacy scalar `confidence` field. Failures here
    // are best-effort: a missing posterior (e.g. pre-V041 schema, or
    // a transient query error) leaves the field None rather than
    // failing the entire `ee why` response. The runtime
    // `update_memory_bayes_posterior` path is the source of truth;
    // `ee why` is a read-only consumer.
    let report = match conn.get_memory_bayes_posterior(memory_id) {
        Ok(Some((alpha, beta))) => {
            let summary = crate::core::bayes::BetaPosterior::new(alpha, beta)
                .map(|posterior| BayesPosteriorSummary::from_posterior(&posterior));
            report.with_bayes_posterior(summary)
        }
        Ok(None) | Err(_) => report,
    };
    let report = if revision_dominance_feature_enabled(options.database_path) {
        match revision_lineage_for_why(&conn, &memory.workspace_id, memory_id) {
            Some(revision_lineage) => report.with_revision_lineage(revision_lineage),
            None => report,
        }
    } else {
        report.with_revision_lineage(revision_lineage_feature_disabled(memory_id))
    };

    trace_why_math_surfaces(&memory.workspace_id, memory_id, "response", started, &[]);

    // Bead bd-17c65.7.7 (G8): best-effort audit row so L3 has a
    // last_accessed signal for `ee why` reads, and G1 can count
    // why-inspection activity per workspace.
    let details = serde_json::json!({"surface": "why"}).to_string();
    let audit_input = crate::db::CreateAuditInput {
        workspace_id: Some(memory.workspace_id.clone()),
        actor: None,
        action: crate::db::audit_actions::WHY_INSPECTED.to_owned(),
        target_type: Some("memory".to_owned()),
        target_id: Some(memory_id.to_owned()),
        details: Some(details),
    };
    let _ = conn.insert_audit(&audit_ids.next_audit_id(), &audit_input);

    report
}

fn revision_dominance_feature_enabled(database_path: &Path) -> bool {
    let Some(workspace_root) = workspace_root_from_database_path(database_path) else {
        return false;
    };
    let options = crate::core::config_surface::ConfigSurfaceOptions {
        workspace_root,
        config_path: None,
    };
    crate::core::config_surface::get_config(&options, GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY)
        .map(|report| report.value == "true")
        .unwrap_or(false)
}

fn workspace_root_from_database_path(database_path: &Path) -> Option<PathBuf> {
    if database_path.file_name()?.to_str()? != "ee.db" {
        return None;
    }
    let ee_dir = database_path.parent()?;
    if ee_dir.file_name()?.to_str()? != ".ee" {
        return None;
    }
    ee_dir.parent().map(Path::to_path_buf)
}

fn revision_lineage_feature_disabled(memory_id: &str) -> serde_json::Value {
    let degraded = aggregate_why_revision_lineage_degraded([DegradationAggregationInput::new(
        "why_revision_lineage",
        "graph_feature_disabled",
        "medium",
        format!(
            "Revision dominance is disabled by {GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY}."
        ),
        format!("ee config set {GRAPH_FEATURE_REVISION_DOMINANCE_ENABLED_KEY} true"),
    )]);

    serde_json::json!({
        "sourceSchema": crate::graph::dominance::MEMORY_IMPACT_ANALYSIS_SCHEMA_V1,
        "memoryId": memory_id,
        "snapshotVersion": 0,
        "rootMemoryId": serde_json::Value::Null,
        "immediateDominator": serde_json::Value::Null,
        "dominanceFrontier": [],
        "ancestorsAtDepth": {},
        "validationStatus": "disabled",
        "degraded": degraded,
    })
}

fn aggregate_why_revision_lineage_degraded<I>(entries: I) -> Vec<serde_json::Value>
where
    I: IntoIterator<Item = DegradationAggregationInput>,
{
    aggregate_degraded_entries(entries)
        .into_iter()
        .map(|entry| {
            serde_json::json!({
                "code": entry.code,
                "severity": entry.severity,
                "message": entry.message,
                "repair": entry.repair,
                "sources": entry.sources,
            })
        })
        .collect()
}

fn aggregate_why_dominance_degraded(
    degraded: &[crate::graph::dominance::DominanceDegradation],
) -> Vec<serde_json::Value> {
    aggregate_why_revision_lineage_degraded(degraded.iter().map(|entry| {
        DegradationAggregationInput::new(
            "graph_dominance",
            entry.code.clone(),
            entry.severity.clone(),
            entry.message.clone(),
            entry
                .repair
                .clone()
                .unwrap_or_else(|| "Refresh graph dominance diagnostics.".to_owned()),
        )
    }))
}

fn revision_lineage_for_why(
    conn: &DbConnection,
    workspace_id: &str,
    memory_id: &str,
) -> Option<serde_json::Value> {
    let graph = crate::graph::build_revision_dag_from_logical_ids(conn, workspace_id).ok()?;
    let snapshot_version = conn
        .get_latest_graph_snapshot(workspace_id, crate::db::GraphSnapshotType::RevisionDag)
        .ok()
        .flatten()
        .map_or(0, |snapshot| u64::from(snapshot.snapshot_version));
    let impact = crate::graph::dominance::compute_memory_impact_analysis(
        &graph,
        memory_id,
        snapshot_version,
    )
    .ok()?;
    let ancestors_at_depth = revision_ancestors_at_depth(&graph, memory_id);
    let has_revision_context = ancestors_at_depth
        .values()
        .map(BTreeSet::len)
        .sum::<usize>()
        > 1
        || !graph.successors(memory_id).unwrap_or_default().is_empty();
    let root_memory_id = ancestors_at_depth
        .iter()
        .next_back()
        .and_then(|(_, ids)| ids.iter().next().cloned())
        .unwrap_or_else(|| memory_id.to_owned());
    let (immediate_dominator, dominance_frontier) = if has_revision_context {
        let idoms = crate::graph::dominance::compute_immediate_dominators(&graph, &root_memory_id)
            .ok()
            .unwrap_or_default();
        let frontiers =
            crate::graph::dominance::compute_dominance_frontiers(&graph, &root_memory_id)
                .ok()
                .unwrap_or_default();
        (
            idoms
                .get(memory_id)
                .filter(|dominator| dominator.as_str() != memory_id)
                .cloned(),
            frontiers.get(memory_id).cloned().unwrap_or_default(),
        )
    } else {
        (
            impact.impact_analysis.immediate_dominator.clone(),
            impact.impact_analysis.dominance_frontier.clone(),
        )
    };
    let mut ancestors = serde_json::Map::new();
    for (depth, ids) in ancestors_at_depth {
        ancestors.insert(
            depth.to_string(),
            serde_json::Value::Array(ids.into_iter().map(serde_json::Value::String).collect()),
        );
    }
    let degraded = aggregate_why_dominance_degraded(&impact.degraded);

    Some(serde_json::json!({
        "sourceSchema": impact.schema,
        "memoryId": memory_id,
        "snapshotVersion": snapshot_version,
        "rootMemoryId": root_memory_id,
        "immediateDominator": immediate_dominator,
        "dominanceFrontier": dominance_frontier,
        "ancestorsAtDepth": ancestors,
        "validationStatus": impact.impact_analysis.validation_status,
        "degraded": degraded,
    }))
}

fn revision_ancestors_at_depth(
    graph: &crate::graph::DiGraph,
    memory_id: &str,
) -> BTreeMap<usize, BTreeSet<String>> {
    let mut by_depth: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut frontier = BTreeSet::from([memory_id.to_owned()]);
    let mut depth = 0usize;

    while !frontier.is_empty() {
        let mut next = BTreeSet::new();
        for id in frontier {
            if !seen.insert(id.clone()) {
                continue;
            }
            by_depth.entry(depth).or_default().insert(id.clone());
            for predecessor in graph.predecessors(&id).unwrap_or_default() {
                if !seen.contains(predecessor) {
                    next.insert(predecessor.to_owned());
                }
            }
        }
        frontier = next;
        depth = depth.saturating_add(1);
    }

    by_depth
}

fn workspace_path_for_memory(conn: &DbConnection, workspace_id: &str) -> Option<PathBuf> {
    conn.get_workspace(workspace_id)
        .ok()
        .flatten()
        .map(|workspace| PathBuf::from(workspace.path))
}

fn why_evidence_freshness_degradation(
    memory_id: &str,
    freshness: &EvidenceFreshness,
) -> Option<WhyDegradation> {
    let code = match freshness.status {
        EvidenceFreshnessStatus::MissingSource => "why_evidence_freshness_missing_source",
        EvidenceFreshnessStatus::ChangedSource => "why_evidence_freshness_changed_source",
        EvidenceFreshnessStatus::UnreachableSource => "why_evidence_freshness_unreachable_source",
        EvidenceFreshnessStatus::UnsupportedSource => "why_evidence_freshness_unsupported_source",
        EvidenceFreshnessStatus::Fresh | EvidenceFreshnessStatus::Unknown => return None,
    };
    Some(WhyDegradation {
        code,
        severity: "low",
        message: format!(
            "Memory {memory_id} evidence freshness is {}: {}",
            freshness.status.as_str(),
            freshness.detail
        ),
        repair: freshness.repair.clone(),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WhyResultDocumentSource {
    Memory,
    Session,
    Artifact,
    CurationCandidate,
    Unknown,
}

impl WhyResultDocumentSource {
    fn from_document_id(document_id: &str) -> Self {
        if document_id.starts_with("mem_") {
            Self::Memory
        } else if document_id.starts_with("sess_") {
            Self::Session
        } else if document_id.starts_with("art_") {
            Self::Artifact
        } else if document_id.starts_with("curate_") {
            Self::CurationCandidate
        } else {
            Self::Unknown
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Session => "session",
            Self::Artifact => "artifact",
            Self::CurationCandidate => "curation_candidate",
            Self::Unknown => "unknown",
        }
    }

    const fn human_label(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Session => "CASS session",
            Self::Artifact => "artifact",
            Self::CurationCandidate => "curation candidate",
            Self::Unknown => "non-memory search",
        }
    }

    const fn repair(self) -> &'static str {
        match self {
            Self::Memory => "ee why <memory-id> --json",
            Self::Session => "ee import sessions --json",
            Self::Artifact => "ee artifact show <artifact-id> --json",
            Self::CurationCandidate => "ee curate show <candidate-id> --json",
            Self::Unknown => "ee search <query> --json",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WhyTarget<'a> {
    document_id: &'a str,
    result_source: Option<WhyResultDocumentSource>,
}

impl WhyTarget<'_> {
    const fn unsupported_result_source(self) -> Option<WhyResultDocumentSource> {
        match self.result_source {
            Some(WhyResultDocumentSource::Memory) | None => None,
            Some(source) => Some(source),
        }
    }
}

fn resolve_why_target(target_id: &str) -> WhyTarget<'_> {
    target_id
        .strip_prefix("result:")
        .filter(|doc_id| !doc_id.trim().is_empty())
        .map_or(
            WhyTarget {
                document_id: target_id,
                result_source: None,
            },
            |document_id| WhyTarget {
                document_id,
                result_source: Some(WhyResultDocumentSource::from_document_id(document_id)),
            },
        )
}

#[cfg(test)]
fn resolve_why_memory_id(target_id: &str) -> &str {
    resolve_why_target(target_id).document_id
}

fn unsupported_result_target_storage(
    document_id: &str,
    source: WhyResultDocumentSource,
    conn: &DbConnection,
) -> StorageExplanation {
    match source {
        WhyResultDocumentSource::Session => {
            conn.get_session(document_id).ok().flatten().map_or_else(
                || generic_unsupported_storage(document_id, source),
                |session| StorageExplanation {
                    origin: "Imported CASS session search document".to_owned(),
                    trust_class: "cass_evidence".to_owned(),
                    trust_subclass: Some("search_result_session".to_owned()),
                    provenance_uri: session
                        .source_path
                        .clone()
                        .or_else(|| Some(format!("cass://session/{}", session.cass_session_id)))
                        .map(redact_why_search_result_provenance_uri),
                    workflow_id: None,
                    created_at: session.imported_at,
                    valid_from: session.started_at,
                    valid_to: session.ended_at,
                    validity_status: "not_applicable".to_owned(),
                    validity_window_kind: "search_document".to_owned(),
                },
            )
        }
        WhyResultDocumentSource::Artifact => {
            conn.get_artifact(document_id).ok().flatten().map_or_else(
                || generic_unsupported_storage(document_id, source),
                |artifact| StorageExplanation {
                    origin: "Registered artifact search document".to_owned(),
                    trust_class: "artifact_metadata".to_owned(),
                    trust_subclass: Some(artifact.artifact_type),
                    provenance_uri: artifact
                        .provenance_uri
                        .or(artifact.original_path)
                        .or(artifact.external_ref)
                        .map(redact_why_search_result_provenance_uri),
                    workflow_id: None,
                    created_at: artifact.created_at,
                    valid_from: None,
                    valid_to: None,
                    validity_status: "not_applicable".to_owned(),
                    validity_window_kind: "search_document".to_owned(),
                },
            )
        }
        WhyResultDocumentSource::CurationCandidate | WhyResultDocumentSource::Unknown => {
            generic_unsupported_storage(document_id, source)
        }
        WhyResultDocumentSource::Memory => generic_unsupported_storage(document_id, source),
    }
}

fn redact_why_search_result_provenance_uri(value: String) -> String {
    let secret_redacted = crate::policy::redact_secret_like_content(&value).content;
    redact_why_absolute_path_like_segments(&secret_redacted)
}

fn redact_why_absolute_path_like_segments(input: &str) -> String {
    const REDACTED_PATH: &str = "[REDACTED_PATH]";
    const PATH_PREFIXES: &[&str] = &[
        "/home/",
        "/Users/",
        "/Volumes/",
        "/private/",
        "/var/",
        "/tmp/",
        "/data/",
        "/dp/",
        "/workspace/",
        "/repo/",
        "/etc/",
        "C:\\",
        "D:\\",
    ];

    let mut output = String::with_capacity(input.len());
    let mut cursor = 0usize;
    while cursor < input.len() {
        let remaining = &input[cursor..];
        if let Some(prefix) = PATH_PREFIXES
            .iter()
            .find(|prefix| remaining.starts_with(**prefix))
        {
            output.push_str(REDACTED_PATH);
            cursor += prefix.len();
            while cursor < input.len() {
                let next = input[cursor..]
                    .chars()
                    .next()
                    .unwrap_or('\0');
                if next.is_whitespace()
                    || matches!(
                        next,
                        '"' | '\'' | '`' | '<' | '>' | ')' | ']' | '}' | ',' | ';' | '|'
                    )
                {
                    break;
                }
                cursor += next.len_utf8();
            }
            continue;
        }

        let next = remaining
            .chars()
            .next()
            .unwrap_or('\0');
        output.push(next);
        cursor += next.len_utf8();
    }

    output
}

fn generic_unsupported_storage(
    document_id: &str,
    source: WhyResultDocumentSource,
) -> StorageExplanation {
    StorageExplanation {
        origin: format!(
            "{} search result target `{document_id}` is not stored as a memory",
            source.human_label()
        ),
        trust_class: "search_document".to_owned(),
        trust_subclass: Some(format!("unsupported_{}", source.as_str())),
        provenance_uri: Some(format!("ee://search-result/{document_id}")),
        workflow_id: None,
        created_at: "unknown".to_owned(),
        valid_from: None,
        valid_to: None,
        validity_status: "not_applicable".to_owned(),
        validity_window_kind: "search_document".to_owned(),
    }
}

fn unsupported_result_target_retrieval(source: WhyResultDocumentSource) -> RetrievalExplanation {
    RetrievalExplanation {
        confidence: 0.0,
        utility: 0.0,
        importance: 0.0,
        tags: vec![
            "result_target".to_owned(),
            format!("source:{}", source.as_str()),
            "unsupported_for_why".to_owned(),
        ],
        level: "search_document".to_owned(),
        kind: source.as_str().to_owned(),
    }
}

struct ReportSelectionInputs {
    is_active: bool,
    selection_score: f32,
    above_threshold: bool,
    latest_pack_selection: Option<PackSelectionExplanation>,
    lifecycle: LifecycleExplanation,
    contradictions: Vec<ContradictionMetadata>,
    links: Vec<MemoryLinkSummary>,
    history: Option<MemoryHistorySummary>,
    rationale_traces: Vec<RationaleTraceSummary>,
    verification_evidence: Vec<VerificationEvidenceRecord>,
    graph_retrieval: GraphRetrievalExplanation,
    degraded: Vec<WhyDegradation>,
    agent_profile: Option<AgentProfileSelectionExplanation>,
}

fn build_report(
    memory_id: &str,
    storage: StorageExplanation,
    retrieval: RetrievalExplanation,
    selection_inputs: ReportSelectionInputs,
) -> WhyReport {
    let selection = SelectionExplanation {
        selection_score: selection_inputs.selection_score,
        above_confidence_threshold: selection_inputs.above_threshold,
        is_active: selection_inputs.is_active,
        score_breakdown: format!(
            "selection_score = 0.5 * confidence({:.2}) + 0.3 * utility({:.2}) + 0.2 * importance({:.2}) = {:.2}",
            retrieval.confidence,
            retrieval.utility,
            retrieval.importance,
            selection_inputs.selection_score
        ),
        latest_pack_selection: selection_inputs.latest_pack_selection,
    };

    WhyReport::found(memory_id.to_string(), storage, retrieval, selection)
        .with_agent_profile(selection_inputs.agent_profile)
        .with_lifecycle(selection_inputs.lifecycle)
        .with_contradictions(selection_inputs.contradictions)
        .with_links(selection_inputs.links)
        .with_optional_history(selection_inputs.history)
        .with_graph_retrieval(selection_inputs.graph_retrieval)
        .with_rationale_traces(selection_inputs.rationale_traces)
        .with_verification_evidence(selection_inputs.verification_evidence)
        .with_degradations(selection_inputs.degraded)
}

fn lifecycle_for_memory(
    memory: &crate::db::StoredMemory,
    history: Option<&MemoryHistorySummary>,
) -> LifecycleExplanation {
    let tombstoned_at = memory.tombstoned_at.clone();
    let tombstoned_reason = tombstoned_at
        .as_ref()
        .and_then(|_| tombstone_reason_from_history(history));
    LifecycleExplanation {
        status: if tombstoned_at.is_some() {
            "tombstoned"
        } else {
            "active"
        },
        tombstoned_at,
        tombstoned_reason,
    }
}

fn tombstone_reason_from_history(history: Option<&MemoryHistorySummary>) -> Option<String> {
    history?
        .entries
        .iter()
        .find(|entry| entry.action == crate::db::audit_actions::MEMORY_TOMBSTONE)
        .and_then(|entry| entry.details.as_deref())
        .and_then(|details| {
            serde_json::from_str::<serde_json::Value>(details)
                .ok()
                .and_then(|value| {
                    value
                        .get("reason")
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|reason| !reason.is_empty())
                        .map(str::to_owned)
                })
        })
}

fn build_graph_retrieval_explanation(
    conn: &DbConnection,
    workspace_id: &str,
    memory_id: &str,
    links: &[MemoryLinkSummary],
    contradictions: &[ContradictionMetadata],
    validity_status: &str,
) -> GraphRetrievalExplanation {
    let options = crate::graph::GraphFeatureEnrichmentOptions {
        max_features: usize::MAX,
        min_combined_score: 0.0,
        ..crate::graph::GraphFeatureEnrichmentOptions::default()
    };

    let report = match conn
        .get_latest_graph_snapshot(workspace_id, crate::db::GraphSnapshotType::MemoryLinks)
    {
        Ok(snapshot) => crate::graph::enrich_graph_features_from_graph_snapshot(
            snapshot.as_ref(),
            workspace_id,
            crate::db::GraphSnapshotType::MemoryLinks,
            &options,
        ),
        Err(error) => {
            return graph_retrieval_unavailable(
                workspace_id,
                "graph_snapshot_query_failed",
                "medium",
                format!("Failed to query graph snapshot: {error}"),
                "ee graph centrality-refresh",
                links,
                contradictions,
            );
        }
    };

    let feature = report
        .features
        .iter()
        .find(|feature| feature.memory_id == memory_id);
    let pagerank = feature.map_or_else(default_graph_metric, |feature| GraphMetricExplanation {
        raw: round_graph_score(feature.pagerank),
        normalized: round_graph_score(feature.pagerank_normalized),
        rank: feature.pagerank_rank,
        weight: 0.6,
        contribution: round_graph_score(feature.pagerank_normalized * 0.6),
        formula: "pagerank_contribution = pagerank.normalized * 0.6".to_owned(),
    });
    let betweenness = feature.map_or_else(default_graph_metric, |feature| GraphMetricExplanation {
        raw: round_graph_score(feature.betweenness),
        normalized: round_graph_score(feature.betweenness_normalized),
        rank: feature.betweenness_rank,
        weight: 0.4,
        contribution: round_graph_score(feature.betweenness_normalized * 0.4),
        formula: "betweenness_contribution = betweenness.normalized * 0.4".to_owned(),
    });
    let mut degraded = graph_degradations_from_report(&report);
    let status = if feature.is_some() {
        "available".to_owned()
    } else if report.status == crate::graph::GraphFeatureEnrichmentStatus::Enriched {
        degraded.push(WhyDegradation {
            code: "graph_memory_not_in_snapshot",
            severity: "low",
            message: "Graph snapshot exists, but this memory has no graph score.".to_owned(),
            repair: Some("ee graph centrality-refresh".to_owned()),
        });
        "memory_not_in_graph_snapshot".to_owned()
    } else {
        report.status.as_str().to_owned()
    };
    degraded.push(WhyDegradation {
        code: "graph_query_relative_features_unavailable",
        severity: "low",
        message: "Community and query-seed graph features are present as stable fields but unavailable without community detection and query-seed expansion."
            .to_owned(),
        repair: Some("ee graph communities && ee search --explain".to_owned()),
    });

    let evidence_support_count = evidence_support_count(links);
    let contradiction_count = contradiction_count(links, contradictions);
    let orphan_penalty = orphan_penalty(links);
    let stale_bridge_penalty = stale_bridge_penalty(validity_status, betweenness.normalized);

    GraphRetrievalExplanation {
        status,
        source: graph_retrieval_source_from_report(&report),
        centrality_score: round_graph_score(feature.map_or(0.0, |feature| feature.combined_score)),
        authority_score: pagerank.normalized,
        hub_score: pagerank.normalized,
        community_id: None,
        distance_to_query_seed: None,
        same_cluster_as_top_result: None,
        evidence_support_count,
        contradiction_count,
        orphan_penalty,
        stale_bridge_penalty,
        pagerank,
        betweenness,
        labels: feature.map_or_else(Vec::new, |feature| feature.labels.clone()),
        reasons: feature.map_or_else(Vec::new, |feature| feature.reasons.clone()),
        centrality_formula: "centrality_score = 0.6 * pagerank.normalized + 0.4 * betweenness.normalized"
            .to_owned(),
        orphan_penalty_formula: "orphan_penalty = 1.0 when no incident memory_links exist, else 0.0"
            .to_owned(),
        stale_bridge_penalty_formula:
            "stale_bridge_penalty = betweenness.normalized when validity_status is expired, else 0.0"
                .to_owned(),
        degraded,
    }
}

fn graph_retrieval_unavailable(
    workspace_id: &str,
    code: &'static str,
    severity: &'static str,
    message: String,
    repair: &'static str,
    links: &[MemoryLinkSummary],
    contradictions: &[ContradictionMetadata],
) -> GraphRetrievalExplanation {
    let pagerank = default_graph_metric();
    let betweenness = default_graph_metric();
    GraphRetrievalExplanation {
        status: code.to_owned(),
        source: GraphRetrievalSourceExplanation {
            kind: "graph_snapshot".to_owned(),
            workspace_id: Some(workspace_id.to_owned()),
            graph_type: Some(crate::db::GraphSnapshotType::MemoryLinks.as_str().to_owned()),
            snapshot: None,
        },
        centrality_score: 0.0,
        authority_score: 0.0,
        hub_score: 0.0,
        community_id: None,
        distance_to_query_seed: None,
        same_cluster_as_top_result: None,
        evidence_support_count: evidence_support_count(links),
        contradiction_count: contradiction_count(links, contradictions),
        orphan_penalty: orphan_penalty(links),
        stale_bridge_penalty: 0.0,
        pagerank,
        betweenness,
        labels: Vec::new(),
        reasons: Vec::new(),
        centrality_formula: "centrality_score = 0.6 * pagerank.normalized + 0.4 * betweenness.normalized"
            .to_owned(),
        orphan_penalty_formula: "orphan_penalty = 1.0 when no incident memory_links exist, else 0.0"
            .to_owned(),
        stale_bridge_penalty_formula:
            "stale_bridge_penalty = betweenness.normalized when validity_status is expired, else 0.0"
                .to_owned(),
        degraded: vec![WhyDegradation {
            code,
            severity,
            message,
            repair: Some(repair.to_owned()),
        }],
    }
}

fn graph_degradations_from_report(
    report: &crate::graph::GraphFeatureEnrichmentReport,
) -> Vec<WhyDegradation> {
    report
        .degraded
        .iter()
        .map(|entry| WhyDegradation {
            code: entry.code,
            severity: entry.severity,
            message: entry.message.clone(),
            repair: Some(entry.repair.clone()),
        })
        .collect()
}

fn graph_retrieval_source_from_report(
    report: &crate::graph::GraphFeatureEnrichmentReport,
) -> GraphRetrievalSourceExplanation {
    GraphRetrievalSourceExplanation {
        kind: report.source.kind.to_owned(),
        workspace_id: report.source.workspace_id.clone(),
        graph_type: report.source.graph_type.clone(),
        snapshot: report.source.snapshot.as_ref().map(|snapshot| {
            GraphRetrievalSnapshotExplanation {
                id: snapshot.id.clone(),
                schema_version: snapshot.schema_version.clone(),
                snapshot_version: snapshot.snapshot_version,
                source_generation: snapshot.source_generation,
                status: snapshot.status.clone(),
                content_hash: snapshot.content_hash.clone(),
                created_at: snapshot.created_at.clone(),
            }
        }),
    }
}

fn default_graph_metric() -> GraphMetricExplanation {
    GraphMetricExplanation {
        raw: 0.0,
        normalized: 0.0,
        rank: None,
        weight: 0.0,
        contribution: 0.0,
        formula: "metric unavailable".to_owned(),
    }
}

fn evidence_support_count(links: &[MemoryLinkSummary]) -> u32 {
    links
        .iter()
        .filter(|link| link.relation == "supports")
        .map(|link| link.evidence_count.max(1))
        .fold(0_u32, u32::saturating_add)
}

fn contradiction_count(
    links: &[MemoryLinkSummary],
    contradictions: &[ContradictionMetadata],
) -> u32 {
    let link_count = links
        .iter()
        .filter(|link| link.relation == "contradicts")
        .map(|link| link.evidence_count.max(1))
        .fold(0_u32, u32::saturating_add);
    link_count.saturating_add(u32::try_from(contradictions.len()).unwrap_or(u32::MAX))
}

fn orphan_penalty(links: &[MemoryLinkSummary]) -> f64 {
    if links.is_empty() { 1.0 } else { 0.0 }
}

fn stale_bridge_penalty(validity_status: &str, betweenness_normalized: f64) -> f64 {
    if validity_status == "expired" {
        round_graph_score(betweenness_normalized.clamp(0.0, 1.0))
    } else {
        0.0
    }
}

fn round_graph_score(value: f64) -> f64 {
    if value.is_finite() {
        (value * 10_000.0).round() / 10_000.0
    } else {
        0.0
    }
}

fn determine_origin(trust_class: &str) -> String {
    match trust_class {
        "human_explicit" => "Explicitly remembered via `ee remember`".to_string(),
        "agent_validated" => "Agent assertion with validated outcome evidence".to_string(),
        "agent_assertion" => "Agent assertion awaiting validation".to_string(),
        "cass_evidence" => "Imported from CASS session evidence".to_string(),
        "legacy_import" => "Imported from a legacy Eidetic Engine store".to_string(),
        _ => format!("Created with trust class: {trust_class}"),
    }
}

fn compute_selection_score(confidence: f32, utility: f32, importance: f32) -> f32 {
    0.5 * confidence + 0.3 * utility + 0.2 * importance
}

fn fetch_agent_profile_selection_explanation(
    conn: &DbConnection,
    workspace_id: &str,
    memory_id: &str,
) -> Option<AgentProfileSelectionExplanation> {
    let agent_name = crate::core::memory_scope::current_agent_name()?;
    let profile = conn
        .get_agent_context_profile(workspace_id, &agent_name, memory_id)
        .ok()
        .flatten()?;
    Some(agent_profile_selection_explanation(
        agent_name,
        profile.counts,
        profile.last_seen_at,
    ))
}

fn agent_profile_selection_explanation(
    agent_name: String,
    counts: AgentContextProfileCounts,
    last_seen_at: String,
) -> AgentProfileSelectionExplanation {
    let bias = counts.bias();
    AgentProfileSelectionExplanation {
        schema: AGENT_CONTEXT_PROFILE_SCHEMA_V1,
        agent_name_hash: agent_context_profile_agent_hash(&agent_name),
        agent_name,
        helpful_count: counts.helpful_count,
        harmful_count: counts.harmful_count,
        ignored_count: counts.ignored_count,
        observed_outcomes: counts.observed_outcomes(),
        bias: bias.weight,
        max_bias_magnitude: AGENT_PROFILE_BIAS_CAP,
        cold_start: bias.cold_start,
        cold_start_threshold: AGENT_PROFILE_COLD_START_OUTCOMES,
        last_seen_at,
    }
}

fn agent_context_profile_agent_hash(agent_name: &str) -> String {
    let digest = blake3::hash(agent_name.as_bytes()).to_hex().to_string();
    format!("blake3:{}", &digest[..12])
}

fn latest_pack_selection(
    conn: &DbConnection,
    memory_id: &str,
) -> Result<Option<PackSelectionExplanation>, String> {
    let rows = conn
        .query(
            "SELECT pi.pack_id, pr.query, pr.profile, pi.rank, pi.section, pi.estimated_tokens, pi.relevance, pi.utility, pi.why, pr.pack_hash, pr.created_at \
             FROM pack_items pi \
             JOIN pack_records pr ON pr.id = pi.pack_id \
             WHERE pi.memory_id = ?1 \
             ORDER BY pr.created_at DESC, pi.pack_id DESC, pi.rank ASC \
             LIMIT 1",
            &[Value::Text(memory_id.to_string())],
        )
        .map_err(|error| format!("Failed to query pack selection: {error}"))?;

    rows.first().map(pack_selection_from_row).transpose()
}

fn pack_selection_from_row(row: &Row) -> Result<PackSelectionExplanation, String> {
    Ok(PackSelectionExplanation {
        pack_id: required_text(row, 0, "pack_id")?,
        query: required_text(row, 1, "query")?,
        profile: required_text(row, 2, "profile")?,
        rank: required_u32(row, 3, "rank")?,
        section: required_text(row, 4, "section")?,
        estimated_tokens: required_u32(row, 5, "estimated_tokens")?,
        relevance: required_f32(row, 6, "relevance")?,
        utility: required_f32(row, 7, "utility")?,
        why: required_text(row, 8, "why")?,
        pack_hash: required_text(row, 9, "pack_hash")?,
        selected_at: required_text(row, 10, "created_at")?,
    })
}

fn required_text(row: &Row, index: usize, column: &str) -> Result<String, String> {
    row.get(index)
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("Pack selection column {column} was missing or not text"))
}

fn required_u32(row: &Row, index: usize, column: &str) -> Result<u32, String> {
    let raw = row
        .get(index)
        .and_then(|value| value.as_i64())
        .ok_or_else(|| format!("Pack selection column {column} was missing or not integer"))?;
    u32::try_from(raw)
        .map_err(|_| format!("Pack selection column {column} was out of range: {raw}"))
}

fn required_f32(row: &Row, index: usize, column: &str) -> Result<f32, String> {
    row.get(index)
        .and_then(|value| value.as_f64())
        .map(|value| value as f32)
        .ok_or_else(|| format!("Pack selection column {column} was missing or not numeric"))
}

struct WhyEvidenceFetch<T> {
    items: Vec<T>,
    degradation: Option<WhyDegradation>,
}

impl<T> WhyEvidenceFetch<T> {
    fn available(items: Vec<T>) -> Self {
        Self {
            items,
            degradation: None,
        }
    }

    fn unavailable(code: &'static str, label: &str, error: impl std::fmt::Display) -> Self {
        Self {
            items: Vec::new(),
            degradation: Some(WhyDegradation {
                code,
                severity: "medium",
                message: format!(
                    "Could not read {label} for this memory; the evidence is omitted instead of treated as absent. Error: {error}"
                ),
                repair: Some("ee db migrate".to_owned()),
            }),
        }
    }
}

/// Fetch contradiction feedback events for a memory (EE-263).
fn fetch_contradictions(
    conn: &DbConnection,
    memory_id: &str,
) -> WhyEvidenceFetch<ContradictionMetadata> {
    let events = match conn.list_feedback_events_for_target("memory", memory_id) {
        Ok(e) => e,
        Err(error) => {
            return WhyEvidenceFetch::unavailable(
                "why_contradictions_unavailable",
                "contradiction feedback events",
                error,
            );
        }
    };

    WhyEvidenceFetch::available(
        events
            .into_iter()
            .filter(|e| e.signal == "contradiction")
            .map(|e| ContradictionMetadata {
                event_id: e.id,
                weight: e.weight,
                source_type: e.source_type,
                reason: e.reason,
                created_at: e.created_at,
                applied: e.applied_at.is_some(),
            })
            .collect(),
    )
}

/// Fetch memory links for a memory (EE-LINK-USAGE-001).
fn fetch_links(conn: &DbConnection, memory_id: &str) -> WhyEvidenceFetch<MemoryLinkSummary> {
    let stored_links = match conn.list_memory_links_for_memory(memory_id, None) {
        Ok(links) => links,
        Err(error) => {
            return WhyEvidenceFetch::unavailable("why_links_unavailable", "memory links", error);
        }
    };

    WhyEvidenceFetch::available(
        stored_links
            .into_iter()
            .filter(|link| {
                crate::graph::memory_link_mesh_metadata_visible(link.metadata_json.as_deref())
            })
            .map(|link| {
                let direction = if !link.directed {
                    "undirected".to_string()
                } else if link.src_memory_id == memory_id {
                    "outgoing".to_string()
                } else {
                    "incoming".to_string()
                };

                let linked_memory_id = if link.src_memory_id == memory_id {
                    link.dst_memory_id.clone()
                } else {
                    link.src_memory_id.clone()
                };

                MemoryLinkSummary {
                    link_id: link.id,
                    linked_memory_id,
                    relation: link.relation,
                    direction,
                    confidence: link.confidence,
                    weight: link.weight,
                    evidence_count: link.evidence_count,
                    source: link.source,
                    created_at: link.created_at,
                }
            })
            .collect(),
    )
}

const WHY_HISTORY_LIMIT: usize = 50;

fn fetch_history(conn: &DbConnection, memory_id: &str) -> WhyEvidenceFetch<MemoryHistorySummary> {
    let stored_entries = match conn.list_audit_by_target("memory", memory_id, None) {
        Ok(entries) => entries,
        Err(error) => {
            return WhyEvidenceFetch::unavailable(
                "why_history_unavailable",
                "memory audit history",
                error,
            );
        }
    };

    // `ee why` records a best-effort inspection audit after assembling the
    // report. Keep that ledger write, but exclude prior self-inspection rows
    // from the response so repeated read calls remain deterministic.
    let visible_entries: Vec<_> = stored_entries
        .into_iter()
        .filter(|entry| entry.action != crate::db::audit_actions::WHY_INSPECTED)
        .collect();
    let total_count = visible_entries.len() as u32;
    let truncated = visible_entries.len() > WHY_HISTORY_LIMIT;
    let entries = visible_entries
        .into_iter()
        .take(WHY_HISTORY_LIMIT)
        .map(|entry| MemoryHistorySummaryEntry {
            audit_id: entry.id,
            timestamp: entry.timestamp,
            actor: entry.actor,
            action: entry.action,
            details: entry.details.map(redact_why_history_details),
        })
        .collect();

    WhyEvidenceFetch::available(vec![MemoryHistorySummary {
        entries,
        total_count,
        truncated,
    }])
}

fn redact_why_history_details(details: String) -> String {
    match serde_json::from_str::<serde_json::Value>(&details) {
        Ok(mut value) => {
            redact_why_history_json_value(&mut value);
            serde_json::to_string(&value)
                .unwrap_or_else(|_| redact_why_search_result_provenance_uri(details))
        }
        Err(_) => redact_why_search_result_provenance_uri(details),
    }
}

fn redact_why_history_json_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            *text = redact_why_search_result_provenance_uri(std::mem::take(text));
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_why_history_json_value(item);
            }
        }
        serde_json::Value::Object(fields) => {
            for item in fields.values_mut() {
                redact_why_history_json_value(item);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

fn fetch_verification_evidence(
    conn: &DbConnection,
    target_type: &str,
    target_id: &str,
) -> WhyEvidenceFetch<VerificationEvidenceRecord> {
    match crate::core::verify::verification_records_for_target(conn, target_type, target_id) {
        Ok(records) => WhyEvidenceFetch::available(records),
        Err(error) => WhyEvidenceFetch::unavailable(
            "why_verification_evidence_unavailable",
            "verification evidence",
            error,
        ),
    }
}

/// Fetch rationale traces linked to a memory (EE-RATIONALE-TRACE-001).
fn fetch_rationale_traces(
    conn: &DbConnection,
    workspace_id: &str,
    memory_id: &str,
) -> WhyEvidenceFetch<RationaleTraceSummary> {
    let stored = match conn.list_rationale_traces_for_target(workspace_id, "memory", memory_id) {
        Ok(traces) => traces,
        Err(error) => {
            return WhyEvidenceFetch::unavailable(
                "why_rationale_traces_unavailable",
                "rationale traces",
                error,
            );
        }
    };

    WhyEvidenceFetch::available(
        stored
            .into_iter()
            .filter_map(|s| RationaleTraceSummary::from_trace(&s.trace))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{CreateArtifactInput, CreateSessionInput, CreateWorkspaceInput};
    use crate::models::{
        RationaleTraceKind, RationaleTracePosture, RationaleTraceVisibility, RedactionStatus,
    };

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn why_report_not_found_is_correct() -> TestResult {
        let report = WhyReport::not_found("mem_test".to_string());

        ensure(report.found, false, "found")?;
        ensure(report.memory_id, "mem_test".to_string(), "memory_id")?;
        ensure(report.storage.is_none(), true, "storage is none")?;
        ensure(report.error.is_none(), true, "no error")
    }

    #[test]
    fn why_report_error_captures_message() -> TestResult {
        let report = WhyReport::error("mem_test".to_string(), "db error".to_string());

        ensure(report.found, false, "found")?;
        ensure(report.error, Some("db error".to_string()), "error message")
    }

    #[test]
    fn why_report_version_matches_package() -> TestResult {
        let report = WhyReport::not_found("mem_test".to_string());
        ensure(report.version, env!("CARGO_PKG_VERSION"), "version")
    }

    #[test]
    fn selection_score_computation() -> TestResult {
        let score = compute_selection_score(0.8, 0.6, 0.7);
        let expected = 0.5 * 0.8 + 0.3 * 0.6 + 0.2 * 0.7;
        ensure((score - expected).abs() < 0.001, true, "score computation")
    }

    #[test]
    fn agent_profile_selection_explanation_reports_counts_and_cap() -> TestResult {
        let explanation = agent_profile_selection_explanation(
            "FrostyMoose".to_string(),
            AgentContextProfileCounts::new(20, 1, 3),
            "2026-05-16T01:12:00Z".to_string(),
        );

        ensure(
            explanation.schema,
            AGENT_CONTEXT_PROFILE_SCHEMA_V1,
            "schema",
        )?;
        ensure(explanation.helpful_count, 20, "helpful count")?;
        ensure(explanation.harmful_count, 1, "harmful count")?;
        ensure(explanation.ignored_count, 3, "ignored count")?;
        ensure(explanation.observed_outcomes, 24, "observed outcomes")?;
        ensure(explanation.cold_start, false, "cold start")?;
        ensure(
            explanation.bias.abs() <= AGENT_PROFILE_BIAS_CAP,
            true,
            "bias cap",
        )?;
        ensure(
            explanation.cold_start_threshold,
            AGENT_PROFILE_COLD_START_OUTCOMES,
            "cold start threshold",
        )
    }

    #[test]
    fn result_target_resolves_to_search_doc_id() -> TestResult {
        ensure(
            resolve_why_memory_id("result:mem_00000000000000000000000001"),
            "mem_00000000000000000000000001",
            "result target",
        )
    }

    #[test]
    fn result_target_classifies_non_memory_sources() -> TestResult {
        ensure(
            resolve_why_target("result:sess_00000000000000000000000001")
                .unsupported_result_source(),
            Some(WhyResultDocumentSource::Session),
            "session result target",
        )?;
        ensure(
            resolve_why_target("result:art_00000000000000000000000001").unsupported_result_source(),
            Some(WhyResultDocumentSource::Artifact),
            "artifact result target",
        )?;
        ensure(
            resolve_why_target("result:curate_0000000000000000000001").unsupported_result_source(),
            Some(WhyResultDocumentSource::CurationCandidate),
            "curation candidate result target",
        )?;
        ensure(
            resolve_why_target("result:mem_00000000000000000000000001").unsupported_result_source(),
            None,
            "memory result target",
        )
    }

    #[test]
    fn empty_result_target_stays_queryable_for_not_found_errors() -> TestResult {
        ensure(
            resolve_why_memory_id("result:"),
            "result:",
            "empty result target",
        )
    }

    #[test]
    fn determine_origin_for_explicit_memory() -> TestResult {
        let origin = determine_origin("human_explicit");
        ensure(
            origin.contains("ee remember"),
            true,
            "human_explicit origin mentions ee remember",
        )
    }

    #[test]
    fn determine_origin_for_cass_import() -> TestResult {
        let origin = determine_origin("cass_evidence");
        ensure(
            origin.contains("CASS"),
            true,
            "cass_evidence origin mentions CASS",
        )
    }

    #[test]
    fn found_report_can_carry_pack_selection() -> TestResult {
        let selection = PackSelectionExplanation {
            pack_id: "pack_00000000000000000000000001".to_string(),
            query: "prepare release".to_string(),
            profile: "compact".to_string(),
            rank: 1,
            section: "procedural_rules".to_string(),
            estimated_tokens: 8,
            relevance: 0.91,
            utility: 0.8,
            why: "selected because it matches the release task".to_string(),
            pack_hash: "hash".to_string(),
            selected_at: "2026-04-29T12:00:00Z".to_string(),
        };
        let report = build_report(
            "mem_00000000000000000000000001",
            StorageExplanation {
                origin: "Explicitly remembered via `ee remember`".to_string(),
                trust_class: "human_explicit".to_string(),
                trust_subclass: None,
                provenance_uri: None,
                workflow_id: None,
                created_at: "2026-04-29T12:00:00Z".to_string(),
                valid_from: None,
                valid_to: None,
                validity_status: "unknown".to_string(),
                validity_window_kind: "unbounded".to_string(),
            },
            RetrievalExplanation {
                confidence: 0.9,
                utility: 0.8,
                importance: 0.7,
                tags: Vec::new(),
                level: "procedural".to_string(),
                kind: "rule".to_string(),
            },
            ReportSelectionInputs {
                is_active: true,
                selection_score: 0.83,
                above_threshold: true,
                latest_pack_selection: Some(selection),
                lifecycle: LifecycleExplanation {
                    status: "active",
                    tombstoned_at: None,
                    tombstoned_reason: None,
                },
                contradictions: Vec::new(),
                links: Vec::new(),
                history: None,
                rationale_traces: Vec::new(),
                verification_evidence: Vec::new(),
                graph_retrieval: graph_retrieval_unavailable(
                    "wsp_01234567890123456789012345",
                    "graph_snapshot_missing",
                    "medium",
                    "No persisted graph snapshot exists for feature enrichment.".to_string(),
                    "ee graph centrality-refresh",
                    &[],
                    &[],
                ),
                degraded: Vec::new(),
                agent_profile: None,
            },
        );

        ensure(report.found, true, "found")?;
        ensure(
            report
                .selection
                .and_then(|selection| selection.latest_pack_selection)
                .map(|selection| selection.rank),
            Some(1),
            "pack rank",
        )
    }

    #[test]
    fn explain_memory_includes_linked_verification_evidence() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join(".ee").join("ee.db");
        std::fs::create_dir_all(
            database_path
                .parent()
                .ok_or("database path should have parent")?,
        )
        .map_err(|error| error.to_string())?;
        let memory_id = "mem_verifywhy00000000000000001";
        let evidence = crate::models::sample_verification_evidence_records()
            .into_iter()
            .next()
            .ok_or("sample evidence exists")?;
        let record_report = crate::core::verify::record_verification_evidence(
            crate::core::verify::VerificationRecordOptions {
                database_path: &database_path,
                workspace_path: temp.path(),
                target_type: "memory",
                target_id: memory_id,
                actor: Some("codex:test"),
                evidence: evidence.clone(),
            },
        )
        .map_err(|error| error.to_string())?;

        let connection = crate::db::DbConnection::open_file(&database_path)
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                memory_id,
                &crate::db::CreateMemoryInput {
                    workspace_id: record_report.workspace_id,
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Run verification gates before closing beads.".to_owned(),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.7,
                    importance: 0.6,
                    provenance_uri: None,
                    trust_class: "agent_assertion".to_owned(),
                    trust_subclass: None,
                    tags: vec!["verification".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let report = explain_memory(&WhyOptions {
            database_path: &database_path,
            memory_id,
            confidence_threshold: WhyOptions::DEFAULT_CONFIDENCE_THRESHOLD,
        });

        ensure(report.found, true, "why found memory")?;
        ensure(
            report.verification_evidence.len(),
            1_usize,
            "verification evidence count",
        )?;
        ensure(
            report.verification_evidence[0].verification_id.as_str(),
            evidence.verification_id.as_str(),
            "verification id",
        )
    }

    #[test]
    fn explain_memory_redacts_stored_memory_provenance_uri() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join(".ee").join("ee.db");
        std::fs::create_dir_all(
            database_path
                .parent()
                .ok_or("database path should have parent")?,
        )
        .map_err(|error| error.to_string())?;
        let conn = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        conn.migrate().map_err(|error| error.to_string())?;
        conn.insert_workspace(
            "wsp_whyredact0000000000000001",
            &CreateWorkspaceInput {
                path: temp.path().display().to_string(),
                name: Some("why-redaction".to_string()),
            },
        )
        .map_err(|error| error.to_string())?;
        conn.insert_memory(
            "mem_whyredact0000000000000001",
            &crate::db::CreateMemoryInput {
                workspace_id: "wsp_whyredact0000000000000001".to_string(),
                level: "procedural".to_string(),
                kind: "rule".to_string(),
                content: "Do not leak stored provenance in why output.".to_string(),
                workflow_id: None,
                confidence: 0.8,
                utility: 0.7,
                importance: 0.6,
                provenance_uri: Some(
                    concat!(
                        "file:///Users/alice/private/repo/notes.md?",
                        "api",
                        "_key=redaction-fixture"
                    )
                    .to_string(),
                ),
                trust_class: "human_explicit".to_string(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
        )
        .map_err(|error| error.to_string())?;

        let report = explain_memory(&WhyOptions {
            database_path: &database_path,
            memory_id: "mem_whyredact0000000000000001",
            confidence_threshold: WhyOptions::DEFAULT_CONFIDENCE_THRESHOLD,
        });
        let provenance = report
            .storage
            .as_ref()
            .and_then(|storage| storage.provenance_uri.as_deref())
            .ok_or_else(|| "stored memory provenance present".to_string())?;

        ensure(
            provenance.contains("/Users/alice"),
            false,
            "stored provenance path redacted",
        )?;
        ensure(
            provenance.contains("[REDACTED_PATH]"),
            true,
            "stored provenance path placeholder",
        )?;
        ensure(
            provenance.contains("redaction-fixture"),
            false,
            "stored provenance secret value redacted",
        )?;
        ensure(
            provenance.contains("[REDACTED:"),
            true,
            "stored provenance secret placeholder",
        )
    }

    #[test]
    fn explain_memory_redacts_history_details_without_mutating_audit_row() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join(".ee").join("ee.db");
        std::fs::create_dir_all(
            database_path
                .parent()
                .ok_or("database path should have parent")?,
        )
        .map_err(|error| error.to_string())?;
        let conn = DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        conn.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_whyhist0000000000000001";
        let memory_id = "mem_whyhist0000000000000001";
        conn.insert_workspace(
            workspace_id,
            &CreateWorkspaceInput {
                path: temp.path().display().to_string(),
                name: Some("why-history-redaction".to_string()),
            },
        )
        .map_err(|error| error.to_string())?;
        conn.insert_memory(
            memory_id,
            &crate::db::CreateMemoryInput {
                workspace_id: workspace_id.to_string(),
                level: "procedural".to_string(),
                kind: "rule".to_string(),
                content: "History details are redacted at the why surface.".to_string(),
                workflow_id: None,
                confidence: 0.8,
                utility: 0.7,
                importance: 0.6,
                provenance_uri: None,
                trust_class: "human_explicit".to_string(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
        )
        .map_err(|error| error.to_string())?;
        let raw_details = serde_json::json!({
            "schema": "ee.audit.memory_create.v1",
            "provenanceUri": concat!(
                "file:///Users/alice/private/repo/notes.md?",
                "api",
                "_key=redaction-fixture"
            ),
            "nested": {
                "reason": "reviewed /Volumes/USBNVME16TB/private/session.jsonl"
            },
            "safe": "cass://session/public#L1"
        })
        .to_string();
        conn.insert_audit(
            "audit_whyhist000000000000000001",
            &crate::db::CreateAuditInput {
                workspace_id: Some(workspace_id.to_string()),
                actor: Some("test-agent".to_string()),
                action: crate::db::audit_actions::MEMORY_CREATE.to_string(),
                target_type: Some("memory".to_string()),
                target_id: Some(memory_id.to_string()),
                details: Some(raw_details.clone()),
            },
        )
        .map_err(|error| error.to_string())?;

        let report = explain_memory(&WhyOptions {
            database_path: &database_path,
            memory_id,
            confidence_threshold: WhyOptions::DEFAULT_CONFIDENCE_THRESHOLD,
        });
        let details = report
            .history
            .as_ref()
            .and_then(|history| history.entries.first())
            .and_then(|entry| entry.details.as_deref())
            .ok_or_else(|| "why history details present".to_string())?;

        ensure(
            details.contains("/Users/alice") || details.contains("/Volumes/USBNVME16TB"),
            false,
            "why history details path redacted",
        )?;
        ensure(
            details.contains("redaction-fixture"),
            false,
            "why history details secret value redacted",
        )?;
        ensure(
            details.contains("[REDACTED_PATH]"),
            true,
            "why history details path placeholder",
        )?;
        ensure(
            details.contains("[REDACTED:"),
            true,
            "why history details secret placeholder",
        )?;
        ensure(
            details.contains("cass://session/public#L1"),
            true,
            "safe cass source remains visible",
        )?;

        let raw_audit_details = conn
            .list_audit_by_target("memory", memory_id, None)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|entry| entry.action == crate::db::audit_actions::MEMORY_CREATE)
            .and_then(|entry| entry.details)
            .ok_or_else(|| "raw memory-create audit details present".to_string())?;
        ensure(
            raw_audit_details,
            raw_details,
            "raw audit details preserved",
        )
    }

    #[test]
    fn explain_memory_seeded_replays_why_inspected_audit_id() -> TestResult {
        fn run_seeded(seed: u64) -> Result<String, String> {
            let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
            let database_path = temp.path().join(".ee").join("ee.db");
            std::fs::create_dir_all(
                database_path
                    .parent()
                    .ok_or("database path should have parent")?,
            )
            .map_err(|error| error.to_string())?;
            let workspace_id = "wsp_00000000000000000000000001";
            let memory_id = "mem_00000000000000000000000001";
            let connection = crate::db::DbConnection::open_file(&database_path)
                .map_err(|error| error.to_string())?;
            connection.migrate().map_err(|error| error.to_string())?;
            connection
                .insert_workspace(
                    workspace_id,
                    &crate::db::CreateWorkspaceInput {
                        path: temp.path().display().to_string(),
                        name: Some("why-seeded".to_owned()),
                    },
                )
                .map_err(|error| error.to_string())?;
            connection
                .insert_memory(
                    memory_id,
                    &crate::db::CreateMemoryInput {
                        workspace_id: workspace_id.to_owned(),
                        level: "procedural".to_owned(),
                        kind: "rule".to_owned(),
                        content: "Use seeded why audit IDs in replay tests.".to_owned(),
                        workflow_id: None,
                        confidence: 0.8,
                        utility: 0.7,
                        importance: 0.6,
                        provenance_uri: None,
                        trust_class: "agent_assertion".to_owned(),
                        trust_subclass: None,
                        tags: Vec::new(),
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| error.to_string())?;

            let mut determinism = Deterministic::from_seed(seed);
            let report = explain_memory_seeded(
                &WhyOptions {
                    database_path: &database_path,
                    memory_id,
                    confidence_threshold: WhyOptions::DEFAULT_CONFIDENCE_THRESHOLD,
                },
                &mut determinism,
            );

            ensure(report.found, true, "why found memory")?;
            let audits = connection
                .list_audit_by_target("memory", memory_id, None)
                .map_err(|error| error.to_string())?;
            let why_audit_ids = audits
                .iter()
                .filter(|entry| entry.action == crate::db::audit_actions::WHY_INSPECTED)
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>();
            ensure(why_audit_ids.len(), 1_usize, "why audit row count")?;
            let audit_id = why_audit_ids
                .first()
                .ok_or_else(|| "why audit row missing".to_string())?
                .clone();
            ensure(audit_id.starts_with("audit_"), true, "audit id prefix")?;
            Ok(audit_id)
        }

        let first = run_seeded(45_001)?;
        let replay = run_seeded(45_001)?;
        let other = run_seeded(45_002)?;
        ensure(first.clone(), replay, "same seed replays why audit ID")?;
        ensure(first == other, false, "different seed changes why audit ID")
    }

    #[test]
    fn explain_memory_attaches_revision_lineage_for_revised_memory() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join(".ee").join("ee.db");
        std::fs::create_dir_all(
            database_path
                .parent()
                .ok_or("database path should have parent")?,
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(
            temp.path().join(".ee").join("config.toml"),
            "[graph.feature.revision_dominance]\nenabled = true\n",
        )
        .map_err(|error| error.to_string())?;
        let original_id = "mem_whyrevision000000000000001";
        let workspace_id = "wsp_whyrevisionlineage00000000";
        let connection =
            crate::db::DbConnection::open_file(&database_path).map_err(|e| e.to_string())?;
        connection.migrate().map_err(|e| e.to_string())?;
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: temp.path().display().to_string(),
                    name: Some("why-revision-lineage".to_owned()),
                },
            )
            .map_err(|e| e.to_string())?;
        connection
            .insert_memory(
                original_id,
                &crate::db::CreateMemoryInput {
                    workspace_id: workspace_id.to_owned(),
                    level: "semantic".to_owned(),
                    kind: "fact".to_owned(),
                    content: "Revision lineage starts here.".to_owned(),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.7,
                    importance: 0.6,
                    provenance_uri: None,
                    trust_class: "agent_assertion".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: Some("2026-05-15T00:00:00Z".to_owned()),
                    valid_to: None,
                },
            )
            .map_err(|e| e.to_string())?;
        drop(connection);

        let revised =
            crate::core::memory::revise_memory(&crate::core::memory::ReviseMemoryOptions {
                database_path: &database_path,
                original_memory_id: original_id,
                content: Some("Revision lineage continues here."),
                level: None,
                kind: None,
                confidence: None,
                tags: None,
                provenance_uri: None,
                reason: crate::core::memory::ReviseReason::Update,
                actor: Some("StormyCove"),
                dry_run: false,
            });
        let revised_id = revised
            .new_id
            .clone()
            .ok_or_else(|| "revision should create a new memory id".to_string())?;

        let report = explain_memory(&WhyOptions {
            database_path: &database_path,
            memory_id: &revised_id,
            confidence_threshold: WhyOptions::DEFAULT_CONFIDENCE_THRESHOLD,
        });

        ensure(report.found, true, "why found revised memory")?;
        let lineage = report
            .revision_lineage
            .as_ref()
            .ok_or_else(|| "why should attach revision lineage".to_string())?;
        ensure(
            lineage["memoryId"].as_str(),
            Some(revised_id.as_str()),
            "lineage memory id",
        )?;
        ensure(
            lineage["immediateDominator"].as_str(),
            Some(original_id),
            "lineage immediate dominator",
        )?;
        ensure(
            lineage["ancestorsAtDepth"]["0"][0].as_str(),
            Some(revised_id.as_str()),
            "lineage depth 0",
        )?;
        ensure(
            lineage["ancestorsAtDepth"]["1"][0].as_str(),
            Some(original_id),
            "lineage depth 1",
        )
    }

    #[test]
    fn explain_memory_reports_disabled_revision_lineage_by_default() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join(".ee").join("ee.db");
        std::fs::create_dir_all(
            database_path
                .parent()
                .ok_or("database path should have parent")?,
        )
        .map_err(|error| error.to_string())?;
        let memory_id = "mem_whydisabledrevision0000001";
        let workspace_id = "wsp_whydisabledrevision0000000";
        let connection =
            crate::db::DbConnection::open_file(&database_path).map_err(|e| e.to_string())?;
        connection.migrate().map_err(|e| e.to_string())?;
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: temp.path().display().to_string(),
                    name: Some("why-disabled-revision-lineage".to_owned()),
                },
            )
            .map_err(|e| e.to_string())?;
        connection
            .insert_memory(
                memory_id,
                &crate::db::CreateMemoryInput {
                    workspace_id: workspace_id.to_owned(),
                    level: "semantic".to_owned(),
                    kind: "fact".to_owned(),
                    content: "Revision dominance should be feature-gated.".to_owned(),
                    workflow_id: None,
                    confidence: 0.8,
                    utility: 0.7,
                    importance: 0.6,
                    provenance_uri: None,
                    trust_class: "agent_assertion".to_owned(),
                    trust_subclass: None,
                    tags: Vec::new(),
                    valid_from: Some("2026-05-15T00:00:00Z".to_owned()),
                    valid_to: None,
                },
            )
            .map_err(|e| e.to_string())?;
        drop(connection);

        let report = explain_memory(&WhyOptions {
            database_path: &database_path,
            memory_id,
            confidence_threshold: WhyOptions::DEFAULT_CONFIDENCE_THRESHOLD,
        });

        ensure(report.found, true, "why found memory")?;
        let lineage = report
            .revision_lineage
            .as_ref()
            .ok_or_else(|| "disabled gate should attach a lineage sentinel".to_string())?;
        ensure(
            lineage["validationStatus"].as_str(),
            Some("disabled"),
            "disabled validation status",
        )?;
        ensure(
            lineage["degraded"][0]["code"].as_str(),
            Some("graph_feature_disabled"),
            "disabled degraded code",
        )?;
        ensure(
            lineage["degraded"][0]["repair"].as_str(),
            Some("ee config set graph.feature.revision_dominance.enabled true"),
            "disabled repair",
        )?;
        ensure(
            lineage["degraded"][0]["sources"][0].as_str(),
            Some("why_revision_lineage"),
            "disabled degraded source",
        )
    }

    #[test]
    fn why_revision_lineage_aggregates_dominance_degradations() -> TestResult {
        let degraded = aggregate_why_dominance_degraded(&[
            crate::graph::dominance::DominanceDegradation {
                code: "graph_dominance_no_revision_chain".to_owned(),
                severity: "info".to_owned(),
                message: "low-detail graph dominance message".to_owned(),
                repair: None,
            },
            crate::graph::dominance::DominanceDegradation {
                code: "graph_dominance_no_revision_chain".to_owned(),
                severity: "warning".to_owned(),
                message: "higher-severity graph dominance message".to_owned(),
                repair: Some("ee graph refresh --workspace .".to_owned()),
            },
        ]);

        ensure(degraded.len(), 1_usize, "aggregate duplicate code count")?;
        ensure(
            degraded[0]["code"].as_str(),
            Some("graph_dominance_no_revision_chain"),
            "aggregate code",
        )?;
        ensure(
            degraded[0]["severity"].as_str(),
            Some("warning"),
            "aggregate severity",
        )?;
        ensure(
            degraded[0]["message"].as_str(),
            Some("higher-severity graph dominance message"),
            "aggregate message",
        )?;
        ensure(
            degraded[0]["repair"].as_str(),
            Some("ee graph refresh --workspace ."),
            "aggregate repair",
        )?;
        ensure(
            degraded[0]["sources"][0].as_str(),
            Some("graph_dominance"),
            "aggregate source",
        )
    }

    #[test]
    fn memory_link_summary_direction_outgoing() -> TestResult {
        let link = MemoryLinkSummary {
            link_id: "link_test".to_string(),
            linked_memory_id: "mem_other".to_string(),
            relation: "supports".to_string(),
            direction: "outgoing".to_string(),
            confidence: 0.9,
            weight: 1.0,
            evidence_count: 3,
            source: "agent".to_string(),
            created_at: "2026-04-30T12:00:00Z".to_string(),
        };
        ensure(link.direction, "outgoing".to_string(), "direction")?;
        ensure(link.relation, "supports".to_string(), "relation")
    }

    #[test]
    fn memory_link_summary_direction_incoming() -> TestResult {
        let link = MemoryLinkSummary {
            link_id: "link_test".to_string(),
            linked_memory_id: "mem_source".to_string(),
            relation: "contradicts".to_string(),
            direction: "incoming".to_string(),
            confidence: 0.85,
            weight: 0.8,
            evidence_count: 1,
            source: "human".to_string(),
            created_at: "2026-04-30T12:00:00Z".to_string(),
        };
        ensure(link.direction, "incoming".to_string(), "direction")?;
        ensure(link.relation, "contradicts".to_string(), "relation")
    }

    #[test]
    fn memory_link_summary_undirected() -> TestResult {
        let link = MemoryLinkSummary {
            link_id: "link_test".to_string(),
            linked_memory_id: "mem_related".to_string(),
            relation: "related".to_string(),
            direction: "undirected".to_string(),
            confidence: 0.7,
            weight: 0.5,
            evidence_count: 2,
            source: "auto".to_string(),
            created_at: "2026-04-30T12:00:00Z".to_string(),
        };
        ensure(link.direction, "undirected".to_string(), "direction")?;
        ensure(link.relation, "related".to_string(), "relation")
    }

    #[test]
    fn why_report_with_links() -> TestResult {
        let links = vec![
            MemoryLinkSummary {
                link_id: "link_01".to_string(),
                linked_memory_id: "mem_support".to_string(),
                relation: "supports".to_string(),
                direction: "outgoing".to_string(),
                confidence: 0.9,
                weight: 1.0,
                evidence_count: 2,
                source: "agent".to_string(),
                created_at: "2026-04-30T12:00:00Z".to_string(),
            },
            MemoryLinkSummary {
                link_id: "link_02".to_string(),
                linked_memory_id: "mem_contradict".to_string(),
                relation: "contradicts".to_string(),
                direction: "incoming".to_string(),
                confidence: 0.8,
                weight: 0.5,
                evidence_count: 1,
                source: "human".to_string(),
                created_at: "2026-04-30T12:01:00Z".to_string(),
            },
        ];

        let report = WhyReport::not_found("mem_test".to_string()).with_links(links);

        ensure(report.links.len(), 2, "link count")?;
        ensure(
            report.links[0].relation.clone(),
            "supports".to_string(),
            "first link relation",
        )?;
        ensure(
            report.links[1].relation.clone(),
            "contradicts".to_string(),
            "second link relation",
        )
    }

    #[test]
    fn why_report_links_default_empty() -> TestResult {
        let report = WhyReport::not_found("mem_test".to_string());
        ensure(
            report.links.is_empty(),
            true,
            "links should be empty by default",
        )
    }

    fn insert_why_artifact(
        conn: &DbConnection,
        artifact_id: &str,
        provenance_uri: Option<&str>,
        original_path: Option<&str>,
        external_ref: Option<&str>,
    ) -> TestResult {
        conn.upsert_artifact(
            artifact_id,
            &CreateArtifactInput {
                workspace_id: "wsp_00000000000000000000000001".to_owned(),
                source_kind: "external".to_owned(),
                artifact_type: "log".to_owned(),
                original_path: original_path.map(ToOwned::to_owned),
                canonical_path: None,
                external_ref: external_ref.map(ToOwned::to_owned),
                content_hash:
                    "blake3:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                        .to_owned(),
                media_type: "text/plain".to_owned(),
                size_bytes: 42,
                redaction_status: "checked".to_owned(),
                snippet: None,
                snippet_hash: None,
                provenance_uri: provenance_uri.map(ToOwned::to_owned),
                metadata_json: Some(r#"{"title":"why artifact"}"#.to_owned()),
            },
        )
        .map_err(|error| error.to_string())
    }

    fn insert_why_session(
        conn: &DbConnection,
        session_id: &str,
        cass_session_id: &str,
        source_path: Option<&str>,
    ) -> TestResult {
        conn.insert_session(
            session_id,
            &CreateSessionInput {
                workspace_id: "wsp_00000000000000000000000001".to_owned(),
                cass_session_id: cass_session_id.to_owned(),
                source_path: source_path.map(ToOwned::to_owned),
                agent_name: Some("test-agent".to_owned()),
                model: Some("test-model".to_owned()),
                started_at: Some("2026-05-17T00:00:00Z".to_owned()),
                ended_at: Some("2026-05-17T00:01:00Z".to_owned()),
                message_count: 2,
                token_count: Some(100),
                content_hash:
                    "blake3:1111111111111111111111111111111111111111111111111111111111111111"
                        .to_owned(),
                metadata_json: Some(r#"{"fixture":"why-session"}"#.to_owned()),
            },
        )
        .map_err(|error| error.to_string())
    }

    #[test]
    fn why_session_result_storage_redacts_path_and_secret_source_path() -> TestResult {
        let conn = DbConnection::open_memory().map_err(|error| error.to_string())?;
        conn.migrate().map_err(|error| error.to_string())?;
        conn.insert_workspace(
            "wsp_00000000000000000000000001",
            &CreateWorkspaceInput {
                path: "/tmp/why-session-redaction".to_owned(),
                name: Some("why-session-redaction".to_owned()),
            },
        )
        .map_err(|error| error.to_string())?;

        insert_why_session(
            &conn,
            "sess_00000000000000000000000001",
            "cass-session-a",
            Some(concat!(
                "file:///Volumes/USBNVME16TB/private/cass/session.jsonl?",
                "api",
                "_key=redaction-fixture"
            )),
        )?;

        let report = WhyReport::unsupported_result_target(
            "sess_00000000000000000000000001".to_owned(),
            WhyResultDocumentSource::Session,
            &conn,
        );
        let provenance = report
            .storage
            .as_ref()
            .and_then(|storage| storage.provenance_uri.as_deref())
            .ok_or_else(|| "session provenance present".to_owned())?;

        ensure(
            provenance.contains("/Volumes/USBNVME16TB"),
            false,
            "raw session source path redacted",
        )?;
        ensure(
            provenance.contains("[REDACTED_PATH]"),
            true,
            "path placeholder present",
        )?;
        ensure(
            provenance.contains("redaction-fixture"),
            false,
            "secret value redacted",
        )?;
        ensure(
            provenance.contains("[REDACTED:"),
            true,
            "secret placeholder present",
        )
    }

    #[test]
    fn why_session_result_storage_preserves_safe_cass_fallback() -> TestResult {
        let conn = DbConnection::open_memory().map_err(|error| error.to_string())?;
        conn.migrate().map_err(|error| error.to_string())?;
        conn.insert_workspace(
            "wsp_00000000000000000000000001",
            &CreateWorkspaceInput {
                path: "/tmp/why-session-fallback".to_owned(),
                name: Some("why-session-fallback".to_owned()),
            },
        )
        .map_err(|error| error.to_string())?;

        insert_why_session(
            &conn,
            "sess_00000000000000000000000002",
            "cass-session-b",
            None,
        )?;

        let report = WhyReport::unsupported_result_target(
            "sess_00000000000000000000000002".to_owned(),
            WhyResultDocumentSource::Session,
            &conn,
        );
        let provenance = report
            .storage
            .as_ref()
            .and_then(|storage| storage.provenance_uri.as_deref())
            .ok_or_else(|| "session fallback provenance present".to_owned())?;

        ensure(
            provenance.to_owned(),
            "cass://session/cass-session-b".to_owned(),
            "safe cass fallback preserved",
        )
    }

    #[test]
    fn why_artifact_result_storage_redacts_path_and_secret_provenance() -> TestResult {
        let conn = DbConnection::open_memory().map_err(|error| error.to_string())?;
        conn.migrate().map_err(|error| error.to_string())?;
        conn.insert_workspace(
            "wsp_00000000000000000000000001",
            &CreateWorkspaceInput {
                path: "/tmp/why-artifact-redaction".to_owned(),
                name: Some("why-artifact-redaction".to_owned()),
            },
        )
        .map_err(|error| error.to_string())?;

        insert_why_artifact(
            &conn,
            "art_00000000000000000000000001",
            Some("file:///Users/alice/private/repo/build.log"),
            None,
            None,
        )?;
        insert_why_artifact(
            &conn,
            "art_00000000000000000000000002",
            None,
            None,
            Some("https://example.invalid/logs?api_key=redaction-fixture"),
        )?;

        let path_report = WhyReport::unsupported_result_target(
            "art_00000000000000000000000001".to_owned(),
            WhyResultDocumentSource::Artifact,
            &conn,
        );
        let path_provenance = path_report
            .storage
            .as_ref()
            .and_then(|storage| storage.provenance_uri.as_deref())
            .ok_or_else(|| "path artifact provenance present".to_owned())?;
        ensure(
            path_provenance.contains("/Users/alice"),
            false,
            "raw path redacted",
        )?;
        ensure(
            path_provenance.contains("[REDACTED_PATH]"),
            true,
            "path placeholder present",
        )?;

        let secret_report = WhyReport::unsupported_result_target(
            "art_00000000000000000000000002".to_owned(),
            WhyResultDocumentSource::Artifact,
            &conn,
        );
        let secret_provenance = secret_report
            .storage
            .as_ref()
            .and_then(|storage| storage.provenance_uri.as_deref())
            .ok_or_else(|| "secret artifact provenance present".to_owned())?;
        ensure(
            secret_provenance.contains("api_key"),
            false,
            "secret key name redacted",
        )?;
        ensure(
            secret_provenance.contains("redaction-fixture"),
            false,
            "secret value redacted",
        )?;
        ensure(
            secret_provenance.contains("[REDACTED:"),
            true,
            "secret placeholder present",
        )
    }

    fn insert_why_link_memory(
        conn: &DbConnection,
        workspace_id: &str,
        memory_id: &str,
        content: &str,
    ) -> TestResult {
        conn.insert_memory(
            memory_id,
            &crate::db::CreateMemoryInput {
                workspace_id: workspace_id.to_owned(),
                level: "semantic".to_owned(),
                kind: "fact".to_owned(),
                content: content.to_owned(),
                workflow_id: None,
                confidence: 0.8,
                utility: 0.7,
                importance: 0.6,
                provenance_uri: None,
                trust_class: "agent_assertion".to_owned(),
                trust_subclass: None,
                tags: Vec::new(),
                valid_from: None,
                valid_to: None,
            },
        )
        .map_err(|error| error.to_string())
    }

    fn denied_mesh_link_metadata() -> String {
        serde_json::json!({
            "mesh": {
                "workspaceScopeDecision": "deny",
                "materialLane": "graphSignal",
                "cachedMaterialId": "mesh_why_denied",
                "originWorkspaceId": "wsp_remote_private",
                "originWorkspaceLabel": "/Users/alice/private/repo",
                "producerPeerId": "peer_builder_one",
                "producerPeerLabel": "/Users/alice/private/peer-agent",
                "importDecisionId": "mesh_why_decision_denied",
                "trustLane": "quarantined",
                "redactionPosture": "metadata_only"
            }
        })
        .to_string()
    }

    #[test]
    fn why_fetch_links_ignores_denied_mesh_links() -> TestResult {
        let conn = DbConnection::open_memory().map_err(|error| error.to_string())?;
        conn.migrate().map_err(|error| error.to_string())?;
        let workspace_id = "wsp_00000000000000000000000001";
        let source_memory_id = "mem_00000000000000000000000001";
        let allowed_memory_id = "mem_00000000000000000000000002";
        let denied_memory_id = "mem_00000000000000000000000003";

        conn.insert_workspace(
            workspace_id,
            &crate::db::CreateWorkspaceInput {
                path: "/tmp/why-mesh-filter".to_owned(),
                name: Some("why-mesh-filter".to_owned()),
            },
        )
        .map_err(|error| error.to_string())?;
        insert_why_link_memory(&conn, workspace_id, source_memory_id, "source why memory")?;
        insert_why_link_memory(&conn, workspace_id, allowed_memory_id, "allowed neighbor")?;
        insert_why_link_memory(
            &conn,
            workspace_id,
            denied_memory_id,
            "denied mesh neighbor",
        )?;

        conn.insert_memory_link(
            "link_00000000000000000000000001",
            &crate::db::CreateMemoryLinkInput {
                src_memory_id: source_memory_id.to_owned(),
                dst_memory_id: allowed_memory_id.to_owned(),
                relation: crate::db::MemoryLinkRelation::Supports,
                weight: 1.0,
                confidence: 0.9,
                directed: true,
                evidence_count: 2,
                last_reinforced_at: None,
                source: crate::db::MemoryLinkSource::Agent,
                created_by: Some("why-mesh-filter-test".to_owned()),
                metadata_json: None,
            },
        )
        .map_err(|error| error.to_string())?;
        conn.insert_memory_link(
            "link_00000000000000000000000002",
            &crate::db::CreateMemoryLinkInput {
                src_memory_id: source_memory_id.to_owned(),
                dst_memory_id: denied_memory_id.to_owned(),
                relation: crate::db::MemoryLinkRelation::Supports,
                weight: 1.0,
                confidence: 0.9,
                directed: true,
                evidence_count: 2,
                last_reinforced_at: None,
                source: crate::db::MemoryLinkSource::Import,
                created_by: Some("why-mesh-filter-test".to_owned()),
                metadata_json: Some(denied_mesh_link_metadata()),
            },
        )
        .map_err(|error| error.to_string())?;

        let links = fetch_links(&conn, source_memory_id);
        ensure(
            links.degradation.is_none(),
            true,
            "visible filtering is not a degradation",
        )?;
        ensure(links.items.len(), 1_usize, "visible link count")?;
        ensure(
            links.items[0].linked_memory_id.as_str(),
            allowed_memory_id,
            "allowed link remains",
        )?;
        ensure(
            links
                .items
                .iter()
                .any(|link| link.linked_memory_id == denied_memory_id),
            false,
            "denied mesh link is absent",
        )
    }

    #[test]
    fn why_evidence_fetchers_report_query_failures_as_degradations() -> TestResult {
        let conn = DbConnection::open_memory().map_err(|error| error.to_string())?;

        let contradictions = fetch_contradictions(&conn, "mem_missing_schema");
        ensure(
            contradictions.items.is_empty(),
            true,
            "failed contradiction query has no items",
        )?;
        let contradiction_degradation = contradictions
            .degradation
            .ok_or_else(|| "missing contradiction query degradation".to_string())?;
        ensure(
            contradiction_degradation.code,
            "why_contradictions_unavailable",
            "contradiction degradation code",
        )?;

        let links = fetch_links(&conn, "mem_missing_schema");
        ensure(
            links.items.is_empty(),
            true,
            "failed link query has no items",
        )?;
        let link_degradation = links
            .degradation
            .ok_or_else(|| "missing link query degradation".to_string())?;
        ensure(
            link_degradation.code,
            "why_links_unavailable",
            "link degradation code",
        )?;

        let rationale = fetch_rationale_traces(&conn, "wsp_missing_schema", "mem_missing_schema");
        ensure(
            rationale.items.is_empty(),
            true,
            "failed rationale trace query has no items",
        )?;
        let rationale_degradation = rationale
            .degradation
            .ok_or_else(|| "missing rationale trace query degradation".to_string())?;
        ensure(
            rationale_degradation.code,
            "why_rationale_traces_unavailable",
            "rationale trace degradation code",
        )?;

        ensure(
            contradiction_degradation.severity,
            "medium",
            "contradiction severity",
        )?;
        ensure(link_degradation.severity, "medium", "link severity")?;
        ensure(
            rationale_degradation.severity,
            "medium",
            "rationale severity",
        )
    }

    #[test]
    fn why_evidence_fetchers_keep_true_empty_evidence_undegraded() -> TestResult {
        let conn = DbConnection::open_memory().map_err(|error| error.to_string())?;
        conn.migrate().map_err(|error| error.to_string())?;

        let contradictions = fetch_contradictions(&conn, "mem_no_evidence");
        ensure(
            contradictions.items.is_empty(),
            true,
            "empty contradiction items",
        )?;
        ensure(
            contradictions.degradation.is_none(),
            true,
            "empty contradiction evidence is not degraded",
        )?;

        let links = fetch_links(&conn, "mem_no_evidence");
        ensure(links.items.is_empty(), true, "empty link items")?;
        ensure(
            links.degradation.is_none(),
            true,
            "empty links are not degraded",
        )?;

        let rationale = fetch_rationale_traces(&conn, "wsp_no_evidence", "mem_no_evidence");
        ensure(rationale.items.is_empty(), true, "empty rationale items")?;
        ensure(
            rationale.degradation.is_none(),
            true,
            "empty rationale traces are not degraded",
        )
    }

    #[test]
    fn rationale_trace_summary_uses_only_storable_visible_trace() -> TestResult {
        let trace = RationaleTrace::new(
            "rat_supported_release",
            RationaleTraceKind::Decision,
            "agent:test",
            "Release checklist evidence supports keeping formatting verification in the pack.",
            "2026-05-03T18:50:00Z",
        )
        .map_err(|error| error.to_string())?
        .with_posture(RationaleTracePosture::Supported)
        .with_visibility(RationaleTraceVisibility::Redacted, RedactionStatus::Partial)
        .with_evidence_uri("cass://session#L10-L14")
        .with_memory_id("mem_release_rule")
        .with_context_pack_id("pack_release")
        .with_recorder_run_id("run_release")
        .with_recorder_event_id("event_release")
        .with_causal_trace_id("causal_release");

        let summary = RationaleTraceSummary::from_trace(&trace)
            .ok_or_else(|| "storable rationale trace was filtered".to_string())?;

        ensure(
            summary.trace_id,
            "rat_supported_release".to_string(),
            "trace id",
        )?;
        ensure(summary.kind, "decision".to_string(), "kind")?;
        ensure(summary.posture, "supported".to_string(), "posture")?;
        ensure(summary.visibility, "redacted".to_string(), "visibility")?;
        ensure(
            summary.linked_memory_ids,
            vec!["mem_release_rule".to_string()],
            "linked memory ids",
        )?;
        ensure(
            summary.linked_causal_trace_ids,
            vec!["causal_release".to_string()],
            "linked causal trace ids",
        )
    }

    #[test]
    fn rationale_trace_summary_rejects_private_visibility() -> TestResult {
        let trace = RationaleTrace::new(
            "rat_private_rejected",
            RationaleTraceKind::Hypothesis,
            "agent:test",
            "Visible rejection marker for unexportable material.",
            "2026-05-03T18:51:00Z",
        )
        .map_err(|error| error.to_string())?
        .with_visibility(
            RationaleTraceVisibility::PrivateRejected,
            RedactionStatus::Full,
        );

        ensure(
            RationaleTraceSummary::from_trace(&trace).is_none(),
            true,
            "private rejected rationale traces are not why evidence",
        )
    }

    #[test]
    fn why_report_rationale_traces_are_sorted_and_deduplicated() -> TestResult {
        let report = WhyReport::not_found("mem_test".to_string()).with_rationale_traces(vec![
            RationaleTraceSummary {
                schema: crate::models::RATIONALE_TRACE_SCHEMA_V1,
                trace_id: "rat_b".to_string(),
                kind: "decision".to_string(),
                posture: "supported".to_string(),
                visibility: "public".to_string(),
                author: "agent:test".to_string(),
                summary: "Second trace.".to_string(),
                confidence_basis_points: 7000,
                evidence_uris: Vec::new(),
                linked_memory_ids: Vec::new(),
                linked_context_pack_ids: Vec::new(),
                linked_recorder_run_ids: Vec::new(),
                linked_recorder_event_ids: Vec::new(),
                linked_causal_trace_ids: Vec::new(),
                supersedes_trace_ids: Vec::new(),
                contradicted_by_trace_ids: Vec::new(),
                created_at: "2026-05-03T18:52:00Z".to_string(),
            },
            RationaleTraceSummary {
                schema: crate::models::RATIONALE_TRACE_SCHEMA_V1,
                trace_id: "rat_a".to_string(),
                kind: "hypothesis".to_string(),
                posture: "asserted".to_string(),
                visibility: "public".to_string(),
                author: "agent:test".to_string(),
                summary: "First trace.".to_string(),
                confidence_basis_points: 5000,
                evidence_uris: Vec::new(),
                linked_memory_ids: Vec::new(),
                linked_context_pack_ids: Vec::new(),
                linked_recorder_run_ids: Vec::new(),
                linked_recorder_event_ids: Vec::new(),
                linked_causal_trace_ids: Vec::new(),
                supersedes_trace_ids: Vec::new(),
                contradicted_by_trace_ids: Vec::new(),
                created_at: "2026-05-03T18:51:00Z".to_string(),
            },
            RationaleTraceSummary {
                schema: crate::models::RATIONALE_TRACE_SCHEMA_V1,
                trace_id: "rat_a".to_string(),
                kind: "hypothesis".to_string(),
                posture: "asserted".to_string(),
                visibility: "public".to_string(),
                author: "agent:test".to_string(),
                summary: "Duplicate trace.".to_string(),
                confidence_basis_points: 5000,
                evidence_uris: Vec::new(),
                linked_memory_ids: Vec::new(),
                linked_context_pack_ids: Vec::new(),
                linked_recorder_run_ids: Vec::new(),
                linked_recorder_event_ids: Vec::new(),
                linked_causal_trace_ids: Vec::new(),
                supersedes_trace_ids: Vec::new(),
                contradicted_by_trace_ids: Vec::new(),
                created_at: "2026-05-03T18:51:00Z".to_string(),
            },
            RationaleTraceSummary {
                schema: crate::models::RATIONALE_TRACE_SCHEMA_V1,
                trace_id: "rat_private".to_string(),
                kind: "hypothesis".to_string(),
                posture: "asserted".to_string(),
                visibility: "private_rejected".to_string(),
                author: "agent:test".to_string(),
                summary: "Private rejected material should never render.".to_string(),
                confidence_basis_points: 5000,
                evidence_uris: Vec::new(),
                linked_memory_ids: Vec::new(),
                linked_context_pack_ids: Vec::new(),
                linked_recorder_run_ids: Vec::new(),
                linked_recorder_event_ids: Vec::new(),
                linked_causal_trace_ids: Vec::new(),
                supersedes_trace_ids: Vec::new(),
                contradicted_by_trace_ids: Vec::new(),
                created_at: "2026-05-03T18:53:00Z".to_string(),
            },
        ]);

        ensure(report.rationale_traces.len(), 2, "rationale trace count")?;
        ensure(
            report
                .rationale_traces
                .iter()
                .any(|trace| trace.visibility.as_str().eq("private_rejected")),
            false,
            "private rejected summaries are filtered at report boundary",
        )?;
        ensure(
            report.rationale_traces[0].trace_id.clone(),
            "rat_a".to_string(),
            "first trace id",
        )?;
        ensure(
            report.rationale_traces[1].trace_id.clone(),
            "rat_b".to_string(),
            "second trace id",
        )
    }

    #[test]
    fn all_link_relation_types_supported() -> TestResult {
        let relations = [
            "supports",
            "contradicts",
            "derived_from",
            "supersedes",
            "related",
            "co_tag",
            "co_mention",
        ];

        for relation in &relations {
            let link = MemoryLinkSummary {
                link_id: format!("link_{relation}"),
                linked_memory_id: "mem_other".to_string(),
                relation: relation.to_string(),
                direction: "outgoing".to_string(),
                confidence: 0.9,
                weight: 1.0,
                evidence_count: 1,
                source: "agent".to_string(),
                created_at: "2026-04-30T12:00:00Z".to_string(),
            };
            ensure(link.relation, relation.to_string(), "relation type")?;
        }
        Ok(())
    }

    #[test]
    fn all_link_sources_supported() -> TestResult {
        let sources = ["agent", "auto", "import", "maintenance", "human"];

        for source in &sources {
            let link = MemoryLinkSummary {
                link_id: format!("link_{source}"),
                linked_memory_id: "mem_other".to_string(),
                relation: "supports".to_string(),
                direction: "outgoing".to_string(),
                confidence: 0.9,
                weight: 1.0,
                evidence_count: 1,
                source: source.to_string(),
                created_at: "2026-04-30T12:00:00Z".to_string(),
            };
            ensure(link.source, source.to_string(), "source type")?;
        }
        Ok(())
    }
}
