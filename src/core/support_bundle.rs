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
use sqlmodel_core::{Row as SqlRow, Value as SqlValue};

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
const PERFORMANCE_EXPLAIN_SAMPLE_DIR: &str = "performance-explain";
const MAX_PERFORMANCE_EXPLAIN_SAMPLES: usize = 16;
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
    let cache_reports_json = cache_reports_json(workspace);
    let write_queue_report_json = write_queue_report_json();
    let performance_explain_samples_json = performance_explain_samples_json(workspace);
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
         WHERE m.workspace_id = ?1 AND m.tombstoned_at IS NULL
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
        "SELECT ml.src_memory_id, COUNT(*) AS hits
         FROM memory_links ml
         JOIN memories m ON m.id = ml.src_memory_id
         WHERE m.workspace_id = ?1 AND m.tombstoned_at IS NULL
         GROUP BY ml.src_memory_id
         ORDER BY hits DESC, ml.src_memory_id ASC
         LIMIT 4",
        &[SqlValue::Text(workspace_id.to_owned())],
    ) {
        entries.extend(rows.iter().filter_map(|row| {
            let memory_id = row_text(row, 0)?;
            Some(SearchHotsetEntry::graph_neighborhood(
                memory_id,
                2,
                generation,
                row_u64(row, 1).max(1),
            ))
        }));
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
                    "pack:selection_certificate:{pack_id}:{profile}:{max_tokens}:{used_tokens}:{item_count}:{pack_hash}"
                )),
                kind: PackHotsetEntryKind::SelectionCertificate,
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

fn discover_performance_explain_samples(workspace: &Path) -> Vec<Value> {
    let report_dir = workspace.join(".ee").join(PERFORMANCE_EXPLAIN_SAMPLE_DIR);
    let Ok(entries) = fs::read_dir(report_dir) else {
        return Vec::new();
    };

    let mut paths = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    paths.sort();
    paths.truncate(MAX_PERFORMANCE_EXPLAIN_SAMPLES);

    paths
        .iter()
        .filter_map(|path| summarize_performance_explain_sample(workspace, path))
        .collect()
}

fn summarize_performance_explain_sample(workspace: &Path, path: &Path) -> Option<Value> {
    let raw_content = fs::read_to_string(path).ok()?;
    let redaction = redact_secret_like_content(&raw_content);
    let parsed = serde_json::from_str::<Value>(&raw_content).ok()?;
    if parsed.get("schema") != Some(&json!(super::search::PERFORMANCE_EXPLAIN_SCHEMA_V1)) {
        return None;
    }
    let data = parsed.get("data")?;
    let relative_path = path.strip_prefix(workspace).unwrap_or(path);
    let relative_path = redact_secret_like_content(&relative_path.display().to_string()).content;
    let redaction_reasons = redaction
        .redacted_reasons
        .iter()
        .map(|reason| (*reason).to_owned())
        .collect::<Vec<_>>();
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
                .any(|entry| entry.pointer("/kind") == Some(&json!("selection_certificate"))),
            "pack hotset must include selection certificate entries from pack records"
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
                    "fingerprint": "blake3:samplequeryhash"
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
