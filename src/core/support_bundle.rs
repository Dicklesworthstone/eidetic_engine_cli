//! Redacted diagnostic support bundle (EE-DIAG-001, eidetic_engine_cli-wtpl).
//!
//! Creates redacted diagnostic bundles containing:
//! - Status report (ee status --json)
//! - Doctor report (ee doctor --json)
//! - Recent audit entries
//! - Schema version
//! - Index manifest
//! - Capabilities matrix
//!
//! All content is passed through the secret redaction scanner before being
//! written to the bundle directory.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use blake3::Hasher;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlmodel_core::{Row as SqlRow, Value as SqlValue};

use crate::cache::CacheBudget;
use crate::db::DbConnection;
use crate::models::{
    ArtifactDegradationSeverity, ArtifactKind, ArtifactSummary, DomainError, MetricValue,
    ProfileReference, ProvenanceEntry, RedactionLevel, RedactionPosture, SummaryDegradation,
    SummaryDegradationCode,
};
use crate::output;
use crate::pack::{
    PackCacheGovernor, PackHotset, PackHotsetEntry, PackHotsetEntryKind, PackSection,
    prewarm_pack_hotset,
};
use crate::policy::{redact_secret_like_content, redaction_placeholder};
use crate::search::{SearchCacheGovernor, SearchHotset, SearchHotsetEntry, prewarm_search_hotset};

use super::doctor::DoctorReport;
use super::singleflight::singleflight_posture_report;
use super::status::{
    STATUS_BENCH_GROUP_NAME, STATUS_BENCH_HARD_CEILING_MS, STATUS_BENCH_QUICK_ITERATIONS,
    STATUS_BENCH_SCALES, StatusReport,
};
use super::write_owner::{WriteSpool, WriteSpoolConfig};

pub const SUPPORT_BUNDLE_SCHEMA_V1: &str = "ee.support_bundle.v1";
pub const SUPPORT_BUNDLE_MANIFEST_SCHEMA_V1: &str = "ee.support_bundle.manifest.v1";
pub const SUPPORT_BUNDLE_INSPECT_SCHEMA_V1: &str = "ee.support_bundle.inspect.v1";

const MANIFEST_FILE: &str = "manifest.json";
const STATUS_FILE: &str = "status.json";
const DOCTOR_FILE: &str = "doctor.json";
const AUDIT_FILE: &str = "audit.jsonl";
const CAPABILITIES_FILE: &str = "capabilities.json";
const SCHEMA_FILE: &str = "schema_version.json";
const PROFILE_EVIDENCE_FILE: &str = "profile_evidence.json";
const AGENT_PROFILE_EVIDENCE_FILE: &str = "agent_profile_evidence.json";
const SCALE_BENCHMARK_SUMMARY_FILE: &str = "scale_benchmark_summary.json";
const SCALE_FIXTURE_MANIFEST_FILE: &str = "scale_fixture_manifest.json";
const CACHE_REPORTS_FILE: &str = "scale_cache_reports.json";
const WRITE_QUEUE_REPORT_FILE: &str = "scale_write_queue_report.json";
const PERFORMANCE_EXPLAIN_SAMPLES_FILE: &str = "scale_performance_explain_samples.json";
const PERFORMANCE_EXPLAIN_SAMPLE_DIR: &str = "performance-explain";
const MAX_PERFORMANCE_EXPLAIN_SAMPLES: usize = 16;
const PACK_REPLAY_SUMMARY_FILE: &str = "pack_replay_summary.json";
const MAX_PACK_REPLAY_SUMMARY_RECORDS: usize = 16;
const SWARM_BRIEF_SUMMARY_FILE: &str = "swarm_brief_summary.json";
const COORDINATION_FALLBACK_SUMMARY_FILE: &str = "coordination_fallback_summary.json";
const COORDINATION_FALLBACK_LEDGER_FILE: &str = "coordination-fallback-evidence.jsonl";
const MAX_COORDINATION_FALLBACK_SUMMARY_RECORDS: usize = 16;
const SINGLEFLIGHT_POSTURE_FILE: &str = "singleflight_posture.json";
const QOS_LANE_SUMMARY_FILE: &str = "qos_lane_summary.json";
const TRIAGE_SUMMARY_FILE: &str = "scale_triage_summary.json";
const TAILSCALE_METADATA_FIELDS: &[&str] = &[
    "selfNodeKey",
    "selfTailscaleIp",
    "selfMagicDnsName",
    "tailnetId",
    "tailnetDisplayName",
    "selfAdvertisedTags",
    "binaryVersionRaw",
    "binaryAbsolutePath",
];
const PERF_COMPARE_BUNDLE_SECTIONS: [(&str, &str); 8] = [
    ("profile_evidence", PROFILE_EVIDENCE_FILE),
    ("benchmark_summary", SCALE_BENCHMARK_SUMMARY_FILE),
    ("fixture_manifest", SCALE_FIXTURE_MANIFEST_FILE),
    ("cache_reports", CACHE_REPORTS_FILE),
    ("write_queue_report", WRITE_QUEUE_REPORT_FILE),
    (
        "performance_explain_samples",
        PERFORMANCE_EXPLAIN_SAMPLES_FILE,
    ),
    ("swarm_brief_summary", SWARM_BRIEF_SUMMARY_FILE),
    ("swarm_contention_reports", SCALE_BENCHMARK_SUMMARY_FILE),
];
const SWARM_SCALE_WORKLOADS_MANIFEST: &str =
    include_str!("../../tests/fixtures/swarm_scale/workloads.json");

/// Options for creating a support bundle.
#[derive(Clone, Debug)]
pub struct BundleOptions {
    pub workspace: PathBuf,
    pub output_dir: Option<PathBuf>,
    pub dry_run: bool,
    pub redacted: bool,
    pub redaction_level: RedactionLevel,
    pub include_raw: bool,
    pub audit_limit: u32,
}

impl Default for BundleOptions {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            output_dir: None,
            dry_run: false,
            redacted: true,
            redaction_level: RedactionLevel::Paranoid,
            include_raw: false,
            audit_limit: 100,
        }
    }
}

impl BundleOptions {
    #[must_use]
    pub const fn effective_redaction_level(&self) -> RedactionLevel {
        if !self.redacted || self.include_raw {
            RedactionLevel::None
        } else {
            self.redaction_level
        }
    }
}

/// Options for inspecting an existing bundle.
#[derive(Clone, Debug)]
pub struct InspectOptions {
    pub bundle_path: PathBuf,
    pub verify_hashes: bool,
}

/// Entry in the bundle manifest describing one collected file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub path: String,
    pub size_bytes: u64,
    pub content_hash: String,
    pub redacted: bool,
}

/// Manifest stored in the bundle directory.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleManifest {
    pub schema: String,
    pub bundle_id: String,
    pub created_at: String,
    pub workspace_path: String,
    pub ee_version: String,
    pub files: Vec<ManifestEntry>,
    pub total_size_bytes: u64,
    pub redaction_applied: bool,
    pub redaction_reasons: Vec<String>,
}

/// Redaction summary for the bundle report.
#[derive(Clone, Debug, Serialize)]
pub struct RedactionSummary {
    pub total_redactions: u32,
    pub reasons: Vec<String>,
}

/// Report from creating or planning a bundle.
#[derive(Clone, Debug, Serialize)]
pub struct BundleReport {
    pub schema: String,
    pub bundle_id: String,
    pub files_collected: Vec<String>,
    pub total_size_bytes: u64,
    pub redaction_applied: bool,
    pub redaction_level: RedactionLevel,
    pub redaction_summary: RedactionSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_hash: Option<String>,
    pub dry_run: bool,
    pub workspace_path: String,
}

impl BundleReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        json!({
            "schema": self.schema,
            "bundleId": self.bundle_id,
            "filesCollected": self.files_collected,
            "totalSizeBytes": self.total_size_bytes,
            "redactionApplied": self.redaction_applied,
            "redactionLevel": self.redaction_level.as_str(),
            "redactionSummary": {
                "totalRedactions": self.redaction_summary.total_redactions,
                "reasons": self.redaction_summary.reasons
            },
            "outputPath": self.output_path,
            "manifestHash": self.manifest_hash,
            "dryRun": self.dry_run,
            "workspacePath": self.workspace_path
        })
    }
}

/// Report from inspecting a bundle.
#[derive(Clone, Debug, Serialize)]
pub struct InspectReport {
    pub schema: String,
    pub bundle_path: PathBuf,
    pub manifest: Option<BundleManifest>,
    pub files_found: Vec<String>,
    pub total_size_bytes: u64,
    pub hash_verified: bool,
    pub hash_mismatches: Vec<String>,
    pub valid: bool,
}

impl InspectReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        json!({
            "schema": self.schema,
            "bundlePath": self.bundle_path.display().to_string(),
            "manifest": self.manifest,
            "filesFound": self.files_found,
            "totalSizeBytes": self.total_size_bytes,
            "hashVerified": self.hash_verified,
            "hashMismatches": self.hash_mismatches,
            "valid": self.valid
        })
    }
}

/// Collected diagnostic data before redaction.
struct CollectedDiagnostics {
    status_json: String,
    doctor_json: String,
    audit_json: String,
    capabilities_json: String,
    schema_json: String,
    profile_evidence_json: String,
    agent_profile_evidence_json: String,
    scale_benchmark_summary_json: String,
    scale_fixture_manifest_json: String,
    cache_reports_json: String,
    write_queue_report_json: String,
    performance_explain_samples_json: String,
    pack_replay_summary_json: String,
    swarm_brief_summary_json: String,
    coordination_fallback_summary_json: String,
    singleflight_posture_json: String,
    qos_lane_summary_json: String,
    triage_summary_json: String,
}

/// Plan what would be collected without actually creating the bundle.
pub fn plan_bundle(options: &BundleOptions) -> Result<BundleReport, DomainError> {
    let workspace_path = options
        .workspace
        .canonicalize()
        .unwrap_or_else(|_| options.workspace.clone());

    let bundle_id = generate_bundle_id();
    let files_collected = planned_files();

    Ok(BundleReport {
        schema: SUPPORT_BUNDLE_SCHEMA_V1.to_owned(),
        bundle_id,
        files_collected,
        total_size_bytes: 0,
        redaction_applied: options.effective_redaction_level().redacts_secrets(),
        redaction_level: options.effective_redaction_level(),
        redaction_summary: RedactionSummary {
            total_redactions: 0,
            reasons: vec![],
        },
        output_path: None,
        manifest_hash: None,
        dry_run: true,
        workspace_path: workspace_path.display().to_string(),
    })
}

/// Create a support bundle with real diagnostic data.
pub fn create_bundle(options: &BundleOptions) -> Result<BundleReport, DomainError> {
    let output_dir = options
        .output_dir
        .clone()
        .ok_or_else(|| DomainError::Usage {
            message: "--out is required".to_string(),
            repair: Some("ee support bundle --out <dir>".to_string()),
        })?;

    let workspace_path = options
        .workspace
        .canonicalize()
        .unwrap_or_else(|_| options.workspace.clone());

    let bundle_id = generate_bundle_id();
    let bundle_dir = output_dir.join(format!("ee_support_{bundle_id}"));

    reject_existing_symlink_component(&bundle_dir, "support bundle output")?;
    fs::create_dir_all(&bundle_dir).map_err(|e| DomainError::Storage {
        message: format!("Failed to create bundle directory: {e}"),
        repair: Some("Check write permissions on output directory".to_string()),
    })?;
    reject_existing_symlink_component(&bundle_dir, "support bundle output")?;

    let diagnostics = collect_diagnostics(&workspace_path, options.audit_limit)?;
    let redaction_level = options.effective_redaction_level();

    let mut manifest_entries = Vec::new();
    let mut all_redaction_reasons: Vec<String> = Vec::new();
    let mut total_redactions = 0u32;
    let mut total_size = 0u64;

    let files_to_write = [
        (STATUS_FILE, &diagnostics.status_json),
        (DOCTOR_FILE, &diagnostics.doctor_json),
        (AUDIT_FILE, &diagnostics.audit_json),
        (CAPABILITIES_FILE, &diagnostics.capabilities_json),
        (SCHEMA_FILE, &diagnostics.schema_json),
        (PROFILE_EVIDENCE_FILE, &diagnostics.profile_evidence_json),
        (
            AGENT_PROFILE_EVIDENCE_FILE,
            &diagnostics.agent_profile_evidence_json,
        ),
        (
            SCALE_BENCHMARK_SUMMARY_FILE,
            &diagnostics.scale_benchmark_summary_json,
        ),
        (
            SCALE_FIXTURE_MANIFEST_FILE,
            &diagnostics.scale_fixture_manifest_json,
        ),
        (CACHE_REPORTS_FILE, &diagnostics.cache_reports_json),
        (
            WRITE_QUEUE_REPORT_FILE,
            &diagnostics.write_queue_report_json,
        ),
        (
            PERFORMANCE_EXPLAIN_SAMPLES_FILE,
            &diagnostics.performance_explain_samples_json,
        ),
        (
            PACK_REPLAY_SUMMARY_FILE,
            &diagnostics.pack_replay_summary_json,
        ),
        (
            SWARM_BRIEF_SUMMARY_FILE,
            &diagnostics.swarm_brief_summary_json,
        ),
        (
            COORDINATION_FALLBACK_SUMMARY_FILE,
            &diagnostics.coordination_fallback_summary_json,
        ),
        (
            SINGLEFLIGHT_POSTURE_FILE,
            &diagnostics.singleflight_posture_json,
        ),
        (QOS_LANE_SUMMARY_FILE, &diagnostics.qos_lane_summary_json),
        (TRIAGE_SUMMARY_FILE, &diagnostics.triage_summary_json),
    ];

    for (filename, content) in files_to_write {
        let (final_content, redacted) = if redaction_level.redacts_secrets() {
            let report = redact_support_bundle_content(content, redaction_level);
            let redacted = report.redacted;
            let reasons = report.redacted_reasons;
            if redacted {
                total_redactions += 1;
                for reason in &reasons {
                    if !all_redaction_reasons.contains(reason) {
                        all_redaction_reasons.push(reason.clone());
                    }
                }
            }
            (report.content, redacted)
        } else {
            (content.clone(), false)
        };

        let file_path = bundle_dir.join(filename);
        let size = write_file_with_hash(&file_path, &final_content)?;
        let content_hash = compute_hash(&final_content);

        manifest_entries.push(ManifestEntry {
            path: filename.to_owned(),
            size_bytes: size,
            content_hash,
            redacted,
        });

        total_size += size;
    }

    let manifest = BundleManifest {
        schema: SUPPORT_BUNDLE_MANIFEST_SCHEMA_V1.to_owned(),
        bundle_id: bundle_id.clone(),
        created_at: Utc::now().to_rfc3339(),
        workspace_path: workspace_path.display().to_string(),
        ee_version: env!("CARGO_PKG_VERSION").to_owned(),
        files: manifest_entries,
        total_size_bytes: total_size,
        redaction_applied: redaction_level.redacts_secrets(),
        redaction_reasons: all_redaction_reasons.clone(),
    };

    let manifest_json =
        serde_json::to_string_pretty(&manifest).map_err(|e| DomainError::Storage {
            message: format!("Failed to serialize manifest: {e}"),
            repair: None,
        })?;

    let manifest_path = bundle_dir.join(MANIFEST_FILE);
    write_file_with_hash(&manifest_path, &manifest_json)?;
    let manifest_hash = compute_hash(&manifest_json);

    let files_collected: Vec<String> = manifest
        .files
        .iter()
        .map(|e| e.path.clone())
        .chain(std::iter::once(MANIFEST_FILE.to_owned()))
        .collect();

    Ok(BundleReport {
        schema: SUPPORT_BUNDLE_SCHEMA_V1.to_owned(),
        bundle_id,
        files_collected,
        total_size_bytes: total_size,
        redaction_applied: redaction_level.redacts_secrets(),
        redaction_level,
        redaction_summary: RedactionSummary {
            total_redactions,
            reasons: all_redaction_reasons,
        },
        output_path: Some(bundle_dir),
        manifest_hash: Some(manifest_hash),
        dry_run: false,
        workspace_path: workspace_path.display().to_string(),
    })
}

/// Inspect an existing bundle and verify its integrity.
pub fn inspect_bundle(options: &InspectOptions) -> Result<InspectReport, DomainError> {
    if !options.bundle_path.exists() {
        return Err(DomainError::NotFound {
            resource: "bundle".to_string(),
            id: options.bundle_path.display().to_string(),
            repair: Some("Provide a valid bundle path".to_string()),
        });
    }
    reject_existing_symlink_component(&options.bundle_path, "support bundle")?;

    let manifest_path = if options.bundle_path.is_dir() {
        options.bundle_path.join(MANIFEST_FILE)
    } else {
        options.bundle_path.clone()
    };
    reject_existing_symlink_component(&manifest_path, "support bundle manifest")?;

    let bundle_dir = manifest_path.parent().unwrap_or(&options.bundle_path);

    let manifest: Option<BundleManifest> = if manifest_path.is_file() {
        let content = fs::read_to_string(&manifest_path).ok();
        content.and_then(|c| serde_json::from_str(&c).ok())
    } else {
        None
    };

    let mut files_found = Vec::new();
    let mut total_size = 0u64;
    let mut hash_mismatches = Vec::new();

    if let Some(ref m) = manifest {
        for entry in &m.files {
            let Ok(file_path) = resolve_bundle_file_no_symlinks(bundle_dir, &entry.path) else {
                hash_mismatches.push(entry.path.clone());
                continue;
            };
            if regular_file_no_symlink(&file_path) {
                files_found.push(entry.path.clone());
                if let Ok(content) = fs::read_to_string(&file_path) {
                    total_size += content.len() as u64;
                    if options.verify_hashes {
                        let actual_hash = compute_hash(&content);
                        if actual_hash != entry.content_hash {
                            hash_mismatches.push(entry.path.clone());
                        }
                    }
                }
            } else if options.verify_hashes {
                hash_mismatches.push(entry.path.clone());
            }
        }
    } else if options.bundle_path.is_dir() {
        if let Ok(entries) = fs::read_dir(bundle_dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    files_found.push(name.to_owned());
                    if let Ok(meta) = fs::symlink_metadata(entry.path()) {
                        if meta.file_type().is_symlink() {
                            continue;
                        }
                        total_size += meta.len();
                    }
                }
            }
        }
    }

    let valid = manifest.is_some() && hash_mismatches.is_empty();

    Ok(InspectReport {
        schema: SUPPORT_BUNDLE_INSPECT_SCHEMA_V1.to_owned(),
        bundle_path: options.bundle_path.clone(),
        manifest,
        files_found,
        total_size_bytes: total_size,
        hash_verified: options.verify_hashes,
        hash_mismatches,
        valid,
    })
}

/// Summarize an inspected support bundle as a normalized perf artifact.
///
/// The adapter is read-only: it verifies manifest-listed hashes, copies only
/// stable counts/profile labels/source hashes, and never embeds raw bundle file
/// contents in the returned summary.
pub fn summarize_bundle_for_perf_compare(
    bundle_path: &Path,
) -> Result<ArtifactSummary, DomainError> {
    let inspect = inspect_bundle(&InspectOptions {
        bundle_path: bundle_path.to_path_buf(),
        verify_hashes: true,
    })?;
    Ok(summarize_inspected_bundle_for_perf_compare(&inspect))
}

#[must_use]
pub fn summarize_inspected_bundle_for_perf_compare(inspect: &InspectReport) -> ArtifactSummary {
    let bundle_dir = inspected_bundle_dir(inspect);
    let manifest = inspect.manifest.as_ref();
    let artifact_id = manifest.map_or_else(
        || "support-bundle:missing-manifest".to_owned(),
        |manifest| format!("support-bundle:{}", manifest.bundle_id),
    );
    let source_schema = manifest.map_or(SUPPORT_BUNDLE_INSPECT_SCHEMA_V1, |manifest| {
        manifest.schema.as_str()
    });
    let mut summary = ArtifactSummary::new(
        artifact_id.clone(),
        ArtifactKind::SupportBundleManifest,
        source_schema,
    )
    .with_source_path(inspect.bundle_path.display().to_string())
    .with_command_family("support_bundle");

    if let Some(manifest) = manifest {
        summary.content_hash = Some(declared_bundle_signature(manifest));
        summary.observed_hash = Some(observed_bundle_signature(&bundle_dir, manifest));
        summary.add_metric(
            "bundle.manifest_file_count",
            MetricValue::measured(manifest.files.len() as f64, "count"),
        );
        summary.add_metric(
            "bundle.files_found_count",
            MetricValue::measured(inspect.files_found.len() as f64, "count"),
        );
        summary.add_metric(
            "bundle.total_size_bytes",
            MetricValue::measured(inspect.total_size_bytes as f64, "bytes"),
        );
        summary.add_metric(
            "bundle.hash_mismatch_count",
            MetricValue::measured(inspect.hash_mismatches.len() as f64, "count"),
        );
        summary.add_metric(
            "bundle.redaction_reason_count",
            MetricValue::measured(manifest.redaction_reasons.len() as f64, "count"),
        );
        summary.set_redaction(if manifest.redaction_applied {
            RedactionPosture::Redacted
        } else {
            RedactionPosture::Clean
        });

        if manifest.schema != SUPPORT_BUNDLE_MANIFEST_SCHEMA_V1 {
            summary.add_degradation(SummaryDegradation {
                code: SummaryDegradationCode::StaleSchemaVersion,
                severity: ArtifactDegradationSeverity::High,
                artifact_id: Some(artifact_id.clone()),
                field_path: Some("manifest.schema".to_owned()),
                message: format!(
                    "Unsupported support bundle manifest schema `{}`.",
                    manifest.schema
                ),
                repair: Some(
                    "Regenerate the support bundle with the current ee version.".to_owned(),
                ),
            });
        }

        for mismatch in &inspect.hash_mismatches {
            summary.add_degradation(SummaryDegradation {
                code: SummaryDegradationCode::TamperedHash,
                severity: ArtifactDegradationSeverity::High,
                artifact_id: Some(artifact_id.clone()),
                field_path: Some(format!("files.{mismatch}.contentHash")),
                message: format!(
                    "Support bundle attachment `{mismatch}` failed hash verification."
                ),
                repair: Some("Regenerate the support bundle before comparing it.".to_owned()),
            });
        }

        for (section, file_name) in PERF_COMPARE_BUNDLE_SECTIONS {
            let present = manifest.files.iter().any(|entry| entry.path == file_name)
                && inspect.files_found.iter().any(|found| found == file_name);
            summary.add_metric(
                format!("section.{section}.present"),
                MetricValue::measured(if present { 1.0 } else { 0.0 }, "bool"),
            );
            summary.add_provenance(ProvenanceEntry {
                field: format!("section.{section}.present"),
                source_path: file_name.to_owned(),
                source_line: None,
            });
            if !present {
                summary.add_degradation(missing_bundle_section(&artifact_id, section, file_name));
            }
        }

        if let Some(profile) =
            read_bundle_json(&bundle_dir, PROFILE_EVIDENCE_FILE).and_then(|json| {
                json.pointer("/profile/activeProfile")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned)
            })
        {
            summary.profile = Some(ProfileReference {
                profile_name: profile,
                confidence: read_bundle_json(&bundle_dir, PROFILE_EVIDENCE_FILE).and_then(|json| {
                    json.pointer("/profile/confidence")
                        .and_then(|value| value.as_str())
                        .map(ToOwned::to_owned)
                }),
                override_source: None,
            });
            summary.add_provenance(ProvenanceEntry {
                field: "profile.profileName".to_owned(),
                source_path: PROFILE_EVIDENCE_FILE.to_owned(),
                source_line: None,
            });
        }

        add_optional_json_metrics(&mut summary, &bundle_dir);
        summary.verify_hash();
    } else {
        summary.set_redaction(RedactionPosture::Uncertain);
        summary.add_degradation(SummaryDegradation {
            code: SummaryDegradationCode::SourceUnavailable,
            severity: ArtifactDegradationSeverity::High,
            artifact_id: Some(artifact_id),
            field_path: Some("manifest.json".to_owned()),
            message: "Support bundle manifest is missing or malformed.".to_owned(),
            repair: Some(
                "Run ee support bundle --out <dir> and inspect the generated bundle.".to_owned(),
            ),
        });
    }

    summary
}

fn inspected_bundle_dir(inspect: &InspectReport) -> PathBuf {
    if inspect.bundle_path.is_dir() {
        inspect.bundle_path.clone()
    } else {
        inspect
            .bundle_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    }
}

fn declared_bundle_signature(manifest: &BundleManifest) -> String {
    let mut parts = manifest
        .files
        .iter()
        .map(|entry| format!("{}={}", entry.path, entry.content_hash))
        .collect::<Vec<_>>();
    parts.sort();
    compute_hash(&parts.join("\n"))
}

fn observed_bundle_signature(bundle_dir: &Path, manifest: &BundleManifest) -> String {
    let mut parts = manifest
        .files
        .iter()
        .map(|entry| {
            let observed = resolve_bundle_file_no_symlinks(bundle_dir, &entry.path)
                .and_then(|path| read_regular_file_no_symlinks(&path))
                .map(|content| compute_hash(&content))
                .unwrap_or_else(|_| "missing_or_unreadable".to_owned());
            format!("{}={observed}", entry.path)
        })
        .collect::<Vec<_>>();
    parts.sort();
    compute_hash(&parts.join("\n"))
}

fn missing_bundle_section(artifact_id: &str, section: &str, file_name: &str) -> SummaryDegradation {
    SummaryDegradation {
        code: SummaryDegradationCode::MissingMetric,
        severity: ArtifactDegradationSeverity::Medium,
        artifact_id: Some(artifact_id.to_owned()),
        field_path: Some(format!("files.{file_name}")),
        message: format!("Support bundle section `{section}` is missing (`{file_name}`)."),
        repair: Some(format!(
            "Regenerate the support bundle and ensure `{file_name}` is present."
        )),
    }
}

fn read_bundle_json(bundle_dir: &Path, file_name: &str) -> Option<Value> {
    resolve_bundle_file_no_symlinks(bundle_dir, file_name)
        .and_then(|path| read_regular_file_no_symlinks(&path))
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
}

fn add_optional_json_metrics(summary: &mut ArtifactSummary, bundle_dir: &Path) {
    if let Some(json) = read_bundle_json(bundle_dir, SCALE_BENCHMARK_SUMMARY_FILE) {
        let report_count = json
            .get("swarmSmokeReports")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        summary.add_metric(
            "swarm_contention.report_count",
            MetricValue::measured(report_count as f64, "count"),
        );
    }
    if let Some(json) = read_bundle_json(bundle_dir, PERFORMANCE_EXPLAIN_SAMPLES_FILE) {
        let sample_count = json.get("sampleCount").and_then(Value::as_u64).unwrap_or(0);
        summary.add_metric(
            "performance_explain.sample_count",
            MetricValue::measured(sample_count as f64, "count"),
        );
    }
    if let Some(json) = read_bundle_json(bundle_dir, CACHE_REPORTS_FILE) {
        if let Some(memory_count) = json
            .pointer("/database/memoryCount")
            .and_then(Value::as_u64)
        {
            summary.add_metric(
                "cache.database_memory_count",
                MetricValue::measured(memory_count as f64, "count"),
            );
        }
    }
}

fn collect_diagnostics(
    workspace: &Path,
    audit_limit: u32,
) -> Result<CollectedDiagnostics, DomainError> {
    let status = StatusReport::gather_for_workspace(workspace);
    let status_json = output::render_status_json(&status);

    let doctor = DoctorReport::gather_for_workspace(workspace);
    let doctor_json = output::render_doctor_json(&doctor);

    let audit_json = collect_audit_entries(workspace, audit_limit);

    let capabilities_json = json!({
        "runtime": status.capabilities.runtime.as_str(),
        "storage": status.capabilities.storage.as_str(),
        "search": status.capabilities.search.as_str(),
        "agentDetection": status.capabilities.agent_detection.as_str(),
    })
    .to_string();

    let schema_json = json!({
        "schemaVersion": crate::db::MIGRATIONS.last().map_or(0, |migration| migration.version()),
        "eeVersion": env!("CARGO_PKG_VERSION"),
    })
    .to_string();

    let profile_evidence_json = profile_evidence_json(workspace);
    let agent_profile_evidence_json = agent_profile_evidence_json(workspace);
    let swarm_reports = discover_swarm_report_summaries(workspace);
    let scale_benchmark_summary_json = scale_benchmark_summary_json(workspace, &swarm_reports);
    let scale_fixture_manifest_json = scale_fixture_manifest_json();
    let cache_reports_json = cache_reports_json(workspace);
    let write_queue_report_json = write_queue_report_json();
    let performance_explain_samples_json = performance_explain_samples_json(workspace);
    let pack_replay_summary_json = pack_replay_summary_json(workspace);
    let swarm_brief_summary_json = swarm_brief_summary_json(workspace);
    let coordination_fallback_summary_json = coordination_fallback_summary_json(workspace);
    let singleflight_posture_json = singleflight_posture_json();
    let qos_lane_summary_json = qos_lane_summary_json(workspace);
    let triage_summary_json = triage_summary_json(&status, &swarm_reports);

    Ok(CollectedDiagnostics {
        status_json,
        doctor_json,
        audit_json,
        capabilities_json,
        schema_json,
        profile_evidence_json,
        agent_profile_evidence_json,
        scale_benchmark_summary_json,
        scale_fixture_manifest_json,
        cache_reports_json,
        write_queue_report_json,
        performance_explain_samples_json,
        pack_replay_summary_json,
        swarm_brief_summary_json,
        coordination_fallback_summary_json,
        singleflight_posture_json,
        qos_lane_summary_json,
        triage_summary_json,
    })
}

fn profile_evidence_json(workspace: &Path) -> String {
    let probe = super::profile::HostResourceProbeReport::gather_for_workspace(workspace);
    let recommendation = super::profile::recommend_operating_profile(&probe);
    let runtime = super::profile::runtime_profile_for_workspace(workspace);
    let active_profile = runtime.active_profile;
    let runtime_source = runtime.source;
    let budgets = super::profile::ProfileBudgets::for_profile(active_profile);
    let verification_recipe = super::profile::VerificationRecipe::for_profile(active_profile);
    let probe_degraded = probe.degraded.clone();
    let verification_degraded = verification_recipe.degraded.clone();
    let profile_source = if runtime_source == "workspace_config" {
        ".ee/config.toml profile.selected"
    } else {
        "host resource probe recommendation"
    };

    stable_json(&json!({
        "schema": "ee.support_bundle.profile_evidence.v1",
        "redactionStatus": "label_only_paths_presence_only_env_no_raw_values",
        "profile": {
            "activeProfile": active_profile.as_str(),
            "recommendedProfile": recommendation.recommended.as_str(),
            "effectiveProfile": recommendation.effective.as_str(),
            "source": runtime_source.as_str(),
            "confidence": recommendation.confidence,
            "reasons": recommendation.reasons,
        },
        "probe": probe,
        "budgets": budgets,
        "verificationRecipe": verification_recipe,
        "degraded": {
            "probe": probe_degraded,
            "verification": verification_degraded,
        },
        "provenance": [
            {
                "field": "profile.activeProfile",
                "sourceKind": runtime_source.as_str(),
                "source": profile_source,
                "redaction": "profile_name_only",
            },
            {
                "field": "profile.recommendedProfile",
                "sourceKind": "host_probe",
                "source": super::profile::HOST_PROFILE_PROBE_SCHEMA_V1,
                "redaction": "label_only_paths_presence_only_env",
            },
            {
                "field": "probe",
                "sourceKind": "host_probe",
                "source": super::profile::HOST_PROFILE_PROBE_SCHEMA_V1,
                "redaction": "path_not_emitted_env_presence_only",
            },
            {
                "field": "budgets",
                "sourceKind": "profile_budget_table",
                "source": "src/core/profile.rs::ProfileBudgets::for_profile",
                "redaction": "numeric_and_enum_budget_values_only",
            },
            {
                "field": "verificationRecipe",
                "sourceKind": "profile_verification_budget",
                "source": super::profile::VERIFICATION_RECIPE_SCHEMA_V1,
                "redaction": "commands_are_templates_no_workspace_paths",
            },
            {
                "field": "degraded",
                "sourceKind": "profile_probe_and_recipe_reports",
                "source": "probe.degraded + verificationRecipe.degraded",
                "redaction": "stable_codes_messages_and_repairs",
            }
        ],
    }))
}

fn agent_profile_evidence_json(workspace: &Path) -> String {
    stable_json(&collect_agent_profile_evidence(workspace))
}

fn collect_agent_profile_evidence(workspace: &Path) -> Value {
    let database_path = workspace.join(".ee").join("ee.db");
    let mut database = json!({
        "present": database_path.is_file(),
        "readable": false,
        "workspaceRowPresent": false,
        "schemaVersion": null,
        "profileRowCount": 0,
        "summarizedAgentCount": 0,
    });

    if !database_path.is_file() {
        return agent_profile_evidence_value("database_missing", database, Vec::new());
    }

    let Ok(connection) = DbConnection::open_file(&database_path) else {
        return agent_profile_evidence_value("database_unreadable", database, Vec::new());
    };
    database["readable"] = json!(true);
    database["schemaVersion"] = connection
        .schema_version()
        .ok()
        .flatten()
        .map_or(Value::Null, Value::from);

    let workspace_path = workspace.display().to_string();
    let Ok(Some(workspace_row)) = connection.get_workspace_by_path(&workspace_path) else {
        return agent_profile_evidence_value("workspace_missing", database, Vec::new());
    };
    database["workspaceRowPresent"] = json!(true);
    database["profileRowCount"] = json!(query_cache_count(
        &connection,
        "SELECT COUNT(*) FROM agent_context_profiles WHERE workspace_id = ?1",
        &workspace_row.id,
    ));

    let Ok(rows) = connection.query(
        "SELECT agent_name, COUNT(*) AS memory_profile_count,
                SUM(helpful_count) AS helpful_count,
                SUM(harmful_count) AS harmful_count,
                SUM(ignored_count) AS ignored_count,
                MAX(last_seen_at) AS last_seen_at
         FROM agent_context_profiles
         WHERE workspace_id = ?1
         GROUP BY agent_name
         ORDER BY agent_name ASC
         LIMIT 64",
        &[SqlValue::Text(workspace_row.id)],
    ) else {
        return agent_profile_evidence_value("query_failed", database, Vec::new());
    };

    let agents = rows
        .iter()
        .filter_map(agent_profile_row_summary)
        .collect::<Vec<_>>();
    database["summarizedAgentCount"] = json!(agents.len());

    agent_profile_evidence_value("available", database, agents)
}

fn agent_profile_evidence_value(status: &str, database: Value, agents: Vec<Value>) -> Value {
    json!({
        "schema": "ee.support_bundle.agent_profile_evidence.v1",
        "sourceSchema": crate::models::AGENT_CONTEXT_PROFILE_SCHEMA_V1,
        "source": "workspace_agent_context_profiles",
        "status": status,
        "redactionStatus": "agent_names_hashed_counts_only_no_raw_agent_names",
        "limits": {
            "maxAgents": 64,
        },
        "database": database,
        "agents": agents,
        "provenance": [
            {
                "field": "agents[].agentNameHash",
                "sourceKind": "agent_context_profile_row",
                "source": "agent_context_profiles.agent_name",
                "redaction": "blake3_hash",
            },
            {
                "field": "agents[].counts",
                "sourceKind": "agent_context_profile_row",
                "source": "agent_context_profiles helpful/harmful/ignored aggregates",
                "redaction": "counts_only",
            }
        ],
    })
}

fn agent_profile_row_summary(row: &SqlRow) -> Option<Value> {
    let agent_name = row_text(row, 0)?;
    Some(json!({
        "agentNameIncluded": false,
        "agentNameHash": support_agent_name_hash(agent_name),
        "memoryProfileCount": row_u64(row, 1),
        "observedOutcomes": row_u64(row, 2)
            .saturating_add(row_u64(row, 3))
            .saturating_add(row_u64(row, 4)),
        "helpfulCount": row_u64(row, 2),
        "harmfulCount": row_u64(row, 3),
        "ignoredCount": row_u64(row, 4),
        "lastSeenAt": row_text(row, 5),
    }))
}

fn support_agent_name_hash(agent_name: &str) -> String {
    let digest = blake3::hash(agent_name.as_bytes()).to_hex().to_string();
    format!("blake3:{}", &digest[..12])
}

fn scale_benchmark_summary_json(workspace: &Path, swarm_reports: &[Value]) -> String {
    stable_json(&json!({
        "schema": "ee.support_bundle.scale_benchmark_summary.v1",
        "owningBead": "eidetic_engine_cli-fcq1.7",
        "workspacePath": workspace.display().to_string(),
        "sourceArtifacts": [
            "tests/perf_bench_status.rs",
            "tests/e2e_swarm_contention_recovery.rs",
            "tests/fixtures/swarm_scale/workloads.json"
        ],
        "statusBenchmark": {
            "groupName": STATUS_BENCH_GROUP_NAME,
            "quickIterations": STATUS_BENCH_QUICK_ITERATIONS,
            "hardCeilingMs": STATUS_BENCH_HARD_CEILING_MS,
            "scales": STATUS_BENCH_SCALES
                .iter()
                .map(|scale| json!({
                    "name": scale.name,
                    "memoryCount": scale.memory_count,
                }))
                .collect::<Vec<_>>(),
        },
        "swarmSmokeReports": swarm_reports,
        "redactionStatus": "content_redacted_before_manifest_write",
    }))
}

fn scale_fixture_manifest_json() -> String {
    let manifest =
        serde_json::from_str::<Value>(SWARM_SCALE_WORKLOADS_MANIFEST).unwrap_or_else(|error| {
            json!({
                "schema": "ee.swarm_scale.workloads.unparseable",
                "parseError": error.to_string(),
            })
        });

    stable_json(&json!({
        "schema": "ee.support_bundle.scale_fixture_manifest.v1",
        "source": "tests/fixtures/swarm_scale/workloads.json",
        "redactionStatus": "fixture_contract_declares_no_synthetic_secrets",
        "manifest": manifest,
    }))
}

fn cache_reports_json(workspace: &Path) -> String {
    let cache_state = collect_cache_directory_state(workspace);
    let snapshot = collect_cache_hotset_snapshot(workspace, &cache_state);
    let search_hotset = SearchHotset::new(snapshot.search_entries);
    let search_report = prewarm_search_hotset(
        &search_hotset,
        SearchCacheGovernor::new(snapshot.generation, CacheBudget::new(16, 64 * 1024))
            .with_current_usage(cache_state.search.entries, cache_state.search.bytes),
    );

    let pack_hotset = PackHotset::new(snapshot.pack_entries);
    let pack_report = prewarm_pack_hotset(
        &pack_hotset,
        PackCacheGovernor::new(snapshot.generation, CacheBudget::new(16, 64 * 1024))
            .with_current_usage(cache_state.pack.entries, cache_state.pack.bytes),
    );

    stable_json(&json!({
        "schema": "ee.support_bundle.scale_cache_reports.v1",
        "redactionStatus": "content_not_stored",
        "source": "workspace_database_and_cache_state",
        "workspacePath": workspace.display().to_string(),
        "database": {
            "present": snapshot.database_present,
            "readable": snapshot.database_readable,
            "workspaceRowPresent": snapshot.workspace_row_present,
            "schemaVersion": snapshot.schema_version,
            "memoryCount": snapshot.memory_count,
            "packRecordCount": snapshot.pack_record_count,
            "packItemCount": snapshot.pack_item_count,
        },
        "cacheState": {
            "root": ".ee/cache",
            "rootPresent": cache_state.root_present,
            "search": cache_state.search.data_json(),
            "pack": cache_state.pack.data_json(),
            "unclassified": cache_state.unclassified.data_json(),
        },
        "reports": {
            "search": search_report.data_json(),
            "pack": pack_report.data_json(),
        },
    }))
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CacheUsage {
    entries: usize,
    bytes: usize,
}

impl CacheUsage {
    fn record_file(&mut self, bytes: u64) {
        self.entries = self.entries.saturating_add(1);
        self.bytes = self
            .bytes
            .saturating_add(usize::try_from(bytes).unwrap_or(usize::MAX));
    }

    fn data_json(self) -> Value {
        json!({
            "entries": self.entries,
            "bytes": self.bytes,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CacheDirectoryState {
    root_present: bool,
    search: CacheUsage,
    pack: CacheUsage,
    unclassified: CacheUsage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CacheBucket {
    Search,
    Pack,
    Unclassified,
}

#[derive(Debug, Default)]
struct CacheHotsetSnapshot {
    database_present: bool,
    database_readable: bool,
    workspace_row_present: bool,
    schema_version: Option<u32>,
    generation: u64,
    memory_count: usize,
    pack_record_count: usize,
    pack_item_count: usize,
    search_entries: Vec<SearchHotsetEntry>,
    pack_entries: Vec<PackHotsetEntry>,
}

fn collect_cache_directory_state(workspace: &Path) -> CacheDirectoryState {
    let cache_root = workspace.join(".ee").join("cache");
    let mut state = CacheDirectoryState {
        root_present: cache_root.is_dir(),
        ..CacheDirectoryState::default()
    };
    if state.root_present {
        let mut relative_segments = Vec::new();
        collect_cache_directory_entries(&cache_root, &mut relative_segments, &mut state);
    }
    state
}

fn collect_cache_directory_entries(
    directory: &Path,
    relative_segments: &mut Vec<String>,
    state: &mut CacheDirectoryState,
) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut paths = entries
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort();

    for path in paths {
        let Some(name) = path.file_name() else {
            continue;
        };
        let name = name.to_string_lossy().to_ascii_lowercase();
        if name.starts_with("._") || name == ".ds_store" {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        relative_segments.push(name);
        if metadata.is_dir() {
            collect_cache_directory_entries(&path, relative_segments, state);
        } else if metadata.is_file() {
            match classify_cache_path(relative_segments) {
                CacheBucket::Search => state.search.record_file(metadata.len()),
                CacheBucket::Pack => state.pack.record_file(metadata.len()),
                CacheBucket::Unclassified => state.unclassified.record_file(metadata.len()),
            }
        }
        relative_segments.pop();
    }
}

fn classify_cache_path(relative_segments: &[String]) -> CacheBucket {
    if relative_segments
        .iter()
        .any(|segment| segment.contains("search"))
    {
        CacheBucket::Search
    } else if relative_segments
        .iter()
        .any(|segment| segment.contains("pack") || segment.contains("context"))
    {
        CacheBucket::Pack
    } else {
        CacheBucket::Unclassified
    }
}

fn collect_cache_hotset_snapshot(
    workspace: &Path,
    cache_state: &CacheDirectoryState,
) -> CacheHotsetSnapshot {
    let database_path = workspace.join(".ee").join("ee.db");
    let mut snapshot = CacheHotsetSnapshot {
        database_present: database_path.is_file(),
        ..CacheHotsetSnapshot::default()
    };
    if !snapshot.database_present {
        snapshot.generation = cache_source_generation(&snapshot, cache_state);
        return snapshot;
    }

    let Ok(connection) = DbConnection::open_file(&database_path) else {
        snapshot.generation = cache_source_generation(&snapshot, cache_state);
        return snapshot;
    };
    snapshot.database_readable = true;
    snapshot.schema_version = connection.schema_version().ok().flatten();

    let workspace_path = workspace.display().to_string();
    let Ok(Some(workspace_row)) = connection.get_workspace_by_path(&workspace_path) else {
        snapshot.generation = cache_source_generation(&snapshot, cache_state);
        return snapshot;
    };
    snapshot.workspace_row_present = true;
    snapshot.memory_count = connection
        .list_memories(&workspace_row.id, None, false)
        .map_or(0, |memories| memories.len());
    snapshot.pack_record_count = query_cache_count(
        &connection,
        "SELECT COUNT(*) FROM pack_records WHERE workspace_id = ?1",
        &workspace_row.id,
    );
    snapshot.pack_item_count = query_cache_count(
        &connection,
        "SELECT COUNT(*)
         FROM pack_items pi
         JOIN pack_records pr ON pr.id = pi.pack_id
         WHERE pr.workspace_id = ?1",
        &workspace_row.id,
    );
    snapshot.generation = cache_source_generation(&snapshot, cache_state);
    snapshot.search_entries =
        collect_search_cache_hotset_entries(&connection, &workspace_row.id, snapshot.generation);
    snapshot.pack_entries =
        collect_pack_cache_hotset_entries(&connection, &workspace_row.id, snapshot.generation);
    snapshot
}

fn cache_source_generation(
    snapshot: &CacheHotsetSnapshot,
    cache_state: &CacheDirectoryState,
) -> u64 {
    let payload = format!(
        "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
        snapshot.schema_version.unwrap_or(0),
        snapshot.memory_count,
        snapshot.pack_record_count,
        snapshot.pack_item_count,
        cache_state.root_present,
        cache_state.search.entries,
        cache_state.search.bytes,
        cache_state.pack.entries,
        cache_state.pack.bytes,
        cache_state.unclassified.entries,
    );
    let hash = blake3::hash(payload.as_bytes());
    let bytes = hash.as_bytes();
    let generation = u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]);
    if snapshot.memory_count == 0
        && snapshot.pack_record_count == 0
        && snapshot.pack_item_count == 0
        && cache_state.search.entries == 0
        && cache_state.pack.entries == 0
        && cache_state.unclassified.entries == 0
    {
        0
    } else {
        generation.max(1)
    }
}

fn collect_search_cache_hotset_entries(
    connection: &DbConnection,
    workspace_id: &str,
    generation: u64,
) -> Vec<SearchHotsetEntry> {
    let mut entries = Vec::new();
    if generation == 0 {
        return entries;
    }

    if let Ok(rows) = connection.query(
        "SELECT m.id, COALESCE(COUNT(pi.memory_id), 0) AS hits
         FROM memories m
         LEFT JOIN pack_items pi ON pi.memory_id = m.id
         WHERE m.workspace_id = ?1 AND m.tombstoned_at IS NULL AND m.valid_to IS NULL
         GROUP BY m.id
         ORDER BY hits DESC, m.importance DESC, m.utility DESC, m.id ASC
         LIMIT 8",
        &[SqlValue::Text(workspace_id.to_owned())],
    ) {
        entries.extend(rows.iter().filter_map(|row| {
            let memory_id = row_text(row, 0)?;
            Some(SearchHotsetEntry::memory(
                memory_id,
                generation,
                row_u64(row, 1).max(1),
            ))
        }));
    }

    if let Ok(rows) = connection.query(
        "SELECT query, COUNT(*) AS hits
         FROM pack_records
         WHERE workspace_id = ?1
         GROUP BY query
         ORDER BY hits DESC, MAX(created_at) DESC, query ASC
         LIMIT 4",
        &[SqlValue::Text(workspace_id.to_owned())],
    ) {
        entries.extend(rows.iter().filter_map(|row| {
            SearchHotsetEntry::query_shape(row_text(row, 0)?, generation, row_u64(row, 1).max(1))
        }));
    }

    if let Ok(rows) = connection.query(
        "SELECT id
         FROM memories
         WHERE workspace_id = ?1 AND tombstoned_at IS NULL AND valid_to IS NULL",
        &[SqlValue::Text(workspace_id.to_owned())],
    ) {
        let active_memory_ids = rows
            .iter()
            .filter_map(|row| row_text(row, 0))
            .collect::<BTreeSet<_>>();
        if let Ok(links) = connection.list_all_memory_links(None) {
            let mut graph_hits = BTreeMap::<String, u64>::new();
            for link in links.into_iter().filter(|link| {
                active_memory_ids.contains(link.src_memory_id.as_str())
                    && crate::graph::memory_link_mesh_metadata_visible(
                        link.metadata_json.as_deref(),
                    )
            }) {
                *graph_hits.entry(link.src_memory_id).or_default() += 1;
            }
            let mut graph_hits = graph_hits.into_iter().collect::<Vec<_>>();
            graph_hits
                .sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
            entries.extend(graph_hits.into_iter().take(4).map(|(memory_id, hits)| {
                SearchHotsetEntry::graph_neighborhood(memory_id, 2, generation, hits.max(1))
            }));
        }
    }

    entries
}

fn collect_pack_cache_hotset_entries(
    connection: &DbConnection,
    workspace_id: &str,
    generation: u64,
) -> Vec<PackHotsetEntry> {
    let mut entries = Vec::new();
    if generation == 0 {
        return entries;
    }

    if let Ok(rows) = connection.query(
        "SELECT pr.id, pi.section, COUNT(*) AS item_count, COALESCE(SUM(pi.estimated_tokens), 0) AS used_tokens
         FROM pack_records pr
         JOIN pack_items pi ON pi.pack_id = pr.id
         WHERE pr.workspace_id = ?1
         GROUP BY pr.id, pi.section
         ORDER BY item_count DESC, pr.id ASC, pi.section ASC
         LIMIT 8",
        &[SqlValue::Text(workspace_id.to_owned())],
    ) {
        entries.extend(rows.iter().filter_map(|row| {
            let pack_id = row_text(row, 0)?;
            let section_name = row_text(row, 1)?;
            let item_count = row_usize(row, 2);
            let used_tokens = row_usize(row, 3);
            Some(PackHotsetEntry {
                key: support_cache_key(&format!(
                    "pack:section:{pack_id}:{section_name}:{used_tokens}:{item_count}"
                )),
                kind: PackHotsetEntryKind::PackSection,
                section: pack_section_from_str(section_name),
                generation,
                estimated_bytes: 128_usize.saturating_add(item_count.saturating_mul(48)),
                hit_count: cache_usize_to_u64(item_count).max(1),
                redaction_status: "content_not_stored",
            })
        }));
    }

    if let Ok(rows) = connection.query(
        "SELECT id, profile, max_tokens, used_tokens, item_count, pack_hash
         FROM pack_records
         WHERE workspace_id = ?1
         ORDER BY created_at DESC, id ASC
         LIMIT 4",
        &[SqlValue::Text(workspace_id.to_owned())],
    ) {
        entries.extend(rows.iter().filter_map(|row| {
            let pack_id = row_text(row, 0)?;
            let profile = row_text(row, 1).unwrap_or("unknown");
            let max_tokens = row_usize(row, 2);
            let used_tokens = row_usize(row, 3);
            let item_count = row_usize(row, 4);
            let pack_hash = row_text(row, 5).unwrap_or("missing_hash");
            Some(PackHotsetEntry {
                key: support_cache_key(&format!(
                    "pack:selection_audit:{pack_id}:{profile}:{max_tokens}:{used_tokens}:{item_count}:{pack_hash}"
                )),
                kind: PackHotsetEntryKind::SelectionAudit,
                section: None,
                generation,
                estimated_bytes: 192_usize.saturating_add(item_count.saturating_mul(40)),
                hit_count: cache_usize_to_u64(item_count).max(1),
                redaction_status: "content_not_stored",
            })
        }));
    }

    entries
}

fn query_cache_count(connection: &DbConnection, sql: &str, workspace_id: &str) -> usize {
    connection
        .query(sql, &[SqlValue::Text(workspace_id.to_owned())])
        .ok()
        .and_then(|rows| rows.first().map(|row| row_usize(row, 0)))
        .unwrap_or(0)
}

fn row_text(row: &SqlRow, index: usize) -> Option<&str> {
    row.get(index).and_then(|value| value.as_str())
}

fn row_usize(row: &SqlRow, index: usize) -> usize {
    row.get(index)
        .and_then(|value| value.as_i64())
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0)
}

fn row_u64(row: &SqlRow, index: usize) -> u64 {
    row.get(index)
        .and_then(|value| value.as_i64())
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(0)
}

fn cache_usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn pack_section_from_str(value: &str) -> Option<PackSection> {
    match value {
        "procedural_rules" => Some(PackSection::ProceduralRules),
        "decisions" => Some(PackSection::Decisions),
        "failures" => Some(PackSection::Failures),
        "evidence" => Some(PackSection::Evidence),
        "artifacts" => Some(PackSection::Artifacts),
        _ => None,
    }
}

fn write_queue_report_json() -> String {
    let spool = WriteSpool::new(WriteSpoolConfig::default(), 0);
    stable_json(&json!({
        "schema": "ee.support_bundle.write_queue_report.v1",
        "status": "not_attached",
        "reason": "support_bundle_cli_process_has_no_live_daemon_spool",
        "emptySpoolContract": spool.status(0),
        "owner": "daemon_write_queue",
        "repair": "Run ee daemon status --json when daemon mode owns writes.",
    }))
}

fn performance_explain_samples_json(workspace: &Path) -> String {
    let samples = discover_performance_explain_samples(workspace);
    let status = if samples.is_empty() {
        "no_persisted_samples_found"
    } else {
        "persisted_samples_collected"
    };
    stable_json(&json!({
        "schema": "ee.support_bundle.performance_explain_samples.v1",
        "sourceSchema": super::search::PERFORMANCE_EXPLAIN_SCHEMA_V1,
        "status": status,
        "sampleSource": ".ee/performance-explain/*.json",
        "sampleCount": samples.len(),
        "samples": samples,
    }))
}

fn pack_replay_summary_json(workspace: &Path) -> String {
    stable_json(&collect_pack_replay_summary(workspace))
}

fn swarm_brief_summary_json(workspace: &Path) -> String {
    stable_json(&super::swarm_brief::collect_swarm_brief_summary(workspace))
}

fn coordination_fallback_summary_json(workspace: &Path) -> String {
    stable_json(&collect_coordination_fallback_summary(workspace))
}

fn singleflight_posture_json() -> String {
    serde_json::to_value(singleflight_posture_report()).map_or_else(
        |error| {
            json!({
                "schema": "ee.support_bundle.serialization_error.v1",
                "message": error.to_string(),
            })
            .to_string()
        },
        |value| stable_json(&value),
    )
}

fn qos_lane_summary_json(workspace: &Path) -> String {
    let workspace_identity = workspace.to_string_lossy();
    let now_epoch_ms = Utc::now().timestamp_millis().try_into().unwrap_or_default();
    let value = serde_json::to_value(super::qos::summarize_qos_lane_registry(
        workspace,
        &workspace_identity,
        now_epoch_ms,
    ))
    .map_or_else(
        |error| {
            json!({
                "schema": "ee.support_bundle.serialization_error.v1",
                "message": error.to_string(),
            })
        },
        |value| value,
    );
    stable_json(&value)
}

fn collect_pack_replay_summary(workspace: &Path) -> Value {
    let database_path = workspace.join(".ee").join("ee.db");
    let mut database = json!({
        "present": database_path.is_file(),
        "readable": false,
        "workspaceRowPresent": false,
        "schemaVersion": null,
        "packRecordCount": 0,
        "summarizedPackCount": 0,
        "ledgerAvailableCount": 0,
        "ledgerMissingCount": 0,
        "ledgerMalformedCount": 0,
        "ledgerHashMismatchCount": 0,
    });

    if !database_path.is_file() {
        return pack_replay_summary_value("database_missing", database, Vec::new());
    }

    let Ok(connection) = DbConnection::open_file(&database_path) else {
        return pack_replay_summary_value("database_unreadable", database, Vec::new());
    };
    database["readable"] = json!(true);
    database["schemaVersion"] = connection
        .schema_version()
        .ok()
        .flatten()
        .map_or(Value::Null, Value::from);

    let workspace_path = workspace.display().to_string();
    let Ok(Some(workspace_row)) = connection.get_workspace_by_path(&workspace_path) else {
        return pack_replay_summary_value("workspace_missing", database, Vec::new());
    };
    database["workspaceRowPresent"] = json!(true);

    let total_count = query_cache_count(
        &connection,
        "SELECT COUNT(*) FROM pack_records WHERE workspace_id = ?1",
        &workspace_row.id,
    );
    database["packRecordCount"] = json!(total_count);

    let Ok(rows) = connection.query(
        "SELECT id, query, profile, max_tokens, used_tokens, item_count, omitted_count, pack_hash, ledger_json, ledger_hash, created_at, created_by
         FROM pack_records
         WHERE workspace_id = ?1
         ORDER BY created_at DESC, id ASC
         LIMIT ?2",
        &[
            SqlValue::Text(workspace_row.id.clone()),
            SqlValue::BigInt(i64::try_from(MAX_PACK_REPLAY_SUMMARY_RECORDS).unwrap_or(i64::MAX)),
        ],
    ) else {
        return pack_replay_summary_value("query_failed", database, Vec::new());
    };

    let packs = rows
        .iter()
        .map(pack_replay_record_summary)
        .collect::<Vec<_>>();
    database["summarizedPackCount"] = json!(packs.len());
    for pack in &packs {
        match pack.pointer("/ledger/status").and_then(Value::as_str) {
            Some("available") => increment_json_count(&mut database, "ledgerAvailableCount"),
            Some("missing") => increment_json_count(&mut database, "ledgerMissingCount"),
            Some("malformed") => increment_json_count(&mut database, "ledgerMalformedCount"),
            Some("hash_mismatch") => increment_json_count(&mut database, "ledgerHashMismatchCount"),
            _ => {}
        }
    }

    pack_replay_summary_value("available", database, packs)
}

fn pack_replay_summary_value(status: &str, database: Value, packs: Vec<Value>) -> Value {
    json!({
        "schema": "ee.support_bundle.pack_replay_summary.v1",
        "sourceSchema": crate::db::PACK_REPLAY_LEDGER_SCHEMA_V1,
        "source": "workspace_pack_records",
        "status": status,
        "redactionStatus": "ids_hashes_counts_codes_only_no_query_text_no_memory_content",
        "limits": {
            "maxPacks": MAX_PACK_REPLAY_SUMMARY_RECORDS,
        },
        "database": database,
        "packs": packs,
    })
}

fn increment_json_count(object: &mut Value, field: &str) {
    let next = object.get(field).and_then(Value::as_u64).unwrap_or(0) + 1;
    object[field] = json!(next);
}

fn pack_replay_record_summary(row: &SqlRow) -> Value {
    let pack_id = row_text(row, 0).unwrap_or("unknown");
    let query = row_text(row, 1).unwrap_or_default();
    let ledger_json = row_text(row, 8);
    let ledger_hash = row_text(row, 9);
    let ledger = summarize_pack_replay_ledger(ledger_json, ledger_hash);

    json!({
        "packId": pack_id,
        "packHash": row_text(row, 7),
        "ledgerHash": ledger_hash,
        "createdAt": row_text(row, 10),
        "createdBy": row_text(row, 11),
        "profile": row_text(row, 2),
        "maxTokens": row_u64(row, 3),
        "usedTokens": row_u64(row, 4),
        "itemCount": row_u64(row, 5),
        "omittedCount": row_u64(row, 6),
        "queryTextIncluded": false,
        "queryHash": blake3_text_hash(query),
        "ledger": ledger,
    })
}

fn summarize_pack_replay_ledger(raw_ledger: Option<&str>, expected_hash: Option<&str>) -> Value {
    let Some(raw_ledger) = raw_ledger else {
        return json!({
            "status": "missing",
            "hashVerified": false,
            "schema": null,
            "selectedItemCount": 0,
            "omittedItemCount": 0,
            "freshnessStates": {},
            "redactionClasses": [],
            "degradationCodes": [],
            "derivedAssets": {},
            "database": {},
            "candidateCounts": {},
        });
    };

    let Ok(parsed) = serde_json::from_str::<Value>(raw_ledger) else {
        return json!({
            "status": "malformed",
            "hashVerified": false,
            "schema": null,
            "selectedItemCount": 0,
            "omittedItemCount": 0,
            "freshnessStates": {},
            "redactionClasses": [],
            "degradationCodes": [],
            "derivedAssets": {},
            "database": {},
            "candidateCounts": {},
        });
    };

    let actual_hash = parsed.get("ledgerHash").and_then(Value::as_str);
    let hash_verified = expected_hash.is_some_and(|hash| Some(hash) == actual_hash);
    let status = if expected_hash.is_some() && !hash_verified {
        "hash_mismatch"
    } else {
        "available"
    };
    let selected_items = support_ledger_core_array(&parsed, "selectedItems");
    let omitted_items = support_ledger_core_array(&parsed, "omittedItems");

    json!({
        "status": status,
        "hashVerified": hash_verified,
        "schema": support_ledger_core_value(&parsed, "schema").cloned().unwrap_or(Value::Null),
        "selectedItemCount": selected_items.map_or(0, Vec::len),
        "omittedItemCount": omitted_items.map_or(0, Vec::len),
        "freshnessStates": pack_ledger_freshness_counts(selected_items),
        "redactionClasses": pack_ledger_redaction_classes(selected_items),
        "degradationCodes": pack_ledger_degradation_codes(&parsed),
        "derivedAssets": support_ledger_core_value(&parsed, "derivedAssets").cloned().unwrap_or_else(|| json!({})),
        "database": support_ledger_core_value(&parsed, "database").cloned().unwrap_or_else(|| json!({})),
        "candidateCounts": support_ledger_core_value(&parsed, "candidateCounts").cloned().unwrap_or_else(|| json!({})),
    })
}

fn support_ledger_core_value<'a>(ledger: &'a Value, field: &str) -> Option<&'a Value> {
    ledger
        .get("core")
        .and_then(|core| core.get(field))
        .or_else(|| ledger.get(field))
}

fn support_ledger_core_array<'a>(ledger: &'a Value, field: &str) -> Option<&'a Vec<Value>> {
    support_ledger_core_value(ledger, field).and_then(Value::as_array)
}

fn pack_ledger_freshness_counts(selected_items: Option<&Vec<Value>>) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for item in selected_items.into_iter().flatten() {
        let freshness = item
            .get("freshness")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        *counts.entry(freshness.to_owned()).or_insert(0) += 1;
    }
    counts
}

fn pack_ledger_redaction_classes(selected_items: Option<&Vec<Value>>) -> Vec<String> {
    let mut classes = BTreeSet::new();
    for item in selected_items.into_iter().flatten() {
        if let Some(values) = item.get("redactionClasses").and_then(Value::as_array) {
            classes.extend(values.iter().filter_map(Value::as_str).map(str::to_owned));
        }
    }
    classes.into_iter().collect()
}

fn pack_ledger_degradation_codes(ledger: &Value) -> Vec<String> {
    let mut codes = support_ledger_core_array(ledger, "degraded")
        .into_iter()
        .flatten()
        .filter_map(|degradation| degradation.get("code").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    codes.sort();
    codes.dedup();
    codes
}

fn collect_coordination_fallback_summary(workspace: &Path) -> Value {
    let ledger_path = workspace
        .join(".ee")
        .join(COORDINATION_FALLBACK_LEDGER_FILE);
    let mut ledger = json!({
        "present": ledger_path.is_file(),
        "readable": false,
        "recordCount": 0,
        "malformedCount": 0,
        "summarizedRecordCount": 0,
    });

    if !ledger_path.exists() {
        return coordination_fallback_summary_value("ledger_missing", ledger, Vec::new());
    }
    if !regular_file_no_symlink(&ledger_path) {
        return coordination_fallback_summary_value("ledger_unreadable", ledger, Vec::new());
    }

    let Ok(content) = fs::read_to_string(&ledger_path) else {
        return coordination_fallback_summary_value("ledger_unreadable", ledger, Vec::new());
    };
    ledger["readable"] = json!(true);

    let mut records = Vec::new();
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        ledger["recordCount"] = json!(
            ledger
                .get("recordCount")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + 1
        );
        match serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|record| summarize_coordination_fallback_record(&record))
        {
            Some(summary) if records.len() < MAX_COORDINATION_FALLBACK_SUMMARY_RECORDS => {
                records.push(summary);
            }
            Some(_) => {}
            None => increment_json_count(&mut ledger, "malformedCount"),
        }
    }

    records.sort_by(|left, right| {
        left.pointer("/evidenceId")
            .and_then(Value::as_str)
            .cmp(&right.pointer("/evidenceId").and_then(Value::as_str))
            .then_with(|| {
                left.pointer("/contentHash")
                    .and_then(Value::as_str)
                    .cmp(&right.pointer("/contentHash").and_then(Value::as_str))
            })
    });
    ledger["summarizedRecordCount"] = json!(records.len());

    coordination_fallback_summary_value("available", ledger, records)
}

fn coordination_fallback_summary_value(status: &str, ledger: Value, records: Vec<Value>) -> Value {
    let status_counts = coordination_fallback_counts(&records, "/status");
    let source_counts = coordination_fallback_counts(&records, "/source/kind");
    let mut reason_codes = records
        .iter()
        .filter_map(|record| record.get("reasonCode").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    reason_codes.sort();
    reason_codes.dedup();

    json!({
        "schema": "ee.support_bundle.coordination_fallback_summary.v1",
        "sourceSchema": "ee.coordination_fallback_evidence.v1",
        "source": ".ee/coordination-fallback-evidence.jsonl",
        "status": status,
        "redactionStatus": "ids_hashes_status_counts_only_no_raw_logs_no_raw_inboxes_no_summary_text",
        "limits": {
            "maxRecords": MAX_COORDINATION_FALLBACK_SUMMARY_RECORDS,
        },
        "ledger": ledger,
        "statusCounts": status_counts,
        "sourceCounts": source_counts,
        "reasonCodes": reason_codes,
        "records": records,
    })
}

fn summarize_coordination_fallback_record(record: &Value) -> Option<Value> {
    if record.get("schema").and_then(Value::as_str)
        != Some("ee.coordination_fallback_ledger_record.v1")
    {
        return None;
    }
    let content_hash = record.get("contentHash").and_then(Value::as_str)?;
    let evidence = record.get("evidence")?;
    if evidence.get("schema").and_then(Value::as_str)
        != Some("ee.coordination_fallback_evidence.v1")
    {
        return None;
    }
    if evidence.pointer("/summary/redacted") != Some(&Value::Bool(true))
        || evidence.pointer("/redaction/rawInboxIncluded") != Some(&Value::Bool(false))
        || evidence.pointer("/redaction/rawLogIncluded") != Some(&Value::Bool(false))
    {
        return None;
    }

    Some(json!({
        "evidenceId": evidence.get("evidenceId").and_then(Value::as_str),
        "capturedAt": evidence.get("capturedAt").and_then(Value::as_str),
        "status": evidence.get("status").and_then(Value::as_str),
        "source": {
            "kind": evidence.pointer("/source/kind").and_then(Value::as_str),
            "sourceId": evidence.pointer("/source/sourceId").and_then(Value::as_str),
        },
        "reasonCode": evidence.get("reasonCode").and_then(Value::as_str),
        "contentHash": content_hash,
        "summaryContentHash": evidence.pointer("/summary/contentHash").and_then(Value::as_str),
        "fallbackActionKind": evidence.pointer("/fallbackAction/kind").and_then(Value::as_str),
        "linkedBeadIds": coordination_fallback_string_array(evidence.pointer("/links/beadIds")),
        "linkedVerificationIds": coordination_fallback_string_array(evidence.pointer("/links/verificationIds")),
        "linkedSupportBundleIds": coordination_fallback_string_array(evidence.pointer("/links/supportBundleIds")),
    }))
}

fn coordination_fallback_string_array(value: Option<&Value>) -> Vec<String> {
    let mut values = value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
}

fn coordination_fallback_counts(records: &[Value], pointer: &str) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for record in records {
        if let Some(value) = record.pointer(pointer).and_then(Value::as_str) {
            *counts.entry(value.to_owned()).or_insert(0) += 1;
        }
    }
    counts
}

fn blake3_text_hash(value: &str) -> String {
    format!("blake3:{}", compute_hash(value))
}

struct SupportDiagnosticRedaction {
    content: String,
    redacted: bool,
    redacted_reasons: Vec<String>,
}

fn discover_performance_explain_samples(workspace: &Path) -> Vec<Value> {
    let report_dir = workspace.join(".ee").join(PERFORMANCE_EXPLAIN_SAMPLE_DIR);
    let Ok(entries) = fs::read_dir(report_dir) else {
        return Vec::new();
    };

    let mut paths = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            regular_file_no_symlink(path) && path.extension().is_some_and(|ext| ext == "json")
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths.truncate(MAX_PERFORMANCE_EXPLAIN_SAMPLES);

    paths
        .iter()
        .filter_map(|path| summarize_performance_explain_sample(workspace, path))
        .collect()
}

fn summarize_performance_explain_sample(workspace: &Path, path: &Path) -> Option<Value> {
    if !regular_file_no_symlink(path) {
        return None;
    }
    let raw_content = fs::read_to_string(path).ok()?;
    let redaction = redact_support_diagnostic_content(&raw_content);
    let parsed = serde_json::from_str::<Value>(&raw_content).ok()?;
    if parsed.get("schema") != Some(&json!(super::search::PERFORMANCE_EXPLAIN_SCHEMA_V1)) {
        return None;
    }
    let data = parsed.get("data")?;
    let relative_path = path.strip_prefix(workspace).unwrap_or(path);
    let relative_path = redact_support_diagnostic_text(&relative_path.display().to_string());
    let redaction_reasons = redaction.redacted_reasons.clone();
    let fallback_count = data
        .get("fallbacks")
        .and_then(Value::as_array)
        .map_or(Value::Null, |items| json!(items.len()));

    Some(json!({
        "path": relative_path,
        "contentHash": compute_hash(&redaction.content),
        "schema": parsed.get("schema").map(redact_json_value).unwrap_or(Value::Null),
        "command": data.get("command").map(redact_json_value).unwrap_or(Value::Null),
        "query": data.get("query").map(redact_json_value).unwrap_or(Value::Null),
        "queryPlan": data.get("queryPlan").map(redact_json_value).unwrap_or_else(|| json!({})),
        "measurements": {
            "dbReads": data.get("dbReads").map(redact_json_value).unwrap_or(Value::Null),
            "searchElapsed": data.pointer("/search/elapsed").map(redact_json_value).unwrap_or(Value::Null),
            "returnedHits": data.pointer("/search/returnedHits").cloned().unwrap_or(Value::Null),
            "timings": data.get("timings").map(redact_json_value).unwrap_or_else(|| json!([])),
            "packSelectedCount": data.pointer("/pack/selectedCount").cloned().unwrap_or(Value::Null),
            "tokenBudget": data.pointer("/pack/tokenBudget").map(redact_json_value).unwrap_or(Value::Null),
            "fallbackCount": fallback_count,
        },
        "redacted": redaction.redacted,
        "redactionReasons": redaction_reasons,
    }))
}

fn triage_summary_json(status: &StatusReport, swarm_reports: &[Value]) -> String {
    let index_state = status
        .derived_assets
        .iter()
        .find(|asset| asset.name == "search_index")
        .map_or("not_reported", |asset| asset.status.as_str());

    let has_failures = swarm_reports.iter().any(|report| {
        report
            .get("failureCount")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 0
    });
    let db_integrity_failed = swarm_reports.iter().any(|report| {
        report
            .get("dbIntegrityOk")
            .and_then(Value::as_bool)
            .is_some_and(|ok| !ok)
    });
    let determinism_failed = swarm_reports.iter().any(|report| {
        report
            .get("determinismOk")
            .and_then(Value::as_bool)
            .is_some_and(|ok| !ok)
    });

    stable_json(&json!({
        "schema": "ee.support_bundle.scale_triage.v1",
        "ownerSignals": [
            {
                "owner": "search",
                "severity": if matches!(index_state, "current" | "not_reported") { "low" } else { "medium" },
                "signals": [format!("search_index={index_state}")],
                "next": "ee index status --workspace . --json",
            },
            {
                "owner": "pack",
                "severity": "low",
                "signals": ["performance explain samples include pack assembly and token-budget fields"],
                "next": "ee context <query> --explain-performance",
            },
            {
                "owner": "db",
                "severity": if db_integrity_failed { "high" } else { "low" },
                "signals": [if db_integrity_failed { "swarm report dbIntegrityOk=false" } else { "no DB integrity failure observed" }],
                "next": "ee doctor --workspace . --json",
            },
            {
                "owner": "daemon_write_queue",
                "severity": if has_failures { "medium" } else { "low" },
                "signals": [if has_failures { "one or more swarm processes failed" } else { "no process failures observed in bundled swarm reports" }],
                "next": "ee daemon status --json",
            },
            {
                "owner": "graph",
                "severity": "low",
                "signals": ["graph metrics are derived and should be checked only after DB/search are healthy"],
                "next": "ee status --workspace . --json",
            },
            {
                "owner": "policy_redaction",
                "severity": "low",
                "signals": ["scale artifacts pass through support-bundle redaction before hashing"],
                "next": "ee support inspect <bundle> --verify-hashes --json",
            },
            {
                "owner": "host_resource_pressure",
                "severity": if swarm_reports.is_empty() { "unknown" } else if determinism_failed { "medium" } else { "low" },
                "signals": [if swarm_reports.is_empty() { "no swarm smoke report was found under .ee/swarm-contention" } else if determinism_failed { "swarm report determinismOk=false" } else { "swarm reports did not flag determinism failure" }],
                "next": "Inspect RCH logs and host CPU/memory telemetry for the benchmark run.",
            }
        ],
        "recommendedOrder": [
            "db",
            "search",
            "daemon_write_queue",
            "pack",
            "host_resource_pressure",
            "graph",
            "policy_redaction"
        ],
    }))
}

fn discover_swarm_report_summaries(workspace: &Path) -> Vec<Value> {
    let report_dir = workspace.join(".ee").join("swarm-contention");
    let Ok(entries) = fs::read_dir(report_dir) else {
        return Vec::new();
    };

    let mut paths = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            regular_file_no_symlink(path) && path.extension().is_some_and(|ext| ext == "json")
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths.truncate(16);

    paths
        .iter()
        .filter_map(|path| summarize_swarm_report(workspace, path))
        .collect()
}

fn summarize_swarm_report(workspace: &Path, path: &Path) -> Option<Value> {
    if !regular_file_no_symlink(path) {
        return None;
    }
    let raw_content = fs::read_to_string(path).ok()?;
    let redaction = redact_support_diagnostic_content(&raw_content);
    let parsed = serde_json::from_str::<Value>(&raw_content).ok()?;
    let relative_path = path.strip_prefix(workspace).unwrap_or(path);
    let relative_path = redact_support_diagnostic_text(&relative_path.display().to_string());
    let redaction_reasons = redaction.redacted_reasons.clone();

    Some(json!({
        "path": relative_path,
        "contentHash": compute_hash(&redaction.content),
        "schema": parsed.get("schema").map(redact_json_value).unwrap_or(Value::Null),
        "scenario": parsed.get("scenario").map(redact_json_value).unwrap_or(Value::Null),
        "processCount": parsed.get("processCount").cloned().unwrap_or(Value::Null),
        "successCount": parsed.get("successCount").cloned().unwrap_or(Value::Null),
        "failureCount": parsed.get("failureCount").cloned().unwrap_or(Value::Null),
        "totalDurationMs": parsed.get("totalDurationMs").cloned().unwrap_or(Value::Null),
        "dbIntegrityOk": parsed.get("dbIntegrityOk").cloned().unwrap_or(Value::Null),
        "determinismOk": parsed.get("determinismOk").cloned().unwrap_or(Value::Null),
        "degradations": parsed.get("degradations").map(redact_json_value).unwrap_or_else(|| json!([])),
        "redacted": redaction.redacted,
        "redactionReasons": redaction_reasons,
    }))
}

fn redact_json_value(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact_support_diagnostic_text(text)),
        Value::Array(items) => Value::Array(items.iter().map(redact_json_value).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), redact_json_value(value)))
                .collect(),
        ),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
    }
}

fn redact_support_diagnostic_text(text: &str) -> String {
    redact_support_diagnostic_content(text).content
}

fn redact_support_bundle_content(text: &str, level: RedactionLevel) -> SupportDiagnosticRedaction {
    match level {
        RedactionLevel::None => SupportDiagnosticRedaction {
            content: text.to_owned(),
            redacted: false,
            redacted_reasons: Vec::new(),
        },
        RedactionLevel::Minimal => {
            let report = redact_secret_like_content(text);
            SupportDiagnosticRedaction {
                content: report.content,
                redacted: report.redacted,
                redacted_reasons: report
                    .redacted_reasons
                    .iter()
                    .map(|reason| (*reason).to_owned())
                    .collect(),
            }
        }
        RedactionLevel::Standard
        | RedactionLevel::Strict
        | RedactionLevel::Paranoid
        | RedactionLevel::Full => redact_support_diagnostic_content(text),
    }
}

fn redact_support_diagnostic_content(text: &str) -> SupportDiagnosticRedaction {
    let secret_redacted = redact_secret_like_content(text);
    let (tailscale_redacted_content, tailscale_redacted) =
        redact_tailscale_metadata_segments(&secret_redacted.content);
    let path_redacted_content = redact_path_like_segments(&tailscale_redacted_content);
    let path_redacted = path_redacted_content != tailscale_redacted_content;
    let mut redacted_reasons = secret_redacted
        .redacted_reasons
        .iter()
        .map(|reason| (*reason).to_owned())
        .collect::<Vec<_>>();
    if tailscale_redacted {
        redacted_reasons.push("tailscale_metadata".to_owned());
    }
    if path_redacted {
        redacted_reasons.push("path_like_segment".to_owned());
    }
    redacted_reasons.sort();
    redacted_reasons.dedup();

    SupportDiagnosticRedaction {
        content: path_redacted_content,
        redacted: secret_redacted.redacted || tailscale_redacted || path_redacted,
        redacted_reasons,
    }
}

fn redact_tailscale_metadata_segments(input: &str) -> (String, bool) {
    if let Ok(mut value) = serde_json::from_str::<Value>(input) {
        if redact_tailscale_metadata_json_value(&mut value) {
            return (stable_json(&value), true);
        }
        return (input.to_owned(), false);
    }

    redact_tailscale_metadata_json_lines(input)
}

fn redact_tailscale_metadata_json_lines(input: &str) -> (String, bool) {
    let mut output = String::with_capacity(input.len());
    let mut redacted = false;

    for line in input.split_inclusive('\n') {
        let (record, newline) = line
            .strip_suffix('\n')
            .map_or((line, ""), |record| (record, "\n"));
        if record.trim().is_empty() {
            output.push_str(line);
            continue;
        }
        let Ok(mut value) = serde_json::from_str::<Value>(record) else {
            output.push_str(line);
            continue;
        };
        if redact_tailscale_metadata_json_value(&mut value) {
            output.push_str(&stable_json(&value));
            output.push_str(newline);
            redacted = true;
        } else {
            output.push_str(line);
        }
    }

    (output, redacted)
}

fn redact_tailscale_metadata_json_value(value: &mut Value) -> bool {
    match value {
        Value::Object(map) => {
            let mut redacted = false;
            for (key, child) in map {
                if is_tailscale_metadata_field(key) {
                    let raw_value = stable_json(child);
                    *child = Value::String(tailscale_metadata_placeholder(key, &raw_value));
                    redacted = true;
                } else if redact_tailscale_metadata_json_value(child) {
                    redacted = true;
                }
            }
            redacted
        }
        Value::Array(items) => {
            let mut redacted = false;
            for item in items {
                if redact_tailscale_metadata_json_value(item) {
                    redacted = true;
                }
            }
            redacted
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn is_tailscale_metadata_field(field: &str) -> bool {
    TAILSCALE_METADATA_FIELDS.contains(&field)
}

fn tailscale_metadata_placeholder(field: &str, raw_value: &str) -> String {
    let digest = blake3::hash(raw_value.as_bytes());
    let hex = digest.to_hex();
    format!("[REDACTED:tailscale_metadata:{field}:#{}]", &hex[..12])
}

fn redact_path_like_segments(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let placeholder = redaction_placeholder("path");
    let mut index = 0;
    while index < input.len() {
        if path_like_prefix_at(input, index) {
            output.push_str(&placeholder);
            index = path_like_segment_end(input, index);
            continue;
        }
        let Some(ch) = input[index..].chars().next() else {
            break;
        };
        output.push(ch);
        index += ch.len_utf8();
    }
    output
}

fn path_like_prefix_at(input: &str, index: usize) -> bool {
    [
        "/Users/",
        "/home/",
        "/private/",
        "/Volumes/",
        "/var/folders/",
    ]
    .iter()
    .any(|prefix| input[index..].starts_with(prefix))
}

fn path_like_segment_end(input: &str, start: usize) -> usize {
    input[start..]
        .char_indices()
        .find_map(|(offset, ch)| {
            (ch.is_whitespace()
                || matches!(
                    ch,
                    '"' | '\'' | ',' | '}' | ']' | ')' | '(' | '<' | '>' | ';'
                ))
            .then_some(start + offset)
        })
        .unwrap_or(input.len())
}

fn stable_json(value: &Value) -> String {
    match serde_json::to_string(value) {
        Ok(serialized) => serialized,
        Err(error) => json!({
            "schema": "ee.support_bundle.serialization_error.v1",
            "message": error.to_string(),
        })
        .to_string(),
    }
}

fn support_cache_key(payload: &str) -> String {
    format!("blake3:{}", compute_hash(payload))
}

fn collect_audit_entries(workspace: &Path, limit: u32) -> String {
    let database_path = workspace.join(".ee").join("ee.db");
    if !database_path.is_file() {
        return "[]".to_string();
    }

    let Ok(connection) = DbConnection::open_file(&database_path) else {
        return "[]".to_string();
    };

    let workspace_key = workspace.to_string_lossy();
    let Ok(Some(workspace_row)) = connection.get_workspace_by_path(&workspace_key) else {
        return "[]".to_string();
    };

    let Ok(entries) = connection.list_audit_entries(Some(&workspace_row.id), Some(limit)) else {
        return "[]".to_string();
    };

    let mut lines = Vec::new();
    for entry in entries {
        let entry_json = json!({
            "id": entry.id,
            "timestamp": entry.timestamp,
            "actor": entry.actor,
            "action": entry.action,
            "targetType": entry.target_type,
            "targetId": entry.target_id,
            "surface": entry.surface,
            "mutationKind": entry.mutation_kind,
        });
        lines.push(entry_json.to_string());
    }
    lines.join("\n")
}

fn planned_files() -> Vec<String> {
    vec![
        STATUS_FILE.to_owned(),
        DOCTOR_FILE.to_owned(),
        AUDIT_FILE.to_owned(),
        CAPABILITIES_FILE.to_owned(),
        SCHEMA_FILE.to_owned(),
        PROFILE_EVIDENCE_FILE.to_owned(),
        AGENT_PROFILE_EVIDENCE_FILE.to_owned(),
        SCALE_BENCHMARK_SUMMARY_FILE.to_owned(),
        SCALE_FIXTURE_MANIFEST_FILE.to_owned(),
        CACHE_REPORTS_FILE.to_owned(),
        WRITE_QUEUE_REPORT_FILE.to_owned(),
        PERFORMANCE_EXPLAIN_SAMPLES_FILE.to_owned(),
        PACK_REPLAY_SUMMARY_FILE.to_owned(),
        SWARM_BRIEF_SUMMARY_FILE.to_owned(),
        COORDINATION_FALLBACK_SUMMARY_FILE.to_owned(),
        SINGLEFLIGHT_POSTURE_FILE.to_owned(),
        QOS_LANE_SUMMARY_FILE.to_owned(),
        TRIAGE_SUMMARY_FILE.to_owned(),
        MANIFEST_FILE.to_owned(),
    ]
}

fn generate_bundle_id() -> String {
    let now = Utc::now();
    format!("{}", now.format("%Y%m%d_%H%M%S"))
}

fn write_file_with_hash(path: &Path, content: &str) -> Result<u64, DomainError> {
    reject_existing_symlink_component(path, "support bundle file")?;
    let mut file = File::create(path).map_err(|e| DomainError::Storage {
        message: format!("Failed to create file {}: {e}", path.display()),
        repair: None,
    })?;
    file.write_all(content.as_bytes())
        .map_err(|e| DomainError::Storage {
            message: format!("Failed to write file {}: {e}", path.display()),
            repair: None,
        })?;
    Ok(content.len() as u64)
}

fn reject_existing_symlink_component(path: &Path, label: &str) -> Result<(), DomainError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(DomainError::Storage {
                    message: format!(
                        "Refusing to access {label} through symlinked path component {}.",
                        current.display()
                    ),
                    repair: Some("Choose a real, non-symlink support bundle path.".to_owned()),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(DomainError::Storage {
                    message: format!(
                        "Failed to inspect {label} path component {}: {error}",
                        current.display()
                    ),
                    repair: Some("Check support bundle path permissions.".to_owned()),
                });
            }
        }
    }
    Ok(())
}

fn resolve_bundle_file_no_symlinks(
    bundle_dir: &Path,
    relative: &str,
) -> Result<PathBuf, DomainError> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(DomainError::Storage {
            message: format!("Unsafe support bundle manifest path: {relative}."),
            repair: Some("Regenerate the support bundle with relative file names only.".to_owned()),
        });
    }

    let resolved = bundle_dir.join(relative_path);
    reject_existing_symlink_component(&resolved, "support bundle file")?;
    Ok(resolved)
}

fn regular_file_no_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
        .unwrap_or(false)
}

fn read_regular_file_no_symlinks(path: &Path) -> Result<String, DomainError> {
    if !regular_file_no_symlink(path) {
        return Err(DomainError::Storage {
            message: format!(
                "Support bundle file is not a regular non-symlink file: {}.",
                path.display()
            ),
            repair: Some("Regenerate the support bundle.".to_owned()),
        });
    }
    fs::read_to_string(path).map_err(|error| DomainError::Storage {
        message: format!(
            "Failed to read support bundle file {}: {error}",
            path.display()
        ),
        repair: Some("Check support bundle file permissions.".to_owned()),
    })
}

fn compute_hash(content: &str) -> String {
    let mut hasher = Hasher::new();
    hasher.update(content.as_bytes());
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    #[test]
    fn plan_bundle_dry_run() -> TestResult {
        let options = BundleOptions {
            workspace: PathBuf::from("."),
            output_dir: None,
            dry_run: true,
            redacted: true,
            redaction_level: RedactionLevel::Paranoid,
            include_raw: false,
            audit_limit: 100,
        };
        let report = plan_bundle(&options).map_err(|e| e.message())?;
        assert!(report.dry_run);
        assert!(report.redaction_applied);
        assert!(!report.files_collected.is_empty());
        Ok(())
    }

    #[test]
    fn create_bundle_requires_output() {
        let options = BundleOptions {
            workspace: PathBuf::from("."),
            output_dir: None,
            dry_run: false,
            redacted: true,
            redaction_level: RedactionLevel::Paranoid,
            include_raw: false,
            audit_limit: 100,
        };
        let result = create_bundle(&options);
        assert!(result.is_err());
    }

    #[test]
    fn compute_hash_deterministic() {
        let content = "test content for hashing";
        let hash1 = compute_hash(content);
        let hash2 = compute_hash(content);
        assert_eq!(hash1, hash2);
        assert!(!hash1.is_empty());
    }

    #[test]
    fn support_bundle_redaction_levels_control_final_pass() {
        let raw = "home=/Users/alice/private token=sk-test_123456789abcdefghijklmnop";

        let none = redact_support_bundle_content(raw, RedactionLevel::None);
        assert_eq!(none.content, raw);
        assert!(!none.redacted);

        let minimal = redact_support_bundle_content(raw, RedactionLevel::Minimal);
        assert!(minimal.redacted);
        assert!(
            minimal.content.contains("/Users/alice/private"),
            "minimal support bundle redaction should leave paths intact"
        );
        assert!(
            !minimal
                .content
                .contains("sk-test_123456789abcdefghijklmnop"),
            "minimal support bundle redaction should redact secret-like values"
        );

        let standard = redact_support_bundle_content(raw, RedactionLevel::Standard);
        assert!(standard.redacted);
        assert!(
            !standard.content.contains("/Users/alice/private"),
            "standard support bundle redaction should redact path-like segments"
        );
        assert!(
            standard
                .redacted_reasons
                .iter()
                .any(|reason| reason == "path_like_segment"),
            "standard support bundle redaction should report path-like redaction"
        );
    }

    #[test]
    fn support_bundle_standard_redacts_tailscale_metadata_fields() -> TestResult {
        let raw = r#"{"mesh":{"tailscale":{"selfNodeKey":"nodekey:selfalpha","selfTailscaleIp":"100.64.0.10","selfMagicDnsName":"ee-local.tailnet.test.","tailnetId":"tailnet-alpha","tailnetDisplayName":"alpha.example","selfAdvertisedTags":["tag:ee-mesh","tag:memory"],"binaryVersionRaw":"1.66.0\n  tailscale commit: abc","binaryAbsolutePath":"/opt/homebrew/bin/tailscale","probeMethod":"cli"}}}"#;

        let report = redact_support_bundle_content(raw, RedactionLevel::Standard);

        assert!(report.redacted);
        assert!(
            report
                .redacted_reasons
                .iter()
                .any(|reason| reason == "tailscale_metadata"),
            "support bundle redaction should report tailscale metadata redaction"
        );
        for raw_value in [
            "nodekey:selfalpha",
            "100.64.0.10",
            "ee-local.tailnet.test.",
            "tailnet-alpha",
            "alpha.example",
            "tag:ee-mesh",
            "/opt/homebrew/bin/tailscale",
        ] {
            assert!(
                !report.content.contains(raw_value),
                "redacted content leaked {raw_value}: {}",
                report.content
            );
        }
        for field in TAILSCALE_METADATA_FIELDS {
            let marker = format!("[REDACTED:tailscale_metadata:{field}:#");
            assert!(
                report.content.contains(&marker),
                "redacted content missing marker {marker}: {}",
                report.content
            );
        }
        assert!(
            report.content.contains(r#""probeMethod":"cli""#),
            "non-sensitive Tailscale fields should remain intact"
        );
        serde_json::from_str::<Value>(&report.content).map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn support_bundle_tailscale_metadata_respects_raw_and_minimal_levels() {
        let raw = r#"{"selfNodeKey":"nodekey:selfalpha","binaryAbsolutePath":"/opt/homebrew/bin/tailscale"}"#;

        let none = redact_support_bundle_content(raw, RedactionLevel::None);
        let minimal = redact_support_bundle_content(raw, RedactionLevel::Minimal);

        assert_eq!(none.content, raw);
        assert!(!none.redacted);
        assert!(minimal.content.contains("nodekey:selfalpha"));
        assert!(minimal.content.contains("/opt/homebrew/bin/tailscale"));
        assert!(
            !minimal
                .redacted_reasons
                .iter()
                .any(|reason| reason == "tailscale_metadata")
        );
    }

    #[test]
    fn coordination_fallback_summary_redacts_ledger_evidence() -> TestResult {
        let workspace = unique_test_path("coordination-fallback-summary");
        let ledger_dir = workspace.join(".ee");
        fs::create_dir_all(&ledger_dir)
            .map_err(|error| format!("failed to create ledger dir: {error}"))?;

        let raw_summary =
            "Agent Mail failed, so the agent recorded fallback evidence without raw inbox bodies.";
        let evidence = json!({
            "schema": "ee.coordination_fallback_evidence.v1",
            "evidenceId": "coord_fallback_test_01",
            "capturedAt": "2026-05-16T21:06:00Z",
            "status": "unavailable",
            "source": {"kind": "agent_mail", "sourceId": "mcp-agent-mail"},
            "reasonCode": "agent_mail_transport_unavailable",
            "summary": {
                "text": raw_summary,
                "contentHash": blake3_text_hash(raw_summary),
                "redacted": true
            },
            "links": {
                "beadIds": ["bd-1zb7k.13.2"],
                "verificationIds": ["rch_cmd_test"],
                "supportBundleIds": []
            },
            "fallbackAction": {
                "kind": "record_only",
                "summary": "Preserve redacted fallback evidence.",
                "command": null,
                "manualStep": null
            },
            "redaction": {
                "rawInboxIncluded": false,
                "rawLogIncluded": false,
                "secretScanApplied": true,
                "pathPolicy": "labels_only"
            },
            "producer": {
                "schema": "ee.producer.metadata.v1",
                "sourceSystem": "coordination_fallback",
                "identity": {"status": "unknown", "agentName": null, "harness": null, "model": null},
                "run": {"runId": "coord-fallback-test", "sessionId": null, "workspaceFingerprint": "repo:test"},
                "observedAt": "2026-05-16T21:06:00Z"
            }
        });
        let content_hash = blake3_text_hash(&stable_json(&evidence));
        let ledger_record = json!({
            "schema": "ee.coordination_fallback_ledger_record.v1",
            "contentHash": content_hash,
            "evidence": evidence,
        });
        fs::write(
            ledger_dir.join(COORDINATION_FALLBACK_LEDGER_FILE),
            stable_json(&ledger_record),
        )
        .map_err(|error| format!("failed to write ledger: {error}"))?;

        let summary = collect_coordination_fallback_summary(&workspace);
        let summary_text = stable_json(&summary);

        assert_eq!(
            summary.pointer("/schema"),
            Some(&json!("ee.support_bundle.coordination_fallback_summary.v1"))
        );
        assert_eq!(summary.pointer("/status"), Some(&json!("available")));
        assert_eq!(summary.pointer("/ledger/recordCount"), Some(&json!(1)));
        assert_eq!(
            summary.pointer("/statusCounts/unavailable"),
            Some(&json!(1))
        );
        assert_eq!(summary.pointer("/sourceCounts/agent_mail"), Some(&json!(1)));
        assert_eq!(
            summary.pointer("/records/0/evidenceId"),
            Some(&json!("coord_fallback_test_01"))
        );
        assert_eq!(
            summary.pointer("/records/0/contentHash"),
            Some(&json!(content_hash))
        );
        assert!(
            summary_text.contains("agent_mail_transport_unavailable"),
            "summary should keep stable reason code"
        );
        assert!(
            !summary_text.contains(raw_summary),
            "summary must not include raw fallback summary text"
        );
        Ok(())
    }

    #[test]
    fn support_bundle_include_raw_forces_effective_none() {
        let options = BundleOptions {
            redaction_level: RedactionLevel::Paranoid,
            include_raw: true,
            ..BundleOptions::default()
        };
        assert_eq!(options.effective_redaction_level(), RedactionLevel::None);
    }

    #[test]
    fn generate_bundle_id_format() {
        let id = generate_bundle_id();
        assert!(id.contains('_'));
        assert!(id.len() >= 15);
    }

    #[test]
    fn inspect_missing_bundle_returns_error() {
        let options = InspectOptions {
            bundle_path: PathBuf::from("/nonexistent/path/bundle"),
            verify_hashes: true,
        };
        let result = inspect_bundle(&options);
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn create_bundle_rejects_symlinked_output_parent() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = unique_test_path("create-symlink-output");
        let workspace = root.join("workspace");
        let real_output = root.join("real-output");
        let linked_output = root.join("linked-output");
        fs::create_dir_all(workspace.join(".ee"))
            .map_err(|error| format!("failed to create workspace: {error}"))?;
        fs::create_dir_all(&real_output)
            .map_err(|error| format!("failed to create real output: {error}"))?;
        symlink(&real_output, &linked_output)
            .map_err(|error| format!("failed to create output symlink: {error}"))?;

        let result = create_bundle(&BundleOptions {
            workspace,
            output_dir: Some(linked_output),
            dry_run: false,
            redacted: true,
            redaction_level: RedactionLevel::Paranoid,
            include_raw: false,
            audit_limit: 5,
        });
        let error = result.expect_err("symlinked output parent should be rejected");
        assert!(
            error.message().contains("symlinked path component"),
            "unexpected error: {}",
            error.message()
        );
        assert!(
            fs::read_dir(real_output)
                .map_err(|error| format!("failed to read real output: {error}"))?
                .next()
                .is_none(),
            "support bundle creation must not write through the symlink target"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn inspect_bundle_rejects_symlinked_manifest() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = unique_test_path("inspect-symlink-manifest");
        let bundle_dir = root.join("bundle");
        fs::create_dir_all(&bundle_dir)
            .map_err(|error| format!("failed to create bundle dir: {error}"))?;
        let outside_manifest = root.join("outside-manifest.json");
        fs::write(&outside_manifest, "{}")
            .map_err(|error| format!("failed to write outside manifest: {error}"))?;
        symlink(&outside_manifest, bundle_dir.join(MANIFEST_FILE))
            .map_err(|error| format!("failed to create manifest symlink: {error}"))?;

        let result = inspect_bundle(&InspectOptions {
            bundle_path: bundle_dir,
            verify_hashes: true,
        });
        let error = result.expect_err("symlinked manifest should be rejected");
        assert!(
            error.message().contains("symlinked path component"),
            "unexpected error: {}",
            error.message()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn inspect_bundle_marks_symlinked_manifest_entry_mismatch() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = unique_test_path("inspect-symlink-entry");
        let bundle_dir = root.join("bundle");
        fs::create_dir_all(&bundle_dir)
            .map_err(|error| format!("failed to create bundle dir: {error}"))?;
        let secret = "outside bundle evidence should not be hashed";
        let outside_file = root.join("outside.json");
        fs::write(&outside_file, secret)
            .map_err(|error| format!("failed to write outside file: {error}"))?;
        symlink(&outside_file, bundle_dir.join("leak.json"))
            .map_err(|error| format!("failed to create entry symlink: {error}"))?;

        let manifest = BundleManifest {
            schema: SUPPORT_BUNDLE_MANIFEST_SCHEMA_V1.to_owned(),
            bundle_id: "test-bundle".to_owned(),
            created_at: "2026-05-16T00:00:00Z".to_owned(),
            workspace_path: "redacted-workspace".to_owned(),
            ee_version: "test".to_owned(),
            files: vec![ManifestEntry {
                path: "leak.json".to_owned(),
                size_bytes: secret.len() as u64,
                content_hash: compute_hash(secret),
                redacted: true,
            }],
            total_size_bytes: secret.len() as u64,
            redaction_applied: true,
            redaction_reasons: vec![],
        };
        fs::write(
            bundle_dir.join(MANIFEST_FILE),
            serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("failed to write manifest: {error}"))?;

        let report = inspect_bundle(&InspectOptions {
            bundle_path: bundle_dir,
            verify_hashes: true,
        })
        .map_err(|error| error.message())?;
        assert!(!report.valid, "symlinked entry must not validate");
        assert!(
            report.hash_mismatches.contains(&"leak.json".to_owned()),
            "symlinked entry should be reported as a mismatch"
        );
        assert!(
            !report.files_found.contains(&"leak.json".to_owned()),
            "symlinked entry must not count as collected bundle evidence"
        );
        Ok(())
    }

    #[test]
    fn inspect_bundle_marks_parent_traversal_manifest_entry_mismatch() -> TestResult {
        let root = unique_test_path("inspect-parent-traversal");
        let bundle_dir = root.join("bundle");
        fs::create_dir_all(&bundle_dir)
            .map_err(|error| format!("failed to create bundle dir: {error}"))?;
        let outside_content = "outside bundle content";
        fs::write(root.join("outside.json"), outside_content)
            .map_err(|error| format!("failed to write outside file: {error}"))?;

        let manifest = BundleManifest {
            schema: SUPPORT_BUNDLE_MANIFEST_SCHEMA_V1.to_owned(),
            bundle_id: "test-bundle".to_owned(),
            created_at: "2026-05-16T00:00:00Z".to_owned(),
            workspace_path: "redacted-workspace".to_owned(),
            ee_version: "test".to_owned(),
            files: vec![ManifestEntry {
                path: "../outside.json".to_owned(),
                size_bytes: outside_content.len() as u64,
                content_hash: compute_hash(outside_content),
                redacted: true,
            }],
            total_size_bytes: outside_content.len() as u64,
            redaction_applied: true,
            redaction_reasons: vec![],
        };
        fs::write(
            bundle_dir.join(MANIFEST_FILE),
            serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("failed to write manifest: {error}"))?;

        let report = inspect_bundle(&InspectOptions {
            bundle_path: bundle_dir,
            verify_hashes: true,
        })
        .map_err(|error| error.message())?;
        assert!(!report.valid, "parent traversal entry must not validate");
        assert!(
            report
                .hash_mismatches
                .contains(&"../outside.json".to_owned()),
            "parent traversal entry should be reported as a mismatch"
        );
        assert!(
            !report.files_found.contains(&"../outside.json".to_owned()),
            "parent traversal entry must not count as collected bundle evidence"
        );
        Ok(())
    }

    #[test]
    fn redaction_summary_tracks_reasons() {
        let summary = RedactionSummary {
            total_redactions: 2,
            reasons: vec!["api_key".to_owned(), "password".to_owned()],
        };
        assert_eq!(summary.total_redactions, 2);
        assert_eq!(summary.reasons.len(), 2);
    }

    #[test]
    fn planned_files_include_scale_regression_artifacts() {
        let files = planned_files();
        for required in [
            PROFILE_EVIDENCE_FILE,
            AGENT_PROFILE_EVIDENCE_FILE,
            SCALE_BENCHMARK_SUMMARY_FILE,
            SCALE_FIXTURE_MANIFEST_FILE,
            CACHE_REPORTS_FILE,
            WRITE_QUEUE_REPORT_FILE,
            PERFORMANCE_EXPLAIN_SAMPLES_FILE,
            PACK_REPLAY_SUMMARY_FILE,
            SWARM_BRIEF_SUMMARY_FILE,
            TRIAGE_SUMMARY_FILE,
            QOS_LANE_SUMMARY_FILE,
        ] {
            assert!(
                files.contains(&required.to_owned()),
                "planned support-bundle files must include {required}"
            );
        }
    }

    #[test]
    fn qos_lane_summary_collects_redacted_active_lane_counts() -> TestResult {
        let workspace = unique_test_path("qos-lane-summary");
        fs::create_dir_all(workspace.join(".ee"))
            .map_err(|error| format!("failed to create workspace: {error}"))?;
        let now = Utc::now()
            .timestamp_millis()
            .try_into()
            .map_err(|error| format!("failed to convert timestamp: {error}"))?;

        super::super::qos::publish_qos_lane_record(
            &workspace,
            &super::super::qos::QosLaneRecordInput {
                workspace_identity: "/private/workspace/path",
                lane: super::super::qos::QosLane::ForegroundRead,
                command_class: "context",
                process_id: Some(42),
                profile_label: Some("portable"),
                budget_label: Some("interactive"),
                request_text: Some("summarize private task"),
                request_hash: None,
                started_at_epoch_ms: now,
                ttl_ms: 60_000,
                status: super::super::qos::QosLaneStatus::Active,
            },
        )
        .map_err(|error| error.message())?;

        let rendered = qos_lane_summary_json(&workspace);
        let value: Value = serde_json::from_str(&rendered)
            .map_err(|error| format!("failed to parse qos summary: {error}"))?;
        assert_eq!(
            value.get("schema"),
            Some(&json!(super::super::qos::QOS_ACTIVE_LANE_SUMMARY_SCHEMA_V1))
        );
        assert_eq!(value.get("foregroundActiveCount"), Some(&json!(1)));
        assert_eq!(value.get("backgroundActiveCount"), Some(&json!(0)));
        let active_record = value
            .get("activeRecords")
            .and_then(Value::as_array)
            .and_then(|records| records.first())
            .ok_or_else(|| "expected one active QoS record".to_owned())?;
        assert_eq!(active_record.get("lane"), Some(&json!("foreground_read")));
        assert!(
            active_record.get("requestHash").is_some(),
            "support summary should include a redacted request hash"
        );
        assert!(
            !rendered.contains("summarize private task"),
            "support summary must not include raw request text"
        );
        assert!(
            !rendered.contains("/private/workspace/path"),
            "support summary must not include raw workspace path"
        );
        Ok(())
    }

    #[test]
    fn profile_evidence_uses_workspace_config_and_reports_provenance() -> TestResult {
        let root = unique_test_path("profile-evidence");
        let workspace = root.join("workspace");
        let config_dir = workspace.join(".ee");
        fs::create_dir_all(&config_dir)
            .map_err(|error| format!("failed to create config dir: {error}"))?;
        fs::write(
            config_dir.join("config.toml"),
            "profile = { selected = \"portable\" }\n",
        )
        .map_err(|error| format!("failed to write profile config: {error}"))?;

        let rendered = profile_evidence_json(&workspace);
        let value: Value = serde_json::from_str(&rendered)
            .map_err(|error| format!("profile evidence must parse: {error}"))?;

        assert_eq!(
            value.pointer("/schema"),
            Some(&json!("ee.support_bundle.profile_evidence.v1"))
        );
        assert_eq!(
            value.pointer("/profile/activeProfile"),
            Some(&json!("portable"))
        );
        assert_eq!(
            value.pointer("/profile/source"),
            Some(&json!("workspace_config"))
        );
        assert_eq!(
            value.pointer("/budgets/diagnostics/supportBundleProfile"),
            Some(&json!("standard"))
        );
        assert_eq!(
            value.pointer("/verificationRecipe/profile"),
            Some(&json!("portable"))
        );
        assert_eq!(
            value.pointer("/verificationRecipe/recipeName"),
            Some(&json!("workspace"))
        );
        assert_eq!(
            value.pointer("/probe/workspace/redaction"),
            Some(&json!("path_not_emitted"))
        );
        assert!(
            !rendered.contains(&workspace.display().to_string()),
            "profile evidence must not emit raw workspace paths"
        );

        let provenance = value
            .pointer("/provenance")
            .and_then(Value::as_array)
            .ok_or_else(|| "profile evidence provenance must be an array".to_owned())?;
        for required in [
            "profile.activeProfile",
            "profile.recommendedProfile",
            "probe",
            "budgets",
            "verificationRecipe",
            "degraded",
        ] {
            assert!(
                provenance
                    .iter()
                    .any(|entry| entry.pointer("/field") == Some(&json!(required))),
                "profile evidence provenance must include {required}"
            );
        }

        Ok(())
    }

    #[test]
    fn agent_profile_evidence_hashes_agent_names_by_default() -> TestResult {
        let root = unique_test_path("agent-profile-evidence");
        let workspace = root.join("workspace");
        fs::create_dir_all(workspace.join(".ee"))
            .map_err(|error| format!("failed to create workspace metadata dir: {error}"))?;
        let workspace = workspace
            .canonicalize()
            .map_err(|error| format!("failed to canonicalize workspace: {error}"))?;

        let database_path = workspace.join(".ee").join("ee.db");
        let connection = DbConnection::open_file(&database_path)
            .map_err(|error| format!("failed to open test db: {error}"))?;
        connection
            .migrate()
            .map_err(|error| format!("failed to migrate test db: {error}"))?;

        let workspace_id = "wsp_01234567890123456789012347";
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("agent-profile-evidence".to_owned()),
                },
            )
            .map_err(|error| format!("failed to insert workspace: {error}"))?;

        let memory_id = "mem_00000000000000000000aprof1";
        connection
            .insert_memory(
                memory_id,
                &crate::db::CreateMemoryInput {
                    workspace_id: workspace_id.to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Prefer the memory that produced helpful agent outcomes.".to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: Some("test://support-bundle/agent-profile".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: Some("test".to_owned()),
                    tags: vec!["agent-profile".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| format!("failed to insert memory: {error}"))?;

        for (agent_name, helpful_count, harmful_count) in [
            ("AgentProfileAlpha", 12_u32, 1_u32),
            ("AgentProfileBeta", 1_u32, 12_u32),
        ] {
            connection
                .upsert_agent_context_profile_event(&crate::db::UpsertAgentContextProfileInput {
                    workspace_id: workspace_id.to_owned(),
                    agent_name: agent_name.to_owned(),
                    memory_id: memory_id.to_owned(),
                    counts_delta: crate::models::AgentContextProfileCounts::new(
                        helpful_count,
                        harmful_count,
                        2,
                    ),
                    last_seen_at: Some("2026-05-16T00:00:00Z".to_owned()),
                    weight_cached: 0.0,
                })
                .map_err(|error| format!("failed to upsert agent profile: {error}"))?;
        }
        connection
            .close()
            .map_err(|error| format!("failed to close test db: {error}"))?;

        let output_dir = root.join("out");
        fs::create_dir_all(&output_dir)
            .map_err(|error| format!("failed to create output dir: {error}"))?;
        let report = create_bundle(&BundleOptions {
            workspace,
            output_dir: Some(output_dir),
            dry_run: false,
            redacted: true,
            redaction_level: RedactionLevel::Paranoid,
            include_raw: false,
            audit_limit: 5,
        })
        .map_err(|error| error.message())?;

        assert!(
            report
                .files_collected
                .contains(&AGENT_PROFILE_EVIDENCE_FILE.to_owned()),
            "support bundle must include agent profile evidence"
        );
        let bundle_dir = report
            .output_path
            .clone()
            .ok_or_else(|| "created bundle must report output path".to_owned())?;
        let evidence_text = fs::read_to_string(bundle_dir.join(AGENT_PROFILE_EVIDENCE_FILE))
            .map_err(|error| format!("failed to read agent profile evidence: {error}"))?;

        assert!(
            !evidence_text.contains("AgentProfileAlpha"),
            "agent profile evidence must not leak raw alpha agent name"
        );
        assert!(
            !evidence_text.contains("AgentProfileBeta"),
            "agent profile evidence must not leak raw beta agent name"
        );

        let evidence: Value = serde_json::from_str(&evidence_text)
            .map_err(|error| format!("agent profile evidence must parse: {error}"))?;
        assert_eq!(
            evidence.pointer("/schema"),
            Some(&json!("ee.support_bundle.agent_profile_evidence.v1"))
        );
        assert_eq!(evidence.pointer("/status"), Some(&json!("available")));
        assert_eq!(
            evidence.pointer("/redactionStatus"),
            Some(&json!("agent_names_hashed_counts_only_no_raw_agent_names"))
        );
        assert_eq!(
            evidence.pointer("/database/profileRowCount"),
            Some(&json!(2))
        );
        assert_eq!(
            evidence.pointer("/database/summarizedAgentCount"),
            Some(&json!(2))
        );
        let agents = evidence
            .pointer("/agents")
            .and_then(Value::as_array)
            .ok_or_else(|| "agent profile evidence must include agents array".to_owned())?;
        assert_eq!(agents.len(), 2);
        for agent in agents {
            assert_eq!(
                agent.pointer("/agentNameIncluded"),
                Some(&json!(false)),
                "agent evidence must explicitly mark raw names as omitted"
            );
            assert!(
                agent
                    .pointer("/agentNameHash")
                    .and_then(Value::as_str)
                    .is_some_and(|hash| hash.starts_with("blake3:")),
                "agent evidence must expose a stable hash instead of raw name"
            );
            assert!(agent.pointer("/helpfulCount").is_some());
            assert!(agent.pointer("/harmfulCount").is_some());
        }

        Ok(())
    }

    #[test]
    fn scale_fixture_manifest_is_wrapped_with_support_bundle_schema() -> TestResult {
        let wrapped = scale_fixture_manifest_json();
        let value: Value = serde_json::from_str(&wrapped)
            .map_err(|error| format!("fixture manifest wrapper must parse: {error}"))?;
        assert_eq!(
            value.pointer("/schema"),
            Some(&json!("ee.support_bundle.scale_fixture_manifest.v1"))
        );
        assert_eq!(
            value.pointer("/manifest/schema"),
            Some(&json!("ee.swarm_scale.workloads.v1"))
        );
        Ok(())
    }

    #[test]
    fn cache_reports_collect_live_workspace_hotsets() -> TestResult {
        let root = unique_test_path("cache-hotsets");
        let workspace = root.join("workspace");
        fs::create_dir_all(workspace.join(".ee"))
            .map_err(|error| format!("failed to create workspace metadata dir: {error}"))?;
        let workspace = workspace
            .canonicalize()
            .map_err(|error| format!("failed to canonicalize workspace: {error}"))?;

        let database_path = workspace.join(".ee").join("ee.db");
        let connection = DbConnection::open_file(&database_path)
            .map_err(|error| format!("failed to open test db: {error}"))?;
        connection
            .migrate()
            .map_err(|error| format!("failed to migrate test db: {error}"))?;

        let workspace_id = "wsp_01234567890123456789012345";
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("cache-hotsets".to_owned()),
                },
            )
            .map_err(|error| format!("failed to insert workspace: {error}"))?;

        for (memory_id, content, importance) in [
            (
                "mem_00000000000000000000cygg01",
                "Run cargo test before closing cache support bundle changes.",
                0.92,
            ),
            (
                "mem_00000000000000000000cygg02",
                "Record cache state from the workspace database.",
                0.81,
            ),
        ] {
            connection
                .insert_memory(
                    memory_id,
                    &crate::db::CreateMemoryInput {
                        workspace_id: workspace_id.to_owned(),
                        level: "procedural".to_owned(),
                        kind: "rule".to_owned(),
                        content: content.to_owned(),
                        workflow_id: None,
                        confidence: 0.9,
                        utility: 0.8,
                        importance,
                        provenance_uri: Some("test://support-bundle/cache-hotsets".to_owned()),
                        trust_class: "human_explicit".to_owned(),
                        trust_subclass: Some("test".to_owned()),
                        tags: vec!["cache".to_owned()],
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| format!("failed to insert memory {memory_id}: {error}"))?;
        }

        let pack_id = "pack_000000000000000000000cygg1";
        let pack_items = vec![
            crate::db::CreatePackItemInput {
                pack_id: pack_id.to_owned(),
                memory_id: "mem_00000000000000000000cygg01".to_owned(),
                rank: 1,
                section: "procedural_rules".to_owned(),
                estimated_tokens: 34,
                relevance: 0.91,
                utility: 0.82,
                why: "exercise procedural section hotset".to_owned(),
                diversity_key: None,
                provenance_json: r#"{"schema":"ee.test.provenance.v1"}"#.to_owned(),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: Some("test".to_owned()),
            },
            crate::db::CreatePackItemInput {
                pack_id: pack_id.to_owned(),
                memory_id: "mem_00000000000000000000cygg02".to_owned(),
                rank: 2,
                section: "decisions".to_owned(),
                estimated_tokens: 21,
                relevance: 0.77,
                utility: 0.74,
                why: "exercise decision section hotset".to_owned(),
                diversity_key: None,
                provenance_json: r#"{"schema":"ee.test.provenance.v1"}"#.to_owned(),
                trust_class: "human_explicit".to_owned(),
                trust_subclass: Some("test".to_owned()),
            },
        ];
        connection
            .insert_pack_record(
                pack_id,
                &crate::db::CreatePackRecordInput {
                    workspace_id: workspace_id.to_owned(),
                    query: "cache support bundle hotsets".to_owned(),
                    profile: "balanced".to_owned(),
                    max_tokens: 4000,
                    used_tokens: 55,
                    item_count: 2,
                    omitted_count: 0,
                    pack_hash: "blake3:cygg-cache-pack".to_owned(),
                    degraded_json: None,
                    created_by: Some("test".to_owned()),
                },
                &pack_items,
                &[],
            )
            .map_err(|error| format!("failed to insert pack record: {error}"))?;
        connection
            .close()
            .map_err(|error| format!("failed to close test db: {error}"))?;

        let search_cache_dir = workspace.join(".ee").join("cache").join("search");
        let pack_cache_dir = workspace.join(".ee").join("cache").join("pack");
        fs::create_dir_all(&search_cache_dir)
            .map_err(|error| format!("failed to create search cache dir: {error}"))?;
        fs::create_dir_all(&pack_cache_dir)
            .map_err(|error| format!("failed to create pack cache dir: {error}"))?;
        fs::write(search_cache_dir.join("hotset.bin"), b"search-cache-index")
            .map_err(|error| format!("failed to write search cache fixture: {error}"))?;
        fs::write(pack_cache_dir.join("hotset.bin"), b"pack-cache-index")
            .map_err(|error| format!("failed to write pack cache fixture: {error}"))?;

        let rendered = cache_reports_json(&workspace);
        let value: Value = serde_json::from_str(&rendered)
            .map_err(|error| format!("cache report must parse: {error}"))?;

        assert_eq!(
            value.pointer("/source"),
            Some(&json!("workspace_database_and_cache_state"))
        );
        assert_eq!(value.pointer("/database/present"), Some(&json!(true)));
        assert_eq!(value.pointer("/database/readable"), Some(&json!(true)));
        assert_eq!(
            value.pointer("/database/workspaceRowPresent"),
            Some(&json!(true))
        );
        assert_eq!(value.pointer("/database/memoryCount"), Some(&json!(2)));
        assert_eq!(value.pointer("/database/packRecordCount"), Some(&json!(1)));
        assert_eq!(value.pointer("/database/packItemCount"), Some(&json!(2)));
        assert_eq!(value.pointer("/cacheState/search/entries"), Some(&json!(1)));
        assert_eq!(value.pointer("/cacheState/pack/entries"), Some(&json!(1)));
        assert_eq!(
            value.pointer("/reports/search/requestedEntries"),
            Some(&json!(3))
        );
        assert_eq!(
            value.pointer("/reports/pack/requestedEntries"),
            Some(&json!(3))
        );

        let search_admitted = value
            .pointer("/reports/search/admitted")
            .and_then(Value::as_array)
            .ok_or_else(|| "search admitted entries must be an array".to_owned())?;
        assert!(
            search_admitted
                .iter()
                .any(|entry| entry.pointer("/kind") == Some(&json!("memory"))),
            "search hotset must include memory entries from persisted memories"
        );
        assert!(
            search_admitted
                .iter()
                .any(|entry| entry.pointer("/kind") == Some(&json!("query_shape"))),
            "search hotset must include query-shape entries from pack records"
        );

        let pack_admitted = value
            .pointer("/reports/pack/admitted")
            .and_then(Value::as_array)
            .ok_or_else(|| "pack admitted entries must be an array".to_owned())?;
        assert!(
            pack_admitted
                .iter()
                .any(|entry| entry.pointer("/kind") == Some(&json!("pack_section"))),
            "pack hotset must include section entries from pack items"
        );
        assert!(
            pack_admitted
                .iter()
                .any(|entry| entry.pointer("/kind") == Some(&json!("selection_audit"))),
            "pack hotset must include selection audit entries from pack records"
        );

        Ok(())
    }

    #[test]
    fn cache_hotsets_ignore_denied_mesh_links_for_graph_entries() -> TestResult {
        let root = unique_test_path("cache-hotset-mesh-links");
        let workspace = root.join("workspace");
        fs::create_dir_all(workspace.join(".ee"))
            .map_err(|error| format!("failed to create workspace metadata dir: {error}"))?;
        let workspace = workspace
            .canonicalize()
            .map_err(|error| format!("failed to canonicalize workspace: {error}"))?;

        let database_path = workspace.join(".ee").join("ee.db");
        let connection = DbConnection::open_file(&database_path)
            .map_err(|error| format!("failed to open test db: {error}"))?;
        connection
            .migrate()
            .map_err(|error| format!("failed to migrate test db: {error}"))?;

        let workspace_id = "wsp_0123456789012345678901mesh";
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("cache-hotset-mesh-links".to_owned()),
                },
            )
            .map_err(|error| format!("failed to insert workspace: {error}"))?;

        for memory_id in [
            "mem_000000000000000000mesh0001",
            "mem_000000000000000000mesh0002",
            "mem_000000000000000000mesh0003",
        ] {
            connection
                .insert_memory(
                    memory_id,
                    &crate::db::CreateMemoryInput {
                        workspace_id: workspace_id.to_owned(),
                        level: "working".to_owned(),
                        kind: "fact".to_owned(),
                        content: format!("Mesh hotset fixture memory {memory_id}."),
                        workflow_id: None,
                        confidence: 0.8,
                        utility: 0.7,
                        importance: 0.6,
                        provenance_uri: Some("test://support-bundle/mesh-hotset".to_owned()),
                        trust_class: "human_explicit".to_owned(),
                        trust_subclass: Some("test".to_owned()),
                        tags: Vec::new(),
                        valid_from: None,
                        valid_to: None,
                    },
                )
                .map_err(|error| format!("failed to insert memory {memory_id}: {error}"))?;
        }

        insert_support_bundle_test_link(
            &connection,
            "link_000000000000000000mesh0001",
            "mem_000000000000000000mesh0001",
            "mem_000000000000000000mesh0002",
            Some(support_bundle_mesh_link_metadata("allow", true)),
        )?;
        insert_support_bundle_test_link(
            &connection,
            "link_000000000000000000mesh0002",
            "mem_000000000000000000mesh0003",
            "mem_000000000000000000mesh0001",
            Some(support_bundle_mesh_link_metadata("deny", true)),
        )?;
        insert_support_bundle_test_link(
            &connection,
            "link_000000000000000000mesh0003",
            "mem_000000000000000000mesh0002",
            "mem_000000000000000000mesh0003",
            Some(support_bundle_mesh_link_metadata("allow", false)),
        )?;

        let entries = super::collect_search_cache_hotset_entries(&connection, workspace_id, 42);
        let graph_entries = entries
            .iter()
            .filter(|entry| entry.kind == crate::search::SearchHotsetEntryKind::GraphNeighborhood)
            .collect::<Vec<_>>();

        assert_eq!(
            graph_entries.len(),
            1,
            "only the allowed complete mesh link should seed a graph hotset entry"
        );
        assert_eq!(
            graph_entries[0].hit_count, 1,
            "denied and incomplete mesh links must not increase hotset hits"
        );

        connection
            .close()
            .map_err(|error| format!("failed to close test db: {error}"))
    }

    fn insert_support_bundle_test_link(
        connection: &DbConnection,
        id: &str,
        src_memory_id: &str,
        dst_memory_id: &str,
        metadata_json: Option<String>,
    ) -> TestResult {
        connection
            .insert_memory_link(
                id,
                &crate::db::CreateMemoryLinkInput {
                    src_memory_id: src_memory_id.to_owned(),
                    dst_memory_id: dst_memory_id.to_owned(),
                    relation: crate::db::MemoryLinkRelation::Supports,
                    weight: 0.9,
                    confidence: 0.9,
                    directed: true,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: crate::db::MemoryLinkSource::Agent,
                    created_by: Some("support-bundle-mesh-test".to_owned()),
                    metadata_json,
                },
            )
            .map_err(|error| format!("failed to insert link {id}: {error}"))
    }

    fn support_bundle_mesh_link_metadata(workspace_scope_decision: &str, complete: bool) -> String {
        let mut metadata = json!({
            "mesh": {
                "workspaceScopeDecision": workspace_scope_decision,
                "cachedMaterialId": "mesh_support_bundle_link",
                "originWorkspaceId": "wsp_remote_private",
                "originWorkspaceLabel": "/Users/alice/private/repo",
                "producerPeerId": "peer_builder_one",
                "producerPeerLabel": "/Users/alice/private/peer-agent",
                "materialLane": "graphSignal",
                "importDecisionId": "mesh_support_bundle_decision",
                "trustLane": "mesh_metadata",
                "redactionPosture": "standard"
            }
        });
        if !complete
            && let Some(object) = metadata
                .get_mut("mesh")
                .and_then(serde_json::Value::as_object_mut)
        {
            object.remove("trustLane");
        }
        metadata.to_string()
    }

    #[test]
    fn create_bundle_includes_redaction_safe_pack_replay_summary() -> TestResult {
        let root = unique_test_path("pack-replay-summary");
        let workspace = root.join("workspace");
        fs::create_dir_all(workspace.join(".ee"))
            .map_err(|error| format!("failed to create workspace metadata dir: {error}"))?;
        let workspace = workspace
            .canonicalize()
            .map_err(|error| format!("failed to canonicalize workspace: {error}"))?;

        let database_path = workspace.join(".ee").join("ee.db");
        let connection = DbConnection::open_file(&database_path)
            .map_err(|error| format!("failed to open test db: {error}"))?;
        connection
            .migrate()
            .map_err(|error| format!("failed to migrate test db: {error}"))?;

        let workspace_id = "wsp_01234567890123456789012346";
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("pack-replay-summary".to_owned()),
                },
            )
            .map_err(|error| format!("failed to insert workspace: {error}"))?;

        let memory_id = "mem_00000000000000000000sprs01";
        connection
            .insert_memory(
                memory_id,
                &crate::db::CreateMemoryInput {
                    workspace_id: workspace_id.to_owned(),
                    level: "procedural".to_owned(),
                    kind: "rule".to_owned(),
                    content: "Run support bundle replay checks before closeout.".to_owned(),
                    workflow_id: None,
                    confidence: 0.9,
                    utility: 0.8,
                    importance: 0.7,
                    provenance_uri: Some("test://support-bundle/pack-replay".to_owned()),
                    trust_class: "human_explicit".to_owned(),
                    trust_subclass: Some("test".to_owned()),
                    tags: vec!["pack".to_owned(), "replay".to_owned()],
                    valid_from: None,
                    valid_to: None,
                },
            )
            .map_err(|error| format!("failed to insert memory: {error}"))?;

        let raw_secret = format!("{}_{}_{}", "api", "key=sk", "test_123");
        let degraded_json = json!([
            {
                "code": "context_evidence_freshness_changed_source",
                "severity": "medium",
                "message": "Source evidence changed after pack creation.",
                "repair": "ee why <memory-id> --json"
            }
        ])
        .to_string();
        let pack_id = "pack_00000000000000000000sprs01";
        let pack_items = vec![crate::db::CreatePackItemInput {
            pack_id: pack_id.to_owned(),
            memory_id: memory_id.to_owned(),
            rank: 1,
            section: "procedural_rules".to_owned(),
            estimated_tokens: 17,
            relevance: 0.91,
            utility: 0.83,
            why: format!("selected by replay query with {raw_secret}"),
            diversity_key: Some("procedural:rule:support-bundle".to_owned()),
            provenance_json: json!({
                "schema": "ee.test.provenance.v1",
                "source": format!("redacted provenance {raw_secret}")
            })
            .to_string(),
            trust_class: "human_explicit".to_owned(),
            trust_subclass: Some("test".to_owned()),
        }];
        connection
            .insert_pack_record(
                pack_id,
                &crate::db::CreatePackRecordInput {
                    workspace_id: workspace_id.to_owned(),
                    query: format!("support bundle replay {raw_secret}"),
                    profile: "compact".to_owned(),
                    max_tokens: 4000,
                    used_tokens: 17,
                    item_count: 1,
                    omitted_count: 0,
                    pack_hash: "blake3:support-bundle-pack-replay".to_owned(),
                    degraded_json: Some(degraded_json),
                    created_by: Some("ee context".to_owned()),
                },
                &pack_items,
                &[],
            )
            .map_err(|error| format!("failed to insert pack record: {error}"))?;
        connection
            .close()
            .map_err(|error| format!("failed to close test db: {error}"))?;

        let output_dir = root.join("out");
        fs::create_dir_all(&output_dir)
            .map_err(|error| format!("failed to create output dir: {error}"))?;
        let report = create_bundle(&BundleOptions {
            workspace,
            output_dir: Some(output_dir),
            dry_run: false,
            redacted: true,
            redaction_level: RedactionLevel::Paranoid,
            include_raw: false,
            audit_limit: 5,
        })
        .map_err(|error| error.message())?;

        assert!(
            report
                .files_collected
                .contains(&PACK_REPLAY_SUMMARY_FILE.to_owned()),
            "support bundle must include pack replay summary"
        );
        assert!(
            report
                .files_collected
                .contains(&SWARM_BRIEF_SUMMARY_FILE.to_owned()),
            "support bundle must include swarm brief summary"
        );
        let bundle_dir = report
            .output_path
            .clone()
            .ok_or_else(|| "created bundle must report output path".to_owned())?;
        let summary_text = fs::read_to_string(bundle_dir.join(PACK_REPLAY_SUMMARY_FILE))
            .map_err(|error| format!("failed to read pack replay summary: {error}"))?;
        assert!(
            !summary_text.contains(&raw_secret),
            "pack replay summary must not leak raw secret-like query or why content"
        );
        let summary: Value = serde_json::from_str(&summary_text)
            .map_err(|error| format!("pack replay summary must parse: {error}"))?;

        assert_eq!(
            summary.pointer("/schema"),
            Some(&json!("ee.support_bundle.pack_replay_summary.v1"))
        );
        assert_eq!(summary.pointer("/status"), Some(&json!("available")));
        assert_eq!(
            summary.pointer("/redactionStatus"),
            Some(&json!(
                "ids_hashes_counts_codes_only_no_query_text_no_memory_content"
            ))
        );
        assert_eq!(
            summary.pointer("/database/packRecordCount"),
            Some(&json!(1))
        );
        assert_eq!(
            summary.pointer("/database/ledgerAvailableCount"),
            Some(&json!(1))
        );
        assert_eq!(
            summary.pointer("/packs/0/queryTextIncluded"),
            Some(&json!(false))
        );
        let query_hash = summary
            .pointer("/packs/0/queryHash")
            .and_then(Value::as_str)
            .ok_or_else(|| "pack summary must include a query hash".to_owned())?;
        assert!(
            query_hash.starts_with("blake3:"),
            "query hash must use blake3 prefix, got {query_hash}"
        );
        assert_eq!(
            summary.pointer("/packs/0/ledger/status"),
            Some(&json!("available"))
        );
        assert_eq!(
            summary.pointer("/packs/0/ledger/freshnessStates/unavailable"),
            Some(&json!(1))
        );
        assert!(
            summary
                .pointer("/packs/0/ledger/redactionClasses")
                .and_then(Value::as_array)
                .is_some_and(|classes| !classes.is_empty()),
            "pack replay summary must expose redaction classes without raw content"
        );
        assert!(
            summary
                .pointer("/packs/0/ledger/degradationCodes")
                .and_then(Value::as_array)
                .is_some_and(|codes| codes.iter().any(|code| {
                    code.as_str() == Some("context_evidence_freshness_changed_source")
                })),
            "pack replay summary must expose freshness degradation codes"
        );

        let swarm_summary_text = fs::read_to_string(bundle_dir.join(SWARM_BRIEF_SUMMARY_FILE))
            .map_err(|error| format!("failed to read swarm brief summary: {error}"))?;
        let swarm_summary: Value = serde_json::from_str(&swarm_summary_text)
            .map_err(|error| format!("swarm brief summary must parse: {error}"))?;
        assert_eq!(
            swarm_summary.pointer("/schema"),
            Some(&json!(
                super::super::swarm_brief::SWARM_BRIEF_SUMMARY_SCHEMA_V1
            ))
        );
        assert_eq!(
            swarm_summary.pointer("/redaction/rawMailBodiesIncluded"),
            Some(&json!(false))
        );
        assert_eq!(
            swarm_summary.pointer("/redaction/rawQueryTextIncluded"),
            Some(&json!(false))
        );
        assert_eq!(
            swarm_summary.pointer("/redaction/rawProvenanceTextIncluded"),
            Some(&json!(false))
        );
        assert_eq!(
            swarm_summary.pointer("/redaction/fullFileListingsIncluded"),
            Some(&json!(false))
        );
        assert!(
            swarm_summary
                .pointer("/reportHash")
                .and_then(Value::as_str)
                .is_some_and(|hash| hash.starts_with("blake3:")),
            "swarm brief summary must hash the underlying brief"
        );
        assert!(
            swarm_summary.pointer("/fileSurfaceRiskSummary").is_some(),
            "swarm brief summary must include the compact ownership-risk section"
        );

        Ok(())
    }

    #[test]
    fn create_bundle_collects_persisted_performance_explain_samples() -> TestResult {
        let root = unique_test_path("performance-samples");
        let workspace = root.join("workspace");
        let performance_dir = workspace.join(".ee").join(PERFORMANCE_EXPLAIN_SAMPLE_DIR);
        fs::create_dir_all(&performance_dir)
            .map_err(|error| format!("failed to create performance sample dir: {error}"))?;

        let persisted_sample = json!({
            "schema": crate::core::search::PERFORMANCE_EXPLAIN_SCHEMA_V1,
            "success": true,
            "data": {
                "command": "search",
                "query": {
                    "textIncluded": false,
                    "lengthBytes": 31,
                    "fingerprint": "blake3:samplequeryhash",
                    "meshProvenance": {
                        "originWorkspaceLabel": "/Users/alice/private/repo",
                        "producerPeerLabel": "/Users/alice/private/peer-agent"
                    }
                },
                "queryPlan": {
                    "retrievalMode": "thorough",
                    "requestedLimit": 17,
                    "candidateBudget": 888,
                    "usesEmbeddings": true,
                    "scoreExplanationsRequested": false
                },
                "dbReads": {
                    "indexStatusChecks": 2,
                    "memoryReads": 3
                },
                "search": {
                    "returnedHits": 7,
                    "elapsed": {
                        "elapsedMs": 12.5,
                        "elapsedMsBucket": "lt_25ms",
                        "nondeterministic": true
                    }
                },
                "pack": {
                    "selectedCount": 4,
                    "tokenBudget": {
                        "limit": 4000,
                        "used": 1234
                    }
                },
                "timings": [
                    {
                        "name": "fixture_run",
                        "elapsedMs": 5.75,
                        "elapsedMsBucket": "lt_10ms",
                        "nondeterministic": true
                    }
                ],
                "fallbacks": [
                    {
                        "code": "search_index_stale"
                    }
                ],
                "redaction": {
                    "memoryContentIncluded": false,
                    "queryTextIncluded": false
                }
            }
        });
        fs::write(
            performance_dir.join("search-release.json"),
            serde_json::to_string_pretty(&persisted_sample)
                .map_err(|error| format!("failed to serialize persisted sample: {error}"))?,
        )
        .map_err(|error| format!("failed to write persisted sample: {error}"))?;

        let output_dir = root.join("out");
        fs::create_dir_all(&output_dir)
            .map_err(|error| format!("failed to create output dir: {error}"))?;

        let report = create_bundle(&BundleOptions {
            workspace,
            output_dir: Some(output_dir),
            dry_run: false,
            redacted: true,
            redaction_level: RedactionLevel::Paranoid,
            include_raw: false,
            audit_limit: 5,
        })
        .map_err(|error| error.message())?;

        let bundle_dir = report
            .output_path
            .clone()
            .ok_or_else(|| "created bundle must report output path".to_owned())?;
        let samples_json = fs::read_to_string(bundle_dir.join(PERFORMANCE_EXPLAIN_SAMPLES_FILE))
            .map_err(|error| format!("failed to read performance samples: {error}"))?;
        assert!(
            !samples_json.contains("/Users/alice/private/repo")
                && !samples_json.contains("/Users/alice/private/peer-agent"),
            "performance samples must not leak raw mesh workspace or producer peer paths"
        );
        let samples: Value = serde_json::from_str(&samples_json)
            .map_err(|error| format!("performance samples must parse: {error}"))?;

        assert_eq!(samples.pointer("/sampleCount"), Some(&json!(1)));
        assert_eq!(
            samples.pointer("/status"),
            Some(&json!("persisted_samples_collected"))
        );
        assert_eq!(
            samples.pointer("/samples/0/command"),
            Some(&json!("search"))
        );
        assert_eq!(
            samples.pointer("/samples/0/queryPlan/requestedLimit"),
            Some(&json!(17))
        );
        assert_eq!(
            samples.pointer("/samples/0/queryPlan/candidateBudget"),
            Some(&json!(888))
        );
        assert_eq!(
            samples.pointer("/samples/0/query/meshProvenance/originWorkspaceLabel"),
            Some(&json!("[REDACTED:path]"))
        );
        assert_eq!(
            samples.pointer("/samples/0/query/meshProvenance/producerPeerLabel"),
            Some(&json!("[REDACTED:path]"))
        );
        assert_eq!(samples.pointer("/samples/0/redacted"), Some(&json!(true)));
        assert!(
            samples
                .pointer("/samples/0/redactionReasons")
                .and_then(Value::as_array)
                .is_some_and(|reasons| {
                    reasons
                        .iter()
                        .any(|reason| reason.as_str() == Some("path_like_segment"))
                }),
            "performance sample must report path-like redaction"
        );
        assert_eq!(
            samples.pointer("/samples/0/measurements/searchElapsed/elapsedMs"),
            Some(&json!(12.5))
        );
        assert_eq!(
            samples.pointer("/samples/0/measurements/returnedHits"),
            Some(&json!(7))
        );
        assert_eq!(
            samples.pointer("/samples/0/measurements/fallbackCount"),
            Some(&json!(1))
        );
        assert_eq!(
            samples.pointer("/samples/0/measurements/tokenBudget/used"),
            Some(&json!(1234))
        );
        assert_eq!(
            samples.pointer("/samples/0/path"),
            Some(&json!(".ee/performance-explain/search-release.json"))
        );

        Ok(())
    }

    #[test]
    fn create_bundle_collects_scale_artifacts_and_detects_tamper() -> TestResult {
        let root = unique_test_path("scale-artifacts");
        let workspace = root.join("workspace");
        let report_dir = workspace.join(".ee").join("swarm-contention");
        fs::create_dir_all(&report_dir)
            .map_err(|error| format!("failed to create report dir: {error}"))?;

        let raw_secret = format!("{}_{}_{}", "api", "key=sk", "test_123");
        let swarm_report = json!({
            "schema": "ee.swarm_contention.report.v1",
            "scenario": "mixed_read_write_contention",
            "processCount": 5,
            "successCount": 4,
            "failureCount": 1,
            "totalDurationMs": 42,
            "dbIntegrityOk": true,
            "determinismOk": true,
            "degradations": [format!("worker stderr included {raw_secret}")],
        });
        fs::write(report_dir.join("report.json"), swarm_report.to_string())
            .map_err(|error| format!("failed to write swarm report: {error}"))?;

        let output_dir = root.join("out");
        fs::create_dir_all(&output_dir)
            .map_err(|error| format!("failed to create output dir: {error}"))?;

        let report = create_bundle(&BundleOptions {
            workspace,
            output_dir: Some(output_dir),
            dry_run: false,
            redacted: true,
            redaction_level: RedactionLevel::Paranoid,
            include_raw: false,
            audit_limit: 5,
        })
        .map_err(|error| error.message())?;

        for required in [
            PROFILE_EVIDENCE_FILE,
            SCALE_BENCHMARK_SUMMARY_FILE,
            SCALE_FIXTURE_MANIFEST_FILE,
            CACHE_REPORTS_FILE,
            WRITE_QUEUE_REPORT_FILE,
            PERFORMANCE_EXPLAIN_SAMPLES_FILE,
            SINGLEFLIGHT_POSTURE_FILE,
            TRIAGE_SUMMARY_FILE,
        ] {
            assert!(
                report.files_collected.contains(&required.to_owned()),
                "created support bundle must include {required}"
            );
        }

        let bundle_dir = report
            .output_path
            .clone()
            .ok_or_else(|| "created bundle must report output path".to_owned())?;
        let benchmark_summary =
            fs::read_to_string(bundle_dir.join(SCALE_BENCHMARK_SUMMARY_FILE))
                .map_err(|error| format!("failed to read benchmark summary: {error}"))?;
        assert!(
            benchmark_summary.contains("mixed_read_write_contention"),
            "benchmark summary must include the discovered swarm scenario"
        );
        assert!(
            !benchmark_summary.contains(&raw_secret),
            "benchmark summary must not leak secret-like report content"
        );

        let singleflight_text = fs::read_to_string(bundle_dir.join(SINGLEFLIGHT_POSTURE_FILE))
            .map_err(|error| format!("failed to read single-flight posture: {error}"))?;
        let singleflight: Value = serde_json::from_str(&singleflight_text)
            .map_err(|error| format!("single-flight posture must parse: {error}"))?;
        assert_eq!(
            singleflight.pointer("/schema"),
            Some(&json!("ee.singleflight.posture.v1"))
        );
        assert!(
            singleflight.pointer("/surfaces").is_some(),
            "single-flight posture must include surface summaries"
        );
        assert!(
            !singleflight_text.contains("raw_query")
                && !singleflight_text.contains("memory_body")
                && !singleflight_text.contains(&raw_secret),
            "single-flight posture must remain redaction-safe"
        );

        let clean_inspect = inspect_bundle(&InspectOptions {
            bundle_path: bundle_dir.clone(),
            verify_hashes: true,
        })
        .map_err(|error| error.message())?;
        assert!(clean_inspect.valid);

        fs::write(bundle_dir.join(CACHE_REPORTS_FILE), "{}")
            .map_err(|error| format!("failed to tamper cache report: {error}"))?;
        let tampered_inspect = inspect_bundle(&InspectOptions {
            bundle_path: bundle_dir,
            verify_hashes: true,
        })
        .map_err(|error| error.message())?;
        assert!(!tampered_inspect.valid);
        assert!(
            tampered_inspect
                .hash_mismatches
                .contains(&CACHE_REPORTS_FILE.to_owned()),
            "tampered metric attachment must be reported as a hash mismatch"
        );

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn performance_sample_discovery_ignores_symlinked_json() -> TestResult {
        let root = unique_test_path("performance-sample-symlink");
        let workspace = root.join("workspace");
        let performance_dir = workspace.join(".ee").join(PERFORMANCE_EXPLAIN_SAMPLE_DIR);
        fs::create_dir_all(&performance_dir)
            .map_err(|error| format!("failed to create performance sample dir: {error}"))?;

        let target = root.join("outside-performance-sample.json");
        fs::write(
            &target,
            json!({
                "schema": crate::core::search::PERFORMANCE_EXPLAIN_SCHEMA_V1,
                "data": {
                    "command": "search",
                    "fallbacks": []
                }
            })
            .to_string(),
        )
        .map_err(|error| format!("failed to write symlink target: {error}"))?;
        let link = performance_dir.join("linked-sample.json");
        std::os::unix::fs::symlink(&target, &link)
            .map_err(|error| format!("failed to create sample symlink: {error}"))?;

        let samples = discover_performance_explain_samples(&workspace);
        assert!(
            samples.is_empty(),
            "performance sample discovery must not follow symlinked json files"
        );
        assert!(
            summarize_performance_explain_sample(&workspace, &link).is_none(),
            "direct performance sample summarization must reject symlinked json files"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn swarm_report_discovery_ignores_symlinked_json() -> TestResult {
        let root = unique_test_path("swarm-report-symlink");
        let workspace = root.join("workspace");
        let report_dir = workspace.join(".ee").join("swarm-contention");
        fs::create_dir_all(&report_dir)
            .map_err(|error| format!("failed to create swarm report dir: {error}"))?;

        let target = root.join("outside-swarm-report.json");
        fs::write(
            &target,
            json!({
                "schema": "ee.swarm_contention.report.v1",
                "scenario": "symlinked_report",
                "processCount": 1,
                "successCount": 1,
                "failureCount": 0,
                "dbIntegrityOk": true,
                "determinismOk": true
            })
            .to_string(),
        )
        .map_err(|error| format!("failed to write symlink target: {error}"))?;
        let link = report_dir.join("linked-report.json");
        std::os::unix::fs::symlink(&target, &link)
            .map_err(|error| format!("failed to create report symlink: {error}"))?;

        let reports = discover_swarm_report_summaries(&workspace);
        assert!(
            reports.is_empty(),
            "swarm report discovery must not follow symlinked json files"
        );
        assert!(
            summarize_swarm_report(&workspace, &link).is_none(),
            "direct swarm report summarization must reject symlinked json files"
        );
        Ok(())
    }

    fn unique_test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ee-support-bundle-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ))
    }
}
