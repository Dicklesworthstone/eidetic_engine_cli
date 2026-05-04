//! Optional science analytics module (EE-171).
//!
//! This module provides offline statistical metrics, clustering diagnostics,
//! and deterministic diagram exports for evaluation and diagnostics. It is
//! gated behind the `science-analytics` feature flag to avoid bloating the
//! default agent loop with heavy dependencies.
//!
//! When the feature is disabled, the module exposes only stub types and
//! `is_available()` returning `false`. This allows callers to degrade
//! gracefully without compile-time feature checks everywhere.
//!
//! # Feature Flag
//!
//! Enable with `--features science-analytics` or add to your Cargo.toml:
//!
//! ```toml
//! [dependencies]
//! ee = { version = "0.1", features = ["science-analytics"] }
//! ```
//!
//! # Design Notes
//!
//! - All metrics must be deterministic given the same inputs.
//! - No network calls or LLM API usage.
//! - Diagram exports use text-based formats (Mermaid, DOT) for portability.
//! - Heavy computations should respect budget/cancellation via Asupersync.

/// Science analytics subsystem identifier.
pub const SUBSYSTEM: &str = "science";

/// Check if science analytics is available at runtime.
///
/// Returns `true` when the `science-analytics` feature is enabled,
/// `false` otherwise. This allows callers to degrade gracefully.
#[must_use]
pub const fn is_available() -> bool {
    cfg!(feature = "science-analytics")
}

/// Science analytics availability status for JSON output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScienceStatus {
    /// Feature enabled and ready.
    Available,
    /// Feature not compiled in.
    NotCompiled,
    /// Feature enabled but backend unavailable.
    BackendUnavailable,
}

impl ScienceStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::NotCompiled => "not_compiled",
            Self::BackendUnavailable => "backend_unavailable",
        }
    }

    #[must_use]
    pub const fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Get the current science analytics status.
#[must_use]
pub const fn status() -> ScienceStatus {
    if cfg!(feature = "science-analytics") {
        ScienceStatus::Available
    } else {
        ScienceStatus::NotCompiled
    }
}

/// Degradation code for science analytics unavailable.
pub const DEGRADATION_CODE_NOT_COMPILED: &str = "science_not_compiled";

/// Degradation code for science backend errors.
pub const DEGRADATION_CODE_BACKEND_UNAVAILABLE: &str = "science_backend_unavailable";

/// Degradation code for input too large for science analysis.
pub const DEGRADATION_CODE_INPUT_TOO_LARGE: &str = "science_input_too_large";

/// Degradation code for science budget exceeded.
pub const DEGRADATION_CODE_BUDGET_EXCEEDED: &str = "science_budget_exceeded";

/// Stable schema for `ee analyze science-status --json` payloads.
pub const SCIENCE_STATUS_SCHEMA_V1: &str = "ee.science.status.v1";

/// Stable schema for clustering diagnostics over consolidation candidates.
pub const CLUSTERING_DIAGNOSTICS_SCHEMA_V1: &str = "ee.science.clustering_diagnostics.v1";

/// Stable schema for clustering analysis reports.
pub const CLUSTERING_ANALYSIS_SCHEMA_V1: &str = "ee.science.clustering_analysis.v1";

/// Stable schema for drift analysis reports over frozen evaluation snapshots.
pub const DRIFT_ANALYSIS_SCHEMA_V1: &str = "ee.science.drift_analysis.v1";

/// Degradation code for drift analysis unavailable.
pub const DEGRADATION_CODE_DRIFT_UNAVAILABLE: &str = "drift_analysis_unavailable";

/// Degradation code for missing evaluation snapshots.
pub const DEGRADATION_CODE_NO_SNAPSHOTS: &str = "drift_no_evaluation_snapshots";

/// Degradation code for no candidates available for clustering.
pub const DEGRADATION_CODE_NO_CANDIDATES: &str = "clustering_no_candidates";

/// Degradation code for missing candidate embeddings.
pub const DEGRADATION_CODE_NO_EMBEDDINGS: &str = "clustering_no_embeddings";

/// Canonical command path for science status diagnostics.
pub const SCIENCE_STATUS_COMMAND: &str = "analyze science-status";

/// Canonical command path for drift analysis.
pub const DRIFT_ANALYSIS_COMMAND: &str = "analyze drift";

/// Canonical command path for clustering analysis.
pub const CLUSTERING_ANALYSIS_COMMAND: &str = "analyze clustering";

/// Feature flag that enables science analytics.
pub const SCIENCE_ANALYTICS_FEATURE: &str = "science-analytics";

/// Stable science analytics status report for machine-readable diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScienceStatusReport {
    /// Versioned schema for the report body.
    pub schema: &'static str,
    /// Canonical command path that will emit this report.
    pub command: &'static str,
    /// Subsystem covered by the report.
    pub subsystem: &'static str,
    /// Overall availability state.
    pub status: ScienceStatus,
    /// Whether science analytics can run in this binary.
    pub available: bool,
    /// Compile-time feature status.
    pub feature: ScienceFeatureStatus,
    /// Deterministically ordered capability inventory.
    pub capabilities: Vec<ScienceCapabilityStatus>,
    /// Current degradations, if any.
    pub degradations: Vec<ScienceDegradation>,
    /// Agent-facing next actions.
    pub next_actions: Vec<&'static str>,
}

impl ScienceStatusReport {
    /// Build the current status report for this binary.
    #[must_use]
    pub fn current() -> Self {
        let status = status();
        let available = status.is_available();
        Self {
            schema: SCIENCE_STATUS_SCHEMA_V1,
            command: SCIENCE_STATUS_COMMAND,
            subsystem: SUBSYSTEM,
            status,
            available,
            feature: ScienceFeatureStatus::current(),
            capabilities: science_capabilities(available),
            degradations: science_degradations(status),
            next_actions: science_next_actions(status),
        }
    }

    /// Stable machine-readable payload for response envelopes.
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "command": self.command,
            "subsystem": self.subsystem,
            "status": self.status.as_str(),
            "available": self.available,
            "feature": self.feature.data_json(),
            "capabilities": self
                .capabilities
                .iter()
                .map(ScienceCapabilityStatus::data_json)
                .collect::<Vec<_>>(),
            "degradations": self
                .degradations
                .iter()
                .map(ScienceDegradation::data_json)
                .collect::<Vec<_>>(),
            "nextActions": self.next_actions,
        })
    }

    /// Human-readable diagnostics summary for terminal usage.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Science analytics: {} (available={})\n",
            self.status.as_str(),
            self.available
        ));
        out.push_str(&format!(
            "Feature: {} (enabled={})\n",
            self.feature.name, self.feature.enabled
        ));
        out.push_str("Capabilities:\n");
        for capability in &self.capabilities {
            match capability.degradation_code {
                Some(code) => out.push_str(&format!(
                    "  - {}: {} ({code})\n",
                    capability.name,
                    capability.state.as_str()
                )),
                None => out.push_str(&format!(
                    "  - {}: {}\n",
                    capability.name,
                    capability.state.as_str()
                )),
            }
        }
        if !self.degradations.is_empty() {
            out.push_str("Degradations:\n");
            for degradation in &self.degradations {
                out.push_str(&format!(
                    "  - [{}] {} -> {}\n",
                    degradation.code, degradation.message, degradation.repair
                ));
            }
        }
        if !self.next_actions.is_empty() {
            out.push_str("Next:\n");
            for action in &self.next_actions {
                out.push_str(&format!("  - {action}\n"));
            }
        }
        out
    }
}

/// Compile-time science feature state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScienceFeatureStatus {
    pub name: &'static str,
    pub enabled: bool,
    pub description: &'static str,
}

impl ScienceFeatureStatus {
    #[must_use]
    pub const fn current() -> Self {
        Self {
            name: SCIENCE_ANALYTICS_FEATURE,
            enabled: cfg!(feature = "science-analytics"),
            description: "Optional offline analytics for evaluation metrics and diagnostics.",
        }
    }

    #[must_use]
    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "enabled": self.enabled,
            "description": self.description,
        })
    }
}

/// Status for an individual science capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScienceCapabilityState {
    /// Capability is usable.
    Available,
    /// Capability is unavailable with a stable degradation code.
    Degraded,
}

impl ScienceCapabilityState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Degraded => "degraded",
        }
    }
}

/// Deterministic status for a named science capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScienceCapabilityStatus {
    pub name: &'static str,
    pub state: ScienceCapabilityState,
    pub required_feature: &'static str,
    pub degradation_code: Option<&'static str>,
    pub description: &'static str,
}

impl ScienceCapabilityStatus {
    #[must_use]
    pub const fn available(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            state: ScienceCapabilityState::Available,
            required_feature: SCIENCE_ANALYTICS_FEATURE,
            degradation_code: None,
            description,
        }
    }

    #[must_use]
    pub const fn degraded(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            state: ScienceCapabilityState::Degraded,
            required_feature: SCIENCE_ANALYTICS_FEATURE,
            degradation_code: Some(DEGRADATION_CODE_NOT_COMPILED),
            description,
        }
    }

    #[must_use]
    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "state": self.state.as_str(),
            "requiredFeature": self.required_feature,
            "degradationCode": self.degradation_code,
            "description": self.description,
        })
    }
}

/// Stable degradation entry for science status diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScienceDegradation {
    pub code: &'static str,
    pub message: &'static str,
    pub repair: &'static str,
}

impl ScienceDegradation {
    #[must_use]
    pub const fn not_compiled() -> Self {
        Self {
            code: DEGRADATION_CODE_NOT_COMPILED,
            message: "Science analytics were not compiled into this binary.",
            repair: "Rebuild ee with --features science-analytics.",
        }
    }

    #[must_use]
    pub const fn backend_unavailable() -> Self {
        Self {
            code: DEGRADATION_CODE_BACKEND_UNAVAILABLE,
            message: "Science analytics backend is unavailable.",
            repair: "Run ee doctor --json and inspect science diagnostics.",
        }
    }

    #[must_use]
    pub const fn input_too_large() -> Self {
        Self {
            code: DEGRADATION_CODE_INPUT_TOO_LARGE,
            message: "Science analytics input exceeds the supported size.",
            repair: "Reduce the science analytics input size and retry.",
        }
    }

    #[must_use]
    pub const fn budget_exceeded() -> Self {
        Self {
            code: DEGRADATION_CODE_BUDGET_EXCEEDED,
            message: "Science analytics budget was exhausted.",
            repair: "Increase the science analytics budget or use a smaller input.",
        }
    }

    #[must_use]
    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code,
            "message": self.message,
            "repair": self.repair,
        })
    }
}

// ============================================================================
// Drift Analysis (EE-179)
// ============================================================================

/// Options for drift analysis over frozen evaluation snapshots.
#[derive(Clone, Debug, Default)]
pub struct DriftAnalysisOptions {
    /// Path to the baseline evaluation snapshot.
    pub baseline_snapshot: Option<std::path::PathBuf>,
    /// Path to the current evaluation snapshot for comparison.
    pub current_snapshot: Option<std::path::PathBuf>,
    /// Workspace path for snapshot discovery.
    pub workspace: std::path::PathBuf,
    /// Drift detection threshold (0.0-1.0). Changes below this are not flagged.
    pub threshold: f64,
    /// Include metric-level detail in the report.
    pub detailed: bool,
}

/// Report from analyzing drift between frozen evaluation snapshots.
#[derive(Clone, Debug)]
pub struct DriftAnalysisReport {
    /// Versioned schema for the report.
    pub schema: &'static str,
    /// Canonical command path.
    pub command: &'static str,
    /// Whether drift was detected above the threshold.
    pub drift_detected: bool,
    /// Overall drift magnitude (0.0-1.0).
    pub drift_magnitude: f64,
    /// Drift detection threshold used.
    pub threshold: f64,
    /// Baseline snapshot identifier.
    pub baseline_id: Option<String>,
    /// Current snapshot identifier.
    pub current_id: Option<String>,
    /// Individual metric drift observations.
    pub metric_drifts: Vec<MetricDrift>,
    /// Degradations encountered during analysis.
    pub degradations: Vec<DriftDegradation>,
    /// Recommended next actions.
    pub next_actions: Vec<String>,
    /// Report generation timestamp.
    pub generated_at: String,
}

impl DriftAnalysisReport {
    /// Build a degraded report when snapshots are unavailable.
    #[must_use]
    pub fn unavailable(options: &DriftAnalysisOptions) -> Self {
        Self {
            schema: DRIFT_ANALYSIS_SCHEMA_V1,
            command: DRIFT_ANALYSIS_COMMAND,
            drift_detected: false,
            drift_magnitude: 0.0,
            threshold: options.threshold,
            baseline_id: None,
            current_id: None,
            metric_drifts: Vec::new(),
            degradations: vec![DriftDegradation {
                code: DEGRADATION_CODE_NO_SNAPSHOTS.to_string(),
                message: "No frozen evaluation snapshots found for drift analysis.".to_string(),
                severity: "high".to_string(),
                repair: "Run ee eval run --workspace . --json to create evaluation snapshots."
                    .to_string(),
            }],
            next_actions: vec![
                "ee eval run --workspace . --json".to_string(),
                "ee analyze science-status --json".to_string(),
            ],
            generated_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Stable machine-readable payload.
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "command": self.command,
            "driftDetected": self.drift_detected,
            "driftMagnitude": self.drift_magnitude,
            "threshold": self.threshold,
            "baselineId": self.baseline_id,
            "currentId": self.current_id,
            "metricDrifts": self.metric_drifts.iter().map(MetricDrift::data_json).collect::<Vec<_>>(),
            "degradations": self.degradations.iter().map(DriftDegradation::data_json).collect::<Vec<_>>(),
            "nextActions": self.next_actions,
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("ee {}\n\n", self.command));
        out.push_str(&format!(
            "Drift detected: {} (magnitude: {:.3}, threshold: {:.3})\n",
            self.drift_detected, self.drift_magnitude, self.threshold
        ));
        if let Some(ref baseline) = self.baseline_id {
            out.push_str(&format!("Baseline: {baseline}\n"));
        }
        if let Some(ref current) = self.current_id {
            out.push_str(&format!("Current: {current}\n"));
        }
        if !self.metric_drifts.is_empty() {
            out.push_str("\nMetric drifts:\n");
            for drift in &self.metric_drifts {
                out.push_str(&format!(
                    "  - {}: {:.3} -> {:.3} (delta: {:.3})\n",
                    drift.metric_name, drift.baseline_value, drift.current_value, drift.delta
                ));
            }
        }
        if !self.degradations.is_empty() {
            out.push_str("\nDegradations:\n");
            for degradation in &self.degradations {
                out.push_str(&format!(
                    "  - [{}] {} -> {}\n",
                    degradation.code, degradation.message, degradation.repair
                ));
            }
        }
        if !self.next_actions.is_empty() {
            out.push_str("\nNext:\n");
            for action in &self.next_actions {
                out.push_str(&format!("  - {action}\n"));
            }
        }
        out
    }
}

/// Drift observation for a single metric.
#[derive(Clone, Debug)]
pub struct MetricDrift {
    /// Metric identifier.
    pub metric_name: String,
    /// Value in baseline snapshot.
    pub baseline_value: f64,
    /// Value in current snapshot.
    pub current_value: f64,
    /// Absolute difference.
    pub delta: f64,
    /// Whether this metric's drift exceeds the threshold.
    pub exceeds_threshold: bool,
}

impl MetricDrift {
    #[must_use]
    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "metricName": self.metric_name,
            "baselineValue": self.baseline_value,
            "currentValue": self.current_value,
            "delta": self.delta,
            "exceedsThreshold": self.exceeds_threshold,
        })
    }
}

/// Degradation entry for drift analysis.
#[derive(Clone, Debug)]
pub struct DriftDegradation {
    pub code: String,
    pub message: String,
    pub severity: String,
    pub repair: String,
}

impl DriftDegradation {
    #[must_use]
    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code,
            "message": self.message,
            "severity": self.severity,
            "repair": self.repair,
        })
    }
}

/// Analyze drift between frozen evaluation snapshots.
///
/// Returns a degraded report if snapshots are unavailable.
#[must_use]
pub fn analyze_drift(options: &DriftAnalysisOptions) -> DriftAnalysisReport {
    // Check if science analytics is available
    if !is_available() {
        return DriftAnalysisReport {
            schema: DRIFT_ANALYSIS_SCHEMA_V1,
            command: DRIFT_ANALYSIS_COMMAND,
            drift_detected: false,
            drift_magnitude: 0.0,
            threshold: options.threshold,
            baseline_id: None,
            current_id: None,
            metric_drifts: Vec::new(),
            degradations: vec![DriftDegradation {
                code: DEGRADATION_CODE_NOT_COMPILED.to_string(),
                message: "Science analytics not compiled into this binary.".to_string(),
                severity: "high".to_string(),
                repair: "Rebuild ee with --features science-analytics.".to_string(),
            }],
            next_actions: vec!["Rebuild ee with --features science-analytics.".to_string()],
            generated_at: chrono::Utc::now().to_rfc3339(),
        };
    }

    // For now, return degraded until evaluation snapshots are implemented
    DriftAnalysisReport::unavailable(options)
}

// ============================================================================
// Clustering Analysis for Consolidation Candidates (EE-174)
// ============================================================================

/// Options for clustering analysis over consolidation candidates.
#[derive(Clone, Debug, Default)]
pub struct ClusteringAnalysisOptions {
    /// Workspace path for candidate discovery.
    pub workspace: std::path::PathBuf,
    /// Optional candidate type filter.
    pub candidate_type: Option<String>,
    /// Optional status filter.
    pub status: Option<String>,
    /// Maximum number of candidates to analyze.
    pub limit: u32,
    /// Include per-cluster detail in the report.
    pub detailed: bool,
}

/// Report from clustering analysis over consolidation candidates.
#[derive(Clone, Debug)]
pub struct ClusteringAnalysisReport {
    /// Versioned schema for the report.
    pub schema: &'static str,
    /// Canonical command path.
    pub command: &'static str,
    /// Whether science analytics is available.
    pub available: bool,
    /// Whether clustering was computed.
    pub computed: bool,
    /// Number of candidates analyzed.
    pub candidate_count: usize,
    /// Clustering diagnostics result.
    pub diagnostics: ClusteringDiagnostics,
    /// Degradations encountered during analysis.
    pub degradations: Vec<ClusteringDegradation>,
    /// Recommended next actions.
    pub next_actions: Vec<String>,
    /// Report generation timestamp.
    pub generated_at: String,
}

impl ClusteringAnalysisReport {
    /// Build a degraded report when science analytics is not compiled.
    #[must_use]
    pub fn not_compiled() -> Self {
        Self {
            schema: CLUSTERING_ANALYSIS_SCHEMA_V1,
            command: CLUSTERING_ANALYSIS_COMMAND,
            available: false,
            computed: false,
            candidate_count: 0,
            diagnostics: ClusteringDiagnostics::default(),
            degradations: vec![ClusteringDegradation {
                code: DEGRADATION_CODE_NOT_COMPILED.to_string(),
                message: "Science analytics not compiled into this binary.".to_string(),
                severity: "high".to_string(),
                repair: "Rebuild ee with --features science-analytics.".to_string(),
            }],
            next_actions: vec!["Rebuild ee with --features science-analytics.".to_string()],
            generated_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Build a degraded report when no candidates are available.
    #[must_use]
    pub fn no_candidates() -> Self {
        Self {
            schema: CLUSTERING_ANALYSIS_SCHEMA_V1,
            command: CLUSTERING_ANALYSIS_COMMAND,
            available: is_available(),
            computed: false,
            candidate_count: 0,
            diagnostics: ClusteringDiagnostics::default(),
            degradations: vec![ClusteringDegradation {
                code: DEGRADATION_CODE_NO_CANDIDATES.to_string(),
                message: "No curation candidates available for clustering analysis.".to_string(),
                severity: "medium".to_string(),
                repair: "ee curate candidates --workspace . --json".to_string(),
            }],
            next_actions: vec!["ee curate candidates --workspace . --json".to_string()],
            generated_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Build a degraded report when candidate embeddings are unavailable.
    #[must_use]
    pub fn no_embeddings(candidate_count: usize) -> Self {
        Self {
            schema: CLUSTERING_ANALYSIS_SCHEMA_V1,
            command: CLUSTERING_ANALYSIS_COMMAND,
            available: is_available(),
            computed: false,
            candidate_count,
            diagnostics: ClusteringDiagnostics::default(),
            degradations: vec![ClusteringDegradation {
                code: DEGRADATION_CODE_NO_EMBEDDINGS.to_string(),
                message: "Candidate embeddings not available for clustering.".to_string(),
                severity: "medium".to_string(),
                repair: "ee index rebuild --workspace . --json".to_string(),
            }],
            next_actions: vec![
                "ee index rebuild --workspace . --json".to_string(),
                "ee analyze science-status --json".to_string(),
            ],
            generated_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Stable machine-readable payload.
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "command": self.command,
            "available": self.available,
            "computed": self.computed,
            "candidateCount": self.candidate_count,
            "diagnostics": self.diagnostics.data_json(),
            "degradations": self.degradations.iter().map(ClusteringDegradation::data_json).collect::<Vec<_>>(),
            "nextActions": self.next_actions,
        })
    }

    /// Human-readable summary.
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("ee {}\n\n", self.command));
        out.push_str(&format!(
            "Computed: {} (candidates: {}, clusters: {})\n",
            self.computed, self.candidate_count, self.diagnostics.cluster_count
        ));
        if let Some(score) = self.diagnostics.silhouette_score {
            out.push_str(&format!("Silhouette score: {score:.3}\n"));
        }
        if !self.degradations.is_empty() {
            out.push_str("\nDegradations:\n");
            for degradation in &self.degradations {
                out.push_str(&format!(
                    "  - [{}] {} -> {}\n",
                    degradation.code, degradation.message, degradation.repair
                ));
            }
        }
        if !self.next_actions.is_empty() {
            out.push_str("\nNext:\n");
            for action in &self.next_actions {
                out.push_str(&format!("  - {action}\n"));
            }
        }
        out
    }
}

/// Degradation entry for clustering analysis.
#[derive(Clone, Debug)]
pub struct ClusteringDegradation {
    pub code: String,
    pub message: String,
    pub severity: String,
    pub repair: String,
}

impl ClusteringDegradation {
    #[must_use]
    fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code,
            "message": self.message,
            "severity": self.severity,
            "repair": self.repair,
        })
    }
}

/// Analyze clustering over consolidation candidates.
///
/// Returns a degraded report if science analytics is unavailable or if
/// candidate embeddings cannot be retrieved.
#[must_use]
pub fn analyze_clustering(_options: &ClusteringAnalysisOptions) -> ClusteringAnalysisReport {
    if !is_available() {
        return ClusteringAnalysisReport::not_compiled();
    }

    // For now, return degraded until candidate embedding retrieval is implemented.
    // The ClusteringDiagnostics::compute() logic is ready and tested; what's
    // missing is the integration to pull embeddings from curation candidates
    // via the search index.
    ClusteringAnalysisReport::no_embeddings(0)
}

#[must_use]
pub fn science_status_report() -> ScienceStatusReport {
    ScienceStatusReport::current()
}

fn science_capabilities(available: bool) -> Vec<ScienceCapabilityStatus> {
    const EVALUATION_METRICS: &str =
        "Deterministic precision, recall, and F1 metrics for evaluation runs.";
    const CLUSTERING_DIAGNOSTICS: &str =
        "Deterministic clustering diagnostics for consolidation candidates.";

    if available {
        vec![
            ScienceCapabilityStatus::available("clustering_diagnostics", CLUSTERING_DIAGNOSTICS),
            ScienceCapabilityStatus::available("evaluation_metrics", EVALUATION_METRICS),
        ]
    } else {
        vec![
            ScienceCapabilityStatus::degraded("clustering_diagnostics", CLUSTERING_DIAGNOSTICS),
            ScienceCapabilityStatus::degraded("evaluation_metrics", EVALUATION_METRICS),
        ]
    }
}

fn science_degradations(status: ScienceStatus) -> Vec<ScienceDegradation> {
    match status {
        ScienceStatus::Available => Vec::new(),
        ScienceStatus::NotCompiled => vec![ScienceDegradation::not_compiled()],
        ScienceStatus::BackendUnavailable => vec![ScienceDegradation::backend_unavailable()],
    }
}

fn science_next_actions(status: ScienceStatus) -> Vec<&'static str> {
    match status {
        ScienceStatus::Available => Vec::new(),
        ScienceStatus::NotCompiled => vec!["Rebuild ee with --features science-analytics."],
        ScienceStatus::BackendUnavailable => {
            vec!["Inspect science backend diagnostics and rebuild if required."]
        }
    }
}

#[cfg(feature = "science-analytics")]
mod enabled {
    //! Science analytics implementation when feature is enabled.
    //!
    //! This submodule contains the actual implementations that depend on
    //! science/numerical crates. It is only compiled when the feature is on.

    use std::collections::BTreeMap;

    const COSINE_CLUSTER_THRESHOLD: f64 = 0.8;

    /// Science-backed evaluation metrics.
    #[derive(Clone, Debug, Default)]
    pub struct EvaluationMetrics {
        pub precision: Option<f64>,
        pub recall: Option<f64>,
        pub f1_score: Option<f64>,
    }

    impl EvaluationMetrics {
        #[must_use]
        pub fn compute(predictions: &[bool], ground_truth: &[bool]) -> Self {
            if predictions.is_empty() || predictions.len() != ground_truth.len() {
                return Self::default();
            }

            let mut true_positive = 0_u32;
            let mut false_positive = 0_u32;
            let mut false_negative = 0_u32;

            for (&predicted, &actual) in predictions.iter().zip(ground_truth.iter()) {
                match (predicted, actual) {
                    (true, true) => true_positive += 1,
                    (true, false) => false_positive += 1,
                    (false, true) => false_negative += 1,
                    (false, false) => {}
                }
            }

            let predicted_positive = true_positive + false_positive;
            let actual_positive = true_positive + false_negative;
            let precision = ratio(true_positive, predicted_positive);
            let recall = ratio(true_positive, actual_positive);
            let f1_score = match (precision, recall) {
                (Some(precision), Some(recall)) if precision + recall > 0.0 => {
                    Some((2.0 * precision * recall) / (precision + recall))
                }
                _ => None,
            };

            Self {
                precision,
                recall,
                f1_score,
            }
        }
    }

    fn ratio(numerator: u32, denominator: u32) -> Option<f64> {
        if denominator == 0 {
            None
        } else {
            Some(f64::from(numerator) / f64::from(denominator))
        }
    }

    /// Science-backed clustering diagnostics.
    #[derive(Clone, Debug, Default)]
    pub struct ClusteringDiagnostics {
        pub cluster_count: usize,
        pub silhouette_score: Option<f64>,
    }

    impl ClusteringDiagnostics {
        #[must_use]
        pub fn compute(embeddings: &[Vec<f32>]) -> Self {
            if !valid_embeddings(embeddings) {
                return Self::default();
            }

            let labels = cluster_labels(embeddings);
            let cluster_count = labels.iter().copied().max().map_or(0, |max| max + 1);
            let silhouette_score = silhouette_score(embeddings, &labels, cluster_count);

            Self {
                cluster_count,
                silhouette_score,
            }
        }

        #[must_use]
        pub fn data_json(&self) -> serde_json::Value {
            serde_json::json!({
                "schema": super::CLUSTERING_DIAGNOSTICS_SCHEMA_V1,
                "status": if self.cluster_count == 0 { "empty" } else { "computed" },
                "available": true,
                "degradationCode": null,
                "clusterCount": self.cluster_count,
                "silhouetteScore": self.silhouette_score,
            })
        }
    }

    fn valid_embeddings(embeddings: &[Vec<f32>]) -> bool {
        let Some(first) = embeddings.first() else {
            return false;
        };
        let dimension = first.len();
        dimension > 0
            && embeddings.iter().all(|embedding| {
                embedding.len() == dimension
                    && embedding.iter().all(|value| value.is_finite())
                    && vector_norm(embedding) > f64::EPSILON
            })
    }

    fn cluster_labels(embeddings: &[Vec<f32>]) -> Vec<usize> {
        let mut parents: Vec<_> = (0..embeddings.len()).collect();
        for (left_index, left_embedding) in embeddings.iter().enumerate() {
            for (right_index, right_embedding) in embeddings.iter().enumerate().skip(left_index + 1)
            {
                if cosine_similarity(left_embedding, right_embedding) >= COSINE_CLUSTER_THRESHOLD {
                    union(&mut parents, left_index, right_index);
                }
            }
        }

        let mut root_to_label = BTreeMap::new();
        let mut next_label = 0_usize;
        (0..embeddings.len())
            .map(|index| {
                let root = find(&mut parents, index);
                *root_to_label.entry(root).or_insert_with(|| {
                    let label = next_label;
                    next_label += 1;
                    label
                })
            })
            .collect()
    }

    fn union(parents: &mut [usize], left: usize, right: usize) {
        let left_root = find(parents, left);
        let right_root = find(parents, right);
        if left_root != right_root {
            let keep = left_root.min(right_root);
            let replace = left_root.max(right_root);
            if let Some(parent) = parents.get_mut(replace) {
                *parent = keep;
            }
        }
    }

    fn find(parents: &mut [usize], index: usize) -> usize {
        let Some(&parent) = parents.get(index) else {
            return index;
        };
        if parent == index {
            index
        } else {
            let root = find(parents, parent);
            if let Some(parent) = parents.get_mut(index) {
                *parent = root;
            }
            root
        }
    }

    fn silhouette_score(
        embeddings: &[Vec<f32>],
        labels: &[usize],
        cluster_count: usize,
    ) -> Option<f64> {
        if cluster_count < 2 || embeddings.len() < 2 {
            return None;
        }

        let mut total = 0.0_f64;
        for (index, current_embedding) in embeddings.iter().enumerate() {
            let Some(&own_label) = labels.get(index) else {
                continue;
            };
            let same_cluster_distances: Vec<_> = embeddings
                .iter()
                .enumerate()
                .filter(|(other, _)| {
                    *other != index && labels.get(*other).is_some_and(|label| *label == own_label)
                })
                .map(|(_, embedding)| cosine_distance(current_embedding, embedding))
                .collect();
            let a = mean_distance(&same_cluster_distances).unwrap_or(0.0);

            let mut nearest_other_cluster: Option<f64> = None;
            for label in 0..cluster_count {
                if label == own_label {
                    continue;
                }
                let distances: Vec<_> = embeddings
                    .iter()
                    .enumerate()
                    .filter(|(other, _)| labels.get(*other).is_some_and(|other| *other == label))
                    .map(|(_, embedding)| cosine_distance(current_embedding, embedding))
                    .collect();
                if let Some(distance) = mean_distance(&distances) {
                    nearest_other_cluster = Some(match nearest_other_cluster {
                        Some(current) if current < distance => current,
                        _ => distance,
                    });
                }
            }

            let Some(b) = nearest_other_cluster else {
                continue;
            };
            let denominator = a.max(b);
            if denominator > f64::EPSILON {
                total += (b - a) / denominator;
            }
        }

        Some(total / sample_count(embeddings))
    }

    fn mean_distance(values: &[f64]) -> Option<f64> {
        if values.is_empty() {
            None
        } else {
            Some(values.iter().sum::<f64>() / sample_count(values))
        }
    }

    fn sample_count<T>(values: &[T]) -> f64 {
        values.iter().fold(0.0, |count, _| count + 1.0)
    }

    fn cosine_distance(left: &[f32], right: &[f32]) -> f64 {
        (1.0 - cosine_similarity(left, right)).clamp(0.0, 2.0)
    }

    fn cosine_similarity(left: &[f32], right: &[f32]) -> f64 {
        dot_product(left, right) / (vector_norm(left) * vector_norm(right))
    }

    fn dot_product(left: &[f32], right: &[f32]) -> f64 {
        left.iter()
            .zip(right.iter())
            .map(|(left, right)| f64::from(*left) * f64::from(*right))
            .sum()
    }

    fn vector_norm(values: &[f32]) -> f64 {
        values
            .iter()
            .map(|value| f64::from(*value).powi(2))
            .sum::<f64>()
            .sqrt()
    }
}

#[cfg(feature = "science-analytics")]
pub use enabled::*;

#[cfg(not(feature = "science-analytics"))]
mod disabled {
    //! Stub types when science-analytics feature is disabled.
    //!
    //! These stubs allow code to compile and degrade gracefully without
    //! compile-time feature checks scattered throughout the codebase.

    /// Stub evaluation metrics when feature is disabled.
    #[derive(Clone, Debug, Default)]
    pub struct EvaluationMetrics {
        pub precision: Option<f64>,
        pub recall: Option<f64>,
        pub f1_score: Option<f64>,
    }

    impl EvaluationMetrics {
        #[must_use]
        pub fn compute(_predictions: &[bool], _ground_truth: &[bool]) -> Self {
            Self::default()
        }
    }

    /// Stub clustering diagnostics when feature is disabled.
    #[derive(Clone, Debug, Default)]
    pub struct ClusteringDiagnostics {
        pub cluster_count: usize,
        pub silhouette_score: Option<f64>,
    }

    impl ClusteringDiagnostics {
        #[must_use]
        pub fn compute(_embeddings: &[Vec<f32>]) -> Self {
            Self::default()
        }

        #[must_use]
        pub fn data_json(&self) -> serde_json::Value {
            serde_json::json!({
                "schema": super::CLUSTERING_DIAGNOSTICS_SCHEMA_V1,
                "status": "not_compiled",
                "available": false,
                "degradationCode": super::DEGRADATION_CODE_NOT_COMPILED,
                "clusterCount": self.cluster_count,
                "silhouetteScore": self.silhouette_score,
            })
        }
    }
}

#[cfg(not(feature = "science-analytics"))]
pub use disabled::*;

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: PartialEq + std::fmt::Debug>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn ensure_json_str(actual: Option<&str>, expected: &str, ctx: &str) -> TestResult {
        match actual {
            Some(actual) if actual == expected => Ok(()),
            Some(actual) => Err(format!("{ctx}: expected {expected:?}, got {actual:?}")),
            None => Err(format!("{ctx}: expected {expected:?}, got None")),
        }
    }

    fn capability<'a>(
        report: &'a ScienceStatusReport,
        name: &str,
    ) -> Result<&'a ScienceCapabilityStatus, String> {
        report
            .capabilities
            .iter()
            .find(|capability| capability.name == name)
            .ok_or_else(|| format!("missing capability {name}"))
    }

    #[cfg(feature = "science-analytics")]
    fn ensure_close(actual: Option<f64>, expected: f64, ctx: &str) -> TestResult {
        match actual {
            Some(actual) if (actual - expected).abs() <= 1.0e-12 => Ok(()),
            Some(actual) => Err(format!("{ctx}: expected {expected:?}, got {actual:?}")),
            None => Err(format!("{ctx}: expected {expected:?}, got None")),
        }
    }

    #[test]
    fn subsystem_name_is_science() -> TestResult {
        ensure(SUBSYSTEM, "science", "subsystem name")
    }

    #[test]
    fn status_returns_consistent_value() -> TestResult {
        let s = status();
        if cfg!(feature = "science-analytics") {
            ensure(s, ScienceStatus::Available, "status when enabled")
        } else {
            ensure(s, ScienceStatus::NotCompiled, "status when disabled")
        }
    }

    #[test]
    fn is_available_matches_feature_flag() -> TestResult {
        let available = is_available();
        if cfg!(feature = "science-analytics") {
            ensure(available, true, "is_available when enabled")
        } else {
            ensure(available, false, "is_available when disabled")
        }
    }

    #[test]
    fn science_status_as_str_is_stable() -> TestResult {
        ensure(
            ScienceStatus::Available.as_str(),
            "available",
            "available str",
        )?;
        ensure(
            ScienceStatus::NotCompiled.as_str(),
            "not_compiled",
            "not_compiled str",
        )?;
        ensure(
            ScienceStatus::BackendUnavailable.as_str(),
            "backend_unavailable",
            "backend_unavailable str",
        )
    }

    #[test]
    fn degradation_codes_are_stable() -> TestResult {
        ensure(
            DEGRADATION_CODE_NOT_COMPILED,
            "science_not_compiled",
            "not compiled code",
        )?;
        ensure(
            DEGRADATION_CODE_BACKEND_UNAVAILABLE,
            "science_backend_unavailable",
            "backend unavailable code",
        )?;
        ensure(
            DEGRADATION_CODE_INPUT_TOO_LARGE,
            "science_input_too_large",
            "input too large code",
        )?;
        ensure(
            DEGRADATION_CODE_BUDGET_EXCEEDED,
            "science_budget_exceeded",
            "budget exceeded code",
        )
    }

    #[test]
    fn science_status_report_contract_is_stable() -> TestResult {
        let report = science_status_report();
        ensure(
            report.schema,
            SCIENCE_STATUS_SCHEMA_V1,
            "science status schema",
        )?;
        ensure(
            report.command,
            SCIENCE_STATUS_COMMAND,
            "science status command",
        )?;
        ensure(report.subsystem, SUBSYSTEM, "science status subsystem")?;
        ensure(
            report.feature.name,
            SCIENCE_ANALYTICS_FEATURE,
            "science feature name",
        )?;
        ensure(
            report.feature.enabled,
            cfg!(feature = "science-analytics"),
            "science feature enabled",
        )?;
        ensure(
            report.available,
            cfg!(feature = "science-analytics"),
            "science report availability",
        )?;
        ensure(report.capabilities.len(), 2, "science capability count")?;
        ensure(
            capability(&report, "clustering_diagnostics")?.required_feature,
            SCIENCE_ANALYTICS_FEATURE,
            "clustering required feature",
        )?;
        ensure(
            capability(&report, "evaluation_metrics")?.required_feature,
            SCIENCE_ANALYTICS_FEATURE,
            "metrics required feature",
        )
    }

    #[test]
    fn science_status_report_json_shape_is_stable() -> TestResult {
        let report = science_status_report();
        let json = report.data_json();
        ensure_json_str(
            json.get("schema").and_then(serde_json::Value::as_str),
            SCIENCE_STATUS_SCHEMA_V1,
            "json schema",
        )?;
        ensure_json_str(
            json.get("command").and_then(serde_json::Value::as_str),
            SCIENCE_STATUS_COMMAND,
            "json command",
        )?;
        ensure_json_str(
            json.get("subsystem").and_then(serde_json::Value::as_str),
            SUBSYSTEM,
            "json subsystem",
        )?;
        ensure_json_str(
            json.get("status").and_then(serde_json::Value::as_str),
            report.status.as_str(),
            "json status",
        )?;
        ensure(
            json.get("available").and_then(serde_json::Value::as_bool),
            Some(report.available),
            "json available",
        )?;
        ensure(
            json.get("capabilities")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|entries| entries.len() == 2),
            true,
            "json capabilities count",
        )
    }

    #[test]
    fn science_status_report_human_summary_contains_status_line() -> TestResult {
        let report = science_status_report();
        let summary = report.human_summary();
        ensure(
            summary.starts_with("Science analytics: "),
            true,
            "summary starts with status line",
        )?;
        ensure(
            summary.contains("Capabilities:"),
            true,
            "summary includes capabilities",
        )
    }

    #[cfg(not(feature = "science-analytics"))]
    #[test]
    fn science_status_report_degrades_without_feature() -> TestResult {
        let report = ScienceStatusReport::current();
        ensure(report.status, ScienceStatus::NotCompiled, "status")?;
        ensure(report.degradations.len(), 1, "degradation count")?;
        ensure(
            report.degradations.iter().map(|entry| entry.code).collect(),
            vec![DEGRADATION_CODE_NOT_COMPILED],
            "degradation codes",
        )?;
        ensure(
            capability(&report, "clustering_diagnostics")?.state,
            ScienceCapabilityState::Degraded,
            "clustering degraded",
        )?;
        ensure(
            capability(&report, "evaluation_metrics")?.state,
            ScienceCapabilityState::Degraded,
            "metrics degraded",
        )?;
        ensure(
            report.next_actions,
            vec!["Rebuild ee with --features science-analytics."],
            "next actions",
        )
    }

    #[cfg(feature = "science-analytics")]
    #[test]
    fn science_status_report_is_available_with_feature() -> TestResult {
        let report = ScienceStatusReport::current();
        ensure(report.status, ScienceStatus::Available, "status")?;
        ensure(report.degradations.is_empty(), true, "degradations empty")?;
        ensure(report.next_actions.is_empty(), true, "next actions empty")?;
        ensure(
            capability(&report, "clustering_diagnostics")?.state,
            ScienceCapabilityState::Available,
            "clustering available",
        )?;
        ensure(
            capability(&report, "evaluation_metrics")?.state,
            ScienceCapabilityState::Available,
            "metrics available",
        )
    }

    #[test]
    fn science_capability_state_strings_are_stable() -> TestResult {
        ensure(
            ScienceCapabilityState::Available.as_str(),
            "available",
            "available state",
        )?;
        ensure(
            ScienceCapabilityState::Degraded.as_str(),
            "degraded",
            "degraded state",
        )
    }

    #[test]
    fn science_backend_unavailable_degradation_is_stable() -> TestResult {
        ensure(
            super::science_degradations(ScienceStatus::BackendUnavailable)
                .iter()
                .map(|entry| entry.code)
                .collect(),
            vec![DEGRADATION_CODE_BACKEND_UNAVAILABLE],
            "backend unavailable degradation",
        )?;
        ensure(
            super::science_next_actions(ScienceStatus::BackendUnavailable),
            vec!["Inspect science backend diagnostics and rebuild if required."],
            "backend unavailable next action",
        )
    }

    #[test]
    fn science_operational_degradation_entries_are_stable() -> TestResult {
        let entries = [
            ScienceDegradation::backend_unavailable(),
            ScienceDegradation::input_too_large(),
            ScienceDegradation::budget_exceeded(),
        ];
        ensure(
            entries.iter().map(|entry| entry.code).collect(),
            vec![
                DEGRADATION_CODE_BACKEND_UNAVAILABLE,
                DEGRADATION_CODE_INPUT_TOO_LARGE,
                DEGRADATION_CODE_BUDGET_EXCEEDED,
            ],
            "operational degradation codes",
        )?;
        ensure(
            entries.iter().map(|entry| entry.repair).collect(),
            vec![
                "Run ee doctor --json and inspect science diagnostics.",
                "Reduce the science analytics input size and retry.",
                "Increase the science analytics budget or use a smaller input.",
            ],
            "operational degradation repairs",
        )
    }

    #[test]
    fn evaluation_metrics_default_is_empty() -> TestResult {
        let metrics = EvaluationMetrics::default();
        ensure(metrics.precision, None, "precision is None")?;
        ensure(metrics.recall, None, "recall is None")?;
        ensure(metrics.f1_score, None, "f1_score is None")
    }

    #[cfg(not(feature = "science-analytics"))]
    #[test]
    fn evaluation_metrics_compute_degrades_when_science_is_not_compiled() -> TestResult {
        let metrics = EvaluationMetrics::compute(&[true, false, true], &[true, true, false]);
        ensure(metrics.precision, None, "precision remains unavailable")?;
        ensure(metrics.recall, None, "recall remains unavailable")?;
        ensure(metrics.f1_score, None, "f1 remains unavailable")
    }

    #[cfg(not(feature = "science-analytics"))]
    #[test]
    fn science_analytics_is_not_enabled_by_default() -> TestResult {
        ensure(
            cfg!(feature = "science-analytics"),
            false,
            "default feature set keeps science disabled",
        )
    }

    #[cfg(feature = "science-analytics")]
    #[test]
    fn science_metric_reference_parity_balanced_case() -> TestResult {
        let metrics = EvaluationMetrics::compute(
            &[true, true, false, true, false, false],
            &[true, false, true, true, false, false],
        );

        ensure_close(metrics.precision, 2.0 / 3.0, "precision parity")?;
        ensure_close(metrics.recall, 2.0 / 3.0, "recall parity")?;
        ensure_close(metrics.f1_score, 2.0 / 3.0, "f1 parity")
    }

    #[cfg(feature = "science-analytics")]
    #[test]
    fn science_metric_reference_parity_perfect_case() -> TestResult {
        let metrics = EvaluationMetrics::compute(&[true, false, true], &[true, false, true]);

        ensure_close(metrics.precision, 1.0, "precision perfect")?;
        ensure_close(metrics.recall, 1.0, "recall perfect")?;
        ensure_close(metrics.f1_score, 1.0, "f1 perfect")
    }

    #[cfg(feature = "science-analytics")]
    #[test]
    fn science_metric_reference_parity_handles_empty_and_mismatched_inputs() -> TestResult {
        let empty = EvaluationMetrics::compute(&[], &[]);
        ensure(empty.precision, None, "empty precision")?;
        ensure(empty.recall, None, "empty recall")?;
        ensure(empty.f1_score, None, "empty f1")?;

        let mismatched = EvaluationMetrics::compute(&[true, false], &[true]);
        ensure(mismatched.precision, None, "mismatched precision")?;
        ensure(mismatched.recall, None, "mismatched recall")?;
        ensure(mismatched.f1_score, None, "mismatched f1")
    }

    #[cfg(feature = "science-analytics")]
    #[test]
    fn science_metric_reference_parity_handles_undefined_denominators() -> TestResult {
        let no_positive_predictions =
            EvaluationMetrics::compute(&[false, false, false], &[true, false, true]);
        ensure(
            no_positive_predictions.precision,
            None,
            "precision without predicted positives",
        )?;
        ensure_close(
            no_positive_predictions.recall,
            0.0,
            "recall without predicted positives",
        )?;
        ensure(
            no_positive_predictions.f1_score,
            None,
            "f1 without precision",
        )?;

        let no_actual_positives = EvaluationMetrics::compute(&[true, false], &[false, false]);
        ensure_close(
            no_actual_positives.precision,
            0.0,
            "precision without true positives",
        )?;
        ensure(
            no_actual_positives.recall,
            None,
            "recall without actual positives",
        )?;
        ensure(no_actual_positives.f1_score, None, "f1 without recall")
    }

    #[test]
    fn clustering_diagnostics_default_is_empty() -> TestResult {
        let diag = ClusteringDiagnostics::default();
        ensure(diag.cluster_count, 0, "cluster_count is 0")?;
        ensure(diag.silhouette_score, None, "silhouette_score is None")
    }

    #[cfg(not(feature = "science-analytics"))]
    #[test]
    fn clustering_diagnostics_compute_degrades_when_science_is_not_compiled() -> TestResult {
        let diag = ClusteringDiagnostics::compute(&[
            vec![1.0, 0.0],
            vec![0.99, 0.1],
            vec![-1.0, 0.0],
            vec![-0.99, -0.1],
        ]);

        ensure(diag.cluster_count, 0, "cluster count remains unavailable")?;
        ensure(
            diag.silhouette_score,
            None,
            "silhouette remains unavailable",
        )?;
        let json = diag.data_json();
        ensure_json_str(
            json.get("schema").and_then(serde_json::Value::as_str),
            CLUSTERING_DIAGNOSTICS_SCHEMA_V1,
            "clustering diagnostics schema",
        )?;
        ensure_json_str(
            json.get("degradationCode")
                .and_then(serde_json::Value::as_str),
            DEGRADATION_CODE_NOT_COMPILED,
            "clustering diagnostics degradation code",
        )
    }

    #[cfg(feature = "science-analytics")]
    #[test]
    fn clustering_diagnostics_reference_parity_two_clear_clusters() -> TestResult {
        let diag = ClusteringDiagnostics::compute(&[
            vec![1.0, 0.0],
            vec![0.99, 0.1],
            vec![-1.0, 0.0],
            vec![-0.99, -0.1],
        ]);

        ensure(diag.cluster_count, 2, "cluster count")?;
        let json = diag.data_json();
        ensure_json_str(
            json.get("schema").and_then(serde_json::Value::as_str),
            CLUSTERING_DIAGNOSTICS_SCHEMA_V1,
            "clustering diagnostics schema",
        )?;
        ensure_json_str(
            json.get("status").and_then(serde_json::Value::as_str),
            "computed",
            "clustering diagnostics status",
        )?;
        match diag.silhouette_score {
            Some(score) if score > 0.9 && score <= 1.0 => Ok(()),
            other => Err(format!("expected high silhouette score, got {other:?}")),
        }
    }

    #[cfg(feature = "science-analytics")]
    #[test]
    fn clustering_diagnostics_reference_parity_single_cluster_has_no_silhouette() -> TestResult {
        let diag =
            ClusteringDiagnostics::compute(&[vec![1.0, 0.0], vec![0.98, 0.1], vec![0.95, 0.2]]);

        ensure(diag.cluster_count, 1, "single cluster count")?;
        ensure(
            diag.silhouette_score,
            None,
            "single cluster silhouette is undefined",
        )
    }

    #[cfg(feature = "science-analytics")]
    #[test]
    fn clustering_diagnostics_rejects_invalid_embeddings() -> TestResult {
        let empty = ClusteringDiagnostics::compute(&[]);
        ensure(empty.cluster_count, 0, "empty cluster count")?;
        ensure(empty.silhouette_score, None, "empty silhouette")?;

        let mismatched = ClusteringDiagnostics::compute(&[vec![1.0, 0.0], vec![1.0]]);
        ensure(mismatched.cluster_count, 0, "mismatched cluster count")?;

        let non_finite = ClusteringDiagnostics::compute(&[vec![1.0, f32::NAN]]);
        ensure(non_finite.cluster_count, 0, "non-finite cluster count")?;

        let zero_vector = ClusteringDiagnostics::compute(&[vec![0.0, 0.0]]);
        ensure(zero_vector.cluster_count, 0, "zero-vector cluster count")
    }

    #[cfg(feature = "science-analytics")]
    #[test]
    fn clustering_diagnostics_are_deterministic_under_input_order() -> TestResult {
        let first = ClusteringDiagnostics::compute(&[
            vec![1.0, 0.0],
            vec![0.99, 0.1],
            vec![-1.0, 0.0],
            vec![-0.99, -0.1],
        ]);
        let reordered = ClusteringDiagnostics::compute(&[
            vec![-0.99, -0.1],
            vec![0.99, 0.1],
            vec![-1.0, 0.0],
            vec![1.0, 0.0],
        ]);

        ensure(
            reordered.cluster_count,
            first.cluster_count,
            "cluster count",
        )?;
        let Some(expected) = first.silhouette_score else {
            return Err("expected first silhouette score".to_owned());
        };
        ensure_close(
            reordered.silhouette_score,
            expected,
            "deterministic silhouette",
        )
    }
}
