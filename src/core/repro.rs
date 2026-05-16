//! Reproducibility pack operations (EE-369).
//!
//! Provides capture, replay, and minimize operations for evaluation fixtures
//! and demo traces. Repro packs are self-contained bundles that capture
//! everything needed to reproduce a test result or demonstration.

use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

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

#[derive(Clone, Debug, Eq, PartialEq)]
struct ManifestArtifactExpectation {
    hash: String,
    size_bytes: Option<u64>,
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

    let pack_path = resolve_pack_output_path(&options.output_dir, &pack_name)?;
    let mut report = CaptureReport::new(
        pack_path.clone(),
        pack_name.clone(),
        options.version.clone(),
    );
    report.dry_run = options.dry_run;

    validate_existing_repro_path(
        &options.source,
        "source",
        "Provide a valid source directory or fixture",
    )?;

    if !options.dry_run {
        reject_existing_symlink_components(&pack_path)
            .map_err(repro_symlink_refused_storage_error)?;
        if let Err(e) = fs::create_dir_all(&pack_path) {
            return Err(DomainError::Storage {
                message: format!("Failed to create pack directory: {e}"),
                repair: Some("Check directory permissions".to_string()),
            });
        }
        validate_pack_root(&pack_path)?;

        let now = Utc::now().to_rfc3339();

        let env_json = create_env_json(options.include_env, &now);
        let lock_json = create_lock_json(&now);
        let prov_json = create_provenance_json(&now);
        let env_file = captured_file_for_content("env.json", env_json.as_bytes());
        let lock_file = captured_file_for_content("repro.lock", lock_json.as_bytes());
        let provenance_file = captured_file_for_content("provenance.json", prov_json.as_bytes());
        let payload_files = vec![env_file.clone(), lock_file.clone(), provenance_file.clone()];
        let manifest_json =
            create_manifest_json(&pack_name, &options.version, &now, &payload_files);

        if let Err(e) = write_pack_file_no_symlinks(&pack_path, "env.json", env_json.as_bytes()) {
            return Err(DomainError::Storage {
                message: format!("Failed to write env.json: {e}"),
                repair: None,
            });
        }
        report.add_file(env_file);

        if let Err(e) =
            write_pack_file_no_symlinks(&pack_path, "manifest.json", manifest_json.as_bytes())
        {
            return Err(DomainError::Storage {
                message: format!("Failed to write manifest.json: {e}"),
                repair: None,
            });
        }
        report.add_file(CapturedFile {
            path: "manifest.json".to_string(),
            hash: format!("blake3:{}", hash_content(manifest_json.as_bytes())),
            size_bytes: len_to_u64(manifest_json.len()),
        });

        if let Err(e) = write_pack_file_no_symlinks(&pack_path, "repro.lock", lock_json.as_bytes())
        {
            return Err(DomainError::Storage {
                message: format!("Failed to write repro.lock: {e}"),
                repair: None,
            });
        }
        report.add_file(lock_file);

        if let Err(e) =
            write_pack_file_no_symlinks(&pack_path, "provenance.json", prov_json.as_bytes())
        {
            return Err(DomainError::Storage {
                message: format!("Failed to write provenance.json: {e}"),
                repair: None,
            });
        }
        report.add_file(provenance_file);
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
    validate_pack_root(&options.pack_path)?;

    let manifest_bytes =
        read_pack_file_no_symlinks(&options.pack_path, "manifest.json").map_err(|e| {
            DomainError::Storage {
                message: format!("Failed to read manifest.json: {e}"),
                repair: Some("Ensure the pack contains a valid manifest.json".to_string()),
            }
        })?;
    let manifest_json = String::from_utf8(manifest_bytes).map_err(|e| DomainError::Import {
        message: format!("manifest.json is not valid UTF-8: {e}"),
        repair: None,
    })?;

    let manifest: serde_json::Value =
        serde_json::from_str(&manifest_json).map_err(|e| DomainError::Import {
            message: format!("Invalid manifest.json: {e}"),
            repair: None,
        })?;
    let expected_artifacts = manifest_artifact_expectations(&manifest)?;

    let pack_name = manifest["name"].as_str().unwrap_or("unknown").to_string();
    let pack_version = manifest["version"].as_str().unwrap_or("0.0.0").to_string();

    let mut report = ReplayReport::new(options.pack_path.clone(), pack_name, pack_version);
    report.dry_run = options.dry_run;

    if options.verify_hashes && !options.dry_run {
        for required_file in ["env.json", "manifest.json", "repro.lock", "provenance.json"] {
            let result =
                verify_required_pack_file(&options.pack_path, required_file, &expected_artifacts);
            report.add_verification(result);
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
        if let Ok(env_bytes) = read_pack_file_no_symlinks(&options.pack_path, "env.json") {
            if let Ok(env_json_str) = String::from_utf8(env_bytes) {
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
    validate_pack_root(&options.pack_path)?;

    let mut report = MinimizeReport::new(options.pack_path.clone(), options.output_dir.clone());
    report.dry_run = options.dry_run;

    let required_files = ["env.json", "manifest.json", "repro.lock", "provenance.json"];

    for file_name in &required_files {
        if let Some(metadata) = pack_file_metadata_no_symlinks(&options.pack_path, file_name)? {
            report.add_kept(metadata.len());
        }
    }

    if let Some(metadata) = pack_file_metadata_no_symlinks(&options.pack_path, "LEGAL.md")? {
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

    Ok(report)
}

fn resolve_pack_output_path(output_dir: &Path, pack_name: &str) -> Result<PathBuf, DomainError> {
    if !is_single_component_pack_name(pack_name) {
        return Err(DomainError::Usage {
            message: format!("Invalid repro pack name `{pack_name}`."),
            repair: Some(
                "Use a simple directory name without path separators, roots, or `..` components."
                    .to_string(),
            ),
        });
    }
    Ok(output_dir.join(pack_name))
}

fn is_single_component_pack_name(pack_name: &str) -> bool {
    if pack_name.trim().is_empty() {
        return false;
    }
    let mut components = Path::new(pack_name).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn captured_file_for_content(path: &str, content: &[u8]) -> CapturedFile {
    CapturedFile {
        path: path.to_string(),
        hash: format!("blake3:{}", hash_content(content)),
        size_bytes: len_to_u64(content.len()),
    }
}

fn len_to_u64(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

fn validate_existing_repro_path(
    path: &Path,
    resource: &str,
    repair: &str,
) -> Result<(), DomainError> {
    reject_existing_symlink_components(path).map_err(repro_symlink_refused_storage_error)?;
    fs::symlink_metadata(path).map_err(|_| DomainError::NotFound {
        resource: resource.to_string(),
        id: path.display().to_string(),
        repair: Some(repair.to_string()),
    })?;
    Ok(())
}

fn reject_existing_symlink_components(path: &Path) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!("repro_path_symlink_refused: {}", current.display()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(format!(
                    "repro_path_unavailable: {}: {}",
                    current.display(),
                    error
                ));
            }
        }
    }
    Ok(())
}

fn repro_symlink_refused_storage_error(error: String) -> DomainError {
    DomainError::Storage {
        message: error,
        repair: Some("Use real repro pack paths without symbolic links".to_string()),
    }
}

fn validate_pack_root(pack_path: &Path) -> Result<(), DomainError> {
    reject_existing_symlink_components(pack_path).map_err(repro_symlink_refused_storage_error)?;
    let metadata = fs::symlink_metadata(pack_path).map_err(|_| DomainError::NotFound {
        resource: "pack".to_string(),
        id: pack_path.display().to_string(),
        repair: Some("Provide a valid repro pack path".to_string()),
    })?;
    if metadata.file_type().is_symlink() {
        return Err(DomainError::Storage {
            message: format!(
                "Repro pack path traverses a symbolic link: {}",
                pack_path.display()
            ),
            repair: Some("Use the real repro pack directory path".to_string()),
        });
    }
    if !metadata.is_dir() {
        return Err(DomainError::Storage {
            message: format!(
                "Repro pack path is not a directory: {}",
                pack_path.display()
            ),
            repair: Some("Provide a repro pack directory".to_string()),
        });
    }
    Ok(())
}

fn manifest_artifact_expectations(
    manifest: &serde_json::Value,
) -> Result<BTreeMap<String, ManifestArtifactExpectation>, DomainError> {
    let artifacts = manifest
        .get("artifacts")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| DomainError::Import {
            message: "manifest.json must contain an artifacts array".to_string(),
            repair: None,
        })?;

    let mut expected = BTreeMap::new();
    for (index, artifact) in artifacts.iter().enumerate() {
        let path = artifact
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| DomainError::Import {
                message: format!("manifest artifact at index {index} is missing path"),
                repair: None,
            })?;
        if !is_safe_pack_member_path(path) {
            return Err(DomainError::Import {
                message: format!("manifest artifact path is invalid: {path}"),
                repair: None,
            });
        }
        if artifact
            .get("required")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        {
            continue;
        }
        let hash = artifact
            .get("hash")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| DomainError::Import {
                message: format!("manifest artifact `{path}` is missing hash"),
                repair: None,
            })?
            .to_string();
        let size_bytes = artifact
            .get("size_bytes")
            .or_else(|| artifact.get("sizeBytes"))
            .or_else(|| artifact.get("bytes"))
            .and_then(serde_json::Value::as_u64);

        expected.insert(
            path.to_string(),
            ManifestArtifactExpectation { hash, size_bytes },
        );
    }
    Ok(expected)
}

fn verify_required_pack_file(
    pack_path: &Path,
    relative_path: &str,
    expected_artifacts: &BTreeMap<String, ManifestArtifactExpectation>,
) -> VerificationResult {
    match read_pack_file_no_symlinks(pack_path, relative_path) {
        Ok(content) if relative_path == "manifest.json" => {
            let actual_hash = format!("blake3:{}", hash_content(&content));
            VerificationResult {
                path: relative_path.to_string(),
                expected_hash: "manifest:parsed".to_string(),
                actual_hash: Some(actual_hash),
                passed: true,
                error: None,
            }
        }
        Ok(content) => {
            let actual_digest = hash_content(&content);
            let actual_hash = format!("blake3:{actual_digest}");
            let Some(expected) = expected_artifacts.get(relative_path) else {
                return VerificationResult {
                    path: relative_path.to_string(),
                    expected_hash: String::new(),
                    actual_hash: Some(actual_hash),
                    passed: false,
                    error: Some("manifest is missing required artifact hash".to_string()),
                };
            };
            let hash_matches = expected.hash.eq_ignore_ascii_case(&actual_hash)
                || expected.hash.eq_ignore_ascii_case(&actual_digest);
            let size_matches = expected
                .size_bytes
                .is_none_or(|expected_size| expected_size == len_to_u64(content.len()));
            let passed = hash_matches && size_matches;
            let error = if !hash_matches {
                Some("hash mismatch".to_string())
            } else if !size_matches {
                Some("size mismatch".to_string())
            } else {
                None
            };

            VerificationResult {
                path: relative_path.to_string(),
                expected_hash: expected.hash.clone(),
                actual_hash: Some(actual_hash),
                passed,
                error,
            }
        }
        Err(error) => VerificationResult {
            path: relative_path.to_string(),
            expected_hash: expected_artifacts
                .get(relative_path)
                .map(|expected| expected.hash.clone())
                .unwrap_or_default(),
            actual_hash: None,
            passed: false,
            error: Some(error),
        },
    }
}

fn read_pack_file_no_symlinks(pack_path: &Path, relative_path: &str) -> Result<Vec<u8>, String> {
    let target_path = resolve_pack_file_path_no_symlinks(pack_path, relative_path)?;
    let metadata = fs::symlink_metadata(&target_path).map_err(|error| {
        format!(
            "pack_artifact_unavailable: {}: {}",
            target_path.display(),
            error
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "pack_artifact_unavailable: {}: not a regular file",
            target_path.display()
        ));
    }
    fs::read(&target_path).map_err(|error| {
        format!(
            "pack_artifact_unavailable: {}: {}",
            target_path.display(),
            error
        )
    })
}

fn write_pack_file_no_symlinks(
    pack_path: &Path,
    relative_path: &str,
    content: &[u8],
) -> Result<(), String> {
    let target_path = resolve_pack_file_path_for_write_no_symlinks(pack_path, relative_path)?;
    fs::write(&target_path, content).map_err(|error| {
        format!(
            "pack_artifact_write_failed: {}: {}",
            target_path.display(),
            error
        )
    })
}

fn pack_file_metadata_no_symlinks(
    pack_path: &Path,
    relative_path: &str,
) -> Result<Option<fs::Metadata>, DomainError> {
    let Some(target_path) = resolve_optional_pack_file_path_no_symlinks(pack_path, relative_path)
        .map_err(|error| DomainError::Storage {
        message: error,
        repair: Some("Use real repro pack member paths without symbolic links".to_string()),
    })?
    else {
        return Ok(None);
    };
    fs::metadata(&target_path)
        .map(Some)
        .map_err(|error| DomainError::Storage {
            message: format!(
                "pack_artifact_metadata_unavailable: {}: {}",
                target_path.display(),
                error
            ),
            repair: None,
        })
}

fn resolve_pack_file_path_no_symlinks(
    pack_path: &Path,
    relative_path: &str,
) -> Result<PathBuf, String> {
    reject_pack_symlink_component(pack_path)?;
    let mut target_path = pack_path.to_path_buf();
    for component in Path::new(relative_path).components() {
        let Component::Normal(segment) = component else {
            return Err(format!("invalid pack member path: {relative_path}"));
        };
        target_path.push(segment);
        reject_pack_symlink_component(&target_path)?;
    }
    Ok(target_path)
}

fn resolve_pack_file_path_for_write_no_symlinks(
    pack_path: &Path,
    relative_path: &str,
) -> Result<PathBuf, String> {
    reject_pack_symlink_component(pack_path)?;
    let mut target_path = pack_path.to_path_buf();
    let mut components = Path::new(relative_path).components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(segment) = component else {
            return Err(format!("invalid pack member path: {relative_path}"));
        };
        target_path.push(segment);
        match fs::symlink_metadata(&target_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!("pack_symlink_refused: {}", target_path.display()));
            }
            Ok(_) => {}
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && components.peek().is_none() => {}
            Err(error) => {
                return Err(format!(
                    "pack_artifact_not_found: {}: {}",
                    target_path.display(),
                    error
                ));
            }
        }
    }
    Ok(target_path)
}

fn resolve_optional_pack_file_path_no_symlinks(
    pack_path: &Path,
    relative_path: &str,
) -> Result<Option<PathBuf>, String> {
    reject_pack_symlink_component(pack_path)?;
    let mut target_path = pack_path.to_path_buf();
    let mut components = Path::new(relative_path).components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(segment) = component else {
            return Err(format!("invalid pack member path: {relative_path}"));
        };
        target_path.push(segment);
        match fs::symlink_metadata(&target_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!("pack_symlink_refused: {}", target_path.display()));
            }
            Ok(_) => {}
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && components.peek().is_none() =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(format!(
                    "pack_artifact_not_found: {}: {}",
                    target_path.display(),
                    error
                ));
            }
        }
    }
    Ok(Some(target_path))
}

fn reject_pack_symlink_component(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(format!("pack_symlink_refused: {}", path.display()))
        }
        Ok(_) => Ok(()),
        Err(error) => Err(format!(
            "pack_artifact_not_found: {}: {}",
            path.display(),
            error
        )),
    }
}

fn is_safe_pack_member_path(path: &str) -> bool {
    if path.trim().is_empty() {
        return false;
    }
    Path::new(path)
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
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

    crate::core::serialize_pretty_or_error(&env)
}

/// Create manifest.json content.
fn create_manifest_json(
    name: &str,
    version: &str,
    timestamp: &str,
    artifacts: &[CapturedFile],
) -> String {
    let artifacts = artifacts
        .iter()
        .map(|artifact| {
            json!({
                "path": artifact.path.as_str(),
                "hash": artifact.hash.as_str(),
                "size_bytes": artifact.size_bytes,
                "required": true
            })
        })
        .collect::<Vec<_>>();
    let manifest = json!({
        "schema": REPRO_MANIFEST_SCHEMA_V1,
        "name": name,
        "version": version,
        "artifacts": artifacts,
        "created_at": timestamp
    });

    crate::core::serialize_pretty_or_error(&manifest)
}

/// Create repro.lock content.
fn create_lock_json(timestamp: &str) -> String {
    let lock = json!({
        "schema": REPRO_LOCK_SCHEMA_V1,
        "lock_version": 1,
        "locked_at": timestamp,
        "dependencies": []
    });

    crate::core::serialize_pretty_or_error(&lock)
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

    crate::core::serialize_pretty_or_error(&provenance)
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
        let temp_dir = tempfile::Builder::new()
            .prefix("ee_repro_capture_")
            .tempdir()
            .map(tempfile::TempDir::keep)
            .map_err(|e| e.to_string())?;

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

    #[cfg(unix)]
    fn temp_root(prefix: &str) -> Result<PathBuf, String> {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .map(tempfile::TempDir::keep)
            .map_err(|e| e.to_string())
    }

    #[cfg(unix)]
    #[test]
    fn capture_pack_rejects_symlinked_output_parent() -> TestResult {
        let workspace = temp_root("ee_repro_capture_symlink_parent_")?;
        let source = workspace.join("source");
        let real_output = workspace.join("real-output");
        let symlink_output = workspace.join("out-link");
        fs::create_dir_all(&source).map_err(|e| e.to_string())?;
        fs::create_dir_all(&real_output).map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink(&real_output, &symlink_output).map_err(|e| e.to_string())?;

        let error = capture_pack(&CaptureOptions {
            source,
            output_dir: symlink_output,
            name: Some("pack".to_string()),
            version: "1.0.0".to_string(),
            dry_run: false,
            ..Default::default()
        })
        .expect_err("symlinked output parent must be rejected");

        assert_eq!(error.code(), "storage");
        assert!(error.message().contains("symlink"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn capture_pack_rejects_symlinked_pack_directory() -> TestResult {
        let workspace = temp_root("ee_repro_capture_symlink_pack_")?;
        let source = workspace.join("source");
        let output = workspace.join("output");
        let real_pack = workspace.join("real-pack");
        fs::create_dir_all(&source).map_err(|e| e.to_string())?;
        fs::create_dir_all(&output).map_err(|e| e.to_string())?;
        fs::create_dir_all(&real_pack).map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink(&real_pack, output.join("pack")).map_err(|e| e.to_string())?;

        let error = capture_pack(&CaptureOptions {
            source,
            output_dir: output,
            name: Some("pack".to_string()),
            version: "1.0.0".to_string(),
            dry_run: false,
            ..Default::default()
        })
        .expect_err("symlinked pack directory must be rejected");

        assert_eq!(error.code(), "storage");
        assert!(error.message().contains("symlink"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn replay_pack_rejects_non_regular_manifest_member() -> TestResult {
        let workspace = temp_root("ee_repro_replay_non_regular_manifest_")?;
        let pack = workspace.join("pack");
        fs::create_dir_all(pack.join("manifest.json")).map_err(|e| e.to_string())?;

        let error = replay_pack(&ReplayOptions {
            pack_path: pack,
            dry_run: false,
            verify_hashes: true,
            check_env: false,
            ..Default::default()
        })
        .expect_err("non-regular manifest member must be rejected");

        assert_eq!(error.code(), "storage");
        assert!(error.message().contains("regular file"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn minimize_pack_rejects_symlinked_required_member() -> TestResult {
        let workspace = temp_root("ee_repro_minimize_symlink_member_")?;
        let pack = workspace.join("pack");
        fs::create_dir_all(&pack).map_err(|e| e.to_string())?;
        let external_env = workspace.join("external-env.json");
        fs::write(&external_env, "{}\n").map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink(&external_env, pack.join("env.json"))
            .map_err(|e| e.to_string())?;

        let error = minimize_pack(&MinimizeOptions {
            pack_path: pack,
            output_dir: workspace.join("minimized"),
            dry_run: true,
            ..Default::default()
        })
        .expect_err("symlinked required pack member must be rejected");

        assert_eq!(error.code(), "storage");
        assert!(error.message().contains("symlink"));
        Ok(())
    }
}
