//! Support-bundle perf compare adapter coverage for mwjq.7.
//!
//! These tests use small synthetic bundles and verify only normalized summaries:
//! source hashes, section presence, profile labels, redaction posture, and stable
//! degradation codes. Raw bundle contents must not appear in compare output.

use std::fs;
use std::path::PathBuf;

use ee::core::perf_forensics::compare_normalized_artifacts;
use ee::core::support_bundle::{
    BundleManifest, ManifestEntry, SUPPORT_BUNDLE_MANIFEST_SCHEMA_V1,
    summarize_bundle_for_perf_compare,
};
use ee::models::{
    ArtifactDegradationSeverity, ArtifactKind, RedactionPosture, SummaryDegradationCode,
};
use serde_json::json;

type TestResult = Result<(), String>;

const PROFILE_EVIDENCE_FILE: &str = "profile_evidence.json";
const SCALE_BENCHMARK_SUMMARY_FILE: &str = "scale_benchmark_summary.json";
const SCALE_FIXTURE_MANIFEST_FILE: &str = "scale_fixture_manifest.json";
const CACHE_REPORTS_FILE: &str = "scale_cache_reports.json";
const WRITE_QUEUE_REPORT_FILE: &str = "scale_write_queue_report.json";
const PERFORMANCE_EXPLAIN_SAMPLES_FILE: &str = "scale_performance_explain_samples.json";
const TRIAGE_SUMMARY_FILE: &str = "scale_triage_summary.json";
const MANIFEST_FILE: &str = "manifest.json";

struct SyntheticBundle {
    _tempdir: tempfile::TempDir,
    path: PathBuf,
}

struct FileSpec {
    path: &'static str,
    content: String,
    declared_content: Option<String>,
    redacted: bool,
}

fn profile_evidence(profile: &str) -> String {
    json!({
        "schema": "ee.support_bundle.profile_evidence.v1",
        "redactionStatus": "profile_name_only",
        "profile": {
            "activeProfile": profile,
            "recommendedProfile": profile,
            "effectiveProfile": profile,
            "source": "test_fixture",
            "confidence": "high",
            "reasons": ["synthetic support bundle"]
        }
    })
    .to_string()
}

fn complete_files(profile: &str) -> Vec<FileSpec> {
    vec![
        FileSpec {
            path: PROFILE_EVIDENCE_FILE,
            content: profile_evidence(profile),
            declared_content: None,
            redacted: true,
        },
        FileSpec {
            path: SCALE_BENCHMARK_SUMMARY_FILE,
            content: json!({
                "schema": "ee.support_bundle.scale_benchmark_summary.v1",
                "swarmSmokeReports": [
                    {
                        "schema": "ee.swarm_contention.report.v1",
                        "scenario": "mixed_read_write_contention",
                        "processCount": 4,
                        "successCount": 4,
                        "failureCount": 0,
                        "totalDurationMs": 42
                    }
                ]
            })
            .to_string(),
            declared_content: None,
            redacted: true,
        },
        FileSpec {
            path: SCALE_FIXTURE_MANIFEST_FILE,
            content: json!({
                "schema": "ee.support_bundle.scale_fixture_manifest.v1",
                "manifest": {"fixtures": ["portable", "swarm"]}
            })
            .to_string(),
            declared_content: None,
            redacted: true,
        },
        FileSpec {
            path: CACHE_REPORTS_FILE,
            content: json!({
                "schema": "ee.support_bundle.scale_cache_reports.v1",
                "database": {"memoryCount": 12}
            })
            .to_string(),
            declared_content: None,
            redacted: true,
        },
        FileSpec {
            path: WRITE_QUEUE_REPORT_FILE,
            content: json!({
                "schema": "ee.support_bundle.write_queue_report.v1",
                "status": "not_attached"
            })
            .to_string(),
            declared_content: None,
            redacted: true,
        },
        FileSpec {
            path: PERFORMANCE_EXPLAIN_SAMPLES_FILE,
            content: json!({
                "schema": "ee.support_bundle.performance_explain_samples.v1",
                "sampleCount": 2,
                "samples": [
                    {"contentHash": "hash-a"},
                    {"contentHash": "hash-b"}
                ]
            })
            .to_string(),
            declared_content: None,
            redacted: true,
        },
        FileSpec {
            path: TRIAGE_SUMMARY_FILE,
            content: json!({
                "schema": "ee.support_bundle.scale_triage.v1",
                "ownerSignals": []
            })
            .to_string(),
            declared_content: None,
            redacted: true,
        },
    ]
}

fn write_bundle(
    name: &str,
    schema: &str,
    files: Vec<FileSpec>,
    redaction_applied: bool,
) -> Result<SyntheticBundle, String> {
    let tempdir = tempfile::Builder::new()
        .prefix("ee-support-bundle-perf-")
        .tempdir()
        .map_err(|error| format!("tempdir: {error}"))?;
    let path = tempdir.path().join(name);
    fs::create_dir_all(&path).map_err(|error| format!("create bundle dir: {error}"))?;

    let mut entries = Vec::new();
    for file in files {
        fs::write(path.join(file.path), &file.content)
            .map_err(|error| format!("write {}: {error}", file.path))?;
        let declared_content = file.declared_content.as_deref().unwrap_or(&file.content);
        entries.push(ManifestEntry {
            path: file.path.to_owned(),
            size_bytes: file.content.len() as u64,
            content_hash: hash(declared_content),
            redacted: file.redacted,
        });
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));

    let manifest = BundleManifest {
        schema: schema.to_owned(),
        bundle_id: name.to_owned(),
        created_at: "2026-05-08T00:00:00Z".to_owned(),
        workspace_path: "/redacted/workspace".to_owned(),
        ee_version: "0.1.0-test".to_owned(),
        files: entries,
        total_size_bytes: 0,
        redaction_applied,
        redaction_reasons: vec!["synthetic_redaction".to_owned()],
    };
    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|error| format!("manifest json: {error}"))?;
    fs::write(path.join(MANIFEST_FILE), manifest_json)
        .map_err(|error| format!("write manifest: {error}"))?;

    Ok(SyntheticBundle {
        _tempdir: tempdir,
        path,
    })
}

fn hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

fn metric_value(summary: &ee::models::ArtifactSummary, metric: &str) -> Result<f64, String> {
    summary
        .metrics
        .get(metric)
        .and_then(|value| value.value)
        .ok_or_else(|| format!("missing metric {metric}"))
}

fn has_degradation(
    summary: &ee::models::ArtifactSummary,
    code: SummaryDegradationCode,
    field_fragment: &str,
) -> bool {
    summary.degraded.iter().any(|degradation| {
        degradation.code == code
            && degradation
                .field_path
                .as_deref()
                .is_some_and(|field| field.contains(field_fragment))
    })
}

#[test]
fn complete_bundle_summarizes_redacted_sections_and_hashes() -> TestResult {
    let bundle = write_bundle(
        "complete-portable",
        SUPPORT_BUNDLE_MANIFEST_SCHEMA_V1,
        complete_files("portable"),
        true,
    )?;

    let summary =
        summarize_bundle_for_perf_compare(&bundle.path).map_err(|error| error.message())?;

    assert_eq!(summary.artifact_kind, ArtifactKind::SupportBundleManifest);
    assert_eq!(summary.redaction, RedactionPosture::Redacted);
    assert_eq!(
        summary
            .profile
            .as_ref()
            .map(|profile| profile.profile_name.as_str()),
        Some("portable")
    );
    assert!(summary.degraded.is_empty());
    assert_eq!(summary.content_hash, summary.observed_hash);
    assert_eq!(
        metric_value(&summary, "section.profile_evidence.present")?,
        1.0
    );
    assert_eq!(
        metric_value(&summary, "section.benchmark_summary.present")?,
        1.0
    );
    assert_eq!(
        metric_value(&summary, "section.cache_reports.present")?,
        1.0
    );
    assert_eq!(
        metric_value(&summary, "performance_explain.sample_count")?,
        2.0
    );
    assert_eq!(
        metric_value(&summary, "swarm_contention.report_count")?,
        1.0
    );

    let rendered = serde_json::to_string(&summary).map_err(|error| error.to_string())?;
    assert!(
        !rendered.contains("sk_live"),
        "summary must not include raw secret-like bundle content"
    );

    Ok(())
}

#[test]
fn partial_bundle_reports_missing_sections_with_repairs() -> TestResult {
    let bundle = write_bundle(
        "partial-portable",
        SUPPORT_BUNDLE_MANIFEST_SCHEMA_V1,
        vec![FileSpec {
            path: PROFILE_EVIDENCE_FILE,
            content: profile_evidence("portable"),
            declared_content: None,
            redacted: true,
        }],
        true,
    )?;

    let summary =
        summarize_bundle_for_perf_compare(&bundle.path).map_err(|error| error.message())?;

    assert_eq!(
        metric_value(&summary, "section.profile_evidence.present")?,
        1.0
    );
    assert_eq!(
        metric_value(&summary, "section.cache_reports.present")?,
        0.0
    );
    assert!(has_degradation(
        &summary,
        SummaryDegradationCode::MissingMetric,
        CACHE_REPORTS_FILE
    ));
    assert!(summary.degraded.iter().all(|degradation| {
        degradation
            .repair
            .as_deref()
            .is_some_and(|repair| !repair.is_empty())
    }));

    Ok(())
}

#[test]
fn tampered_bundle_reports_hash_mismatch_before_comparison() -> TestResult {
    let mut files = complete_files("workstation");
    for file in &mut files {
        if file.path == CACHE_REPORTS_FILE {
            file.declared_content = Some(
                json!({
                    "schema": "ee.support_bundle.scale_cache_reports.v1",
                    "database": {"memoryCount": 999}
                })
                .to_string(),
            );
        }
    }
    let bundle = write_bundle(
        "tampered-workstation",
        SUPPORT_BUNDLE_MANIFEST_SCHEMA_V1,
        files,
        true,
    )?;

    let summary =
        summarize_bundle_for_perf_compare(&bundle.path).map_err(|error| error.message())?;

    assert_ne!(summary.content_hash, summary.observed_hash);
    assert!(has_degradation(
        &summary,
        SummaryDegradationCode::TamperedHash,
        CACHE_REPORTS_FILE
    ));
    assert!(summary.degraded.iter().any(|degradation| {
        degradation.code == SummaryDegradationCode::TamperedHash
            && degradation.severity == ArtifactDegradationSeverity::High
    }));

    Ok(())
}

#[test]
fn mismatched_profile_bundles_degrade_compare_confidence() -> TestResult {
    let baseline = write_bundle(
        "baseline-portable",
        SUPPORT_BUNDLE_MANIFEST_SCHEMA_V1,
        complete_files("portable"),
        true,
    )?;
    let candidate = write_bundle(
        "candidate-swarm",
        SUPPORT_BUNDLE_MANIFEST_SCHEMA_V1,
        complete_files("swarm"),
        true,
    )?;
    let baseline_summary =
        summarize_bundle_for_perf_compare(&baseline.path).map_err(|error| error.message())?;
    let candidate_summary =
        summarize_bundle_for_perf_compare(&candidate.path).map_err(|error| error.message())?;

    let report = compare_normalized_artifacts(&baseline_summary, &candidate_summary);

    assert!(report.degraded.iter().any(|degradation| {
        degradation.code == "profile_mismatch"
            && degradation.affected_field.as_deref() == Some("profile")
    }));
    let rendered = serde_json::to_string(&report).map_err(|error| error.to_string())?;
    assert!(
        !rendered.contains("mixed_read_write_contention"),
        "compare output must not copy raw support-bundle report contents"
    );

    Ok(())
}

#[test]
fn unsupported_bundle_manifest_version_is_inconclusive() -> TestResult {
    let bundle = write_bundle(
        "unsupported-version",
        "ee.support_bundle.manifest.v0",
        complete_files("portable"),
        true,
    )?;

    let summary =
        summarize_bundle_for_perf_compare(&bundle.path).map_err(|error| error.message())?;

    assert!(has_degradation(
        &summary,
        SummaryDegradationCode::StaleSchemaVersion,
        "manifest.schema"
    ));
    assert!(summary.degraded.iter().any(|degradation| {
        degradation.code == SummaryDegradationCode::StaleSchemaVersion
            && degradation.severity == ArtifactDegradationSeverity::High
    }));

    Ok(())
}
