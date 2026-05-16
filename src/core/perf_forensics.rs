//! Differential performance forensics: read-only compare of performance artifacts.
//!
//! This module implements ADR 0024: read-only regression comparison of benchmark
//! reports, support-bundle manifests, explain-performance reports, profile evidence,
//! cache reports, write-queue reports, and swarm-contention reports.
//!
//! The compare surface is read-only and does not mutate memory, indexes, profiles,
//! caches, or beads state.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::models::{self, DomainError};

pub const COMPARE_RESULT_SCHEMA_V1: &str = "ee.perf.compare.v1";
pub const BUDGET_CHECK_SCHEMA_V1: &str = "ee.perf.budget_check.v1";

const ARTIFACT_SUMMARY_SCHEMA: &str = "ee.perf.artifact_summary.v1";

fn default_artifact_summary_schema() -> String {
    ARTIFACT_SUMMARY_SCHEMA.to_owned()
}

fn default_compare_result_schema() -> &'static str {
    COMPARE_RESULT_SCHEMA_V1
}

/// Artifact kinds supported by the perf forensics compare surface.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    BenchmarkReport,
    SupportBundleManifest,
    ExplainPerformanceReport,
    ProfileEvidence,
    CacheReport,
    WriteQueueReport,
    SwarmContentionReport,
}

impl ArtifactKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BenchmarkReport => "benchmark_report",
            Self::SupportBundleManifest => "support_bundle_manifest",
            Self::ExplainPerformanceReport => "explain_performance_report",
            Self::ProfileEvidence => "profile_evidence",
            Self::CacheReport => "cache_report",
            Self::WriteQueueReport => "write_queue_report",
            Self::SwarmContentionReport => "swarm_contention_report",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "benchmark_report" => Some(Self::BenchmarkReport),
            "support_bundle_manifest" => Some(Self::SupportBundleManifest),
            "explain_performance_report" => Some(Self::ExplainPerformanceReport),
            "profile_evidence" => Some(Self::ProfileEvidence),
            "cache_report" => Some(Self::CacheReport),
            "write_queue_report" => Some(Self::WriteQueueReport),
            "swarm_contention_report" => Some(Self::SwarmContentionReport),
            _ => None,
        }
    }
}

/// Normalized artifact summary for comparison.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactSummary {
    #[serde(default = "default_artifact_summary_schema")]
    pub schema: String,
    pub artifact_id: String,
    pub artifact_kind: ArtifactKind,
    pub source_schema: Option<String>,
    pub source_path: Option<String>,
    pub content_hash: Option<String>,
    pub observed_hash: Option<String>,
    pub profile: Option<String>,
    pub fixture_tier: Option<String>,
    pub command_family: Option<String>,
    pub metrics: BTreeMap<String, MetricValue>,
    pub degraded: Vec<SummaryDegradation>,
    pub redaction: RedactionPosture,
    pub provenance: Vec<ProvenanceEntry>,
}

impl ArtifactSummary {
    #[must_use]
    pub fn new(artifact_id: impl Into<String>, artifact_kind: ArtifactKind) -> Self {
        Self {
            schema: ARTIFACT_SUMMARY_SCHEMA.to_owned(),
            artifact_id: artifact_id.into(),
            artifact_kind,
            source_schema: None,
            source_path: None,
            content_hash: None,
            observed_hash: None,
            profile: None,
            fixture_tier: None,
            command_family: None,
            metrics: BTreeMap::new(),
            degraded: Vec::new(),
            redaction: RedactionPosture::default(),
            provenance: Vec::new(),
        }
    }

    pub fn with_metric(mut self, name: impl Into<String>, value: MetricValue) -> Self {
        self.metrics.insert(name.into(), value);
        self
    }

    pub fn with_degradation(mut self, degradation: SummaryDegradation) -> Self {
        self.degraded.push(degradation);
        self
    }

    #[must_use]
    pub fn hash_mismatch(&self) -> bool {
        match (&self.content_hash, &self.observed_hash) {
            (Some(declared), Some(observed)) => declared != observed,
            _ => false,
        }
    }
}

/// A comparable metric value with volatility classification.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricValue {
    pub value: f64,
    pub unit: Option<String>,
    pub volatility: Volatility,
    pub source_field: Option<String>,
}

impl MetricValue {
    #[must_use]
    pub fn stable(value: f64) -> Self {
        Self {
            value,
            unit: None,
            volatility: Volatility::Stable,
            source_field: None,
        }
    }

    #[must_use]
    pub fn timing(value: f64) -> Self {
        Self {
            value,
            unit: Some("ms".to_string()),
            volatility: Volatility::Timing,
            source_field: None,
        }
    }

    #[must_use]
    pub fn resource(value: f64, unit: impl Into<String>) -> Self {
        Self {
            value,
            unit: Some(unit.into()),
            volatility: Volatility::Resource,
            source_field: None,
        }
    }
}

/// Volatility classification for metric comparison.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Volatility {
    #[default]
    Stable,
    Timing,
    Resource,
    Unavailable,
}

/// Redaction posture for artifact summary.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RedactionPosture {
    pub applied: bool,
    pub uncertain_fields: Vec<String>,
    pub secret_indicators: u32,
}

/// Provenance entry for a copied field.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceEntry {
    pub field: String,
    pub source: String,
    pub confidence: Option<String>,
}

/// Degradation record for artifact summary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryDegradation {
    pub code: String,
    pub severity: Severity,
    pub message: String,
    pub affected_field: Option<String>,
    pub repair: Option<String>,
}

impl SummaryDegradation {
    #[must_use]
    pub fn missing_metric(metric_name: &str) -> Self {
        Self {
            code: "missing_metric".to_string(),
            severity: Severity::Low,
            message: format!("Metric '{metric_name}' is not available in this artifact"),
            affected_field: Some(metric_name.to_string()),
            repair: None,
        }
    }

    #[must_use]
    pub fn stale_schema(schema: &str) -> Self {
        Self {
            code: "stale_schema_version".to_string(),
            severity: Severity::Medium,
            message: format!("Artifact uses older schema version: {schema}"),
            affected_field: None,
            repair: Some("Re-export artifact with current ee version".to_string()),
        }
    }

    #[must_use]
    pub fn tampered_hash() -> Self {
        Self {
            code: "tampered_hash".to_string(),
            severity: Severity::High,
            message: "Declared content hash does not match observed hash".to_string(),
            affected_field: None,
            repair: Some("Verify artifact integrity or re-export".to_string()),
        }
    }

    #[must_use]
    pub fn unsupported_kind(kind: &str) -> Self {
        Self {
            code: "unsupported_artifact_kind".to_string(),
            severity: Severity::High,
            message: format!("Artifact kind '{kind}' is not supported for comparison"),
            affected_field: None,
            repair: None,
        }
    }

    #[must_use]
    pub fn redaction_uncertain(field: &str) -> Self {
        Self {
            code: "redaction_uncertain".to_string(),
            severity: Severity::Medium,
            message: format!("Field '{field}' may contain sensitive data"),
            affected_field: Some(field.to_string()),
            repair: None,
        }
    }

    #[must_use]
    pub fn profile_mismatch(baseline: &str, candidate: &str) -> Self {
        Self {
            code: "profile_mismatch".to_string(),
            severity: Severity::Medium,
            message: format!("Profile mismatch: baseline={baseline}, candidate={candidate}"),
            affected_field: Some("profile".to_string()),
            repair: Some("Compare artifacts from the same profile".to_string()),
        }
    }
}

/// Severity levels for degradations and deltas.
#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    #[default]
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

/// Subsystem owner taxonomy for regression hints.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubsystemOwner {
    Search,
    Pack,
    Db,
    Cache,
    WriteSpool,
    Profile,
    HostPressure,
    Unknown,
}

impl SubsystemOwner {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Pack => "pack",
            Self::Db => "db",
            Self::Cache => "cache",
            Self::WriteSpool => "write_spool",
            Self::Profile => "profile",
            Self::HostPressure => "host_pressure",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub fn infer_from_metric(metric_name: &str) -> Self {
        let lower = metric_name.to_lowercase();
        if lower.contains("search") || lower.contains("query") || lower.contains("candidate") {
            Self::Search
        } else if lower.contains("pack") || lower.contains("token") || lower.contains("context") {
            Self::Pack
        } else if lower.contains("db") || lower.contains("storage") || lower.contains("migration") {
            Self::Db
        } else if lower.contains("cache") || lower.contains("hotset") || lower.contains("prewarm") {
            Self::Cache
        } else if lower.contains("write") || lower.contains("spool") || lower.contains("queue") {
            Self::WriteSpool
        } else if lower.contains("profile") || lower.contains("budget") {
            Self::Profile
        } else if lower.contains("rss")
            || lower.contains("memory")
            || lower.contains("cpu")
            || lower.contains("load")
        {
            Self::HostPressure
        } else {
            Self::Unknown
        }
    }
}

/// Compare result classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompareResult {
    Improved,
    Unchanged,
    Regressed,
    Mixed,
    Inconclusive,
}

impl CompareResult {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Improved => "improved",
            Self::Unchanged => "unchanged",
            Self::Regressed => "regressed",
            Self::Mixed => "mixed",
            Self::Inconclusive => "inconclusive",
        }
    }
}

/// Confidence level for comparison results.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    #[default]
    Low,
    Medium,
    High,
}

impl Confidence {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// A single metric delta in the comparison.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricDelta {
    pub metric: String,
    pub baseline_value: Option<f64>,
    pub candidate_value: Option<f64>,
    pub delta: Option<f64>,
    pub delta_percent: Option<f64>,
    pub direction: DeltaDirection,
    pub severity: Severity,
    pub owner_hint: SubsystemOwner,
    pub volatility: Volatility,
}

impl MetricDelta {
    #[must_use]
    pub fn from_values(
        metric: impl Into<String>,
        baseline: Option<&MetricValue>,
        candidate: Option<&MetricValue>,
    ) -> Self {
        let metric = metric.into();
        let owner_hint = SubsystemOwner::infer_from_metric(&metric);

        match (baseline, candidate) {
            (Some(b), Some(c)) => {
                let delta = c.value - b.value;
                let delta_percent = if b.value.abs() > f64::EPSILON {
                    Some((delta / b.value) * 100.0)
                } else {
                    None
                };
                let direction = if delta > 0.0 {
                    DeltaDirection::Increased
                } else if delta < 0.0 {
                    DeltaDirection::Decreased
                } else {
                    DeltaDirection::Unchanged
                };
                let severity = classify_delta_severity(delta_percent, &b.volatility);

                Self {
                    metric,
                    baseline_value: Some(b.value),
                    candidate_value: Some(c.value),
                    delta: Some(delta),
                    delta_percent,
                    direction,
                    severity,
                    owner_hint,
                    volatility: b.volatility,
                }
            }
            (Some(b), None) => Self {
                metric,
                baseline_value: Some(b.value),
                candidate_value: None,
                delta: None,
                delta_percent: None,
                direction: DeltaDirection::Missing,
                severity: Severity::Low,
                owner_hint,
                volatility: b.volatility,
            },
            (None, Some(c)) => Self {
                metric,
                baseline_value: None,
                candidate_value: Some(c.value),
                delta: None,
                delta_percent: None,
                direction: DeltaDirection::Added,
                severity: Severity::Low,
                owner_hint,
                volatility: c.volatility,
            },
            (None, None) => Self {
                metric,
                baseline_value: None,
                candidate_value: None,
                delta: None,
                delta_percent: None,
                direction: DeltaDirection::Missing,
                severity: Severity::Low,
                owner_hint,
                volatility: Volatility::Unavailable,
            },
        }
    }
}

fn classify_delta_severity(delta_percent: Option<f64>, volatility: &Volatility) -> Severity {
    let Some(pct) = delta_percent else {
        return Severity::Low;
    };
    let abs_pct = pct.abs();

    match volatility {
        Volatility::Stable => {
            if abs_pct > 50.0 {
                Severity::Critical
            } else if abs_pct > 20.0 {
                Severity::High
            } else if abs_pct > 5.0 {
                Severity::Medium
            } else {
                Severity::Low
            }
        }
        Volatility::Timing => {
            if abs_pct > 100.0 {
                Severity::High
            } else if abs_pct > 50.0 {
                Severity::Medium
            } else {
                Severity::Low
            }
        }
        Volatility::Resource => {
            if abs_pct > 200.0 {
                Severity::High
            } else if abs_pct > 100.0 {
                Severity::Medium
            } else {
                Severity::Low
            }
        }
        Volatility::Unavailable => Severity::Low,
    }
}

fn metric_higher_is_better(metric_name: &str) -> bool {
    let lower = metric_name.to_lowercase();
    lower.contains("hit_rate")
        || lower.contains("success_rate")
        || lower.contains("cache_hit")
        || lower.contains("precision")
        || lower.contains("recall")
        || lower.contains("throughput")
}

fn delta_is_regression(delta: &MetricDelta) -> bool {
    if delta.severity < Severity::Medium {
        return false;
    }

    if metric_higher_is_better(&delta.metric) {
        delta.direction == DeltaDirection::Decreased
    } else {
        delta.direction == DeltaDirection::Increased
    }
}

fn delta_is_improvement(delta: &MetricDelta) -> bool {
    if delta.severity < Severity::Medium {
        return false;
    }

    if metric_higher_is_better(&delta.metric) {
        delta.direction == DeltaDirection::Increased
    } else {
        delta.direction == DeltaDirection::Decreased
    }
}

/// Direction of a metric change.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaDirection {
    Increased,
    Decreased,
    Unchanged,
    Added,
    Missing,
}

/// Owner hint with supporting evidence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerHint {
    pub owner: SubsystemOwner,
    pub confidence: Confidence,
    pub evidence: Vec<String>,
}

/// Comparison degradation record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareDegradation {
    pub code: String,
    pub severity: Severity,
    pub artifact_side: ArtifactSide,
    pub affected_field: Option<String>,
    pub message: String,
    pub repair: Option<String>,
}

/// Which artifact side is affected.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactSide {
    Baseline,
    Candidate,
    Both,
}

/// Full comparison report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareReport {
    #[serde(skip_deserializing, default = "default_compare_result_schema")]
    pub schema: &'static str,
    pub artifacts: CompareArtifacts,
    pub summary: CompareSummary,
    pub deltas: Vec<MetricDelta>,
    pub owner_hints: Vec<OwnerHint>,
    pub degraded: Vec<CompareDegradation>,
    pub next_commands: Vec<String>,
}

/// Artifact pair in comparison.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareArtifacts {
    pub baseline: ArtifactSummary,
    pub candidate: ArtifactSummary,
}

/// Summary of comparison result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareSummary {
    pub result: CompareResult,
    pub confidence: Confidence,
    pub worst_severity: Severity,
    pub delta_count: usize,
    pub regression_count: usize,
    pub improvement_count: usize,
}

/// Budget-check result classification for a normalized performance artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetCheckResult {
    Passed,
    Degraded,
    Failed,
    Inconclusive,
}

impl BudgetCheckResult {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Degraded => "degraded",
            Self::Failed => "failed",
            Self::Inconclusive => "inconclusive",
        }
    }
}

/// Single-artifact posture returned by `ee perf budget check`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetCheckReport {
    #[serde(skip_deserializing, default = "default_budget_check_schema")]
    pub schema: &'static str,
    pub requested_profile: String,
    pub artifact: BudgetCheckArtifact,
    pub summary: BudgetCheckSummary,
    pub degraded: Vec<BudgetCheckDegradation>,
    pub next_commands: Vec<String>,
}

fn default_budget_check_schema() -> &'static str {
    BUDGET_CHECK_SCHEMA_V1
}

/// Redaction-safe artifact identity for budget-check output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetCheckArtifact {
    pub artifact_id: String,
    pub artifact_kind: ArtifactKind,
    pub source_schema: String,
    pub profile: Option<String>,
    pub fixture_tier: Option<String>,
    pub command_family: Option<String>,
}

/// Budget-check summary metrics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetCheckSummary {
    pub result: BudgetCheckResult,
    pub confidence: Confidence,
    pub worst_severity: Severity,
    pub comparable_metric_count: usize,
    pub missing_metric_count: usize,
}

/// Degradation record for budget-check output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetCheckDegradation {
    pub code: String,
    pub severity: Severity,
    pub affected_field: Option<String>,
    pub message: String,
    pub repair: Option<String>,
}

impl BudgetCheckDegradation {
    #[must_use]
    pub fn profile_mismatch(requested: &str, observed: &str) -> Self {
        Self {
            code: "profile_mismatch".to_owned(),
            severity: Severity::Medium,
            affected_field: Some("profile".to_owned()),
            message: format!("Profile mismatch: requested={requested}, artifact={observed}"),
            repair: Some("Re-run with the artifact profile or regenerate the report.".to_owned()),
        }
    }

    #[must_use]
    pub fn missing_profile(requested: &str) -> Self {
        Self {
            code: "profile_missing".to_owned(),
            severity: Severity::Medium,
            affected_field: Some("profile".to_owned()),
            message: format!("Artifact does not declare requested profile `{requested}`."),
            repair: Some("Regenerate the artifact with profile evidence enabled.".to_owned()),
        }
    }

    #[must_use]
    pub fn missing_metrics() -> Self {
        Self {
            code: "missing_metric".to_owned(),
            severity: Severity::Medium,
            affected_field: Some("metrics".to_owned()),
            message: "Artifact does not contain comparable measured metrics.".to_owned(),
            repair: Some(
                "Regenerate the artifact with performance metric collection enabled.".to_owned(),
            ),
        }
    }
}

/// Load a normalized perf artifact summary from a JSON file.
pub fn read_perf_artifact_summary(path: &Path) -> Result<models::ArtifactSummary, DomainError> {
    validate_perf_artifact_path(path)?;
    let body = fs::read_to_string(path).map_err(|error| DomainError::Storage {
        message: format!(
            "Could not read perf artifact summary {}: {error}",
            path.display()
        ),
        repair: Some("Verify the file is readable and retry.".to_owned()),
    })?;
    serde_json::from_str(&body).map_err(|error| DomainError::Usage {
        message: format!("Malformed perf artifact summary JSON: {error}"),
        repair: Some("Re-run with an ee.perf.artifact_summary.v1 JSON file.".to_owned()),
    })
}

fn validate_perf_artifact_path(path: &Path) -> Result<(), DomainError> {
    if let Some(symlink_path) =
        first_existing_symlink_component(path).map_err(|error| DomainError::Storage {
            message: format!(
                "Could not inspect perf artifact path component {}: {}",
                error.path.display(),
                error.source
            ),
            repair: Some("Verify the file is readable and retry.".to_owned()),
        })?
    {
        return Err(DomainError::Usage {
            message: format!(
                "Unsupported perf artifact path {}: path traverses symlinked component {}",
                path.display(),
                symlink_path.display()
            ),
            repair: Some("Pass a regular JSON artifact file, not a symlink.".to_owned()),
        });
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| DomainError::NotFound {
        resource: "perf artifact summary".to_owned(),
        id: path.display().to_string(),
        repair: Some("Pass a readable ee.perf.artifact_summary.v1 JSON file.".to_owned()),
    })?;
    if !metadata.is_file() {
        return Err(DomainError::Usage {
            message: format!("Unsupported perf artifact path: {}", path.display()),
            repair: Some("Pass a JSON file, not a directory or special file.".to_owned()),
        });
    }
    if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
        return Err(DomainError::Usage {
            message: format!("Unsupported perf artifact extension: {}", path.display()),
            repair: Some("Use a .json normalized perf artifact summary.".to_owned()),
        });
    }
    Ok(())
}

#[derive(Debug)]
struct SymlinkComponentInspectionError {
    path: PathBuf,
    source: std::io::Error,
}

fn first_existing_symlink_component(
    path: &Path,
) -> Result<Option<PathBuf>, SymlinkComponentInspectionError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(Some(current)),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(source) => {
                return Err(SymlinkComponentInspectionError {
                    path: current,
                    source,
                });
            }
        }
    }
    Ok(None)
}

/// Compare normalized artifact summary files without mutating durable state.
pub fn compare_artifact_summary_files(
    baseline_path: &Path,
    candidate_path: &Path,
) -> Result<CompareReport, DomainError> {
    let baseline = read_perf_artifact_summary(baseline_path)?;
    let candidate = read_perf_artifact_summary(candidate_path)?;
    Ok(compare_normalized_artifacts(&baseline, &candidate))
}

/// Compare canonical `models::ArtifactSummary` records through the compare core.
#[must_use]
pub fn compare_normalized_artifacts(
    baseline: &models::ArtifactSummary,
    candidate: &models::ArtifactSummary,
) -> CompareReport {
    let baseline = normalize_artifact_summary(baseline);
    let candidate = normalize_artifact_summary(candidate);
    compare_artifacts(&baseline, &candidate)
}

/// Check the profile and degradation posture of a normalized perf artifact file.
pub fn check_perf_budget_report(
    profile: &str,
    report_path: &Path,
) -> Result<BudgetCheckReport, DomainError> {
    let profile = profile.trim();
    if profile.is_empty() {
        return Err(DomainError::Usage {
            message: "Performance budget profile must not be empty.".to_owned(),
            repair: Some("Pass --profile <name> with a non-empty profile.".to_owned()),
        });
    }
    let artifact = read_perf_artifact_summary(report_path)?;
    Ok(check_perf_budget_summary(profile, &artifact))
}

/// Check the profile and degradation posture of a canonical artifact summary.
#[must_use]
pub fn check_perf_budget_summary(
    requested_profile: &str,
    artifact: &models::ArtifactSummary,
) -> BudgetCheckReport {
    let normalized = normalize_artifact_summary(artifact);
    let observed_profile = normalized.profile.clone();
    let mut degraded: Vec<BudgetCheckDegradation> = normalized
        .degraded
        .iter()
        .map(summary_degradation_to_budget)
        .collect();

    match observed_profile.as_deref() {
        Some(profile) if profile != requested_profile => {
            degraded.push(BudgetCheckDegradation::profile_mismatch(
                requested_profile,
                profile,
            ));
        }
        None => degraded.push(BudgetCheckDegradation::missing_profile(requested_profile)),
        _ => {}
    }

    if normalized.metrics.is_empty() {
        degraded.push(BudgetCheckDegradation::missing_metrics());
    }

    degraded.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.code.cmp(&b.code))
            .then_with(|| a.affected_field.cmp(&b.affected_field))
    });

    let worst_severity = degraded
        .iter()
        .map(|degradation| degradation.severity)
        .max()
        .unwrap_or(Severity::Low);
    let missing_metric_count = degraded
        .iter()
        .filter(|degradation| degradation.code == "missing_metric")
        .count();
    let result = if degraded.iter().any(|d| d.code == "tampered_hash") {
        BudgetCheckResult::Inconclusive
    } else if degraded.iter().any(|d| d.severity >= Severity::High) {
        BudgetCheckResult::Failed
    } else if degraded.is_empty() {
        BudgetCheckResult::Passed
    } else {
        BudgetCheckResult::Degraded
    };
    let confidence = if degraded.iter().any(|d| d.severity >= Severity::High) {
        Confidence::Low
    } else if degraded.iter().any(|d| d.severity >= Severity::Medium) {
        Confidence::Medium
    } else {
        Confidence::High
    };
    let mut next_commands = Vec::new();
    if result != BudgetCheckResult::Passed {
        next_commands.push(
            "ee perf compare --baseline <baseline.json> --candidate <candidate.json> --json"
                .to_owned(),
        );
        next_commands.push("ee profile config plan --json".to_owned());
    }

    BudgetCheckReport {
        schema: BUDGET_CHECK_SCHEMA_V1,
        requested_profile: requested_profile.to_owned(),
        artifact: BudgetCheckArtifact {
            artifact_id: normalized.artifact_id,
            artifact_kind: normalized.artifact_kind,
            source_schema: normalized.source_schema.unwrap_or_default(),
            profile: observed_profile,
            fixture_tier: normalized.fixture_tier,
            command_family: normalized.command_family,
        },
        summary: BudgetCheckSummary {
            result,
            confidence,
            worst_severity,
            comparable_metric_count: normalized.metrics.len(),
            missing_metric_count,
        },
        degraded,
        next_commands,
    }
}

fn normalize_artifact_summary(summary: &models::ArtifactSummary) -> ArtifactSummary {
    let mut normalized = ArtifactSummary {
        schema: summary.schema.clone(),
        artifact_id: summary.artifact_id.clone(),
        artifact_kind: convert_artifact_kind(summary.artifact_kind),
        source_schema: Some(summary.source_schema.clone()),
        source_path: summary.source_path.clone(),
        content_hash: summary.content_hash.clone(),
        observed_hash: summary.observed_hash.clone(),
        profile: summary
            .profile
            .as_ref()
            .map(|profile| profile.profile_name.clone()),
        fixture_tier: summary.fixture_tier.clone(),
        command_family: summary.command_family.clone(),
        metrics: BTreeMap::new(),
        degraded: summary
            .degraded
            .iter()
            .map(convert_summary_degradation)
            .collect(),
        redaction: convert_redaction(summary.redaction),
        provenance: summary
            .provenance
            .iter()
            .map(convert_provenance_entry)
            .collect(),
    };

    for (name, metric) in &summary.metrics {
        if metric.kind == models::MetricValueKind::Measured {
            if let Some(value) = metric.value {
                normalized.metrics.insert(
                    name.clone(),
                    MetricValue {
                        value,
                        unit: metric.unit.clone(),
                        volatility: infer_metric_volatility(name, metric.unit.as_deref()),
                        source_field: Some(format!("metrics.{name}")),
                    },
                );
            } else {
                normalized
                    .degraded
                    .push(SummaryDegradation::missing_metric(name));
            }
        } else {
            normalized
                .degraded
                .push(SummaryDegradation::missing_metric(name));
        }
    }

    if summary.redaction == models::RedactionPosture::Uncertain {
        normalized
            .degraded
            .push(SummaryDegradation::redaction_uncertain("*"));
    }

    normalized
}

fn convert_artifact_kind(kind: models::ArtifactKind) -> ArtifactKind {
    match kind {
        models::ArtifactKind::BenchmarkReport => ArtifactKind::BenchmarkReport,
        models::ArtifactKind::SupportBundleManifest => ArtifactKind::SupportBundleManifest,
        models::ArtifactKind::ExplainPerformanceReport => ArtifactKind::ExplainPerformanceReport,
        models::ArtifactKind::ProfileEvidence => ArtifactKind::ProfileEvidence,
        models::ArtifactKind::CacheReport => ArtifactKind::CacheReport,
        models::ArtifactKind::WriteQueueReport => ArtifactKind::WriteQueueReport,
        models::ArtifactKind::SwarmContentionReport => ArtifactKind::SwarmContentionReport,
    }
}

fn convert_summary_degradation(degradation: &models::SummaryDegradation) -> SummaryDegradation {
    SummaryDegradation {
        code: degradation.code.as_str().to_owned(),
        severity: convert_artifact_severity(degradation.severity),
        message: degradation.message.clone(),
        affected_field: degradation.field_path.clone(),
        repair: degradation.repair.clone(),
    }
}

fn summary_degradation_to_budget(degradation: &SummaryDegradation) -> BudgetCheckDegradation {
    BudgetCheckDegradation {
        code: degradation.code.clone(),
        severity: degradation.severity,
        affected_field: degradation.affected_field.clone(),
        message: degradation.message.clone(),
        repair: degradation.repair.clone(),
    }
}

fn convert_artifact_severity(severity: models::ArtifactDegradationSeverity) -> Severity {
    match severity {
        models::ArtifactDegradationSeverity::Low => Severity::Low,
        models::ArtifactDegradationSeverity::Medium => Severity::Medium,
        models::ArtifactDegradationSeverity::High => Severity::High,
    }
}

fn convert_redaction(redaction: models::RedactionPosture) -> RedactionPosture {
    match redaction {
        models::RedactionPosture::Clean => RedactionPosture::default(),
        models::RedactionPosture::Redacted => RedactionPosture {
            applied: true,
            uncertain_fields: Vec::new(),
            secret_indicators: 0,
        },
        models::RedactionPosture::Uncertain => RedactionPosture {
            applied: true,
            uncertain_fields: vec!["*".to_owned()],
            secret_indicators: 0,
        },
    }
}

fn convert_provenance_entry(entry: &models::ProvenanceEntry) -> ProvenanceEntry {
    let source = entry.source_line.map_or_else(
        || entry.source_path.clone(),
        |line| format!("{}:{line}", entry.source_path),
    );
    ProvenanceEntry {
        field: entry.field.clone(),
        source,
        confidence: None,
    }
}

fn infer_metric_volatility(name: &str, unit: Option<&str>) -> Volatility {
    let lower_name = name.to_lowercase();
    let lower_unit = unit.unwrap_or_default().to_lowercase();
    if lower_name.contains("elapsed")
        || lower_name.contains("duration")
        || lower_name.contains("latency")
        || lower_name.contains("time")
        || matches!(
            lower_unit.as_str(),
            "ms" | "s" | "sec" | "second" | "seconds"
        )
    {
        Volatility::Timing
    } else if lower_name.contains("rss")
        || lower_name.contains("memory")
        || lower_name.contains("cpu")
        || lower_name.contains("load")
        || matches!(lower_unit.as_str(), "byte" | "bytes" | "kb" | "mb" | "gb")
    {
        Volatility::Resource
    } else {
        Volatility::Stable
    }
}

/// Compare two artifact summaries deterministically.
#[must_use]
pub fn compare_artifacts(baseline: &ArtifactSummary, candidate: &ArtifactSummary) -> CompareReport {
    let mut deltas = Vec::new();
    let mut degraded = Vec::new();
    let mut owner_evidence: BTreeMap<SubsystemOwner, Vec<String>> = BTreeMap::new();

    // Check schema compatibility
    if baseline.schema != ARTIFACT_SUMMARY_SCHEMA {
        degraded.push(CompareDegradation {
            code: "unsupported_schema".to_string(),
            severity: Severity::High,
            artifact_side: ArtifactSide::Baseline,
            affected_field: Some("schema".to_string()),
            message: format!(
                "Baseline artifact uses unsupported schema {}",
                baseline.schema
            ),
            repair: Some("Normalize the baseline artifact with the current ee version".to_string()),
        });
    }
    if candidate.schema != ARTIFACT_SUMMARY_SCHEMA {
        degraded.push(CompareDegradation {
            code: "unsupported_schema".to_string(),
            severity: Severity::High,
            artifact_side: ArtifactSide::Candidate,
            affected_field: Some("schema".to_string()),
            message: format!(
                "Candidate artifact uses unsupported schema {}",
                candidate.schema
            ),
            repair: Some(
                "Normalize the candidate artifact with the current ee version".to_string(),
            ),
        });
    }

    // Check for hash tampering
    if baseline.hash_mismatch() {
        degraded.push(CompareDegradation {
            code: "tampered_hash".to_string(),
            severity: Severity::High,
            artifact_side: ArtifactSide::Baseline,
            affected_field: None,
            message: "Baseline artifact has mismatched hash".to_string(),
            repair: Some("Verify artifact integrity".to_string()),
        });
    }
    if candidate.hash_mismatch() {
        degraded.push(CompareDegradation {
            code: "tampered_hash".to_string(),
            severity: Severity::High,
            artifact_side: ArtifactSide::Candidate,
            affected_field: None,
            message: "Candidate artifact has mismatched hash".to_string(),
            repair: Some("Verify artifact integrity".to_string()),
        });
    }

    // Check for profile mismatch
    if let (Some(bp), Some(cp)) = (&baseline.profile, &candidate.profile) {
        if bp != cp {
            degraded.push(CompareDegradation {
                code: "profile_mismatch".to_string(),
                severity: Severity::Medium,
                artifact_side: ArtifactSide::Both,
                affected_field: Some("profile".to_string()),
                message: format!("Profile mismatch: baseline={bp}, candidate={cp}"),
                repair: Some("Compare artifacts from the same profile".to_string()),
            });
        }
    }

    // Check for fixture-tier mismatch
    if let (Some(bt), Some(ct)) = (&baseline.fixture_tier, &candidate.fixture_tier) {
        if bt != ct {
            degraded.push(CompareDegradation {
                code: "fixture_tier_mismatch".to_string(),
                severity: Severity::Medium,
                artifact_side: ArtifactSide::Both,
                affected_field: Some("fixtureTier".to_string()),
                message: format!("Fixture tier mismatch: baseline={bt}, candidate={ct}"),
                repair: Some("Compare artifacts from the same fixture tier".to_string()),
            });
        }
    }

    // Propagate summary degradations
    for d in &baseline.degraded {
        degraded.push(CompareDegradation {
            code: d.code.clone(),
            severity: d.severity,
            artifact_side: ArtifactSide::Baseline,
            affected_field: d.affected_field.clone(),
            message: d.message.clone(),
            repair: d.repair.clone(),
        });
    }
    for d in &candidate.degraded {
        degraded.push(CompareDegradation {
            code: d.code.clone(),
            severity: d.severity,
            artifact_side: ArtifactSide::Candidate,
            affected_field: d.affected_field.clone(),
            message: d.message.clone(),
            repair: d.repair.clone(),
        });
    }

    // Collect all metric names from both artifacts
    let mut all_metrics: Vec<&String> = baseline.metrics.keys().collect();
    for k in candidate.metrics.keys() {
        if !all_metrics.contains(&k) {
            all_metrics.push(k);
        }
    }
    all_metrics.sort();

    // Compute deltas
    for metric in all_metrics {
        let b_val = baseline.metrics.get(metric);
        let c_val = candidate.metrics.get(metric);
        let delta = MetricDelta::from_values(metric, b_val, c_val);

        if matches!(
            delta.direction,
            DeltaDirection::Missing | DeltaDirection::Added
        ) {
            degraded.push(CompareDegradation {
                code: "metric_missing".to_string(),
                severity: Severity::Medium,
                artifact_side: if delta.direction == DeltaDirection::Missing {
                    ArtifactSide::Candidate
                } else {
                    ArtifactSide::Baseline
                },
                affected_field: Some(delta.metric.clone()),
                message: format!(
                    "Metric {} is missing on one side of the comparison",
                    delta.metric
                ),
                repair: Some(
                    "Compare artifacts produced by the same fixture and command family".to_string(),
                ),
            });
        }

        // Collect evidence for owner hints
        if delta_is_regression(&delta) || delta_is_improvement(&delta) {
            let evidence = format!(
                "{}: {} -> {} ({:+.1}%)",
                metric,
                delta
                    .baseline_value
                    .map_or("N/A".to_string(), |v| format!("{v:.2}")),
                delta
                    .candidate_value
                    .map_or("N/A".to_string(), |v| format!("{v:.2}")),
                delta.delta_percent.unwrap_or(0.0)
            );
            owner_evidence
                .entry(delta.owner_hint)
                .or_default()
                .push(evidence);
        }

        deltas.push(delta);
    }

    // Sort deltas by severity (descending), then metric name
    deltas.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.metric.cmp(&b.metric))
    });

    // Sort degradations
    degraded.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.code.cmp(&b.code))
            .then_with(|| format!("{:?}", a.artifact_side).cmp(&format!("{:?}", b.artifact_side)))
    });

    // Compute summary stats
    let regression_count = deltas.iter().filter(|d| delta_is_regression(d)).count();
    let improvement_count = deltas.iter().filter(|d| delta_is_improvement(d)).count();
    let worst_severity = deltas
        .iter()
        .map(|d| d.severity)
        .max()
        .unwrap_or(Severity::Low);
    let worst_degrade_severity = degraded.iter().map(|d| d.severity).max();

    // Determine overall result
    let result = if !degraded.iter().any(|d| d.code == "tampered_hash") {
        if regression_count > 0 && improvement_count > 0 {
            CompareResult::Mixed
        } else if regression_count > 0 {
            CompareResult::Regressed
        } else if improvement_count > 0 {
            CompareResult::Improved
        } else if deltas.is_empty() {
            CompareResult::Inconclusive
        } else {
            CompareResult::Unchanged
        }
    } else {
        CompareResult::Inconclusive
    };

    // Determine confidence
    let confidence = if degraded.iter().any(|d| d.severity >= Severity::High) {
        Confidence::Low
    } else if degraded.iter().any(|d| d.severity >= Severity::Medium) {
        Confidence::Medium
    } else {
        Confidence::High
    };

    // Build owner hints
    let mut owner_hints: Vec<OwnerHint> = owner_evidence
        .into_iter()
        .filter(|(_, evidence)| !evidence.is_empty())
        .map(|(owner, evidence)| OwnerHint {
            owner,
            confidence: if evidence.len() > 2 {
                Confidence::High
            } else if evidence.len() > 1 {
                Confidence::Medium
            } else {
                Confidence::Low
            },
            evidence,
        })
        .collect();
    owner_hints.sort_by(|a, b| {
        b.confidence
            .cmp(&a.confidence)
            .then_with(|| a.owner.cmp(&b.owner))
    });

    // Build next commands
    let mut next_commands = Vec::new();
    if result == CompareResult::Regressed {
        next_commands.push("ee doctor --json".to_string());
        if owner_hints.iter().any(|h| h.owner == SubsystemOwner::Db) {
            next_commands.push("ee status --json".to_string());
        }
        if owner_hints
            .iter()
            .any(|h| h.owner == SubsystemOwner::Profile)
        {
            next_commands.push("ee profile config plan --json".to_string());
        }
    }

    CompareReport {
        schema: COMPARE_RESULT_SCHEMA_V1,
        artifacts: CompareArtifacts {
            baseline: baseline.clone(),
            candidate: candidate.clone(),
        },
        summary: CompareSummary {
            result,
            confidence,
            worst_severity: worst_degrade_severity
                .map_or(worst_severity, |ds| ds.max(worst_severity)),
            delta_count: deltas.len(),
            regression_count,
            improvement_count,
        },
        deltas,
        owner_hints,
        degraded,
        next_commands,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn artifact_kind_roundtrip() {
        for kind in [
            ArtifactKind::BenchmarkReport,
            ArtifactKind::SupportBundleManifest,
            ArtifactKind::ExplainPerformanceReport,
            ArtifactKind::ProfileEvidence,
            ArtifactKind::CacheReport,
            ArtifactKind::WriteQueueReport,
            ArtifactKind::SwarmContentionReport,
        ] {
            let s = kind.as_str();
            let parsed = ArtifactKind::parse(s).expect("should parse");
            assert_eq!(kind, parsed);
        }
    }

    #[cfg(unix)]
    #[test]
    fn perf_artifact_path_rejects_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().expect("tempdir");
        let real_parent = tempdir.path().join("real-artifacts");
        std::fs::create_dir(&real_parent).expect("real parent");
        std::fs::write(real_parent.join("summary.json"), "{}").expect("summary");
        let linked_parent = tempdir.path().join("linked-artifacts");
        symlink(&real_parent, &linked_parent).expect("symlink parent");

        let error = validate_perf_artifact_path(&linked_parent.join("summary.json"))
            .expect_err("symlinked parent should reject");
        assert!(
            error.to_string().contains("symlinked component"),
            "unexpected error: {error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn perf_artifact_path_rejects_symlinked_file() {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().expect("tempdir");
        let real_summary = tempdir.path().join("real-summary.json");
        std::fs::write(&real_summary, "{}").expect("summary");
        let linked_summary = tempdir.path().join("linked-summary.json");
        symlink(&real_summary, &linked_summary).expect("symlink file");

        let error =
            validate_perf_artifact_path(&linked_summary).expect_err("symlinked file should reject");
        assert!(
            error.to_string().contains("symlinked component"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn subsystem_owner_inference() {
        assert_eq!(
            SubsystemOwner::infer_from_metric("search_elapsed_ms"),
            SubsystemOwner::Search
        );
        assert_eq!(
            SubsystemOwner::infer_from_metric("pack_tokens"),
            SubsystemOwner::Pack
        );
        assert_eq!(
            SubsystemOwner::infer_from_metric("db_generation"),
            SubsystemOwner::Db
        );
        assert_eq!(
            SubsystemOwner::infer_from_metric("rss_bytes"),
            SubsystemOwner::HostPressure
        );
        assert_eq!(
            SubsystemOwner::infer_from_metric("cache_hit_rate"),
            SubsystemOwner::Cache
        );
        assert_eq!(
            SubsystemOwner::infer_from_metric("write_queue_depth"),
            SubsystemOwner::WriteSpool
        );
        assert_eq!(
            SubsystemOwner::infer_from_metric("profile_selected"),
            SubsystemOwner::Profile
        );
        assert_eq!(
            SubsystemOwner::infer_from_metric("unknown_metric"),
            SubsystemOwner::Unknown
        );
    }

    #[test]
    fn compare_unchanged_artifacts() {
        let baseline = ArtifactSummary::new("baseline", ArtifactKind::BenchmarkReport)
            .with_metric("elapsed_ms", MetricValue::timing(100.0))
            .with_metric("count", MetricValue::stable(42.0));

        let candidate = ArtifactSummary::new("candidate", ArtifactKind::BenchmarkReport)
            .with_metric("elapsed_ms", MetricValue::timing(100.0))
            .with_metric("count", MetricValue::stable(42.0));

        let report = compare_artifacts(&baseline, &candidate);

        assert_eq!(report.summary.result, CompareResult::Unchanged);
        assert_eq!(report.summary.confidence, Confidence::High);
        assert_eq!(report.summary.regression_count, 0);
        assert_eq!(report.summary.improvement_count, 0);
    }

    #[test]
    fn compare_regressed_artifacts() {
        let baseline = ArtifactSummary::new("baseline", ArtifactKind::BenchmarkReport)
            .with_metric("elapsed_ms", MetricValue::timing(100.0));

        let candidate = ArtifactSummary::new("candidate", ArtifactKind::BenchmarkReport)
            .with_metric("elapsed_ms", MetricValue::timing(250.0)); // 150% increase

        let report = compare_artifacts(&baseline, &candidate);

        assert_eq!(report.summary.result, CompareResult::Regressed);
        assert!(report.summary.regression_count > 0);
    }

    #[test]
    fn compare_improved_artifacts() {
        let baseline = ArtifactSummary::new("baseline", ArtifactKind::BenchmarkReport)
            .with_metric("elapsed_ms", MetricValue::timing(200.0));

        let candidate = ArtifactSummary::new("candidate", ArtifactKind::BenchmarkReport)
            .with_metric("elapsed_ms", MetricValue::timing(50.0)); // 75% decrease

        let report = compare_artifacts(&baseline, &candidate);

        assert_eq!(report.summary.result, CompareResult::Improved);
        assert!(report.summary.improvement_count > 0);
    }

    #[test]
    fn compare_with_missing_metric() {
        let baseline = ArtifactSummary::new("baseline", ArtifactKind::BenchmarkReport)
            .with_metric("elapsed_ms", MetricValue::timing(100.0))
            .with_metric("memory_mb", MetricValue::resource(512.0, "MB"));

        let candidate = ArtifactSummary::new("candidate", ArtifactKind::BenchmarkReport)
            .with_metric("elapsed_ms", MetricValue::timing(100.0));
        // memory_mb is missing

        let report = compare_artifacts(&baseline, &candidate);

        assert!(
            report
                .deltas
                .iter()
                .any(|d| d.metric == "memory_mb" && d.direction == DeltaDirection::Missing)
        );
        assert!(report.degraded.iter().any(
            |d| d.code == "metric_missing" && d.affected_field.as_deref() == Some("memory_mb")
        ));
    }

    #[test]
    fn compare_with_profile_mismatch() {
        let mut baseline = ArtifactSummary::new("baseline", ArtifactKind::ProfileEvidence);
        baseline.profile = Some("workstation".to_string());

        let mut candidate = ArtifactSummary::new("candidate", ArtifactKind::ProfileEvidence);
        candidate.profile = Some("swarm".to_string());

        let report = compare_artifacts(&baseline, &candidate);

        assert!(report.degraded.iter().any(|d| d.code == "profile_mismatch"));
        assert!(report.summary.confidence < Confidence::High);
    }

    #[test]
    fn compare_with_fixture_tier_mismatch() {
        let mut baseline = ArtifactSummary::new("baseline", ArtifactKind::BenchmarkReport);
        baseline.fixture_tier = Some("smoke".to_string());

        let mut candidate = ArtifactSummary::new("candidate", ArtifactKind::BenchmarkReport);
        candidate.fixture_tier = Some("stress".to_string());

        let report = compare_artifacts(&baseline, &candidate);

        assert!(
            report
                .degraded
                .iter()
                .any(|d| d.code == "fixture_tier_mismatch")
        );
        assert!(report.summary.confidence < Confidence::High);
    }

    #[test]
    fn compare_with_unsupported_schema() {
        let mut baseline = ArtifactSummary::new("baseline", ArtifactKind::BenchmarkReport);
        baseline.schema = "ee.perf.artifact_summary.v0".to_owned();

        let candidate = ArtifactSummary::new("candidate", ArtifactKind::BenchmarkReport);
        let report = compare_artifacts(&baseline, &candidate);

        assert!(
            report
                .degraded
                .iter()
                .any(|d| d.code == "unsupported_schema")
        );
        assert_eq!(report.summary.confidence, Confidence::Low);
    }

    #[test]
    fn compare_cache_hit_rate_drop_is_regression() {
        let baseline = ArtifactSummary::new("baseline", ArtifactKind::CacheReport)
            .with_metric("cache_hit_rate", MetricValue::stable(0.95));

        let candidate = ArtifactSummary::new("candidate", ArtifactKind::CacheReport)
            .with_metric("cache_hit_rate", MetricValue::stable(0.40));

        let report = compare_artifacts(&baseline, &candidate);

        assert_eq!(report.summary.result, CompareResult::Regressed);
        assert_eq!(report.summary.regression_count, 1);
        assert!(
            report
                .owner_hints
                .iter()
                .any(|hint| hint.owner == SubsystemOwner::Cache)
        );
    }

    #[test]
    fn compare_write_queue_depth_regression_points_to_write_spool() {
        let baseline = ArtifactSummary::new("baseline", ArtifactKind::WriteQueueReport)
            .with_metric("write_queue_depth", MetricValue::stable(2.0));

        let candidate = ArtifactSummary::new("candidate", ArtifactKind::WriteQueueReport)
            .with_metric("write_queue_depth", MetricValue::stable(30.0));

        let report = compare_artifacts(&baseline, &candidate);

        assert_eq!(report.summary.result, CompareResult::Regressed);
        assert!(
            report
                .owner_hints
                .iter()
                .any(|hint| hint.owner == SubsystemOwner::WriteSpool)
        );
    }

    #[test]
    fn compare_memory_regression_points_to_host_pressure() {
        let baseline = ArtifactSummary::new("baseline", ArtifactKind::BenchmarkReport)
            .with_metric("rss_bytes", MetricValue::resource(1024.0, "bytes"));

        let candidate = ArtifactSummary::new("candidate", ArtifactKind::BenchmarkReport)
            .with_metric("rss_bytes", MetricValue::resource(4096.0, "bytes"));

        let report = compare_artifacts(&baseline, &candidate);

        assert_eq!(report.summary.result, CompareResult::Regressed);
        assert!(
            report
                .owner_hints
                .iter()
                .any(|hint| hint.owner == SubsystemOwner::HostPressure)
        );
    }

    #[test]
    fn compare_with_tampered_hash() {
        let mut baseline = ArtifactSummary::new("baseline", ArtifactKind::SupportBundleManifest);
        baseline.content_hash = Some("declared_hash".to_string());
        baseline.observed_hash = Some("different_hash".to_string());

        let candidate = ArtifactSummary::new("candidate", ArtifactKind::SupportBundleManifest);

        let report = compare_artifacts(&baseline, &candidate);

        assert!(report.degraded.iter().any(|d| d.code == "tampered_hash"));
        assert_eq!(report.summary.result, CompareResult::Inconclusive);
    }

    #[test]
    fn compare_deterministic_ordering() {
        let baseline = ArtifactSummary::new("baseline", ArtifactKind::BenchmarkReport)
            .with_metric("z_metric", MetricValue::stable(1.0))
            .with_metric("a_metric", MetricValue::stable(1.0))
            .with_metric("m_metric", MetricValue::stable(1.0));

        let candidate = ArtifactSummary::new("candidate", ArtifactKind::BenchmarkReport)
            .with_metric("z_metric", MetricValue::stable(1.0))
            .with_metric("a_metric", MetricValue::stable(1.0))
            .with_metric("m_metric", MetricValue::stable(1.0));

        let report1 = compare_artifacts(&baseline, &candidate);
        let report2 = compare_artifacts(&baseline, &candidate);

        // Ordering should be deterministic
        let metrics1: Vec<_> = report1.deltas.iter().map(|d| &d.metric).collect();
        let metrics2: Vec<_> = report2.deltas.iter().map(|d| &d.metric).collect();
        assert_eq!(metrics1, metrics2);
    }

    #[test]
    fn summary_serializes_to_json() {
        let summary = ArtifactSummary::new("test", ArtifactKind::CacheReport)
            .with_metric("hit_rate", MetricValue::stable(0.95));

        let json = serde_json::to_string(&summary).expect("should serialize");
        assert!(json.contains("ee.perf.artifact_summary.v1"));
        assert!(json.contains("cache_report"));
    }

    #[test]
    fn compare_report_serializes_to_json() {
        let baseline = ArtifactSummary::new("baseline", ArtifactKind::BenchmarkReport);
        let candidate = ArtifactSummary::new("candidate", ArtifactKind::BenchmarkReport);

        let report = compare_artifacts(&baseline, &candidate);
        let json = serde_json::to_string(&report).expect("should serialize");

        assert!(json.contains("ee.perf.compare.v1"));
    }
}
