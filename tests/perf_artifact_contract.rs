//! Contract tests for perf artifact summary schema (mwjq.2).
//!
//! Verifies that the artifact summary schema:
//! - Is registered in KNOWN_SCHEMAS
//! - Produces stable JSON output matching golden fixtures
//! - Handles all artifact kinds correctly
//! - Maintains deterministic ordering for metrics, degradations, provenance

#![allow(clippy::expect_used, clippy::unwrap_used)]

use ee::models::{
    ARTIFACT_SUMMARY_SCHEMA_V1, ArtifactKind, ArtifactSummary, DegradedSummary, KNOWN_SCHEMAS,
    MetricValue, PERF_METRIC_SCHEMA_V1, PERF_SCHEMA_CATALOG_V1, ProfileReference, ProvenanceEntry,
    RedactionPosture, SummaryDegradation, perf_schema_catalog, perf_schema_catalog_json,
};

#[test]
fn perf_schemas_in_known_schemas() {
    assert!(
        KNOWN_SCHEMAS.contains(&ARTIFACT_SUMMARY_SCHEMA_V1),
        "ARTIFACT_SUMMARY_SCHEMA_V1 should be in KNOWN_SCHEMAS"
    );
    assert!(
        KNOWN_SCHEMAS.contains(&PERF_METRIC_SCHEMA_V1),
        "PERF_METRIC_SCHEMA_V1 should be in KNOWN_SCHEMAS"
    );
    assert!(
        KNOWN_SCHEMAS.contains(&PERF_SCHEMA_CATALOG_V1),
        "PERF_SCHEMA_CATALOG_V1 should be in KNOWN_SCHEMAS"
    );
}

#[test]
fn artifact_summary_golden_benchmark() {
    let mut summary = ArtifactSummary::new(
        "bench-001",
        ArtifactKind::BenchmarkReport,
        "ee.bench.smoke.v1",
    )
    .with_source_path("redacted/benchmark.json")
    .with_content_hash("abc123")
    .with_observed_hash("abc123")
    .with_profile(ProfileReference {
        profile_name: "workstation".to_owned(),
        confidence: Some("high".to_owned()),
        override_source: None,
    })
    .with_fixture_tier("smoke")
    .with_command_family("pack");

    summary.add_metric("elapsed_ms", MetricValue::measured(42.5, "ms"));
    summary.add_metric("rss_bytes", MetricValue::measured(10485760.0, "bytes"));
    summary.add_provenance(ProvenanceEntry {
        field: "elapsed_ms".to_owned(),
        source_path: "benchmark.json".to_owned(),
        source_line: Some(15),
    });

    let json = serde_json::to_string_pretty(&summary).expect("serialize");
    let golden = include_str!("fixtures/golden/perf_artifact/summary_benchmark.golden");
    let golden_trimmed = golden.trim();
    let json_trimmed = json.trim();

    assert_eq!(
        json_trimmed, golden_trimmed,
        "Artifact summary JSON should match golden fixture"
    );
}

#[test]
fn all_artifact_kinds_serialize() {
    for kind in ArtifactKind::ALL {
        let summary = ArtifactSummary::new(format!("test-{}", kind.as_str()), kind, "ee.test.v1");
        let json = serde_json::to_string(&summary);
        assert!(json.is_ok(), "Failed to serialize {kind}");

        let parsed: serde_json::Value = serde_json::from_str(&json.unwrap()).unwrap();
        assert_eq!(
            parsed["artifactKind"].as_str().unwrap(),
            kind.as_str(),
            "Round-trip for {kind}"
        );
    }
}

#[test]
fn degraded_summary_stable_output() {
    let degraded = DegradedSummary::unsupported("artifact-x", "unknown_kind");
    let json = serde_json::to_string(&degraded).expect("serialize");

    assert!(json.contains("\"schema\":\"ee.perf.artifact_summary.v1\""));
    assert!(json.contains("unsupported_artifact_kind"));
}

#[test]
fn schema_catalog_advertises_all_schemas() {
    let catalog = perf_schema_catalog();
    let names: Vec<_> = catalog.schemas.iter().map(|s| s.name.as_str()).collect();

    assert!(names.contains(&ARTIFACT_SUMMARY_SCHEMA_V1));
    assert!(names.contains(&PERF_METRIC_SCHEMA_V1));
    assert!(names.contains(&PERF_SCHEMA_CATALOG_V1));
}

#[test]
fn schema_catalog_json_exports_catalog_id() {
    let json = perf_schema_catalog_json();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed["schema"], PERF_SCHEMA_CATALOG_V1);
    assert_eq!(parsed["schemas"].as_array().unwrap().len(), 3);
}

#[test]
fn metrics_ordering_deterministic() {
    let mut summary = ArtifactSummary::new("order-test", ArtifactKind::CacheReport, "ee.cache.v1");
    summary.add_metric("zulu", MetricValue::measured(3.0, "ms"));
    summary.add_metric("alpha", MetricValue::measured(1.0, "ms"));
    summary.add_metric("bravo", MetricValue::measured(2.0, "ms"));

    let json1 = serde_json::to_string(&summary).unwrap();
    let json2 = serde_json::to_string(&summary).unwrap();

    assert_eq!(json1, json2, "JSON output should be deterministic");
    assert!(
        json1.find("\"alpha\"").unwrap() < json1.find("\"bravo\"").unwrap(),
        "alpha should come before bravo"
    );
    assert!(
        json1.find("\"bravo\"").unwrap() < json1.find("\"zulu\"").unwrap(),
        "bravo should come before zulu"
    );
}

#[test]
fn degradations_ordering_by_severity_then_code() {
    let mut summary =
        ArtifactSummary::new("deg-test", ArtifactKind::ProfileEvidence, "ee.profile.v1");
    summary.add_degradation(SummaryDegradation::missing_metric("m1", Some("deg-test")));
    summary.add_degradation(SummaryDegradation::tampered_hash("deg-test", "a", "b"));
    summary.add_degradation(SummaryDegradation::stale_schema(
        "ee.old.v0",
        Some("deg-test"),
    ));

    let json = serde_json::to_string(&summary).unwrap();
    let tampered_pos = json.find("tampered_hash").unwrap();
    let stale_pos = json.find("stale_schema_version").unwrap();
    let missing_pos = json.find("missing_metric").unwrap();

    assert!(
        tampered_pos < stale_pos && tampered_pos < missing_pos,
        "High severity (tampered_hash) should come first"
    );
}

#[test]
fn provenance_ordering_by_field() {
    let mut summary =
        ArtifactSummary::new("prov-test", ArtifactKind::BenchmarkReport, "ee.bench.v1");
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

    let json = serde_json::to_string(&summary).unwrap();
    let alpha_pos = json.find("\"alpha\"").unwrap();
    let zulu_pos = json.find("\"zulu\"").unwrap();

    assert!(
        alpha_pos < zulu_pos,
        "alpha should come before zulu in provenance"
    );
}

#[test]
fn hash_verification_adds_degradation() {
    let mut summary = ArtifactSummary::new(
        "hash-test",
        ArtifactKind::SupportBundleManifest,
        "ee.support.v1",
    )
    .with_content_hash("declared")
    .with_observed_hash("different");

    assert!(summary.degraded.is_empty());
    let valid = summary.verify_hash();
    assert!(!valid);
    assert_eq!(summary.degraded.len(), 1);
    assert!(summary.has_critical_degradation());
}

#[test]
fn missing_optional_fields_stay_absent_from_summary_json() {
    let summary = ArtifactSummary::new("minimal", ArtifactKind::ProfileEvidence, "ee.profile.v1");
    let json = serde_json::to_string(&summary).unwrap();

    assert!(!json.contains("\"sourcePath\""));
    assert!(!json.contains("\"contentHash\""));
    assert!(!json.contains("\"profile\":"));
}

#[test]
fn redaction_uncertainty_is_explicit_and_high_severity() {
    let mut summary = ArtifactSummary::new(
        "redaction-test",
        ArtifactKind::ExplainPerformanceReport,
        "ee.search.performance_explain.v1",
    );
    summary.set_redaction(RedactionPosture::Uncertain);
    summary.add_degradation(SummaryDegradation::redaction_uncertain(
        Some("redaction-test"),
        Some("redaction"),
    ));

    let json = serde_json::to_string(&summary).unwrap();

    assert!(json.contains("\"redaction\":\"uncertain\""));
    assert!(json.contains("redaction_uncertain"));
    assert!(summary.has_critical_degradation());
}
