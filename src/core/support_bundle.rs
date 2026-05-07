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

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use blake3::Hasher;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cache::CacheBudget;
use crate::db::DbConnection;
use crate::models::DomainError;
use crate::output;
use crate::pack::{
    PackCacheGovernor, PackHotset, PackHotsetEntry, PackHotsetEntryKind, PackSection,
    prewarm_pack_hotset,
};
use crate::policy::redact_secret_like_content;
use crate::search::{SearchCacheGovernor, SearchHotset, SearchHotsetEntry, prewarm_search_hotset};

use super::doctor::DoctorReport;
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
const SCALE_BENCHMARK_SUMMARY_FILE: &str = "scale_benchmark_summary.json";
const SCALE_FIXTURE_MANIFEST_FILE: &str = "scale_fixture_manifest.json";
const CACHE_REPORTS_FILE: &str = "scale_cache_reports.json";
const WRITE_QUEUE_REPORT_FILE: &str = "scale_write_queue_report.json";
const PERFORMANCE_EXPLAIN_SAMPLES_FILE: &str = "scale_performance_explain_samples.json";
const TRIAGE_SUMMARY_FILE: &str = "scale_triage_summary.json";
const SWARM_SCALE_WORKLOADS_MANIFEST: &str =
    include_str!("../../tests/fixtures/swarm_scale/workloads.json");

/// Options for creating a support bundle.
#[derive(Clone, Debug)]
pub struct BundleOptions {
    pub workspace: PathBuf,
    pub output_dir: Option<PathBuf>,
    pub dry_run: bool,
    pub redacted: bool,
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
            include_raw: false,
            audit_limit: 100,
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
    scale_benchmark_summary_json: String,
    scale_fixture_manifest_json: String,
    cache_reports_json: String,
    write_queue_report_json: String,
    performance_explain_samples_json: String,
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
        redaction_applied: options.redacted,
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

    fs::create_dir_all(&bundle_dir).map_err(|e| DomainError::Storage {
        message: format!("Failed to create bundle directory: {e}"),
        repair: Some("Check write permissions on output directory".to_string()),
    })?;

    let diagnostics = collect_diagnostics(&workspace_path, options.audit_limit)?;

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
        (TRIAGE_SUMMARY_FILE, &diagnostics.triage_summary_json),
    ];

    for (filename, content) in files_to_write {
        let (final_content, redacted) = if options.redacted && !options.include_raw {
            let report = redact_secret_like_content(content);
            let redacted = report.redacted;
            let reasons: Vec<String> = report
                .redacted_reasons
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
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
        redaction_applied: options.redacted,
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
        redaction_applied: options.redacted,
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

    let manifest_path = if options.bundle_path.is_dir() {
        options.bundle_path.join(MANIFEST_FILE)
    } else {
        options.bundle_path.clone()
    };

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
            let file_path = bundle_dir.join(&entry.path);
            if file_path.is_file() {
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
            }
        }
    } else if options.bundle_path.is_dir() {
        if let Ok(entries) = fs::read_dir(bundle_dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    files_found.push(name.to_owned());
                    if let Ok(meta) = entry.metadata() {
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

    let swarm_reports = discover_swarm_report_summaries(workspace);
    let scale_benchmark_summary_json = scale_benchmark_summary_json(workspace, &swarm_reports);
    let scale_fixture_manifest_json = scale_fixture_manifest_json();
    let cache_reports_json = cache_reports_json();
    let write_queue_report_json = write_queue_report_json();
    let performance_explain_samples_json = performance_explain_samples_json();
    let triage_summary_json = triage_summary_json(&status, &swarm_reports);

    Ok(CollectedDiagnostics {
        status_json,
        doctor_json,
        audit_json,
        capabilities_json,
        schema_json,
        scale_benchmark_summary_json,
        scale_fixture_manifest_json,
        cache_reports_json,
        write_queue_report_json,
        performance_explain_samples_json,
        triage_summary_json,
    })
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

fn cache_reports_json() -> String {
    let generation = 7;
    let mut search_entries = vec![
        SearchHotsetEntry::memory("mem_swarm_small_00000001", generation, 9),
        SearchHotsetEntry::graph_neighborhood("mem_swarm_small_00000001", 2, generation, 3),
    ];
    if let Some(query_entry) =
        SearchHotsetEntry::query_shape("prepare release under swarm load", generation, 12)
    {
        search_entries.push(query_entry);
    }
    let search_hotset = SearchHotset::new(search_entries);
    let search_report = prewarm_search_hotset(
        &search_hotset,
        SearchCacheGovernor::new(generation, CacheBudget::new(16, 64 * 1024)),
    );

    let pack_hotset = PackHotset::new([
        PackHotsetEntry {
            key: support_cache_key("pack:section:procedural_rules"),
            kind: PackHotsetEntryKind::PackSection,
            section: Some(PackSection::ProceduralRules),
            generation,
            estimated_bytes: 256,
            hit_count: 8,
            redaction_status: "content_not_stored",
        },
        PackHotsetEntry {
            key: support_cache_key("pack:selection_certificate:swarm"),
            kind: PackHotsetEntryKind::SelectionCertificate,
            section: None,
            generation,
            estimated_bytes: 192,
            hit_count: 3,
            redaction_status: "content_not_stored",
        },
    ]);
    let pack_report = prewarm_pack_hotset(
        &pack_hotset,
        PackCacheGovernor::new(generation, CacheBudget::new(16, 64 * 1024)),
    );

    stable_json(&json!({
        "schema": "ee.support_bundle.scale_cache_reports.v1",
        "redactionStatus": "content_not_stored",
        "reports": {
            "search": search_report.data_json(),
            "pack": pack_report.data_json(),
        },
    }))
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

fn performance_explain_samples_json() -> String {
    stable_json(&json!({
        "schema": "ee.support_bundle.performance_explain_samples.v1",
        "sourceSchema": super::search::PERFORMANCE_EXPLAIN_SCHEMA_V1,
        "samples": [
            {
                "command": "search",
                "queryPlan": {
                    "retrievalMode": "instant",
                    "requestedLimit": 10,
                    "candidateBudget": 200,
                    "usesEmbeddings": false,
                    "scoreExplanationsRequested": true,
                },
                "owners": ["search", "host_resource_pressure"],
                "redactionStatus": "query_shape_only",
            },
            {
                "command": "context",
                "queryPlan": {
                    "retrievalMode": "balanced",
                    "requestedCandidatePool": 100,
                    "maxTokens": 4000,
                    "profile": "balanced",
                },
                "owners": ["pack", "search", "graph"],
                "redactionStatus": "query_shape_only",
            }
        ],
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
    let db_integrity_failed = swarm_reports
        .iter()
        .any(|report| report.get("dbIntegrityOk") == Some(&Value::Bool(false)));
    let determinism_failed = swarm_reports
        .iter()
        .any(|report| report.get("determinismOk") == Some(&Value::Bool(false)));

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
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    paths.sort();
    paths.truncate(16);

    paths
        .iter()
        .filter_map(|path| summarize_swarm_report(workspace, path))
        .collect()
}

fn summarize_swarm_report(workspace: &Path, path: &Path) -> Option<Value> {
    let raw_content = fs::read_to_string(path).ok()?;
    let redaction = redact_secret_like_content(&raw_content);
    let parsed = serde_json::from_str::<Value>(&raw_content).ok()?;
    let relative_path = path.strip_prefix(workspace).unwrap_or(path);
    let relative_path = redact_secret_like_content(&relative_path.display().to_string()).content;
    let redaction_reasons = redaction
        .redacted_reasons
        .iter()
        .map(|reason| (*reason).to_owned())
        .collect::<Vec<_>>();

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
        Value::String(text) => Value::String(redact_secret_like_content(text).content),
        Value::Array(items) => Value::Array(items.iter().map(redact_json_value).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), redact_json_value(value)))
                .collect(),
        ),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
    }
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
        SCALE_BENCHMARK_SUMMARY_FILE.to_owned(),
        SCALE_FIXTURE_MANIFEST_FILE.to_owned(),
        CACHE_REPORTS_FILE.to_owned(),
        WRITE_QUEUE_REPORT_FILE.to_owned(),
        PERFORMANCE_EXPLAIN_SAMPLES_FILE.to_owned(),
        TRIAGE_SUMMARY_FILE.to_owned(),
        MANIFEST_FILE.to_owned(),
    ]
}

fn generate_bundle_id() -> String {
    let now = Utc::now();
    format!("{}", now.format("%Y%m%d_%H%M%S"))
}

fn write_file_with_hash(path: &Path, content: &str) -> Result<u64, DomainError> {
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
            SCALE_BENCHMARK_SUMMARY_FILE,
            SCALE_FIXTURE_MANIFEST_FILE,
            CACHE_REPORTS_FILE,
            WRITE_QUEUE_REPORT_FILE,
            PERFORMANCE_EXPLAIN_SAMPLES_FILE,
            TRIAGE_SUMMARY_FILE,
        ] {
            assert!(
                files.contains(&required.to_owned()),
                "planned support-bundle files must include {required}"
            );
        }
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
    fn create_bundle_collects_scale_artifacts_and_detects_tamper() -> TestResult {
        let root = unique_test_path("scale-artifacts");
        let workspace = root.join("workspace");
        let report_dir = workspace.join(".ee").join("swarm-contention");
        fs::create_dir_all(&report_dir)
            .map_err(|error| format!("failed to create report dir: {error}"))?;

        let raw_secret = "api_key=sk_test_123";
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
            include_raw: false,
            audit_limit: 5,
        })
        .map_err(|error| error.message())?;

        for required in [
            SCALE_BENCHMARK_SUMMARY_FILE,
            SCALE_FIXTURE_MANIFEST_FILE,
            CACHE_REPORTS_FILE,
            WRITE_QUEUE_REPORT_FILE,
            PERFORMANCE_EXPLAIN_SAMPLES_FILE,
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
            !benchmark_summary.contains(raw_secret),
            "benchmark summary must not leak secret-like report content"
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

    fn unique_test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ee-support-bundle-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ))
    }
}
