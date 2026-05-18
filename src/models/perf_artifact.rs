//! Performance artifact summary schema for regression forensics (mwjq.2).
//!
//! Defines redaction-safe normalized summaries of benchmark reports, support-bundle
//! manifests, explain-performance reports, profile evidence, cache reports, write-queue
//! reports, and swarm-contention reports. These summaries are consumed by the compare
//! surface without exposing raw logs, queries, host paths, or secrets.

use std::{collections::BTreeMap, fmt, str::FromStr};

use serde::{Deserialize, Serialize};

pub const ARTIFACT_SUMMARY_SCHEMA_V1: &str = "ee.perf.artifact_summary.v1";
pub const PERF_METRIC_SCHEMA_V1: &str = "ee.perf.metric.v1";
pub const PERF_SCHEMA_CATALOG_V1: &str = "ee.perf.schema_catalog.v1";
const JSON_SCHEMA_DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
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

    pub const ALL: [ArtifactKind; 7] = [
        Self::BenchmarkReport,
        Self::SupportBundleManifest,
        Self::ExplainPerformanceReport,
        Self::ProfileEvidence,
        Self::CacheReport,
        Self::WriteQueueReport,
        Self::SwarmContentionReport,
    ];
}

impl fmt::Display for ArtifactKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseArtifactKindError(pub String);

impl fmt::Display for ParseArtifactKindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown artifact kind: {}", self.0)
    }
}

impl std::error::Error for ParseArtifactKindError {}

impl FromStr for ArtifactKind {
    type Err = ParseArtifactKindError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "benchmark_report" => Ok(Self::BenchmarkReport),
            "support_bundle_manifest" => Ok(Self::SupportBundleManifest),
            "explain_performance_report" => Ok(Self::ExplainPerformanceReport),
            "profile_evidence" => Ok(Self::ProfileEvidence),
            "cache_report" => Ok(Self::CacheReport),
            "write_queue_report" => Ok(Self::WriteQueueReport),
            "swarm_contention_report" => Ok(Self::SwarmContentionReport),
            other => Err(ParseArtifactKindError(other.to_owned())),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricValueKind {
    Measured,
    Unavailable,
    Redacted,
    NonComparable,
}

impl MetricValueKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Measured => "measured",
            Self::Unavailable => "unavailable",
            Self::Redacted => "redacted",
            Self::NonComparable => "non_comparable",
        }
    }
}

impl fmt::Display for MetricValueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricValue {
    pub kind: MetricValueKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

impl MetricValue {
    #[must_use]
    pub fn measured(value: f64, unit: impl Into<String>) -> Self {
        Self {
            kind: MetricValueKind::Measured,
            value: Some(value),
            unit: Some(unit.into()),
        }
    }

    #[must_use]
    pub const fn unavailable() -> Self {
        Self {
            kind: MetricValueKind::Unavailable,
            value: None,
            unit: None,
        }
    }

    #[must_use]
    pub const fn redacted() -> Self {
        Self {
            kind: MetricValueKind::Redacted,
            value: None,
            unit: None,
        }
    }

    #[must_use]
    pub const fn non_comparable() -> Self {
        Self {
            kind: MetricValueKind::NonComparable,
            value: None,
            unit: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionPosture {
    Clean,
    Redacted,
    Uncertain,
}

impl RedactionPosture {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Redacted => "redacted",
            Self::Uncertain => "uncertain",
        }
    }
}

impl fmt::Display for RedactionPosture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryDegradationCode {
    MissingMetric,
    ProfileMismatch,
    StaleSchemaVersion,
    TamperedHash,
    UnsupportedArtifactKind,
    RedactionUncertain,
    SourceUnavailable,
    MalformedSource,
}

impl SummaryDegradationCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingMetric => "missing_metric",
            Self::ProfileMismatch => "profile_mismatch",
            Self::StaleSchemaVersion => "stale_schema_version",
            Self::TamperedHash => "tampered_hash",
            Self::UnsupportedArtifactKind => "unsupported_artifact_kind",
            Self::RedactionUncertain => "redaction_uncertain",
            Self::SourceUnavailable => "source_unavailable",
            Self::MalformedSource => "malformed_source",
        }
    }
}

impl fmt::Display for SummaryDegradationCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactDegradationSeverity {
    Low,
    Medium,
    High,
}

impl ArtifactDegradationSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

impl fmt::Display for ArtifactDegradationSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryDegradation {
    pub code: SummaryDegradationCode,
    pub severity: ArtifactDegradationSeverity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field_path: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair: Option<String>,
}

impl SummaryDegradation {
    #[must_use]
    pub fn missing_metric(metric_name: &str, artifact_id: Option<&str>) -> Self {
        Self {
            code: SummaryDegradationCode::MissingMetric,
            severity: ArtifactDegradationSeverity::Medium,
            artifact_id: artifact_id.map(String::from),
            field_path: Some(format!("metrics.{metric_name}")),
            message: format!("Metric `{metric_name}` is not available in source artifact."),
            repair: Some("Ensure the source artifact includes the expected metric.".to_owned()),
        }
    }

    #[must_use]
    pub fn tampered_hash(artifact_id: &str, declared: &str, observed: &str) -> Self {
        Self {
            code: SummaryDegradationCode::TamperedHash,
            severity: ArtifactDegradationSeverity::High,
            artifact_id: Some(artifact_id.to_owned()),
            field_path: Some("contentHash".to_owned()),
            message: format!(
                "Declared hash `{declared}` does not match observed hash `{observed}`."
            ),
            repair: Some("Re-generate the artifact or use --inspection-only mode.".to_owned()),
        }
    }

    #[must_use]
    pub fn stale_schema(schema: &str, artifact_id: Option<&str>) -> Self {
        Self {
            code: SummaryDegradationCode::StaleSchemaVersion,
            severity: ArtifactDegradationSeverity::Medium,
            artifact_id: artifact_id.map(String::from),
            field_path: Some("schema".to_owned()),
            message: format!(
                "Schema `{schema}` is older than current; normalization may lose fields."
            ),
            repair: Some("Re-generate the artifact with the current ee version.".to_owned()),
        }
    }

    #[must_use]
    pub fn redaction_uncertain(artifact_id: Option<&str>, field_path: Option<&str>) -> Self {
        Self {
            code: SummaryDegradationCode::RedactionUncertain,
            severity: ArtifactDegradationSeverity::High,
            artifact_id: artifact_id.map(String::from),
            field_path: field_path.map(String::from),
            message:
                "Source artifact redaction state is uncertain; summary is not safe for comparison."
                    .to_owned(),
            repair: Some(
                "Re-generate the artifact through a redaction-aware support bundle path."
                    .to_owned(),
            ),
        }
    }

    #[must_use]
    pub fn unsupported_kind(kind: &str) -> Self {
        Self {
            code: SummaryDegradationCode::UnsupportedArtifactKind,
            severity: ArtifactDegradationSeverity::High,
            artifact_id: None,
            field_path: Some("artifactKind".to_owned()),
            message: format!("Artifact kind `{kind}` is not supported for summarization."),
            repair: Some("Use one of: benchmark_report, support_bundle_manifest, explain_performance_report, profile_evidence, cache_report, write_queue_report, swarm_contention_report.".to_owned()),
        }
    }
}

impl Ord for SummaryDegradation {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.severity
            .cmp(&other.severity)
            .reverse()
            .then_with(|| self.code.cmp(&other.code))
            .then_with(|| self.artifact_id.cmp(&other.artifact_id))
            .then_with(|| self.field_path.cmp(&other.field_path))
    }
}

impl PartialOrd for SummaryDegradation {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceEntry {
    pub field: String,
    pub source_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_line: Option<u32>,
}

impl Ord for ProvenanceEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.field
            .cmp(&other.field)
            .then_with(|| self.source_path.cmp(&other.source_path))
    }
}

impl PartialOrd for ProvenanceEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Eq for ProvenanceEntry {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileReference {
    pub profile_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub override_source: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactSummary {
    pub schema: String,
    pub artifact_id: String,
    pub artifact_kind: ArtifactKind,
    pub source_schema: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<ProfileReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixture_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_family: Option<String>,
    pub metrics: BTreeMap<String, MetricValue>,
    pub degraded: Vec<SummaryDegradation>,
    pub redaction: RedactionPosture,
    pub provenance: Vec<ProvenanceEntry>,
}

impl ArtifactSummary {
    #[must_use]
    pub fn new(
        artifact_id: impl Into<String>,
        kind: ArtifactKind,
        source_schema: impl Into<String>,
    ) -> Self {
        Self {
            schema: ARTIFACT_SUMMARY_SCHEMA_V1.to_owned(),
            artifact_id: artifact_id.into(),
            artifact_kind: kind,
            source_schema: source_schema.into(),
            source_path: None,
            content_hash: None,
            observed_hash: None,
            profile: None,
            fixture_tier: None,
            command_family: None,
            metrics: BTreeMap::new(),
            degraded: Vec::new(),
            redaction: RedactionPosture::Clean,
            provenance: Vec::new(),
        }
    }

    pub fn with_source_path(mut self, path: impl Into<String>) -> Self {
        self.source_path = Some(path.into());
        self
    }

    pub fn with_content_hash(mut self, hash: impl Into<String>) -> Self {
        self.content_hash = Some(hash.into());
        self
    }

    pub fn with_observed_hash(mut self, hash: impl Into<String>) -> Self {
        self.observed_hash = Some(hash.into());
        self
    }

    pub fn with_profile(mut self, profile: ProfileReference) -> Self {
        self.profile = Some(profile);
        self
    }

    pub fn with_fixture_tier(mut self, tier: impl Into<String>) -> Self {
        self.fixture_tier = Some(tier.into());
        self
    }

    pub fn with_command_family(mut self, family: impl Into<String>) -> Self {
        self.command_family = Some(family.into());
        self
    }

    pub fn add_metric(&mut self, name: impl Into<String>, value: MetricValue) {
        self.metrics.insert(name.into(), value);
    }

    pub fn add_degradation(&mut self, degradation: SummaryDegradation) {
        self.degraded.push(degradation);
        self.degraded.sort();
    }

    pub fn add_provenance(&mut self, entry: ProvenanceEntry) {
        self.provenance.push(entry);
        self.provenance.sort();
    }

    pub fn set_redaction(&mut self, posture: RedactionPosture) {
        self.redaction = posture;
    }

    pub fn verify_hash(&mut self) -> bool {
        match (&self.content_hash, &self.observed_hash) {
            (Some(declared), Some(observed)) if declared != observed => {
                self.add_degradation(SummaryDegradation::tampered_hash(
                    &self.artifact_id,
                    declared,
                    observed,
                ));
                false
            }
            _ => true,
        }
    }

    #[must_use]
    pub fn has_critical_degradation(&self) -> bool {
        self.degraded
            .iter()
            .any(|d| d.severity == ArtifactDegradationSeverity::High)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DegradedSummary {
    pub schema: String,
    pub artifact_id: String,
    pub degraded: Vec<SummaryDegradation>,
}

impl DegradedSummary {
    #[must_use]
    pub fn unsupported(artifact_id: impl Into<String>, kind: &str) -> Self {
        Self {
            schema: ARTIFACT_SUMMARY_SCHEMA_V1.to_owned(),
            artifact_id: artifact_id.into(),
            degraded: vec![SummaryDegradation::unsupported_kind(kind)],
        }
    }

    #[must_use]
    pub fn malformed(artifact_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            schema: ARTIFACT_SUMMARY_SCHEMA_V1.to_owned(),
            artifact_id: artifact_id.into(),
            degraded: vec![SummaryDegradation {
                code: SummaryDegradationCode::MalformedSource,
                severity: ArtifactDegradationSeverity::High,
                artifact_id: None,
                field_path: None,
                message: message.into(),
                repair: Some("Check the source artifact format and retry.".to_owned()),
            }],
        }
    }

    #[must_use]
    pub fn source_unavailable(artifact_id: impl Into<String>, path: &str) -> Self {
        Self {
            schema: ARTIFACT_SUMMARY_SCHEMA_V1.to_owned(),
            artifact_id: artifact_id.into(),
            degraded: vec![SummaryDegradation {
                code: SummaryDegradationCode::SourceUnavailable,
                severity: ArtifactDegradationSeverity::High,
                artifact_id: None,
                field_path: Some("sourcePath".to_owned()),
                message: format!("Source artifact at `{path}` could not be read."),
                repair: Some("Verify the path exists and is readable.".to_owned()),
            }],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerfSchemaCatalog {
    pub schema: String,
    pub schemas: Vec<PerfSchemaEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerfSchemaEntry {
    pub name: String,
    pub version: u32,
    pub description: String,
}

#[must_use]
pub fn perf_schemas() -> Vec<PerfSchemaEntry> {
    vec![
        PerfSchemaEntry {
            name: ARTIFACT_SUMMARY_SCHEMA_V1.to_owned(),
            version: 1,
            description: "Normalized artifact summary for regression comparison.".to_owned(),
        },
        PerfSchemaEntry {
            name: PERF_METRIC_SCHEMA_V1.to_owned(),
            version: 1,
            description: "Individual metric value with kind and unit.".to_owned(),
        },
        PerfSchemaEntry {
            name: PERF_SCHEMA_CATALOG_V1.to_owned(),
            version: 1,
            description: "Performance forensics schema catalog.".to_owned(),
        },
    ]
}

#[must_use]
pub fn perf_schema_catalog() -> PerfSchemaCatalog {
    PerfSchemaCatalog {
        schema: PERF_SCHEMA_CATALOG_V1.to_owned(),
        schemas: perf_schemas(),
    }
}

#[must_use]
pub fn perf_schema_catalog_json() -> String {
    let catalog = perf_schema_catalog();
    let mut output = String::from("{\n");
    output.push_str(&format!("  \"schema\": \"{PERF_SCHEMA_CATALOG_V1}\",\n"));
    output.push_str("  \"schemas\": [\n");
    for (index, schema) in catalog.schemas.iter().enumerate() {
        output.push_str("    {\n");
        output.push_str(&format!(
            "      \"$schema\": \"{JSON_SCHEMA_DRAFT_2020_12}\",\n"
        ));
        output.push_str("      \"$id\": ");
        push_json_string(&mut output, &schema.name);
        output.push_str(",\n");
        output.push_str("      \"eeSchema\": ");
        push_json_string(&mut output, &schema.name);
        output.push_str(",\n");
        output.push_str(&format!("      \"version\": {},\n", schema.version));
        output.push_str("      \"description\": ");
        push_json_string(&mut output, &schema.description);
        output.push_str("\n    }");
        if index + 1 < catalog.schemas.len() {
            output.push(',');
        }
        output.push('\n');
    }
    output.push_str("  ]\n}");
    output
}

fn push_json_string(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            other if (other as u32) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(output, "\\u{:04x}", other as u32);
            }
            other => output.push(other),
        }
    }
    output.push('"');
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn artifact_kind_roundtrip() {
        for kind in ArtifactKind::ALL {
            let s = kind.as_str();
            let parsed: ArtifactKind = s.parse().unwrap();
            assert_eq!(kind, parsed);
        }
    }

    #[test]
    fn artifact_kind_parse_error() {
        let result: Result<ArtifactKind, _> = "unknown_kind".parse();
        assert!(result.is_err());
    }

    #[test]
    fn metric_value_measured() {
        let m = MetricValue::measured(123.5, "ms");
        assert_eq!(m.kind, MetricValueKind::Measured);
        assert_eq!(m.value, Some(123.5));
        assert_eq!(m.unit.as_deref(), Some("ms"));
    }

    #[test]
    fn metric_value_unavailable() {
        let m = MetricValue::unavailable();
        assert_eq!(m.kind, MetricValueKind::Unavailable);
        assert!(m.value.is_none());
    }

    #[test]
    fn summary_with_metrics() {
        let mut summary =
            ArtifactSummary::new("test-001", ArtifactKind::BenchmarkReport, "ee.bench.v1");
        summary.add_metric("elapsed_ms", MetricValue::measured(42.0, "ms"));
        summary.add_metric("rss_bytes", MetricValue::measured(1024.0, "bytes"));

        assert_eq!(summary.metrics.len(), 2);
        assert!(summary.metrics.contains_key("elapsed_ms"));
        assert!(summary.metrics.contains_key("rss_bytes"));
    }

    #[test]
    fn degradations_sorted_by_severity() {
        let mut summary =
            ArtifactSummary::new("test-002", ArtifactKind::CacheReport, "ee.cache.v1");
        summary.add_degradation(SummaryDegradation::missing_metric(
            "hit_rate",
            Some("test-002"),
        ));
        summary.add_degradation(SummaryDegradation::tampered_hash("test-002", "abc", "def"));
        summary.add_degradation(SummaryDegradation::stale_schema(
            "ee.cache.v0",
            Some("test-002"),
        ));

        assert_eq!(summary.degraded.len(), 3);
        assert_eq!(
            summary.degraded[0].severity,
            ArtifactDegradationSeverity::High
        );
        assert_eq!(
            summary.degraded[1].severity,
            ArtifactDegradationSeverity::Medium
        );
    }

    #[test]
    fn hash_verification_detects_mismatch() {
        let mut summary =
            ArtifactSummary::new("test-003", ArtifactKind::ProfileEvidence, "ee.profile.v1")
                .with_content_hash("declared123")
                .with_observed_hash("observed456");

        let valid = summary.verify_hash();
        assert!(!valid);
        assert!(summary.has_critical_degradation());
    }

    #[test]
    fn hash_verification_passes_on_match() {
        let mut summary =
            ArtifactSummary::new("test-004", ArtifactKind::ProfileEvidence, "ee.profile.v1")
                .with_content_hash("samehash")
                .with_observed_hash("samehash");

        let valid = summary.verify_hash();
        assert!(valid);
        assert!(!summary.has_critical_degradation());
    }

    #[test]
    fn degraded_summary_unsupported() {
        let degraded = DegradedSummary::unsupported("artifact-x", "unknown_kind");
        assert_eq!(degraded.degraded.len(), 1);
        assert_eq!(
            degraded.degraded[0].code,
            SummaryDegradationCode::UnsupportedArtifactKind
        );
    }

    #[test]
    fn provenance_deterministic_ordering() {
        let mut summary =
            ArtifactSummary::new("test-005", ArtifactKind::BenchmarkReport, "ee.bench.v1");
        summary.add_provenance(ProvenanceEntry {
            field: "zulu".to_owned(),
            source_path: "a.json".to_owned(),
            source_line: None,
        });
        summary.add_provenance(ProvenanceEntry {
            field: "alpha".to_owned(),
            source_path: "b.json".to_owned(),
            source_line: Some(10),
        });

        assert_eq!(summary.provenance[0].field, "alpha");
        assert_eq!(summary.provenance[1].field, "zulu");
    }

    #[test]
    fn json_serialization_stable() {
        let summary = ArtifactSummary::new("stable-001", ArtifactKind::CacheReport, "ee.cache.v1")
            .with_source_path("redacted/path")
            .with_fixture_tier("smoke");

        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"schema\":\"ee.perf.artifact_summary.v1\""));
        assert!(json.contains("\"artifactKind\":\"cache_report\""));
    }

    #[test]
    fn perf_schema_catalog_contents() {
        let catalog = perf_schema_catalog();
        assert_eq!(catalog.schema, PERF_SCHEMA_CATALOG_V1);
        assert_eq!(catalog.schemas.len(), 3);
    }

    #[test]
    fn perf_schema_catalog_json_is_stable_and_parseable() {
        let json = perf_schema_catalog_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["schema"], PERF_SCHEMA_CATALOG_V1);
        assert_eq!(parsed["schemas"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn metrics_btreemap_deterministic_order() {
        let mut summary =
            ArtifactSummary::new("order-test", ArtifactKind::BenchmarkReport, "ee.bench.v1");
        summary.add_metric("zulu", MetricValue::measured(3.0, "ms"));
        summary.add_metric("alpha", MetricValue::measured(1.0, "ms"));
        summary.add_metric("bravo", MetricValue::measured(2.0, "ms"));

        let keys: Vec<_> = summary.metrics.keys().collect();
        assert_eq!(keys, vec!["alpha", "bravo", "zulu"]);
    }
}
