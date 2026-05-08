//! Differential performance forensics: read-only compare of performance artifacts.
//!
//! This module implements ADR 0024: read-only regression comparison of benchmark
//! reports, support-bundle manifests, explain-performance reports, profile evidence,
//! cache reports, write-queue reports, and swarm-contention reports.
//!
//! The compare surface is read-only and does not mutate memory, indexes, profiles,
//! caches, or beads state.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const ARTIFACT_SUMMARY_SCHEMA_V1: &str = "ee.perf.artifact_summary.v1";
pub const COMPARE_RESULT_SCHEMA_V1: &str = "ee.perf.compare.v1";

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
    pub fn from_str(s: &str) -> Option<Self> {
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
    pub schema: &'static str,
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
            schema: ARTIFACT_SUMMARY_SCHEMA_V1,
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
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubsystemOwner {
    Retrieval,
    Packing,
    Storage,
    Indexing,
    Cache,
    WriteOwner,
    Profile,
    Support,
    Runtime,
    Unknown,
}

impl SubsystemOwner {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Retrieval => "retrieval",
            Self::Packing => "packing",
            Self::Storage => "storage",
            Self::Indexing => "indexing",
            Self::Cache => "cache",
            Self::WriteOwner => "write_owner",
            Self::Profile => "profile",
            Self::Support => "support",
            Self::Runtime => "runtime",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub fn infer_from_metric(metric_name: &str) -> Self {
        let lower = metric_name.to_lowercase();
        if lower.contains("search") || lower.contains("query") || lower.contains("candidate") {
            Self::Retrieval
        } else if lower.contains("pack") || lower.contains("token") || lower.contains("context") {
            Self::Packing
        } else if lower.contains("db") || lower.contains("storage") || lower.contains("migration") {
            Self::Storage
        } else if lower.contains("index") || lower.contains("rebuild") {
            Self::Indexing
        } else if lower.contains("cache") || lower.contains("hotset") || lower.contains("prewarm") {
            Self::Cache
        } else if lower.contains("write") || lower.contains("spool") || lower.contains("queue") {
            Self::WriteOwner
        } else if lower.contains("profile") || lower.contains("budget") {
            Self::Profile
        } else if lower.contains("bundle") || lower.contains("support") {
            Self::Support
        } else if lower.contains("runtime") || lower.contains("cancel") || lower.contains("async") {
            Self::Runtime
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

/// Compare two artifact summaries deterministically.
#[must_use]
pub fn compare_artifacts(baseline: &ArtifactSummary, candidate: &ArtifactSummary) -> CompareReport {
    let mut deltas = Vec::new();
    let mut degraded = Vec::new();
    let mut owner_evidence: BTreeMap<SubsystemOwner, Vec<String>> = BTreeMap::new();

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

        // Collect evidence for owner hints
        if delta.severity >= Severity::Medium {
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
    let regression_count = deltas
        .iter()
        .filter(|d| d.direction == DeltaDirection::Increased && d.severity >= Severity::Medium)
        .count();
    let improvement_count = deltas
        .iter()
        .filter(|d| d.direction == DeltaDirection::Decreased && d.severity >= Severity::Medium)
        .count();
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
    owner_hints.sort_by(|a, b| b.confidence.cmp(&a.confidence));

    // Build next commands
    let mut next_commands = Vec::new();
    if result == CompareResult::Regressed {
        next_commands.push("ee doctor --json".to_string());
        if owner_hints
            .iter()
            .any(|h| h.owner == SubsystemOwner::Indexing)
        {
            next_commands.push("ee index status --json".to_string());
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
            let parsed = ArtifactKind::from_str(s).expect("should parse");
            assert_eq!(kind, parsed);
        }
    }

    #[test]
    fn subsystem_owner_inference() {
        assert_eq!(
            SubsystemOwner::infer_from_metric("search_elapsed_ms"),
            SubsystemOwner::Retrieval
        );
        assert_eq!(
            SubsystemOwner::infer_from_metric("pack_tokens"),
            SubsystemOwner::Packing
        );
        assert_eq!(
            SubsystemOwner::infer_from_metric("db_generation"),
            SubsystemOwner::Storage
        );
        assert_eq!(
            SubsystemOwner::infer_from_metric("index_rebuild_ms"),
            SubsystemOwner::Indexing
        );
        assert_eq!(
            SubsystemOwner::infer_from_metric("cache_hit_rate"),
            SubsystemOwner::Cache
        );
        assert_eq!(
            SubsystemOwner::infer_from_metric("write_queue_depth"),
            SubsystemOwner::WriteOwner
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
