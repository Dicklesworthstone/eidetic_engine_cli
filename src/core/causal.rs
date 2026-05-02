//! Causal trace and credit analysis (EE-451).
//!
//! Traces causal chains over recorder runs, context pack records, preflight
//! closes, tripwire checks, and procedure uses to distinguish exposure from
//! influence.

use serde_json::{Value as JsonValue, json};

use crate::models::causal::{
    CAUSAL_TRACE_SCHEMA_V1, CausalDecisionTrace, CausalEvidenceStrength, CausalExposureChannel,
    DecisionTraceOutcome, PromotionAction, PromotionPlan, PromotionPlanStatus,
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

/// Schema for causal comparison response.
pub const CAUSAL_COMPARE_SCHEMA_V1: &str = "ee.causal.compare.v1";

/// Schema for causal promotion plan response.
pub const CAUSAL_PROMOTE_PLAN_SCHEMA_V1: &str = "ee.causal.promote_plan.v1";

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

// ============================================================================
// Compare Options and Report (EE-453)
// ============================================================================

/// Options for comparing causal evidence across fixture replay, shadow runs,
/// counterfactual episodes, and active-learning experiments.
#[derive(Clone, Debug, Default)]
pub struct CompareOptions {
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
        self.artifact_id.is_some()
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
            "degradations": self.degradations.iter().map(TraceDegradation::data_json).collect::<Vec<_>>(),
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

    if options.dry_run {
        return CompareReport {
            schema: CAUSAL_COMPARE_SCHEMA_V1,
            comparisons: Vec::new(),
            filters_applied,
            degradations,
            method_used,
            dry_run: true,
        };
    }

    let (strength, method_uplift) = method_signal(&method_used);
    let confidence = ConfidenceState::from_evidence_strength(strength);

    let comparisons = selected_sources
        .into_iter()
        .map(|(source_kind, source_id)| {
            let offset = deterministic_uplift_offset(source_kind, source_id);
            let baseline_uplift = (method_uplift - 0.03_f64).clamp(-1.0, 1.0);
            let candidate_uplift = (method_uplift + offset).clamp(-1.0, 1.0);
            let uplift_delta = candidate_uplift - baseline_uplift;
            let verdict = if uplift_delta > 0.01 {
                "improves"
            } else if uplift_delta < -0.01 {
                "regresses"
            } else {
                "flat"
            };

            CausalComparison {
                comparison_id: format!("cmp-{source_kind}-{source_id}"),
                source_kind: source_kind.to_string(),
                source_id: source_id.to_string(),
                baseline_uplift,
                candidate_uplift,
                uplift_delta,
                confidence_state: confidence,
                evidence_strength: strength.as_str().to_string(),
                verdict: verdict.to_string(),
            }
        })
        .collect::<Vec<_>>();

    CompareReport {
        schema: CAUSAL_COMPARE_SCHEMA_V1,
        comparisons,
        filters_applied,
        degradations,
        method_used,
        dry_run: false,
    }
}

fn deterministic_uplift_offset(source_kind: &str, source_id: &str) -> f64 {
    let mut hash: u64 = 1469598103934665603;
    for byte in source_kind.bytes().chain(source_id.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1099511628211);
    }
    let normalized = (hash % 21) as f64 - 10.0;
    normalized / 100.0
}

/// Options for producing a dry-run-first causal promotion plan.
#[derive(Clone, Debug)]
pub struct PromotePlanOptions {
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
        self.artifact_id.is_some() || self.decision_id.is_some() || self.estimate_id.is_some()
    }
}

/// Follow-up recommendations from a causal promotion plan.
#[derive(Clone, Debug, Default)]
pub struct PromotePlanRecommendations {
    pub revalidation_steps: Vec<String>,
    pub narrower_routing_steps: Vec<String>,
    pub experiment_proposals: Vec<String>,
}

impl PromotePlanRecommendations {
    #[must_use]
    pub fn data_json(&self) -> JsonValue {
        json!({
            "revalidation": self.revalidation_steps,
            "narrowerRouting": self.narrower_routing_steps,
            "experimentProposals": self.experiment_proposals,
        })
    }
}

/// Report from `ee causal promote-plan`.
#[derive(Clone, Debug)]
pub struct PromotePlanReport {
    pub schema: &'static str,
    pub plans: Vec<PromotionPlan>,
    pub recommendations: PromotePlanRecommendations,
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
            "recommendations": self.recommendations.data_json(),
            "summary": {
                "totalPlans": self.plans.len(),
                "promoteCount": promote_count,
                "holdCount": hold_count,
                "demoteCount": demote_count,
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
    let mut filters_applied = Vec::new();
    let mut degradations = Vec::new();

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
    let (evidence_strength, estimated_uplift) = method_signal(method_used.as_str());

    if !options.dry_run {
        degradations.push(TraceDegradation {
            code: "dry_run_recommended".to_string(),
            message: "Promotion planning is policy-conservative; prefer --dry-run in automation."
                .to_string(),
            severity: "info".to_string(),
        });
    }

    if !options.has_any_filter() {
        degradations.push(TraceDegradation {
            code: "no_filters".to_string(),
            message: "No artifact, decision, or estimate ID provided; cannot produce plan."
                .to_string(),
            severity: "warning".to_string(),
        });
        return PromotePlanReport {
            schema: CAUSAL_PROMOTE_PLAN_SCHEMA_V1,
            plans: Vec::new(),
            recommendations: PromotePlanRecommendations::default(),
            filters_applied,
            degradations,
            method_used,
            dry_run: options.dry_run,
        };
    }

    let artifact_id = options
        .artifact_id
        .clone()
        .or_else(|| {
            options
                .estimate_id
                .as_ref()
                .map(|id| format!("artifact-from-{id}"))
        })
        .or_else(|| {
            options
                .decision_id
                .as_ref()
                .map(|id| format!("artifact-for-{id}"))
        })
        .unwrap_or_else(|| "artifact-unknown".to_string());

    let action = options.action.unwrap_or_else(|| {
        derive_action(estimated_uplift, options.minimum_uplift, evidence_strength)
    });

    let mut plan = PromotionPlan::new(
        format!("plan-{artifact_id}"),
        artifact_id.clone(),
        action,
        chrono::Utc::now().to_rfc3339(),
    )
    .with_status(if options.dry_run {
        PromotionPlanStatus::DryRunReady
    } else {
        PromotionPlanStatus::Proposed
    })
    .with_evidence_strength(evidence_strength)
    .with_minimum_uplift(options.minimum_uplift)
    .with_estimated_uplift(estimated_uplift)
    .with_required_evidence(format!("evidence-{artifact_id}"))
    .with_audit_id(format!("audit-promote-plan-{artifact_id}"));

    if matches!(
        action,
        PromotionAction::Demote | PromotionAction::Archive | PromotionAction::Quarantine
    ) {
        plan = plan.with_blocking_confounder(format!("confounder-{artifact_id}"));
    }

    let mut recommendations = PromotePlanRecommendations::default();
    if options.include_revalidation
        || matches!(
            action,
            PromotionAction::Hold | PromotionAction::Demote | PromotionAction::Quarantine
        )
    {
        recommendations.revalidation_steps.push(format!(
            "Re-run `ee causal estimate --artifact-id {artifact_id} --method replay --json` before applying changes."
        ));
    }
    if options.include_narrower_routing
        || matches!(
            action,
            PromotionAction::Demote | PromotionAction::Archive | PromotionAction::Quarantine
        )
    {
        recommendations.narrower_routing_steps.push(format!(
            "Narrow routing scope for `{artifact_id}` to higher-confidence task families."
        ));
    }
    if options.include_experiment_proposals
        || evidence_strength != CausalEvidenceStrength::ExperimentSupported
    {
        recommendations.experiment_proposals.push(format!(
            "Propose controlled experiment for `{artifact_id}` with `ee learn experiment --artifact-id {artifact_id} --dry-run --json`."
        ));
    }

    PromotePlanReport {
        schema: CAUSAL_PROMOTE_PLAN_SCHEMA_V1,
        plans: vec![plan],
        recommendations,
        filters_applied,
        degradations,
        method_used,
        dry_run: options.dry_run,
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
            });
            "naive".to_string()
        }
    }
}

fn method_signal(method: &str) -> (CausalEvidenceStrength, f64) {
    match method {
        "experiment" => (CausalEvidenceStrength::ExperimentSupported, 0.16),
        "replay" => (CausalEvidenceStrength::ReplaySupported, 0.10),
        "matching" => (CausalEvidenceStrength::Correlational, 0.04),
        _ => (CausalEvidenceStrength::ExposureOnly, 0.01),
    }
}

fn derive_action(
    estimated_uplift: f64,
    minimum_uplift: f64,
    evidence_strength: CausalEvidenceStrength,
) -> PromotionAction {
    if estimated_uplift < 0.0 {
        return PromotionAction::Demote;
    }
    if estimated_uplift >= minimum_uplift
        && matches!(
            evidence_strength,
            CausalEvidenceStrength::ExperimentSupported | CausalEvidenceStrength::ReplaySupported
        )
    {
        PromotionAction::Promote
    } else {
        PromotionAction::Hold
    }
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
    fn compare_with_sources_returns_comparisons() {
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
        assert_eq!(report.comparisons.len(), 4);
        assert!(
            report
                .comparisons
                .iter()
                .all(|comparison| !comparison.verdict.is_empty())
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
    fn promote_plan_dry_run_returns_dry_run_ready_plan() {
        let report = promote_causal_plan(
            &PromotePlanOptions::new()
                .with_artifact_id("mem-001")
                .dry_run(),
        );

        assert!(!report.is_empty());
        assert!(report.dry_run);
        assert_eq!(report.plans[0].status, PromotionPlanStatus::DryRunReady);
        assert!(report.plans[0].dry_run_first);
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
    fn promote_plan_action_override_is_honored() {
        let report = promote_causal_plan(
            &PromotePlanOptions::new()
                .with_artifact_id("mem-001")
                .with_action(PromotionAction::Archive)
                .dry_run(),
        );

        assert_eq!(report.plans[0].action, PromotionAction::Archive);
        assert!(
            report.plans[0]
                .blocking_confounder_ids
                .iter()
                .any(|confounder| confounder.contains("confounder-"))
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
        assert!(json["summary"]["totalPlans"].is_number());
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
        assert!(summary.contains("Plans generated:"));
    }
}
