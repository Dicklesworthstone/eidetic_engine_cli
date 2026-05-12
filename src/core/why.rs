//! Memory selection explanation (EE-150).
//!
//! Provides the `ee why <memory-id>` command which explains:
//! - How a memory was stored (provenance, trust class)
//! - How it would be retrieved (scoring factors)
//! - How it would be selected for packs (relevance, utility, importance)
//! - Related memory links (supports, contradicts, derived_from, etc.)
//!
//! This makes the system explainable and auditable.

use std::path::{Path, PathBuf};

use crate::core::memory::{
    EvidenceFreshness, EvidenceFreshnessStatus, assess_memory_evidence_freshness, memory_validity,
};
use crate::db::DbConnection;
use crate::models::{RationaleTrace, RationaleTraceVisibility};
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
    /// Contradiction feedback recorded against this memory (EE-263).
    pub contradictions: Vec<ContradictionMetadata>,
    /// Memory links: supports, contradicts, derived_from, etc. (EE-LINK-USAGE-001).
    pub links: Vec<MemoryLinkSummary>,
    /// Audit history timeline for the memory.
    pub history: Option<MemoryHistorySummary>,
    /// Safe visible rationale traces linked to this memory or latest pack.
    pub rationale_traces: Vec<RationaleTraceSummary>,
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
            contradictions: Vec::new(),
            links: Vec::new(),
            history: None,
            rationale_traces: Vec::new(),
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
            contradictions: Vec::new(),
            links: Vec::new(),
            history: None,
            rationale_traces: Vec::new(),
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
            contradictions: Vec::new(),
            links: Vec::new(),
            history: None,
            rationale_traces: Vec::new(),
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

    /// Add graph-derived retrieval feature explanation to the report.
    #[must_use]
    pub fn with_graph_retrieval(mut self, graph_retrieval: GraphRetrievalExplanation) -> Self {
        self.graph_retrieval = Some(graph_retrieval);
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

/// Get a why explanation for a memory.
///
/// Explains why a memory was stored, how it would be retrieved,
/// and how it would be selected for context packs.
pub fn explain_memory(options: &WhyOptions<'_>) -> WhyReport {
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
    let workspace_path = workspace_path_for_memory(&conn, &memory.workspace_id);
    let freshness = assess_memory_evidence_freshness(&memory, workspace_path.as_deref());
    if let Some(degradation) = why_evidence_freshness_degradation(memory_id, &freshness) {
        evidence_degradations.push(degradation);
    }
    let contradictions = contradiction_fetch.items;
    let links = link_fetch.items;
    let history = history_fetch.items.into_iter().next();
    let rationale_traces = rationale_trace_fetch.items;

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
        provenance_uri: memory.provenance_uri.clone(),
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
                    contradictions,
                    links,
                    history,
                    rationale_traces,
                    graph_retrieval,
                    degraded: evidence_degradations,
                },
            )
            .with_content(memory.content.clone());
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
            contradictions,
            links,
            history,
            rationale_traces,
            graph_retrieval,
            degraded: evidence_degradations,
        },
    )
    .with_content(memory.content.clone());

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
    let _ = conn.insert_audit(&crate::db::generate_audit_id(), &audit_input);

    report
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
                        .or_else(|| Some(format!("cass://session/{}", session.cass_session_id))),
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
                        .or(artifact.external_ref),
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
    contradictions: Vec<ContradictionMetadata>,
    links: Vec<MemoryLinkSummary>,
    history: Option<MemoryHistorySummary>,
    rationale_traces: Vec<RationaleTraceSummary>,
    graph_retrieval: GraphRetrievalExplanation,
    degraded: Vec<WhyDegradation>,
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
        .with_contradictions(selection_inputs.contradictions)
        .with_links(selection_inputs.links)
        .with_optional_history(selection_inputs.history)
        .with_graph_retrieval(selection_inputs.graph_retrieval)
        .with_rationale_traces(selection_inputs.rationale_traces)
        .with_degradations(selection_inputs.degraded)
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

    let total_count = stored_entries.len() as u32;
    let truncated = stored_entries.len() > WHY_HISTORY_LIMIT;
    let entries = stored_entries
        .into_iter()
        .take(WHY_HISTORY_LIMIT)
        .map(|entry| MemoryHistorySummaryEntry {
            audit_id: entry.id,
            timestamp: entry.timestamp,
            actor: entry.actor,
            action: entry.action,
            details: entry.details,
        })
        .collect();

    WhyEvidenceFetch::available(vec![MemoryHistorySummary {
        entries,
        total_count,
        truncated,
    }])
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
                contradictions: Vec::new(),
                links: Vec::new(),
                history: None,
                rationale_traces: Vec::new(),
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
