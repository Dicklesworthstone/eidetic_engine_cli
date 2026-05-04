//! Agent-safe install and update checks.

use std::cmp::Ordering;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::core::build_info;
use crate::models::{
    INSTALL_CHECK_SCHEMA_V1, INSTALL_PLAN_SCHEMA_V1, InstallArtifactSelection, InstallCheckReport,
    InstallFinding, InstallFindingCode, InstallOperation, InstallPathAnalysis, InstallPathStatus,
    InstallPermissionCheck, InstallPermissionStatus, InstallPlanReport, InstallPlanStatus,
    InstallTarget, InstallVerificationPlan, PathBinary, PlannedInstallOperation,
    RELEASE_BINARY_NAME, RELEASE_MANIFEST_SCHEMA_V1, ReleaseManifest, ReleaseVerificationCode,
    ReleaseVerificationSeverity, UPDATE_PLAN_SCHEMA_V1, UpdateSourcePosture, compare_versions,
    is_safe_install_path, is_safe_release_artifact_path, is_supported_release_target,
};

const TRUSTED_TAR_PATHS: &[&str] = &["/usr/bin/tar", "/bin/tar"];
const TRUSTED_INSTALL_TOOL_PATH: &str = "/usr/bin:/bin";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InstallCheckOptions {
    pub install_dir: Option<PathBuf>,
    pub current_binary: Option<PathBuf>,
    pub path_env: Option<OsString>,
    pub target_triple: Option<String>,
    pub manifest: Option<PathBuf>,
    pub offline: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallPlanOptions {
    pub operation: InstallOperation,
    pub manifest: Option<PathBuf>,
    pub artifact_root: Option<PathBuf>,
    pub install_dir: Option<PathBuf>,
    pub current_binary: Option<PathBuf>,
    pub target_triple: Option<String>,
    pub target_version: Option<String>,
    pub pinned_version: Option<String>,
    pub allow_downgrade: bool,
    pub offline: bool,
}

impl Default for InstallPlanOptions {
    fn default() -> Self {
        Self {
            operation: InstallOperation::Install,
            manifest: None,
            artifact_root: None,
            install_dir: None,
            current_binary: None,
            target_triple: None,
            target_version: None,
            pinned_version: None,
            allow_downgrade: false,
            offline: false,
        }
    }
}

#[must_use]
pub fn check_install(options: &InstallCheckOptions) -> InstallCheckReport {
    let info = build_info();
    let target_triple = selected_target_triple(options.target_triple.as_deref());
    let install_dir = options
        .install_dir
        .clone()
        .unwrap_or_else(default_install_dir);
    let current_binary = options
        .current_binary
        .clone()
        .or_else(|| env::current_exe().ok());
    let target = install_target(&target_triple, &install_dir);
    let path = analyze_path(
        &target.executable_name,
        current_binary.as_deref(),
        options.path_env.clone().or_else(|| env::var_os("PATH")),
    );
    let permissions = check_permissions(&install_dir, &target.install_path);
    let update_source = UpdateSourcePosture {
        configured: options.manifest.is_some(),
        offline: options.offline,
        source: options
            .manifest
            .as_ref()
            .map(|path| normalize_path(path.as_path())),
        status: if options.manifest.is_some() {
            "manifest_configured".to_owned()
        } else if options.offline {
            "offline_no_manifest".to_owned()
        } else {
            "not_configured".to_owned()
        },
    };
    let mut findings = Vec::new();

    if !target.supported {
        findings.push(InstallFinding::error(
            InstallFindingCode::UnsupportedTarget,
            format!(
                "target triple '{}' is not supported by release manifests",
                target_triple
            ),
            "Use a supported target or add an explicit release compatibility contract.",
        ));
    }

    if path.binaries.is_empty() {
        findings.push(InstallFinding::warning(
            InstallFindingCode::BinaryNotOnPath,
            format!("no '{}' binary was found in PATH", target.executable_name),
            "Install into a PATH directory or update PATH explicitly after install.",
        ));
    } else if path.duplicate_count > 1 {
        findings.push(InstallFinding::warning(
            InstallFindingCode::DuplicatePathBinary,
            format!(
                "{} '{}' binaries were found in PATH",
                path.duplicate_count, target.executable_name
            ),
            "Remove stale duplicates or make the intended install directory appear first in PATH.",
        ));
    }

    if matches!(
        permissions.status,
        InstallPermissionStatus::MissingParentUnknown | InstallPermissionStatus::NotWritable
    ) {
        findings.push(InstallFinding::error(
            InstallFindingCode::InstallDirNotWritable,
            format!("install target '{}' is not writable", permissions.target_path),
            "Choose a writable --install-dir or create the parent directory with appropriate permissions.",
        ));
    } else if matches!(
        permissions.status,
        InstallPermissionStatus::MissingParentWritable
    ) {
        findings.push(InstallFinding::warning(
            InstallFindingCode::InstallDirMissing,
            format!(
                "install directory '{}' does not exist",
                permissions.install_dir
            ),
            "Create the install directory before applying an install plan.",
        ));
    }

    if options.manifest.is_none() {
        findings.push(InstallFinding::info(
            if options.offline {
                InstallFindingCode::OfflineNoManifest
            } else {
                InstallFindingCode::NoUpdateSourceConfigured
            },
            "no release manifest source is configured for update checks",
            "Pass --manifest for deterministic offline update planning.",
        ));
    }

    InstallCheckReport {
        command: "install check".to_owned(),
        schema: INSTALL_CHECK_SCHEMA_V1.to_owned(),
        version: info.version.to_owned(),
        current_binary: crate::models::CurrentBinary {
            path: current_binary.as_deref().map(normalize_path),
            version: info.version.to_owned(),
            source: "running_process".to_owned(),
        },
        target,
        path,
        permissions,
        update_source,
        findings,
    }
}

#[must_use]
pub fn plan_install(options: &InstallPlanOptions) -> InstallPlanReport {
    let info = build_info();
    let target_triple = selected_target_triple(options.target_triple.as_deref());
    let install_dir = options
        .install_dir
        .clone()
        .unwrap_or_else(default_install_dir);
    let target = install_target(&target_triple, &install_dir);
    let current_version = info.version.to_owned();
    let mut findings = Vec::new();
    let mut artifact = None;
    let mut manifest_status = "missing".to_owned();
    let mut checksum_status = "not_checked".to_owned();
    let mut signature_status = "not_checked".to_owned();
    let mut target_status = if target.supported {
        "supported".to_owned()
    } else {
        findings.push(InstallFinding::error(
            InstallFindingCode::UnsupportedTarget,
            format!(
                "target triple '{}' is not supported by release manifests",
                target_triple
            ),
            "Use a supported target or add an explicit release compatibility contract.",
        ));
        "unsupported".to_owned()
    };
    let mut target_version = options
        .target_version
        .clone()
        .or_else(|| options.pinned_version.clone());

    if !is_safe_install_path(Path::new(&target.install_path)) {
        findings.push(InstallFinding::error(
            InstallFindingCode::UnsafeTargetPath,
            format!(
                "install target '{}' contains unsafe path components",
                target.install_path
            ),
            "Choose an absolute install directory without traversal components.",
        ));
    }

    if let Some(manifest_path) = &options.manifest {
        match load_manifest(manifest_path, &target_triple, &mut findings) {
            Ok(manifest) => {
                manifest_status = "loaded".to_owned();
                target_version = target_version.or(Some(manifest.release_version.clone()));
                let verification = manifest.verify(options.artifact_root.as_deref());
                for finding in &verification.findings {
                    findings.push(map_release_finding(finding));
                }

                if let Some(selected) = manifest
                    .artifacts
                    .iter()
                    .find(|artifact| artifact.target_triple == target_triple)
                {
                    if !is_safe_release_artifact_path(&selected.file_name) {
                        findings.push(InstallFinding::error(
                            InstallFindingCode::UnsafeArtifact,
                            format!("artifact path '{}' is unsafe", selected.file_name),
                            "Regenerate the manifest with safe release artifact names.",
                        ));
                    }

                    checksum_status = if options.artifact_root.is_some() {
                        if verification.findings.iter().any(|finding| {
                            matches!(
                                finding.code,
                                ReleaseVerificationCode::ChecksumMismatch
                                    | ReleaseVerificationCode::InvalidChecksum
                                    | ReleaseVerificationCode::MissingArtifact
                            )
                        }) {
                            "failed".to_owned()
                        } else {
                            "verified".to_owned()
                        }
                    } else {
                        findings.push(InstallFinding::warning(
                            InstallFindingCode::ChecksumVerificationPending,
                            "artifact checksum cannot be verified without --artifact-root",
                            "Pass --artifact-root pointing at downloaded release artifacts before apply.",
                        ));
                        "planned".to_owned()
                    };
                    signature_status = if selected.signature.is_some() {
                        "present".to_owned()
                    } else {
                        "missing".to_owned()
                    };
                    target_status = "matched".to_owned();
                    artifact = Some(InstallArtifactSelection {
                        artifact_id: selected.artifact_id.clone(),
                        release_version: selected.release_version.clone(),
                        file_name: selected.file_name.clone(),
                        target_triple: selected.target_triple.clone(),
                        archive_format: selected.archive_format.as_str().to_owned(),
                        checksum_algorithm: selected.checksum.algorithm.as_str().to_owned(),
                        checksum: selected.checksum.value.clone(),
                        signature: signature_status.clone(),
                    });
                } else {
                    target_status = "missing_artifact".to_owned();
                    findings.push(InstallFinding::error(
                        InstallFindingCode::TargetMismatch,
                        format!("manifest has no artifact for target '{}'", target_triple),
                        "Choose a target from the manifest or build the missing artifact.",
                    ));
                }
            }
            Err(finding) => {
                manifest_status = if finding.code == InstallFindingCode::ManifestMissing {
                    "missing".to_owned()
                } else {
                    "invalid".to_owned()
                };
                findings.push(finding);
            }
        }
    } else {
        findings.push(InstallFinding::error(
            if options.offline {
                InstallFindingCode::OfflineNoManifest
            } else {
                InstallFindingCode::ManifestMissing
            },
            "no release manifest was supplied",
            "Pass --manifest to plan from a verified release manifest.",
        ));
    }

    if let Some(target_version) = target_version.as_deref()
        && compare_versions(&current_version, target_version) == Ordering::Greater
        && !options.allow_downgrade
    {
        findings.push(InstallFinding::error(
            InstallFindingCode::WouldDowngrade,
            format!(
                "target version '{}' is older than current version '{}'",
                target_version, current_version
            ),
            "Pass --allow-downgrade with an explicit --pin only when rollback is intentional.",
        ));
    }

    let overwrite_status = overwrite_status(
        &target.install_path,
        options.current_binary.as_deref(),
        artifact.is_some(),
        &mut findings,
    );
    let mut status = crate::models::findings_status(&findings);
    if status == InstallPlanStatus::Ready
        && target_version
            .as_deref()
            .is_some_and(|version| compare_versions(&current_version, version) == Ordering::Equal)
    {
        status = InstallPlanStatus::Idempotent;
    }

    let planned_operations = if artifact.is_some() {
        vec![
            PlannedInstallOperation {
                action: "verify_archive".to_owned(),
                path: artifact
                    .as_ref()
                    .map(|artifact| artifact.file_name.clone())
                    .unwrap_or_default(),
                mode: "read_only".to_owned(),
                requires_verification: true,
            },
            PlannedInstallOperation {
                action: "write_binary".to_owned(),
                path: target.install_path.clone(),
                mode: "apply_requires_explicit_future_command".to_owned(),
                requires_verification: true,
            },
        ]
    } else {
        Vec::new()
    };

    let verification = InstallVerificationPlan {
        manifest_status,
        checksum_status,
        signature_status,
        target_status,
        overwrite_status,
    };
    let schema = match options.operation {
        InstallOperation::Install => INSTALL_PLAN_SCHEMA_V1,
        InstallOperation::Update => UPDATE_PLAN_SCHEMA_V1,
    };
    let command = match options.operation {
        InstallOperation::Install => "install plan",
        InstallOperation::Update => "update",
    };
    let idempotency_key = install_idempotency_key(
        options.operation,
        target_version.as_deref(),
        &target.target_triple,
        &target.install_path,
        artifact
            .as_ref()
            .map(|artifact| artifact.artifact_id.as_str()),
    );

    InstallPlanReport {
        command: command.to_owned(),
        schema: schema.to_owned(),
        version: info.version.to_owned(),
        operation: options.operation,
        dry_run: true,
        status,
        current_version,
        target_version,
        pinned_version: options.pinned_version.clone(),
        target,
        artifact,
        verification,
        planned_operations,
        idempotency_key,
        rollback: "side_path_before_replace".to_owned(),
        findings,
    }
}

#[must_use]
pub fn selected_target_triple(override_value: Option<&str>) -> String {
    override_value
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            let build = build_info();
            if build.target_triple != "unknown" {
                build.target_triple.to_owned()
            } else {
                inferred_target_triple()
            }
        })
}

#[must_use]
pub fn install_idempotency_key(
    operation: InstallOperation,
    target_version: Option<&str>,
    target_triple: &str,
    install_path: &str,
    artifact_id: Option<&str>,
) -> String {
    let mut input = String::new();
    input.push_str(operation.as_str());
    input.push('|');
    input.push_str(target_version.unwrap_or("unknown"));
    input.push('|');
    input.push_str(target_triple);
    input.push('|');
    input.push_str(install_path);
    input.push('|');
    input.push_str(artifact_id.unwrap_or("none"));
    let hash = blake3::hash(input.as_bytes()).to_hex().to_string();
    format!("install_{}", &hash[..24])
}

fn install_target(target_triple: &str, install_dir: &Path) -> InstallTarget {
    let executable_name = if target_triple.contains("windows") {
        format!("{RELEASE_BINARY_NAME}.exe")
    } else {
        RELEASE_BINARY_NAME.to_owned()
    };
    let install_path = if target_triple.contains("windows") {
        install_dir.join("ee.exe")
    } else {
        install_dir.join("ee")
    };
    InstallTarget {
        target_triple: target_triple.to_owned(),
        supported: is_supported_release_target(target_triple),
        binary_name: RELEASE_BINARY_NAME.to_owned(),
        executable_name,
        install_dir: normalize_path(install_dir),
        install_path: normalize_path(&install_path),
    }
}

fn analyze_path(
    executable_name: &str,
    current_binary: Option<&Path>,
    path_env: Option<OsString>,
) -> InstallPathAnalysis {
    let entries: Vec<PathBuf> = path_env
        .as_ref()
        .map(|raw| env::split_paths(raw).collect())
        .unwrap_or_default();
    let current = current_binary.map(normalize_path);
    let mut binaries = Vec::new();
    for (ordinal, entry) in entries.iter().enumerate() {
        let candidate = entry.join(executable_name);
        if candidate.is_file() {
            let path = normalize_path(&candidate);
            binaries.push(PathBinary {
                is_current_binary: current.as_ref() == Some(&path),
                path,
                ordinal,
            });
        }
    }
    let current_binary_on_path = binaries.iter().any(|binary| binary.is_current_binary);
    let first_binary = binaries.first().map(|binary| binary.path.clone());
    let duplicate_count = binaries.len();
    let status = if binaries.is_empty() {
        InstallPathStatus::Missing
    } else if duplicate_count > 1 {
        InstallPathStatus::Duplicate
    } else if current.is_some() && !current_binary_on_path {
        InstallPathStatus::Shadowed
    } else {
        InstallPathStatus::Ok
    };

    InstallPathAnalysis {
        status,
        path_entries: entries.iter().map(|path| normalize_path(path)).collect(),
        binaries,
        first_binary,
        current_binary_on_path,
        duplicate_count,
    }
}

fn check_permissions(install_dir: &Path, install_path: &str) -> InstallPermissionCheck {
    let metadata = fs::metadata(install_dir);
    let (status, exists, writable) = match metadata {
        Ok(metadata) => {
            let writable = metadata.is_dir() && !metadata.permissions().readonly();
            (
                if writable {
                    InstallPermissionStatus::Writable
                } else {
                    InstallPermissionStatus::NotWritable
                },
                true,
                writable,
            )
        }
        Err(_) => match install_dir
            .parent()
            .and_then(|parent| fs::metadata(parent).ok())
        {
            Some(parent) if parent.is_dir() && !parent.permissions().readonly() => {
                (InstallPermissionStatus::MissingParentWritable, false, false)
            }
            _ => (InstallPermissionStatus::MissingParentUnknown, false, false),
        },
    };

    InstallPermissionCheck {
        status,
        install_dir: normalize_path(install_dir),
        target_path: install_path.to_owned(),
        exists,
        writable,
    }
}

fn load_manifest(
    path: &Path,
    target_triple: &str,
    findings: &mut Vec<InstallFinding>,
) -> Result<ReleaseManifest, InstallFinding> {
    let raw = fs::read_to_string(path).map_err(|error| {
        InstallFinding::error(
            InstallFindingCode::ManifestMissing,
            format!(
                "failed to read release manifest '{}': {error}",
                path.display()
            ),
            "Pass a readable --manifest path.",
        )
    })?;
    collect_manifest_shape_findings(&raw, target_triple, findings);
    serde_json::from_str(&raw).map_err(|error| {
        InstallFinding::error(
            InstallFindingCode::ManifestInvalid,
            format!(
                "release manifest '{}' is invalid JSON: {error}",
                path.display()
            ),
            "Regenerate the release manifest or pass a valid ee.release_manifest.v1 file.",
        )
    })
}

fn collect_manifest_shape_findings(
    raw: &str,
    target_triple: &str,
    findings: &mut Vec<InstallFinding>,
) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return;
    };
    if value.get("schema").and_then(serde_json::Value::as_str) != Some(RELEASE_MANIFEST_SCHEMA_V1) {
        return;
    }

    let Some(artifacts) = value.get("artifacts").and_then(serde_json::Value::as_array) else {
        return;
    };
    if artifacts.is_empty() {
        findings.push(InstallFinding::error(
            InstallFindingCode::NoArtifacts,
            "release manifest contains no artifacts",
            "Regenerate the release manifest after packaging at least one supported target.",
        ));
        return;
    }

    let matching_targets = artifacts
        .iter()
        .filter(|artifact| manifest_artifact_target(artifact) == Some(target_triple))
        .count();
    if matching_targets > 1 {
        findings.push(InstallFinding::warning(
            InstallFindingCode::DuplicateTarget,
            format!(
                "release manifest contains {matching_targets} artifacts for target '{target_triple}'"
            ),
            "Keep one artifact per target triple or split variants behind explicit target names.",
        ));
    }
}

fn manifest_artifact_target(artifact: &serde_json::Value) -> Option<&str> {
    artifact
        .get("targetTriple")
        .or_else(|| artifact.get("target"))
        .and_then(serde_json::Value::as_str)
}

fn map_release_finding(finding: &crate::models::ReleaseVerificationFinding) -> InstallFinding {
    let code = match finding.code {
        ReleaseVerificationCode::ChecksumMismatch => InstallFindingCode::ArtifactChecksumMismatch,
        ReleaseVerificationCode::MissingArtifact => InstallFindingCode::ArtifactMissing,
        ReleaseVerificationCode::SignatureMissing => InstallFindingCode::SignatureMissing,
        ReleaseVerificationCode::UnsupportedTarget => InstallFindingCode::UnsupportedTarget,
        ReleaseVerificationCode::UnsafeArtifactPath => InstallFindingCode::UnsafeArtifact,
        ReleaseVerificationCode::InvalidManifestJson
        | ReleaseVerificationCode::InvalidManifestSchema
        | ReleaseVerificationCode::UnsupportedFutureManifestVersion => {
            InstallFindingCode::ManifestInvalid
        }
        _ => InstallFindingCode::UnsafeArtifact,
    };
    match finding.severity {
        ReleaseVerificationSeverity::Warning => {
            InstallFinding::warning(code, finding.message.clone(), finding.repair.clone())
        }
        ReleaseVerificationSeverity::Error => {
            InstallFinding::error(code, finding.message.clone(), finding.repair.clone())
        }
    }
}

fn overwrite_status(
    target_path: &str,
    current_binary: Option<&Path>,
    artifact_selected: bool,
    findings: &mut Vec<InstallFinding>,
) -> String {
    let target = Path::new(target_path);
    if !target.exists() {
        return "new_file".to_owned();
    }
    if current_binary
        .map(normalize_path)
        .as_deref()
        .is_some_and(|current| current == target_path)
    {
        return "managed_current_binary".to_owned();
    }
    if artifact_selected {
        findings.push(InstallFinding::error(
            InstallFindingCode::ExistingUnknownFile,
            format!(
                "target path '{}' already exists and is not the running ee binary",
                target_path
            ),
            "Move the existing file aside manually or choose an empty --install-dir.",
        ));
    }
    "existing_unknown_file".to_owned()
}

fn default_install_dir() -> PathBuf {
    if cfg!(windows) {
        env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Programs")
            .join("ee")
    } else {
        env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".local")
            .join("bin")
    }
}

fn inferred_target_triple() -> String {
    match (env::consts::ARCH, env::consts::OS) {
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu".to_owned(),
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu".to_owned(),
        ("x86_64", "macos") => "x86_64-apple-darwin".to_owned(),
        ("aarch64", "macos") => "aarch64-apple-darwin".to_owned(),
        ("x86_64", "windows") => "x86_64-pc-windows-msvc".to_owned(),
        (arch, os) => format!("{arch}-unknown-{os}"),
    }
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Result of executing an install/update plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallExecutionResult {
    pub success: bool,
    pub artifact_verified: bool,
    pub binary_installed: bool,
    pub backup_path: Option<String>,
    pub error_message: Option<String>,
}

/// Execute a verified install plan, installing the binary from the artifact root.
///
/// Pre-conditions:
/// - `plan.status` must be `Ready` or `Idempotent`
/// - `artifact_root` must contain the artifact named in the plan
/// - The artifact must pass checksum verification
///
/// Steps:
/// 1. Verify artifact checksum
/// 2. Extract binary from archive
/// 3. Back up existing binary (if present)
/// 4. Install new binary with executable permissions
pub fn execute_install_plan(
    plan: &InstallPlanReport,
    artifact_root: &Path,
) -> InstallExecutionResult {
    if plan.status != InstallPlanStatus::Ready && plan.status != InstallPlanStatus::Idempotent {
        return InstallExecutionResult {
            success: false,
            artifact_verified: false,
            binary_installed: false,
            backup_path: None,
            error_message: Some(format!(
                "plan status '{}' is not executable; status must be 'ready' or 'idempotent'",
                plan.status.as_str()
            )),
        };
    }

    let artifact = match &plan.artifact {
        Some(artifact) => artifact,
        None => {
            return InstallExecutionResult {
                success: false,
                artifact_verified: false,
                binary_installed: false,
                backup_path: None,
                error_message: Some("no artifact selected in plan".to_owned()),
            };
        }
    };

    let artifact_path = artifact_root.join(&artifact.file_name);
    if !artifact_path.is_file() {
        return InstallExecutionResult {
            success: false,
            artifact_verified: false,
            binary_installed: false,
            backup_path: None,
            error_message: Some(format!(
                "artifact '{}' not found in artifact root '{}'",
                artifact.file_name,
                artifact_root.display()
            )),
        };
    }

    // Verify checksum
    if !verify_artifact_checksum(
        &artifact_path,
        &artifact.checksum_algorithm,
        &artifact.checksum,
    ) {
        return InstallExecutionResult {
            success: false,
            artifact_verified: false,
            binary_installed: false,
            backup_path: None,
            error_message: Some(format!(
                "checksum verification failed for '{}'",
                artifact.file_name
            )),
        };
    }

    let install_path = Path::new(&plan.target.install_path);
    let install_dir = install_path.parent().unwrap_or(Path::new("."));

    // Create install directory if needed
    if !install_dir.exists() {
        if let Err(error) = fs::create_dir_all(install_dir) {
            return InstallExecutionResult {
                success: false,
                artifact_verified: true,
                binary_installed: false,
                backup_path: None,
                error_message: Some(format!(
                    "failed to create install directory '{}': {error}",
                    install_dir.display()
                )),
            };
        }
    }

    // Back up existing binary
    let backup_path = if install_path.exists() {
        let backup = install_path.with_extension("backup");
        if let Err(error) = fs::rename(install_path, &backup) {
            return InstallExecutionResult {
                success: false,
                artifact_verified: true,
                binary_installed: false,
                backup_path: None,
                error_message: Some(format!(
                    "failed to back up existing binary to '{}': {error}",
                    backup.display()
                )),
            };
        }
        Some(normalize_path(&backup))
    } else {
        None
    };

    // Extract binary from archive
    let extraction_result =
        extract_binary_from_archive(&artifact_path, &artifact.archive_format, install_path);
    if let Err(error) = extraction_result {
        // Restore backup on failure
        if let Some(backup) = &backup_path {
            let _ = fs::rename(backup, install_path);
        }
        return InstallExecutionResult {
            success: false,
            artifact_verified: true,
            binary_installed: false,
            backup_path,
            error_message: Some(format!("failed to extract binary: {error}")),
        };
    }

    // Set executable permissions (Unix only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = fs::metadata(install_path) {
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o755);
            let _ = fs::set_permissions(install_path, permissions);
        }
    }

    InstallExecutionResult {
        success: true,
        artifact_verified: true,
        binary_installed: true,
        backup_path,
        error_message: None,
    }
}

fn verify_artifact_checksum(path: &Path, algorithm: &str, expected: &str) -> bool {
    match algorithm {
        "sha256" | "SHA256" => {
            let Ok(bytes) = fs::read(path) else {
                return false;
            };
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let result = hasher.finalize();
            let actual = bytes_to_hex(&result);
            actual.eq_ignore_ascii_case(expected)
        }
        "blake3" | "BLAKE3" => {
            let Ok(bytes) = fs::read(path) else {
                return false;
            };
            let actual = blake3::hash(&bytes).to_hex().to_string();
            actual.eq_ignore_ascii_case(expected)
        }
        _ => false,
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn extract_binary_from_archive(
    archive_path: &Path,
    archive_format: &str,
    install_path: &Path,
) -> Result<(), String> {
    let temp_dir = env::temp_dir().join(format!("ee-extract-{}", std::process::id()));
    fs::create_dir_all(&temp_dir).map_err(|e| format!("failed to create temp directory: {e}"))?;

    let result = match archive_format {
        "tar.xz" | "tar+xz" => extract_tar_xz(archive_path, &temp_dir),
        "tar.gz" | "tar+gzip" => extract_tar_gz(archive_path, &temp_dir),
        _ => Err(format!("unsupported archive format: {archive_format}")),
    };

    if let Err(error) = result {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(error);
    }

    // Find the extracted binary (should be named 'ee' or 'ee.exe')
    let binary_name = install_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("ee");

    let extracted_binary = find_binary_in_dir(&temp_dir, binary_name)?;

    // Move binary to install path
    fs::copy(&extracted_binary, install_path)
        .map_err(|e| format!("failed to copy binary to '{}': {e}", install_path.display()))?;

    let _ = fs::remove_dir_all(&temp_dir);
    Ok(())
}

fn extract_tar_xz(archive_path: &Path, dest_dir: &Path) -> Result<(), String> {
    extract_with_trusted_tar(archive_path, dest_dir, "-xJf")
}

fn extract_tar_gz(archive_path: &Path, dest_dir: &Path) -> Result<(), String> {
    extract_with_trusted_tar(archive_path, dest_dir, "-xzf")
}

fn extract_with_trusted_tar(
    archive_path: &Path,
    dest_dir: &Path,
    extract_flag: &str,
) -> Result<(), String> {
    let tar_path = resolve_trusted_tar_binary()?;
    let mut command = trusted_tar_command(&tar_path)?;
    let status = command
        .arg(extract_flag)
        .arg(archive_path)
        .arg("-C")
        .arg(dest_dir)
        .env_clear()
        .env("PATH", TRUSTED_INSTALL_TOOL_PATH)
        .env("LANG", "C")
        .status()
        .map_err(|e| format!("failed to run trusted tar '{}': {e}", tar_path.display()))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "trusted tar '{}' extraction failed with status {status}",
            tar_path.display()
        ))
    }
}

fn trusted_tar_command(path: &Path) -> Result<std::process::Command, String> {
    if path == Path::new("/usr/bin/tar") {
        Ok(std::process::Command::new("/usr/bin/tar"))
    } else if path == Path::new("/bin/tar") {
        Ok(std::process::Command::new("/bin/tar"))
    } else {
        Err(format!(
            "tar binary '{}' is not in the trusted command allowlist",
            path.display()
        ))
    }
}

fn resolve_trusted_tar_binary() -> Result<PathBuf, String> {
    resolve_trusted_tar_binary_from_candidates(
        TRUSTED_TAR_PATHS.iter().map(|path| Path::new(*path)),
    )
}

fn resolve_trusted_tar_binary_from_candidates<'a>(
    candidates: impl IntoIterator<Item = &'a Path>,
) -> Result<PathBuf, String> {
    let mut errors = Vec::new();
    for candidate in candidates {
        match validate_trusted_tar_binary(candidate) {
            Ok(()) => return Ok(candidate.to_path_buf()),
            Err(error) => errors.push(format!("{}: {error}", candidate.display())),
        }
    }

    if errors.is_empty() {
        Err("no trusted tar binary candidates configured".to_owned())
    } else {
        Err(format!(
            "no trusted tar binary available; refused candidates: {}",
            errors.join("; ")
        ))
    }
}

fn validate_trusted_tar_binary(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("tar binary path must be absolute; refusing PATH lookup".to_owned());
    }
    if !TRUSTED_TAR_PATHS
        .iter()
        .any(|trusted_path| path == Path::new(trusted_path))
    {
        return Err(format!(
            "tar binary '{}' is not a trusted system path",
            path.display()
        ));
    }

    let metadata = fs::metadata(path)
        .map_err(|error| format!("failed to stat tar binary '{}': {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "tar binary '{}' is not a regular file",
            path.display()
        ));
    }

    validate_trusted_executable_metadata(path, &metadata)
}

#[cfg(unix)]
fn validate_trusted_executable_metadata(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if metadata.uid() != 0 {
        return Err(format!("tar binary '{}' is not root-owned", path.display()));
    }

    let mode = metadata.permissions().mode();
    if mode & 0o111 == 0 {
        return Err(format!("tar binary '{}' is not executable", path.display()));
    }
    if mode & 0o022 != 0 {
        return Err(format!(
            "tar binary '{}' is writable by group or other users",
            path.display()
        ));
    }

    if let Some(parent) = path.parent() {
        validate_trusted_executable_parent(parent)?;
    }

    Ok(())
}

#[cfg(unix)]
fn validate_trusted_executable_parent(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = fs::metadata(path)
        .map_err(|error| format!("failed to stat tar parent '{}': {error}", path.display()))?;
    if !metadata.is_dir() {
        return Err(format!(
            "tar parent '{}' is not a directory",
            path.display()
        ));
    }
    if metadata.uid() != 0 {
        return Err(format!("tar parent '{}' is not root-owned", path.display()));
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(format!(
            "tar parent '{}' is writable by group or other users",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_trusted_executable_metadata(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), String> {
    if metadata.permissions().readonly() {
        Ok(())
    } else {
        Err(format!(
            "tar binary '{}' integrity cannot be validated on this platform",
            path.display()
        ))
    }
}

fn find_binary_in_dir(dir: &Path, binary_name: &str) -> Result<PathBuf, String> {
    // First try direct match
    let direct = dir.join(binary_name);
    if direct.is_file() {
        return Ok(direct);
    }

    // Search recursively (archives often have a top-level directory)
    for entry in walkdir_simple(dir, 3) {
        if let Some(name) = entry.file_name().and_then(|n| n.to_str()) {
            if name == binary_name && entry.is_file() {
                return Ok(entry);
            }
        }
    }

    Err(format!(
        "binary '{}' not found in extracted archive",
        binary_name
    ))
}

fn walkdir_simple(dir: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut result = Vec::new();
    walkdir_recurse(dir, 0, max_depth, &mut result);
    result
}

fn walkdir_recurse(dir: &Path, depth: usize, max_depth: usize, result: &mut Vec<PathBuf>) {
    if depth > max_depth {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        result.push(path.clone());
        if path.is_dir() {
            walkdir_recurse(&path, depth + 1, max_depth, result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, context: &str) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(context.to_owned())
        }
    }

    fn ensure_equal<T: std::fmt::Debug + PartialEq>(
        actual: T,
        expected: T,
        context: &str,
    ) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn idempotency_key_is_stable_for_same_inputs() -> TestResult {
        let left = install_idempotency_key(
            InstallOperation::Install,
            Some("0.1.0"),
            "x86_64-unknown-linux-gnu",
            "/tmp/bin/ee",
            Some("artifact"),
        );
        let right = install_idempotency_key(
            InstallOperation::Install,
            Some("0.1.0"),
            "x86_64-unknown-linux-gnu",
            "/tmp/bin/ee",
            Some("artifact"),
        );
        ensure_equal(left, right, "stable key")
    }

    #[test]
    fn install_check_reports_missing_path_binary() -> TestResult {
        let options = InstallCheckOptions {
            install_dir: Some(PathBuf::from("/tmp/ee-test-bin")),
            current_binary: Some(PathBuf::from("/tmp/ee-test-bin/ee")),
            path_env: Some(OsString::from("/tmp/no-ee-here")),
            target_triple: Some("x86_64-unknown-linux-gnu".to_owned()),
            manifest: None,
            offline: true,
        };
        let report = check_install(&options);

        ensure_equal(
            report.path.status,
            InstallPathStatus::Missing,
            "path status",
        )?;
        ensure(
            report
                .findings
                .iter()
                .any(|finding| finding.code == InstallFindingCode::BinaryNotOnPath),
            "binary_not_on_path finding",
        )
    }

    #[test]
    fn install_plan_without_manifest_is_blocked() -> TestResult {
        let options = InstallPlanOptions {
            target_triple: Some("x86_64-unknown-linux-gnu".to_owned()),
            install_dir: Some(PathBuf::from("/tmp/ee-test-bin")),
            offline: true,
            ..InstallPlanOptions::default()
        };
        let report = plan_install(&options);

        ensure_equal(report.status, InstallPlanStatus::Blocked, "status")?;
        ensure(
            report
                .findings
                .iter()
                .any(|finding| finding.code == InstallFindingCode::OfflineNoManifest),
            "offline_no_manifest finding",
        )
    }

    #[test]
    fn manifest_shape_reports_empty_artifacts() -> TestResult {
        let mut findings = Vec::new();
        collect_manifest_shape_findings(
            r#"{"schema":"ee.release_manifest.v1","artifacts":[]}"#,
            "x86_64-unknown-linux-gnu",
            &mut findings,
        );

        ensure(
            findings
                .iter()
                .any(|finding| finding.code == InstallFindingCode::NoArtifacts),
            "no_artifacts finding",
        )
    }

    #[test]
    fn manifest_shape_reports_duplicate_target_aliases() -> TestResult {
        let mut findings = Vec::new();
        collect_manifest_shape_findings(
            r#"{
              "schema":"ee.release_manifest.v1",
              "artifacts":[
                {"target":"x86_64-unknown-linux-gnu"},
                {"targetTriple":"x86_64-unknown-linux-gnu"}
              ]
            }"#,
            "x86_64-unknown-linux-gnu",
            &mut findings,
        );

        ensure(
            findings
                .iter()
                .any(|finding| finding.code == InstallFindingCode::DuplicateTarget),
            "duplicate_target finding",
        )
    }

    #[test]
    fn selected_target_triple_honors_explicit_nonempty_value() -> TestResult {
        ensure_equal(
            selected_target_triple(Some("aarch64-apple-darwin")),
            "aarch64-apple-darwin".to_owned(),
            "explicit target",
        )?;
        ensure(
            !selected_target_triple(Some("")).is_empty(),
            "empty override falls back to inferred target",
        )
    }

    #[test]
    fn trusted_tar_resolver_rejects_path_based_invocation() -> TestResult {
        let candidates = [Path::new("tar")];
        let error = match resolve_trusted_tar_binary_from_candidates(candidates) {
            Ok(path) => {
                return Err(format!(
                    "relative tar candidate resolved to {}",
                    path.display()
                ));
            }
            Err(error) => error,
        };

        ensure(
            error.contains("refusing PATH lookup"),
            "relative tar candidate should be rejected before process invocation",
        )
    }

    #[test]
    fn install_plan_rejects_unforced_downgrade_pin() -> TestResult {
        let options = InstallPlanOptions {
            target_triple: Some("x86_64-unknown-linux-gnu".to_owned()),
            install_dir: Some(PathBuf::from("/tmp/ee-test-bin")),
            pinned_version: Some("0.0.1".to_owned()),
            offline: true,
            ..InstallPlanOptions::default()
        };
        let report = plan_install(&options);

        ensure(
            report
                .findings
                .iter()
                .any(|finding| finding.code == InstallFindingCode::WouldDowngrade),
            "would_downgrade finding",
        )?;
        ensure_equal(report.status, InstallPlanStatus::Blocked, "status")
    }

    #[cfg(unix)]
    #[test]
    fn install_check_reports_nonwritable_parent() -> TestResult {
        let options = InstallCheckOptions {
            install_dir: Some(PathBuf::from("/dev/null/ee")),
            current_binary: Some(PathBuf::from("/dev/null/not-ee")),
            path_env: Some(OsString::from("/dev/null")),
            target_triple: Some("x86_64-unknown-linux-gnu".to_owned()),
            manifest: None,
            offline: true,
        };
        let report = check_install(&options);

        ensure_equal(
            report.permissions.status,
            InstallPermissionStatus::MissingParentUnknown,
            "permission status",
        )?;
        ensure(
            report
                .findings
                .iter()
                .any(|finding| finding.code == InstallFindingCode::InstallDirNotWritable),
            "install_dir_not_writable finding",
        )
    }

    #[test]
    fn execute_install_plan_rejects_blocked_status() -> TestResult {
        let report = InstallPlanReport {
            command: "update".to_owned(),
            schema: UPDATE_PLAN_SCHEMA_V1.to_owned(),
            version: "0.1.0".to_owned(),
            operation: InstallOperation::Update,
            dry_run: true,
            status: InstallPlanStatus::Blocked,
            current_version: "0.1.0".to_owned(),
            target_version: Some("0.2.0".to_owned()),
            pinned_version: None,
            target: InstallTarget {
                target_triple: "x86_64-unknown-linux-gnu".to_owned(),
                supported: true,
                binary_name: "ee".to_owned(),
                executable_name: "ee".to_owned(),
                install_dir: "/tmp/ee-test".to_owned(),
                install_path: "/tmp/ee-test/ee".to_owned(),
            },
            artifact: None,
            verification: InstallVerificationPlan {
                manifest_status: "loaded".to_owned(),
                checksum_status: "planned".to_owned(),
                signature_status: "missing".to_owned(),
                target_status: "matched".to_owned(),
                overwrite_status: "new_file".to_owned(),
            },
            planned_operations: Vec::new(),
            idempotency_key: "test".to_owned(),
            rollback: "side_path_before_replace".to_owned(),
            findings: Vec::new(),
        };

        let result = execute_install_plan(&report, Path::new("/tmp/artifacts"));
        ensure(!result.success, "blocked plan should fail")?;
        ensure(
            result
                .error_message
                .as_ref()
                .is_some_and(|msg| msg.contains("not executable")),
            "error message should mention non-executable status",
        )
    }

    #[test]
    fn execute_install_plan_rejects_missing_artifact() -> TestResult {
        let report = InstallPlanReport {
            command: "update".to_owned(),
            schema: UPDATE_PLAN_SCHEMA_V1.to_owned(),
            version: "0.1.0".to_owned(),
            operation: InstallOperation::Update,
            dry_run: true,
            status: InstallPlanStatus::Ready,
            current_version: "0.1.0".to_owned(),
            target_version: Some("0.2.0".to_owned()),
            pinned_version: None,
            target: InstallTarget {
                target_triple: "x86_64-unknown-linux-gnu".to_owned(),
                supported: true,
                binary_name: "ee".to_owned(),
                executable_name: "ee".to_owned(),
                install_dir: "/tmp/ee-test".to_owned(),
                install_path: "/tmp/ee-test/ee".to_owned(),
            },
            artifact: None,
            verification: InstallVerificationPlan {
                manifest_status: "loaded".to_owned(),
                checksum_status: "planned".to_owned(),
                signature_status: "missing".to_owned(),
                target_status: "matched".to_owned(),
                overwrite_status: "new_file".to_owned(),
            },
            planned_operations: Vec::new(),
            idempotency_key: "test".to_owned(),
            rollback: "side_path_before_replace".to_owned(),
            findings: Vec::new(),
        };

        let result = execute_install_plan(&report, Path::new("/tmp/artifacts"));
        ensure(!result.success, "missing artifact should fail")?;
        ensure(
            result
                .error_message
                .as_ref()
                .is_some_and(|msg| msg.contains("no artifact")),
            "error message should mention missing artifact",
        )
    }

    #[test]
    fn verify_checksum_blake3_matches() -> TestResult {
        let temp_dir = env::temp_dir().join("ee-checksum-test");
        let _ = fs::create_dir_all(&temp_dir);
        let test_file = temp_dir.join("test.bin");
        fs::write(&test_file, b"hello world").expect("write test file");

        let expected = blake3::hash(b"hello world").to_hex().to_string();
        ensure(
            verify_artifact_checksum(&test_file, "blake3", &expected),
            "blake3 checksum should match",
        )?;
        ensure(
            !verify_artifact_checksum(
                &test_file,
                "blake3",
                "0000000000000000000000000000000000000000000000000000000000000000",
            ),
            "blake3 checksum should not match wrong value",
        )?;

        let _ = fs::remove_dir_all(&temp_dir);
        Ok(())
    }

    #[test]
    fn verify_checksum_sha256_matches() -> TestResult {
        use sha2::{Digest, Sha256};

        let temp_dir = env::temp_dir().join("ee-sha256-test");
        let _ = fs::create_dir_all(&temp_dir);
        let test_file = temp_dir.join("test.bin");
        fs::write(&test_file, b"hello world").expect("write test file");

        let mut hasher = Sha256::new();
        hasher.update(b"hello world");
        let expected = bytes_to_hex(&hasher.finalize());

        ensure(
            verify_artifact_checksum(&test_file, "sha256", &expected),
            "sha256 checksum should match",
        )?;
        ensure(
            !verify_artifact_checksum(
                &test_file,
                "sha256",
                "0000000000000000000000000000000000000000000000000000000000000000",
            ),
            "sha256 checksum should not match wrong value",
        )?;

        let _ = fs::remove_dir_all(&temp_dir);
        Ok(())
    }
}
