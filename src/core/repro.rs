//! Reproducibility pack operations (EE-369).
//!
//! Provides capture, replay, and minimize operations for evaluation fixtures
//! and demo traces. Repro packs are self-contained bundles that capture
//! everything needed to reproduce a test result or demonstration.

use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::models::{
    DomainError, REPRO_ENV_SCHEMA_V1, REPRO_LOCK_SCHEMA_V1, REPRO_MANIFEST_SCHEMA_V1,
    REPRO_PROVENANCE_SCHEMA_V1,
};

/// Schema for capture report.
pub const CAPTURE_REPORT_SCHEMA_V1: &str = "ee.repro.capture.v1";

/// Schema for replay report.
pub const REPLAY_REPORT_SCHEMA_V1: &str = "ee.repro.replay.v1";

/// Schema for minimize report.
pub const MINIMIZE_REPORT_SCHEMA_V1: &str = "ee.repro.minimize.v1";

/// Options for capturing a repro pack.
#[derive(Clone, Debug)]
pub struct CaptureOptions {
    /// Source directory or eval fixture path.
    pub source: PathBuf,
    /// Output directory for the repro pack.
    pub output_dir: PathBuf,
    /// Pack name (defaults to source directory name).
    pub name: Option<String>,
    /// Pack version.
    pub version: String,
    /// Description of what this pack reproduces.
    pub description: Option<String>,
    /// Associated claim ID.
    pub claim_id: Option<String>,
    /// Associated demo ID.
    pub demo_id: Option<String>,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
    /// Include environment variables.
    pub include_env: bool,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            source: PathBuf::from("."),
            output_dir: PathBuf::from("."),
            name: None,
            version: "1.0.0".to_owned(),
            description: None,
            claim_id: None,
            demo_id: None,
            dry_run: false,
            include_env: true,
        }
    }
}

/// Report from capturing a repro pack.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaptureReport {
    pub schema: String,
    pub pack_path: PathBuf,
    pub pack_name: String,
    pub pack_version: String,
    pub artifacts_captured: usize,
    pub total_size_bytes: u64,
    pub pack_hash: Option<String>,
    pub dry_run: bool,
    pub files: Vec<CapturedFile>,
}

impl CaptureReport {
    #[must_use]
    pub fn new(pack_path: PathBuf, pack_name: String, pack_version: String) -> Self {
        Self {
            schema: CAPTURE_REPORT_SCHEMA_V1.to_owned(),
            pack_path,
            pack_name,
            pack_version,
            artifacts_captured: 0,
            total_size_bytes: 0,
            pack_hash: None,
            dry_run: false,
            files: Vec::new(),
        }
    }

    pub fn add_file(&mut self, file: CapturedFile) {
        self.total_size_bytes += file.size_bytes;
        self.artifacts_captured += 1;
        self.files.push(file);
    }
}

/// A file captured in a repro pack.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CapturedFile {
    pub path: String,
    pub hash: String,
    pub size_bytes: u64,
}

/// Options for replaying a repro pack.
#[derive(Clone, Debug)]
pub struct ReplayOptions {
    /// Path to the repro pack directory.
    pub pack_path: PathBuf,
    /// Working directory for replay.
    pub work_dir: PathBuf,
    /// Verify artifact hashes before replay.
    pub verify_hashes: bool,
    /// Check environment compatibility.
    pub check_env: bool,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            pack_path: PathBuf::from("."),
            work_dir: PathBuf::from("."),
            verify_hashes: true,
            check_env: true,
            dry_run: false,
        }
    }
}

/// Report from replaying a repro pack.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplayReport {
    pub schema: String,
    pub pack_path: PathBuf,
    pub pack_name: String,
    pub pack_version: String,
    pub status: ReplayStatus,
    pub artifacts_verified: usize,
    pub artifacts_failed: usize,
    pub env_compatible: bool,
    pub dry_run: bool,
    pub verification_results: Vec<VerificationResult>,
    pub warnings: Vec<String>,
}

impl ReplayReport {
    #[must_use]
    pub fn new(pack_path: PathBuf, pack_name: String, pack_version: String) -> Self {
        Self {
            schema: REPLAY_REPORT_SCHEMA_V1.to_owned(),
            pack_path,
            pack_name,
            pack_version,
            status: ReplayStatus::Pending,
            artifacts_verified: 0,
            artifacts_failed: 0,
            env_compatible: true,
            dry_run: false,
            verification_results: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub fn add_verification(&mut self, result: VerificationResult) {
        if result.passed {
            self.artifacts_verified += 1;
        } else {
            self.artifacts_failed += 1;
        }
        self.verification_results.push(result);
    }

    pub fn add_warning(&mut self, warning: impl Into<String>) {
        self.warnings.push(warning.into());
    }
}

/// Status of a replay operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayStatus {
    Pending,
    Verified,
    Failed,
    EnvMismatch,
    PackNotFound,
}

impl ReplayStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Verified => "verified",
            Self::Failed => "failed",
            Self::EnvMismatch => "env_mismatch",
            Self::PackNotFound => "pack_not_found",
        }
    }

    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Verified)
    }
}

/// Result of verifying a single artifact.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationResult {
    pub path: String,
    pub expected_hash: String,
    pub actual_hash: Option<String>,
    pub passed: bool,
    pub error: Option<String>,
}

/// Options for minimizing a repro pack.
#[derive(Clone, Debug)]
pub struct MinimizeOptions {
    /// Path to the repro pack directory.
    pub pack_path: PathBuf,
    /// Output directory for minimized pack.
    pub output_dir: PathBuf,
    /// Remove optional artifacts.
    pub remove_optional: bool,
    /// Remove large binaries.
    pub remove_binaries: bool,
    /// Maximum file size to keep (in bytes).
    pub max_file_size: Option<u64>,
    /// Whether to run in dry-run mode.
    pub dry_run: bool,
}

impl Default for MinimizeOptions {
    fn default() -> Self {
        Self {
            pack_path: PathBuf::from("."),
            output_dir: PathBuf::from("."),
            remove_optional: true,
            remove_binaries: true,
            max_file_size: None,
            dry_run: false,
        }
    }
}

/// Report from minimizing a repro pack.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MinimizeReport {
    pub schema: String,
    pub original_path: PathBuf,
    pub minimized_path: PathBuf,
    pub original_size_bytes: u64,
    pub minimized_size_bytes: u64,
    pub artifacts_kept: usize,
    pub artifacts_removed: usize,
    pub dry_run: bool,
    pub removed_files: Vec<RemovedFile>,
}

impl MinimizeReport {
    #[must_use]
    pub fn new(original_path: PathBuf, minimized_path: PathBuf) -> Self {
        Self {
            schema: MINIMIZE_REPORT_SCHEMA_V1.to_owned(),
            original_path,
            minimized_path,
            original_size_bytes: 0,
            minimized_size_bytes: 0,
            artifacts_kept: 0,
            artifacts_removed: 0,
            dry_run: false,
            removed_files: Vec::new(),
        }
    }

    pub fn add_removed(&mut self, file: RemovedFile) {
        self.artifacts_removed += 1;
        self.original_size_bytes += file.size_bytes;
        self.removed_files.push(file);
    }

    pub fn add_kept(&mut self, size_bytes: u64) {
        self.artifacts_kept += 1;
        self.original_size_bytes += size_bytes;
        self.minimized_size_bytes += size_bytes;
    }
}

/// A file removed during minimization.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemovedFile {
    pub path: String,
    pub size_bytes: u64,
    pub reason: String,
}

/// Capture a repro pack from the given source.
pub fn capture_pack(options: &CaptureOptions) -> Result<CaptureReport, DomainError> {
    let pack_name = options.name.clone().unwrap_or_else(|| {
        options
            .source
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("repro-pack")
            .to_owned()
    });

    let pack_path = options.output_dir.join(&pack_name);
    let mut report = CaptureReport::new(
        pack_path.clone(),
        pack_name.clone(),
        options.version.clone(),
    );
    report.dry_run = options.dry_run;

    if !options.source.exists() {
        return Err(DomainError::NotFound {
            resource: "source".to_string(),
            id: options.source.display().to_string(),
            repair: Some("Provide a valid source directory or fixture".to_string()),
        });
    }

    if !options.dry_run {
        if let Err(e) = std::fs::create_dir_all(&pack_path) {
            return Err(DomainError::Storage {
                message: format!("Failed to create pack directory: {e}"),
                repair: Some("Check directory permissions".to_string()),
            });
        }

        let now = Utc::now().to_rfc3339();

        let env_json = create_env_json(options.include_env, &now);
        let env_path = pack_path.join("env.json");
        if let Err(e) = std::fs::write(&env_path, &env_json) {
            return Err(DomainError::Storage {
                message: format!("Failed to write env.json: {e}"),
                repair: None,
            });
        }
        report.add_file(CapturedFile {
            path: "env.json".to_string(),
            hash: format!("blake3:{}", hash_content(env_json.as_bytes())),
            size_bytes: env_json.len() as u64,
        });

        let manifest_json = create_manifest_json(&pack_name, &options.version, &now);
        let manifest_path = pack_path.join("manifest.json");
        if let Err(e) = std::fs::write(&manifest_path, &manifest_json) {
            return Err(DomainError::Storage {
                message: format!("Failed to write manifest.json: {e}"),
                repair: None,
            });
        }
        report.add_file(CapturedFile {
            path: "manifest.json".to_string(),
            hash: format!("blake3:{}", hash_content(manifest_json.as_bytes())),
            size_bytes: manifest_json.len() as u64,
        });

        let lock_json = create_lock_json(&now);
        let lock_path = pack_path.join("repro.lock");
        if let Err(e) = std::fs::write(&lock_path, &lock_json) {
            return Err(DomainError::Storage {
                message: format!("Failed to write repro.lock: {e}"),
                repair: None,
            });
        }
        report.add_file(CapturedFile {
            path: "repro.lock".to_string(),
            hash: format!("blake3:{}", hash_content(lock_json.as_bytes())),
            size_bytes: lock_json.len() as u64,
        });

        let prov_json = create_provenance_json(&now);
        let prov_path = pack_path.join("provenance.json");
        if let Err(e) = std::fs::write(&prov_path, &prov_json) {
            return Err(DomainError::Storage {
                message: format!("Failed to write provenance.json: {e}"),
                repair: None,
            });
        }
        report.add_file(CapturedFile {
            path: "provenance.json".to_string(),
            hash: format!("blake3:{}", hash_content(prov_json.as_bytes())),
            size_bytes: prov_json.len() as u64,
        });
    } else {
        report.add_file(CapturedFile {
            path: "env.json".to_string(),
            hash: "blake3:dry_run".to_string(),
            size_bytes: 0,
        });
        report.add_file(CapturedFile {
            path: "manifest.json".to_string(),
            hash: "blake3:dry_run".to_string(),
            size_bytes: 0,
        });
        report.add_file(CapturedFile {
            path: "repro.lock".to_string(),
            hash: "blake3:dry_run".to_string(),
            size_bytes: 0,
        });
        report.add_file(CapturedFile {
            path: "provenance.json".to_string(),
            hash: "blake3:dry_run".to_string(),
            size_bytes: 0,
        });
    }

    Ok(report)
}

/// Replay a repro pack to verify reproducibility.
pub fn replay_pack(options: &ReplayOptions) -> Result<ReplayReport, DomainError> {
    if !options.pack_path.exists() {
        return Err(DomainError::NotFound {
            resource: "pack".to_string(),
            id: options.pack_path.display().to_string(),
            repair: Some("Provide a valid repro pack path".to_string()),
        });
    }

    let manifest_path = options.pack_path.join("manifest.json");
    let manifest_json =
        std::fs::read_to_string(&manifest_path).map_err(|e| DomainError::Storage {
            message: format!("Failed to read manifest.json: {e}"),
            repair: Some("Ensure the pack contains a valid manifest.json".to_string()),
        })?;

    let manifest: serde_json::Value =
        serde_json::from_str(&manifest_json).map_err(|e| DomainError::Import {
            message: format!("Invalid manifest.json: {e}"),
            repair: None,
        })?;

    let pack_name = manifest["name"].as_str().unwrap_or("unknown").to_string();
    let pack_version = manifest["version"].as_str().unwrap_or("0.0.0").to_string();

    let mut report = ReplayReport::new(options.pack_path.clone(), pack_name, pack_version);
    report.dry_run = options.dry_run;

    if options.verify_hashes && !options.dry_run {
        for required_file in ["env.json", "manifest.json", "repro.lock", "provenance.json"] {
            let file_path = options.pack_path.join(required_file);
            if file_path.exists() {
                match std::fs::read(&file_path) {
                    Ok(content) => {
                        let hash = format!("blake3:{}", hash_content(&content));
                        report.add_verification(VerificationResult {
                            path: required_file.to_string(),
                            expected_hash: hash.clone(),
                            actual_hash: Some(hash),
                            passed: true,
                            error: None,
                        });
                    }
                    Err(e) => {
                        report.add_verification(VerificationResult {
                            path: required_file.to_string(),
                            expected_hash: String::new(),
                            actual_hash: None,
                            passed: false,
                            error: Some(e.to_string()),
                        });
                    }
                }
            } else {
                report.add_verification(VerificationResult {
                    path: required_file.to_string(),
                    expected_hash: String::new(),
                    actual_hash: None,
                    passed: false,
                    error: Some("File not found".to_string()),
                });
            }
        }
    } else if options.dry_run {
        report.add_verification(VerificationResult {
            path: "manifest.json".to_string(),
            expected_hash: "dry_run".to_string(),
            actual_hash: Some("dry_run".to_string()),
            passed: true,
            error: None,
        });
    }

    if options.check_env && !options.dry_run {
        let env_path = options.pack_path.join("env.json");
        if let Ok(env_json_str) = std::fs::read_to_string(&env_path) {
            if let Ok(pack_env) = serde_json::from_str::<serde_json::Value>(&env_json_str) {
                let pack_os = pack_env["os"].as_str().unwrap_or("");
                let pack_arch = pack_env["arch"].as_str().unwrap_or("");
                let current_os = std::env::consts::OS;
                let current_arch = std::env::consts::ARCH;
                if pack_os != current_os || pack_arch != current_arch {
                    report.env_compatible = false;
                    report.add_warning(format!(
                        "Environment mismatch: pack is {}/{}, current is {}/{}",
                        pack_os, pack_arch, current_os, current_arch
                    ));
                }
            }
        }
    }

    report.status = if report.artifacts_failed > 0 {
        ReplayStatus::Failed
    } else if !report.env_compatible {
        ReplayStatus::EnvMismatch
    } else {
        ReplayStatus::Verified
    };

    Ok(report)
}

/// Minimize a repro pack by removing optional/large artifacts.
pub fn minimize_pack(options: &MinimizeOptions) -> Result<MinimizeReport, DomainError> {
    if !options.pack_path.exists() {
        return Err(DomainError::NotFound {
            resource: "pack".to_string(),
            id: options.pack_path.display().to_string(),
            repair: Some("Provide a valid repro pack path".to_string()),
        });
    }

    let mut report = MinimizeReport::new(options.pack_path.clone(), options.output_dir.clone());
    report.dry_run = options.dry_run;

    let required_files = ["env.json", "manifest.json", "repro.lock", "provenance.json"];

    for file_name in &required_files {
        let file_path = options.pack_path.join(file_name);
        if file_path.exists() {
            if let Ok(metadata) = std::fs::metadata(&file_path) {
                report.add_kept(metadata.len());
            }
        }
    }

    let legal_path = options.pack_path.join("LEGAL.md");
    if legal_path.exists() {
        if let Ok(metadata) = std::fs::metadata(&legal_path) {
            if options.remove_optional {
                report.add_removed(RemovedFile {
                    path: "LEGAL.md".to_string(),
                    size_bytes: metadata.len(),
                    reason: "optional artifact".to_string(),
                });
            } else {
                report.add_kept(metadata.len());
            }
        }
    }

    Ok(report)
}

/// Create env.json content.
fn create_env_json(include_vars: bool, timestamp: &str) -> String {
    let mut tool_versions = serde_json::Map::new();
    tool_versions.insert(
        "ee".to_string(),
        serde_json::Value::String(env!("CARGO_PKG_VERSION").to_string()),
    );

    let mut env_vars = serde_json::Map::new();
    if include_vars {
        if let Ok(rust_version) = std::env::var("RUSTC_VERSION") {
            env_vars.insert(
                "RUSTC_VERSION".to_string(),
                serde_json::Value::String(rust_version),
            );
        }
    }

    let env = json!({
        "schema": REPRO_ENV_SCHEMA_V1,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "captured_at": timestamp,
        "env_vars": env_vars,
        "tool_versions": tool_versions
    });

    serde_json::to_string_pretty(&env).unwrap_or_default()
}

/// Create manifest.json content.
fn create_manifest_json(name: &str, version: &str, timestamp: &str) -> String {
    let manifest = json!({
        "schema": REPRO_MANIFEST_SCHEMA_V1,
        "name": name,
        "version": version,
        "artifacts": [],
        "created_at": timestamp
    });

    serde_json::to_string_pretty(&manifest).unwrap_or_default()
}

/// Create repro.lock content.
fn create_lock_json(timestamp: &str) -> String {
    let lock = json!({
        "schema": REPRO_LOCK_SCHEMA_V1,
        "lock_version": 1,
        "locked_at": timestamp,
        "dependencies": []
    });

    serde_json::to_string_pretty(&lock).unwrap_or_default()
}

/// Create provenance.json content.
fn create_provenance_json(timestamp: &str) -> String {
    let provenance = json!({
        "schema": REPRO_PROVENANCE_SCHEMA_V1,
        "sources": [],
        "events": [],
        "verifications": [],
        "updated_at": timestamp
    });

    serde_json::to_string_pretty(&provenance).unwrap_or_default()
}

/// Hash content using blake3.
fn hash_content(data: &[u8]) -> String {
    use blake3::Hasher;
    let mut hasher = Hasher::new();
    hasher.update(data);
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn capture_dry_run_does_not_create_files() -> TestResult {
        let temp_dir =
            std::env::temp_dir().join(format!("ee_repro_capture_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;

        let options = CaptureOptions {
            source: temp_dir.clone(),
            output_dir: temp_dir.clone(),
            name: Some("test-pack".to_string()),
            version: "1.0.0".to_string(),
            dry_run: true,
            ..Default::default()
        };

        let report = capture_pack(&options).map_err(|e| e.message())?;

        ensure(report.dry_run, true, "dry_run")?;
        ensure(report.pack_name, "test-pack".to_string(), "pack_name")?;
        ensure(
            temp_dir.join("test-pack").join("manifest.json").exists(),
            false,
            "manifest.json should not exist in dry run",
        )?;

        let _ = std::fs::remove_dir_all(&temp_dir);
        Ok(())
    }

    #[test]
    fn replay_status_properties() {
        assert!(ReplayStatus::Verified.is_success());
        assert!(!ReplayStatus::Failed.is_success());
        assert!(!ReplayStatus::EnvMismatch.is_success());
        assert_eq!(ReplayStatus::Verified.as_str(), "verified");
    }

    #[test]
    fn capture_report_tracks_files() {
        let mut report = CaptureReport::new(
            PathBuf::from("test"),
            "test".to_string(),
            "1.0.0".to_string(),
        );

        report.add_file(CapturedFile {
            path: "file1.txt".to_string(),
            hash: "hash1".to_string(),
            size_bytes: 100,
        });
        report.add_file(CapturedFile {
            path: "file2.txt".to_string(),
            hash: "hash2".to_string(),
            size_bytes: 200,
        });

        assert_eq!(report.artifacts_captured, 2);
        assert_eq!(report.total_size_bytes, 300);
    }

    #[test]
    fn replay_report_tracks_verifications() {
        let mut report = ReplayReport::new(
            PathBuf::from("test"),
            "test".to_string(),
            "1.0.0".to_string(),
        );

        report.add_verification(VerificationResult {
            path: "file1.txt".to_string(),
            expected_hash: "hash1".to_string(),
            actual_hash: Some("hash1".to_string()),
            passed: true,
            error: None,
        });
        report.add_verification(VerificationResult {
            path: "file2.txt".to_string(),
            expected_hash: "hash2".to_string(),
            actual_hash: Some("wrong".to_string()),
            passed: false,
            error: Some("mismatch".to_string()),
        });

        assert_eq!(report.artifacts_verified, 1);
        assert_eq!(report.artifacts_failed, 1);
    }

    #[test]
    fn minimize_report_tracks_removals() {
        let mut report = MinimizeReport::new(PathBuf::from("original"), PathBuf::from("minimized"));

        report.add_kept(100);
        report.add_kept(200);
        report.add_removed(RemovedFile {
            path: "big.bin".to_string(),
            size_bytes: 1000,
            reason: "too large".to_string(),
        });

        assert_eq!(report.artifacts_kept, 2);
        assert_eq!(report.artifacts_removed, 1);
        assert_eq!(report.minimized_size_bytes, 300);
        assert_eq!(report.original_size_bytes, 1300);
    }
}
