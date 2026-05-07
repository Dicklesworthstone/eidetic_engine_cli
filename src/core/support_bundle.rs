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
use serde_json::json;

use crate::db::DbConnection;
use crate::models::DomainError;
use crate::output;
use crate::policy::redact_secret_like_content;

use super::doctor::DoctorReport;
use super::status::StatusReport;

pub const SUPPORT_BUNDLE_SCHEMA_V1: &str = "ee.support_bundle.v1";
pub const SUPPORT_BUNDLE_MANIFEST_SCHEMA_V1: &str = "ee.support_bundle.manifest.v1";
pub const SUPPORT_BUNDLE_INSPECT_SCHEMA_V1: &str = "ee.support_bundle.inspect.v1";

const MANIFEST_FILE: &str = "manifest.json";
const STATUS_FILE: &str = "status.json";
const DOCTOR_FILE: &str = "doctor.json";
const AUDIT_FILE: &str = "audit.jsonl";
const CAPABILITIES_FILE: &str = "capabilities.json";
const SCHEMA_FILE: &str = "schema_version.json";

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

    Ok(CollectedDiagnostics {
        status_json,
        doctor_json,
        audit_json,
        capabilities_json,
        schema_json,
    })
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
}
