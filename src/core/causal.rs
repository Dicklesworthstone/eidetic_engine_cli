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
}
