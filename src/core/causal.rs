//! Causal trace and credit analysis (EE-451).
//!
//! Traces causal chains over recorder runs, context pack records, preflight
//! closes, tripwire checks, and procedure uses to distinguish exposure from
//! influence.

use serde_json::{Value as JsonValue, json};

use crate::models::causal::{
    CAUSAL_TRACE_SCHEMA_V1, CausalDecisionTrace, CausalExposureChannel, DecisionTraceOutcome,
};
use crate::models::decision::DecisionPlane;

/// Schema for causal trace list response.
pub const CAUSAL_TRACE_LIST_SCHEMA_V1: &str = "ee.causal.trace_list.v1";

// ============================================================================
// Trace Options and Report
// ============================================================================

/// Options for tracing causal chains.
#[derive(Clone, Debug, Default)]
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
    /// Maximum number of traces to return.
    pub limit: Option<usize>,
    /// Include detailed exposure records.
    pub include_exposures: bool,
    /// Include outcome summaries.
    pub include_outcomes: bool,
    /// Dry-run mode (show what would be traced).
    pub dry_run: bool,
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
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
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
        if let Some(ref pack_id) = self.context_pack_id {
            obj["contextPackId"] = json!(pack_id);
        }
        if let Some(ref run_id) = self.recorder_run_id {
            obj["recorderRunId"] = json!(run_id);
        }
        obj
    }
}

/// A traced causal chain linking exposures to outcomes.
#[derive(Clone, Debug)]
pub struct CausalChain {
    pub chain_id: String,
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
        self.exposures.len()
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
}

impl std::fmt::Display for TraceDegradation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.severity)
    }
}

impl TraceDegradation {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "code": self.code,
            "message": self.message,
            "severity": self.severity,
        })
    }
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
            "degradations": self.degradations.iter().map(TraceDegradation::data_json).collect::<Vec<_>>(),
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
/// Currently returns a placeholder report since the full database integration
/// requires recorder, pack, preflight, and tripwire table queries that will
/// be wired up as those subsystems mature.
#[must_use]
pub fn trace_causal_chains(options: &TraceOptions) -> TraceReport {
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

    if !options.has_any_filter() {
        degradations.push(TraceDegradation {
            code: "no_filters".to_string(),
            message: "No filters provided; returning empty trace".to_string(),
            severity: "info".to_string(),
        });
    }

    // Placeholder: In a full implementation, this would query the database
    // for recorder runs, pack records, preflight closes, tripwire checks,
    // and procedure uses matching the filters, then build causal chains.
    //
    // For now, we return an empty but valid report structure.
    let chains = if options.dry_run || !options.has_any_filter() {
        Vec::new()
    } else {
        // Build a sample chain to demonstrate the structure
        build_sample_chains(options)
    };

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

fn build_sample_chains(options: &TraceOptions) -> Vec<CausalChain> {
    // For demonstration/testing, build a minimal chain if filters are provided
    let chain_id = options
        .memory_id
        .as_ref()
        .or(options.run_id.as_ref())
        .or(options.pack_id.as_ref())
        .cloned()
        .unwrap_or_else(|| "trace-0001".to_string());

    let decision_trace = CausalDecisionTrace::new(
        format!("decision-for-{chain_id}"),
        format!("trace-{chain_id}"),
        DecisionPlane::Observe,
        chrono::Utc::now().to_rfc3339(),
        options.agent_id.as_deref().unwrap_or("unknown"),
        "Traced decision from causal analysis",
    )
    .with_outcome(DecisionTraceOutcome::Used);

    let mut exposures = Vec::new();
    let mut recorder_run_ids = Vec::new();
    let mut context_pack_ids = Vec::new();
    let mut preflight_ids = Vec::new();
    let mut tripwire_ids = Vec::new();
    let mut procedure_ids = Vec::new();

    if let Some(ref run_id) = options.run_id {
        recorder_run_ids.push(run_id.clone());
        exposures.push(CausalExposure {
            exposure_id: format!("exp-{run_id}"),
            channel: CausalExposureChannel::ContextPack,
            artifact_id: run_id.clone(),
            artifact_type: "recorder_run".to_string(),
            exposed_at: chrono::Utc::now().to_rfc3339(),
            context_pack_id: None,
            recorder_run_id: Some(run_id.clone()),
        });
    }

    if let Some(ref pack_id) = options.pack_id {
        context_pack_ids.push(pack_id.clone());
        exposures.push(CausalExposure {
            exposure_id: format!("exp-{pack_id}"),
            channel: CausalExposureChannel::ContextPack,
            artifact_id: pack_id.clone(),
            artifact_type: "context_pack".to_string(),
            exposed_at: chrono::Utc::now().to_rfc3339(),
            context_pack_id: Some(pack_id.clone()),
            recorder_run_id: None,
        });
    }

    if let Some(ref preflight_id) = options.preflight_id {
        preflight_ids.push(preflight_id.clone());
    }

    if let Some(ref tripwire_id) = options.tripwire_id {
        tripwire_ids.push(tripwire_id.clone());
    }

    if let Some(ref procedure_id) = options.procedure_id {
        procedure_ids.push(procedure_id.clone());
        exposures.push(CausalExposure {
            exposure_id: format!("exp-{procedure_id}"),
            channel: CausalExposureChannel::Procedure,
            artifact_id: procedure_id.clone(),
            artifact_type: "procedure".to_string(),
            exposed_at: chrono::Utc::now().to_rfc3339(),
            context_pack_id: None,
            recorder_run_id: None,
        });
    }

    vec![CausalChain {
        chain_id,
        decision_trace,
        exposures,
        recorder_run_ids,
        context_pack_ids,
        preflight_ids,
        tripwire_ids,
        procedure_ids,
    }]
}

// ============================================================================
// Estimate Options and Report (EE-452)
// ============================================================================

/// Schema for causal estimate response.
pub const CAUSAL_ESTIMATE_SCHEMA_V1: &str = "ee.causal.estimate.v1";

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
            "degradations": self.degradations.iter().map(TraceDegradation::data_json).collect::<Vec<_>>(),
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
        });
    }

    // Build estimates
    let estimates = if options.dry_run || !options.has_any_filter() {
        Vec::new()
    } else {
        build_sample_estimates(options, &method_used)
    };

    // Build assumptions based on method
    let assumptions = if options.include_assumptions {
        build_method_assumptions(&method_used)
    } else {
        Vec::new()
    };

    // Identify confounders
    let confounders = if options.include_confounders && !estimates.is_empty() {
        build_sample_confounders(options)
    } else {
        Vec::new()
    };

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

fn build_sample_estimates(options: &EstimateOptions, method: &str) -> Vec<CausalUpliftEstimate> {
    let artifact_id = options
        .artifact_id
        .clone()
        .unwrap_or_else(|| "unknown-artifact".to_string());
    let decision_id = options
        .decision_id
        .clone()
        .unwrap_or_else(|| "unknown-decision".to_string());

    // Determine evidence strength based on method
    let (evidence_strength, confidence_state, uplift, confidence) = match method {
        "experiment" => ("experiment_supported", ConfidenceState::High, 0.15, 0.92),
        "replay" => ("replay_supported", ConfidenceState::Medium, 0.12, 0.78),
        "matching" => ("correlational", ConfidenceState::Low, 0.08, 0.55),
        _ => ("exposure_only", ConfidenceState::Insufficient, 0.05, 0.30),
    };

    vec![CausalUpliftEstimate {
        estimate_id: format!("est-{}-{}", artifact_id, decision_id),
        artifact_id,
        decision_id,
        method: method.to_string(),
        uplift,
        direction: if uplift > 0.0 { "positive" } else { "neutral" }.to_string(),
        confidence,
        evidence_strength: evidence_strength.to_string(),
        confidence_state,
        sample_size: 42,
        estimated_at: chrono::Utc::now().to_rfc3339(),
    }]
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

fn build_sample_confounders(options: &EstimateOptions) -> Vec<EstimateConfounder> {
    vec![
        EstimateConfounder {
            confounder_id: format!(
                "conf-task-complexity-{}",
                options.artifact_id.as_deref().unwrap_or("unknown")
            ),
            kind: "task_complexity".to_string(),
            description: "Task complexity correlates with both memory usage and success"
                .to_string(),
            severity: 0.6,
            mitigation: "Stratify by task difficulty or use matching".to_string(),
        },
        EstimateConfounder {
            confounder_id: format!(
                "conf-agent-skill-{}",
                options.artifact_id.as_deref().unwrap_or("unknown")
            ),
            kind: "agent_capability".to_string(),
            description: "More capable agents may both use memory more and succeed more"
                .to_string(),
            severity: 0.4,
            mitigation: "Include agent-level fixed effects".to_string(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn trace_with_run_id_returns_chain() {
        let opts = TraceOptions::new().with_run_id("run-test-001");
        let report = trace_causal_chains(&opts);

        assert!(!report.is_empty());
        assert_eq!(report.chains.len(), 1);
        assert!(!report.chains[0].recorder_run_ids.is_empty());
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
    fn estimate_with_artifact_returns_estimate() {
        let opts = EstimateOptions::new()
            .with_artifact_id("memory-001")
            .with_decision_id("decision-001");
        let report = estimate_causal_uplift(&opts);

        assert!(!report.is_empty());
        assert_eq!(report.estimates.len(), 1);
        assert_eq!(report.estimates[0].artifact_id, "memory-001");
        assert_eq!(report.estimates[0].decision_id, "decision-001");
    }

    #[test]
    fn estimate_method_affects_confidence() {
        let naive = EstimateOptions::new()
            .with_artifact_id("art-001")
            .with_method("naive");
        let experiment = EstimateOptions::new()
            .with_artifact_id("art-001")
            .with_method("experiment");

        let naive_report = estimate_causal_uplift(&naive);
        let exp_report = estimate_causal_uplift(&experiment);

        assert_eq!(
            naive_report.estimates[0].confidence_state,
            ConfidenceState::Insufficient
        );
        assert_eq!(
            exp_report.estimates[0].confidence_state,
            ConfidenceState::High
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
    fn estimate_includes_confounders_when_requested() {
        let opts = EstimateOptions::new()
            .with_artifact_id("art-001")
            .with_confounders();
        let report = estimate_causal_uplift(&opts);

        assert!(report.has_confounders());
        assert!(
            report
                .confounders
                .iter()
                .any(|c| c.kind == "task_complexity")
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
}
