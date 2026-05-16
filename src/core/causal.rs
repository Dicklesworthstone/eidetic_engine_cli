//! Causal trace and credit analysis (EE-451).
//!
//! Traces causal chains over recorder runs, context pack records, preflight
//! closes, tripwire checks, and procedure uses to distinguish exposure from
//! influence.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Value as JsonValue, json};
use sqlmodel_core::{Row, Value};

use crate::core::degraded_aggregation::{DegradationAggregationInput, aggregate_degraded_entries};
use crate::db::{CreateCurationCandidateInput, DbConnection};
use crate::models::causal::{
    CAUSAL_TRACE_SCHEMA_V1, CausalDecisionTrace, CausalEvidenceMethod, CausalEvidenceStrength,
    CausalExposureChannel, PromotionAction, PromotionPlan, PromotionPlanStatus,
};
use crate::models::{CandidateId, DomainError, TraceId, WorkspaceId};

/// Schema for causal trace list response.
pub const CAUSAL_TRACE_LIST_SCHEMA_V1: &str = "ee.causal.trace_list.v1";

// ============================================================================
// Trace Options and Report
// ============================================================================

/// Options for tracing causal chains.
#[derive(Clone, Debug)]
pub struct TraceOptions {
    /// Filter by memory ID.
    pub memory_id: Option<String>,
    /// Filter by recorder run ID.
    pub run_id: Option<String>,
    /// Filter by context pack ID.
    pub pack_id: Option<String>,
    /// Filter by preflight ID.
    pub preflight_id: Option<String>,
    /// Filter by tripwire ID.
    pub tripwire_id: Option<String>,
    /// Filter by procedure ID.
    pub procedure_id: Option<String>,
    /// Filter by agent ID.
    pub agent_id: Option<String>,
    /// Filter by workspace ID.
    pub workspace_id: Option<String>,
    /// Explicit database path for direct helper calls that need persisted ledger rows.
    pub database_path: Option<PathBuf>,
    /// Maximum number of traces to return.
    pub limit: Option<usize>,
    /// Maximum number of causal edges to walk backward from the failure.
    pub depth: usize,
    /// Include detailed exposure records.
    pub include_exposures: bool,
    /// Include outcome summaries.
    pub include_outcomes: bool,
    /// Dry-run mode (show what would be traced).
    pub dry_run: bool,
}

impl Default for TraceOptions {
    fn default() -> Self {
        Self {
            memory_id: None,
            run_id: None,
            pack_id: None,
            preflight_id: None,
            tripwire_id: None,
            procedure_id: None,
            agent_id: None,
            workspace_id: None,
            database_path: None,
            limit: None,
            depth: 8,
            include_exposures: false,
            include_outcomes: false,
            dry_run: false,
        }
    }
}

impl TraceOptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_memory_id(mut self, id: impl Into<String>) -> Self {
        self.memory_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_run_id(mut self, id: impl Into<String>) -> Self {
        self.run_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_pack_id(mut self, id: impl Into<String>) -> Self {
        self.pack_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_preflight_id(mut self, id: impl Into<String>) -> Self {
        self.preflight_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_tripwire_id(mut self, id: impl Into<String>) -> Self {
        self.tripwire_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_procedure_id(mut self, id: impl Into<String>) -> Self {
        self.procedure_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_agent_id(mut self, id: impl Into<String>) -> Self {
        self.agent_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_workspace_id(mut self, id: impl Into<String>) -> Self {
        self.workspace_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_database_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.database_path = Some(path.into());
        self
    }

    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    #[must_use]
    pub fn with_depth(mut self, depth: usize) -> Self {
        self.depth = depth.max(1);
        self
    }

    #[must_use]
    pub fn with_exposures(mut self) -> Self {
        self.include_exposures = true;
        self
    }

    #[must_use]
    pub fn with_outcomes(mut self) -> Self {
        self.include_outcomes = true;
        self
    }

    #[must_use]
    pub fn dry_run(mut self) -> Self {
        self.dry_run = true;
        self
    }

    fn has_any_filter(&self) -> bool {
        self.memory_id.is_some()
            || self.run_id.is_some()
            || self.pack_id.is_some()
            || self.preflight_id.is_some()
            || self.tripwire_id.is_some()
            || self.procedure_id.is_some()
            || self.agent_id.is_some()
            || self.workspace_id.is_some()
    }
}

/// A single exposure event in the causal chain.
#[derive(Clone, Debug)]
pub struct CausalExposure {
    pub exposure_id: String,
    pub channel: CausalExposureChannel,
    pub artifact_id: String,
    pub artifact_type: String,
    pub exposed_at: String,
    pub context_pack_id: Option<String>,
    pub recorder_run_id: Option<String>,
}

impl CausalExposure {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        let mut obj = json!({
            "exposureId": self.exposure_id,
            "channel": self.channel.as_str(),
            "artifactId": self.artifact_id,
            "artifactType": self.artifact_type,
            "exposedAt": self.exposed_at,
        });
        if let Some(obj_map) = obj.as_object_mut() {
            if let Some(ref pack_id) = self.context_pack_id {
                obj_map.insert("contextPackId".to_string(), json!(pack_id));
            }
            if let Some(ref run_id) = self.recorder_run_id {
                obj_map.insert("recorderRunId".to_string(), json!(run_id));
            }
        }
        obj
    }
}

/// One persisted causal evidence edge from an observed effect to a candidate cause.
#[derive(Clone, Debug, PartialEq)]
pub struct CausalLedgerEdge {
    pub edge_id: String,
    pub failure_id: String,
    pub candidate_cause_id: String,
    pub contribution_score: f64,
    pub evidence_uris: Vec<String>,
    pub computed_at: String,
    pub method: CausalEvidenceMethod,
}

impl CausalLedgerEdge {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "edgeId": self.edge_id,
            "failureId": self.failure_id,
            "candidateCauseId": self.candidate_cause_id,
            "contributionScore": rounded_causal_metric(self.contribution_score),
            "evidenceUris": self.evidence_uris,
            "computedAt": self.computed_at,
            "method": self.method.as_str(),
        })
    }
}

/// One node in a traced causal chain.
#[derive(Clone, Debug, PartialEq)]
pub struct CausalTraceNode {
    pub node_id: String,
    pub role: String,
    pub depth: usize,
    pub memory_level: Option<String>,
    pub memory_kind: Option<String>,
}

impl CausalTraceNode {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "nodeId": self.node_id,
            "role": self.role,
            "depth": self.depth,
            "memoryLevel": self.memory_level,
            "memoryKind": self.memory_kind,
        })
    }
}

/// A traced causal chain linking exposures to outcomes.
#[derive(Clone, Debug)]
pub struct CausalChain {
    pub chain_id: String,
    pub failure_id: Option<String>,
    pub root_cause_id: Option<String>,
    pub nodes: Vec<CausalTraceNode>,
    pub edges: Vec<CausalLedgerEdge>,
    pub contribution_estimate: f64,
    pub evidence_uris: Vec<String>,
    pub decision_trace: CausalDecisionTrace,
    pub exposures: Vec<CausalExposure>,
    pub recorder_run_ids: Vec<String>,
    pub context_pack_ids: Vec<String>,
    pub preflight_ids: Vec<String>,
    pub tripwire_ids: Vec<String>,
    pub procedure_ids: Vec<String>,
}

impl CausalChain {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "chainId": self.chain_id,
            "failureId": self.failure_id,
            "rootCauseId": self.root_cause_id,
            "nodes": self.nodes.iter().map(CausalTraceNode::data_json).collect::<Vec<_>>(),
            "edges": self.edges.iter().map(CausalLedgerEdge::data_json).collect::<Vec<_>>(),
            "nodeCount": self.nodes.len(),
            "edgeCount": self.edges.len(),
            "contributionEstimate": rounded_causal_metric(self.contribution_estimate),
            "evidenceUris": self.evidence_uris,
            "decisionTrace": self.decision_trace.data_json(),
            "exposures": self.exposures.iter().map(CausalExposure::data_json).collect::<Vec<_>>(),
            "recorderRunIds": self.recorder_run_ids,
            "contextPackIds": self.context_pack_ids,
            "preflightIds": self.preflight_ids,
            "tripwireIds": self.tripwire_ids,
            "procedureIds": self.procedure_ids,
        })
    }

    #[must_use]
    pub fn exposure_count(&self) -> usize {
        self.exposures.len().max(self.edges.len())
    }

    #[must_use]
    pub fn total_artifact_count(&self) -> usize {
        self.recorder_run_ids.len()
            + self.context_pack_ids.len()
            + self.preflight_ids.len()
            + self.tripwire_ids.len()
            + self.procedure_ids.len()
    }
}

/// Degradation in trace results.
#[derive(Clone, Debug)]
pub struct TraceDegradation {
    pub code: String,
    pub message: String,
    pub severity: String,
    pub repair: Option<String>,
}

impl std::fmt::Display for TraceDegradation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.severity)
    }
}

fn trace_degradation(
    code: impl Into<String>,
    message: impl Into<String>,
    severity: impl Into<String>,
) -> TraceDegradation {
    TraceDegradation {
        code: code.into(),
        message: message.into(),
        severity: severity.into(),
        repair: None,
    }
}

fn trace_degradation_with_repair(
    code: impl Into<String>,
    message: impl Into<String>,
    severity: impl Into<String>,
    repair: impl Into<String>,
) -> TraceDegradation {
    TraceDegradation {
        code: code.into(),
        message: message.into(),
        severity: severity.into(),
        repair: Some(repair.into()),
    }
}

fn causal_degradations_data_json(
    source: &'static str,
    degradations: &[TraceDegradation],
) -> Vec<JsonValue> {
    aggregate_degraded_entries(degradations.iter().map(|entry| {
        DegradationAggregationInput::new(
            source,
            entry.code.clone(),
            entry.severity.clone(),
            entry.message.clone(),
            entry.repair.clone().unwrap_or_default(),
        )
    }))
    .into_iter()
    .map(|entry| {
        let mut value = json!({
            "code": entry.code,
            "message": entry.message,
            "severity": entry.severity,
            "sources": entry.sources,
        });
        if !entry.repair.is_empty()
            && let Some(object) = value.as_object_mut()
        {
            object.insert("repair".to_string(), json!(entry.repair));
        }
        value
    })
    .collect()
}

fn rounded_causal_metric(value: f64) -> f64 {
    if value.is_finite() {
        (value.clamp(-1.0, 1.0) * 1000.0).round() / 1000.0
    } else {
        0.0
    }
}

fn causal_evidence_unavailable(scope: &str) -> TraceDegradation {
    trace_degradation(
        "causal_evidence_unavailable",
        format!(
            "No persisted causal evidence ledger rows are available for {scope}; refusing to infer exposures, decisions, outcomes, or uplift from IDs."
        ),
        "warning",
    )
}

fn causal_sample_underpowered(scope: &str) -> TraceDegradation {
    trace_degradation(
        "causal_sample_underpowered",
        format!(
            "{scope} has sample size 0 with no observed baseline/outcome ledger; no causal effect is actionable."
        ),
        "warning",
    )
}

/// Report from tracing causal chains.
#[derive(Clone, Debug)]
pub struct TraceReport {
    pub schema: &'static str,
    pub chains: Vec<CausalChain>,
    pub total_exposures: usize,
    pub total_decisions: usize,
    pub filters_applied: Vec<String>,
    pub degradations: Vec<TraceDegradation>,
    pub dry_run: bool,
}

impl TraceReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "command": "causal trace",
            "chains": self.chains.iter().map(CausalChain::data_json).collect::<Vec<_>>(),
            "summary": {
                "totalChains": self.chains.len(),
                "totalExposures": self.total_exposures,
                "totalDecisions": self.total_decisions,
            },
            "filtersApplied": self.filters_applied,
            "degradations": causal_degradations_data_json("causal_trace", &self.degradations),
            "dryRun": self.dry_run,
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(1024);
        if self.dry_run {
            out.push_str("Causal Trace [DRY RUN]\n");
        } else {
            out.push_str("Causal Trace Report\n");
        }
        out.push_str("===================\n\n");

        out.push_str(&format!("Chains found:     {}\n", self.chains.len()));
        out.push_str(&format!("Total exposures:  {}\n", self.total_exposures));
        out.push_str(&format!("Total decisions:  {}\n", self.total_decisions));

        if !self.filters_applied.is_empty() {
            out.push_str("\nFilters applied:\n");
            for filter in &self.filters_applied {
                out.push_str(&format!("  - {filter}\n"));
            }
        }

        if !self.degradations.is_empty() {
            out.push_str("\nDegradations:\n");
            for deg in &self.degradations {
                out.push_str(&format!(
                    "  - [{}] {}: {}\n",
                    deg.severity, deg.code, deg.message
                ));
            }
        }

        if !self.chains.is_empty() {
            out.push_str("\nChains:\n");
            for (i, chain) in self.chains.iter().enumerate() {
                out.push_str(&format!(
                    "  {}. {} ({} exposures, outcome: {})\n",
                    i + 1,
                    chain.chain_id,
                    chain.exposure_count(),
                    chain.decision_trace.outcome.as_str()
                ));
            }
        }

        out
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chains.is_empty()
    }

    #[must_use]
    pub fn has_degradations(&self) -> bool {
        !self.degradations.is_empty()
    }
}

// ============================================================================
// Trace Function
// ============================================================================

/// Trace causal chains based on the provided options.
///
/// Filter-only calls return an empty report with explicit evidence status.
/// Calls that provide a database path, workspace ID, and failure memory ID read
/// persisted `causal_evidence` rows and build deterministic causal paths.
#[must_use]
pub fn trace_causal_chains(options: &TraceOptions) -> TraceReport {
    if options.database_path.is_some() {
        return trace_causal_chains_from_options_store(options);
    }

    base_trace_report(options)
}

fn base_trace_report(options: &TraceOptions) -> TraceReport {
    let mut filters_applied = Vec::new();
    let mut degradations = Vec::new();

    if let Some(ref memory_id) = options.memory_id {
        filters_applied.push(format!("memory_id={memory_id}"));
    }
    if let Some(ref run_id) = options.run_id {
        filters_applied.push(format!("run_id={run_id}"));
    }
    if let Some(ref pack_id) = options.pack_id {
        filters_applied.push(format!("pack_id={pack_id}"));
    }
    if let Some(ref preflight_id) = options.preflight_id {
        filters_applied.push(format!("preflight_id={preflight_id}"));
    }
    if let Some(ref tripwire_id) = options.tripwire_id {
        filters_applied.push(format!("tripwire_id={tripwire_id}"));
    }
    if let Some(ref procedure_id) = options.procedure_id {
        filters_applied.push(format!("procedure_id={procedure_id}"));
    }
    if let Some(ref agent_id) = options.agent_id {
        filters_applied.push(format!("agent_id={agent_id}"));
    }
    if let Some(ref workspace_id) = options.workspace_id {
        filters_applied.push(format!("workspace_id={workspace_id}"));
    }
    if let Some(limit) = options.limit {
        filters_applied.push(format!("limit={limit}"));
    }
    filters_applied.push(format!("depth={}", options.depth.max(1)));

    if !options.has_any_filter() {
        degradations.push(trace_degradation(
            "no_filters",
            "No filters provided; returning empty trace.",
            "info",
        ));
    } else if !options.dry_run {
        degradations.push(causal_evidence_unavailable("causal trace"));
    }

    let chains: Vec<CausalChain> = Vec::new();

    let total_exposures: usize = chains.iter().map(|c| c.exposure_count()).sum();
    let total_decisions = chains.len();

    TraceReport {
        schema: CAUSAL_TRACE_SCHEMA_V1,
        chains,
        total_exposures,
        total_decisions,
        filters_applied,
        degradations,
        dry_run: options.dry_run,
    }
}

fn trace_causal_chains_from_options_store(options: &TraceOptions) -> TraceReport {
    let mut report = base_trace_report(options);
    report.degradations.retain(|degradation| {
        degradation.code != "causal_evidence_unavailable" && degradation.code != "no_filters"
    });

    if options.dry_run {
        return report;
    }

    let Some(database_path) = options.database_path.as_deref() else {
        return report;
    };

    let Some(workspace_id) = options.workspace_id.as_deref() else {
        report.degradations.push(trace_degradation(
            "causal_workspace_id_required",
            "Provide a workspace ID when reading causal trace rows through the direct helper.",
            "warning",
        ));
        return report;
    };

    if !database_path.exists() {
        report.degradations.push(trace_degradation(
            "causal_database_missing",
            format!(
                "Causal trace database does not exist at {}.",
                database_path.display()
            ),
            "warning",
        ));
        return report;
    }

    let conn = match DbConnection::open_file(database_path) {
        Ok(conn) => conn,
        Err(error) => {
            report.degradations.push(trace_degradation(
                "causal_database_open_failed",
                format!("Failed to open causal trace database: {error}"),
                "warning",
            ));
            return report;
        }
    };

    if let Err(error) = conn.migrate() {
        report.degradations.push(trace_degradation(
            "causal_database_migration_failed",
            format!("Failed to migrate causal trace database: {error}"),
            "warning",
        ));
        return report;
    }

    match trace_causal_chains_from_store(&conn, workspace_id, options) {
        Ok(store_report) => store_report,
        Err(error) => {
            report.degradations.push(trace_degradation(
                "causal_trace_store_failed",
                format!("Failed to read causal trace rows: {}", error.message()),
                "warning",
            ));
            report
        }
    }
}

/// Trace causal chains from persisted `causal_evidence` ledger rows.
pub fn trace_causal_chains_from_store(
    conn: &DbConnection,
    workspace_id: &str,
    options: &TraceOptions,
) -> Result<TraceReport, DomainError> {
    let mut report = base_trace_report(options);
    report.degradations.retain(|degradation| {
        degradation.code != "causal_evidence_unavailable" && degradation.code != "no_filters"
    });

    let Some(failure_id) = options.memory_id.as_deref() else {
        report.degradations.push(trace_degradation(
            "causal_failure_id_required",
            "Provide a failure memory ID with `ee causal trace <failure-mem-id>` or `--memory-id`.",
            "warning",
        ));
        return Ok(report);
    };

    if options.dry_run {
        return Ok(report);
    }

    let (edges, mut degradations) = load_causal_ledger_edges(conn, workspace_id)?;
    report.degradations.append(&mut degradations);
    if edges.is_empty() {
        if report.degradations.is_empty() {
            report
                .degradations
                .push(causal_evidence_unavailable("causal trace"));
        }
        return Ok(report);
    }

    let chains = build_causal_chains(conn, workspace_id, failure_id, &edges, options)?;
    report.total_exposures = chains.iter().map(CausalChain::exposure_count).sum();
    report.total_decisions = chains.len();
    report.chains = chains;
    Ok(report)
}

fn build_causal_chains(
    conn: &DbConnection,
    workspace_id: &str,
    failure_id: &str,
    edges: &[CausalLedgerEdge],
    options: &TraceOptions,
) -> Result<Vec<CausalChain>, DomainError> {
    let mut by_failure: BTreeMap<&str, Vec<&CausalLedgerEdge>> = BTreeMap::new();
    for edge in edges {
        by_failure
            .entry(edge.failure_id.as_str())
            .or_default()
            .push(edge);
    }
    for bucket in by_failure.values_mut() {
        bucket.sort_by(|left, right| {
            left.candidate_cause_id
                .cmp(&right.candidate_cause_id)
                .then_with(|| left.edge_id.cmp(&right.edge_id))
        });
    }

    let mut paths = Vec::new();
    let mut visited = BTreeSet::from([failure_id.to_owned()]);
    collect_causal_paths(
        failure_id,
        &by_failure,
        options.depth.max(1),
        &mut visited,
        &mut Vec::new(),
        &mut paths,
    )?;

    let limit = options.limit.unwrap_or(usize::MAX);
    paths
        .into_iter()
        .take(limit)
        .map(|path| chain_from_path(conn, workspace_id, failure_id, &path))
        .collect()
}

fn collect_causal_paths<'a>(
    node_id: &str,
    by_failure: &BTreeMap<&'a str, Vec<&'a CausalLedgerEdge>>,
    remaining_depth: usize,
    visited: &mut BTreeSet<String>,
    current: &mut Vec<CausalLedgerEdge>,
    paths: &mut Vec<Vec<CausalLedgerEdge>>,
) -> Result<(), DomainError> {
    if remaining_depth == 0 {
        if !current.is_empty() {
            paths.push(current.clone());
        }
        return Ok(());
    }

    let Some(next_edges) = by_failure.get(node_id) else {
        if !current.is_empty() {
            paths.push(current.clone());
        }
        return Ok(());
    };

    for edge in next_edges {
        if visited.contains(&edge.candidate_cause_id) {
            return Err(DomainError::Graph {
                message: format!(
                    "Causal evidence cycle rejected: `{}` already appears in the active chain.",
                    edge.candidate_cause_id
                ),
                repair: Some(
                    "Remove or correct the cyclic causal_evidence row before tracing.".to_owned(),
                ),
            });
        }

        visited.insert(edge.candidate_cause_id.clone());
        current.push((*edge).clone());
        collect_causal_paths(
            &edge.candidate_cause_id,
            by_failure,
            remaining_depth.saturating_sub(1),
            visited,
            current,
            paths,
        )?;
        current.pop();
        visited.remove(&edge.candidate_cause_id);
    }

    Ok(())
}

fn chain_from_path(
    conn: &DbConnection,
    workspace_id: &str,
    failure_id: &str,
    path: &[CausalLedgerEdge],
) -> Result<CausalChain, DomainError> {
    let mut node_ids = Vec::with_capacity(path.len() + 1);
    node_ids.push(failure_id.to_owned());
    node_ids.extend(path.iter().map(|edge| edge.candidate_cause_id.clone()));

    let root_cause_id = node_ids.last().cloned();
    let chain_id = deterministic_chain_id(workspace_id, &node_ids, path);
    let nodes = node_ids
        .iter()
        .enumerate()
        .map(|(depth, node_id)| trace_node_from_memory(conn, node_id, depth, node_ids.len()))
        .collect::<Result<Vec<_>, _>>()?;
    let evidence_uris = unique_evidence_uris(path);
    let contribution_estimate = path_contribution_estimate(path);
    let created_at = path
        .iter()
        .map(|edge| edge.computed_at.as_str())
        .max()
        .unwrap_or("1970-01-01T00:00:00Z")
        .to_owned();

    Ok(CausalChain {
        chain_id,
        failure_id: Some(failure_id.to_owned()),
        root_cause_id,
        nodes,
        edges: path.to_vec(),
        contribution_estimate,
        evidence_uris,
        decision_trace: CausalDecisionTrace::new(
            failure_id,
            "causal-ledger",
            crate::models::decision::DecisionPlane::Observe,
            created_at,
            "causal_evidence",
            "Causal trace was reconstructed from persisted causal_evidence ledger rows.",
        ),
        exposures: Vec::new(),
        recorder_run_ids: Vec::new(),
        context_pack_ids: Vec::new(),
        preflight_ids: Vec::new(),
        tripwire_ids: Vec::new(),
        procedure_ids: Vec::new(),
    })
}

fn trace_node_from_memory(
    conn: &DbConnection,
    node_id: &str,
    depth: usize,
    node_count: usize,
) -> Result<CausalTraceNode, DomainError> {
    let stored = conn
        .get_memory(node_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to read causal trace memory `{node_id}`: {error}"),
            repair: Some("ee doctor --json".to_owned()),
        })?;
    let role = if depth == 0 {
        "failure"
    } else if depth + 1 == node_count {
        "root_cause"
    } else {
        "intermediate"
    };
    Ok(CausalTraceNode {
        node_id: node_id.to_owned(),
        role: role.to_owned(),
        depth,
        memory_level: stored.as_ref().map(|memory| memory.level.clone()),
        memory_kind: stored.as_ref().map(|memory| memory.kind.clone()),
    })
}

fn load_causal_ledger_edges(
    conn: &DbConnection,
    workspace_id: &str,
) -> Result<(Vec<CausalLedgerEdge>, Vec<TraceDegradation>), DomainError> {
    let rows = match conn.query(
        "SELECT id, failure_id, candidate_cause_id, contribution_score, evidence_uris_json, computed_at, method
         FROM causal_evidence
         WHERE workspace_id = ?1
         ORDER BY failure_id ASC, candidate_cause_id ASC, computed_at ASC, id ASC",
        &[Value::Text(workspace_id.to_owned())],
    ) {
        Ok(rows) => rows,
        Err(error) if db_error_mentions_missing_causal_table(&error) => {
            return Ok((
                Vec::new(),
                vec![trace_degradation_with_repair(
                    "causal_evidence_table_missing",
                    "The causal_evidence table is missing; run `ee init --workspace .` with the current binary or migrate the database.",
                    "warning",
                    "ee init --workspace .",
                )],
            ));
        }
        Err(error) => {
            return Err(DomainError::Storage {
                message: format!("Failed to query causal evidence ledger: {error}"),
                repair: Some("ee doctor --json".to_owned()),
            });
        }
    };

    rows.iter()
        .map(causal_ledger_edge_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map(|edges| (edges, Vec::new()))
}

fn db_error_mentions_missing_causal_table(error: &crate::db::DbError) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("no such table") && message.contains("causal_evidence")
}

fn causal_ledger_edge_from_row(row: &Row) -> Result<CausalLedgerEdge, DomainError> {
    let evidence_uris_json = row_text(row, 4, "causal_evidence.evidence_uris_json")?;
    let evidence_uris =
        serde_json::from_str::<Vec<String>>(&evidence_uris_json).map_err(|error| {
            DomainError::Storage {
                message: format!("causal_evidence.evidence_uris_json is invalid: {error}"),
                repair: Some("Repair the causal_evidence row and retry.".to_owned()),
            }
        })?;
    let method_raw = row_text(row, 6, "causal_evidence.method")?;
    let method = method_raw
        .parse::<CausalEvidenceMethod>()
        .map_err(|error| DomainError::Storage {
            message: error.to_string(),
            repair: Some("Use method manual, graph-inferred, or cass-derived.".to_owned()),
        })?;

    Ok(CausalLedgerEdge {
        edge_id: row_text(row, 0, "causal_evidence.id")?,
        failure_id: row_text(row, 1, "causal_evidence.failure_id")?,
        candidate_cause_id: row_text(row, 2, "causal_evidence.candidate_cause_id")?,
        contribution_score: row_f64(row, 3, "causal_evidence.contribution_score")?.clamp(0.0, 1.0),
        evidence_uris,
        computed_at: row_text(row, 5, "causal_evidence.computed_at")?,
        method,
    })
}

fn row_text(row: &Row, index: usize, column: &str) -> Result<String, DomainError> {
    row.get(index)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| DomainError::Storage {
            message: format!("{column} column at index {index} is not text"),
            repair: Some("Repair the causal_evidence row and retry.".to_owned()),
        })
}

fn row_f64(row: &Row, index: usize, column: &str) -> Result<f64, DomainError> {
    row.get(index)
        .and_then(Value::as_f64)
        .ok_or_else(|| DomainError::Storage {
            message: format!("{column} column at index {index} is not numeric"),
            repair: Some("Repair the causal_evidence row and retry.".to_owned()),
        })
}

fn unique_evidence_uris(path: &[CausalLedgerEdge]) -> Vec<String> {
    path.iter()
        .flat_map(|edge| edge.evidence_uris.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn path_contribution_estimate(path: &[CausalLedgerEdge]) -> f64 {
    if path.is_empty() {
        return 0.0;
    }
    rounded_causal_metric(
        path.iter()
            .map(|edge| edge.contribution_score.clamp(0.0, 1.0))
            .product::<f64>(),
    )
}

fn deterministic_chain_id(
    workspace_id: &str,
    node_ids: &[String],
    path: &[CausalLedgerEdge],
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(workspace_id.as_bytes());
    hasher.update(b"\0nodes\0");
    for node_id in node_ids {
        hasher.update(node_id.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(b"\0edges\0");
    for edge in path {
        hasher.update(edge.edge_id.as_bytes());
        hasher.update(b"\0");
    }
    let hash = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    TraceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

fn causal_evidence_strength(path: &[CausalLedgerEdge]) -> CausalEvidenceStrength {
    if path.is_empty() || path.iter().any(|edge| edge.evidence_uris.is_empty()) {
        return CausalEvidenceStrength::ExposureOnly;
    }
    if path
        .iter()
        .any(|edge| edge.method == CausalEvidenceMethod::GraphInferred)
    {
        return CausalEvidenceStrength::ReplaySupported;
    }
    if path
        .iter()
        .any(|edge| edge.method == CausalEvidenceMethod::CassDerived)
    {
        return CausalEvidenceStrength::Correlational;
    }
    CausalEvidenceStrength::Correlational
}

pub(crate) fn stable_workspace_id(path: &Path) -> String {
    let hash = blake3::hash(format!("workspace:{}", path.to_string_lossy()).as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    WorkspaceId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string()
}

// ============================================================================
// Estimate Options and Report (EE-452)
// ============================================================================

/// Schema for causal estimate response.
pub const CAUSAL_ESTIMATE_SCHEMA_V1: &str = "ee.causal.estimate.v1";

/// Schema for causal comparison response.
pub const CAUSAL_COMPARE_SCHEMA_V1: &str = "ee.causal.compare.v1";

/// Schema for causal promotion plan response.
pub const CAUSAL_PROMOTE_PLAN_SCHEMA_V1: &str = "ee.causal.promote_plan.v1";

/// Schema for causal downstream projection effects.
pub const CAUSAL_DOWNSTREAM_EFFECTS_SCHEMA_V1: &str = "ee.causal.downstream_effects.v1";

/// Options for computing causal estimates.
#[derive(Clone, Debug, Default)]
pub struct EstimateOptions {
    /// Artifact ID to estimate uplift for.
    pub artifact_id: Option<String>,
    /// Decision ID to scope estimation.
    pub decision_id: Option<String>,
    /// Causal chain ID to base estimate on.
    pub chain_id: Option<String>,
    /// Agent ID to filter by.
    pub agent_id: Option<String>,
    /// Workspace ID to scope to.
    pub workspace_id: Option<String>,
    /// Method to use for estimation (naive, matching, replay, experiment).
    pub method: Option<String>,
    /// Include identified confounders in output.
    pub include_confounders: bool,
    /// Include assumptions made during estimation.
    pub include_assumptions: bool,
    /// Dry-run mode (show estimation plan without computing).
    pub dry_run: bool,
}

impl EstimateOptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_artifact_id(mut self, id: impl Into<String>) -> Self {
        self.artifact_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_decision_id(mut self, id: impl Into<String>) -> Self {
        self.decision_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_chain_id(mut self, id: impl Into<String>) -> Self {
        self.chain_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_agent_id(mut self, id: impl Into<String>) -> Self {
        self.agent_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_workspace_id(mut self, id: impl Into<String>) -> Self {
        self.workspace_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_method(mut self, method: impl Into<String>) -> Self {
        self.method = Some(method.into());
        self
    }

    #[must_use]
    pub fn with_confounders(mut self) -> Self {
        self.include_confounders = true;
        self
    }

    #[must_use]
    pub fn with_assumptions(mut self) -> Self {
        self.include_assumptions = true;
        self
    }

    #[must_use]
    pub fn dry_run(mut self) -> Self {
        self.dry_run = true;
        self
    }

    fn has_any_filter(&self) -> bool {
        self.artifact_id.is_some()
            || self.decision_id.is_some()
            || self.chain_id.is_some()
            || self.agent_id.is_some()
            || self.workspace_id.is_some()
    }
}

/// Assumption made during causal estimation.
#[derive(Clone, Debug)]
pub struct EstimateAssumption {
    pub code: String,
    pub description: String,
    pub impact: String,
}

impl EstimateAssumption {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        json!({
            "code": self.code,
            "description": self.description,
            "impact": self.impact,
        })
    }
}

/// Identified confounder in estimation.
#[derive(Clone, Debug)]
pub struct EstimateConfounder {
    pub confounder_id: String,
    pub kind: String,
    pub description: String,
    pub severity: f64,
    pub mitigation: String,
}

impl EstimateConfounder {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        json!({
            "confounderId": self.confounder_id,
            "kind": self.kind,
            "description": self.description,
            "severity": self.severity,
            "mitigation": self.mitigation,
        })
    }
}

/// Confidence state with conservative interpretation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfidenceState {
    /// High confidence: experiment-backed evidence
    High,
    /// Medium confidence: replay-backed evidence
    Medium,
    /// Low confidence: correlational evidence only
    Low,
    /// Insufficient: exposure-only, no causal claim possible
    Insufficient,
    /// Rejected: evidence actively contradicts causal claim
    Rejected,
}

impl ConfidenceState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Insufficient => "insufficient",
            Self::Rejected => "rejected",
        }
    }

    #[must_use]
    pub const fn from_evidence_strength(
        strength: crate::models::causal::CausalEvidenceStrength,
    ) -> Self {
        use crate::models::causal::CausalEvidenceStrength;
        match strength {
            CausalEvidenceStrength::ExperimentSupported => Self::High,
            CausalEvidenceStrength::ReplaySupported => Self::Medium,
            CausalEvidenceStrength::Correlational => Self::Low,
            CausalEvidenceStrength::ExposureOnly => Self::Insufficient,
            CausalEvidenceStrength::Rejected => Self::Rejected,
        }
    }
}

/// A single causal uplift estimate.
#[derive(Clone, Debug)]
pub struct CausalUpliftEstimate {
    pub estimate_id: String,
    pub chain_id: Option<String>,
    pub artifact_id: String,
    pub decision_id: String,
    pub method: String,
    pub uplift: f64,
    pub direction: String,
    pub confidence: f64,
    pub evidence_strength: String,
    pub confidence_state: ConfidenceState,
    pub sample_size: u32,
    pub estimated_at: String,
}

impl CausalUpliftEstimate {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        json!({
            "estimateId": self.estimate_id,
            "chainId": self.chain_id,
            "artifactId": self.artifact_id,
            "decisionId": self.decision_id,
            "method": self.method,
            "uplift": self.uplift,
            "direction": self.direction,
            "confidence": self.confidence,
            "evidenceStrength": self.evidence_strength,
            "confidenceState": self.confidence_state.as_str(),
            "sampleSize": self.sample_size,
            "estimatedAt": self.estimated_at,
        })
    }
}

/// Report from causal estimation.
#[derive(Clone, Debug)]
pub struct EstimateReport {
    pub schema: &'static str,
    pub estimates: Vec<CausalUpliftEstimate>,
    pub assumptions: Vec<EstimateAssumption>,
    pub confounders: Vec<EstimateConfounder>,
    pub filters_applied: Vec<String>,
    pub degradations: Vec<TraceDegradation>,
    pub method_used: String,
    pub dry_run: bool,
}

impl EstimateReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        json!({
            "schema": self.schema,
            "command": "causal estimate",
            "estimates": self.estimates.iter().map(CausalUpliftEstimate::data_json).collect::<Vec<_>>(),
            "assumptions": self.assumptions.iter().map(EstimateAssumption::data_json).collect::<Vec<_>>(),
            "confounders": self.confounders.iter().map(EstimateConfounder::data_json).collect::<Vec<_>>(),
            "summary": {
                "totalEstimates": self.estimates.len(),
                "totalAssumptions": self.assumptions.len(),
                "totalConfounders": self.confounders.len(),
                "methodUsed": self.method_used,
            },
            "filtersApplied": self.filters_applied,
            "degradations": causal_degradations_data_json("causal_estimate", &self.degradations),
            "dryRun": self.dry_run,
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(1024);
        if self.dry_run {
            out.push_str("Causal Estimate [DRY RUN]\n");
        } else {
            out.push_str("Causal Estimate Report\n");
        }
        out.push_str("======================\n\n");

        out.push_str(&format!("Method: {}\n", self.method_used));
        out.push_str(&format!("Estimates found: {}\n", self.estimates.len()));

        if !self.estimates.is_empty() {
            out.push_str("\nEstimates:\n");
            for (i, est) in self.estimates.iter().enumerate() {
                out.push_str(&format!(
                    "  {}. {} -> {} (uplift: {:.3}, confidence: {}, evidence: {})\n",
                    i + 1,
                    est.artifact_id,
                    est.decision_id,
                    est.uplift,
                    est.confidence_state.as_str(),
                    est.evidence_strength
                ));
            }
        }

        if !self.assumptions.is_empty() {
            out.push_str("\nAssumptions:\n");
            for assumption in &self.assumptions {
                out.push_str(&format!(
                    "  - [{}] {}\n",
                    assumption.code, assumption.description
                ));
            }
        }

        if !self.confounders.is_empty() {
            out.push_str("\nConfounders:\n");
            for conf in &self.confounders {
                out.push_str(&format!(
                    "  - [{}] {} (severity: {:.2})\n",
                    conf.kind, conf.description, conf.severity
                ));
            }
        }

        if !self.degradations.is_empty() {
            out.push_str("\nDegradations:\n");
            for deg in &self.degradations {
                out.push_str(&format!(
                    "  - [{}] {}: {}\n",
                    deg.severity, deg.code, deg.message
                ));
            }
        }

        out
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.estimates.is_empty()
    }

    #[must_use]
    pub fn has_confounders(&self) -> bool {
        !self.confounders.is_empty()
    }
}

/// Compute causal estimates based on the provided options.
#[must_use]
pub fn estimate_causal_uplift(options: &EstimateOptions) -> EstimateReport {
    let mut filters_applied = Vec::new();
    let mut degradations = Vec::new();

    if let Some(ref artifact_id) = options.artifact_id {
        filters_applied.push(format!("artifact_id={artifact_id}"));
    }
    if let Some(ref decision_id) = options.decision_id {
        filters_applied.push(format!("decision_id={decision_id}"));
    }
    if let Some(ref chain_id) = options.chain_id {
        filters_applied.push(format!("chain_id={chain_id}"));
    }
    if let Some(ref agent_id) = options.agent_id {
        filters_applied.push(format!("agent_id={agent_id}"));
    }
    if let Some(ref workspace_id) = options.workspace_id {
        filters_applied.push(format!("workspace_id={workspace_id}"));
    }

    let method_used = options
        .method
        .clone()
        .unwrap_or_else(|| "naive".to_string());

    if !options.has_any_filter() {
        degradations.push(TraceDegradation {
            code: "no_filters".to_string(),
            message: "No artifact or decision ID provided; cannot compute estimate".to_string(),
            severity: "warning".to_string(),
            repair: None,
        });
    } else if !options.dry_run {
        degradations.push(TraceDegradation {
            code: "causal_sample_underpowered".to_string(),
            message: "Sample size 0 with no observed baseline/outcome ledger; no causal estimate is actionable.".to_string(),
            severity: "warning".to_string(),
            repair: None,
        });
        if options.include_confounders {
            degradations.push(TraceDegradation {
                code: "causal_confounders_unavailable".to_string(),
                message: "No explicit confounder ledger rows were supplied; refusing to fabricate confounders.".to_string(),
                severity: "warning".to_string(),
                repair: None,
            });
        }
    }

    // Causal estimate requires real sample data from exposure/outcome ledgers.
    // Without persisted evidence, we abstain rather than invent uplift values.
    let estimates: Vec<CausalUpliftEstimate> = Vec::new();

    // Assumptions depend on the method, but without samples they are purely informational.
    let assumptions = if options.include_assumptions {
        build_method_assumptions(&method_used)
    } else {
        Vec::new()
    };

    // No confounders without real evidence data.
    let confounders: Vec<EstimateConfounder> = Vec::new();

    EstimateReport {
        schema: CAUSAL_ESTIMATE_SCHEMA_V1,
        estimates,
        assumptions,
        confounders,
        filters_applied,
        degradations,
        method_used,
        dry_run: options.dry_run,
    }
}

/// Estimate one persisted causal chain using the ledger edge contribution scores.
pub fn estimate_causal_chain_from_store(
    conn: &DbConnection,
    workspace_id: &str,
    options: &EstimateOptions,
) -> Result<EstimateReport, DomainError> {
    let mut report = estimate_causal_uplift(options);
    report.degradations.retain(|degradation| {
        degradation.code != "causal_sample_underpowered" && degradation.code != "no_filters"
    });

    let Some(chain_id) = options
        .chain_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
    else {
        report.degradations.push(trace_degradation(
            "causal_chain_id_required",
            "Provide a chain ID from `ee causal trace ... --json`.",
            "warning",
        ));
        return Ok(report);
    };
    if options.dry_run {
        return Ok(report);
    }

    let Some(chain) = find_causal_chain_by_id(conn, workspace_id, chain_id, 32)? else {
        report.degradations.push(trace_degradation(
            "causal_chain_not_found",
            format!("No persisted causal chain `{chain_id}` was found in the evidence ledger."),
            "warning",
        ));
        return Ok(report);
    };

    let estimate = estimate_from_chain(&chain, report.method_used.clone());
    report.estimates = vec![estimate];
    Ok(report)
}

/// Estimate causal uplift for chains matching artifact_id or decision_id filters.
///
/// When `chain_id` is not provided but `artifact_id` or `decision_id` is, this
/// function finds all matching chains and estimates each one.
pub fn estimate_causal_filtered_from_store(
    conn: &DbConnection,
    workspace_id: &str,
    options: &EstimateOptions,
) -> Result<EstimateReport, DomainError> {
    let mut report = estimate_causal_uplift(options);
    report
        .degradations
        .retain(|d| d.code != "causal_sample_underpowered" && d.code != "no_filters");

    if options.dry_run {
        return Ok(report);
    }

    let (edges, _) = load_causal_ledger_edges(conn, workspace_id)?;
    if edges.is_empty() {
        report.degradations.push(trace_degradation(
            "causal_ledger_empty",
            "No causal ledger edges found in workspace.",
            "warning",
        ));
        return Ok(report);
    }

    let candidate_failures = edges
        .iter()
        .map(|edge| edge.failure_id.clone())
        .collect::<BTreeSet<_>>();

    let mut matching_chains = Vec::new();
    for failure_id in candidate_failures {
        let trace_opts = TraceOptions::new()
            .with_memory_id(failure_id)
            .with_depth(32)
            .with_limit(usize::MAX);
        for chain in build_causal_chains(
            conn,
            workspace_id,
            trace_opts.memory_id.as_deref().unwrap_or_default(),
            &edges,
            &trace_opts,
        )? {
            let chain_artifact_id = chain.root_cause_id.as_deref();
            let chain_decision_id = chain.failure_id.as_deref();

            let artifact_match = options
                .artifact_id
                .as_ref()
                .is_none_or(|id| chain_artifact_id == Some(id.as_str()));
            let decision_match = options
                .decision_id
                .as_ref()
                .is_none_or(|id| chain_decision_id == Some(id.as_str()));

            if artifact_match && decision_match {
                matching_chains.push(chain);
            }
        }
    }

    if matching_chains.is_empty() {
        report.degradations.push(trace_degradation(
            "causal_no_matching_chains",
            "No causal chains match the provided filters.",
            "info",
        ));
    } else {
        report.estimates = matching_chains
            .iter()
            .map(|chain| estimate_from_chain(chain, report.method_used.clone()))
            .collect();
    }

    Ok(report)
}

fn estimate_from_chain(chain: &CausalChain, method: String) -> CausalUpliftEstimate {
    let evidence_strength = causal_evidence_strength(&chain.edges);
    let confidence_state = ConfidenceState::from_evidence_strength(evidence_strength);
    let uplift = rounded_causal_metric(chain.contribution_estimate);
    let confidence = match confidence_state {
        ConfidenceState::High => uplift,
        ConfidenceState::Medium => uplift * 0.8,
        ConfidenceState::Low => uplift * 0.6,
        ConfidenceState::Insufficient | ConfidenceState::Rejected => 0.0,
    };

    CausalUpliftEstimate {
        estimate_id: format!("estimate-{}", chain.chain_id),
        chain_id: Some(chain.chain_id.clone()),
        artifact_id: chain
            .root_cause_id
            .clone()
            .unwrap_or_else(|| "artifact-unknown".to_owned()),
        decision_id: chain
            .failure_id
            .clone()
            .unwrap_or_else(|| "failure-unknown".to_owned()),
        method,
        uplift,
        direction: if uplift > 0.0 {
            "positive".to_owned()
        } else {
            "neutral".to_owned()
        },
        confidence: rounded_causal_metric(confidence),
        evidence_strength: evidence_strength.as_str().to_owned(),
        confidence_state,
        sample_size: u32::try_from(chain.edges.len()).unwrap_or(u32::MAX),
        estimated_at: chain
            .edges
            .iter()
            .map(|edge| edge.computed_at.as_str())
            .max()
            .unwrap_or("1970-01-01T00:00:00Z")
            .to_owned(),
    }
}

fn find_causal_chain_by_id(
    conn: &DbConnection,
    workspace_id: &str,
    chain_id: &str,
    depth: usize,
) -> Result<Option<CausalChain>, DomainError> {
    let (edges, _) = load_causal_ledger_edges(conn, workspace_id)?;
    if edges.is_empty() {
        return Ok(None);
    }

    let candidate_failures = edges
        .iter()
        .map(|edge| edge.failure_id.clone())
        .collect::<BTreeSet<_>>();
    for failure_id in candidate_failures {
        let options = TraceOptions::new()
            .with_memory_id(failure_id)
            .with_depth(depth)
            .with_limit(usize::MAX);
        for chain in build_causal_chains(
            conn,
            workspace_id,
            options.memory_id.as_deref().unwrap_or_default(),
            &edges,
            &options,
        )? {
            if chain.chain_id == chain_id {
                return Ok(Some(chain));
            }
        }
    }
    Ok(None)
}

fn build_method_assumptions(method: &str) -> Vec<EstimateAssumption> {
    let mut assumptions = vec![
        EstimateAssumption {
            code: "stable_unit".to_string(),
            description: "Treatment of one unit does not affect outcomes of other units"
                .to_string(),
            impact: "Violation could lead to biased estimates".to_string(),
        },
        EstimateAssumption {
            code: "positivity".to_string(),
            description: "All units have non-zero probability of receiving treatment".to_string(),
            impact: "Violation prevents extrapolation to untreated units".to_string(),
        },
    ];

    match method {
        "naive" => {
            assumptions.push(EstimateAssumption {
                code: "no_confounders".to_string(),
                description: "No unmeasured confounders affect both exposure and outcome"
                    .to_string(),
                impact: "Almost certainly violated; estimate is likely biased".to_string(),
            });
        }
        "matching" => {
            assumptions.push(EstimateAssumption {
                code: "conditional_independence".to_string(),
                description: "Conditional on covariates, exposure is independent of outcome"
                    .to_string(),
                impact: "Unobserved confounders still cause bias".to_string(),
            });
        }
        "replay" => {
            assumptions.push(EstimateAssumption {
                code: "replay_fidelity".to_string(),
                description: "Counterfactual replay accurately represents what would have happened"
                    .to_string(),
                impact: "Model errors propagate to estimate uncertainty".to_string(),
            });
        }
        "experiment" => {
            assumptions.push(EstimateAssumption {
                code: "proper_randomization".to_string(),
                description: "Treatment assignment was properly randomized".to_string(),
                impact: "If violated, selection bias contaminates estimate".to_string(),
            });
        }
        _ => {}
    }

    assumptions
}

// ============================================================================
// Compare Options and Report (EE-453)
// ============================================================================

/// Options for comparing causal evidence across fixture replay, shadow runs,
/// counterfactual episodes, and active-learning experiments.
#[derive(Clone, Debug, Default)]
pub struct CompareOptions {
    /// Baseline causal chain ID.
    pub chain_a_id: Option<String>,
    /// Candidate causal chain ID.
    pub chain_b_id: Option<String>,
    /// Optional artifact scope.
    pub artifact_id: Option<String>,
    /// Optional decision scope.
    pub decision_id: Option<String>,
    /// Fixture replay record ID.
    pub fixture_replay_id: Option<String>,
    /// Shadow-run output ID.
    pub shadow_run_id: Option<String>,
    /// Counterfactual episode ID.
    pub counterfactual_episode_id: Option<String>,
    /// Active-learning experiment ID.
    pub experiment_id: Option<String>,
    /// Estimation method (naive, matching, replay, experiment).
    pub method: Option<String>,
    /// Dry-run mode (plan only).
    pub dry_run: bool,
}

impl CompareOptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_artifact_id(mut self, id: impl Into<String>) -> Self {
        self.artifact_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_chain_a_id(mut self, id: impl Into<String>) -> Self {
        self.chain_a_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_chain_b_id(mut self, id: impl Into<String>) -> Self {
        self.chain_b_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_decision_id(mut self, id: impl Into<String>) -> Self {
        self.decision_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_fixture_replay_id(mut self, id: impl Into<String>) -> Self {
        self.fixture_replay_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_shadow_run_id(mut self, id: impl Into<String>) -> Self {
        self.shadow_run_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_counterfactual_episode_id(mut self, id: impl Into<String>) -> Self {
        self.counterfactual_episode_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_experiment_id(mut self, id: impl Into<String>) -> Self {
        self.experiment_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_method(mut self, method: impl Into<String>) -> Self {
        self.method = Some(method.into());
        self
    }

    #[must_use]
    pub fn dry_run(mut self) -> Self {
        self.dry_run = true;
        self
    }

    fn has_any_scope(&self) -> bool {
        self.chain_a_id.is_some()
            || self.chain_b_id.is_some()
            || self.artifact_id.is_some()
            || self.decision_id.is_some()
            || self.fixture_replay_id.is_some()
            || self.shadow_run_id.is_some()
            || self.counterfactual_episode_id.is_some()
            || self.experiment_id.is_some()
    }

    fn selected_sources(&self) -> Vec<(&'static str, &str)> {
        let mut selected = Vec::new();
        if let Some(source_id) = self.fixture_replay_id.as_deref() {
            selected.push(("fixture_replay", source_id));
        }
        if let Some(source_id) = self.shadow_run_id.as_deref() {
            selected.push(("shadow_run", source_id));
        }
        if let Some(source_id) = self.counterfactual_episode_id.as_deref() {
            selected.push(("counterfactual_episode", source_id));
        }
        if let Some(source_id) = self.experiment_id.as_deref() {
            selected.push(("active_learning_experiment", source_id));
        }
        selected
    }
}

/// Per-source comparison record.
#[derive(Clone, Debug)]
pub struct CausalComparison {
    pub comparison_id: String,
    pub source_kind: String,
    pub source_id: String,
    pub baseline_uplift: f64,
    pub candidate_uplift: f64,
    pub uplift_delta: f64,
    pub confidence_state: ConfidenceState,
    pub evidence_strength: String,
    pub verdict: String,
}

impl CausalComparison {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "comparisonId": self.comparison_id,
            "sourceKind": self.source_kind,
            "sourceId": self.source_id,
            "baselineUplift": self.baseline_uplift,
            "candidateUplift": self.candidate_uplift,
            "upliftDelta": self.uplift_delta,
            "confidenceState": self.confidence_state.as_str(),
            "evidenceStrength": self.evidence_strength,
            "verdict": self.verdict,
        })
    }
}

/// Report from `ee causal compare`.
#[derive(Clone, Debug)]
pub struct CompareReport {
    pub schema: &'static str,
    pub comparisons: Vec<CausalComparison>,
    pub filters_applied: Vec<String>,
    pub degradations: Vec<TraceDegradation>,
    pub method_used: String,
    pub dry_run: bool,
}

impl CompareReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        let (improved, regressed, flat) =
            self.comparisons
                .iter()
                .fold(
                    (0usize, 0usize, 0usize),
                    |acc, comparison| match comparison.verdict.as_str() {
                        "improves" => (acc.0 + 1, acc.1, acc.2),
                        "regresses" => (acc.0, acc.1 + 1, acc.2),
                        _ => (acc.0, acc.1, acc.2 + 1),
                    },
                );

        json!({
            "schema": self.schema,
            "command": "causal compare",
            "comparisons": self.comparisons.iter().map(CausalComparison::data_json).collect::<Vec<_>>(),
            "summary": {
                "totalComparisons": self.comparisons.len(),
                "improvesCount": improved,
                "regressesCount": regressed,
                "flatCount": flat,
                "methodUsed": self.method_used,
            },
            "filtersApplied": self.filters_applied,
            "degradations": causal_degradations_data_json("causal_compare", &self.degradations),
            "dryRun": self.dry_run,
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(1024);
        if self.dry_run {
            out.push_str("Causal Compare [DRY RUN]\n");
        } else {
            out.push_str("Causal Compare Report\n");
        }
        out.push_str("====================\n\n");
        out.push_str(&format!("Method: {}\n", self.method_used));
        out.push_str(&format!("Comparisons: {}\n", self.comparisons.len()));
        if !self.comparisons.is_empty() {
            out.push_str("\nResults:\n");
            for (index, comparison) in self.comparisons.iter().enumerate() {
                out.push_str(&format!(
                    "  {}. {}:{} -> {} (delta: {:+.3}, confidence: {})\n",
                    index + 1,
                    comparison.source_kind,
                    comparison.source_id,
                    comparison.verdict,
                    comparison.uplift_delta,
                    comparison.confidence_state.as_str(),
                ));
            }
        }
        if !self.degradations.is_empty() {
            out.push_str("\nDegradations:\n");
            for degradation in &self.degradations {
                out.push_str(&format!(
                    "  - [{}] {}: {}\n",
                    degradation.severity, degradation.code, degradation.message
                ));
            }
        }
        out
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.comparisons.is_empty()
    }
}

/// Compare causal uplift evidence across multiple evidence sources.
#[must_use]
pub fn compare_causal_evidence(options: &CompareOptions) -> CompareReport {
    let mut filters_applied = Vec::new();
    let mut degradations = Vec::new();

    if let Some(chain_id) = options.chain_a_id.as_ref() {
        filters_applied.push(format!("chain_a={chain_id}"));
    }
    if let Some(chain_id) = options.chain_b_id.as_ref() {
        filters_applied.push(format!("chain_b={chain_id}"));
    }
    if let Some(artifact_id) = options.artifact_id.as_ref() {
        filters_applied.push(format!("artifact_id={artifact_id}"));
    }
    if let Some(decision_id) = options.decision_id.as_ref() {
        filters_applied.push(format!("decision_id={decision_id}"));
    }
    if let Some(source_id) = options.fixture_replay_id.as_ref() {
        filters_applied.push(format!("fixture_replay_id={source_id}"));
    }
    if let Some(source_id) = options.shadow_run_id.as_ref() {
        filters_applied.push(format!("shadow_run_id={source_id}"));
    }
    if let Some(source_id) = options.counterfactual_episode_id.as_ref() {
        filters_applied.push(format!("counterfactual_episode_id={source_id}"));
    }
    if let Some(source_id) = options.experiment_id.as_ref() {
        filters_applied.push(format!("experiment_id={source_id}"));
    }

    let requested_method = options
        .method
        .clone()
        .unwrap_or_else(|| "replay".to_string());
    let method_used = normalize_method(&requested_method, &mut degradations);

    if !options.has_any_scope() {
        degradations.push(TraceDegradation {
            code: "no_filters".to_string(),
            message: "No comparison scope provided; add at least one source or scope filter."
                .to_string(),
            severity: "warning".to_string(),
            repair: None,
        });
        return CompareReport {
            schema: CAUSAL_COMPARE_SCHEMA_V1,
            comparisons: Vec::new(),
            filters_applied,
            degradations,
            method_used,
            dry_run: options.dry_run,
        };
    }

    let selected_sources = options.selected_sources();
    if selected_sources.is_empty() {
        degradations.push(TraceDegradation {
            code: "no_sources".to_string(),
            message: "No source IDs provided; add fixture replay, shadow run, counterfactual episode, or experiment ID."
                .to_string(),
            severity: "warning".to_string(),
            repair: None,
        });
        return CompareReport {
            schema: CAUSAL_COMPARE_SCHEMA_V1,
            comparisons: Vec::new(),
            filters_applied,
            degradations,
            method_used,
            dry_run: options.dry_run,
        };
    }

    // Causal comparison requires real fixture replay, shadow run, counterfactual
    // episode, or experiment evidence. Without persisted baseline/outcome ledgers,
    // we cannot compare uplift and must abstain rather than fabricate verdicts.
    if !options.dry_run {
        degradations.push(TraceDegradation {
            code: "causal_comparison_evidence_unavailable".to_string(),
            message: "No persisted comparison evidence ledger rows are available; refusing to synthesize baseline/candidate comparisons from source IDs.".to_string(),
            severity: "warning".to_string(),
            repair: None,
        });
    }

    CompareReport {
        schema: CAUSAL_COMPARE_SCHEMA_V1,
        comparisons: Vec::new(),
        filters_applied,
        degradations,
        method_used,
        dry_run: options.dry_run,
    }
}

/// Compare two persisted causal chains by their deterministic contribution estimate.
pub fn compare_causal_chains_from_store(
    conn: &DbConnection,
    workspace_id: &str,
    options: &CompareOptions,
) -> Result<CompareReport, DomainError> {
    let mut report = compare_causal_evidence(options);
    report.degradations.retain(|degradation| {
        !matches!(
            degradation.code.as_str(),
            "causal_comparison_evidence_unavailable" | "no_sources" | "no_filters"
        )
    });

    let chain_a_id = options
        .chain_a_id
        .as_deref()
        .filter(|id| !id.trim().is_empty());
    let chain_b_id = options
        .chain_b_id
        .as_deref()
        .filter(|id| !id.trim().is_empty());
    let (Some(chain_a_id), Some(chain_b_id)) = (chain_a_id, chain_b_id) else {
        report.degradations.push(trace_degradation(
            "causal_chain_pair_required",
            "Provide two chain IDs: `ee causal compare <chain-a> <chain-b> --json`.",
            "warning",
        ));
        return Ok(report);
    };
    if options.dry_run {
        return Ok(report);
    }

    let Some(chain_a) = find_causal_chain_by_id(conn, workspace_id, chain_a_id, 32)? else {
        report.degradations.push(trace_degradation(
            "causal_chain_not_found",
            format!("No persisted causal chain `{chain_a_id}` was found."),
            "warning",
        ));
        return Ok(report);
    };
    let Some(chain_b) = find_causal_chain_by_id(conn, workspace_id, chain_b_id, 32)? else {
        report.degradations.push(trace_degradation(
            "causal_chain_not_found",
            format!("No persisted causal chain `{chain_b_id}` was found."),
            "warning",
        ));
        return Ok(report);
    };

    let baseline_uplift = rounded_causal_metric(chain_a.contribution_estimate);
    let candidate_uplift = rounded_causal_metric(chain_b.contribution_estimate);
    let uplift_delta = rounded_causal_metric(candidate_uplift - baseline_uplift);
    let evidence_strength = strongest_evidence_strength(&[
        causal_evidence_strength(&chain_a.edges),
        causal_evidence_strength(&chain_b.edges),
    ]);
    let verdict = if uplift_delta > 0.0 {
        "improves"
    } else if uplift_delta < 0.0 {
        "regresses"
    } else {
        "flat"
    };

    report.comparisons = vec![CausalComparison {
        comparison_id: deterministic_comparison_id(workspace_id, chain_a_id, chain_b_id),
        source_kind: "causal_chain_pair".to_owned(),
        source_id: format!("{chain_a_id}..{chain_b_id}"),
        baseline_uplift,
        candidate_uplift,
        uplift_delta,
        confidence_state: ConfidenceState::from_evidence_strength(evidence_strength),
        evidence_strength: evidence_strength.as_str().to_owned(),
        verdict: verdict.to_owned(),
    }];
    Ok(report)
}

/// Compare causal chains matching artifact_id or decision_id filters.
///
/// When both chain IDs are not provided but artifact_id or decision_id is, find
/// all matching chains and produce pairwise comparisons.
pub fn compare_causal_filtered_from_store(
    conn: &DbConnection,
    workspace_id: &str,
    options: &CompareOptions,
) -> Result<CompareReport, DomainError> {
    let mut report = compare_causal_evidence(options);
    let missing_source_degradations = report
        .degradations
        .iter()
        .filter(|degradation| degradation.code == "no_sources")
        .cloned()
        .collect::<Vec<_>>();
    report.degradations.retain(|d| {
        !matches!(
            d.code.as_str(),
            "causal_comparison_evidence_unavailable" | "no_sources" | "no_filters"
        )
    });

    if options.dry_run {
        return Ok(report);
    }

    let (edges, _) = load_causal_ledger_edges(conn, workspace_id)?;
    if edges.is_empty() {
        report.degradations.extend(missing_source_degradations);
        report.degradations.push(trace_degradation(
            "causal_ledger_empty",
            "No causal ledger edges found in workspace.",
            "warning",
        ));
        return Ok(report);
    }

    let candidate_failures = edges
        .iter()
        .map(|edge| edge.failure_id.clone())
        .collect::<BTreeSet<_>>();

    let mut matching_chains = Vec::new();
    for failure_id in candidate_failures {
        let trace_opts = TraceOptions::new()
            .with_memory_id(failure_id)
            .with_depth(32)
            .with_limit(usize::MAX);
        for chain in build_causal_chains(
            conn,
            workspace_id,
            trace_opts.memory_id.as_deref().unwrap_or_default(),
            &edges,
            &trace_opts,
        )? {
            let chain_artifact_id = chain.root_cause_id.as_deref();
            let chain_decision_id = chain.failure_id.as_deref();

            let artifact_match = options
                .artifact_id
                .as_ref()
                .is_none_or(|id| chain_artifact_id == Some(id.as_str()));
            let decision_match = options
                .decision_id
                .as_ref()
                .is_none_or(|id| chain_decision_id == Some(id.as_str()));

            if artifact_match && decision_match {
                matching_chains.push(chain);
            }
        }
    }

    if matching_chains.len() < 2 {
        report.degradations.push(trace_degradation(
            "causal_insufficient_chains",
            format!(
                "Found {} matching chain(s); comparison requires at least 2.",
                matching_chains.len()
            ),
            "info",
        ));
        return Ok(report);
    }

    // Produce pairwise comparisons for up to first 10 chains to avoid explosion
    let limit = matching_chains.len().min(10);
    let mut comparisons = Vec::new();
    for i in 0..limit {
        for j in (i + 1)..limit {
            let chain_a = &matching_chains[i];
            let chain_b = &matching_chains[j];

            let baseline_uplift = rounded_causal_metric(chain_a.contribution_estimate);
            let candidate_uplift = rounded_causal_metric(chain_b.contribution_estimate);
            let uplift_delta = rounded_causal_metric(candidate_uplift - baseline_uplift);
            let evidence_strength = strongest_evidence_strength(&[
                causal_evidence_strength(&chain_a.edges),
                causal_evidence_strength(&chain_b.edges),
            ]);
            let verdict = if uplift_delta > 0.0 {
                "improves"
            } else if uplift_delta < 0.0 {
                "regresses"
            } else {
                "flat"
            };

            comparisons.push(CausalComparison {
                comparison_id: deterministic_comparison_id(
                    workspace_id,
                    &chain_a.chain_id,
                    &chain_b.chain_id,
                ),
                source_kind: "causal_chain_pair".to_owned(),
                source_id: format!("{}..{}", chain_a.chain_id, chain_b.chain_id),
                baseline_uplift,
                candidate_uplift,
                uplift_delta,
                confidence_state: ConfidenceState::from_evidence_strength(evidence_strength),
                evidence_strength: evidence_strength.as_str().to_owned(),
                verdict: verdict.to_owned(),
            });
        }
    }

    report.comparisons = comparisons;
    Ok(report)
}

fn strongest_evidence_strength(strengths: &[CausalEvidenceStrength]) -> CausalEvidenceStrength {
    if strengths.contains(&CausalEvidenceStrength::ExperimentSupported) {
        CausalEvidenceStrength::ExperimentSupported
    } else if strengths.contains(&CausalEvidenceStrength::ReplaySupported) {
        CausalEvidenceStrength::ReplaySupported
    } else if strengths.contains(&CausalEvidenceStrength::Correlational) {
        CausalEvidenceStrength::Correlational
    } else if strengths.contains(&CausalEvidenceStrength::Rejected) {
        CausalEvidenceStrength::Rejected
    } else {
        CausalEvidenceStrength::ExposureOnly
    }
}

fn deterministic_comparison_id(workspace_id: &str, chain_a_id: &str, chain_b_id: &str) -> String {
    let hash = blake3::hash(format!("{workspace_id}\0{chain_a_id}\0{chain_b_id}").as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    format!(
        "cmp_{}",
        TraceId::from_uuid(uuid::Uuid::from_bytes(bytes))
            .to_string()
            .trim_start_matches("trace_")
    )
}

/// Options for producing a dry-run-first causal promotion plan.
#[derive(Clone, Debug)]
pub struct PromotePlanOptions {
    /// Causal chain ID to route into curation.
    pub chain_id: Option<String>,
    /// Artifact ID targeted by the plan.
    pub artifact_id: Option<String>,
    /// Decision ID used to scope the plan.
    pub decision_id: Option<String>,
    /// Estimate ID used to scope the plan.
    pub estimate_id: Option<String>,
    /// Explicit action override (promote, hold, demote, archive, quarantine).
    pub action: Option<PromotionAction>,
    /// Method used for the supporting estimate (naive, matching, replay, experiment).
    pub method: Option<String>,
    /// Minimum uplift required before auto-promoting.
    pub minimum_uplift: f64,
    /// Include revalidation follow-up recommendations.
    pub include_revalidation: bool,
    /// Include narrower routing recommendations.
    pub include_narrower_routing: bool,
    /// Include experiment proposals.
    pub include_experiment_proposals: bool,
    /// Dry-run mode only (required by policy, but surfaced explicitly for logs).
    pub dry_run: bool,
}

impl Default for PromotePlanOptions {
    fn default() -> Self {
        Self {
            chain_id: None,
            artifact_id: None,
            decision_id: None,
            estimate_id: None,
            action: None,
            method: Some("replay".to_string()),
            minimum_uplift: 0.05,
            include_revalidation: false,
            include_narrower_routing: false,
            include_experiment_proposals: false,
            dry_run: false,
        }
    }
}

impl PromotePlanOptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_artifact_id(mut self, id: impl Into<String>) -> Self {
        self.artifact_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_chain_id(mut self, id: impl Into<String>) -> Self {
        self.chain_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_decision_id(mut self, id: impl Into<String>) -> Self {
        self.decision_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_estimate_id(mut self, id: impl Into<String>) -> Self {
        self.estimate_id = Some(id.into());
        self
    }

    #[must_use]
    pub const fn with_action(mut self, action: PromotionAction) -> Self {
        self.action = Some(action);
        self
    }

    #[must_use]
    pub fn with_method(mut self, method: impl Into<String>) -> Self {
        self.method = Some(method.into());
        self
    }

    #[must_use]
    pub fn with_minimum_uplift(mut self, uplift: f64) -> Self {
        self.minimum_uplift = uplift.clamp(-1.0, 1.0);
        self
    }

    #[must_use]
    pub fn with_revalidation(mut self) -> Self {
        self.include_revalidation = true;
        self
    }

    #[must_use]
    pub fn with_narrower_routing(mut self) -> Self {
        self.include_narrower_routing = true;
        self
    }

    #[must_use]
    pub fn with_experiment_proposals(mut self) -> Self {
        self.include_experiment_proposals = true;
        self
    }

    #[must_use]
    pub fn dry_run(mut self) -> Self {
        self.dry_run = true;
        self
    }

    fn has_any_filter(&self) -> bool {
        self.chain_id.is_some()
            || self.artifact_id.is_some()
            || self.decision_id.is_some()
            || self.estimate_id.is_some()
    }
}

/// Follow-up recommendations from a causal promotion plan.
#[derive(Clone, Debug, Default)]
pub struct PromotePlanRecommendations {
    pub revalidation_steps: Vec<String>,
    pub narrower_routing_steps: Vec<String>,
    pub experiment_proposals: Vec<String>,
    pub review_recommendations: Vec<String>,
    pub safety_guards: Vec<String>,
}

impl PromotePlanRecommendations {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "revalidation": self.revalidation_steps,
            "narrowerRouting": self.narrower_routing_steps,
            "experimentProposals": self.experiment_proposals,
            "reviewRecommendations": self.review_recommendations,
            "safetyGuards": self.safety_guards,
        })
    }
}

/// Deterministic economy projection derived from causal uplift planning.
#[derive(Clone, Debug)]
pub struct PromotePlanEconomyProjection {
    pub priority_delta: i32,
    pub utility_delta: f64,
    pub confidence_delta: f64,
    pub reasoning: String,
}

impl PromotePlanEconomyProjection {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "priorityDelta": self.priority_delta,
            "utilityDelta": self.utility_delta,
            "confidenceDelta": self.confidence_delta,
            "reasoning": self.reasoning,
        })
    }
}

/// Deterministic learning-agenda projection derived from causal uplift planning.
#[derive(Clone, Debug)]
pub struct PromotePlanLearningAgendaProjection {
    pub priority_delta: i32,
    pub queue_action: String,
    pub reasoning: String,
}

impl PromotePlanLearningAgendaProjection {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "priorityDelta": self.priority_delta,
            "queueAction": self.queue_action,
            "reasoning": self.reasoning,
        })
    }
}

/// Deterministic preflight routing projection derived from causal uplift planning.
#[derive(Clone, Debug)]
pub struct PromotePlanPreflightRoutingProjection {
    pub profile: String,
    pub confidence_gate: String,
    pub reasoning: String,
}

impl PromotePlanPreflightRoutingProjection {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "profile": self.profile,
            "confidenceGate": self.confidence_gate,
            "reasoning": self.reasoning,
        })
    }
}

/// Deterministic procedure-verification projection derived from causal uplift planning.
#[derive(Clone, Debug)]
pub struct PromotePlanProcedureVerificationProjection {
    pub status: String,
    pub requires_revalidation: bool,
    pub reasoning: String,
}

impl PromotePlanProcedureVerificationProjection {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "status": self.status,
            "requiresRevalidation": self.requires_revalidation,
            "reasoning": self.reasoning,
        })
    }
}

/// Audit metadata proving downstream effects remain projection-only.
#[derive(Clone, Debug)]
pub struct PromotePlanDownstreamAudit {
    pub mutation_mode: String,
    pub raw_evidence_replaced: bool,
    pub silent_mutation: bool,
}

impl PromotePlanDownstreamAudit {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "mutationMode": self.mutation_mode,
            "rawEvidenceReplaced": self.raw_evidence_replaced,
            "silentMutation": self.silent_mutation,
        })
    }
}

/// Cross-subsystem deterministic projections derived from causal uplift planning.
#[derive(Clone, Debug)]
pub struct PromotePlanDownstreamEffects {
    pub schema: &'static str,
    pub economy_score: PromotePlanEconomyProjection,
    pub learning_agenda: PromotePlanLearningAgendaProjection,
    pub preflight_routing: PromotePlanPreflightRoutingProjection,
    pub procedure_verification: PromotePlanProcedureVerificationProjection,
    pub audit: PromotePlanDownstreamAudit,
}

impl PromotePlanDownstreamEffects {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "schema": self.schema,
            "economyScore": self.economy_score.data_json(),
            "learningAgenda": self.learning_agenda.data_json(),
            "preflightRouting": self.preflight_routing.data_json(),
            "procedureVerification": self.procedure_verification.data_json(),
            "audit": self.audit.data_json(),
        })
    }
}

/// Report from `ee causal promote-plan`.
#[derive(Clone, Debug)]
pub struct PromotePlanReport {
    pub schema: &'static str,
    pub plans: Vec<PromotionPlan>,
    pub curation_candidate_ids: Vec<String>,
    pub recommendations: PromotePlanRecommendations,
    pub downstream_effects: PromotePlanDownstreamEffects,
    pub filters_applied: Vec<String>,
    pub degradations: Vec<TraceDegradation>,
    pub method_used: String,
    pub dry_run: bool,
}

impl PromotePlanReport {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        let (promote_count, hold_count, demote_count) =
            self.plans
                .iter()
                .fold((0usize, 0usize, 0usize), |acc, plan| match plan.action {
                    PromotionAction::Promote => (acc.0 + 1, acc.1, acc.2),
                    PromotionAction::Hold => (acc.0, acc.1 + 1, acc.2),
                    PromotionAction::Demote => (acc.0, acc.1, acc.2 + 1),
                    PromotionAction::Archive | PromotionAction::Quarantine => acc,
                });

        json!({
            "schema": self.schema,
            "command": "causal promote-plan",
            "plans": self.plans.iter().map(PromotionPlan::data_json).collect::<Vec<_>>(),
            "curationCandidateIds": self.curation_candidate_ids,
            "recommendations": self.recommendations.data_json(),
            "downstreamEffects": self.downstream_effects.data_json(),
            "summary": {
                "totalPlans": self.plans.len(),
                "promoteCount": promote_count,
                "holdCount": hold_count,
                "demoteCount": demote_count,
                "methodUsed": self.method_used,
            },
            "filtersApplied": self.filters_applied,
            "degradations": causal_degradations_data_json("causal_promote_plan", &self.degradations),
            "dryRun": self.dry_run,
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::with_capacity(1024);
        if self.dry_run {
            out.push_str("Causal Promotion Plan [DRY RUN]\n");
        } else {
            out.push_str("Causal Promotion Plan\n");
        }
        out.push_str("=====================\n\n");
        out.push_str(&format!("Method: {}\n", self.method_used));
        out.push_str(&format!("Plans generated: {}\n", self.plans.len()));

        if !self.plans.is_empty() {
            out.push_str("\nPlans:\n");
            for (index, plan) in self.plans.iter().enumerate() {
                out.push_str(&format!(
                    "  {}. {} -> {} (uplift: {:.3}, evidence: {})\n",
                    index + 1,
                    plan.artifact_id,
                    plan.action.as_str(),
                    plan.estimated_uplift,
                    plan.evidence_strength.as_str()
                ));
            }
        }

        if !self.recommendations.revalidation_steps.is_empty() {
            out.push_str("\nRevalidation:\n");
            for step in &self.recommendations.revalidation_steps {
                out.push_str(&format!("  - {step}\n"));
            }
        }
        if !self.recommendations.narrower_routing_steps.is_empty() {
            out.push_str("\nNarrower Routing:\n");
            for step in &self.recommendations.narrower_routing_steps {
                out.push_str(&format!("  - {step}\n"));
            }
        }
        if !self.recommendations.experiment_proposals.is_empty() {
            out.push_str("\nExperiment Proposals:\n");
            for proposal in &self.recommendations.experiment_proposals {
                out.push_str(&format!("  - {proposal}\n"));
            }
        }
        if !self.recommendations.review_recommendations.is_empty() {
            out.push_str("\nReview Recommendations:\n");
            for recommendation in &self.recommendations.review_recommendations {
                out.push_str(&format!("  - {recommendation}\n"));
            }
        }
        if !self.recommendations.safety_guards.is_empty() {
            out.push_str("\nSafety Guards:\n");
            for guard in &self.recommendations.safety_guards {
                out.push_str(&format!("  - {guard}\n"));
            }
        }
        out.push_str("\nDownstream Projections:\n");
        out.push_str(&format!(
            "  - Economy priority delta: {}\n",
            self.downstream_effects.economy_score.priority_delta
        ));
        out.push_str(&format!(
            "  - Learning agenda action: {}\n",
            self.downstream_effects.learning_agenda.queue_action
        ));
        out.push_str(&format!(
            "  - Preflight routing profile: {}\n",
            self.downstream_effects.preflight_routing.profile
        ));
        out.push_str(&format!(
            "  - Procedure verification: {}\n",
            self.downstream_effects.procedure_verification.status
        ));
        if !self.degradations.is_empty() {
            out.push_str("\nDegradations:\n");
            for degradation in &self.degradations {
                out.push_str(&format!(
                    "  - [{}] {}: {}\n",
                    degradation.severity, degradation.code, degradation.message
                ));
            }
        }
        out
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.plans.is_empty()
    }
}

#[must_use]
pub fn promote_causal_plan(options: &PromotePlanOptions) -> PromotePlanReport {
    let mut report = empty_promote_plan_report(options);

    if !options.has_any_filter() {
        report.degradations.push(trace_degradation(
            "no_filters",
            "No causal chain, artifact, decision, or estimate ID provided; cannot produce promotion plan.",
            "warning",
        ));
        return report;
    }

    report
        .degradations
        .push(causal_sample_underpowered("causal promote-plan"));
    if let Some(action) = options.action
        && action != PromotionAction::Hold
    {
        report.degradations.push(trace_degradation(
            "action_override_not_actionable",
            format!(
                "Requested action `{}` is recorded as review input only; underpowered evidence cannot promote, demote, archive, or quarantine.",
                action.as_str()
            ),
            "warning",
        ));
    }

    report.recommendations =
        evidence_required_recommendations(options, target_label_from_options(options));
    report
}

fn empty_promote_plan_report(options: &PromotePlanOptions) -> PromotePlanReport {
    let (filters_applied, degradations, method_used) = promote_plan_metadata(options);
    PromotePlanReport {
        schema: CAUSAL_PROMOTE_PLAN_SCHEMA_V1,
        plans: Vec::new(),
        curation_candidate_ids: Vec::new(),
        recommendations: PromotePlanRecommendations::default(),
        downstream_effects: project_downstream_effects(
            PromotionAction::Hold,
            CausalEvidenceStrength::ExposureOnly,
            0.0,
            options.dry_run,
        ),
        filters_applied,
        degradations,
        method_used,
        dry_run: options.dry_run,
    }
}

fn promote_plan_metadata(
    options: &PromotePlanOptions,
) -> (Vec<String>, Vec<TraceDegradation>, String) {
    let mut filters_applied = Vec::new();
    let mut degradations = Vec::new();

    if let Some(ref chain_id) = options.chain_id {
        filters_applied.push(format!("chain_id={chain_id}"));
    }
    if let Some(ref artifact_id) = options.artifact_id {
        filters_applied.push(format!("artifact_id={artifact_id}"));
    }
    if let Some(ref decision_id) = options.decision_id {
        filters_applied.push(format!("decision_id={decision_id}"));
    }
    if let Some(ref estimate_id) = options.estimate_id {
        filters_applied.push(format!("estimate_id={estimate_id}"));
    }

    let requested_method = options
        .method
        .clone()
        .unwrap_or_else(|| "replay".to_string());
    let method_used = normalize_method(&requested_method, &mut degradations);

    if !options.dry_run {
        degradations.push(trace_degradation(
            "dry_run_recommended",
            "Promotion planning is report-only; prefer --dry-run in automation.",
            "info",
        ));
    }

    (filters_applied, degradations, method_used)
}

fn target_label_from_options(options: &PromotePlanOptions) -> String {
    options
        .artifact_id
        .as_deref()
        .or(options.decision_id.as_deref())
        .or(options.estimate_id.as_deref())
        .or(options.chain_id.as_deref())
        .unwrap_or("causal evidence")
        .to_owned()
}

fn evidence_required_recommendations(
    options: &PromotePlanOptions,
    target_label: String,
) -> PromotePlanRecommendations {
    let mut recommendations = PromotePlanRecommendations::default();
    recommendations.safety_guards.push(
        "Safety-critical warnings remain pinned and are never randomized away for evidence collection."
            .to_string(),
    );
    if options.include_revalidation {
        recommendations.revalidation_steps.push(format!(
            "Collect persisted exposure, baseline, outcome, and confounder evidence for `{target_label}` before any causal estimate."
        ));
    }
    if options.include_narrower_routing {
        recommendations.narrower_routing_steps.push(format!(
            "Review routing scope for `{target_label}` manually; this report does not reroute memory."
        ));
    }
    recommendations.review_recommendations.push(format!(
        "Route `{target_label}` to evidence collection; no persisted causal chain supports promotion, demotion, or rerouting."
    ));
    recommendations.experiment_proposals.push(format!(
        "Design an explicit experiment for `{target_label}` and persist treatment, baseline, outcome, and confounder evidence before re-running causal promotion review."
    ));
    recommendations
}

fn chain_recommendations(
    options: &PromotePlanOptions,
    chain: &CausalChain,
    artifact_id: &str,
    evidence_strength: CausalEvidenceStrength,
    action: PromotionAction,
) -> PromotePlanRecommendations {
    let mut recommendations = PromotePlanRecommendations::default();
    recommendations.safety_guards.push(
        "Safety-critical warnings remain pinned and are never randomized away for evidence collection."
            .to_string(),
    );
    if options.include_revalidation || action == PromotionAction::Hold {
        recommendations.revalidation_steps.push(format!(
            "Revalidate `{artifact_id}` against causal chain `{}` before applying any durable rule or routing change.",
            chain.chain_id
        ));
    }
    if options.include_narrower_routing {
        recommendations.narrower_routing_steps.push(format!(
            "Constrain routing for `{artifact_id}` to evidence URI(s): {}.",
            chain.evidence_uris.join(", ")
        ));
    }
    if action == PromotionAction::Promote {
        recommendations.review_recommendations.push(format!(
            "Review promotion candidate `{artifact_id}` from causal chain `{}` before applying it.",
            chain.chain_id
        ));
    } else {
        recommendations.review_recommendations.push(format!(
            "Hold `{artifact_id}` until causal chain `{}` clears the configured uplift and evidence thresholds.",
            chain.chain_id
        ));
    }
    if options.include_experiment_proposals
        || !matches!(
            evidence_strength,
            CausalEvidenceStrength::ExperimentSupported | CausalEvidenceStrength::ReplaySupported
        )
    {
        recommendations.experiment_proposals.push(format!(
            "Add replay or experiment evidence for causal chain `{}` before raising confidence beyond {}.",
            chain.chain_id,
            evidence_strength.as_str()
        ));
    }
    recommendations
}

/// Route one verified causal chain into the curation queue as a plan-recipe proposal.
pub fn promote_causal_chain_from_store(
    conn: &DbConnection,
    workspace_id: &str,
    options: &PromotePlanOptions,
) -> Result<PromotePlanReport, DomainError> {
    let mut report = empty_promote_plan_report(options);

    let Some(chain_id) = options.chain_id.as_deref() else {
        report.degradations.push(trace_degradation(
            "causal_chain_id_required",
            "Provide a chain ID from `ee causal trace ... --json`.",
            "warning",
        ));
        return Ok(report);
    };
    let Some(chain) = find_causal_chain_by_id(conn, workspace_id, chain_id, 32)? else {
        report.degradations.push(trace_degradation(
            "causal_chain_not_found",
            format!("No persisted causal chain `{chain_id}` was found in the evidence ledger."),
            "warning",
        ));
        report.plans.clear();
        return Ok(report);
    };

    let evidence_strength = causal_evidence_strength(&chain.edges);
    let estimated_uplift = rounded_causal_metric(chain.contribution_estimate);
    let action = if estimated_uplift >= options.minimum_uplift
        && matches!(
            evidence_strength,
            CausalEvidenceStrength::ExperimentSupported | CausalEvidenceStrength::ReplaySupported
        ) {
        PromotionAction::Promote
    } else {
        PromotionAction::Hold
    };
    let artifact_id = chain
        .root_cause_id
        .clone()
        .unwrap_or_else(|| "artifact-unknown".to_owned());
    let created_at = chrono::Utc::now().to_rfc3339();
    let candidate_id = deterministic_curation_candidate_id(workspace_id, chain_id);
    let proposed_content = plan_recipe_candidate_content(&chain);
    let reason = format!(
        "Causal chain `{chain_id}` estimates contribution {:.3} from {} ledger edge(s) with {} evidence URI(s).",
        estimated_uplift,
        chain.edges.len(),
        chain.evidence_uris.len()
    );

    if action == PromotionAction::Promote
        && !options.dry_run
        && conn
            .get_curation_candidate(workspace_id, &candidate_id)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to check causal curation candidate: {error}"),
                repair: Some("ee curate candidates --all --json".to_owned()),
            })?
            .is_none()
    {
        ensure_target_memory_exists(conn, workspace_id, &artifact_id)?;
        conn.insert_curation_candidate(
            &candidate_id,
            &CreateCurationCandidateInput {
                workspace_id: workspace_id.to_owned(),
                candidate_type: "procedure".to_owned(),
                target_memory_id: artifact_id.clone(),
                proposed_content: Some(proposed_content),
                proposed_confidence: Some(estimated_uplift as f32),
                proposed_trust_class: Some("agent_validated".to_owned()),
                source_type: "counterfactual_replay".to_owned(),
                source_id: Some(chain_id.to_owned()),
                reason,
                confidence: estimated_uplift as f32,
                status: Some("pending".to_owned()),
                created_at: Some(created_at.clone()),
                ttl_expires_at: None,
            },
        )
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to create causal curation candidate: {error}"),
            repair: Some("ee curate candidates --json".to_owned()),
        })?;
    }

    let plan_status = if options.dry_run {
        PromotionPlanStatus::DryRunReady
    } else {
        PromotionPlanStatus::Proposed
    };
    let plan = PromotionPlan::new(
        format!("plan-{chain_id}"),
        artifact_id.clone(),
        action,
        created_at,
    )
    .with_status(plan_status)
    .with_evidence_strength(evidence_strength)
    .with_minimum_uplift(options.minimum_uplift)
    .with_estimated_uplift(estimated_uplift)
    .with_required_evidence(chain_id)
    .with_audit_id(candidate_id.clone());

    report.plans = vec![plan];
    if !options.dry_run && action == PromotionAction::Promote {
        report.curation_candidate_ids = vec![candidate_id];
    }
    report.recommendations =
        chain_recommendations(options, &chain, &artifact_id, evidence_strength, action);
    report.downstream_effects =
        project_downstream_effects(action, evidence_strength, estimated_uplift, options.dry_run);
    Ok(report)
}

fn ensure_target_memory_exists(
    conn: &DbConnection,
    workspace_id: &str,
    memory_id: &str,
) -> Result<(), DomainError> {
    match conn
        .get_memory(memory_id)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to read candidate target memory `{memory_id}`: {error}"),
            repair: Some("ee memory show <memory-id> --json".to_owned()),
        })? {
        Some(memory) if memory.workspace_id == workspace_id && memory.tombstoned_at.is_none() => {
            Ok(())
        }
        Some(_) => Err(DomainError::PolicyDenied {
            message: format!(
                "Causal promote-plan target `{memory_id}` is not an active memory in this workspace."
            ),
            repair: Some("Use a causal chain whose root cause is an active memory.".to_owned()),
        }),
        None => Err(DomainError::NotFound {
            resource: "causal promote-plan target memory".to_owned(),
            id: memory_id.to_owned(),
            repair: Some(
                "Create or import the root-cause memory before promotion planning.".to_owned(),
            ),
        }),
    }
}

fn deterministic_curation_candidate_id(workspace_id: &str, chain_id: &str) -> String {
    let hash = blake3::hash(format!("{workspace_id}\0causal-plan\0{chain_id}").as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    let candidate = CandidateId::from_uuid(uuid::Uuid::from_bytes(bytes)).to_string();
    format!("curate_{}", candidate.trim_start_matches("cand_"))
}

fn plan_recipe_candidate_content(chain: &CausalChain) -> String {
    let root = chain
        .root_cause_id
        .as_deref()
        .unwrap_or("unknown-root-cause");
    let failure = chain.failure_id.as_deref().unwrap_or("unknown-failure");
    format!(
        "Plan recipe candidate from causal chain `{}`.\nWhen failures resemble `{failure}`, inspect `{root}` first, then walk the persisted evidence URIs before changing memory posture. Evidence: {}",
        chain.chain_id,
        chain.evidence_uris.join(", ")
    )
}

fn project_downstream_effects(
    action: PromotionAction,
    evidence_strength: CausalEvidenceStrength,
    estimated_uplift: f64,
    dry_run: bool,
) -> PromotePlanDownstreamEffects {
    // Without actionable evidence, return review-only projections
    let actionable_evidence = matches!(
        evidence_strength,
        CausalEvidenceStrength::ExperimentSupported | CausalEvidenceStrength::ReplaySupported
    ) && estimated_uplift.abs() > 0.0
        && action != PromotionAction::Hold;

    if !actionable_evidence {
        return PromotePlanDownstreamEffects {
            schema: CAUSAL_DOWNSTREAM_EFFECTS_SCHEMA_V1,
            economy_score: PromotePlanEconomyProjection {
                priority_delta: 0,
                utility_delta: 0.0,
                confidence_delta: 0.0,
                reasoning: "No supported causal evidence is available; economy scoring remains unchanged.".to_string(),
            },
            learning_agenda: PromotePlanLearningAgendaProjection {
                priority_delta: 0,
                queue_action: "review_only".to_string(),
                reasoning: "Underpowered causal evidence opens only a review task and does not mutate learning priority.".to_string(),
            },
            preflight_routing: PromotePlanPreflightRoutingProjection {
                profile: "unchanged".to_string(),
                confidence_gate: "low".to_string(),
                reasoning: "No causal routing change is projected without persisted baseline/outcome evidence.".to_string(),
            },
            procedure_verification: PromotePlanProcedureVerificationProjection {
                status: "evidence_required".to_string(),
                requires_revalidation: true,
                reasoning: "Procedure verification requires explicit causal evidence before status changes.".to_string(),
            },
            audit: PromotePlanDownstreamAudit {
                mutation_mode: if dry_run { "dry_run_review_only".to_string() } else { "proposal_review_only".to_string() },
                raw_evidence_replaced: false,
                silent_mutation: false,
            },
        };
    }

    let priority_delta = match action {
        PromotionAction::Promote => 3,
        PromotionAction::Hold => 1,
        PromotionAction::Demote => -2,
        PromotionAction::Archive | PromotionAction::Quarantine => -3,
    };
    let utility_base = match action {
        PromotionAction::Promote => 0.08,
        PromotionAction::Hold => 0.01,
        PromotionAction::Demote => -0.06,
        PromotionAction::Archive | PromotionAction::Quarantine => -0.09,
    };
    let utility_delta = (utility_base + (estimated_uplift * 0.5)).clamp(-1.0, 1.0);
    let confidence_delta = estimated_uplift.clamp(-0.25, 0.25);

    let learning_priority_delta = match action {
        PromotionAction::Promote => 2,
        PromotionAction::Hold => 1,
        PromotionAction::Demote => -1,
        PromotionAction::Archive | PromotionAction::Quarantine => -2,
    };
    let learning_queue_action = match action {
        PromotionAction::Promote => "raise_priority",
        PromotionAction::Hold => "monitor",
        PromotionAction::Demote => "investigate_regression",
        PromotionAction::Archive | PromotionAction::Quarantine => "quarantine_review",
    };

    let confidence_gate = match evidence_strength {
        CausalEvidenceStrength::ExperimentSupported => "high",
        CausalEvidenceStrength::ReplaySupported => "medium",
        CausalEvidenceStrength::Correlational
        | CausalEvidenceStrength::ExposureOnly
        | CausalEvidenceStrength::Rejected => "low",
    };
    let preflight_profile = match action {
        PromotionAction::Promote => {
            if matches!(
                evidence_strength,
                CausalEvidenceStrength::ExperimentSupported
                    | CausalEvidenceStrength::ReplaySupported
            ) {
                "standard"
            } else {
                "full"
            }
        }
        PromotionAction::Hold => "standard",
        PromotionAction::Demote | PromotionAction::Archive | PromotionAction::Quarantine => "full",
    };

    let (procedure_status, requires_revalidation) = match action {
        PromotionAction::Promote => {
            if matches!(
                evidence_strength,
                CausalEvidenceStrength::ExperimentSupported
                    | CausalEvidenceStrength::ReplaySupported
            ) {
                ("validated_by_uplift", false)
            } else {
                ("provisional_requires_revalidation", true)
            }
        }
        PromotionAction::Hold => ("revalidation_required", true),
        PromotionAction::Demote | PromotionAction::Archive | PromotionAction::Quarantine => {
            ("blocked_until_reverified", true)
        }
    };

    PromotePlanDownstreamEffects {
        schema: CAUSAL_DOWNSTREAM_EFFECTS_SCHEMA_V1,
        economy_score: PromotePlanEconomyProjection {
            priority_delta,
            utility_delta,
            confidence_delta,
            reasoning: format!(
                "Causal action `{}` with `{}` evidence projects deterministic economy scoring movement.",
                action.as_str(),
                evidence_strength.as_str()
            ),
        },
        learning_agenda: PromotePlanLearningAgendaProjection {
            priority_delta: learning_priority_delta,
            queue_action: learning_queue_action.to_string(),
            reasoning: format!(
                "Uplift {:.3} and `{}` evidence update learning agenda priority without mutating source evidence.",
                estimated_uplift,
                evidence_strength.as_str()
            ),
        },
        preflight_routing: PromotePlanPreflightRoutingProjection {
            profile: preflight_profile.to_string(),
            confidence_gate: confidence_gate.to_string(),
            reasoning: format!(
                "Promotion action `{}` maps to `{}` preflight profile under `{}` confidence gate.",
                action.as_str(),
                preflight_profile,
                confidence_gate
            ),
        },
        procedure_verification: PromotePlanProcedureVerificationProjection {
            status: procedure_status.to_string(),
            requires_revalidation,
            reasoning: format!(
                "Procedure verification status tracks causal action `{}` while preserving raw evidence immutability.",
                action.as_str()
            ),
        },
        audit: PromotePlanDownstreamAudit {
            mutation_mode: if dry_run {
                "dry_run_projection".to_string()
            } else {
                "proposal_projection".to_string()
            },
            raw_evidence_replaced: false,
            silent_mutation: false,
        },
    }
}

fn normalize_method(method: &str, degradations: &mut Vec<TraceDegradation>) -> String {
    match method {
        "naive" | "matching" | "replay" | "experiment" => method.to_string(),
        _ => {
            degradations.push(TraceDegradation {
                code: "unknown_method".to_string(),
                message: format!(
                    "Unknown method `{method}`; falling back to `naive` for conservative planning."
                ),
                severity: "warning".to_string(),
                repair: None,
            });
            "naive".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{CreateMemoryInput, CreateWorkspaceInput, DbConnection};
    use crate::models::decision::DecisionPlane;
    use crate::models::{MemoryId, WorkspaceId};

    #[test]
    fn trace_options_builder_works() {
        let opts = TraceOptions::new()
            .with_memory_id("mem-001")
            .with_run_id("run-001")
            .with_limit(10)
            .dry_run();

        assert_eq!(opts.memory_id, Some("mem-001".to_string()));
        assert_eq!(opts.run_id, Some("run-001".to_string()));
        assert_eq!(opts.limit, Some(10));
        assert!(opts.dry_run);
        assert!(opts.has_any_filter());
    }

    #[test]
    fn trace_with_no_filters_returns_empty_with_degradation() {
        let opts = TraceOptions::new();
        let report = trace_causal_chains(&opts);

        assert!(report.is_empty());
        assert!(report.has_degradations());
        assert_eq!(report.degradations[0].code, "no_filters");
    }

    #[test]
    fn trace_dry_run_returns_empty_chains() {
        let opts = TraceOptions::new().with_memory_id("mem-001").dry_run();
        let report = trace_causal_chains(&opts);

        assert!(report.is_empty());
        assert!(report.dry_run);
        assert!(!report.filters_applied.is_empty());
    }

    #[test]
    fn trace_with_run_id_abstains_without_evidence_ledger() {
        let opts = TraceOptions::new().with_run_id("run-test-001");
        let report = trace_causal_chains(&opts);

        assert!(
            report.is_empty(),
            "Without evidence ledger, trace should be empty"
        );
        assert!(
            report
                .degradations
                .iter()
                .any(|d| d.code == "causal_evidence_unavailable"),
            "Should report causal_evidence_unavailable degradation"
        );
        assert!(
            !report.filters_applied.is_empty(),
            "Filter should still be recorded"
        );
    }

    #[test]
    fn trace_report_json_has_correct_schema() {
        let opts = TraceOptions::new().with_pack_id("pack-001");
        let report = trace_causal_chains(&opts);
        let json = report.data_json();

        assert_eq!(json["schema"], CAUSAL_TRACE_SCHEMA_V1);
        assert_eq!(json["command"], "causal trace");
        assert!(json["chains"].is_array());
        assert!(json["summary"]["totalChains"].is_number());
    }

    #[test]
    fn trace_report_json_aggregates_duplicate_degradations() {
        let report = TraceReport {
            schema: CAUSAL_TRACE_SCHEMA_V1,
            chains: Vec::new(),
            total_exposures: 0,
            total_decisions: 0,
            filters_applied: vec!["pack:pack-001".to_string()],
            degradations: vec![
                TraceDegradation {
                    code: "causal_evidence_unavailable".to_string(),
                    message: "No evidence rows for the causal trace.".to_string(),
                    severity: "low".to_string(),
                    repair: Some("ee causal trace --workspace .".to_string()),
                },
                TraceDegradation {
                    code: "causal_evidence_unavailable".to_string(),
                    message: "No persisted causal evidence ledger rows are available.".to_string(),
                    severity: "medium".to_string(),
                    repair: Some("ee doctor --json".to_string()),
                },
            ],
            dry_run: true,
        };

        let json = report.data_json();
        let degradations = json["degradations"]
            .as_array()
            .expect("degradations should be an array");

        assert_eq!(degradations.len(), 1);
        assert_eq!(degradations[0]["code"], "causal_evidence_unavailable");
        assert_eq!(degradations[0]["severity"], "medium");
        assert_eq!(degradations[0]["repair"], "ee doctor --json");
        assert_eq!(
            degradations[0]["sources"],
            serde_json::json!(["causal_trace"])
        );
    }

    #[test]
    fn trace_report_human_summary_is_readable() {
        let opts = TraceOptions::new()
            .with_procedure_id("proc-001")
            .with_agent_id("claude-code");
        let report = trace_causal_chains(&opts);
        let summary = report.human_summary();

        assert!(summary.contains("Causal Trace Report"));
        assert!(summary.contains("Chains found:"));
    }

    #[test]
    fn causal_exposure_json_includes_all_fields() {
        let exposure = CausalExposure {
            exposure_id: "exp-001".to_string(),
            channel: CausalExposureChannel::ContextPack,
            artifact_id: "art-001".to_string(),
            artifact_type: "memory".to_string(),
            exposed_at: "2026-05-02T00:00:00Z".to_string(),
            context_pack_id: Some("pack-001".to_string()),
            recorder_run_id: None,
        };
        let json = exposure.data_json();

        assert_eq!(json["exposureId"], "exp-001");
        assert_eq!(json["channel"], "context_pack");
        assert_eq!(json["contextPackId"], "pack-001");
    }

    #[test]
    fn causal_chain_total_artifact_count_is_accurate() {
        let chain = CausalChain {
            chain_id: "chain-001".to_string(),
            failure_id: None,
            root_cause_id: None,
            nodes: Vec::new(),
            edges: Vec::new(),
            contribution_estimate: 0.0,
            evidence_uris: Vec::new(),
            decision_trace: CausalDecisionTrace::new(
                "dec-001",
                "trace-001",
                DecisionPlane::Observe,
                "2026-05-02T00:00:00Z",
                "agent",
                "rationale",
            ),
            exposures: vec![],
            recorder_run_ids: vec!["run-1".to_string(), "run-2".to_string()],
            context_pack_ids: vec!["pack-1".to_string()],
            preflight_ids: vec![],
            tripwire_ids: vec!["tw-1".to_string()],
            procedure_ids: vec!["proc-1".to_string()],
        };

        assert_eq!(chain.total_artifact_count(), 5);
    }

    #[test]
    fn trace_from_store_builds_four_node_chain() -> Result<(), String> {
        let fixture = CausalStoreFixture::new()?;
        fixture.insert_edge("cev_001", &fixture.failure, &fixture.decision, 0.9)?;
        fixture.insert_edge("cev_002", &fixture.decision, &fixture.action, 0.8)?;
        fixture.insert_edge("cev_003", &fixture.action, &fixture.root, 0.7)?;

        let report = trace_causal_chains_from_store(
            &fixture.connection,
            &fixture.workspace_id,
            &TraceOptions::new()
                .with_memory_id(&fixture.failure)
                .with_depth(4),
        )
        .map_err(|error| error.message())?;

        assert_eq!(report.chains.len(), 1);
        assert_eq!(report.chains[0].nodes.len(), 4);
        assert_eq!(report.chains[0].edges.len(), 3);
        assert_eq!(
            report.chains[0].root_cause_id.as_deref(),
            Some(fixture.root.as_str())
        );
        Ok(())
    }

    #[test]
    fn estimate_from_store_is_deterministic() -> Result<(), String> {
        let fixture = CausalStoreFixture::new()?;
        fixture.insert_edge("cev_010", &fixture.failure, &fixture.decision, 0.5)?;
        let trace = trace_causal_chains_from_store(
            &fixture.connection,
            &fixture.workspace_id,
            &TraceOptions::new().with_memory_id(&fixture.failure),
        )
        .map_err(|error| error.message())?;
        let chain_id = trace.chains[0].chain_id.clone();
        let options = EstimateOptions::new().with_chain_id(chain_id);

        let first =
            estimate_causal_chain_from_store(&fixture.connection, &fixture.workspace_id, &options)
                .map_err(|error| error.message())?;
        let second =
            estimate_causal_chain_from_store(&fixture.connection, &fixture.workspace_id, &options)
                .map_err(|error| error.message())?;

        assert_eq!(first.data_json(), second.data_json());
        assert_eq!(first.estimates[0].uplift, 0.5);
        Ok(())
    }

    #[test]
    fn estimate_from_store_treats_blank_chain_id_as_missing() -> Result<(), String> {
        let fixture = CausalStoreFixture::new()?;
        let report = estimate_causal_chain_from_store(
            &fixture.connection,
            &fixture.workspace_id,
            &EstimateOptions::new().with_chain_id("  "),
        )
        .map_err(|error| error.message())?;

        assert!(report.estimates.is_empty());
        assert!(
            report
                .degradations
                .iter()
                .any(|degradation| degradation.code == "causal_chain_id_required"),
            "expected causal_chain_id_required, got {:?}",
            report.degradations
        );
        Ok(())
    }

    #[test]
    fn compare_from_store_treats_blank_chain_id_as_missing() -> Result<(), String> {
        let fixture = CausalStoreFixture::new()?;
        let report = compare_causal_chains_from_store(
            &fixture.connection,
            &fixture.workspace_id,
            &CompareOptions::new()
                .with_chain_a_id("chain-a")
                .with_chain_b_id(""),
        )
        .map_err(|error| error.message())?;

        assert!(report.comparisons.is_empty());
        assert!(
            report
                .degradations
                .iter()
                .any(|degradation| degradation.code == "causal_chain_pair_required"),
            "expected causal_chain_pair_required, got {:?}",
            report.degradations
        );
        Ok(())
    }

    #[test]
    fn compare_filtered_from_empty_store_reports_missing_sources_and_ledger() -> Result<(), String>
    {
        let fixture = CausalStoreFixture::new()?;
        let report = compare_causal_filtered_from_store(
            &fixture.connection,
            &fixture.workspace_id,
            &CompareOptions::new().with_artifact_id(&fixture.root),
        )
        .map_err(|error| error.message())?;

        assert!(report.comparisons.is_empty());
        assert!(
            report
                .degradations
                .iter()
                .any(|degradation| degradation.code == "no_sources"),
            "expected no_sources, got {:?}",
            report.degradations
        );
        assert!(
            report
                .degradations
                .iter()
                .any(|degradation| degradation.code == "causal_ledger_empty"),
            "expected causal_ledger_empty, got {:?}",
            report.degradations
        );
        Ok(())
    }

    #[test]
    fn promote_plan_from_store_creates_curation_candidate() -> Result<(), String> {
        let fixture = CausalStoreFixture::new()?;
        fixture.insert_edge_with_method(
            "cev_020",
            &fixture.failure,
            &fixture.root,
            0.75,
            CausalEvidenceMethod::GraphInferred,
        )?;
        let trace = trace_causal_chains_from_store(
            &fixture.connection,
            &fixture.workspace_id,
            &TraceOptions::new().with_memory_id(&fixture.failure),
        )
        .map_err(|error| error.message())?;
        let chain_id = trace.chains[0].chain_id.clone();

        let report = promote_causal_chain_from_store(
            &fixture.connection,
            &fixture.workspace_id,
            &PromotePlanOptions::new().with_chain_id(chain_id),
        )
        .map_err(|error| error.message())?;

        assert_eq!(report.curation_candidate_ids.len(), 1);
        assert_eq!(report.plans.len(), 1);
        assert_eq!(report.plans[0].artifact_id, fixture.root);
        assert_eq!(report.plans[0].action, PromotionAction::Promote);
        assert_eq!(
            report.plans[0].evidence_strength,
            CausalEvidenceStrength::ReplaySupported
        );
        assert_eq!(report.plans[0].estimated_uplift, 0.75);
        let candidates = fixture
            .connection
            .list_curation_candidates(
                &fixture.workspace_id,
                Some("procedure"),
                Some("pending"),
                Some(&fixture.root),
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].id, report.curation_candidate_ids[0]);
        Ok(())
    }

    #[test]
    fn promote_plan_from_store_dry_run_uses_persisted_chain_without_candidate() -> Result<(), String>
    {
        let fixture = CausalStoreFixture::new()?;
        fixture.insert_edge_with_method(
            "cev_021",
            &fixture.failure,
            &fixture.root,
            0.75,
            CausalEvidenceMethod::GraphInferred,
        )?;
        let trace = trace_causal_chains_from_store(
            &fixture.connection,
            &fixture.workspace_id,
            &TraceOptions::new().with_memory_id(&fixture.failure),
        )
        .map_err(|error| error.message())?;
        let chain_id = trace.chains[0].chain_id.clone();

        let report = promote_causal_chain_from_store(
            &fixture.connection,
            &fixture.workspace_id,
            &PromotePlanOptions::new().with_chain_id(chain_id).dry_run(),
        )
        .map_err(|error| error.message())?;

        assert!(report.curation_candidate_ids.is_empty());
        assert_eq!(report.plans.len(), 1);
        assert_eq!(report.plans[0].artifact_id, fixture.root);
        assert_eq!(report.plans[0].status, PromotionPlanStatus::DryRunReady);
        assert_eq!(report.plans[0].estimated_uplift, 0.75);
        let candidates = fixture
            .connection
            .list_curation_candidates(&fixture.workspace_id, None, None, None)
            .map_err(|error| error.to_string())?;
        assert!(candidates.is_empty());
        Ok(())
    }

    #[test]
    fn trace_from_store_rejects_cycles() -> Result<(), String> {
        let fixture = CausalStoreFixture::new()?;
        fixture.insert_edge("cev_030", &fixture.failure, &fixture.decision, 0.6)?;
        fixture.insert_edge("cev_031", &fixture.decision, &fixture.failure, 0.4)?;

        let error = match trace_causal_chains_from_store(
            &fixture.connection,
            &fixture.workspace_id,
            &TraceOptions::new().with_memory_id(&fixture.failure),
        ) {
            Ok(_) => return Err("cycle should be rejected".to_owned()),
            Err(error) => error,
        };

        assert_eq!(error.code(), "graph");
        assert!(error.message().contains("cycle"));
        Ok(())
    }

    struct CausalStoreFixture {
        connection: DbConnection,
        workspace_id: String,
        failure: String,
        decision: String,
        action: String,
        root: String,
    }

    impl CausalStoreFixture {
        fn new() -> Result<Self, String> {
            let connection = DbConnection::open_memory().map_err(|error| error.to_string())?;
            connection.migrate().map_err(|error| error.to_string())?;
            connection
                .execute_raw(
                    "CREATE TABLE IF NOT EXISTS causal_evidence (
                        id TEXT PRIMARY KEY,
                        workspace_id TEXT NOT NULL,
                        failure_id TEXT NOT NULL,
                        candidate_cause_id TEXT NOT NULL,
                        contribution_score REAL NOT NULL,
                        evidence_uris_json TEXT NOT NULL,
                        computed_at TEXT NOT NULL,
                        method TEXT NOT NULL
                    )",
                )
                .map_err(|error| error.to_string())?;
            let workspace_id = WorkspaceId::from_uuid(uuid::Uuid::from_u128(900)).to_string();
            connection
                .insert_workspace(
                    &workspace_id,
                    &CreateWorkspaceInput {
                        path: "/tmp/ee-causal-store-fixture".to_owned(),
                        name: Some("causal fixture".to_owned()),
                    },
                )
                .map_err(|error| error.to_string())?;

            let failure = MemoryId::from_uuid(uuid::Uuid::from_u128(901)).to_string();
            let decision = MemoryId::from_uuid(uuid::Uuid::from_u128(902)).to_string();
            let action = MemoryId::from_uuid(uuid::Uuid::from_u128(903)).to_string();
            let root = MemoryId::from_uuid(uuid::Uuid::from_u128(904)).to_string();
            for (id, kind, content) in [
                (
                    &failure,
                    "failure",
                    "release failed after stale causal rule",
                ),
                (
                    &decision,
                    "decision",
                    "decision skipped evidence validation",
                ),
                (&action, "action", "action changed release workflow"),
                (
                    &root,
                    "root-cause",
                    "root cause was missing evidence ledger",
                ),
            ] {
                connection
                    .insert_memory(id, &memory_input(&workspace_id, kind, content))
                    .map_err(|error| error.to_string())?;
            }

            Ok(Self {
                connection,
                workspace_id,
                failure,
                decision,
                action,
                root,
            })
        }

        fn insert_edge(
            &self,
            edge_id: &str,
            failure_id: &str,
            candidate_cause_id: &str,
            score: f64,
        ) -> Result<(), String> {
            self.insert_edge_with_method(
                edge_id,
                failure_id,
                candidate_cause_id,
                score,
                CausalEvidenceMethod::Manual,
            )
        }

        fn insert_edge_with_method(
            &self,
            edge_id: &str,
            failure_id: &str,
            candidate_cause_id: &str,
            score: f64,
            method: CausalEvidenceMethod,
        ) -> Result<(), String> {
            self.connection
                .execute_raw(&format!(
                    "INSERT INTO causal_evidence (id, workspace_id, failure_id, candidate_cause_id, contribution_score, evidence_uris_json, computed_at, method)
                     VALUES ('{edge_id}', '{}', '{failure_id}', '{candidate_cause_id}', {score}, '[\"agent-mail://causal-fixture/{edge_id}\"]', '2026-05-06T00:00:00Z', '{}')",
                    self.workspace_id,
                    method.as_str()
                ))
                .map_err(|error| error.to_string())
        }
    }

    fn memory_input(workspace_id: &str, kind: &str, content: &str) -> CreateMemoryInput {
        CreateMemoryInput {
            workspace_id: workspace_id.to_owned(),
            level: "episodic".to_owned(),
            kind: kind.to_owned(),
            content: content.to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.8,
            importance: 0.7,
            provenance_uri: Some("agent-mail://causal-fixture".to_owned()),
            trust_class: "agent_validated".to_owned(),
            trust_subclass: None,
            tags: Vec::new(),
            valid_from: None,
            valid_to: None,
        }
    }

    // ========================================================================
    // Estimate Tests (EE-452)
    // ========================================================================

    #[test]
    fn estimate_options_builder_works() {
        let opts = EstimateOptions::new()
            .with_artifact_id("art-001")
            .with_decision_id("dec-001")
            .with_method("replay")
            .with_assumptions()
            .with_confounders()
            .dry_run();

        assert_eq!(opts.artifact_id, Some("art-001".to_string()));
        assert_eq!(opts.decision_id, Some("dec-001".to_string()));
        assert_eq!(opts.method, Some("replay".to_string()));
        assert!(opts.include_assumptions);
        assert!(opts.include_confounders);
        assert!(opts.dry_run);
        assert!(opts.has_any_filter());
    }

    #[test]
    fn estimate_with_no_filters_returns_degradation() {
        let opts = EstimateOptions::new();
        let report = estimate_causal_uplift(&opts);

        assert!(report.is_empty());
        assert!(!report.degradations.is_empty());
        assert_eq!(report.degradations[0].code, "no_filters");
    }

    #[test]
    fn estimate_dry_run_returns_empty_but_applies_filters() {
        let opts = EstimateOptions::new().with_artifact_id("art-001").dry_run();
        let report = estimate_causal_uplift(&opts);

        assert!(report.is_empty());
        assert!(report.dry_run);
        assert!(
            report
                .filters_applied
                .iter()
                .any(|f| f.contains("artifact_id"))
        );
    }

    #[test]
    fn estimate_with_artifact_abstains_without_evidence_ledger() {
        let opts = EstimateOptions::new()
            .with_artifact_id("memory-001")
            .with_decision_id("decision-001");
        let report = estimate_causal_uplift(&opts);

        assert!(
            report.is_empty(),
            "Without evidence ledger, estimates should be empty"
        );
        assert!(
            report
                .degradations
                .iter()
                .any(|d| d.code == "causal_sample_underpowered"),
            "Should report causal_sample_underpowered degradation"
        );
        assert!(
            !report.filters_applied.is_empty(),
            "Filters should still be recorded"
        );
    }

    #[test]
    fn estimate_method_is_recorded_even_without_evidence() {
        let naive = EstimateOptions::new()
            .with_artifact_id("art-001")
            .with_method("naive");
        let experiment = EstimateOptions::new()
            .with_artifact_id("art-001")
            .with_method("experiment");

        let naive_report = estimate_causal_uplift(&naive);
        let exp_report = estimate_causal_uplift(&experiment);

        assert_eq!(naive_report.method_used, "naive");
        assert_eq!(exp_report.method_used, "experiment");
        assert!(
            naive_report.estimates.is_empty(),
            "No estimates without evidence"
        );
        assert!(
            exp_report.estimates.is_empty(),
            "No estimates without evidence"
        );
    }

    #[test]
    fn estimate_includes_assumptions_when_requested() {
        let opts = EstimateOptions::new()
            .with_artifact_id("art-001")
            .with_assumptions();
        let report = estimate_causal_uplift(&opts);

        assert!(!report.assumptions.is_empty());
        assert!(report.assumptions.iter().any(|a| a.code == "stable_unit"));
    }

    #[test]
    fn estimate_confounders_unavailable_without_evidence_ledger() {
        let opts = EstimateOptions::new()
            .with_artifact_id("art-001")
            .with_confounders();
        let report = estimate_causal_uplift(&opts);

        assert!(
            !report.has_confounders(),
            "No confounders without evidence ledger"
        );
        assert!(
            report
                .degradations
                .iter()
                .any(|d| d.code == "causal_confounders_unavailable"),
            "Should report causal_confounders_unavailable degradation"
        );
    }

    #[test]
    fn estimate_report_json_has_correct_schema() {
        let opts = EstimateOptions::new()
            .with_artifact_id("art-001")
            .with_decision_id("dec-001");
        let report = estimate_causal_uplift(&opts);
        let json = report.data_json();

        assert_eq!(json["schema"], CAUSAL_ESTIMATE_SCHEMA_V1);
        assert_eq!(json["command"], "causal estimate");
        assert!(json["estimates"].is_array());
        assert!(json["summary"]["totalEstimates"].is_number());
    }

    #[test]
    fn estimate_human_summary_is_readable() {
        let opts = EstimateOptions::new()
            .with_artifact_id("mem-001")
            .with_method("replay")
            .with_assumptions();
        let report = estimate_causal_uplift(&opts);
        let summary = report.human_summary();

        assert!(summary.contains("Causal Estimate Report"));
        assert!(summary.contains("Method:"));
        assert!(summary.contains("Estimates found:"));
    }

    #[test]
    fn confidence_state_from_evidence_strength_maps_correctly() {
        use crate::models::causal::CausalEvidenceStrength;

        assert_eq!(
            ConfidenceState::from_evidence_strength(CausalEvidenceStrength::ExperimentSupported),
            ConfidenceState::High
        );
        assert_eq!(
            ConfidenceState::from_evidence_strength(CausalEvidenceStrength::ReplaySupported),
            ConfidenceState::Medium
        );
        assert_eq!(
            ConfidenceState::from_evidence_strength(CausalEvidenceStrength::Correlational),
            ConfidenceState::Low
        );
        assert_eq!(
            ConfidenceState::from_evidence_strength(CausalEvidenceStrength::ExposureOnly),
            ConfidenceState::Insufficient
        );
        assert_eq!(
            ConfidenceState::from_evidence_strength(CausalEvidenceStrength::Rejected),
            ConfidenceState::Rejected
        );
    }

    #[test]
    fn compare_options_builder_works() {
        let options = CompareOptions::new()
            .with_artifact_id("mem-001")
            .with_decision_id("dec-001")
            .with_fixture_replay_id("fixture-001")
            .with_shadow_run_id("shadow-001")
            .with_counterfactual_episode_id("counterfactual-001")
            .with_experiment_id("exp-001")
            .with_method("replay")
            .dry_run();

        assert_eq!(options.artifact_id, Some("mem-001".to_string()));
        assert_eq!(options.decision_id, Some("dec-001".to_string()));
        assert_eq!(options.fixture_replay_id, Some("fixture-001".to_string()));
        assert_eq!(options.shadow_run_id, Some("shadow-001".to_string()));
        assert_eq!(
            options.counterfactual_episode_id,
            Some("counterfactual-001".to_string())
        );
        assert_eq!(options.experiment_id, Some("exp-001".to_string()));
        assert_eq!(options.method, Some("replay".to_string()));
        assert!(options.dry_run);
    }

    #[test]
    fn compare_without_filters_returns_degradation() {
        let report = compare_causal_evidence(&CompareOptions::new());
        assert!(report.is_empty());
        assert!(
            report
                .degradations
                .iter()
                .any(|degradation| degradation.code == "no_filters")
        );
    }

    #[test]
    fn compare_without_sources_returns_no_sources_degradation() {
        let report = compare_causal_evidence(&CompareOptions::new().with_artifact_id("mem-001"));
        assert!(report.is_empty());
        assert!(
            report
                .degradations
                .iter()
                .any(|degradation| degradation.code == "no_sources")
        );
    }

    #[test]
    fn compare_with_sources_abstains_without_evidence_ledger() {
        let report = compare_causal_evidence(
            &CompareOptions::new()
                .with_artifact_id("mem-001")
                .with_fixture_replay_id("fixture-001")
                .with_shadow_run_id("shadow-001")
                .with_counterfactual_episode_id("counterfactual-001")
                .with_experiment_id("exp-001")
                .with_method("experiment"),
        );

        assert_eq!(report.schema, CAUSAL_COMPARE_SCHEMA_V1);
        assert_eq!(report.method_used, "experiment");
        assert!(
            report.comparisons.is_empty(),
            "No comparisons without evidence ledger"
        );
        assert!(
            report
                .degradations
                .iter()
                .any(|d| d.code == "causal_comparison_evidence_unavailable"),
            "Should report causal_comparison_evidence_unavailable degradation"
        );
    }

    #[test]
    fn compare_dry_run_returns_empty_output() {
        let report = compare_causal_evidence(
            &CompareOptions::new()
                .with_fixture_replay_id("fixture-001")
                .with_method("matching")
                .dry_run(),
        );

        assert!(report.is_empty());
        assert!(report.dry_run);
        assert!(report.degradations.is_empty());
    }

    #[test]
    fn compare_unknown_method_degrades_to_naive() {
        let report = compare_causal_evidence(
            &CompareOptions::new()
                .with_fixture_replay_id("fixture-001")
                .with_method("mystery"),
        );

        assert_eq!(report.method_used, "naive");
        assert!(
            report
                .degradations
                .iter()
                .any(|degradation| degradation.code == "unknown_method")
        );
    }

    #[test]
    fn compare_report_json_has_correct_schema() {
        let report = compare_causal_evidence(
            &CompareOptions::new()
                .with_fixture_replay_id("fixture-001")
                .with_shadow_run_id("shadow-001"),
        );
        let json = report.data_json();

        assert_eq!(json["schema"], CAUSAL_COMPARE_SCHEMA_V1);
        assert_eq!(json["command"], "causal compare");
        assert!(json["comparisons"].is_array());
        assert!(json["summary"]["totalComparisons"].is_number());
    }

    #[test]
    fn promote_plan_options_builder_works() {
        let options = PromotePlanOptions::new()
            .with_artifact_id("mem-001")
            .with_decision_id("dec-001")
            .with_estimate_id("est-001")
            .with_action(PromotionAction::Demote)
            .with_method("matching")
            .with_minimum_uplift(0.12)
            .with_revalidation()
            .with_narrower_routing()
            .with_experiment_proposals()
            .dry_run();

        assert_eq!(options.artifact_id, Some("mem-001".to_string()));
        assert_eq!(options.decision_id, Some("dec-001".to_string()));
        assert_eq!(options.estimate_id, Some("est-001".to_string()));
        assert_eq!(options.action, Some(PromotionAction::Demote));
        assert_eq!(options.method, Some("matching".to_string()));
        assert_eq!(options.minimum_uplift, 0.12);
        assert!(options.include_revalidation);
        assert!(options.include_narrower_routing);
        assert!(options.include_experiment_proposals);
        assert!(options.dry_run);
    }

    #[test]
    fn promote_plan_without_filters_returns_degradation() {
        let report = promote_causal_plan(&PromotePlanOptions::new().dry_run());
        assert!(report.is_empty());
        assert!(!report.degradations.is_empty());
        assert_eq!(report.degradations[0].code, "no_filters");
    }

    #[test]
    fn promote_plan_without_store_does_not_fabricate_dry_run_plan() {
        let report = promote_causal_plan(
            &PromotePlanOptions::new()
                .with_artifact_id("mem-001")
                .dry_run(),
        );

        assert!(report.is_empty());
        assert!(report.dry_run);
        assert!(report.curation_candidate_ids.is_empty());
        assert!(
            report
                .degradations
                .iter()
                .any(|degradation| degradation.code == "causal_sample_underpowered")
        );
        assert!(!report.data_json().to_string().contains("artifact-from-"));
    }

    #[test]
    fn promote_plan_unknown_method_degrades_to_naive() {
        let report = promote_causal_plan(
            &PromotePlanOptions::new()
                .with_artifact_id("mem-001")
                .with_method("mystery")
                .dry_run(),
        );

        assert_eq!(report.method_used, "naive");
        assert!(
            report
                .degradations
                .iter()
                .any(|degradation| degradation.code == "unknown_method")
        );
    }

    #[test]
    fn promote_plan_action_override_recorded_but_not_actionable() {
        let report = promote_causal_plan(
            &PromotePlanOptions::new()
                .with_artifact_id("mem-001")
                .with_action(PromotionAction::Archive)
                .dry_run(),
        );

        assert!(report.is_empty());
        assert!(
            report
                .degradations
                .iter()
                .any(|d| d.code == "action_override_not_actionable"),
            "Should report action_override_not_actionable degradation"
        );
    }

    #[test]
    fn promote_plan_report_json_has_correct_schema() {
        let report = promote_causal_plan(
            &PromotePlanOptions::new()
                .with_artifact_id("mem-001")
                .with_method("experiment")
                .with_revalidation()
                .with_experiment_proposals()
                .dry_run(),
        );
        let json = report.data_json();

        assert_eq!(json["schema"], CAUSAL_PROMOTE_PLAN_SCHEMA_V1);
        assert_eq!(json["command"], "causal promote-plan");
        assert!(json["plans"].is_array());
        assert!(json["recommendations"]["revalidation"].is_array());
        assert_eq!(
            json["downstreamEffects"]["schema"],
            CAUSAL_DOWNSTREAM_EFFECTS_SCHEMA_V1
        );
        assert!(json["summary"]["totalPlans"].is_number());
    }

    #[test]
    fn promote_plan_projects_review_only_effects_without_evidence() {
        let report = promote_causal_plan(
            &PromotePlanOptions::new()
                .with_artifact_id("mem-001")
                .with_method("experiment")
                .dry_run(),
        );
        let downstream = &report.downstream_effects;

        assert!(report.is_empty());
        assert_eq!(downstream.schema, CAUSAL_DOWNSTREAM_EFFECTS_SCHEMA_V1);
        // Without evidence, no priority delta
        assert_eq!(downstream.economy_score.priority_delta, 0);
        // Review-only queue action
        assert_eq!(downstream.learning_agenda.queue_action, "review_only");
        // No profile change without evidence
        assert_eq!(downstream.preflight_routing.profile, "unchanged");
        // Evidence required before status changes
        assert_eq!(
            downstream.procedure_verification.status,
            "evidence_required"
        );
        assert!(downstream.procedure_verification.requires_revalidation);
        // Mutation mode reflects review-only
        assert_eq!(downstream.audit.mutation_mode, "dry_run_review_only");
        assert!(!downstream.audit.raw_evidence_replaced);
        assert!(!downstream.audit.silent_mutation);
    }

    #[test]
    fn promote_plan_human_summary_is_readable() {
        let report = promote_causal_plan(
            &PromotePlanOptions::new()
                .with_artifact_id("mem-001")
                .dry_run(),
        );
        let summary = report.human_summary();

        assert!(summary.contains("Causal Promotion Plan"));
        assert!(summary.contains("Plans generated: 0"));
    }
}
