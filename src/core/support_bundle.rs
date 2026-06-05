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
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use blake3::Hasher;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlmodel_core::{Row as SqlRow, Value as SqlValue};

use crate::cache::CacheBudget;
use crate::core::qos::{
    QosBackgroundThrottleDecision, QosBackgroundThrottleInput, QosLane, QosLaneSummary,
    QosThrottleCheckpoint, decide_background_throttle,
};
use crate::db::{DbConnection, StoredAuditEntry, audit_actions};
use crate::models::regression_causality::{
    REGRESSION_CAUSALITY_SCHEMA_V1, RegressionEvidenceInput, RegressionEvidenceKind,
    normalize_regression_evidence_inputs, rank_regression_cause_hypotheses,
};
use crate::models::{
    ArtifactDegradationSeverity, ArtifactKind, ArtifactSummary, DomainError, MetricValue,
    ProfileReference, ProvenanceEntry, RedactionLevel, RedactionPosture, SummaryDegradation,
    SummaryDegradationCode, VERIFICATION_EVIDENCE_SCHEMA_V1, VerificationEvidenceRecord,
    VerificationStatus, verification_evidence_beads_summary,
};
use crate::output;
use crate::pack::{
    PackCacheGovernor, PackHotset, PackHotsetEntry, PackHotsetEntryKind, PackSection,
    prewarm_pack_hotset,
};
use crate::policy::{redact_secret_like_content, redaction_placeholder};
use crate::search::{SearchCacheGovernor, SearchHotset, SearchHotsetEntry, prewarm_search_hotset};

use super::derived_asset::gather_default_derived_asset_store_summary;
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
const VERIFICATION_EVIDENCE_SUMMARY_FILE: &str = "verification_evidence_summary.json";
const MEMORY_DRIFT_SUMMARY_FILE: &str = "memory_drift_summary.json";
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

/// Hard upper bound on the byte length of an individual support-bundle
/// JSON sample file (per-sample, not per-summary). Applies to the
/// per-file reads in `summarize_performance_explain_sample` and
/// `summarize_swarm_report`. Each underlying file is a single
/// `ee.search.performance_explain.v1` (~few KB typical) or
/// `swarm-contention` report (~few KB typical) JSON document.
///
/// Without this cap, both call sites' `fs::read_to_string(path).ok()?`
/// shape would pre-size its destination `String` from the file's
/// metadata length and allocate the entire body before
/// `serde_json::from_str` could reject it. A peer-planted or
/// runaway-writer multi-GiB file at
/// `<workspace>/.ee/performance-explain/<name>.json` or
/// `<workspace>/.ee/swarm-contention/<name>.json` would OOM every
/// `ee support bundle` invocation. The count-cap
/// (`MAX_PERFORMANCE_EXPLAIN_SAMPLES`, `paths.truncate(16)` for swarm
/// reports) bounds the number of files; this cap bounds the per-file
/// allocation. Parallel to `COORDINATION_FALLBACK_LEDGER_MAX_BYTES`
/// (same file, sibling sample reader) and the convergence-pass shape
/// at `EVALUATION_SNAPSHOT_MAX_BYTES` (`src/science/mod.rs`).
const MAX_SUPPORT_BUNDLE_SAMPLE_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Hard upper bound on support-bundle members read during inspection.
///
/// `ee support bundle inspect` reads the manifest plus manifest-listed
/// files from an operator-provided bundle path. Without a cap, a corrupt
/// or hostile bundle can point at a multi-GB regular file and force an
/// unbounded `String` allocation before hash/size validation can reject it.
/// 16 MiB is generous for the JSON diagnostics in current bundles while
/// keeping inspection bounded.
const MAX_SUPPORT_BUNDLE_INSPECT_FILE_BYTES: u64 = 16 * 1024 * 1024;
const PACK_REPLAY_SUMMARY_FILE: &str = "pack_replay_summary.json";
const MAX_PACK_REPLAY_SUMMARY_RECORDS: usize = 16;
const SWARM_REPLAY_SUMMARY_FILE: &str = "swarm_replay_summary.json";
const SWARM_BRIEF_SUMMARY_FILE: &str = "swarm_brief_summary.json";
const SWARM_INCIDENT_SUMMARY_FILE: &str = "swarm_incident_summary.json";
const COORDINATION_FALLBACK_SUMMARY_FILE: &str = "coordination_fallback_summary.json";
const COORDINATION_FALLBACK_LEDGER_FILE: &str = "coordination-fallback-evidence.jsonl";
const MAX_COORDINATION_FALLBACK_SUMMARY_RECORDS: usize = 16;
/// Hard upper bound on the byte length of the coordination-fallback ledger
/// read into the support bundle summary. The ledger is an append-only
/// workspace-local JSONL file at `.ee/coordination-fallback-evidence.jsonl`
/// that grows on every `ee coordination evidence ingest`. The previous
/// `fs::read_to_string(&ledger_path)` shape pre-sized its buffer from the
/// file's metadata length, so a peer-planted multi-GB ledger would OOM the
/// support-bundle hot path. 16 MiB matches the parallel cap in
/// `src/core/why.rs::COORDINATION_FALLBACK_LEDGER_MAX_BYTES` (same file,
/// parallel reader); the support bundle only summarizes the first
/// `MAX_COORDINATION_FALLBACK_SUMMARY_RECORDS = 16` records anyway so the
/// cap is generous for legitimate ledgers and bounded against hostile growth.
const COORDINATION_FALLBACK_LEDGER_MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_VERIFICATION_EVIDENCE_SUMMARY_RECORDS: usize = 16;
const SINGLEFLIGHT_POSTURE_FILE: &str = "singleflight_posture.json";
const QOS_LANE_SUMMARY_FILE: &str = "qos_lane_summary.json";
const TRIAGE_SUMMARY_FILE: &str = "scale_triage_summary.json";
const LOCAL_CARGO_TRIPWIRE_FILE: &str = "local_cargo_tripwire.json";
const REGRESSION_CAUSALITY_SUMMARY_FILE: &str = "regression_causality_summary.json";
const SUPPORT_BUNDLE_REGRESSION_CAUSALITY_SUMMARY_SCHEMA_V1: &str =
    "ee.support_bundle.regression_causality_summary.v1";
const ENVIRONMENT_ATTESTATION_SUMMARY_FILE: &str = "environment_attestation_summary.json";
pub(crate) const SUPPORT_BUNDLE_ENVIRONMENT_ATTESTATION_SUMMARY_SCHEMA_V1: &str =
    "ee.support_bundle.environment_attestation_summary.v1";
const SUPPORT_BUNDLE_REQUIRED_REMOTE_WRAPPER: &str = "scripts/rch_verify.sh -- <cargo command>";
const TAILSCALE_METADATA_FIELDS: &[&str] = &[
    "selfNodeKey",
    "selfTailscaleIp",
    "selfMagicDnsName",
    "tailnetId",
    "tailnetDisplayName",
    "selfAdvertisedTags",
    "peerNodeKey",
    "peerTailscaleIps",
    "peerMagicDnsName",
    "peerHostname",
    "peerAdvertisedTags",
    "binaryVersionRaw",
    "binaryAbsolutePath",
];
const PERF_COMPARE_BUNDLE_SECTIONS: [(&str, &str); 9] = [
    ("profile_evidence", PROFILE_EVIDENCE_FILE),
    ("benchmark_summary", SCALE_BENCHMARK_SUMMARY_FILE),
    ("fixture_manifest", SCALE_FIXTURE_MANIFEST_FILE),
    ("cache_reports", CACHE_REPORTS_FILE),
    ("write_queue_report", WRITE_QUEUE_REPORT_FILE),
    (
        "performance_explain_samples",
        PERFORMANCE_EXPLAIN_SAMPLES_FILE,
    ),
    ("swarm_replay_summary", SWARM_REPLAY_SUMMARY_FILE),
    ("swarm_brief_summary", SWARM_BRIEF_SUMMARY_FILE),
    ("swarm_contention_reports", SCALE_BENCHMARK_SUMMARY_FILE),
];
const SWARM_SCALE_WORKLOADS_MANIFEST: &str =
    include_str!("../../tests/fixtures/swarm_scale/workloads.json");
static SUPPORT_BUNDLE_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
    verification_evidence_summary_json: String,
    memory_drift_summary_json: String,
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
    swarm_replay_summary_json: String,
    swarm_brief_summary_json: String,
    swarm_incident_summary_json: String,
    coordination_fallback_summary_json: String,
    singleflight_posture_json: String,
    qos_lane_summary_json: String,
    triage_summary_json: String,
    local_cargo_tripwire_json: String,
    regression_causality_summary_json: String,
    environment_attestation_summary_json: String,
}

/// Plan what would be collected without actually creating the bundle.
pub fn plan_bundle(options: &BundleOptions) -> Result<BundleReport, DomainError> {
    let workspace_path = options
        .workspace
        .canonicalize()
        .unwrap_or_else(|_| options.workspace.clone());
    let redaction_level = options.effective_redaction_level();
    let workspace_redaction = redact_support_bundle_path(&workspace_path, redaction_level);

    let bundle_id = generate_bundle_id();
    let files_collected = planned_files();

    Ok(BundleReport {
        schema: SUPPORT_BUNDLE_SCHEMA_V1.to_owned(),
        bundle_id,
        files_collected,
        total_size_bytes: 0,
        redaction_applied: redaction_level.redacts_secrets(),
        redaction_level,
        redaction_summary: RedactionSummary {
            total_redactions: if workspace_redaction.redacted { 1 } else { 0 },
            reasons: workspace_redaction.redacted_reasons.clone(),
        },
        output_path: None,
        manifest_hash: None,
        dry_run: true,
        workspace_path: workspace_redaction.content,
    })
}

/// Create a support bundle with real diagnostic data.
pub fn create_bundle(options: &BundleOptions) -> Result<BundleReport, DomainError> {
    let output_dir = options
        .output_dir
        .clone()
        .ok_or_else(|| DomainError::Usage {
            message: "--out is required".to_string(),
            repair: Some("ee support bundle --help".to_string()),
        })?;

    let workspace_path = options
        .workspace
        .canonicalize()
        .unwrap_or_else(|_| options.workspace.clone());

    let bundle_id = generate_bundle_id();
    let bundle_dir = output_dir.join(format!("ee_support_{bundle_id}"));

    create_support_bundle_directory(&output_dir, &bundle_dir)?;

    let redaction_level = options.effective_redaction_level();
    let workspace_redaction = redact_support_bundle_path(&workspace_path, redaction_level);
    let diagnostics = collect_diagnostics(&workspace_path, options.audit_limit)?;

    let mut manifest_entries = Vec::new();
    let mut all_redaction_reasons: Vec<String> = Vec::new();
    let mut total_redactions = 0u32;
    let mut total_size = 0u64;

    if workspace_redaction.redacted {
        total_redactions += 1;
        for reason in &workspace_redaction.redacted_reasons {
            if !all_redaction_reasons.contains(reason) {
                all_redaction_reasons.push(reason.clone());
            }
        }
    }

    let files_to_write = [
        (STATUS_FILE, &diagnostics.status_json),
        (DOCTOR_FILE, &diagnostics.doctor_json),
        (AUDIT_FILE, &diagnostics.audit_json),
        (
            VERIFICATION_EVIDENCE_SUMMARY_FILE,
            &diagnostics.verification_evidence_summary_json,
        ),
        (
            MEMORY_DRIFT_SUMMARY_FILE,
            &diagnostics.memory_drift_summary_json,
        ),
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
            SWARM_REPLAY_SUMMARY_FILE,
            &diagnostics.swarm_replay_summary_json,
        ),
        (
            SWARM_BRIEF_SUMMARY_FILE,
            &diagnostics.swarm_brief_summary_json,
        ),
        (
            SWARM_INCIDENT_SUMMARY_FILE,
            &diagnostics.swarm_incident_summary_json,
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
        (
            LOCAL_CARGO_TRIPWIRE_FILE,
            &diagnostics.local_cargo_tripwire_json,
        ),
        (
            REGRESSION_CAUSALITY_SUMMARY_FILE,
            &diagnostics.regression_causality_summary_json,
        ),
        (
            ENVIRONMENT_ATTESTATION_SUMMARY_FILE,
            &diagnostics.environment_attestation_summary_json,
        ),
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
        workspace_path: workspace_redaction.content.clone(),
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
        workspace_path: workspace_redaction.content,
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

    let manifest_present = match fs::symlink_metadata(&manifest_path) {
        Ok(metadata) if metadata.file_type().is_file() => true,
        Ok(_) => {
            return Err(DomainError::Storage {
                message: format!(
                    "Support bundle manifest is not a regular file: {}.",
                    manifest_path.display()
                ),
                repair: Some("Regenerate the support bundle.".to_owned()),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(DomainError::Storage {
                message: format!(
                    "Failed to inspect support bundle manifest {}: {error}",
                    manifest_path.display()
                ),
                repair: Some("Check support bundle manifest permissions.".to_owned()),
            });
        }
    };

    let manifest: Option<BundleManifest> = if manifest_present {
        let content = read_regular_file_no_symlinks(&manifest_path).ok();
        content.and_then(|c| serde_json::from_str(&c).ok())
    } else {
        None
    };

    let mut files_found = Vec::new();
    let mut total_size = 0u64;
    let mut hash_mismatches = Vec::new();

    if let Some(ref m) = manifest {
        if m.schema != SUPPORT_BUNDLE_MANIFEST_SCHEMA_V1 {
            hash_mismatches.push(MANIFEST_FILE.to_owned());
        }
        let declared_total_size = m
            .files
            .iter()
            .fold(0u64, |total, entry| total.saturating_add(entry.size_bytes));
        if declared_total_size != m.total_size_bytes
            && !hash_mismatches
                .iter()
                .any(|mismatch| mismatch.as_str() == MANIFEST_FILE)
        {
            hash_mismatches.push(MANIFEST_FILE.to_owned());
        }
        for entry in &m.files {
            let Ok(file_path) = resolve_bundle_file_no_symlinks(bundle_dir, &entry.path) else {
                hash_mismatches.push(entry.path.clone());
                continue;
            };
            let Ok(content) = read_regular_file_no_symlinks(&file_path) else {
                hash_mismatches.push(entry.path.clone());
                continue;
            };

            files_found.push(entry.path.clone());
            let actual_size = content.len() as u64;
            total_size += actual_size;
            if actual_size != entry.size_bytes && !hash_mismatches.contains(&entry.path) {
                hash_mismatches.push(entry.path.clone());
            }
            if options.verify_hashes {
                let actual_hash = compute_hash(&content);
                if actual_hash != entry.content_hash && !hash_mismatches.contains(&entry.path) {
                    hash_mismatches.push(entry.path.clone());
                }
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

    let hash_verified = options.verify_hashes && manifest.is_some();
    let valid = manifest.is_some() && hash_mismatches.is_empty();

    Ok(InspectReport {
        schema: SUPPORT_BUNDLE_INSPECT_SCHEMA_V1.to_owned(),
        bundle_path: options.bundle_path.clone(),
        manifest,
        files_found,
        total_size_bytes: total_size,
        hash_verified,
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
            repair: Some("ee support bundle --help".to_owned()),
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
    if let Some(json) = read_bundle_json(bundle_dir, REGRESSION_CAUSALITY_SUMMARY_FILE) {
        summary.add_metric(
            "section.regression_causality_summary.present",
            MetricValue::measured(1.0, "bool"),
        );
        summary.add_provenance(ProvenanceEntry {
            field: "section.regression_causality_summary.present".to_owned(),
            source_path: REGRESSION_CAUSALITY_SUMMARY_FILE.to_owned(),
            source_line: None,
        });

        let top_hypothesis_count = json
            .get("topHypotheses")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        summary.add_metric(
            "regression_causality.top_hypothesis_count",
            MetricValue::measured(top_hypothesis_count as f64, "count"),
        );

        let normalized_row_count = json
            .get("normalizedRowCount")
            .and_then(Value::as_u64)
            .or_else(|| {
                json.pointer("/normalization/rows")
                    .and_then(Value::as_array)
                    .map(|rows| rows.len() as u64)
            })
            .unwrap_or(0);
        summary.add_metric(
            "regression_causality.normalized_row_count",
            MetricValue::measured(normalized_row_count as f64, "count"),
        );

        let suppressed_field_count = json
            .get("suppressedFieldCount")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        summary.add_metric(
            "regression_causality.suppressed_field_count",
            MetricValue::measured(suppressed_field_count as f64, "count"),
        );
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
    let verification_evidence_summary_json =
        verification_evidence_summary_json(workspace, audit_limit);
    let memory_drift_summary_json = memory_drift_summary_json(workspace, audit_limit);

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
    let swarm_replay_summary_json = swarm_replay_summary_json(workspace);
    let swarm_brief_summary_json = swarm_brief_summary_json(workspace);
    let swarm_incident_summary_json = swarm_incident_summary_json(workspace);
    let coordination_fallback_summary_json = coordination_fallback_summary_json(workspace);
    let singleflight_posture_json = singleflight_posture_json();
    let qos_lane_summary_json = qos_lane_summary_json(workspace);
    let triage_summary_json = triage_summary_json(&status, &swarm_reports);
    let local_cargo_tripwire_json = local_cargo_tripwire_json(workspace);
    let environment_attestation_summary_json = environment_attestation_summary_json(workspace);
    let regression_causality_summary_json =
        regression_causality_summary_json(&regression_causality_support_sections(
            verification_evidence_summary_json.as_str(),
            pack_replay_summary_json.as_str(),
            swarm_replay_summary_json.as_str(),
            swarm_brief_summary_json.as_str(),
            swarm_incident_summary_json.as_str(),
            performance_explain_samples_json.as_str(),
            scale_benchmark_summary_json.as_str(),
            triage_summary_json.as_str(),
            coordination_fallback_summary_json.as_str(),
            local_cargo_tripwire_json.as_str(),
            environment_attestation_summary_json.as_str(),
        ));

    Ok(CollectedDiagnostics {
        status_json,
        doctor_json,
        audit_json,
        verification_evidence_summary_json,
        memory_drift_summary_json,
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
        swarm_replay_summary_json,
        swarm_brief_summary_json,
        swarm_incident_summary_json,
        coordination_fallback_summary_json,
        singleflight_posture_json,
        qos_lane_summary_json,
        triage_summary_json,
        local_cargo_tripwire_json,
        regression_causality_summary_json,
        environment_attestation_summary_json,
    })
}

fn profile_evidence_json(workspace: &Path) -> String {
    let probe = super::profile::HostResourceProbeReport::gather_for_workspace(workspace);
    let recommendation = super::profile::recommend_operating_profile(&probe);
    let runtime = super::profile::runtime_profile_for_workspace(workspace);
    let active_profile = runtime.active_profile;
    let runtime_source = runtime.source;
    let host_calibration =
        super::budget_delta_recommender::build_host_calibration_posture(&probe, active_profile);
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
        "hostCalibration": host_calibration,
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
                "field": "hostCalibration",
                "sourceKind": "host_calibration",
                "source": super::budget_delta_recommender::HOST_CALIBRATION_POSTURE_SCHEMA_V1,
                "redaction": "label_only_paths_presence_only_env_no_raw_values",
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
    let database_present = support_bundle_database_path_is_regular(&database_path);
    let mut database = json!({
        "present": database_present,
        "readable": false,
        "workspaceRowPresent": false,
        "schemaVersion": null,
        "profileRowCount": 0,
        "summarizedAgentCount": 0,
    });

    if !database_present {
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
    let derived_asset_store = gather_default_derived_asset_store_summary();

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
        "derivedAssetStore": derived_asset_store.data_json(),
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
    let database_present = support_bundle_database_path_is_regular(&database_path);
    let mut snapshot = CacheHotsetSnapshot {
        database_present,
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

fn swarm_replay_summary_json(workspace: &Path) -> String {
    stable_json(&super::swarm_brief::collect_swarm_replay_summary(workspace))
}

fn swarm_brief_summary_json(workspace: &Path) -> String {
    let mut summary = super::swarm_brief::collect_swarm_brief_summary(workspace);
    redact_support_bundle_swarm_brief_summary(&mut summary);
    stable_json(&summary)
}

fn environment_attestation_summary_json(workspace: &Path) -> String {
    stable_json(&collect_environment_attestation_summary(workspace))
}

pub(crate) fn collect_regression_causality_summary(workspace: &Path) -> Value {
    let status = StatusReport::gather_for_workspace(workspace);
    let swarm_reports = discover_swarm_report_summaries(workspace);
    let verification_evidence_summary_json = verification_evidence_summary_json(workspace, 100);
    let pack_replay_summary_json = pack_replay_summary_json(workspace);
    let swarm_replay_summary_json = swarm_replay_summary_json(workspace);
    let swarm_brief_summary_json = swarm_brief_summary_json(workspace);
    let swarm_incident_summary_json = swarm_incident_summary_json(workspace);
    let performance_explain_samples_json = performance_explain_samples_json(workspace);
    let scale_benchmark_summary_json = scale_benchmark_summary_json(workspace, &swarm_reports);
    let triage_summary_json = triage_summary_json(&status, &swarm_reports);
    let coordination_fallback_summary_json = coordination_fallback_summary_json(workspace);
    let local_cargo_tripwire_json = local_cargo_tripwire_json(workspace);
    let environment_attestation_summary_json = environment_attestation_summary_json(workspace);

    regression_causality_summary_value(&regression_causality_support_sections(
        verification_evidence_summary_json.as_str(),
        pack_replay_summary_json.as_str(),
        swarm_replay_summary_json.as_str(),
        swarm_brief_summary_json.as_str(),
        swarm_incident_summary_json.as_str(),
        performance_explain_samples_json.as_str(),
        scale_benchmark_summary_json.as_str(),
        triage_summary_json.as_str(),
        coordination_fallback_summary_json.as_str(),
        local_cargo_tripwire_json.as_str(),
        environment_attestation_summary_json.as_str(),
    ))
}

pub(crate) fn collect_environment_attestation_summary(workspace: &Path) -> Value {
    let mut options = super::swarm_brief::SwarmBriefCollectOptions::for_workspace(workspace);
    options.include_rch = true;
    options.enabled_sources = super::swarm_brief::all_swarm_brief_sources();
    let runner = super::swarm_brief::SystemSwarmBriefCommandRunner;
    let report = super::environment_attestation::collect_environment_attestation(&options, &runner);
    environment_attestation_summary_from_report(&report)
}

pub(crate) fn environment_attestation_summary_from_report(
    report: &super::environment_attestation::EnvironmentAttestationReport,
) -> Value {
    let degraded_codes = attestation_degraded_codes(report);
    let mut status_counts = BTreeMap::new();
    let mut authority_counts = BTreeMap::new();
    for entry in &report.source_authority {
        increment_attestation_count(&mut status_counts, serialized_token(&entry.status));
        increment_attestation_count(&mut authority_counts, serialized_token(&entry.authority));
    }

    let mut summary = json!({
        "schema": SUPPORT_BUNDLE_ENVIRONMENT_ATTESTATION_SUMMARY_SCHEMA_V1,
        "sourceSchema": report.schema,
        "status": "available",
        "attestationId": &report.attestation_id,
        "workspaceIncluded": false,
        "workspaceHash": support_cache_key(&report.workspace),
        "redactionStatus": "counts_ids_statuses_codes_hashes_redacted_text_no_raw_paths_no_mail_bodies_no_source_text",
        "summary": &report.summary,
        "verdict": serialized_token(&report.verdict),
        "proofAdmission": {
            "remoteVerificationAdmitted": report.summary.remote_verification_admitted,
            "sourceTestVerdict": serialized_token(&report.summary.source_test_verdict),
            "environmentVerdict": serialized_token(&report.summary.environment_verdict),
            "localCargoFallbackObserved": report.summary.local_cargo_fallback_observed,
            "separateFromSourceTestVerdict": true,
        },
        "sourceAuthorityCounts": {
            "total": report.source_authority.len(),
            "byStatus": status_counts,
            "byAuthority": authority_counts,
        },
        "sourceAuthority": report
            .source_authority
            .iter()
            .map(attestation_source_authority_summary)
            .collect::<Vec<_>>(),
        "degradedCodes": degraded_codes,
        "recoveryActions": report
            .recovery_actions
            .iter()
            .map(attestation_recovery_action_summary)
            .collect::<Vec<_>>(),
        "firstFailure": attestation_first_failure(report),
        "disagreementEvidence": attestation_disagreement_evidence(report),
        "evidenceRefHashes": report
            .evidence_refs
            .iter()
            .map(|reference| support_cache_key(reference))
            .collect::<Vec<_>>(),
        "redaction": {
            "rawWorkspacePathIncluded": false,
            "rawMailBodiesIncluded": false,
            "rawSourceSnippetsIncluded": false,
            "rawCommandArgvIncluded": false,
            "rawEvidenceRefsIncluded": false,
            "hostPrivatePathsRedacted": true,
        },
    });
    let summary_hash = support_cache_key(&stable_json(&summary));
    if let Some(object) = summary.as_object_mut() {
        object.insert("summaryHash".to_owned(), json!(summary_hash));
    }
    summary
}

pub(crate) fn environment_attestation_summary_evidence_id(summary: &Value) -> String {
    let hash = summary
        .get("summaryHash")
        .and_then(Value::as_str)
        .or_else(|| summary.get("attestationId").and_then(Value::as_str))
        .unwrap_or("unknown")
        .trim_start_matches("blake3:")
        .trim_start_matches("environment_attestation_");
    let short_hash = hash.get(..12).unwrap_or(hash);
    format!("environment_attestation_summary:{short_hash}")
}

pub(crate) fn render_environment_attestation_summary_for_handoff(summary: &Value) -> String {
    let proof = summary
        .get("proofAdmission")
        .unwrap_or(&serde_json::Value::Null);
    let source_counts = summary
        .get("sourceAuthorityCounts")
        .unwrap_or(&serde_json::Value::Null);
    let total_sources = source_counts
        .get("total")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let by_status = source_counts
        .get("byStatus")
        .map(stable_json)
        .unwrap_or_else(|| "{}".to_owned());
    let verdict = summary
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let safe_to_claim = summary
        .pointer("/summary/safeToClaim")
        .and_then(Value::as_bool)
        .map_or("unknown".to_owned(), |value| value.to_string());
    let remote_admitted = proof
        .get("remoteVerificationAdmitted")
        .and_then(Value::as_bool)
        .map_or("unknown".to_owned(), |value| value.to_string());
    let source_test = proof
        .get("sourceTestVerdict")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let environment = proof
        .get("environmentVerdict")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let local_fallback = proof
        .get("localCargoFallbackObserved")
        .and_then(Value::as_bool)
        .map_or("unknown".to_owned(), |value| value.to_string());
    let summary_hash = summary
        .get("summaryHash")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let degraded_codes = summary
        .get("degradedCodes")
        .and_then(Value::as_array)
        .map(|codes| {
            codes
                .iter()
                .filter_map(Value::as_str)
                .take(6)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let first_failure_code = summary
        .pointer("/firstFailure/code")
        .and_then(Value::as_str)
        .unwrap_or("none");

    let mut lines = vec![
        format!(
            "Environment attestation: verdict={verdict}, safe_to_claim={safe_to_claim}, environment_verdict={environment}, source_test_verdict={source_test}."
        ),
        format!(
            "Proof admission: remote_verification_admitted={remote_admitted}, local_cargo_fallback_observed={local_fallback}; source_result_and_environment_admission_are_separate=true."
        ),
        format!(
            "Source authority: sources={total_sources}, by_status={by_status}, first_failure={first_failure_code}, summary_hash={summary_hash}."
        ),
        "Redaction: raw_mail_bodies_included=false, raw_paths_included=false, evidence_refs=hashes_only, command_argv=hashes_only."
            .to_owned(),
        "Diagnostic posture only; run ee diag environment-attestation --workspace . --include-rch --json before claiming, closing, or treating proof as current."
            .to_owned(),
    ];
    if !degraded_codes.is_empty() {
        lines.push(format!(
            "Attestation degraded codes: {}.",
            degraded_codes.join(", ")
        ));
    }
    lines.join("\n")
}

pub(crate) fn regression_causality_summary_evidence_id(summary: &Value) -> String {
    let hash = blake3_text_hash(&stable_json(summary));
    let short_hash = hash.trim_start_matches("blake3:");
    let short_hash = short_hash.get(..12).unwrap_or(short_hash);
    format!("regression_causality_summary:{short_hash}")
}

pub(crate) fn render_regression_causality_summary_for_handoff(summary: &Value) -> String {
    let schema = summary
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or(SUPPORT_BUNDLE_REGRESSION_CAUSALITY_SUMMARY_SCHEMA_V1);
    let source_schema = summary
        .get("sourceSchema")
        .and_then(Value::as_str)
        .unwrap_or(REGRESSION_CAUSALITY_SCHEMA_V1);
    let status = summary
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let redaction_status = summary
        .get("redactionStatus")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let input_count = summary
        .get("inputSectionCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let row_count = summary
        .get("normalizedRowCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let suppressed = summary
        .get("suppressedFieldCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let summary_hash = blake3_text_hash(&stable_json(summary));
    let top_codes = summary
        .get("topHypotheses")
        .and_then(Value::as_array)
        .map(|hypotheses| {
            hypotheses
                .iter()
                .filter_map(|hypothesis| hypothesis.get("code").and_then(Value::as_str))
                .take(5)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let top_codes = if top_codes.is_empty() {
        "none".to_owned()
    } else {
        top_codes.join(", ")
    };

    [
        format!("Regression causality summary: status={status}, source_schema={source_schema}, schema={schema}."),
        format!("Evidence normalization: input_sections={input_count}, normalized_rows={row_count}, suppressed_fields={suppressed}, top_hypothesis_codes={top_codes}."),
        format!("Redaction: status={redaction_status}, input_artifacts_copied=false, raw_logs_present=false, raw_mail_bodies_present=false, raw_memory_bodies_present=false, private_paths_present=false, hashes_only=true, summary_hash={summary_hash}."),
        "Diagnostic posture only; rerun ee support bundle or ee regress explain against current artifacts before treating hypotheses as current."
            .to_owned(),
    ]
    .join("\n")
}

fn increment_attestation_count(counts: &mut BTreeMap<String, u64>, key: String) {
    let count = counts.entry(key).or_insert(0);
    *count = count.saturating_add(1);
}

fn attestation_source_authority_summary(
    entry: &super::environment_attestation::EnvironmentAttestationSourceAuthorityEntry,
) -> Value {
    json!({
        "source": serialized_token(&entry.source),
        "authority": serialized_token(&entry.authority),
        "status": serialized_token(&entry.status),
        "freshness": serialized_token(&entry.freshness),
        "observedAt": entry.observed_at.as_deref(),
        "summary": redact_support_diagnostic_text(&entry.summary),
        "metricCount": entry.metrics.len(),
        "metrics": entry
            .metrics
            .iter()
            .map(|metric| json!({
                "name": &metric.name,
                "value": redact_support_diagnostic_text(&metric.value),
            }))
            .collect::<Vec<_>>(),
        "degradedCodes": entry
            .degraded_codes
            .iter()
            .map(serialized_token)
            .collect::<Vec<_>>(),
        "recoveryActionCount": entry.recovery_actions.len(),
        "recoveryActions": entry
            .recovery_actions
            .iter()
            .map(attestation_recovery_action_summary)
            .collect::<Vec<_>>(),
        "evidenceRefHashes": entry
            .evidence_refs
            .iter()
            .map(|reference| support_cache_key(reference))
            .collect::<Vec<_>>(),
    })
}

fn attestation_recovery_action_summary(
    action: &super::environment_attestation::EnvironmentAttestationRecoveryAction,
) -> Value {
    let command = action.command.as_ref().map(|command| {
        json!({
            "displayCommand": redact_support_diagnostic_text(&command.display_command),
            "argvHash": support_cache_key(&command.argv.join("\x1f")),
            "shellRequired": command.shell_required,
            "copySafety": serialized_token(&command.copy_safety),
        })
    });
    json!({
        "priority": action.priority,
        "kind": serialized_token(&action.kind),
        "mutatesState": action.mutates_state,
        "requiredSubstrate": serialized_token(&action.required_substrate),
        "rationale": redact_support_diagnostic_text(&action.rationale),
        "command": command,
    })
}

fn attestation_degraded_codes(
    report: &super::environment_attestation::EnvironmentAttestationReport,
) -> Vec<String> {
    let mut codes = report
        .degraded
        .iter()
        .map(|degraded| serialized_token(&degraded.code))
        .collect::<Vec<_>>();
    codes.sort();
    codes.dedup();
    codes
}

fn attestation_first_failure(
    report: &super::environment_attestation::EnvironmentAttestationReport,
) -> Value {
    report.degraded.first().map_or(Value::Null, |degraded| {
        json!({
            "code": serialized_token(&degraded.code),
            "severity": degraded.severity,
            "message": redact_support_diagnostic_text(&degraded.message),
            "repair": degraded
                .repair
                .as_deref()
                .map(redact_support_diagnostic_text),
        })
    })
}

fn attestation_disagreement_evidence(
    report: &super::environment_attestation::EnvironmentAttestationReport,
) -> Value {
    let codes = attestation_degraded_codes(report);
    json!({
        "beadsTrackerStale": codes.iter().any(|code| code == "beads_tracker_stale"),
        "bvRecommendationStale": codes.iter().any(|code| code == "bv_recommendation_stale"),
        "agentMailProbeMismatch": codes.iter().any(|code| code == "agent_mail_probe_mismatch"),
        "sourceAuthorityAmbiguous": codes.iter().any(|code| code == "source_authority_ambiguous"),
        "claimGateNeedsFreshRun": codes.iter().any(|code| {
            matches!(
                code.as_str(),
                "dirty_checkout_observed"
                    | "reservation_evidence_stale"
                    | "source_authority_ambiguous"
                    | "stale_binary_suspected"
                    | "ci_proof_lane_artifact_missing"
                    | "ci_proof_lane_artifact_stale"
                    | "ci_proof_lane_cancelled_before_artifact"
                    | "ci_proof_lane_checksum_mismatch"
                    | "ci_proof_lane_surface_probe_failed"
                    | "ci_proof_lane_unknown_source"
                    | "ci_proof_lane_duplicate_dispatch"
            )
        }),
        "ciProofLaneArtifactMissing": codes
            .iter()
            .any(|code| code == "ci_proof_lane_artifact_missing"),
        "ciProofLaneArtifactStale": codes
            .iter()
            .any(|code| code == "ci_proof_lane_artifact_stale"),
        "ciProofLaneCancelledBeforeArtifact": codes
            .iter()
            .any(|code| code == "ci_proof_lane_cancelled_before_artifact"),
        "ciProofLaneChecksumMismatch": codes
            .iter()
            .any(|code| code == "ci_proof_lane_checksum_mismatch"),
        "ciProofLaneSurfaceProbeFailed": codes
            .iter()
            .any(|code| code == "ci_proof_lane_surface_probe_failed"),
        "ciProofLaneUnknownSource": codes
            .iter()
            .any(|code| code == "ci_proof_lane_unknown_source"),
        "ciProofLaneDuplicateDispatch": codes
            .iter()
            .any(|code| code == "ci_proof_lane_duplicate_dispatch"),
    })
}

fn serialized_token<T: Serialize + ?Sized>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(token)) => token,
        Ok(value) => stable_json(&value),
        Err(_) => "serialization_error".to_owned(),
    }
}

pub(crate) fn redact_support_bundle_swarm_brief_summary(summary: &mut Value) {
    hash_holder_count_keys(summary, "/fileSurfaceRiskSummary/countsByReservationHolder");
    hash_holder_array_field(
        summary,
        "/fileSurfaceRiskSummary/topRisks",
        "reservationHolders",
    );
    hash_holder_count_keys(
        summary,
        "/readyReservationPressureSummary/countsByReservationHolder",
    );
    hash_holder_array_field(
        summary,
        "/readyReservationPressureSummary/topReadyBeads",
        "reservationHolders",
    );
    let Some(summary_object) = summary.as_object_mut() else {
        return;
    };
    let redaction_value = summary_object
        .entry("redaction".to_string())
        .or_insert_with(|| json!({}));
    if !redaction_value.is_object() {
        *redaction_value = json!({});
    }
    if let Some(redaction) = redaction_value.as_object_mut() {
        redaction.insert("rawAgentNamesIncluded".to_string(), json!(false));
        redaction.insert("rawSymbolNamesIncluded".to_string(), json!(false));
        redaction.insert(
            "reservationHolderLabelsIncluded".to_string(),
            json!("hashes_only"),
        );
    }
}

fn hash_holder_count_keys(summary: &mut Value, pointer: &str) {
    let Some(counts) = summary
        .pointer_mut(pointer)
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    let original = std::mem::take(counts);
    for (holder, count) in original {
        insert_holder_count(counts, support_holder_label_hash(&holder), count);
    }
}

fn insert_holder_count(
    counts: &mut serde_json::Map<String, Value>,
    holder_hash: String,
    count: Value,
) {
    let Some(existing) = counts.get_mut(&holder_hash) else {
        counts.insert(holder_hash, count);
        return;
    };
    if let (Some(existing_count), Some(incoming_count)) = (existing.as_u64(), count.as_u64()) {
        *existing = json!(existing_count.saturating_add(incoming_count));
    }
}

fn hash_holder_array_field(summary: &mut Value, array_pointer: &str, field: &str) {
    let Some(items) = summary
        .pointer_mut(array_pointer)
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for item in items {
        let Some(holders) = item
            .get_mut(field)
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        for holder in holders {
            let Some(raw_holder) = holder.as_str() else {
                continue;
            };
            *holder = Value::String(support_holder_label_hash(raw_holder));
        }
    }
}

fn support_holder_label_hash(holder: &str) -> String {
    if is_support_holder_label_hash(holder) {
        holder.to_string()
    } else {
        support_agent_name_hash(holder)
    }
}

fn is_support_holder_label_hash(holder: &str) -> bool {
    let Some(hex) = holder.strip_prefix("blake3:") else {
        return false;
    };
    matches!(hex.len(), 12 | 64) && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn swarm_incident_summary_json(workspace: &Path) -> String {
    stable_json(&super::swarm_brief::collect_swarm_incident_summary(
        workspace,
    ))
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
    // EE-QOS-001: QoS lane summary (safe to bundle unredacted, keys are abstract)
    let value = serde_json::to_value(super::qos::summarize_qos_lane_registry(
        workspace,
        &workspace_identity,
        now_epoch_ms,
    ))
    .unwrap_or_else(|error| {
        json!({
            "schema": "ee.support_bundle.serialization_error.v1",
            "message": error.to_string(),
        })
    });
    stable_json(&value)
}

fn local_cargo_tripwire_json(workspace: &Path) -> String {
    let direct_cargo = local_cargo_preflight_classification(
        workspace,
        "cargo test --lib support_bundle_tripwire_probe",
    );
    let wrapped_cargo = local_cargo_preflight_classification(
        workspace,
        "scripts/rch_verify.sh -- cargo test --lib support_bundle_tripwire_probe",
    );
    let direct_status = direct_cargo
        .get("policyStatus")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let wrapped_status = wrapped_cargo
        .get("policyStatus")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let build_admission = super::disk_pressure::gather_build_admission_report(
        &super::disk_pressure::BuildAdmissionOptions {
            workspace: workspace.to_path_buf(),
            workspace_source: "support_bundle",
            min_free_bytes: 1024 * 1024 * 1024,
            artifact_destinations: Vec::new(),
        },
    );
    let build_admission_json = serde_json::to_value(&build_admission).unwrap_or_else(|error| {
        json!({
            "schema": "ee.support_bundle.serialization_error.v1",
            "message": error.to_string(),
        })
    });
    let build_admission_status = if build_admission.admitted {
        "remote_required_ready"
    } else {
        "remote_required_blocked"
    };
    let process_scan = local_cargo_tripwire_process_scan_json(workspace);
    let process_status = process_scan
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unavailable");
    let detected_local_builds = process_scan
        .get("detectedLocalBuilds")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let process_evidence = process_scan
        .get("evidence")
        .and_then(Value::as_array)
        .and_then(|evidence| evidence.first())
        .cloned()
        .unwrap_or_else(|| {
            json!({
                "kind": "active_process_scan",
                "result": process_status,
            })
        });
    let disk_pressure_context = process_scan
        .get("disk_pressure_context")
        .cloned()
        .unwrap_or(Value::Null);
    let process_scan_detected_bypass = process_status == "bypass_detected"
        || detected_local_builds
            .as_array()
            .is_some_and(|items| !items.is_empty());
    let policy_state = if process_scan_detected_bypass {
        "local_disallowed_attempt"
    } else if !build_admission.admitted {
        "remote_required_blocked"
    } else if direct_status == "local_cargo_disallowed"
        && wrapped_status == "remote_wrapper_required"
    {
        "remote_required_ready"
    } else {
        "needs_review"
    };
    let collection_status = if process_scan_detected_bypass {
        "local_cargo_bypass_detected"
    } else if process_status == "clean" {
        "policy_and_live_process_scan"
    } else {
        "policy_summary_process_scan_unavailable"
    };
    let policy_status = if process_scan_detected_bypass {
        "blocked"
    } else if direct_status == "local_cargo_disallowed"
        && wrapped_status == "remote_wrapper_required"
    {
        "enforced"
    } else {
        "needs_review"
    };
    let repair_actions = if process_scan_detected_bypass {
        process_scan
            .get("repairActions")
            .cloned()
            .unwrap_or_else(|| json!([]))
    } else if direct_status == "local_cargo_disallowed" {
        json!([{
            "priority": 1,
            "kind": "use_remote_wrapper",
            "command": SUPPORT_BUNDLE_REQUIRED_REMOTE_WRAPPER,
            "message": "Run Rust verification through the repo RCH wrapper; do not retry local Cargo.",
        }])
    } else {
        json!([])
    };

    stable_json(&json!({
        "schema": "ee.support_bundle.local_cargo_tripwire.v1",
        "collectionStatus": collection_status,
        "localBuildPolicy": {
            "policy": "rch_only",
            "policyState": policy_state,
            "status": policy_status,
            "commandScope": "support_bundle_policy_summary",
            "allowedReadOnlyCargoSubcommands": ["metadata", "locate-project", "pkgid", "tree"],
        },
        "requiredRemoteWrapper": SUPPORT_BUNDLE_REQUIRED_REMOTE_WRAPPER,
        "detectedLocalBuilds": detected_local_builds,
        "repairActions": repair_actions,
        "disk_pressure_context": disk_pressure_context,
        "evidence": [
            {
                "kind": "planned_command_classification",
                "result": direct_status,
                "command": "cargo test --lib support_bundle_tripwire_probe",
            },
            {
                "kind": "planned_command_classification",
                "result": wrapped_status,
                "command": "scripts/rch_verify.sh -- cargo test --lib support_bundle_tripwire_probe",
            },
            {
                "kind": "build_admission",
                "result": build_admission_status,
                "admitted": build_admission.admitted,
            },
            process_evidence
        ],
        "plannedCommandClassifications": [direct_cargo, wrapped_cargo],
        "buildAdmission": build_admission_json,
        "processScan": process_scan,
        "notes": [
            "Support bundle collection is read-only and does not execute Cargo.",
            "Live process evidence comes from the read-only local-Cargo tripwire process scanner."
        ],
    }))
}

fn regression_causality_summary_json(sections: &[(&str, RegressionEvidenceKind, &str)]) -> String {
    stable_json(&regression_causality_summary_value(sections))
}

fn regression_causality_support_sections<'a>(
    verification_evidence_summary_json: &'a str,
    pack_replay_summary_json: &'a str,
    swarm_replay_summary_json: &'a str,
    swarm_brief_summary_json: &'a str,
    swarm_incident_summary_json: &'a str,
    performance_explain_samples_json: &'a str,
    scale_benchmark_summary_json: &'a str,
    triage_summary_json: &'a str,
    coordination_fallback_summary_json: &'a str,
    local_cargo_tripwire_json: &'a str,
    environment_attestation_summary_json: &'a str,
) -> [(&'static str, RegressionEvidenceKind, &'a str); 11] {
    [
        (
            "support_bundle:verification_evidence_summary",
            RegressionEvidenceKind::VerificationEvidence,
            verification_evidence_summary_json,
        ),
        (
            "support_bundle:pack_replay_summary",
            RegressionEvidenceKind::PackReplay,
            pack_replay_summary_json,
        ),
        (
            "support_bundle:swarm_replay_summary",
            RegressionEvidenceKind::SwarmReplay,
            swarm_replay_summary_json,
        ),
        (
            "support_bundle:swarm_brief_summary",
            RegressionEvidenceKind::SwarmReplay,
            swarm_brief_summary_json,
        ),
        (
            "support_bundle:swarm_incident_summary",
            RegressionEvidenceKind::SwarmReplay,
            swarm_incident_summary_json,
        ),
        (
            "support_bundle:performance_explain_samples",
            RegressionEvidenceKind::PerfReport,
            performance_explain_samples_json,
        ),
        (
            "support_bundle:scale_benchmark_summary",
            RegressionEvidenceKind::PerfReport,
            scale_benchmark_summary_json,
        ),
        (
            "support_bundle:triage_summary",
            RegressionEvidenceKind::SupportBundle,
            triage_summary_json,
        ),
        (
            "support_bundle:coordination_fallback_summary",
            RegressionEvidenceKind::SupportBundle,
            coordination_fallback_summary_json,
        ),
        (
            "support_bundle:local_cargo_tripwire",
            RegressionEvidenceKind::VerificationEvidence,
            local_cargo_tripwire_json,
        ),
        (
            "support_bundle:environment_attestation_summary",
            RegressionEvidenceKind::SupportBundle,
            environment_attestation_summary_json,
        ),
    ]
}

fn regression_causality_summary_value(sections: &[(&str, RegressionEvidenceKind, &str)]) -> Value {
    let inputs = sections
        .iter()
        .map(|(id, kind, content)| {
            regression_causality_input_from_support_section(id, *kind, content)
        })
        .collect::<Vec<_>>();
    let normalization = normalize_regression_evidence_inputs(&inputs);
    let ranking = rank_regression_cause_hypotheses(&normalization.rows);
    let top_hypotheses = ranking
        .hypotheses
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>();
    let suppressed_field_count = normalization
        .rows
        .iter()
        .map(|row| row.provenance.suppressed_fields.len())
        .sum::<usize>();
    let status = if ranking.hypotheses.is_empty() {
        "no_ranked_hypotheses"
    } else {
        "ranked"
    };
    let provenance = sections
        .iter()
        .map(|(id, kind, _)| {
            json!({
                "sourceId": id,
                "kind": kind.as_str(),
                "redaction": "section_json_not_copied_values_normalized_rows_only",
            })
        })
        .collect::<Vec<_>>();

    json!({
        "schema": SUPPORT_BUNDLE_REGRESSION_CAUSALITY_SUMMARY_SCHEMA_V1,
        "sourceSchema": REGRESSION_CAUSALITY_SCHEMA_V1,
        "status": status,
        "redactionStatus": "derived_redaction_safe_no_raw_logs",
        "inputSectionCount": sections.len(),
        "normalizedRowCount": normalization.rows.len(),
        "suppressedFieldCount": suppressed_field_count,
        "topHypotheses": top_hypotheses,
        "normalization": normalization,
        "ranking": ranking,
        "redaction": {
            "inputArtifactsCopied": false,
            "rawLogsPresent": false,
            "rawMailBodiesPresent": false,
            "rawMemoryBodiesPresent": false,
            "privatePathsPresent": false,
            "hashesOnly": true,
            "secretScanApplied": true,
            "normalizerSuppressedFieldCount": suppressed_field_count,
        },
        "provenance": provenance,
    })
}

fn regression_causality_input_from_support_section(
    id: &str,
    kind: RegressionEvidenceKind,
    content: &str,
) -> RegressionEvidenceInput {
    let artifact = serde_json::from_str::<Value>(content).unwrap_or_else(|error| {
        json!({
            "schema": "ee.support_bundle.section_parse_error.v1",
            "status": "malformed",
            "degradedCodes": ["support_bundle_section_parse_error"],
            "redactionStatus": "safe",
            "message": error.to_string(),
        })
    });
    RegressionEvidenceInput::new(id.to_owned(), kind, artifact)
        .with_artifact_hash(format!("blake3:{}", compute_hash(content)))
}

pub(crate) fn local_cargo_tripwire_process_scan_json(workspace: &Path) -> Value {
    let script = workspace
        .join("scripts")
        .join("check-local-cargo-tripwire.sh");
    // bd-hwowj: gate Command::new behind a strict "regular file at
    // the literal path" check. The prior `is_file()` followed
    // symlinks, so a malicious workspace could plant
    // `scripts/check-local-cargo-tripwire.sh -> /bin/sh` (or any
    // other binary inside or outside the workspace) and gain command
    // execution as soon as a user ran `ee support bundle` or the
    // completion-audit pathway against that workspace. fs::
    // symlink_metadata inspects the link itself; refusing
    // file_type().is_symlink() before `Command::new` closes the
    // attack without affecting workspaces that ship the real
    // regular file at the documented path. The component walk below
    // closes the sibling parent-symlink variant
    // (`scripts/ -> outside-dir`) before `symlink_metadata` can
    // observe a regular final file through that parent.
    if reject_existing_symlink_component(&script, "local cargo tripwire script").is_err() {
        return unavailable_local_cargo_process_scan_json(
            "tripwire_script_symlink_refused",
            Some(
                "Refusing to execute scripts/check-local-cargo-tripwire.sh because the path \
                 includes a symlinked component; replace it with a real regular file to enable \
                 the process scan.",
            ),
        );
    }
    let metadata = match fs::symlink_metadata(&script) {
        Ok(metadata) => metadata,
        Err(_) => {
            return unavailable_local_cargo_process_scan_json("tripwire_script_missing", None);
        }
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return unavailable_local_cargo_process_scan_json(
            "tripwire_script_symlink_refused",
            Some(
                "Refusing to execute scripts/check-local-cargo-tripwire.sh because the path is \
                 a symlink; replace it with a regular file to enable the process scan.",
            ),
        );
    }
    if !file_type.is_file() {
        return unavailable_local_cargo_process_scan_json("tripwire_script_missing", None);
    }

    match Command::new(&script)
        .arg("--probe-processes")
        .arg("--json")
        .current_dir(workspace)
        .output()
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            match serde_json::from_str::<Value>(&stdout) {
                Ok(value) => value,
                Err(error) => unavailable_local_cargo_process_scan_json(
                    "tripwire_output_unparseable",
                    Some(&error.to_string()),
                ),
            }
        }
        Err(error) => unavailable_local_cargo_process_scan_json(
            "tripwire_execution_failed",
            Some(&error.to_string()),
        ),
    }
}

fn unavailable_local_cargo_process_scan_json(reason: &str, detail: Option<&str>) -> Value {
    json!({
        "schema": "ee.rch_local_cargo_tripwire.v1",
        "mode": "probe_processes",
        "status": "unavailable",
        "count": 0,
        "reason": reason,
        "detail": detail,
        "detectedLocalBuilds": [],
        "repairActions": [],
        "evidence": [{
            "kind": "active_process_scan",
            "result": "unavailable",
            "reason": reason,
        }],
    })
}

fn local_cargo_preflight_classification(workspace: &Path, command: &str) -> Value {
    let registry = super::preflight_guard::PreflightGuardRegistry::with_builtins();
    let report = super::preflight_guard::run_preflight_guard(
        &registry,
        &super::preflight_guard::PreflightGuardOptions {
            command: command.to_owned(),
            workspace: workspace.to_path_buf(),
            bypass_tokens: Vec::new(),
            bypass_secret: None,
        },
    );
    let matched_rule_ids = report
        .matches
        .iter()
        .map(|matched| matched.rule_id.clone())
        .collect::<Vec<_>>();
    let local_cargo_denied = matched_rule_ids.iter().any(|rule_id| {
        matches!(
            rule_id.as_str(),
            "builtin:local_cargo_heavy_verification"
                | "builtin:local_cargo_target_dir_override"
                | "builtin:local_rust_compiler_verification"
        )
    });
    let policy_status = if local_cargo_denied {
        "local_cargo_disallowed"
    } else if report.exit_code == 0 && command.contains("scripts/rch_verify.sh") {
        "remote_wrapper_required"
    } else if report.exit_code == 0 {
        "allowed"
    } else {
        "blocked_by_other_policy"
    };

    json!({
        "schema": super::preflight_guard::PREFLIGHT_GUARD_SCHEMA_V1,
        "command": command,
        "policyStatus": policy_status,
        "exitCode": report.exit_code,
        "matchedRuleIds": matched_rule_ids,
        "guardReport": report.to_json(),
    })
}

fn collect_pack_replay_summary(workspace: &Path) -> Value {
    let database_path = workspace.join(".ee").join("ee.db");
    let database_present = support_bundle_database_path_is_regular(&database_path);
    let mut database = json!({
        "present": database_present,
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

    if !database_present {
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
    let parsed = crate::db::parse_pack_ledger_fields(
        "support_bundle_pack_replay_summary",
        raw_ledger,
        expected_hash,
    );
    let storage = crate::db::pack_ledger_storage_summary(raw_ledger);
    let Some(ledger) = parsed.ledger.as_ref() else {
        return json!({
            "status": parsed.status.as_str(),
            "hashVerified": false,
            "schema": null,
            "storage": storage,
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

    let actual_hash = ledger.get("ledgerHash").and_then(Value::as_str);
    let hash_verified = expected_hash.is_some_and(|hash| Some(hash) == actual_hash);
    let selected_items = support_ledger_core_array(ledger, "selectedItems");
    let omitted_items = support_ledger_core_array(ledger, "omittedItems");

    json!({
        "status": parsed.status.as_str(),
        "hashVerified": hash_verified,
        "schema": support_ledger_core_value(ledger, "schema").cloned().unwrap_or(Value::Null),
        "storage": storage,
        "selectedItemCount": selected_items.map_or(0, Vec::len),
        "omittedItemCount": omitted_items.map_or(0, Vec::len),
        "freshnessStates": pack_ledger_freshness_counts(selected_items),
        "redactionClasses": pack_ledger_redaction_classes(selected_items),
        "degradationCodes": pack_ledger_degradation_codes(ledger),
        "derivedAssets": support_ledger_core_value(ledger, "derivedAssets").cloned().unwrap_or_else(|| json!({})),
        "database": support_ledger_core_value(ledger, "database").cloned().unwrap_or_else(|| json!({})),
        "candidateCounts": support_ledger_core_value(ledger, "candidateCounts").cloned().unwrap_or_else(|| json!({})),
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
    let Ok(metadata) = fs::symlink_metadata(&ledger_path) else {
        return coordination_fallback_summary_value("ledger_unreadable", ledger, Vec::new());
    };
    if !metadata.file_type().is_file() {
        return coordination_fallback_summary_value("ledger_unreadable", ledger, Vec::new());
    }

    // Bounded read: previous `fs::read_to_string(&ledger_path)` allocated
    // the entire (peer-controllable, append-only) ledger into memory before
    // we even looked at the bytes. A peer-planted or runaway-emitter multi-
    // GB ledger would OOM the support-bundle hot path with no signal. Cap
    // at `COORDINATION_FALLBACK_LEDGER_MAX_BYTES` to bound peak allocation;
    // the function only summarizes the first 16 matching records anyway,
    // so silently truncating past the cap loses nothing the summary surfaces.
    // Matches the parallel cap on the same file in
    // `src/core/why.rs::fetch_coordination_fallback_evidence`.
    let mut content = String::new();
    let Ok(mut file) = open_support_bundle_file_for_read_no_follow(&ledger_path) else {
        return coordination_fallback_summary_value("ledger_unreadable", ledger, Vec::new());
    };
    let Ok(opened_metadata) = file.metadata() else {
        return coordination_fallback_summary_value("ledger_unreadable", ledger, Vec::new());
    };
    if !opened_metadata.file_type().is_file() {
        return coordination_fallback_summary_value("ledger_unreadable", ledger, Vec::new());
    }
    if (&mut file)
        .take(COORDINATION_FALLBACK_LEDGER_MAX_BYTES)
        .read_to_string(&mut content)
        .is_err()
    {
        return coordination_fallback_summary_value("ledger_unreadable", ledger, Vec::new());
    }
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
            "sourceId": evidence
                .pointer("/source/sourceId")
                .and_then(Value::as_str)
                .map(redact_support_diagnostic_text),
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

/// Bounded read for per-sample workspace JSON files.
///
/// The prior shape `fs::read_to_string(path).ok()?` (used by both
/// `summarize_performance_explain_sample` and `summarize_swarm_report`)
/// pre-sized its destination `String` from the file's stat-time
/// metadata length. A peer-planted or runaway-writer multi-GiB file at
/// `<workspace>/.ee/performance-explain/<name>.json` or
/// `<workspace>/.ee/swarm-contention/<name>.json` would force a
/// matching allocation BEFORE `serde_json::from_str` could reject the
/// payload. The two-layer cap matches the recipe used by the
/// convergence-pass siblings (`src/science/mod.rs::EVALUATION_SNAPSHOT_MAX_BYTES`,
/// `src/core/symbol_graph.rs` Rust source cap via 27a3cb9b,
/// `src/core/jsonl_import.rs::read_jsonl_source_bounded`):
///
/// 1. `fs::metadata(...)` rejects an oversized file at stat time.
/// 2. `file.take(MAX + 1).read_to_string(...)` closes the growth
///    window: if the file grew between stat and open, the bounded read
///    still pins peak allocation to MAX + 1 bytes; the post-read
///    `len > MAX` check then drops the sample.
///
/// Returns `None` on any failure (matches the existing `.ok()?` flow at
/// the call sites — an unparseable sample is dropped from the summary,
/// not surfaced as a hard error, since the summary already truncates to
/// 16 samples per directory).
fn read_support_bundle_sample_file_bounded(path: &Path) -> Option<String> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_SUPPORT_BUNDLE_SAMPLE_FILE_BYTES {
        return None;
    }
    let file = open_support_bundle_file_for_read_no_follow(path).ok()?;
    let opened_metadata = file.metadata().ok()?;
    if !opened_metadata.file_type().is_file()
        || opened_metadata.len() > MAX_SUPPORT_BUNDLE_SAMPLE_FILE_BYTES
    {
        return None;
    }
    let mut content = String::new();
    let mut limited = file.take(MAX_SUPPORT_BUNDLE_SAMPLE_FILE_BYTES.saturating_add(1));
    limited.read_to_string(&mut content).ok()?;
    if content.len() as u64 > MAX_SUPPORT_BUNDLE_SAMPLE_FILE_BYTES {
        return None;
    }
    Some(content)
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
    let raw_content = read_support_bundle_sample_file_bounded(path)?;
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
    let raw_content = read_support_bundle_sample_file_bounded(path)?;
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

/// Recursively route every string in a JSON value through the canonical
/// support-diagnostic redaction (secrets, PII, tailscale metadata,
/// path-like segments). Numbers, bools, and null pass through unchanged.
/// Exposed to the crate so content-bearing surfaces outside the support
/// bundle — e.g. the daemon RPC dispatch echo path (bd-3uev6) — can reuse
/// the single canonical redaction implementation rather than reflecting
/// raw bytes.
pub(crate) fn redact_json_value(value: &Value) -> Value {
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

fn redact_support_bundle_path(path: &Path, level: RedactionLevel) -> SupportDiagnosticRedaction {
    redact_support_bundle_content(&path.display().to_string(), level)
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
    if !support_bundle_database_path_is_regular(&database_path) {
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

fn verification_evidence_summary_json(workspace: &Path, limit: u32) -> String {
    stable_json(&collect_verification_evidence_summary(workspace, limit))
}

fn memory_drift_summary_json(workspace: &Path, limit: u32) -> String {
    stable_json(&collect_memory_drift_support_summary(workspace, limit))
}

fn collect_memory_drift_support_summary(workspace: &Path, limit: u32) -> Value {
    let database_path = workspace.join(".ee").join("ee.db");
    if !support_bundle_database_path_is_regular(&database_path) {
        return super::memory_drift::memory_drift_support_summary_unavailable(
            "database_unavailable",
            "memory_drift_source_unverifiable",
            "Memory drift summary is unavailable because the workspace database is missing or unsafe to read.",
        );
    }

    let options = super::memory_drift::MemoryDriftReportOptions {
        database_path: &database_path,
        workspace_path: workspace,
        mode: super::memory_drift::MemoryDriftReportMode::RecentPackItems,
        memory_id: None,
        limit,
        include_tombstoned: false,
    };

    match super::memory_drift::build_memory_drift_report_read_only(&options) {
        Ok(report) => super::memory_drift::memory_drift_support_summary_from_report(&report),
        Err(_) => super::memory_drift::memory_drift_support_summary_unavailable(
            "report_unavailable",
            "memory_drift_source_unverifiable",
            "Memory drift summary is unavailable because the read-only report could not be built.",
        ),
    }
}

fn collect_verification_evidence_summary(workspace: &Path, limit: u32) -> Value {
    let database_path = workspace.join(".ee").join("ee.db");
    let mut database = json!({
        "path": ".ee/ee.db",
        "present": database_path.exists(),
        "readable": false,
        "workspaceMatched": false,
        "queriedAuditRows": 0,
        "workspaceAuditRows": 0,
        "malformedCount": 0,
        "summarizedRecordCount": 0,
    });

    if !support_bundle_database_path_is_regular(&database_path) {
        return verification_evidence_summary_value("database_unavailable", database, Vec::new());
    }

    let Ok(connection) = DbConnection::open_file(&database_path) else {
        return verification_evidence_summary_value("database_unreadable", database, Vec::new());
    };
    database["readable"] = json!(true);

    let workspace_key = workspace.to_string_lossy();
    let Ok(Some(workspace_row)) = connection.get_workspace_by_path(&workspace_key) else {
        return verification_evidence_summary_value("workspace_missing", database, Vec::new());
    };
    database["workspaceMatched"] = json!(true);

    let max_records = usize::try_from(limit)
        .unwrap_or(usize::MAX)
        .min(MAX_VERIFICATION_EVIDENCE_SUMMARY_RECORDS);
    let query_limit = limit
        .saturating_mul(4)
        .max(limit)
        .max(MAX_VERIFICATION_EVIDENCE_SUMMARY_RECORDS as u32);
    let Ok(entries) =
        connection.list_audit_by_action(audit_actions::VERIFICATION_INGEST, Some(query_limit))
    else {
        return verification_evidence_summary_value("audit_query_failed", database, Vec::new());
    };
    database["queriedAuditRows"] = json!(entries.len());

    let mut records = Vec::new();
    for entry in entries
        .into_iter()
        .filter(|entry| entry.workspace_id.as_deref() == Some(workspace_row.id.as_str()))
    {
        increment_json_count(&mut database, "workspaceAuditRows");
        match summarize_verification_evidence_audit_entry(&entry) {
            Some(summary) if records.len() < max_records => records.push(summary),
            Some(_) => {}
            None => increment_json_count(&mut database, "malformedCount"),
        }
    }

    records.sort_by(|left, right| {
        left.pointer("/auditId")
            .and_then(Value::as_str)
            .cmp(&right.pointer("/auditId").and_then(Value::as_str))
            .then_with(|| {
                left.pointer("/verificationId")
                    .and_then(Value::as_str)
                    .cmp(&right.pointer("/verificationId").and_then(Value::as_str))
            })
    });
    database["summarizedRecordCount"] = json!(records.len());

    verification_evidence_summary_value("available", database, records)
}

fn verification_evidence_summary_value(
    status: &str,
    database: Value,
    records: Vec<Value>,
) -> Value {
    let status_counts = coordination_fallback_counts(&records, "/status");
    let result_class_counts = coordination_fallback_counts(&records, "/resultClass");
    let offload_tool_counts = coordination_fallback_counts(&records, "/offload/tool");

    json!({
        "schema": "ee.support_bundle.verification_evidence_summary.v1",
        "sourceSchema": VERIFICATION_EVIDENCE_SCHEMA_V1,
        "source": ".ee/ee.db audit_log action=verification.ingest details",
        "status": status,
        "redactionStatus": "ids_hashes_status_counts_only_no_raw_commands_no_raw_output_tails",
        "limits": {
            "maxRecords": MAX_VERIFICATION_EVIDENCE_SUMMARY_RECORDS,
        },
        "database": database,
        "statusCounts": status_counts,
        "resultClassCounts": result_class_counts,
        "offloadToolCounts": offload_tool_counts,
        "records": records,
    })
}

fn summarize_verification_evidence_audit_entry(entry: &StoredAuditEntry) -> Option<Value> {
    let details = serde_json::from_str::<Value>(entry.details.as_deref()?).ok()?;
    let details_schema = details.get("schema").and_then(Value::as_str)?;
    let (record_value, ledger_content_hash) = match details_schema {
        super::verify::VERIFICATION_LEDGER_ENTRY_SCHEMA_V1 => (
            details.get("evidence")?.clone(),
            details
                .get("contentHash")
                .and_then(Value::as_str)
                .map(str::to_owned),
        ),
        VERIFICATION_EVIDENCE_SCHEMA_V1 => (details.clone(), None),
        _ => return None,
    };
    let record = serde_json::from_value::<VerificationEvidenceRecord>(record_value).ok()?;
    if record.schema != VERIFICATION_EVIDENCE_SCHEMA_V1 {
        return None;
    }
    let content_hash = ledger_content_hash.or_else(|| {
        serde_json::to_string(&record)
            .ok()
            .map(|serialized| blake3_text_hash(&serialized))
    });

    Some(json!({
        "auditId": verification_summary_label(&entry.id),
        "targetType": verification_optional_summary_label(entry.target_type.as_deref()),
        "targetId": verification_optional_summary_label(entry.target_id.as_deref()),
        "verificationId": verification_summary_label(&record.verification_id),
        "beadId": verification_optional_summary_label(record.bead_id.as_deref()),
        "gateName": verification_summary_label(&record.gate_name),
        "status": record.status.as_str(),
        "resultClass": verification_evidence_support_result_class(&record),
        "commandHash": verification_summary_label(&record.command_hash),
        "contentHash": content_hash,
        "exitCode": record.exit_code,
        "startedAt": record.started_at.as_deref(),
        "finishedAt": record.finished_at.as_deref(),
        "durationMs": record.duration_ms,
        "offload": {
            "tool": verification_optional_summary_label(record.offload.offload_tool.as_deref()),
            "remoteRequired": record.offload.required_remote,
            "worker": verification_optional_summary_label(record.offload.worker.as_deref()),
            "fallbackDetected": record.offload.fallback_detected,
            "fallbackReasonHash": record.offload.fallback_reason.as_deref().map(blake3_text_hash),
        },
        "workspaceFingerprint": verification_optional_summary_label(
            record.environment.workspace_fingerprint.as_deref(),
        ),
        "artifactCount": record.artifacts.len(),
        "beadsSummary": verification_evidence_beads_summary(&record),
        "rawCommandIncluded": false,
        "rawOutputIncluded": false,
    }))
}

fn verification_evidence_support_result_class(record: &VerificationEvidenceRecord) -> &'static str {
    match record.status {
        VerificationStatus::Passed if record.is_authoritative_pass() => "authoritative_pass",
        VerificationStatus::Passed => "non_authoritative_pass",
        VerificationStatus::Failed => "code_failure",
        VerificationStatus::Blocked => "environment_blocker",
        VerificationStatus::FallbackDetected if record.offload.required_remote => {
            "environment_blocker"
        }
        VerificationStatus::FallbackDetected => "fallback_detected",
        VerificationStatus::Interrupted => "interrupted",
        VerificationStatus::Unknown => "unknown",
    }
}

fn verification_optional_summary_label(value: Option<&str>) -> Option<String> {
    value.map(verification_summary_label)
}

fn verification_summary_label(value: &str) -> String {
    let sanitized: String = value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, ':' | '_' | '-' | '.' | '/' | '=' | ',') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "empty".to_owned()
    } else {
        sanitized
    }
}

fn support_bundle_database_path_is_regular(database_path: &Path) -> bool {
    if reject_existing_symlink_component(database_path, "support bundle database").is_err() {
        return false;
    }
    fs::symlink_metadata(database_path)
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false)
}

fn planned_files() -> Vec<String> {
    vec![
        STATUS_FILE.to_owned(),
        DOCTOR_FILE.to_owned(),
        AUDIT_FILE.to_owned(),
        VERIFICATION_EVIDENCE_SUMMARY_FILE.to_owned(),
        MEMORY_DRIFT_SUMMARY_FILE.to_owned(),
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
        SWARM_REPLAY_SUMMARY_FILE.to_owned(),
        SWARM_BRIEF_SUMMARY_FILE.to_owned(),
        SWARM_INCIDENT_SUMMARY_FILE.to_owned(),
        COORDINATION_FALLBACK_SUMMARY_FILE.to_owned(),
        SINGLEFLIGHT_POSTURE_FILE.to_owned(),
        QOS_LANE_SUMMARY_FILE.to_owned(),
        TRIAGE_SUMMARY_FILE.to_owned(),
        LOCAL_CARGO_TRIPWIRE_FILE.to_owned(),
        REGRESSION_CAUSALITY_SUMMARY_FILE.to_owned(),
        ENVIRONMENT_ATTESTATION_SUMMARY_FILE.to_owned(),
        MANIFEST_FILE.to_owned(),
    ]
}

fn generate_bundle_id() -> String {
    let now = Utc::now();
    let sequence = SUPPORT_BUNDLE_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}_{:09}_p{}_{}",
        now.format("%Y%m%d_%H%M%S"),
        now.timestamp_subsec_nanos(),
        std::process::id(),
        sequence
    )
}

fn write_file_with_hash(path: &Path, content: &str) -> Result<u64, DomainError> {
    reject_existing_symlink_component(path, "support bundle file")?;
    ensure_support_bundle_file_final_path_absent(path)?;
    let temp_path = support_bundle_temp_path(path)?;
    ensure_support_bundle_file_temp_path_absent(&temp_path)?;

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .map_err(|e| DomainError::Storage {
            message: format!(
                "Failed to create temporary support bundle file {}: {e}",
                temp_path.display()
            ),
            repair: None,
        })?;
    file.write_all(content.as_bytes())
        .map_err(|e| DomainError::Storage {
            message: format!(
                "Failed to write temporary support bundle file {}: {e}",
                temp_path.display()
            ),
            repair: None,
        })?;
    file.sync_data().map_err(|e| DomainError::Storage {
        message: format!(
            "Failed to sync temporary support bundle file {}: {e}",
            temp_path.display()
        ),
        repair: None,
    })?;
    drop(file);

    publish_support_bundle_temp_file(&temp_path, path)?;

    Ok(content.len() as u64)
}

fn publish_support_bundle_temp_file(temp_path: &Path, path: &Path) -> Result<(), DomainError> {
    reject_existing_symlink_component(path, "support bundle file")?;
    ensure_support_bundle_file_final_path_absent(path)?;
    reject_existing_symlink_component(temp_path, "support bundle temp file")?;
    ensure_support_bundle_created_temp_path_regular(temp_path)?;
    fs::rename(temp_path, path).map_err(|e| DomainError::Storage {
        message: format!(
            "Failed to publish temporary support bundle file {} to {}: {e}",
            temp_path.display(),
            path.display()
        ),
        repair: None,
    })
}

fn create_support_bundle_directory(
    output_dir: &Path,
    bundle_dir: &Path,
) -> Result<(), DomainError> {
    reject_existing_symlink_component(output_dir, "support bundle output root")?;
    fs::create_dir_all(output_dir).map_err(|e| DomainError::Storage {
        message: format!(
            "Failed to create support bundle output directory {}: {e}",
            output_dir.display()
        ),
        repair: Some("Check write permissions on output directory".to_string()),
    })?;
    reject_existing_symlink_component(output_dir, "support bundle output root")?;
    reject_existing_symlink_component(bundle_dir, "support bundle output")?;
    fs::create_dir(bundle_dir).map_err(|e| {
        let (message, repair) = if e.kind() == std::io::ErrorKind::AlreadyExists {
            (
                format!(
                    "Support bundle directory {} already exists; refusing to overwrite it.",
                    bundle_dir.display()
                ),
                "Retry support bundle creation with a fresh output directory.".to_owned(),
            )
        } else {
            (
                format!(
                    "Failed to create support bundle directory {}: {e}",
                    bundle_dir.display()
                ),
                "Check write permissions on output directory.".to_owned(),
            )
        };
        DomainError::Storage {
            message,
            repair: Some(repair),
        }
    })?;
    reject_existing_symlink_component(bundle_dir, "support bundle output")?;
    Ok(())
}

fn support_bundle_temp_path(path: &Path) -> Result<PathBuf, DomainError> {
    let Some(file_name) = path.file_name() else {
        return Err(DomainError::Storage {
            message: format!(
                "Failed to derive temporary support bundle file for {}: missing file name.",
                path.display()
            ),
            repair: None,
        });
    };
    let mut temp_name = file_name.to_os_string();
    temp_name.push(".tmp");
    Ok(path.with_file_name(temp_name))
}

fn ensure_support_bundle_file_temp_path_absent(path: &Path) -> Result<(), DomainError> {
    reject_existing_symlink_component(path, "support bundle temp file")?;
    match fs::symlink_metadata(path) {
        Ok(_) => Err(DomainError::Storage {
            message: format!(
                "Refusing to create support bundle temp file {} because it already exists.",
                path.display()
            ),
            repair: Some(
                "Remove the stale support bundle temp file or choose a fresh output directory."
                    .to_owned(),
            ),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "Failed to inspect support bundle temp file {} before create: {error}",
                path.display()
            ),
            repair: Some("Check support bundle temp path permissions.".to_owned()),
        }),
    }
}

fn ensure_support_bundle_created_temp_path_regular(path: &Path) -> Result<(), DomainError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(DomainError::Storage {
            message: format!(
                "Refusing to publish support bundle temp file {} because it is not a regular file.",
                path.display()
            ),
            repair: Some("Remove the stale support bundle temp path and retry.".to_owned()),
        }),
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "Failed to inspect support bundle temp file {} before publish: {error}",
                path.display()
            ),
            repair: Some("Check support bundle temp path permissions.".to_owned()),
        }),
    }
}

fn ensure_support_bundle_file_final_path_absent(path: &Path) -> Result<(), DomainError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Err(DomainError::Storage {
            message: format!(
                "Refusing to create support bundle file {} because it already exists.",
                path.display()
            ),
            repair: Some(
                "Choose a fresh support bundle output directory or remove the stale file."
                    .to_owned(),
            ),
        }),
        Ok(_) => Err(DomainError::Storage {
            message: format!(
                "Refusing to create support bundle file {} because the final path is not a regular file.",
                path.display()
            ),
            repair: Some(
                "Remove the non-regular support bundle path or choose a fresh output directory."
                    .to_owned(),
            ),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "Failed to inspect support bundle file {} before create: {error}",
                path.display()
            ),
            repair: Some("Check support bundle path permissions.".to_owned()),
        }),
    }
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
    let metadata = fs::symlink_metadata(path).map_err(|error| DomainError::Storage {
        message: format!(
            "Failed to inspect support bundle file {}: {error}",
            path.display()
        ),
        repair: Some("Check support bundle file permissions.".to_owned()),
    })?;
    if !metadata.file_type().is_file() {
        return Err(DomainError::Storage {
            message: format!(
                "Support bundle file is not a regular non-symlink file: {}.",
                path.display()
            ),
            repair: Some("Regenerate the support bundle.".to_owned()),
        });
    }
    if metadata.len() > MAX_SUPPORT_BUNDLE_INSPECT_FILE_BYTES {
        return Err(DomainError::Storage {
            message: format!(
                "Support bundle file {} exceeds the {}-byte inspect read cap.",
                path.display(),
                MAX_SUPPORT_BUNDLE_INSPECT_FILE_BYTES
            ),
            repair: Some("Regenerate the support bundle without oversized files.".to_owned()),
        });
    }

    let file = open_support_bundle_file_for_read_no_follow(path)?;
    let opened_metadata = file.metadata().map_err(|error| DomainError::Storage {
        message: format!(
            "Failed to inspect opened support bundle file {}: {error}",
            path.display()
        ),
        repair: Some("Check support bundle file permissions.".to_owned()),
    })?;
    if !opened_metadata.file_type().is_file() {
        return Err(DomainError::Storage {
            message: format!(
                "Support bundle file is not a regular non-symlink file after open: {}.",
                path.display()
            ),
            repair: Some("Regenerate the support bundle.".to_owned()),
        });
    }
    if opened_metadata.len() > MAX_SUPPORT_BUNDLE_INSPECT_FILE_BYTES {
        return Err(DomainError::Storage {
            message: format!(
                "Support bundle file {} exceeded the {}-byte inspect read cap after open.",
                path.display(),
                MAX_SUPPORT_BUNDLE_INSPECT_FILE_BYTES
            ),
            repair: Some("Regenerate the support bundle without oversized files.".to_owned()),
        });
    }

    let mut content = String::new();
    file.take(MAX_SUPPORT_BUNDLE_INSPECT_FILE_BYTES.saturating_add(1))
        .read_to_string(&mut content)
        .map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to read support bundle file {}: {error}",
                path.display()
            ),
            repair: Some("Check support bundle file permissions.".to_owned()),
        })?;
    if u64::try_from(content.len()).unwrap_or(u64::MAX) > MAX_SUPPORT_BUNDLE_INSPECT_FILE_BYTES {
        return Err(DomainError::Storage {
            message: format!(
                "Support bundle file {} exceeded the {}-byte inspect read cap while reading.",
                path.display(),
                MAX_SUPPORT_BUNDLE_INSPECT_FILE_BYTES
            ),
            repair: Some("Regenerate the support bundle without oversized files.".to_owned()),
        });
    }
    Ok(content)
}

fn open_support_bundle_file_for_read_no_follow(path: &Path) -> Result<fs::File, DomainError> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    configure_support_bundle_open_no_follow(&mut options);
    options.open(path).map_err(|error| DomainError::Storage {
        message: format!(
            "Failed to read support bundle file {}: {error}",
            path.display()
        ),
        repair: Some("Check support bundle file permissions.".to_owned()),
    })
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn configure_support_bundle_open_no_follow(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn configure_support_bundle_open_no_follow(_options: &mut fs::OpenOptions) {}

fn compute_hash(content: &str) -> String {
    let mut hasher = Hasher::new();
    hasher.update(content.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// Cooperative QoS throttle decision for support-bundle collection
/// (bd-1zb7k.20.3). Routes the shared `decide_background_throttle`
/// policy through the support-bundle context so the bundle path
/// participates in the same advisory active-lane registry as steward
/// jobs, instead of inventing a parallel budget system.
///
/// Lane is fixed to `QosLane::BackgroundDerived` because the support
/// bundle collection is derived-asset work; checkpoint is fixed to
/// `BeforeExpensivePhase` because the bundle's expensive phase is the
/// `collect_diagnostics` call that follows. Returns the decision so
/// callers can render a `qosThrottle` block in their bundle report
/// only when behavior changed (preserves output-hash determinism when
/// no throttling decision affected selection — see the
/// throttle_decision_continues_under_no_foreground_pressure test).
///
/// `remaining_item_budget` is the number of bundle items the caller
/// was about to collect; `minimum_item_budget` is the smallest
/// honest collection size the caller will accept rather than skip.
/// `may_yield` should be `false` for the support bundle because the
/// command is foreground-initiated and yielding would surface as a
/// stuck command to the operator — under foreground pressure the
/// shared helper falls back to `ShrinkItemBudget` instead, which is
/// what the support bundle wants.
#[must_use]
pub fn support_bundle_qos_throttle_decision(
    summary: &QosLaneSummary,
    remaining_item_budget: u32,
    minimum_item_budget: u32,
) -> QosBackgroundThrottleDecision {
    decide_background_throttle(
        summary,
        QosBackgroundThrottleInput {
            lane: QosLane::BackgroundDerived,
            checkpoint: QosThrottleCheckpoint::BeforeExpensivePhase,
            remaining_item_budget,
            minimum_item_budget,
            may_yield: false,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::environment_attestation::{
        ENVIRONMENT_ATTESTATION_REDACTION_STATUS, ENVIRONMENT_ATTESTATION_SCHEMA_V1,
        EnvironmentAttestationAuthority, EnvironmentAttestationCommandAction,
        EnvironmentAttestationCommandCopySafety, EnvironmentAttestationDegradation,
        EnvironmentAttestationDegradedCode, EnvironmentAttestationFreshness,
        EnvironmentAttestationMetric, EnvironmentAttestationRecoveryAction,
        EnvironmentAttestationRecoveryKind, EnvironmentAttestationReport,
        EnvironmentAttestationSourceAuthorityEntry, EnvironmentAttestationSourceKind,
        EnvironmentAttestationSourceStatus, EnvironmentAttestationSourceTestVerdict,
        EnvironmentAttestationSubstrate, EnvironmentAttestationSummary,
        EnvironmentAttestationVerdict,
    };
    use crate::core::qos::QosBackgroundThrottleAction;

    type TestResult = Result<(), String>;
    const SUPPORT_BUNDLE_ATTESTATION_SUMMARY_SCHEMA_TEXT: &str = include_str!(
        "../../docs/schemas/ee.support_bundle.environment_attestation_summary.v1.json"
    );
    const SUPPORT_BUNDLE_ATTESTATION_SUMMARY_MATRIX_GOLDEN: &str = include_str!(
        "../../tests/fixtures/golden/environment_attestation/support_bundle_summary_matrix.json.golden"
    );
    const ATTESTATION_SUMMARY_DENIED_SUBSTRINGS: &[&str] = &[
        "body_md",
        "raw mail body",
        "raw-body",
        "raw source snippet",
        "BEGIN PRIVATE KEY",
        "ghp_",
        "Bearer ",
        "DATABASE_URL=",
        "/Users/",
        "/Volumes/",
        "/private/",
    ];

    fn empty_qos_summary() -> QosLaneSummary {
        QosLaneSummary {
            schema: "ee.qos.lane_summary.v1".to_owned(),
            workspace_hash: "blake3:abc".to_owned(),
            active_records: Vec::new(),
            foreground_active_count: 0,
            background_active_count: 0,
            verification_active_count: 0,
            maintenance_active_count: 0,
            stale_ignored_count: 0,
            degraded: Vec::new(),
        }
    }

    #[test]
    fn environment_attestation_summary_redacts_paths_and_separates_proof_admission() {
        let recovery = EnvironmentAttestationRecoveryAction {
            priority: 0,
            kind: EnvironmentAttestationRecoveryKind::RepairEnvironment,
            command: Some(EnvironmentAttestationCommandAction {
                display_command:
                    "rch status --config /Users/jemanuel/projects/eidetic_engine_cli/rch.toml"
                        .to_owned(),
                argv: vec![
                    "rch".to_owned(),
                    "status".to_owned(),
                    "--config".to_owned(),
                    "/Users/jemanuel/projects/eidetic_engine_cli/rch.toml".to_owned(),
                ],
                shell_required: false,
                copy_safety: EnvironmentAttestationCommandCopySafety::DisplayOnly,
            }),
            mutates_state: false,
            required_substrate: EnvironmentAttestationSubstrate::Rch,
            rationale: "Inspect /Users/jemanuel/projects/eidetic_engine_cli before retrying RCH."
                .to_owned(),
        };
        let report = EnvironmentAttestationReport {
            schema: ENVIRONMENT_ATTESTATION_SCHEMA_V1,
            attestation_id: "environment_attestation_test".to_owned(),
            workspace: "/Users/jemanuel/projects/eidetic_engine_cli".to_owned(),
            generated_at: Utc::now(),
            redaction_status: ENVIRONMENT_ATTESTATION_REDACTION_STATUS,
            summary: EnvironmentAttestationSummary {
                safe_to_claim: false,
                remote_verification_admitted: Some(false),
                source_test_verdict:
                    EnvironmentAttestationSourceTestVerdict::EnvironmentBlockedBeforeSource,
                environment_verdict: EnvironmentAttestationVerdict::ProofEnvironmentBlocked,
                local_cargo_fallback_observed: false,
            },
            source_authority: vec![EnvironmentAttestationSourceAuthorityEntry {
                source: EnvironmentAttestationSourceKind::Rch,
                authority: EnvironmentAttestationAuthority::Degraded,
                status: EnvironmentAttestationSourceStatus::RemoteBlocked,
                freshness: EnvironmentAttestationFreshness::Current,
                observed_at: Some("2026-06-05T01:00:00Z".to_owned()),
                summary: "RCH topology blocked for /Users/jemanuel/projects/eidetic_engine_cli."
                    .to_owned(),
                evidence_refs: vec![
                    "agent-mail://thread/raw-body?workspace=/Users/jemanuel/projects/eidetic_engine_cli"
                        .to_owned(),
                ],
                metrics: vec![EnvironmentAttestationMetric {
                    name: "blocked_worker_count".to_owned(),
                    value: "1".to_owned(),
                }],
                degraded_codes: vec![EnvironmentAttestationDegradedCode::RchWorkerTopologyBlocked],
                recovery_actions: vec![recovery.clone()],
            }],
            verdict: EnvironmentAttestationVerdict::ProofEnvironmentBlocked,
            evidence_refs: vec![
                "rch://proof?workspace=/Users/jemanuel/projects/eidetic_engine_cli".to_owned(),
            ],
            recovery_actions: vec![recovery],
            degraded: vec![EnvironmentAttestationDegradation {
                code: EnvironmentAttestationDegradedCode::RchWorkerTopologyBlocked,
                severity: "high",
                message:
                    "Remote verification was blocked before Cargo at /Users/jemanuel/projects."
                        .to_owned(),
                repair: Some(
                    "rch status --config /Users/jemanuel/projects/eidetic_engine_cli/rch.toml"
                        .to_owned(),
                ),
            }],
        };

        let summary = environment_attestation_summary_from_report(&report);
        let rendered = stable_json(&summary);

        assert_eq!(
            summary.get("schema").and_then(Value::as_str),
            Some(SUPPORT_BUNDLE_ENVIRONMENT_ATTESTATION_SUMMARY_SCHEMA_V1)
        );
        assert_eq!(
            summary
                .pointer("/proofAdmission/remoteVerificationAdmitted")
                .and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            summary
                .pointer("/proofAdmission/sourceTestVerdict")
                .and_then(Value::as_str),
            Some("environment_blocked_before_source")
        );
        assert_eq!(
            summary
                .pointer("/sourceAuthority/0/status")
                .and_then(Value::as_str),
            Some("remote_blocked")
        );
        assert_eq!(
            summary
                .pointer("/firstFailure/code")
                .and_then(Value::as_str),
            Some("rch_worker_topology_blocked")
        );
        assert_eq!(
            summary
                .pointer("/redaction/rawMailBodiesIncluded")
                .and_then(Value::as_bool),
            Some(false)
        );
        assert!(
            summary
                .pointer("/sourceAuthority/0/evidenceRefHashes/0")
                .and_then(Value::as_str)
                .is_some_and(|hash| hash.starts_with("blake3:"))
        );
        assert!(
            summary
                .pointer("/recoveryActions/0/command/argvHash")
                .and_then(Value::as_str)
                .is_some_and(|hash| hash.starts_with("blake3:"))
        );
        assert!(!rendered.contains("/Users/jemanuel"));
        assert!(!rendered.contains("raw-body"));
    }

    #[test]
    fn environment_attestation_summary_projects_ci_proof_lane_artifact_authority() {
        let report = attestation_report(
            "ci_proof_lane_stale_artifact",
            EnvironmentAttestationSummary {
                safe_to_claim: false,
                remote_verification_admitted: None,
                source_test_verdict: EnvironmentAttestationSourceTestVerdict::StaleSource,
                environment_verdict: EnvironmentAttestationVerdict::SourceAuthorityAmbiguous,
                local_cargo_fallback_observed: false,
            },
            EnvironmentAttestationVerdict::SourceAuthorityAmbiguous,
            vec![ci_proof_lane_stale_source_entry()],
            vec![attestation_degradation(
                EnvironmentAttestationDegradedCode::CiProofLaneArtifactStale,
                "warning",
                "CI proof-lane artifact source SHA is stale relative to requested head SHA.",
                None,
            )],
        );

        let summary = environment_attestation_summary_from_report(&report);
        let source = summary
            .pointer("/sourceAuthority/0")
            .and_then(Value::as_object)
            .expect("source authority summary is present");
        let metrics = source
            .get("metrics")
            .and_then(Value::as_array)
            .expect("ci proof lane metrics are present");
        let rendered = stable_json(&summary);

        assert_eq!(
            summary
                .pointer("/firstFailure/code")
                .and_then(Value::as_str),
            Some("ci_proof_lane_artifact_stale")
        );
        assert_eq!(
            summary
                .pointer("/disagreementEvidence/ciProofLaneArtifactStale")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            summary
                .pointer("/disagreementEvidence/claimGateNeedsFreshRun")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert!(metrics.iter().any(|metric| {
            metric.pointer("/name").and_then(Value::as_str) == Some("workflow_path")
                && metric.pointer("/value").and_then(Value::as_str)
                    == Some(".github/workflows/macos-ee-artifact.yml")
        }));
        assert!(metrics.iter().any(|metric| {
            metric.pointer("/name").and_then(Value::as_str) == Some("first_failure_diagnosis")
                && metric
                    .pointer("/value")
                    .and_then(Value::as_str)
                    .is_some_and(|value| {
                        value.contains("older than the requested repository head SHA")
                    })
        }));
        assert!(!rendered.contains("stdout:"));
        assert!(!rendered.contains("stderr:"));
        assert!(!rendered.contains("/Users/"));
    }

    #[test]
    fn environment_attestation_summary_matrix_matches_schema_and_golden() -> TestResult {
        let schema: Value = serde_json::from_str(SUPPORT_BUNDLE_ATTESTATION_SUMMARY_SCHEMA_TEXT)
            .map_err(|error| error.to_string())?;
        let mut cases = Vec::new();

        for (name, report) in support_bundle_attestation_summary_cases() {
            let summary = environment_attestation_summary_from_report(&report);
            validate_support_bundle_attestation_summary_schema(&summary, &schema, name)?;
            assert_no_attestation_summary_denied_substrings(name, &summary)?;
            cases.push(compact_support_bundle_attestation_summary_case(
                name, &summary,
            )?);
        }

        let matrix = json!({
            "schema": "ee.support_bundle.environment_attestation_summary.fixture_matrix.v1",
            "cases": cases,
        });
        let rendered =
            serde_json::to_string_pretty(&matrix).map_err(|error| error.to_string())? + "\n";
        if rendered != SUPPORT_BUNDLE_ATTESTATION_SUMMARY_MATRIX_GOLDEN {
            return Err(format!(
                "support-bundle environment attestation summary golden drifted\n--- expected\n{}--- actual\n{}",
                SUPPORT_BUNDLE_ATTESTATION_SUMMARY_MATRIX_GOLDEN, rendered
            ));
        }

        Ok(())
    }

    fn support_bundle_attestation_summary_cases()
    -> Vec<(&'static str, EnvironmentAttestationReport)> {
        vec![
            (
                "clean_remote_ready",
                attestation_report(
                    "clean_remote_ready",
                    EnvironmentAttestationSummary {
                        safe_to_claim: true,
                        remote_verification_admitted: Some(true),
                        source_test_verdict: EnvironmentAttestationSourceTestVerdict::NotEvaluated,
                        environment_verdict:
                            EnvironmentAttestationVerdict::RemoteVerificationAdmitted,
                        local_cargo_fallback_observed: false,
                    },
                    EnvironmentAttestationVerdict::RemoteVerificationAdmitted,
                    vec![
                        attestation_source_entry(
                            EnvironmentAttestationSourceKind::SourceTree,
                            EnvironmentAttestationAuthority::Authoritative,
                            EnvironmentAttestationSourceStatus::Ok,
                            EnvironmentAttestationFreshness::Current,
                            "Source tree is clean for claim-gate evaluation.",
                            vec![],
                            vec![],
                        ),
                        attestation_source_entry(
                            EnvironmentAttestationSourceKind::BeadsTracker,
                            EnvironmentAttestationAuthority::Authoritative,
                            EnvironmentAttestationSourceStatus::Ok,
                            EnvironmentAttestationFreshness::Current,
                            "Beads tracker is current.",
                            vec![],
                            vec![],
                        ),
                        attestation_source_entry(
                            EnvironmentAttestationSourceKind::AgentMailProbe,
                            EnvironmentAttestationAuthority::Authoritative,
                            EnvironmentAttestationSourceStatus::Ok,
                            EnvironmentAttestationFreshness::Current,
                            "Agent Mail probe and MCP state agree.",
                            vec![],
                            vec![],
                        ),
                        attestation_source_entry(
                            EnvironmentAttestationSourceKind::Rch,
                            EnvironmentAttestationAuthority::Authoritative,
                            EnvironmentAttestationSourceStatus::RemoteReady,
                            EnvironmentAttestationFreshness::Current,
                            "RCH remote-only verification is admitted.",
                            vec![],
                            vec![],
                        ),
                    ],
                    vec![],
                ),
            ),
            (
                "unsafe_reservation_conflict",
                attestation_report(
                    "unsafe_reservation_conflict",
                    EnvironmentAttestationSummary {
                        safe_to_claim: false,
                        remote_verification_admitted: None,
                        source_test_verdict: EnvironmentAttestationSourceTestVerdict::NotEvaluated,
                        environment_verdict: EnvironmentAttestationVerdict::UnsafeDueToConflict,
                        local_cargo_fallback_observed: false,
                    },
                    EnvironmentAttestationVerdict::UnsafeDueToConflict,
                    vec![attestation_source_entry(
                        EnvironmentAttestationSourceKind::FileReservations,
                        EnvironmentAttestationAuthority::Advisory,
                        EnvironmentAttestationSourceStatus::Blocked,
                        EnvironmentAttestationFreshness::Current,
                        "An active exclusive reservation overlaps the candidate file surface.",
                        vec![EnvironmentAttestationDegradedCode::ReservationEvidenceStale],
                        vec![attestation_recovery(
                            0,
                            EnvironmentAttestationRecoveryKind::Coordinate,
                            EnvironmentAttestationSubstrate::AgentMail,
                            None,
                            "Coordinate with the reservation holder before claiming overlapping work.",
                        )],
                    )],
                    vec![attestation_degradation(
                        EnvironmentAttestationDegradedCode::ReservationEvidenceStale,
                        "medium",
                        "Reservation evidence requires coordination before claim.",
                        None,
                    )],
                ),
            ),
            (
                "stale_tracker_and_bv",
                attestation_report(
                    "stale_tracker_and_bv",
                    EnvironmentAttestationSummary {
                        safe_to_claim: false,
                        remote_verification_admitted: None,
                        source_test_verdict: EnvironmentAttestationSourceTestVerdict::NotEvaluated,
                        environment_verdict: EnvironmentAttestationVerdict::TrackerStale,
                        local_cargo_fallback_observed: false,
                    },
                    EnvironmentAttestationVerdict::TrackerStale,
                    vec![
                        attestation_source_entry(
                            EnvironmentAttestationSourceKind::BeadsTracker,
                            EnvironmentAttestationAuthority::Stale,
                            EnvironmentAttestationSourceStatus::Stale,
                            EnvironmentAttestationFreshness::Stale,
                            "Beads JSONL is newer than the local DB.",
                            vec![EnvironmentAttestationDegradedCode::BeadsTrackerStale],
                            vec![attestation_recovery(
                                0,
                                EnvironmentAttestationRecoveryKind::Sync,
                                EnvironmentAttestationSubstrate::Beads,
                                Some("br sync --import-only"),
                                "Import committed tracker state before making claim decisions.",
                            )],
                        ),
                        attestation_source_entry(
                            EnvironmentAttestationSourceKind::BvRecommendation,
                            EnvironmentAttestationAuthority::Degraded,
                            EnvironmentAttestationSourceStatus::Degraded,
                            EnvironmentAttestationFreshness::Current,
                            "BV recommended a Beads-blocked candidate.",
                            vec![EnvironmentAttestationDegradedCode::BvRecommendationStale],
                            vec![attestation_recovery(
                                1,
                                EnvironmentAttestationRecoveryKind::Inspect,
                                EnvironmentAttestationSubstrate::Beads,
                                Some("br show bd-37ugy --json"),
                                "Cross-check BV robot output with Beads status.",
                            )],
                        ),
                    ],
                    vec![
                        attestation_degradation(
                            EnvironmentAttestationDegradedCode::BeadsTrackerStale,
                            "high",
                            "Beads JSONL is newer than the local DB.",
                            Some("br sync --import-only"),
                        ),
                        attestation_degradation(
                            EnvironmentAttestationDegradedCode::BvRecommendationStale,
                            "warning",
                            "BV recommended work that Beads reports blocked.",
                            Some("br show bd-37ugy --json"),
                        ),
                    ],
                ),
            ),
            (
                "agent_mail_probe_mismatch",
                attestation_report(
                    "agent_mail_probe_mismatch",
                    EnvironmentAttestationSummary {
                        safe_to_claim: false,
                        remote_verification_admitted: None,
                        source_test_verdict: EnvironmentAttestationSourceTestVerdict::NotEvaluated,
                        environment_verdict: EnvironmentAttestationVerdict::CoordinateBeforeClaim,
                        local_cargo_fallback_observed: false,
                    },
                    EnvironmentAttestationVerdict::CoordinateBeforeClaim,
                    vec![attestation_source_entry(
                        EnvironmentAttestationSourceKind::AgentMailProbe,
                        EnvironmentAttestationAuthority::Contradicted,
                        EnvironmentAttestationSourceStatus::Contradicted,
                        EnvironmentAttestationFreshness::Current,
                        "Agent Mail CLI probe reported unavailable while MCP coordination worked.",
                        vec![EnvironmentAttestationDegradedCode::AgentMailProbeMismatch],
                        vec![attestation_recovery(
                            0,
                            EnvironmentAttestationRecoveryKind::Coordinate,
                            EnvironmentAttestationSubstrate::AgentMail,
                            None,
                            "Use MCP Agent Mail as the live authority and refresh the redacted probe.",
                        )],
                    )],
                    vec![attestation_degradation(
                        EnvironmentAttestationDegradedCode::AgentMailProbeMismatch,
                        "medium",
                        "Agent Mail probe authority disagrees with live MCP coordination.",
                        None,
                    )],
                ),
            ),
            (
                "ci_proof_lane_stale_artifact",
                attestation_report(
                    "ci_proof_lane_stale_artifact",
                    EnvironmentAttestationSummary {
                        safe_to_claim: false,
                        remote_verification_admitted: None,
                        source_test_verdict: EnvironmentAttestationSourceTestVerdict::StaleSource,
                        environment_verdict:
                            EnvironmentAttestationVerdict::SourceAuthorityAmbiguous,
                        local_cargo_fallback_observed: false,
                    },
                    EnvironmentAttestationVerdict::SourceAuthorityAmbiguous,
                    vec![ci_proof_lane_stale_source_entry()],
                    vec![attestation_degradation(
                        EnvironmentAttestationDegradedCode::CiProofLaneArtifactStale,
                        "warning",
                        "CI proof-lane artifact source SHA is stale relative to requested head SHA.",
                        None,
                    )],
                ),
            ),
            (
                "rch_environment_blocked",
                attestation_report(
                    "rch_environment_blocked",
                    EnvironmentAttestationSummary {
                        safe_to_claim: false,
                        remote_verification_admitted: Some(false),
                        source_test_verdict:
                            EnvironmentAttestationSourceTestVerdict::EnvironmentBlockedBeforeSource,
                        environment_verdict: EnvironmentAttestationVerdict::ProofEnvironmentBlocked,
                        local_cargo_fallback_observed: false,
                    },
                    EnvironmentAttestationVerdict::ProofEnvironmentBlocked,
                    vec![attestation_source_entry(
                        EnvironmentAttestationSourceKind::Rch,
                        EnvironmentAttestationAuthority::Degraded,
                        EnvironmentAttestationSourceStatus::RemoteBlocked,
                        EnvironmentAttestationFreshness::Current,
                        "RCH-E327 blocked before Cargo under /Users/jemanuel/projects.",
                        vec![
                            EnvironmentAttestationDegradedCode::RchWorkerTopologyBlocked,
                            EnvironmentAttestationDegradedCode::RchRemoteRequiredFallbackPrevented,
                        ],
                        vec![attestation_recovery(
                            0,
                            EnvironmentAttestationRecoveryKind::RepairEnvironment,
                            EnvironmentAttestationSubstrate::Rch,
                            Some(
                                "rch status --config /Users/jemanuel/projects/eidetic_engine_cli/rch.toml",
                            ),
                            "Repair worker topology before treating proof as source evidence.",
                        )],
                    )],
                    vec![
                        attestation_degradation(
                            EnvironmentAttestationDegradedCode::RchWorkerTopologyBlocked,
                            "high",
                            "Remote verification was blocked before Cargo by RCH topology.",
                            Some("rch status --json"),
                        ),
                        attestation_degradation(
                            EnvironmentAttestationDegradedCode::RchRemoteRequiredFallbackPrevented,
                            "high",
                            "Remote-required verification refused local fallback before source tests.",
                            None,
                        ),
                    ],
                ),
            ),
        ]
    }

    fn ci_proof_lane_stale_source_entry() -> EnvironmentAttestationSourceAuthorityEntry {
        EnvironmentAttestationSourceAuthorityEntry {
            source: EnvironmentAttestationSourceKind::CiProofLane,
            authority: EnvironmentAttestationAuthority::Stale,
            status: EnvironmentAttestationSourceStatus::Stale,
            freshness: EnvironmentAttestationFreshness::Stale,
            observed_at: Some("2026-06-05T10:15:00Z".to_owned()),
            summary: "CI proof lane artifact source SHA is stale relative to requested head SHA."
                .to_owned(),
            evidence_refs: vec![
                "ci-proof-lane://snapshot/ci_proof_lane_666666666666666666666666".to_owned(),
            ],
            metrics: vec![
                EnvironmentAttestationMetric {
                    name: "workflow_path".to_owned(),
                    value: ".github/workflows/macos-ee-artifact.yml".to_owned(),
                },
                EnvironmentAttestationMetric {
                    name: "workflow_name".to_owned(),
                    value: "macOS EE Artifact".to_owned(),
                },
                EnvironmentAttestationMetric {
                    name: "run_id".to_owned(),
                    value: "27006448051".to_owned(),
                },
                EnvironmentAttestationMetric {
                    name: "job_id".to_owned(),
                    value: "79699108969".to_owned(),
                },
                EnvironmentAttestationMetric {
                    name: "requested_head_sha".to_owned(),
                    value: "3140dbf1ea21a4d3e3de9b0f1edefd236e1b30c3".to_owned(),
                },
                EnvironmentAttestationMetric {
                    name: "run_head_sha".to_owned(),
                    value: "6afe302491b7ff5d869c42eaf57a7c9675fd5ef7".to_owned(),
                },
                EnvironmentAttestationMetric {
                    name: "artifact_name".to_owned(),
                    value: "ee-aarch64-apple-darwin-debug".to_owned(),
                },
                EnvironmentAttestationMetric {
                    name: "checksum_status".to_owned(),
                    value: "verified".to_owned(),
                },
                EnvironmentAttestationMetric {
                    name: "surface_probe_status".to_owned(),
                    value: "passed".to_owned(),
                },
                EnvironmentAttestationMetric {
                    name: "first_failure_diagnosis".to_owned(),
                    value: "artifact source SHA is older than the requested repository head SHA"
                        .to_owned(),
                },
            ],
            degraded_codes: vec![EnvironmentAttestationDegradedCode::CiProofLaneArtifactStale],
            recovery_actions: vec![attestation_recovery(
                0,
                EnvironmentAttestationRecoveryKind::Coordinate,
                EnvironmentAttestationSubstrate::AgentMail,
                None,
                "Coordinate proof-lane authority before reusing the stale artifact.",
            )],
        }
    }

    fn attestation_report(
        name: &str,
        summary: EnvironmentAttestationSummary,
        verdict: EnvironmentAttestationVerdict,
        source_authority: Vec<EnvironmentAttestationSourceAuthorityEntry>,
        degraded: Vec<EnvironmentAttestationDegradation>,
    ) -> EnvironmentAttestationReport {
        let mut evidence_refs = source_authority
            .iter()
            .flat_map(|entry| entry.evidence_refs.clone())
            .collect::<Vec<_>>();
        evidence_refs.sort();
        evidence_refs.dedup();

        let mut recovery_actions = source_authority
            .iter()
            .flat_map(|entry| entry.recovery_actions.clone())
            .collect::<Vec<_>>();
        recovery_actions.sort();
        recovery_actions.dedup();

        EnvironmentAttestationReport {
            schema: ENVIRONMENT_ATTESTATION_SCHEMA_V1,
            attestation_id: format!("environment_attestation_summary_{name}"),
            workspace: "/Users/jemanuel/projects/eidetic_engine_cli".to_owned(),
            generated_at: Utc::now(),
            redaction_status: ENVIRONMENT_ATTESTATION_REDACTION_STATUS,
            summary,
            source_authority,
            verdict,
            evidence_refs,
            recovery_actions,
            degraded,
        }
    }

    fn attestation_source_entry(
        source: EnvironmentAttestationSourceKind,
        authority: EnvironmentAttestationAuthority,
        status: EnvironmentAttestationSourceStatus,
        freshness: EnvironmentAttestationFreshness,
        summary: &str,
        degraded_codes: Vec<EnvironmentAttestationDegradedCode>,
        recovery_actions: Vec<EnvironmentAttestationRecoveryAction>,
    ) -> EnvironmentAttestationSourceAuthorityEntry {
        EnvironmentAttestationSourceAuthorityEntry {
            source,
            authority,
            status,
            freshness,
            observed_at: Some("2026-06-05T02:00:00Z".to_owned()),
            summary: summary.to_owned(),
            evidence_refs: vec![format!(
                "attestation://{}?workspace=/Users/jemanuel/projects/eidetic_engine_cli",
                serialized_token(&source)
            )],
            metrics: vec![EnvironmentAttestationMetric {
                name: "item_count".to_owned(),
                value: "1".to_owned(),
            }],
            degraded_codes,
            recovery_actions,
        }
    }

    fn attestation_recovery(
        priority: u8,
        kind: EnvironmentAttestationRecoveryKind,
        required_substrate: EnvironmentAttestationSubstrate,
        command: Option<&str>,
        rationale: &str,
    ) -> EnvironmentAttestationRecoveryAction {
        EnvironmentAttestationRecoveryAction {
            priority,
            kind,
            command: command.map(|display_command| EnvironmentAttestationCommandAction {
                display_command: display_command.to_owned(),
                argv: display_command
                    .split_whitespace()
                    .map(ToOwned::to_owned)
                    .collect(),
                shell_required: false,
                copy_safety: EnvironmentAttestationCommandCopySafety::DisplayOnly,
            }),
            mutates_state: false,
            required_substrate,
            rationale: rationale.to_owned(),
        }
    }

    fn attestation_degradation(
        code: EnvironmentAttestationDegradedCode,
        severity: &'static str,
        message: &str,
        repair: Option<&str>,
    ) -> EnvironmentAttestationDegradation {
        EnvironmentAttestationDegradation {
            code,
            severity,
            message: message.to_owned(),
            repair: repair.map(ToOwned::to_owned),
        }
    }

    fn validate_support_bundle_attestation_summary_schema(
        summary: &Value,
        schema: &Value,
        case_name: &str,
    ) -> TestResult {
        let object = summary
            .as_object()
            .ok_or_else(|| format!("{case_name}: summary is not an object"))?;
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .ok_or_else(|| "schema missing required list".to_owned())?;
        for field in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(field) {
                return Err(format!(
                    "{case_name}: missing schema-required field {field}"
                ));
            }
        }
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .ok_or_else(|| "schema missing properties".to_owned())?;
        for field in object.keys() {
            if !properties.contains_key(field) {
                return Err(format!("{case_name}: unexpected field {field}"));
            }
        }
        if summary.get("schema").and_then(Value::as_str)
            != Some(SUPPORT_BUNDLE_ENVIRONMENT_ATTESTATION_SUMMARY_SCHEMA_V1)
        {
            return Err(format!("{case_name}: wrong schema token"));
        }
        if summary.get("sourceSchema").and_then(Value::as_str)
            != Some(ENVIRONMENT_ATTESTATION_SCHEMA_V1)
        {
            return Err(format!("{case_name}: wrong source schema token"));
        }
        for pointer in ["/workspaceHash", "/summaryHash"] {
            require_blake3_hash(summary.pointer(pointer), case_name, pointer)?;
        }
        if summary
            .pointer("/proofAdmission/separateFromSourceTestVerdict")
            .and_then(Value::as_bool)
            != Some(true)
        {
            return Err(format!(
                "{case_name}: proof admission did not mark source-test separation"
            ));
        }
        for pointer in [
            "/redaction/rawWorkspacePathIncluded",
            "/redaction/rawMailBodiesIncluded",
            "/redaction/rawSourceSnippetsIncluded",
            "/redaction/rawCommandArgvIncluded",
            "/redaction/rawEvidenceRefsIncluded",
        ] {
            if summary.pointer(pointer).and_then(Value::as_bool) != Some(false) {
                return Err(format!(
                    "{case_name}: redaction flag {pointer} was not false"
                ));
            }
        }
        if summary
            .pointer("/redaction/hostPrivatePathsRedacted")
            .and_then(Value::as_bool)
            != Some(true)
        {
            return Err(format!(
                "{case_name}: hostPrivatePathsRedacted was not true"
            ));
        }
        let sources = summary
            .get("sourceAuthority")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{case_name}: sourceAuthority missing"))?;
        let total = summary
            .pointer("/sourceAuthorityCounts/total")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{case_name}: sourceAuthorityCounts.total missing"))?;
        if total != sources.len() as u64 {
            return Err(format!(
                "{case_name}: sourceAuthorityCounts.total did not match sourceAuthority length"
            ));
        }
        for (index, source) in sources.iter().enumerate() {
            let prefix = format!("{case_name}: sourceAuthority[{index}]");
            let evidence_hashes = source
                .get("evidenceRefHashes")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("{prefix}: evidenceRefHashes missing"))?;
            for hash in evidence_hashes {
                require_blake3_hash(Some(hash), &prefix, "evidenceRefHashes")?;
            }
            let recovery_actions = source
                .get("recoveryActions")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("{prefix}: recoveryActions missing"))?;
            let recovery_count = source
                .get("recoveryActionCount")
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("{prefix}: recoveryActionCount missing"))?;
            if recovery_count != recovery_actions.len() as u64 {
                return Err(format!("{prefix}: recoveryActionCount mismatch"));
            }
            for action in recovery_actions {
                validate_redacted_attestation_recovery_action(action, &prefix)?;
            }
        }
        for action in summary
            .get("recoveryActions")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{case_name}: recoveryActions missing"))?
        {
            validate_redacted_attestation_recovery_action(action, case_name)?;
        }
        Ok(())
    }

    fn validate_redacted_attestation_recovery_action(action: &Value, context: &str) -> TestResult {
        let Some(command) = action.get("command") else {
            return Err(format!("{context}: recovery action missing command field"));
        };
        if command.is_null() {
            return Ok(());
        }
        if command.get("argv").is_some() {
            return Err(format!("{context}: redacted command leaked argv array"));
        }
        require_blake3_hash(command.get("argvHash"), context, "argvHash")
    }

    fn require_blake3_hash(value: Option<&Value>, context: &str, field: &str) -> TestResult {
        if value
            .and_then(Value::as_str)
            .is_some_and(|hash| hash.starts_with("blake3:"))
        {
            Ok(())
        } else {
            Err(format!("{context}: {field} was not a blake3 hash"))
        }
    }

    fn assert_no_attestation_summary_denied_substrings(
        case_name: &str,
        summary: &Value,
    ) -> TestResult {
        let rendered = stable_json(summary);
        for denied in ATTESTATION_SUMMARY_DENIED_SUBSTRINGS {
            if rendered.contains(denied) {
                return Err(format!(
                    "{case_name}: support-bundle attestation summary leaked denied substring {denied:?}"
                ));
            }
        }
        Ok(())
    }

    fn compact_support_bundle_attestation_summary_case(
        case_name: &str,
        summary: &Value,
    ) -> Result<Value, String> {
        Ok(json!({
            "case": case_name,
            "verdict": summary
                .get("verdict")
                .cloned()
                .ok_or_else(|| format!("{case_name}: verdict missing"))?,
            "summary": summary
                .get("summary")
                .cloned()
                .ok_or_else(|| format!("{case_name}: summary missing"))?,
            "proofAdmission": summary
                .get("proofAdmission")
                .cloned()
                .ok_or_else(|| format!("{case_name}: proofAdmission missing"))?,
            "sourceAuthorityCounts": summary
                .get("sourceAuthorityCounts")
                .cloned()
                .ok_or_else(|| format!("{case_name}: sourceAuthorityCounts missing"))?,
            "sourceAuthority": compact_support_bundle_attestation_sources(
                summary
                    .get("sourceAuthority")
                    .and_then(Value::as_array)
                    .ok_or_else(|| format!("{case_name}: sourceAuthority missing"))?,
            ),
            "degradedCodes": summary
                .get("degradedCodes")
                .cloned()
                .ok_or_else(|| format!("{case_name}: degradedCodes missing"))?,
            "firstFailureCode": summary
                .pointer("/firstFailure/code")
                .cloned()
                .unwrap_or(Value::Null),
            "disagreementEvidence": summary
                .get("disagreementEvidence")
                .cloned()
                .ok_or_else(|| format!("{case_name}: disagreementEvidence missing"))?,
            "redaction": summary
                .get("redaction")
                .cloned()
                .ok_or_else(|| format!("{case_name}: redaction missing"))?,
        }))
    }

    fn compact_support_bundle_attestation_sources(sources: &[Value]) -> Vec<Value> {
        sources
            .iter()
            .map(|source| {
                json!({
                    "source": source["source"].clone(),
                    "authority": source["authority"].clone(),
                    "status": source["status"].clone(),
                    "freshness": source["freshness"].clone(),
                    "degradedCodes": source["degradedCodes"].clone(),
                    "recoveryActionCount": source["recoveryActionCount"].clone(),
                    "metricCount": source["metricCount"].clone(),
                })
            })
            .collect()
    }

    #[test]
    fn throttle_decision_continues_under_no_foreground_pressure() {
        let summary = empty_qos_summary();
        let decision = support_bundle_qos_throttle_decision(&summary, 64, 8);
        assert_eq!(decision.action, QosBackgroundThrottleAction::Continue);
        assert!(!decision.behavior_changed());
        assert_eq!(decision.adjusted_item_budget, None);
    }

    #[test]
    fn throttle_decision_shrinks_under_foreground_pressure() {
        let mut summary = empty_qos_summary();
        summary.foreground_active_count = 2;
        let decision = support_bundle_qos_throttle_decision(&summary, 64, 8);
        assert_eq!(
            decision.action,
            QosBackgroundThrottleAction::ShrinkItemBudget
        );
        assert!(decision.behavior_changed());
        assert_eq!(decision.adjusted_item_budget, Some(32));
        assert!(decision.foreground_pressure);
    }

    #[test]
    fn throttle_decision_fails_open_when_qos_summary_degraded() {
        let mut summary = empty_qos_summary();
        summary.foreground_active_count = 2;
        summary
            .degraded
            .push(crate::core::qos::QosRegistryDegradation::registry_unavailable("test"));
        let decision = support_bundle_qos_throttle_decision(&summary, 64, 8);
        // Bead acceptance: degraded summary fails open (continue) so the
        // support bundle never silently drops required work because the
        // registry could not be consulted.
        assert_eq!(decision.action, QosBackgroundThrottleAction::Continue);
        assert!(!decision.behavior_changed());
    }

    #[test]
    fn throttle_decision_holds_at_minimum_budget_floor() {
        let mut summary = empty_qos_summary();
        summary.foreground_active_count = 2;
        let decision = support_bundle_qos_throttle_decision(&summary, 8, 8);
        // Already at floor; the helper refuses to shrink below it.
        assert_eq!(decision.action, QosBackgroundThrottleAction::Continue);
        assert!(decision.foreground_pressure);
    }

    #[test]
    fn throttle_decision_does_not_yield_for_support_bundle() {
        // Even though the shared helper supports `Yield` at checkpoint
        // boundaries, the support-bundle wrapper pins may_yield=false
        // because yielding mid-collection would surface as a stuck
        // foreground command. The wrapper falls back to ShrinkItemBudget
        // when foreground pressure is present.
        let mut summary = empty_qos_summary();
        summary.foreground_active_count = 1;
        let decision = support_bundle_qos_throttle_decision(&summary, 100, 8);
        assert_ne!(decision.action, QosBackgroundThrottleAction::Yield);
    }

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
    fn plan_bundle_redacts_workspace_path_under_paranoid() -> TestResult {
        let options = BundleOptions {
            workspace: PathBuf::from("/Users/alice/private/support-bundle-workspace"),
            output_dir: None,
            dry_run: true,
            redacted: true,
            redaction_level: RedactionLevel::Paranoid,
            include_raw: false,
            audit_limit: 100,
        };

        let report = plan_bundle(&options).map_err(|e| e.message())?;
        assert!(
            !report.workspace_path.contains("/Users/alice"),
            "paranoid support-bundle reports must not expose raw workspace paths: {}",
            report.workspace_path
        );
        assert!(
            report
                .redaction_summary
                .reasons
                .iter()
                .any(|reason| reason == "path_like_segment"),
            "workspace-path redaction must be reflected in the redaction summary"
        );
        Ok(())
    }

    #[test]
    fn create_bundle_manifest_redacts_workspace_path_under_paranoid() -> TestResult {
        let out_dir = unique_test_path("manifest-workspace-redaction-out");
        fs::create_dir_all(&out_dir)
            .map_err(|error| format!("failed to create {}: {error}", out_dir.display()))?;
        let options = BundleOptions {
            workspace: PathBuf::from("/Users/alice/private/support-bundle-workspace"),
            output_dir: Some(out_dir),
            dry_run: false,
            redacted: true,
            redaction_level: RedactionLevel::Paranoid,
            include_raw: false,
            audit_limit: 100,
        };

        let report = create_bundle(&options).map_err(|e| e.message())?;
        assert!(
            !report.workspace_path.contains("/Users/alice"),
            "created support-bundle report must not expose raw workspace paths: {}",
            report.workspace_path
        );

        let bundle_dir = report
            .output_path
            .as_ref()
            .ok_or_else(|| "created bundle must report output path".to_owned())?;
        let manifest_text = fs::read_to_string(bundle_dir.join(MANIFEST_FILE))
            .map_err(|error| format!("failed to read manifest: {error}"))?;
        assert!(
            !manifest_text.contains("/Users/alice"),
            "support bundle manifest must not expose raw workspace paths: {manifest_text}"
        );
        let manifest: BundleManifest = serde_json::from_str(&manifest_text)
            .map_err(|error| format!("manifest must parse: {error}"))?;
        assert!(
            manifest.workspace_path.contains("REDACTED"),
            "manifest workspace path must carry a redaction placeholder: {}",
            manifest.workspace_path
        );
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

    #[cfg(unix)]
    #[test]
    fn support_bundle_database_helper_rejects_symlinked_database_path() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = unique_test_path("symlink-db-path");
        let workspace = root.join("workspace");
        let metadata_dir = workspace.join(".ee");
        fs::create_dir_all(&metadata_dir)
            .map_err(|error| format!("failed to create metadata dir: {error}"))?;
        let outside_db = root.join("outside.db");
        fs::write(&outside_db, b"not a support-bundle database")
            .map_err(|error| format!("failed to write outside db: {error}"))?;
        let database_path = metadata_dir.join("ee.db");
        symlink(&outside_db, &database_path)
            .map_err(|error| format!("failed to symlink database: {error}"))?;

        assert!(
            !support_bundle_database_path_is_regular(&database_path),
            "support bundle evidence must not follow a symlinked ee.db"
        );
        let summary = collect_pack_replay_summary(&workspace);
        assert_eq!(
            summary.pointer("/status"),
            Some(&json!("database_missing")),
            "symlinked database should be reported as unavailable to support-bundle evidence"
        );
        assert_eq!(
            summary.pointer("/database/present"),
            Some(&json!(false)),
            "symlinked database should not be reported as present"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn support_bundle_database_helper_rejects_symlinked_metadata_parent() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = unique_test_path("symlink-db-parent");
        let workspace = root.join("workspace");
        let real_metadata = root.join("real-ee");
        fs::create_dir_all(&real_metadata)
            .map_err(|error| format!("failed to create real metadata dir: {error}"))?;
        fs::create_dir_all(&workspace)
            .map_err(|error| format!("failed to create workspace: {error}"))?;
        symlink(&real_metadata, workspace.join(".ee"))
            .map_err(|error| format!("failed to symlink metadata dir: {error}"))?;
        let database_path = workspace.join(".ee").join("ee.db");

        assert!(
            !support_bundle_database_path_is_regular(&database_path),
            "support bundle evidence must not traverse a symlinked .ee parent"
        );
        assert!(
            !real_metadata.join("ee.db").exists(),
            "support bundle database probe must not create or touch the symlink target"
        );
        Ok(())
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

        for level in [
            RedactionLevel::Standard,
            RedactionLevel::Strict,
            RedactionLevel::Paranoid,
        ] {
            let redacted = redact_support_bundle_content(raw, level);
            assert!(
                redacted.redacted,
                "{level} support bundle redaction should apply the diagnostic redactor"
            );
            assert!(
                !redacted.content.contains("/Users/alice/private"),
                "{level} support bundle redaction should redact path-like segments"
            );
            assert!(
                !redacted
                    .content
                    .contains("sk-test_123456789abcdefghijklmnop"),
                "{level} support bundle redaction should redact secret-like values"
            );
            assert!(
                redacted
                    .redacted_reasons
                    .iter()
                    .any(|reason| reason == "path_like_segment"),
                "{level} support bundle redaction should report path-like redaction"
            );
        }
    }

    #[test]
    fn support_bundle_standard_redacts_tailscale_metadata_fields() -> TestResult {
        let raw = r#"{"mesh":{"tailscale":{"selfNodeKey":"nodekey:selfalpha","selfTailscaleIp":"100.64.0.10","selfMagicDnsName":"ee-local.tailnet.test.","tailnetId":"tailnet-alpha","tailnetDisplayName":"alpha.example","selfAdvertisedTags":["tag:ee-mesh","tag:memory"],"peers":[{"peerNodeKey":"nodekey:peeralpha","peerTailscaleIps":["100.64.0.20"],"peerMagicDnsName":"peer-alpha.tailnet.test.","peerHostname":"peer-alpha","peerAdvertisedTags":["tag:ee-mesh"],"online":true}],"binaryVersionRaw":"1.66.0\n  tailscale commit: abc","binaryAbsolutePath":"/opt/homebrew/bin/tailscale","probeMethod":"cli"}}}"#;

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
            "nodekey:peeralpha",
            "100.64.0.20",
            "peer-alpha.tailnet.test.",
            "peer-alpha",
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
            "source": {
                "kind": "agent_mail",
                "sourceId": "file:///Users/alice/private/agent-mail.jsonl?api_key=redaction-fixture"
            },
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
        assert!(
            summary_text.contains("[REDACTED:path]"),
            "summary should redact path-like source IDs"
        );
        assert!(
            summary_text.contains("[REDACTED:"),
            "summary should redact secret-like source IDs"
        );
        assert!(
            !summary_text.contains("/Users/alice") && !summary_text.contains("redaction-fixture"),
            "summary leaked sensitive source ID: {summary_text}"
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
        assert!(id.len() >= 28);
        assert!(id.contains("_p"));
    }

    #[test]
    fn generate_bundle_id_is_unique_within_process() {
        let first = generate_bundle_id();
        let second = generate_bundle_id();
        assert_ne!(
            first, second,
            "same-process support bundle IDs must not collide inside one timestamp bucket"
        );
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

    #[test]
    fn write_file_with_hash_rejects_non_regular_final_path() -> TestResult {
        let root = unique_test_path("write-non-regular-final");
        let file_path = root.join("bundle").join("evidence.json");
        fs::create_dir_all(&file_path)
            .map_err(|error| format!("failed to create non-regular final path: {error}"))?;

        let error = write_file_with_hash(&file_path, "{}")
            .expect_err("non-regular final path should be rejected before publish");

        assert!(
            error.message().contains("not a regular file"),
            "unexpected error: {}",
            error.message()
        );
        assert!(
            fs::symlink_metadata(&file_path)
                .map_err(|error| format!("failed to inspect final path: {error}"))?
                .file_type()
                .is_dir(),
            "support bundle writer must leave non-regular final path untouched"
        );
        Ok(())
    }

    #[test]
    fn write_file_with_hash_rejects_existing_final_without_overwriting() -> TestResult {
        let root = unique_test_path("write-existing-final");
        let file_path = root.join("bundle").join("evidence.json");
        fs::create_dir_all(
            file_path
                .parent()
                .ok_or_else(|| "file path missing parent".to_owned())?,
        )
        .map_err(|error| format!("failed to create bundle dir: {error}"))?;
        fs::write(&file_path, "existing bundle content")
            .map_err(|error| format!("failed to write existing final path: {error}"))?;

        let error = write_file_with_hash(&file_path, "{}")
            .expect_err("existing final path should reject support bundle publish");

        assert!(
            error.message().contains("already exists"),
            "unexpected error: {}",
            error.message()
        );
        assert_eq!(
            fs::read_to_string(&file_path)
                .map_err(|error| format!("failed to read existing final path: {error}"))?,
            "existing bundle content",
            "support bundle writer must leave existing final content untouched"
        );
        Ok(())
    }

    #[test]
    fn write_file_with_hash_rejects_existing_temp_without_truncating() -> TestResult {
        let root = unique_test_path("write-existing-temp");
        let file_path = root.join("bundle").join("evidence.json");
        let temp_path = support_bundle_temp_path(&file_path).map_err(|error| error.message())?;
        fs::create_dir_all(
            file_path
                .parent()
                .ok_or_else(|| "file path missing parent".to_owned())?,
        )
        .map_err(|error| format!("failed to create bundle dir: {error}"))?;
        fs::write(&temp_path, "stale temp content")
            .map_err(|error| format!("failed to write stale temp: {error}"))?;

        let error = write_file_with_hash(&file_path, "{}")
            .expect_err("existing temp path should reject support bundle publish");

        assert!(
            error.message().contains("already exists"),
            "unexpected error: {}",
            error.message()
        );
        assert_eq!(
            fs::read_to_string(&temp_path)
                .map_err(|error| format!("failed to read stale temp: {error}"))?,
            "stale temp content",
            "support bundle writer must leave existing temp content untouched"
        );
        assert!(
            !file_path.exists(),
            "final support bundle file must not be published when temp exists"
        );
        Ok(())
    }

    #[test]
    fn create_support_bundle_directory_rejects_existing_bundle_dir() -> TestResult {
        let root = unique_test_path("existing-bundle-dir");
        let output_dir = root.join("support-out");
        let bundle_dir = output_dir.join("ee_support_existing");
        fs::create_dir_all(&bundle_dir)
            .map_err(|error| format!("failed to create stale bundle dir: {error}"))?;
        let sentinel_path = bundle_dir.join(STATUS_FILE);
        fs::write(&sentinel_path, "existing status")
            .map_err(|error| format!("failed to write existing bundle sentinel: {error}"))?;

        let error = create_support_bundle_directory(&output_dir, &bundle_dir)
            .expect_err("existing bundle directory should be rejected");

        assert!(
            error.message().contains("already exists"),
            "unexpected error: {}",
            error.message()
        );
        assert_eq!(
            fs::read_to_string(&sentinel_path)
                .map_err(|error| format!("failed to read existing bundle sentinel: {error}"))?,
            "existing status",
            "support bundle directory creation must not alter existing bundle contents"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn publish_support_bundle_temp_rechecks_final_symlink_before_rename() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = unique_test_path("publish-final-symlink");
        let file_path = root.join("bundle").join("evidence.json");
        let temp_path = support_bundle_temp_path(&file_path).map_err(|error| error.message())?;
        fs::create_dir_all(
            file_path
                .parent()
                .ok_or_else(|| "file path missing parent".to_owned())?,
        )
        .map_err(|error| format!("failed to create bundle dir: {error}"))?;
        fs::write(&temp_path, "{}")
            .map_err(|error| format!("failed to write support bundle temp file: {error}"))?;
        let outside_file = root.join("outside-evidence.json");
        fs::write(&outside_file, "outside content")
            .map_err(|error| format!("failed to write outside file: {error}"))?;
        symlink(&outside_file, &file_path)
            .map_err(|error| format!("failed to symlink final path: {error}"))?;

        let error = publish_support_bundle_temp_file(&temp_path, &file_path)
            .expect_err("symlinked final support bundle path should be rejected before publish");

        assert!(
            error.message().contains("symlinked path component"),
            "unexpected error: {}",
            error.message()
        );
        assert_eq!(
            fs::read_to_string(&outside_file)
                .map_err(|error| format!("failed to read outside file: {error}"))?,
            "outside content",
            "support bundle publish must not overwrite a symlink target"
        );
        assert!(
            temp_path.is_file(),
            "temporary support bundle file should remain available for inspection"
        );
        assert!(
            fs::symlink_metadata(&file_path)
                .map_err(|error| format!("failed to inspect final path: {error}"))?
                .file_type()
                .is_symlink(),
            "final symlink should remain untouched"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn publish_support_bundle_temp_rechecks_temp_component_symlink_before_rename() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = unique_test_path("publish-temp-component-symlink");
        let bundle_dir = root.join("bundle");
        let file_path = bundle_dir.join("evidence.json");
        let outside_dir = root.join("outside-temp-dir");
        let symlinked_temp_dir = root.join("symlinked-temp-dir");
        fs::create_dir_all(&bundle_dir)
            .map_err(|error| format!("failed to create bundle dir: {error}"))?;
        fs::create_dir_all(&outside_dir)
            .map_err(|error| format!("failed to create outside temp dir: {error}"))?;
        symlink(&outside_dir, &symlinked_temp_dir)
            .map_err(|error| format!("failed to create symlinked temp dir: {error}"))?;
        let temp_path = symlinked_temp_dir.join("evidence.json.tmp");
        let outside_temp = outside_dir.join("evidence.json.tmp");
        fs::write(&outside_temp, "{}")
            .map_err(|error| format!("failed to write outside temp file: {error}"))?;

        let error = publish_support_bundle_temp_file(&temp_path, &file_path)
            .expect_err("symlinked temp component should reject support bundle publish");

        assert!(
            error.message().contains("symlinked path component"),
            "unexpected error: {}",
            error.message()
        );
        assert!(
            !file_path.exists(),
            "final support bundle file must not be published through symlinked temp component"
        );
        assert_eq!(
            fs::read_to_string(&outside_temp)
                .map_err(|error| format!("failed to read outside temp file: {error}"))?,
            "{}",
            "outside temp file must remain unchanged"
        );
        assert!(
            fs::symlink_metadata(&symlinked_temp_dir)
                .map_err(|error| format!("failed to inspect symlinked temp dir: {error}"))?
                .file_type()
                .is_symlink(),
            "symlinked temp component should remain untouched"
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

    #[test]
    fn inspect_bundle_rejects_non_regular_manifest_path() -> TestResult {
        let root = unique_test_path("inspect-non-regular-manifest");
        let bundle_dir = root.join("bundle");
        let manifest_path = bundle_dir.join(MANIFEST_FILE);
        fs::create_dir_all(&manifest_path)
            .map_err(|error| format!("failed to create non-regular manifest: {error}"))?;

        let result = inspect_bundle(&InspectOptions {
            bundle_path: bundle_dir,
            verify_hashes: true,
        });
        let error = result.expect_err("non-regular manifest should be rejected");
        assert!(
            error.message().contains("not a regular file"),
            "unexpected error: {}",
            error.message()
        );
        assert!(
            fs::symlink_metadata(&manifest_path)
                .map_err(|error| format!("failed to inspect manifest path: {error}"))?
                .file_type()
                .is_dir(),
            "inspection must not alter the non-regular manifest path"
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
    fn read_regular_file_no_symlinks_rejects_oversized_bundle_member() -> TestResult {
        let root = unique_test_path("inspect-oversized-member");
        fs::create_dir_all(&root).map_err(|error| format!("failed to create test dir: {error}"))?;
        let oversized_path = root.join("oversized.json");
        let file = fs::File::create(&oversized_path)
            .map_err(|error| format!("failed to create oversized file: {error}"))?;
        file.set_len(MAX_SUPPORT_BUNDLE_INSPECT_FILE_BYTES.saturating_add(1))
            .map_err(|error| format!("failed to size oversized file: {error}"))?;

        let error = read_regular_file_no_symlinks(&oversized_path)
            .expect_err("oversized support bundle member should be rejected before reading");

        assert!(
            error.message().contains("inspect read cap"),
            "unexpected error: {}",
            error.message()
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
    fn inspect_bundle_marks_missing_entry_invalid_without_hashes() -> TestResult {
        let root = unique_test_path("inspect-missing-entry-no-hash");
        let bundle_dir = root.join("bundle");
        fs::create_dir_all(&bundle_dir)
            .map_err(|error| format!("failed to create bundle dir: {error}"))?;

        let missing_path = "missing.json".to_owned();
        let manifest = BundleManifest {
            schema: SUPPORT_BUNDLE_MANIFEST_SCHEMA_V1.to_owned(),
            bundle_id: "test-bundle".to_owned(),
            created_at: "2026-05-16T00:00:00Z".to_owned(),
            workspace_path: "redacted-workspace".to_owned(),
            ee_version: "test".to_owned(),
            files: vec![ManifestEntry {
                path: missing_path.clone(),
                size_bytes: 2,
                content_hash: compute_hash("{}"),
                redacted: true,
            }],
            total_size_bytes: 2,
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
            verify_hashes: false,
        })
        .map_err(|error| error.message())?;

        assert!(
            !report.valid,
            "missing manifest entries must invalidate bundles even when hashes are not verified"
        );
        assert!(
            !report.hash_verified,
            "test setup must exercise structure-only inspection"
        );
        assert!(
            report.hash_mismatches.contains(&missing_path),
            "missing entry should be reported as an integrity mismatch"
        );
        assert!(
            !report.files_found.contains(&missing_path),
            "missing entry must not count as collected bundle evidence"
        );
        Ok(())
    }

    #[test]
    fn inspect_bundle_does_not_claim_hash_verification_without_manifest() -> TestResult {
        let root = unique_test_path("inspect-missing-manifest-hash-flag");
        let bundle_dir = root.join("bundle");
        fs::create_dir_all(&bundle_dir)
            .map_err(|error| format!("failed to create bundle dir: {error}"))?;
        fs::write(bundle_dir.join(STATUS_FILE), "{}")
            .map_err(|error| format!("failed to write bundle file: {error}"))?;

        let report = inspect_bundle(&InspectOptions {
            bundle_path: bundle_dir,
            verify_hashes: true,
        })
        .map_err(|error| error.message())?;

        assert!(!report.valid, "missing manifest must invalidate inspection");
        assert!(
            !report.hash_verified,
            "inspection must not claim hash verification when no manifest hashes exist"
        );
        assert!(
            report.manifest.is_none(),
            "test setup must exercise missing manifest handling"
        );
        Ok(())
    }

    #[test]
    fn inspect_bundle_rejects_unsupported_manifest_schema() -> TestResult {
        let root = unique_test_path("inspect-unsupported-manifest-schema");
        let bundle_dir = root.join("bundle");
        fs::create_dir_all(&bundle_dir)
            .map_err(|error| format!("failed to create bundle dir: {error}"))?;
        let payload = "{}";
        fs::write(bundle_dir.join(STATUS_FILE), payload)
            .map_err(|error| format!("failed to write bundle file: {error}"))?;

        let manifest = BundleManifest {
            schema: "ee.support_bundle.manifest.v0".to_owned(),
            bundle_id: "test-bundle".to_owned(),
            created_at: "2026-05-16T00:00:00Z".to_owned(),
            workspace_path: "redacted-workspace".to_owned(),
            ee_version: "test".to_owned(),
            files: vec![ManifestEntry {
                path: STATUS_FILE.to_owned(),
                size_bytes: payload.len() as u64,
                content_hash: compute_hash(payload),
                redacted: true,
            }],
            total_size_bytes: payload.len() as u64,
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

        assert!(
            !report.valid,
            "unsupported manifest schema must not validate as a current support bundle"
        );
        assert!(
            report
                .hash_mismatches
                .iter()
                .any(|mismatch| mismatch.as_str() == MANIFEST_FILE),
            "unsupported manifest schema should be reported as an integrity mismatch"
        );
        Ok(())
    }

    #[test]
    fn inspect_bundle_marks_entry_size_mismatch_invalid_without_hashes() -> TestResult {
        let root = unique_test_path("inspect-entry-size-mismatch-no-hash");
        let bundle_dir = root.join("bundle");
        fs::create_dir_all(&bundle_dir)
            .map_err(|error| format!("failed to create bundle dir: {error}"))?;
        let payload = "{}";
        fs::write(bundle_dir.join(STATUS_FILE), payload)
            .map_err(|error| format!("failed to write bundle file: {error}"))?;

        let manifest = BundleManifest {
            schema: SUPPORT_BUNDLE_MANIFEST_SCHEMA_V1.to_owned(),
            bundle_id: "test-bundle".to_owned(),
            created_at: "2026-05-16T00:00:00Z".to_owned(),
            workspace_path: "redacted-workspace".to_owned(),
            ee_version: "test".to_owned(),
            files: vec![ManifestEntry {
                path: STATUS_FILE.to_owned(),
                size_bytes: 999,
                content_hash: compute_hash(payload),
                redacted: true,
            }],
            total_size_bytes: 999,
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
            verify_hashes: false,
        })
        .map_err(|error| error.message())?;

        assert!(
            !report.valid,
            "manifest entry sizes must be verified even in structure-only inspection"
        );
        assert!(
            !report.hash_verified,
            "test setup must exercise structure-only inspection"
        );
        assert!(
            report
                .hash_mismatches
                .iter()
                .any(|mismatch| mismatch.as_str() == STATUS_FILE),
            "entry size mismatch should be reported as an integrity mismatch"
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
            SWARM_REPLAY_SUMMARY_FILE,
            SWARM_BRIEF_SUMMARY_FILE,
            SWARM_INCIDENT_SUMMARY_FILE,
            TRIAGE_SUMMARY_FILE,
            QOS_LANE_SUMMARY_FILE,
            LOCAL_CARGO_TRIPWIRE_FILE,
            REGRESSION_CAUSALITY_SUMMARY_FILE,
            VERIFICATION_EVIDENCE_SUMMARY_FILE,
            MEMORY_DRIFT_SUMMARY_FILE,
            ENVIRONMENT_ATTESTATION_SUMMARY_FILE,
        ] {
            assert!(
                files.contains(&required.to_owned()),
                "planned support-bundle files must include {required}"
            );
        }
    }

    #[test]
    fn regression_causality_summary_is_redaction_safe_and_non_authoritative() -> TestResult {
        let verification = stable_json(&json!({
            "schema": "ee.rch.verify.v1",
            "status": "rch_environment_failure",
            "selector_admission_probe": {
                "status": "selection_failed",
                "local_fallback_refused": true
            },
            "source_materialization": "remote_checkout_unverified",
            "remote_source_materialized": false,
            "degradedCodes": ["rch_worker_topology_blocked"],
            "redactionStatus": "safe",
            "stderrTail": "failure in /Users/alice/private/src/lib.rs with sk-test-secret"
        }));
        let support_summary = stable_json(&json!({
            "schema": "ee.support_bundle.v1",
            "status": "passed",
            "artifactHash": "blake3:support",
            "degradedCodes": ["prompt_budget_exceeded"],
            "redactionStatus": "safe"
        }));

        let encoded = regression_causality_summary_json(&[
            (
                "support_bundle:verification",
                RegressionEvidenceKind::VerificationEvidence,
                verification.as_str(),
            ),
            (
                "support_bundle:summary",
                RegressionEvidenceKind::SupportBundle,
                support_summary.as_str(),
            ),
        ]);
        let value: Value = serde_json::from_str(&encoded)
            .map_err(|error| format!("regression causality summary must parse: {error}"))?;
        let top_codes = value
            .pointer("/topHypotheses")
            .and_then(Value::as_array)
            .ok_or_else(|| "summary must expose top hypotheses".to_owned())?
            .iter()
            .filter_map(|hypothesis| hypothesis.pointer("/code").and_then(Value::as_str))
            .collect::<BTreeSet<_>>();

        assert_eq!(
            value.pointer("/schema"),
            Some(&json!(
                SUPPORT_BUNDLE_REGRESSION_CAUSALITY_SUMMARY_SCHEMA_V1
            ))
        );
        assert_eq!(
            value.pointer("/sourceSchema"),
            Some(&json!(REGRESSION_CAUSALITY_SCHEMA_V1))
        );
        assert_eq!(
            value.pointer("/redaction/inputArtifactsCopied"),
            Some(&json!(false))
        );
        assert_eq!(value.pointer("/redaction/hashesOnly"), Some(&json!(true)));
        assert!(top_codes.contains("source_not_materialized"));
        assert!(top_codes.contains("known_environment_blocker"));
        assert!(top_codes.contains("output_budget_regression"));
        assert!(
            value
                .pointer("/ranking/hypotheses")
                .and_then(Value::as_array)
                .is_some_and(|hypotheses| hypotheses
                    .iter()
                    .all(|hypothesis| hypothesis.pointer("/authoritative") == Some(&json!(false)))),
            "regression hypotheses must remain non-authoritative: {encoded}"
        );
        assert!(
            value
                .pointer("/normalization/rows/0/artifactHash")
                .and_then(Value::as_str)
                .is_some_and(|hash| hash.starts_with("blake3:")),
            "normalized rows must carry section artifact hashes: {encoded}"
        );
        for forbidden in ["/Users/alice", "sk-test-secret", "failure in "] {
            assert!(
                !encoded.contains(forbidden),
                "regression causality summary leaked forbidden text {forbidden:?}: {encoded}"
            );
        }

        Ok(())
    }

    #[test]
    fn perf_compare_summary_tracks_regression_causality_section_counts() -> TestResult {
        let root = unique_test_path("support-bundle-regression-causality-perf-summary");
        let bundle_dir = root.join("bundle");
        fs::create_dir_all(&bundle_dir)
            .map_err(|error| format!("failed to create bundle dir: {error}"))?;

        let causality_summary = regression_causality_summary_value(&[
            (
                "support_bundle:verification",
                RegressionEvidenceKind::VerificationEvidence,
                stable_json(&json!({
                    "schema": "ee.rch.verify.v1",
                    "status": "rch_environment_failure",
                    "selector_admission_probe": {
                        "status": "selection_failed",
                        "local_fallback_refused": true
                    },
                    "source_materialization": "remote_checkout_unverified",
                    "remote_source_materialized": false,
                    "degradedCodes": ["rch_worker_topology_blocked"],
                    "redactionStatus": "safe"
                }))
                .as_str(),
            ),
            (
                "support_bundle:summary",
                RegressionEvidenceKind::SupportBundle,
                stable_json(&json!({
                    "schema": "ee.support_bundle.v1",
                    "status": "passed",
                    "degradedCodes": ["prompt_budget_exceeded"],
                    "redactionStatus": "safe"
                }))
                .as_str(),
            ),
        ]);
        let causality_summary_json = stable_json(&causality_summary);
        fs::write(
            bundle_dir.join(REGRESSION_CAUSALITY_SUMMARY_FILE),
            &causality_summary_json,
        )
        .map_err(|error| format!("failed to write causality summary: {error}"))?;

        let manifest = BundleManifest {
            schema: SUPPORT_BUNDLE_MANIFEST_SCHEMA_V1.to_owned(),
            bundle_id: "regression-causality-perf-summary".to_owned(),
            created_at: "2026-06-04T00:00:00Z".to_owned(),
            workspace_path: "REDACTED/workspace".to_owned(),
            ee_version: env!("CARGO_PKG_VERSION").to_owned(),
            files: vec![ManifestEntry {
                path: REGRESSION_CAUSALITY_SUMMARY_FILE.to_owned(),
                size_bytes: causality_summary_json.len() as u64,
                content_hash: compute_hash(&causality_summary_json),
                redacted: true,
            }],
            total_size_bytes: causality_summary_json.len() as u64,
            redaction_applied: true,
            redaction_reasons: vec!["fixture".to_owned()],
        };
        let inspect = InspectReport {
            schema: SUPPORT_BUNDLE_INSPECT_SCHEMA_V1.to_owned(),
            bundle_path: bundle_dir.clone(),
            manifest: Some(manifest),
            files_found: vec![REGRESSION_CAUSALITY_SUMMARY_FILE.to_owned()],
            total_size_bytes: causality_summary_json.len() as u64,
            hash_verified: true,
            hash_mismatches: Vec::new(),
            valid: true,
        };

        let summary = summarize_inspected_bundle_for_perf_compare(&inspect);

        assert_eq!(
            summary
                .metrics
                .get("section.regression_causality_summary.present")
                .and_then(|metric| metric.value),
            Some(1.0)
        );
        assert_eq!(
            summary
                .metrics
                .get("regression_causality.normalized_row_count")
                .and_then(|metric| metric.value),
            Some(2.0)
        );
        assert!(
            summary
                .metrics
                .get("regression_causality.top_hypothesis_count")
                .and_then(|metric| metric.value)
                .is_some_and(|count| count >= 2.0),
            "perf summary must expose compact causality hypothesis counts: {summary:?}"
        );
        assert!(
            summary.provenance.iter().any(|entry| {
                entry.field == "section.regression_causality_summary.present"
                    && entry.source_path == REGRESSION_CAUSALITY_SUMMARY_FILE
            }),
            "perf summary must cite the causality summary section"
        );

        Ok(())
    }

    #[test]
    fn swarm_incident_summary_redacts_replay_evidence_for_support_and_handoff() -> TestResult {
        let workspace = unique_test_path("swarm-incident-summary");
        let fixture_dir = workspace
            .join("tests")
            .join("fixtures")
            .join("swarm_incidents");
        fs::create_dir_all(&fixture_dir)
            .map_err(|error| format!("failed to create incident fixture dir: {error}"))?;

        let fixture = json!({
            "schema": "ee.swarm_incident.v1",
            "scenarioId": "redaction_fixture",
            "fixedClock": "2026-05-21T00:00:00Z",
            "purpose": "Exercise /Users/alice/private incident evidence without leaking it.",
            "substrates": {
                "agentMail": {"status": "unavailable", "evidence": ["mail body: secret"], "degradedCodes": ["agent_mail_unavailable"]},
                "beads": {"status": "ok", "evidence": [], "degradedCodes": []},
                "rch": {"status": "blocked", "evidence": ["worker host alpha.internal"], "degradedCodes": ["rch_worker_topology_blocked"]},
                "disk": {"status": "degraded", "evidence": [], "degradedCodes": ["disk_pressure_high"]},
                "hotPath": {"status": "ok", "evidence": [], "degradedCodes": []}
            },
            "expectedDegraded": [
                {"code": "agent_mail_unavailable", "severity": "medium", "surface": "diag incident", "reason": "raw mail body omitted"},
                {"code": "rch_worker_topology_blocked", "severity": "warning", "surface": "diag incident", "reason": "raw worker hostname omitted"}
            ],
            "expectedRecoveryActions": [
                {
                    "priority": 1,
                    "kind": "observe",
                    "summary": "Capture a redacted incident note for /Users/alice/private/workspace.",
                    "command": "rm -rf /Users/alice/private/workspace --token sk-test-redaction",
                    "manualStep": "Open /Users/alice/private/mail.txt and redact the body.",
                    "evidence": ["RCH-E327", "bd-17c65.10.17.1.2"],
                    "destructive": false,
                    "preconditions": ["human approval"]
                }
            ],
            "redactionExpectations": {"pathPolicy": "redact_home", "secretPolicy": "no_secrets"},
            "assertions": {"deterministic": true, "noLiveServices": true, "noLocalCargo": true, "noDeletion": true, "noMutation": true},
            "artifacts": [
                {"path": "/Users/alice/private/raw-log.txt", "kind": "fixture-log"},
                {"path": "tests/fixtures/swarm_incidents/redaction_fixture.json", "kind": "fixture"}
            ]
        });
        fs::write(
            fixture_dir.join("redaction_fixture.json"),
            stable_json(&fixture),
        )
        .map_err(|error| format!("failed to write incident fixture: {error}"))?;

        let summary = crate::core::swarm_brief::collect_swarm_incident_summary(&workspace);
        let encoded = stable_json(&summary);

        assert_eq!(
            summary.pointer("/schema"),
            Some(&json!(
                crate::core::swarm_brief::SWARM_INCIDENT_SUMMARY_SCHEMA_V1
            ))
        );
        assert_eq!(summary.pointer("/status"), Some(&json!("available")));
        assert_eq!(
            summary.pointer("/counts/summarizedIncidentCount"),
            Some(&json!(1))
        );
        assert_eq!(
            summary.pointer("/incidents/0/scenarioId"),
            Some(&json!("redaction_fixture"))
        );
        assert_eq!(
            summary.pointer("/incidents/0/substratePosture/rch"),
            Some(&json!("blocked"))
        );
        assert_eq!(
            summary.pointer("/incidents/0/recoveryActionSummaries/0/commandIncluded"),
            Some(&json!(false))
        );
        assert!(
            summary
                .pointer("/incidents/0/recoveryActionSummaries/0/evidenceHashes/0")
                .and_then(Value::as_str)
                .is_some_and(|hash| hash.starts_with("blake3:")),
            "summary must keep evidence hashes for provenance"
        );
        assert!(
            summary
                .pointer("/incidents/0/outputHash")
                .and_then(Value::as_str)
                .is_some_and(|hash| hash.starts_with("blake3:")),
            "summary must expose deterministic replay output hash"
        );
        assert_eq!(summary.pointer("/withinSizeBudget"), Some(&json!(true)));
        for forbidden in [
            "/Users/alice",
            "rm -rf",
            "--token",
            "sk-test-redaction",
            "mail body: secret",
            "Open /Users/alice",
            "raw-log.txt",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "incident support summary leaked forbidden text {forbidden:?}: {encoded}"
            );
        }

        let rendered =
            crate::core::swarm_brief::render_swarm_incident_summary_for_handoff(&summary);
        assert!(rendered.contains("Swarm incident summary: status=available"));
        assert!(rendered.contains("raw logs"));
        assert!(!rendered.contains("rm -rf"));
        Ok(())
    }

    #[test]
    fn swarm_replay_summary_redacts_artifacts_for_support_and_handoff() -> TestResult {
        let workspace = unique_test_path("swarm-replay-summary");
        let replay_dir = workspace
            .join(crate::core::lab::SWARM_REPLAY_ARTIFACT_DIR_TAIL)
            .join("run_redaction");
        fs::create_dir_all(&replay_dir)
            .map_err(|error| format!("failed to create replay dir: {error}"))?;

        let replay_result = json!({
            "schema": "ee.swarm_replay_result.v1",
            "workloadId": "workload_redaction",
            "runId": "run_redaction",
            "sideEffectFree": true,
            "status": "degraded",
            "hostProfileAdmission": {
                "declaredProfile": "standard",
                "requestedParallelAgents": 8,
                "requiredClass": "standard",
                "observedClass": "standard",
                "status": "admitted",
                "degradedCodes": ["swarm_replay_memory_unknown"]
            },
            "commandResults": [
                {
                    "stepId": "remember_secret",
                    "agentSlot": 0,
                    "commandHash": "blake3:command",
                    "exitCode": 0,
                    "elapsedMs": 42,
                    "stdoutBytes": 12,
                    "stderrBytes": 0,
                    "degradedCodes": ["swarm_replay_slo_budget_warned: /Users/alice/private"],
                    "artifactPaths": [
                        {
                            "kind": "stdout",
                            "pathTail": "/Users/alice/private/stdout.txt",
                            "pathHash": "blake3:stdout-hash"
                        }
                    ],
                    "redactionStatus": "redacted",
                    "slo": {
                        "status": "warn",
                        "diagnosis": "swarm_replay_slo_budget_warned: token sk-test-redaction"
                    }
                }
            ],
            "aggregate": {
                "commandCount": 1,
                "successCount": 1,
                "failureCount": 0,
                "degradedCount": 1,
                "sloWarningCount": 1,
                "sloFailureCount": 0,
                "firstSloFailureStepId": null,
                "p95Ms": 42,
                "p99Ms": 42
            },
            "redactionStatus": {
                "rawTaskStringPresent": false,
                "rawQueryTextPresent": false,
                "rawMemoryBodyPresent": false,
                "rawMailBodyPresent": false,
                "absoluteHostPathPresent": false,
                "secretsPresent": false,
                "environmentDumpPresent": false,
                "fullFileListingPresent": false,
                "redactionProbesPassed": true
            },
            "firstFailure": {
                "stepId": "remember_secret",
                "agentSlot": 0,
                "code": "swarm_replay_slo_budget_warned",
                "severity": "warning",
                "diagnosis": "Inspect /Users/alice/private/stdout.txt with sk-test-redaction",
                "repairHint": "Do not paste raw output."
            },
            "verification": {
                "rchRequired": true,
                "rchStatus": "passed",
                "workloadHash": "blake3:workload",
                "replayHash": "blake3:replay",
                "proofCapsule": {
                    "schema": "ee.swarm_replay.verification_capsule.v1",
                    "proofLevel": "remote_verified",
                    "rch": {
                        "commandHash": "sha256:proof-command",
                        "workerId": "vmi-private-worker",
                        "remoteMarkerPresent": true,
                        "cargoStarted": true,
                        "rawOutputIncluded": false,
                        "localPathsRedacted": true,
                        "degradedCodes": []
                    }
                }
            },
            "warnings": ["swarm_replay_slo_budget_warned: /Users/alice/private"]
        });
        fs::write(
            replay_dir.join(crate::core::lab::SWARM_REPLAY_RESULT_ARTIFACT_FILE),
            stable_json(&replay_result),
        )
        .map_err(|error| format!("failed to write replay result: {error}"))?;

        let summary = crate::core::swarm_brief::collect_swarm_replay_summary(&workspace);
        let encoded = stable_json(&summary);

        assert_eq!(
            summary.pointer("/schema"),
            Some(&json!(
                crate::core::swarm_brief::SWARM_REPLAY_SUMMARY_SCHEMA_V1
            ))
        );
        assert_eq!(summary.pointer("/status"), Some(&json!("available")));
        assert_eq!(
            summary.pointer("/counts/summarizedReplayCount"),
            Some(&json!(1))
        );
        assert_eq!(
            summary.pointer("/latestReplay/proofCapsule/proofLevel"),
            Some(&json!("remote_verified"))
        );
        assert_eq!(
            summary.pointer("/latestReplay/proofCapsule/rchStatus"),
            Some(&json!("passed"))
        );
        assert_eq!(
            summary.pointer("/latestReplay/artifacts/pathIncluded"),
            Some(&json!(false))
        );
        assert!(
            summary
                .pointer("/latestReplay/proofCapsule/workerIdHash")
                .and_then(Value::as_str)
                .is_some_and(|hash| hash.starts_with("blake3:")),
            "worker id must be represented only as a hash"
        );
        for forbidden in [
            "/Users/alice",
            "sk-test-redaction",
            "vmi-private-worker",
            "stdout.txt",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "replay support summary leaked forbidden text {forbidden:?}: {encoded}"
            );
        }

        let rendered = crate::core::swarm_brief::render_swarm_replay_summary_for_handoff(&summary);
        assert!(rendered.contains("Swarm replay summary: status=available"));
        assert!(rendered.contains("proof_level=remote_verified"));
        assert!(!rendered.contains("/Users/alice"));
        Ok(())
    }

    #[test]
    fn swarm_replay_summary_degrades_honestly_when_artifacts_are_missing() -> TestResult {
        let workspace = unique_test_path("swarm-replay-summary-missing-artifacts");

        let summary = crate::core::swarm_brief::collect_swarm_replay_summary(&workspace);

        assert_eq!(
            summary.pointer("/schema"),
            Some(&json!(
                crate::core::swarm_brief::SWARM_REPLAY_SUMMARY_SCHEMA_V1
            ))
        );
        assert_eq!(
            summary.pointer("/status"),
            Some(&json!("artifact_directory_missing"))
        );
        assert_eq!(
            summary.pointer("/counts/summarizedReplayCount"),
            Some(&json!(0))
        );
        assert_eq!(
            summary.pointer("/redaction/rawCommandOutputIncluded"),
            Some(&json!(false))
        );
        assert_eq!(
            summary.pointer("/redaction/commandArgsIncluded"),
            Some(&json!(false))
        );
        assert_eq!(
            summary.pointer("/redaction/artifactPathsIncluded"),
            Some(&json!(false))
        );
        assert_eq!(summary.pointer("/withinSizeBudget"), Some(&json!(true)));
        assert!(
            summary
                .pointer("/summaryHash")
                .and_then(Value::as_str)
                .is_some_and(|hash| hash.starts_with("blake3:")),
            "missing-artifact replay summary still gets a stable hash"
        );
        Ok(())
    }

    #[test]
    fn memory_drift_support_summary_degrades_without_database() {
        let workspace = unique_test_path("memory-drift-summary-missing-db");
        let summary = collect_memory_drift_support_summary(&workspace, 8);
        let encoded = stable_json(&summary);

        assert!(
            encoded.contains("\"schema\":\"ee.support_bundle.memory_drift_summary.v1\""),
            "memory drift support summary schema missing: {encoded}"
        );
        assert!(
            encoded.contains("\"status\":\"database_unavailable\""),
            "missing database must be explicit: {encoded}"
        );
        assert!(
            encoded.contains("memory_drift_source_unverifiable"),
            "missing database must carry a drift degraded code: {encoded}"
        );
        assert!(
            encoded.contains("\"rawSnippetsIncluded\":false"),
            "raw snippets must not be included: {encoded}"
        );
        assert!(
            !encoded.contains(&workspace.to_string_lossy().to_string()),
            "summary must not include raw workspace paths: {encoded}"
        );
    }

    #[test]
    fn verification_evidence_summary_redacts_raw_command_and_output() -> TestResult {
        use crate::models::{
            ProducerMetadata, ProducerSourceSystem, VerificationEnvironment,
            VerificationEvidenceInput, VerificationOffload, VerificationOutputSummary,
        };

        let producer = ProducerMetadata::unknown_agent(
            ProducerSourceSystem::Verification,
            Some("run-danger"),
            None,
            Some("repo:abc$(oops)"),
            Some("2026-05-19T07:10:00Z"),
        );
        let mut evidence = VerificationEvidenceRecord::from_input(VerificationEvidenceInput {
            verification_id: "ver_$(touch pwned)",
            bead_id: Some("bd-1nxz4.5`bad`"),
            gate_name: "cargo test $(launch)",
            command: "cargo test --lib $(cat secret)",
            status: VerificationStatus::Blocked,
            exit_code: None,
            started_at: Some("2026-05-19T07:00:00Z"),
            finished_at: None,
            duration_ms: None,
            environment: VerificationEnvironment::new(
                Some("git_tree:abc$(oops)"),
                Some("/repo"),
                None,
            ),
            offload: VerificationOffload::rch_fallback(
                Some("vmi`123`"),
                Some("$(launch remote) && retry"),
            ),
            output_summary: VerificationOutputSummary::redacted(Some("stderr $(secret) tail")),
            artifacts: Vec::new(),
            producer,
        });
        evidence.command_hash = "sha256:$(unsafe)`hash`".to_owned();
        let details = json!({
            "schema": crate::core::verify::VERIFICATION_LEDGER_ENTRY_SCHEMA_V1,
            "contentHash": "blake3:ledger",
            "producer": evidence.producer.clone(),
            "status": evidence.status,
            "evidence": evidence,
        });
        let entry = StoredAuditEntry {
            id: "audit_$(danger)".to_owned(),
            workspace_id: Some("wsp_test".to_owned()),
            timestamp: "2026-05-19T07:20:00Z".to_owned(),
            actor: Some("ChartreuseHawk".to_owned()),
            action: audit_actions::VERIFICATION_INGEST.to_owned(),
            target_type: Some("verification".to_owned()),
            target_id: Some("ver_$(touch pwned)".to_owned()),
            details: Some(details.to_string()),
            surface: "verification".to_owned(),
            mutation_kind: audit_actions::VERIFICATION_INGEST.to_owned(),
            before_hash: None,
            after_hash: None,
            prev_row_hash: None,
            this_row_hash: Some("blake3:row".to_owned()),
        };

        let summary = summarize_verification_evidence_audit_entry(&entry)
            .ok_or_else(|| "expected verification evidence audit summary".to_owned())?;
        let bundle_value =
            verification_evidence_summary_value("available", json!({}), vec![summary]);
        let encoded = stable_json(&bundle_value);

        assert!(
            encoded.contains("\"schema\":\"ee.support_bundle.verification_evidence_summary.v1\"")
        );
        assert!(encoded.contains("\"sourceSchema\":\"ee.verification.evidence.v1\""));
        assert!(encoded.contains("\"resultClass\":\"environment_blocker\""));
        assert!(encoded.contains("\"rawCommandIncluded\":false"));
        assert!(encoded.contains("\"rawOutputIncluded\":false"));
        assert!(encoded.contains("raw_output_included=false"));
        assert!(encoded.contains("blake3:ledger"));
        assert!(!encoded.contains("cargo test --lib"));
        assert!(!encoded.contains("cat secret"));
        assert!(!encoded.contains("stderr"));
        assert!(!encoded.contains('$'));
        assert!(!encoded.contains('`'));
        Ok(())
    }

    #[test]
    fn local_cargo_tripwire_summary_reports_policy_without_running_cargo() -> TestResult {
        let workspace = unique_test_path("local-cargo-tripwire-summary");
        fs::create_dir_all(&workspace)
            .map_err(|error| format!("failed to create workspace: {error}"))?;

        let value: Value = serde_json::from_str(&local_cargo_tripwire_json(&workspace))
            .map_err(|error| format!("local cargo tripwire summary must parse: {error}"))?;

        assert_eq!(
            value.pointer("/schema"),
            Some(&json!("ee.support_bundle.local_cargo_tripwire.v1"))
        );
        assert_eq!(
            value.pointer("/collectionStatus"),
            Some(&json!("policy_summary_process_scan_unavailable"))
        );
        assert_eq!(
            value.pointer("/localBuildPolicy/status"),
            Some(&json!("enforced"))
        );
        assert!(
            value
                .pointer("/localBuildPolicy/policyState")
                .and_then(Value::as_str)
                .is_some(),
            "summary must expose a stable local build policy state"
        );
        assert_eq!(
            value.pointer("/requiredRemoteWrapper"),
            Some(&json!(SUPPORT_BUNDLE_REQUIRED_REMOTE_WRAPPER))
        );
        assert_eq!(value.pointer("/detectedLocalBuilds"), Some(&json!([])));
        assert_eq!(
            value.pointer("/plannedCommandClassifications/0/policyStatus"),
            Some(&json!("local_cargo_disallowed"))
        );
        assert_eq!(
            value.pointer("/plannedCommandClassifications/1/policyStatus"),
            Some(&json!("remote_wrapper_required"))
        );
        assert!(
            value.pointer("/buildAdmission/admitted").is_some(),
            "summary must include build-admission evidence"
        );

        Ok(())
    }

    #[test]
    fn local_cargo_tripwire_process_scan_detects_live_bypass_without_running_cargo() -> TestResult {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
        let value = local_cargo_tripwire_process_scan_json(workspace);

        assert_eq!(
            value.pointer("/schema"),
            Some(&json!("ee.rch_local_cargo_tripwire.v1"))
        );
        assert_eq!(value.pointer("/mode"), Some(&json!("probe_processes")));
        assert!(
            value.pointer("/detectedLocalBuilds").is_some(),
            "process scan must expose stable detectedLocalBuilds evidence"
        );
        assert!(
            value.pointer("/disk_pressure_context").is_some()
                || value.pointer("/status") == Some(&json!("unavailable")),
            "available process scans must carry disk_pressure_context"
        );

        Ok(())
    }

    #[test]
    fn local_cargo_tripwire_summary_classifies_direct_rustdoc_as_disallowed() -> TestResult {
        let workspace = unique_test_path("local-rustdoc-tripwire-summary");
        fs::create_dir_all(&workspace)
            .map_err(|error| format!("failed to create workspace: {error}"))?;

        let value = local_cargo_preflight_classification(&workspace, "rustdoc --test src/lib.rs");

        assert_eq!(
            value.pointer("/policyStatus"),
            Some(&json!("local_cargo_disallowed"))
        );
        assert!(
            value
                .pointer("/matchedRuleIds")
                .and_then(Value::as_array)
                .is_some_and(|ids| ids
                    .iter()
                    .any(|id| id == "builtin:local_rust_compiler_verification")),
            "classification must cite the direct rustc/rustdoc guard: {value}"
        );

        Ok(())
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
        assert_eq!(
            value.pointer("/derivedAssetStore/schema"),
            Some(&json!("ee.derived_asset_store.summary.v1"))
        );
        assert_eq!(
            value.pointer("/derivedAssetStore/reuseMode"),
            Some(&json!("read_only"))
        );
        assert_eq!(
            value.pointer("/derivedAssetStore/cleanup/automaticDeletion"),
            Some(&json!(false))
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
    fn support_bundle_swarm_brief_summary_hashes_holder_labels() -> TestResult {
        let holder_hash = support_agent_name_hash("OtherAgent");
        let prefixed_holder_hash = support_agent_name_hash("blake3:OtherAgent");
        let mut summary = json!({
            "fileSurfaceRiskSummary": {
                "countsByReservationHolder": {"OtherAgent": 1},
                "topRisks": [
                    {
                        "reservationHolders": ["OtherAgent"],
                    }
                ]
            },
            "readyReservationPressureSummary": {
                "countsByReservationHolder": {"OtherAgent": 2},
                "topReadyBeads": [
                    {
                        "reservationHolders": ["OtherAgent"],
                    }
                ]
            },
            "redaction": {
                "rawMailBodiesIncluded": false,
                "rawQueryTextIncluded": false,
                "rawProvenanceTextIncluded": false,
                "fullFileListingsIncluded": false,
                "recommendationEvidenceIncluded": "hashes_only"
            }
        });
        summary
            .pointer_mut("/fileSurfaceRiskSummary/countsByReservationHolder")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| "test summary missing countsByReservationHolder".to_string())?
            .insert(holder_hash.clone(), json!(3));
        summary
            .pointer_mut("/readyReservationPressureSummary/countsByReservationHolder")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| "test summary missing ready countsByReservationHolder".to_string())?
            .insert("blake3:OtherAgent".to_string(), json!(5));

        redact_support_bundle_swarm_brief_summary(&mut summary);
        let encoded = stable_json(&summary);

        assert!(!encoded.contains("OtherAgent"));
        assert!(!encoded.contains("blake3:OtherAgent"));
        assert_eq!(
            summary.pointer(&format!(
                "/fileSurfaceRiskSummary/countsByReservationHolder/{holder_hash}"
            )),
            Some(&json!(4))
        );
        assert_eq!(
            summary.pointer("/fileSurfaceRiskSummary/topRisks/0/reservationHolders/0"),
            Some(&json!(holder_hash.clone()))
        );
        assert_eq!(
            summary.pointer(&format!(
                "/readyReservationPressureSummary/countsByReservationHolder/{holder_hash}"
            )),
            Some(&json!(2))
        );
        assert_eq!(
            summary.pointer(&format!(
                "/readyReservationPressureSummary/countsByReservationHolder/{prefixed_holder_hash}"
            )),
            Some(&json!(5))
        );
        assert_eq!(
            summary
                .pointer("/readyReservationPressureSummary/topReadyBeads/0/reservationHolders/0"),
            Some(&json!(holder_hash))
        );
        assert_eq!(
            summary.pointer("/redaction/rawAgentNamesIncluded"),
            Some(&json!(false))
        );
        assert_eq!(
            summary.pointer("/redaction/reservationHolderLabelsIncluded"),
            Some(&json!("hashes_only"))
        );

        let mut missing_redaction_summary = json!({
            "fileSurfaceRiskSummary": {
                "countsByReservationHolder": {"OtherAgent": 1},
                "topRisks": []
            }
        });
        redact_support_bundle_swarm_brief_summary(&mut missing_redaction_summary);
        assert_eq!(
            missing_redaction_summary.pointer("/redaction/rawAgentNamesIncluded"),
            Some(&json!(false))
        );
        assert_eq!(
            missing_redaction_summary.pointer("/redaction/reservationHolderLabelsIncluded"),
            Some(&json!("hashes_only"))
        );
        Ok(())
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
                "repair": format!("ee why {memory_id} --json")
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
        assert!(
            report
                .files_collected
                .contains(&SWARM_INCIDENT_SUMMARY_FILE.to_owned()),
            "support bundle must include swarm incident summary"
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
        assert!(
            swarm_summary.pointer("/gitAhead").is_some(),
            "swarm brief summary must include the push-safety section"
        );
        assert!(
            swarm_summary.pointer("/verificationBroker").is_some(),
            "swarm brief summary must include the verification broker section"
        );
        assert!(
            swarm_summary
                .pointer("/readyReservationPressureSummary")
                .is_some(),
            "swarm brief summary must include ready-work reservation pressure"
        );
        assert!(
            swarm_summary.pointer("/symbolRiskSummary").is_some(),
            "swarm brief summary must include symbol-risk posture"
        );
        let incident_summary_text =
            fs::read_to_string(bundle_dir.join(SWARM_INCIDENT_SUMMARY_FILE))
                .map_err(|error| format!("failed to read swarm incident summary: {error}"))?;
        let incident_summary: Value = serde_json::from_str(&incident_summary_text)
            .map_err(|error| format!("swarm incident summary must parse: {error}"))?;
        assert_eq!(
            incident_summary.pointer("/schema"),
            Some(&json!(
                super::super::swarm_brief::SWARM_INCIDENT_SUMMARY_SCHEMA_V1
            ))
        );
        assert_eq!(
            incident_summary.pointer("/redaction/rawLogsIncluded"),
            Some(&json!(false))
        );
        assert_eq!(
            incident_summary.pointer("/redaction/mailBodiesIncluded"),
            Some(&json!(false))
        );
        assert_eq!(
            incident_summary.pointer("/redaction/commandArgsIncluded"),
            Some(&json!(false))
        );
        assert!(
            incident_summary
                .pointer("/summaryHash")
                .and_then(Value::as_str)
                .is_some_and(|hash| hash.starts_with("blake3:")),
            "swarm incident summary must hash the compact incident projection"
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
            LOCAL_CARGO_TRIPWIRE_FILE,
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

    // bd-hwowj: pin the symlink-gate on the workspace-script that
    // `local_cargo_tripwire_process_scan_json` would otherwise have
    // executed via Command::new. A malicious workspace that plants
    // a symlink at `scripts/check-local-cargo-tripwire.sh` must
    // fail closed BEFORE the spawn, surfacing the
    // `tripwire_script_symlink_refused` reason for downstream
    // visibility.
    //
    // The benign-workspace case is already covered by
    // `local_cargo_tripwire_process_scan_detects_live_bypass_without_running_cargo`
    // above, which exercises the real `CARGO_MANIFEST_DIR` workspace
    // (regular file at the documented path) and continues to pass
    // through the new gate untouched.
    #[cfg(unix)]
    #[test]
    fn local_cargo_tripwire_process_scan_refuses_symlinked_script() -> TestResult {
        let workspace = unique_test_path("local-tripwire-symlink-attack");
        let scripts_dir = workspace.join("scripts");
        fs::create_dir_all(&scripts_dir)
            .map_err(|error| format!("failed to create workspace scripts dir: {error}"))?;

        let attack_target = unique_test_path("local-tripwire-symlink-target");
        fs::write(
            &attack_target,
            "#!/bin/sh\necho 'attacker-controlled output'\n",
        )
        .map_err(|error| format!("failed to write attack target: {error}"))?;

        let link = scripts_dir.join("check-local-cargo-tripwire.sh");
        std::os::unix::fs::symlink(&attack_target, &link)
            .map_err(|error| format!("failed to create symlink: {error}"))?;

        let value = local_cargo_tripwire_process_scan_json(&workspace);

        // (a) symlink-attack workspace must fail closed.
        assert_eq!(
            value.pointer("/status"),
            Some(&json!("unavailable")),
            "symlink-attack workspace must report status=unavailable; got: {value}",
        );
        assert_eq!(
            value.pointer("/reason"),
            Some(&json!("tripwire_script_symlink_refused")),
            "symlink-attack workspace must surface the specific refusal reason; got: {value}",
        );
        assert_eq!(
            value.pointer("/schema"),
            Some(&json!("ee.rch_local_cargo_tripwire.v1")),
            "refusal envelope must keep the stable schema for downstream consumers; got: {value}",
        );
        // The detail string should point the operator at the fix
        // path (replace symlink with a regular file).
        assert!(
            value
                .pointer("/detail")
                .and_then(Value::as_str)
                .is_some_and(|detail| detail.contains("symlink")),
            "refusal envelope must explain the symlink refusal; got: {value}",
        );

        let _ = fs::remove_file(&link);
        let _ = fs::remove_file(&attack_target);
        let _ = fs::remove_dir(&scripts_dir);
        let _ = fs::remove_dir(&workspace);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn local_cargo_tripwire_process_scan_refuses_symlinked_script_parent() -> TestResult {
        let root = unique_test_path("local-tripwire-parent-symlink-attack");
        let workspace = root.join("workspace");
        let outside_scripts = root.join("outside-scripts");
        fs::create_dir_all(&workspace)
            .map_err(|error| format!("failed to create workspace dir: {error}"))?;
        fs::create_dir_all(&outside_scripts)
            .map_err(|error| format!("failed to create outside scripts dir: {error}"))?;
        fs::write(
            outside_scripts.join("check-local-cargo-tripwire.sh"),
            "#!/bin/sh\necho '{\"status\":\"attacker-controlled\"}'\n",
        )
        .map_err(|error| format!("failed to write outside tripwire script: {error}"))?;
        std::os::unix::fs::symlink(&outside_scripts, workspace.join("scripts"))
            .map_err(|error| format!("failed to create symlinked scripts dir: {error}"))?;

        let value = local_cargo_tripwire_process_scan_json(&workspace);

        assert_eq!(
            value.pointer("/status"),
            Some(&json!("unavailable")),
            "symlinked parent workspace must report status=unavailable; got: {value}",
        );
        assert_eq!(
            value.pointer("/reason"),
            Some(&json!("tripwire_script_symlink_refused")),
            "symlinked parent workspace must surface the specific refusal reason; got: {value}",
        );
        assert!(
            value
                .pointer("/detail")
                .and_then(Value::as_str)
                .is_some_and(|detail| detail.contains("symlinked component")),
            "refusal envelope must explain the symlinked parent component; got: {value}",
        );
        Ok(())
    }
}
