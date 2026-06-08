//! Agent-safe install and update checks.

use std::cmp::Ordering;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::core::build_info;
use crate::models::install::{
    INSTALL_FRESHNESS_SCHEMA_V1, InstallFreshnessReport, InstallFreshnessVerdict,
    InstallVersionEvidence,
};
use crate::models::{
    INSTALL_CHECK_SCHEMA_V1, INSTALL_PLAN_SCHEMA_V1, InstallArtifactSelection, InstallCheckReport,
    InstallFinding, InstallFindingCode, InstallOperation, InstallPathAnalysis, InstallPathStatus,
    InstallPermissionCheck, InstallPermissionStatus, InstallPlanReport, InstallPlanStatus,
    InstallTarget, InstallVerificationPlan, PathBinary, PlannedInstallOperation,
    RELEASE_BINARY_NAME, RELEASE_MANIFEST_SCHEMA_V1, ReleaseManifest, ReleaseVerificationCode,
    ReleaseVerificationSeverity, UPDATE_PLAN_SCHEMA_V1, UpdateSourcePosture, compare_versions,
    is_safe_install_path, is_safe_release_artifact_path, is_supported_release_target,
};
use toml_edit::DocumentMut;

const TRUSTED_TAR_PATHS: &[&str] = &["/usr/bin/tar", "/bin/tar"];
const TRUSTED_INSTALL_TOOL_PATH: &str = "/usr/bin:/bin";
const EXTRACT_TEMP_PREFIX: &str = "ee-extract-";
const MAX_BACKUP_PATH_ATTEMPTS: usize = 1000;
const PATH_BINARY_VERSION_TIMEOUT: Duration = Duration::from_millis(750);
const PATH_BINARY_VERSION_STDOUT_MAX_BYTES: u64 = 4096;
const CARGO_TOML_MAX_BYTES: u64 = 1024 * 1024;
const INSTALL_FRESHNESS_DEFAULT_REQUIRED_SURFACES: &[&str] = &["install_check"];
const INSTALL_FRESHNESS_SUPPORTED_SURFACES: &[&str] =
    &["install_check", "claim_gate_install_freshness", "version"];

/// Hard upper bound on the byte length of a release manifest read from a
/// user-supplied `--manifest <path>`. Realistic manifests are on the order
/// of a few KB; 4 MiB is a generous ceiling that still bounds memory if a
/// user (accidentally or otherwise) passes a non-manifest path to
/// `ee install plan` / `ee install check`. The `symlink_metadata` check
/// above already refuses FIFOs/sockets/dirs by requiring a regular file,
/// so the only remaining unbounded vector was a large regular file —
/// `fs::read_to_string` would otherwise pre-size its buffer from the
/// file's metadata length and attempt a giant single allocation. Same
/// pattern as `HANDOFF_FILE_MAX_BYTES` in src/core/handoff.rs (6d8d00e5).
const RELEASE_MANIFEST_MAX_BYTES: u64 = 4 * 1024 * 1024;

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
    let current_binary_path = current_binary.as_deref().map(normalize_path);
    if let (Some(current_path), Some(first_binary)) =
        (current_binary_path.as_deref(), path.first_binary.as_deref())
        && first_binary != current_path
    {
        findings.push(InstallFinding::warning(
            InstallFindingCode::CurrentBinaryShadowed,
            format!(
                "the running ee binary ({current_path}) is shadowed by the first PATH binary ({first_binary})"
            ),
            "Run `ee install check --json` from the PATH binary you expect agents to use, or update PATH/install-dir ordering.",
        ));
    }
    let mismatched_versions = path
        .binaries
        .iter()
        .filter_map(|binary| {
            let version = binary.version.as_deref()?;
            (compare_versions(version, info.version) != Ordering::Equal)
                .then(|| format!("{}={version}", binary.path))
        })
        .collect::<Vec<_>>();
    if !mismatched_versions.is_empty() {
        let shown = mismatched_versions
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        findings.push(InstallFinding::warning(
            InstallFindingCode::PathBinaryVersionMismatch,
            format!(
                "{} PATH ee binar{} report a different version than the running binary ({}){}",
                mismatched_versions.len(),
                if mismatched_versions.len() == 1 { "y" } else { "ies" },
                info.version,
                if shown.is_empty() {
                    String::new()
                } else {
                    format!(": {shown}")
                }
            ),
            "Run the intended PATH binary directly, then rebuild/install the current release if the PATH version is stale.",
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
            "Pass --manifest for deterministic no-local-Cargo freshness evidence; use install plan with --artifact-root before adoption.",
        ));
    }

    let source_version = detect_install_source_version(options.manifest.as_deref());
    let current_binary = crate::models::CurrentBinary {
        path: current_binary.as_deref().map(normalize_path),
        version: info.version.to_owned(),
        source: "running_process".to_owned(),
    };
    let freshness = evaluate_install_freshness(
        source_version,
        &current_binary,
        &path,
        &findings,
        INSTALL_FRESHNESS_DEFAULT_REQUIRED_SURFACES,
    );
    findings.extend(install_freshness_findings(&freshness));

    InstallCheckReport {
        command: "install check".to_owned(),
        schema: INSTALL_CHECK_SCHEMA_V1.to_owned(),
        version: info.version.to_owned(),
        current_binary,
        target,
        path,
        permissions,
        update_source,
        freshness,
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
                    if supported_archive_format(selected.archive_format.as_str()).is_none() {
                        findings.push(InstallFinding::error(
                            InstallFindingCode::UpdateApplyUnsupported,
                            format!(
                                "archive format '{}' cannot be applied by ee update",
                                selected.archive_format
                            ),
                            "Publish a tar_xz artifact for this target before using update apply.",
                        ));
                    }

                    checksum_status = if options.artifact_root.is_some() {
                        if verification.findings.iter().any(|finding| {
                            matches!(
                                finding.code,
                                ReleaseVerificationCode::ChecksumMismatch
                                    | ReleaseVerificationCode::InvalidChecksum
                                    | ReleaseVerificationCode::MissingArtifact
                                    | ReleaseVerificationCode::UnsafeArtifactPath
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
                            "Pass --artifact-root pointing at downloaded release artifacts before no-local-Cargo adoption.",
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
                        "Choose a target from the manifest or ask a release operator to publish the missing artifact; do not run local Cargo in agent automation.",
                    ));
                }
            }
            Err(finding) => {
                manifest_status = if matches!(finding.code, InstallFindingCode::ManifestMissing) {
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
            "Pass --manifest to plan from a verified release artifact, or record an operator-exception request; do not run local Cargo.",
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
                mode: "operator_approval_required_no_local_cargo".to_owned(),
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

#[must_use]
pub fn evaluate_install_freshness(
    source_version: InstallVersionEvidence,
    current_binary: &crate::models::CurrentBinary,
    path: &InstallPathAnalysis,
    _findings: &[InstallFinding],
    required_surfaces: &[&str],
) -> InstallFreshnessReport {
    let required_surfaces = required_surfaces
        .iter()
        .map(|surface| (*surface).to_owned())
        .collect::<Vec<_>>();
    let missing_required_surfaces = required_surfaces
        .iter()
        .filter(|surface| !INSTALL_FRESHNESS_SUPPORTED_SURFACES.contains(&surface.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let installed_version = InstallVersionEvidence {
        version: if current_binary.version.trim().is_empty() {
            None
        } else {
            Some(current_binary.version.clone())
        },
        source: current_binary.source.clone(),
        status: if current_binary.version.trim().is_empty() {
            "missing".to_owned()
        } else {
            "reported".to_owned()
        },
        path: current_binary.path.clone(),
        path_class: current_binary
            .path
            .as_ref()
            .map(|_| "host_local_path".to_owned()),
    };
    let comparison = install_version_comparison(
        installed_version.version.as_deref(),
        source_version.version.as_deref(),
    );
    let shadowed = current_binary
        .path
        .as_deref()
        .zip(path.first_binary.as_deref())
        .is_some_and(|(current, first)| current != first);

    let verdict = if !missing_required_surfaces.is_empty() {
        InstallFreshnessVerdict::MissingRequiredSurface
    } else if source_version.version.is_none() {
        InstallFreshnessVerdict::UnknownSourceVersion
    } else if installed_version.version.is_none() {
        InstallFreshnessVerdict::UnknownInstalledVersion
    } else if shadowed {
        InstallFreshnessVerdict::ShadowedBinary
    } else if !path.current_binary_on_path {
        InstallFreshnessVerdict::PathBinaryMissing
    } else if comparison != "equal" {
        InstallFreshnessVerdict::Stale
    } else {
        InstallFreshnessVerdict::Fresh
    };

    let mut blocking_findings = Vec::new();
    match verdict {
        InstallFreshnessVerdict::Fresh => {}
        InstallFreshnessVerdict::Stale => {
            blocking_findings.push(InstallFindingCode::InstalledBinaryStale);
        }
        InstallFreshnessVerdict::UnknownSourceVersion => {
            blocking_findings.push(InstallFindingCode::SourceVersionUnknown);
        }
        InstallFreshnessVerdict::UnknownInstalledVersion => {
            blocking_findings.push(InstallFindingCode::InstalledVersionUnknown);
        }
        InstallFreshnessVerdict::MissingRequiredSurface => {
            blocking_findings.push(InstallFindingCode::RequiredSurfaceMissing);
        }
        InstallFreshnessVerdict::PathBinaryMissing => {
            blocking_findings.push(InstallFindingCode::BinaryNotOnPath);
        }
        InstallFreshnessVerdict::ShadowedBinary => {
            blocking_findings.push(InstallFindingCode::CurrentBinaryShadowed);
        }
    }

    InstallFreshnessReport {
        schema: INSTALL_FRESHNESS_SCHEMA_V1.to_owned(),
        verdict,
        authoritative: verdict == InstallFreshnessVerdict::Fresh,
        comparison,
        source_version,
        installed_version,
        path_status: path.status,
        required_surfaces,
        missing_required_surfaces,
        blocking_findings,
        repair: install_freshness_repair(verdict).to_owned(),
    }
}

fn install_version_comparison(installed: Option<&str>, source: Option<&str>) -> String {
    match (installed, source) {
        (Some(installed), Some(source)) => match compare_versions(installed, source) {
            Ordering::Equal => "equal",
            Ordering::Less => "installed_older_than_source",
            Ordering::Greater => "installed_newer_than_source",
        },
        _ => "unknown",
    }
    .to_owned()
}

fn install_freshness_repair(verdict: InstallFreshnessVerdict) -> &'static str {
    match verdict {
        InstallFreshnessVerdict::Fresh => "No freshness repair required.",
        InstallFreshnessVerdict::Stale => {
            "Plan no-local-Cargo adoption from a verified release artifact with ee install plan --manifest <release-manifest.json> --artifact-root <release-artifact-dir>, or file an operator exception; do not run local Cargo."
        }
        InstallFreshnessVerdict::UnknownSourceVersion => {
            "Run the check from the eidetic-engine source checkout or pass a release manifest with a source version; if no artifact exists, record an operator-exception request instead of building locally."
        }
        InstallFreshnessVerdict::UnknownInstalledVersion => {
            "Run a normal ee binary with build version metadata before trusting install freshness."
        }
        InstallFreshnessVerdict::MissingRequiredSurface => {
            "Adopt a newer verified ee artifact that supports every required automation surface, or file an operator exception; do not run local Cargo."
        }
        InstallFreshnessVerdict::PathBinaryMissing => {
            "Plan adoption into PATH from a verified artifact or run the PATH-resolved ee binary before trusting agent automation; do not create it with local Cargo."
        }
        InstallFreshnessVerdict::ShadowedBinary => {
            "Run the first ee binary found in PATH or fix PATH ordering before trusting agent automation; if a newer binary is needed, use verified artifact adoption instead of local Cargo."
        }
    }
}

fn install_freshness_findings(freshness: &InstallFreshnessReport) -> Vec<InstallFinding> {
    match freshness.verdict {
        InstallFreshnessVerdict::Fresh => Vec::new(),
        InstallFreshnessVerdict::Stale => vec![InstallFinding::error(
            InstallFindingCode::InstalledBinaryStale,
            install_freshness_stale_message(freshness),
            freshness.repair.clone(),
        )],
        InstallFreshnessVerdict::UnknownSourceVersion => vec![InstallFinding::error(
            InstallFindingCode::SourceVersionUnknown,
            "source package version could not be established from Cargo.toml or a release manifest",
            freshness.repair.clone(),
        )],
        InstallFreshnessVerdict::UnknownInstalledVersion => vec![InstallFinding::error(
            InstallFindingCode::InstalledVersionUnknown,
            "running ee binary did not report usable build version metadata",
            freshness.repair.clone(),
        )],
        InstallFreshnessVerdict::MissingRequiredSurface => vec![InstallFinding::error(
            InstallFindingCode::RequiredSurfaceMissing,
            format!(
                "installed ee is missing required surface(s): {}",
                freshness.missing_required_surfaces.join(", ")
            ),
            freshness.repair.clone(),
        )],
        InstallFreshnessVerdict::PathBinaryMissing => vec![InstallFinding::error(
            InstallFindingCode::BinaryNotOnPath,
            "running ee binary is not available through PATH",
            freshness.repair.clone(),
        )],
        InstallFreshnessVerdict::ShadowedBinary => vec![InstallFinding::error(
            InstallFindingCode::CurrentBinaryShadowed,
            "running ee binary is not the first ee binary found in PATH",
            freshness.repair.clone(),
        )],
    }
}

fn install_freshness_stale_message(freshness: &InstallFreshnessReport) -> String {
    format!(
        "running ee version '{}' does not match source version '{}'",
        freshness
            .installed_version
            .version
            .as_deref()
            .unwrap_or("unknown"),
        freshness
            .source_version
            .version
            .as_deref()
            .unwrap_or("unknown")
    )
}

fn detect_install_source_version(manifest: Option<&Path>) -> InstallVersionEvidence {
    if let Some(evidence) = detect_cargo_toml_source_version() {
        return evidence;
    }
    if let Some(manifest) = manifest {
        return read_release_manifest_version(manifest);
    }
    unknown_source_version("no_source_evidence", None)
}

fn detect_cargo_toml_source_version() -> Option<InstallVersionEvidence> {
    let mut current = env::current_dir().ok()?;
    loop {
        let candidate = current.join("Cargo.toml");
        if candidate.is_file() {
            let evidence = read_cargo_toml_version(&candidate);
            if evidence.status != "not_eidetic_package" {
                return Some(evidence);
            }
        }
        if !current.pop() {
            return None;
        }
    }
}

fn read_cargo_toml_version(path: &Path) -> InstallVersionEvidence {
    let path_string = normalize_path(path);
    let Some(raw) = read_bounded_regular_text(path, CARGO_TOML_MAX_BYTES) else {
        return unknown_source_version("cargo_toml_unreadable", Some(path_string));
    };
    let Ok(document) = raw.parse::<DocumentMut>() else {
        return unknown_source_version("cargo_toml_invalid", Some(path_string));
    };
    let Some(package) = document.get("package").and_then(toml_edit::Item::as_table) else {
        return unknown_source_version("not_eidetic_package", Some(path_string));
    };
    let package_name = package
        .get("name")
        .and_then(toml_edit::Item::as_str)
        .unwrap_or_default();
    if package_name != "eidetic-engine" {
        return unknown_source_version("not_eidetic_package", Some(path_string));
    }
    let Some(version) = package.get("version").and_then(toml_edit::Item::as_str) else {
        return unknown_source_version("cargo_toml_missing_version", Some(path_string));
    };
    version_evidence(Some(version.to_owned()), "cargo_toml", "ok", None)
}

fn read_release_manifest_version(path: &Path) -> InstallVersionEvidence {
    let path_string = normalize_path(path);
    let Some(raw) = read_bounded_regular_text(path, RELEASE_MANIFEST_MAX_BYTES) else {
        return unknown_source_version("release_manifest_unreadable", Some(path_string));
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return unknown_source_version("release_manifest_invalid_json", Some(path_string));
    };
    if value.get("schema").and_then(serde_json::Value::as_str) != Some(RELEASE_MANIFEST_SCHEMA_V1) {
        return unknown_source_version("release_manifest_invalid_schema", Some(path_string));
    }
    let version = value
        .get("releaseVersion")
        .or_else(|| value.get("release_version"))
        .or_else(|| value.get("version"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    if version.is_none() {
        return unknown_source_version("release_manifest_missing_version", Some(path_string));
    }
    version_evidence(version, "release_manifest", "ok", Some(path_string))
}

fn read_bounded_regular_text(path: &Path, max_bytes: u64) -> Option<String> {
    if install_path_has_symlink_component(path)
        .ok()
        .flatten()
        .is_some()
    {
        return None;
    }
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > max_bytes {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if u64::try_from(bytes.len()).ok()? > max_bytes {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn unknown_source_version(status: &str, path: Option<String>) -> InstallVersionEvidence {
    version_evidence(None, "unknown", status, path)
}

fn version_evidence(
    version: Option<String>,
    source: &str,
    status: &str,
    path: Option<String>,
) -> InstallVersionEvidence {
    InstallVersionEvidence {
        version,
        source: source.to_owned(),
        status: status.to_owned(),
        path_class: path.as_ref().map(|_| "host_local_path".to_owned()),
        path,
    }
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
            let version_probe = probe_ee_binary_version(&candidate);
            binaries.push(PathBinary {
                is_current_binary: current.as_ref() == Some(&path),
                path,
                ordinal,
                version: version_probe.version,
                version_status: version_probe.status,
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
    let target_exists = Path::new(install_path).is_file();
    let metadata = fs::metadata(install_dir);
    let (status, writable) = match metadata {
        Ok(metadata) => {
            let writable = metadata.is_dir() && !metadata.permissions().readonly();
            (
                if writable {
                    InstallPermissionStatus::Writable
                } else {
                    InstallPermissionStatus::NotWritable
                },
                writable,
            )
        }
        Err(_) => match install_dir
            .parent()
            .and_then(|parent| fs::metadata(parent).ok())
        {
            Some(parent) if parent.is_dir() && !parent.permissions().readonly() => {
                (InstallPermissionStatus::MissingParentWritable, false)
            }
            _ => (InstallPermissionStatus::MissingParentUnknown, false),
        },
    };

    InstallPermissionCheck {
        status,
        install_dir: normalize_path(install_dir),
        target_path: install_path.to_owned(),
        exists: target_exists,
        writable,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BinaryVersionProbe {
    version: Option<String>,
    status: Option<String>,
}

fn probe_ee_binary_version(path: &Path) -> BinaryVersionProbe {
    let mut child = match Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            return BinaryVersionProbe {
                version: None,
                status: Some("probe_failed".to_owned()),
            };
        }
    };

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return BinaryVersionProbe {
                        version: None,
                        status: Some("nonzero".to_owned()),
                    };
                }
                let mut bytes = Vec::new();
                if let Some(stdout) = child.stdout.take()
                    && stdout
                        .take(PATH_BINARY_VERSION_STDOUT_MAX_BYTES)
                        .read_to_end(&mut bytes)
                        .is_err()
                {
                    return BinaryVersionProbe {
                        version: None,
                        status: Some("read_failed".to_owned()),
                    };
                }
                let stdout = String::from_utf8_lossy(&bytes);
                let version = stdout
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .and_then(parse_ee_version_line);
                return BinaryVersionProbe {
                    status: Some(if version.is_some() {
                        "reported".to_owned()
                    } else {
                        "unparseable".to_owned()
                    }),
                    version,
                };
            }
            Ok(None) if started.elapsed() >= PATH_BINARY_VERSION_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return BinaryVersionProbe {
                    version: None,
                    status: Some("timeout".to_owned()),
                };
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                return BinaryVersionProbe {
                    version: None,
                    status: Some("probe_failed".to_owned()),
                };
            }
        }
    }
}

fn parse_ee_version_line(line: &str) -> Option<String> {
    let mut parts = line.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some("ee"), Some(version)) => Some(version.to_owned()),
        (Some(version), None) if version.chars().next().is_some_and(|c| c.is_ascii_digit()) => {
            Some(version.to_owned())
        }
        _ => None,
    }
}

fn load_manifest(
    path: &Path,
    target_triple: &str,
    findings: &mut Vec<InstallFinding>,
) -> Result<ReleaseManifest, InstallFinding> {
    match install_path_has_symlink_component(path) {
        Ok(Some(component)) => {
            return Err(InstallFinding::error(
                InstallFindingCode::ManifestInvalid,
                format!(
                    "release manifest '{}' traverses symbolic link '{}'",
                    path.display(),
                    component.display()
                ),
                "Pass a release manifest through regular directories and a regular manifest file.",
            ));
        }
        Ok(None) => {}
        Err(error) => {
            return Err(InstallFinding::error(
                InstallFindingCode::ManifestMissing,
                format!(
                    "failed to inspect release manifest '{}': {error}",
                    path.display()
                ),
                "Pass a readable --manifest path.",
            ));
        }
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            // Cap before `fs::read_to_string` so a (regular) but enormous
            // file passed via `--manifest <path>` cannot pre-size the
            // read buffer to an unbounded allocation. Realistic
            // manifests are kilobytes; 4 MiB is the design ceiling.
            if metadata.len() > RELEASE_MANIFEST_MAX_BYTES {
                return Err(InstallFinding::error(
                    InstallFindingCode::ManifestInvalid,
                    format!(
                        "release manifest '{}' is {} bytes, exceeding the {RELEASE_MANIFEST_MAX_BYTES}-byte ceiling.",
                        path.display(),
                        metadata.len(),
                    ),
                    "Confirm the file is a real ee.release_manifest.v1 (typically <10 KB) and not a stray large file.",
                ));
            }
        }
        Ok(_) => {
            return Err(InstallFinding::error(
                InstallFindingCode::ManifestInvalid,
                format!(
                    "release manifest '{}' is not a regular file",
                    path.display()
                ),
                "Pass a regular ee.release_manifest.v1 JSON file.",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(InstallFinding::error(
                InstallFindingCode::ManifestMissing,
                format!("release manifest '{}' was not found", path.display()),
                "Pass a readable --manifest path.",
            ));
        }
        Err(error) => {
            return Err(InstallFinding::error(
                InstallFindingCode::ManifestMissing,
                format!(
                    "failed to inspect release manifest '{}': {error}",
                    path.display()
                ),
                "Pass a readable --manifest path.",
            ));
        }
    }
    // Bounded read with `take(CAP + 1)`. The metadata pre-check above is
    // TOCTOU-racy: a peer process can grow the file between the
    // `fs::symlink_metadata` stat and the open here, so the underlying
    // `fs::read_to_string` would still allocate past
    // `RELEASE_MANIFEST_MAX_BYTES`. The bounded read closes the window —
    // if the file has grown to CAP + 1 bytes by the time we hit it, bail
    // with a `ManifestInvalid` finding instead of allocating without
    // limit. Parallel defense to the `take(CAP + 1)` shape used in
    // `read_preflight_rules_file_no_follow` and
    // `read_preflight_run_store_file_no_follow`.
    let file = fs::File::open(path).map_err(|error| {
        InstallFinding::error(
            InstallFindingCode::ManifestMissing,
            format!(
                "failed to read release manifest '{}': {error}",
                path.display()
            ),
            "Pass a readable --manifest path.",
        )
    })?;
    let limit = RELEASE_MANIFEST_MAX_BYTES.saturating_add(1);
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes).map_err(|error| {
        InstallFinding::error(
            InstallFindingCode::ManifestMissing,
            format!(
                "failed to read release manifest '{}': {error}",
                path.display()
            ),
            "Pass a readable --manifest path.",
        )
    })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > RELEASE_MANIFEST_MAX_BYTES {
        return Err(InstallFinding::error(
            InstallFindingCode::ManifestInvalid,
            format!(
                "release manifest '{}' grew past the {RELEASE_MANIFEST_MAX_BYTES}-byte cap after the metadata check (TOCTOU).",
                path.display()
            ),
            "Confirm the file is a real ee.release_manifest.v1 (typically <10 KB) and not a stray large file.",
        ));
    }
    let raw = String::from_utf8(bytes).map_err(|error| {
        InstallFinding::error(
            InstallFindingCode::ManifestInvalid,
            format!(
                "release manifest '{}' is not valid UTF-8: {error}",
                path.display()
            ),
            "Regenerate the release manifest as a UTF-8 JSON file.",
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

fn select_install_backup_path(install_path: &Path) -> Result<PathBuf, String> {
    for attempt in 0..MAX_BACKUP_PATH_ATTEMPTS {
        let backup = if attempt == 0 {
            install_path.with_extension("backup")
        } else {
            install_path.with_extension(format!("backup.{attempt}"))
        };
        if !backup.exists() {
            return Ok(backup);
        }
    }

    Err(format!(
        "no available backup path for '{}' after {} attempts",
        install_path.display(),
        MAX_BACKUP_PATH_ATTEMPTS
    ))
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

    let install_path = Path::new(&plan.target.install_path);
    if !is_safe_install_path(install_path) {
        return InstallExecutionResult {
            success: false,
            artifact_verified: false,
            binary_installed: false,
            backup_path: None,
            error_message: Some(format!(
                "install target '{}' contains unsafe path components",
                plan.target.install_path
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

    if !is_safe_release_artifact_path(&artifact.file_name) {
        return InstallExecutionResult {
            success: false,
            artifact_verified: false,
            binary_installed: false,
            backup_path: None,
            error_message: Some(format!("artifact path '{}' is unsafe", artifact.file_name)),
        };
    }

    let artifact_relative_path = Path::new(&artifact.file_name);
    match install_artifact_path_has_symlink_component(artifact_root, artifact_relative_path) {
        Ok(true) => {
            return InstallExecutionResult {
                success: false,
                artifact_verified: false,
                binary_installed: false,
                backup_path: None,
                error_message: Some(format!(
                    "artifact '{}' traverses a symbolic link",
                    artifact.file_name
                )),
            };
        }
        Ok(false) => {}
        Err(error) => {
            return InstallExecutionResult {
                success: false,
                artifact_verified: false,
                binary_installed: false,
                backup_path: None,
                error_message: Some(format!(
                    "failed to inspect artifact '{}': {error}",
                    artifact.file_name
                )),
            };
        }
    }

    let artifact_path = artifact_root.join(artifact_relative_path);
    let artifact_metadata = match fs::symlink_metadata(&artifact_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
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
        Err(error) => {
            return InstallExecutionResult {
                success: false,
                artifact_verified: false,
                binary_installed: false,
                backup_path: None,
                error_message: Some(format!(
                    "failed to inspect artifact '{}': {error}",
                    artifact.file_name
                )),
            };
        }
    };
    if !artifact_metadata.is_file() {
        return InstallExecutionResult {
            success: false,
            artifact_verified: false,
            binary_installed: false,
            backup_path: None,
            error_message: Some(format!(
                "artifact '{}' is not a regular file",
                artifact.file_name
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

    let install_dir = install_path.parent().unwrap_or(Path::new("."));

    if let Err(error) = ensure_install_target_path_is_regular_or_missing(install_path) {
        return InstallExecutionResult {
            success: false,
            artifact_verified: true,
            binary_installed: false,
            backup_path: None,
            error_message: Some(error),
        };
    }

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

    // Re-check after creating the parent to catch races or newly materialized final paths.
    if let Err(error) = ensure_install_target_path_is_regular_or_missing(install_path) {
        return InstallExecutionResult {
            success: false,
            artifact_verified: true,
            binary_installed: false,
            backup_path: None,
            error_message: Some(error),
        };
    }

    // Back up existing binary
    let backup_path = if install_path.exists() {
        let backup = match select_install_backup_path(install_path) {
            Ok(backup) => backup,
            Err(error) => {
                return InstallExecutionResult {
                    success: false,
                    artifact_verified: true,
                    binary_installed: false,
                    backup_path: None,
                    error_message: Some(error),
                };
            }
        };
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
        match fs::metadata(install_path) {
            Ok(metadata) => {
                let mut permissions = metadata.permissions();
                permissions.set_mode(0o755);
                if let Err(error) = fs::set_permissions(install_path, permissions) {
                    if let Some(backup) = &backup_path {
                        let _ = fs::rename(backup, install_path);
                    }
                    return InstallExecutionResult {
                        success: false,
                        artifact_verified: true,
                        binary_installed: false,
                        backup_path,
                        error_message: Some(format!(
                            "failed to set executable permissions on '{}': {error}",
                            install_path.display()
                        )),
                    };
                }
            }
            Err(error) => {
                if let Some(backup) = &backup_path {
                    let _ = fs::rename(backup, install_path);
                }
                return InstallExecutionResult {
                    success: false,
                    artifact_verified: true,
                    binary_installed: false,
                    backup_path,
                    error_message: Some(format!(
                        "failed to read metadata for '{}' to set permissions: {error}",
                        install_path.display()
                    )),
                };
            }
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
    let temp_dir = create_extract_temp_dir()?;
    let temp_path = temp_dir.path();

    let result = match supported_archive_format(archive_format) {
        Some(SupportedArchiveFormat::TarXz) => extract_tar_xz(archive_path, temp_path),
        Some(SupportedArchiveFormat::TarGz) => extract_tar_gz(archive_path, temp_path),
        _ => Err(format!("unsupported archive format: {archive_format}")),
    };

    result?;

    // Find the extracted binary (should be named 'ee' or 'ee.exe')
    let binary_name = install_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("ee");

    let extracted_binary = find_binary_in_dir(temp_path, binary_name)?;

    publish_extracted_binary(&extracted_binary, install_path)?;

    Ok(())
}

fn publish_extracted_binary(extracted_binary: &Path, install_path: &Path) -> Result<(), String> {
    if !is_regular_file_no_symlink(extracted_binary)? {
        return Err(format!(
            "extracted binary '{}' is not a regular file",
            extracted_binary.display()
        ));
    }
    ensure_install_target_path_is_regular_or_missing(install_path)?;
    let temp_path = install_temp_path(install_path)?;
    ensure_install_temp_path_absent(&temp_path)?;

    let source_file = fs::File::open(extracted_binary).map_err(|error| {
        format!(
            "failed to open extracted binary '{}': {error}",
            extracted_binary.display()
        )
    })?;
    publish_extracted_binary_from_reader(source_file, install_path)
}

fn publish_extracted_binary_from_reader(
    mut source: impl io::Read,
    install_path: &Path,
) -> Result<(), String> {
    ensure_install_target_path_is_regular_or_missing(install_path)?;
    let temp_path = install_temp_path(install_path)?;
    ensure_install_temp_path_absent(&temp_path)?;

    let mut temp_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .map_err(|error| {
            format!(
                "failed to create temporary install binary '{}': {error}",
                temp_path.display()
            )
        })?;
    if let Err(error) = io::copy(&mut source, &mut temp_file) {
        drop(temp_file);
        return Err(cleanup_created_install_temp_after_error(
            &temp_path,
            format!(
                "failed to copy extracted binary to temporary install path '{}': {error}",
                temp_path.display()
            ),
        ));
    }
    if let Err(error) = temp_file.sync_all() {
        drop(temp_file);
        return Err(cleanup_created_install_temp_after_error(
            &temp_path,
            format!(
                "failed to sync temporary install binary '{}': {error}",
                temp_path.display()
            ),
        ));
    }
    drop(temp_file);

    publish_install_temp_binary(&temp_path, install_path)
}

fn cleanup_created_install_temp_after_error(temp_path: &Path, error_message: String) -> String {
    match fs::remove_file(temp_path) {
        Ok(()) => error_message,
        Err(cleanup_error) if cleanup_error.kind() == io::ErrorKind::NotFound => error_message,
        Err(cleanup_error) => format!(
            "{error_message}; additionally failed to remove temporary install binary '{}': {cleanup_error}",
            temp_path.display()
        ),
    }
}

fn publish_install_temp_binary(temp_path: &Path, install_path: &Path) -> Result<(), String> {
    ensure_install_target_path_is_regular_or_missing(install_path)?;
    ensure_install_created_temp_path_is_regular(temp_path)?;
    fs::rename(temp_path, install_path).map_err(|error| {
        let _ = fs::remove_file(temp_path);
        format!(
            "failed to publish temporary install binary '{}' to '{}': {error}",
            temp_path.display(),
            install_path.display()
        )
    })
}

fn ensure_install_temp_path_absent(path: &Path) -> Result<(), String> {
    match install_path_has_symlink_component(path) {
        Ok(Some(component)) => {
            return Err(format!(
                "install temp path '{}' traverses symbolic link '{}'",
                path.display(),
                component.display()
            ));
        }
        Ok(None) => {}
        Err(error) => {
            return Err(format!(
                "failed to inspect install temp path '{}': {error}",
                path.display()
            ));
        }
    }

    match fs::symlink_metadata(path) {
        Ok(_) => Err(format!(
            "install temp path '{}' already exists",
            path.display()
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "failed to inspect install temp path '{}': {error}",
            path.display()
        )),
    }
}

fn ensure_install_created_temp_path_is_regular(path: &Path) -> Result<(), String> {
    match install_path_has_symlink_component(path) {
        Ok(Some(component)) => {
            return Err(format!(
                "install temp path '{}' traverses symbolic link '{}'",
                path.display(),
                component.display()
            ));
        }
        Ok(None) => {}
        Err(error) => {
            return Err(format!(
                "failed to inspect install temp path '{}': {error}",
                path.display()
            ));
        }
    }

    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(format!(
            "install temp path '{}' is not a regular file",
            path.display()
        )),
        Err(error) => Err(format!(
            "failed to inspect install temp path '{}': {error}",
            path.display()
        )),
    }
}

fn install_temp_path(install_path: &Path) -> Result<PathBuf, String> {
    let Some(file_name) = install_path.file_name() else {
        return Err("install path has no file name".to_owned());
    };
    let mut temp_name = file_name.to_os_string();
    temp_name.push(".tmp");
    Ok(install_path.with_file_name(temp_name))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SupportedArchiveFormat {
    TarXz,
    TarGz,
}

fn supported_archive_format(archive_format: &str) -> Option<SupportedArchiveFormat> {
    match archive_format {
        "tar_xz" | "tar.xz" | "tar+xz" => Some(SupportedArchiveFormat::TarXz),
        "tar_gz" | "tar.gz" | "tar+gzip" => Some(SupportedArchiveFormat::TarGz),
        _ => None,
    }
}

fn create_extract_temp_dir() -> Result<tempfile::TempDir, String> {
    tempfile::Builder::new()
        .prefix(EXTRACT_TEMP_PREFIX)
        .tempdir()
        .map_err(|error| format!("failed to create secure extraction temp directory: {error}"))
}

fn extract_tar_xz(archive_path: &Path, dest_dir: &Path) -> Result<(), String> {
    extract_with_trusted_tar(archive_path, dest_dir, "-tJf", "-tvJf", "-xJf")
}

fn extract_tar_gz(archive_path: &Path, dest_dir: &Path) -> Result<(), String> {
    extract_with_trusted_tar(archive_path, dest_dir, "-tzf", "-tvzf", "-xzf")
}

fn extract_with_trusted_tar(
    archive_path: &Path,
    dest_dir: &Path,
    list_flag: &str,
    verbose_list_flag: &str,
    extract_flag: &str,
) -> Result<(), String> {
    let tar_path = resolve_trusted_tar_binary()?;
    validate_tar_archive_members(&tar_path, archive_path, list_flag, verbose_list_flag)?;
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

fn validate_tar_archive_members(
    tar_path: &Path,
    archive_path: &Path,
    list_flag: &str,
    verbose_list_flag: &str,
) -> Result<(), String> {
    let member_listing = run_trusted_tar_capture(tar_path, list_flag, archive_path)?;
    let verbose_listing = run_trusted_tar_capture(tar_path, verbose_list_flag, archive_path)?;
    let member_listing = std::str::from_utf8(&member_listing).map_err(|error| {
        format!(
            "trusted tar '{}' listed non-UTF-8 member paths in '{}': {error}",
            tar_path.display(),
            archive_path.display()
        )
    })?;
    let verbose_listing = std::str::from_utf8(&verbose_listing).map_err(|error| {
        format!(
            "trusted tar '{}' listed non-UTF-8 member metadata in '{}': {error}",
            tar_path.display(),
            archive_path.display()
        )
    })?;
    validate_tar_archive_member_listing(archive_path, member_listing, verbose_listing)
}

fn run_trusted_tar_capture(
    tar_path: &Path,
    flag: &str,
    archive_path: &Path,
) -> Result<Vec<u8>, String> {
    let mut command = trusted_tar_command(tar_path)?;
    let output = command
        .arg(flag)
        .arg(archive_path)
        .env_clear()
        .env("PATH", TRUSTED_INSTALL_TOOL_PATH)
        .env("LANG", "C")
        .output()
        .map_err(|error| {
            format!(
                "failed to run trusted tar '{}': {error}",
                tar_path.display()
            )
        })?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(format!(
            "trusted tar '{}' list failed for '{}' with status {}",
            tar_path.display(),
            archive_path.display(),
            output.status
        ))
    }
}

fn validate_tar_archive_member_listing(
    archive_path: &Path,
    member_listing: &str,
    verbose_listing: &str,
) -> Result<(), String> {
    let members = member_listing.lines().collect::<Vec<_>>();
    let verbose_members = verbose_listing.lines().collect::<Vec<_>>();
    if members.len() != verbose_members.len() {
        return Err(format!(
            "archive '{}' member listing is ambiguous: {} path rows but {} metadata rows",
            archive_path.display(),
            members.len(),
            verbose_members.len()
        ));
    }

    for (index, member) in members.iter().enumerate() {
        let member = member.strip_suffix('\r').unwrap_or(member);
        if !is_safe_archive_member_path(member) {
            return Err(format!(
                "archive '{}' contains unsafe member path '{}' at listing line {}",
                archive_path.display(),
                member,
                index + 1
            ));
        }
        let verbose_member = verbose_members[index]
            .strip_suffix('\r')
            .unwrap_or(verbose_members[index]);
        if !tar_verbose_member_type_is_allowed(verbose_member) {
            return Err(format!(
                "archive '{}' contains unsupported member type at listing line {}",
                archive_path.display(),
                index + 1
            ));
        }
    }

    Ok(())
}

fn is_safe_archive_member_path(member: &str) -> bool {
    if member.is_empty() || member.contains('\\') || member.chars().any(|ch| ch.is_control()) {
        return false;
    }
    let path = Path::new(member);
    if path.is_absolute() {
        return false;
    }
    let mut has_normal = false;
    for component in path.components() {
        match component {
            Component::Normal(_) => has_normal = true,
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return false;
            }
        }
    }
    has_normal
}

fn tar_verbose_member_type_is_allowed(line: &str) -> bool {
    matches!(line.as_bytes().first().copied(), Some(b'-' | b'd'))
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
    if is_regular_file_no_symlink(&direct)? {
        return Ok(direct);
    }

    // Search recursively (archives often have a top-level directory)
    for entry in walkdir_simple(dir, 3) {
        if let Some(name) = entry.file_name().and_then(|n| n.to_str()) {
            if name == binary_name && is_regular_file_no_symlink(&entry)? {
                return Ok(entry);
            }
        }
    }

    Err(format!(
        "binary '{}' not found in extracted archive",
        binary_name
    ))
}

fn is_regular_file_no_symlink(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(!metadata.file_type().is_symlink() && metadata.is_file()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("failed to inspect '{}': {error}", path.display())),
    }
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
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        result.push(path.clone());
        if metadata.is_dir() {
            walkdir_recurse(&path, depth + 1, max_depth, result);
        }
    }
}

fn install_artifact_path_has_symlink_component(root: &Path, relative: &Path) -> io::Result<bool> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(segment) => {
                current.push(segment);
                match fs::symlink_metadata(&current) {
                    Ok(metadata) if metadata.file_type().is_symlink() => return Ok(true),
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                    Err(error) => return Err(error),
                }
            }
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => return Ok(true),
        }
    }
    Ok(false)
}

fn ensure_install_target_path_is_regular_or_missing(path: &Path) -> Result<(), String> {
    match install_path_has_symlink_component(path) {
        Ok(Some(component)) => {
            return Err(format!(
                "install target '{}' traverses symbolic link '{}'",
                path.display(),
                component.display()
            ));
        }
        Ok(None) => {}
        Err(error) => {
            return Err(format!(
                "failed to inspect install target '{}': {error}",
                path.display()
            ));
        }
    }

    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(format!(
            "install target '{}' is not a regular file",
            path.display()
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "failed to inspect install target '{}': {error}",
            path.display()
        )),
    }
}

fn install_path_has_symlink_component(path: &Path) -> io::Result<Option<PathBuf>> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                current.push(component.as_os_str());
                continue;
            }
            Component::CurDir => continue,
            Component::ParentDir | Component::Normal(_) => {
                current.push(component.as_os_str());
            }
        }

        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(Some(current)),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    Ok(None)
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

    fn ensure_no_local_cargo_adoption_guidance(value: &str, context: &str) -> TestResult {
        ensure(
            value.contains("local Cargo") || value.contains("no-local-Cargo"),
            context,
        )?;
        ensure(
            !value.contains("cargo build")
                && !value.contains("cargo install")
                && !value.contains("Rebuild")
                && !value.contains("rebuild"),
            context,
        )
    }

    fn executable_plan_for_artifact(
        artifact: InstallArtifactSelection,
        install_path: &Path,
    ) -> InstallPlanReport {
        let install_dir = install_path.parent().unwrap_or_else(|| Path::new("/tmp"));
        InstallPlanReport {
            command: "update".to_owned(),
            schema: UPDATE_PLAN_SCHEMA_V1.to_owned(),
            version: "0.1.0".to_owned(),
            operation: InstallOperation::Update,
            dry_run: true,
            status: InstallPlanStatus::Ready,
            current_version: "0.1.0".to_owned(),
            target_version: Some(artifact.release_version.clone()),
            pinned_version: None,
            target: InstallTarget {
                target_triple: artifact.target_triple.clone(),
                supported: true,
                binary_name: "ee".to_owned(),
                executable_name: "ee".to_owned(),
                install_dir: normalize_path(install_dir),
                install_path: normalize_path(install_path),
            },
            artifact: Some(artifact),
            verification: InstallVerificationPlan {
                manifest_status: "loaded".to_owned(),
                checksum_status: "verified".to_owned(),
                signature_status: "missing".to_owned(),
                target_status: "matched".to_owned(),
                overwrite_status: "new_file".to_owned(),
            },
            planned_operations: Vec::new(),
            idempotency_key: "test".to_owned(),
            rollback: "side_path_before_replace".to_owned(),
            findings: Vec::new(),
        }
    }

    fn freshness_source(version: Option<&str>) -> InstallVersionEvidence {
        InstallVersionEvidence {
            version: version.map(str::to_owned),
            source: "test".to_owned(),
            status: if version.is_some() { "ok" } else { "missing" }.to_owned(),
            path: None,
            path_class: None,
        }
    }

    fn current_binary(path: &str, version: &str) -> crate::models::CurrentBinary {
        crate::models::CurrentBinary {
            path: Some(path.to_owned()),
            version: version.to_owned(),
            source: "running_process".to_owned(),
        }
    }

    fn path_analysis(first_binary: &str, current_binary: &str) -> InstallPathAnalysis {
        InstallPathAnalysis {
            status: if first_binary == current_binary {
                InstallPathStatus::Ok
            } else {
                InstallPathStatus::Shadowed
            },
            path_entries: vec!["/usr/local/bin".to_owned()],
            binaries: vec![PathBinary {
                path: first_binary.to_owned(),
                ordinal: 0,
                is_current_binary: first_binary == current_binary,
                version: None,
                version_status: None,
            }],
            first_binary: Some(first_binary.to_owned()),
            current_binary_on_path: first_binary == current_binary,
            duplicate_count: 1,
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
    fn install_freshness_marks_stale_running_binary() -> TestResult {
        let current = current_binary("/usr/local/bin/ee", "0.5.0");
        let path = path_analysis("/usr/local/bin/ee", "/usr/local/bin/ee");
        let report = evaluate_install_freshness(
            freshness_source(Some("0.6.0")),
            &current,
            &path,
            &[],
            &["install_check"],
        );

        ensure_equal(
            report.verdict,
            InstallFreshnessVerdict::Stale,
            "freshness verdict",
        )?;
        ensure(!report.authoritative, "stale binary fails closed")?;
        ensure_equal(
            report.comparison.as_str(),
            "installed_older_than_source",
            "version comparison",
        )?;
        ensure(
            report
                .blocking_findings
                .contains(&InstallFindingCode::InstalledBinaryStale),
            "stale finding code",
        )?;
        ensure_no_local_cargo_adoption_guidance(&report.repair, "stale repair")
    }

    #[test]
    fn install_freshness_marks_shadowed_binary() -> TestResult {
        let current = current_binary("/opt/ee/ee", "0.6.0");
        let path = path_analysis("/usr/local/bin/ee", "/opt/ee/ee");
        let report = evaluate_install_freshness(
            freshness_source(Some("0.6.0")),
            &current,
            &path,
            &[],
            &["install_check"],
        );

        ensure_equal(
            report.verdict,
            InstallFreshnessVerdict::ShadowedBinary,
            "freshness verdict",
        )?;
        ensure(!report.authoritative, "shadowed binary fails closed")?;
        ensure(
            report
                .blocking_findings
                .contains(&InstallFindingCode::CurrentBinaryShadowed),
            "shadowed finding code",
        )?;
        ensure_no_local_cargo_adoption_guidance(&report.repair, "shadowed repair")
    }

    #[test]
    fn install_freshness_marks_missing_required_surface() -> TestResult {
        let current = current_binary("/usr/local/bin/ee", "0.6.0");
        let path = path_analysis("/usr/local/bin/ee", "/usr/local/bin/ee");
        let report = evaluate_install_freshness(
            freshness_source(Some("0.6.0")),
            &current,
            &path,
            &[],
            &["claim_gate_install_freshness", "future_surface"],
        );

        ensure_equal(
            report.verdict,
            InstallFreshnessVerdict::MissingRequiredSurface,
            "freshness verdict",
        )?;
        ensure_equal(
            report.missing_required_surfaces,
            vec!["future_surface".to_owned()],
            "missing surfaces",
        )?;
        ensure(
            report
                .blocking_findings
                .contains(&InstallFindingCode::RequiredSurfaceMissing),
            "missing-surface finding code",
        )?;
        ensure_no_local_cargo_adoption_guidance(&report.repair, "missing-surface repair")
    }

    #[test]
    fn install_freshness_marks_missing_path_binary() -> TestResult {
        let current = current_binary("/opt/ee/ee", "0.6.0");
        let mut path = path_analysis("/usr/local/bin/ee", "/opt/ee/ee");
        path.binaries.clear();
        path.first_binary = None;
        path.current_binary_on_path = false;
        path.duplicate_count = 0;
        path.status = InstallPathStatus::Missing;
        let report = evaluate_install_freshness(
            freshness_source(Some("0.6.0")),
            &current,
            &path,
            &[],
            &["install_check"],
        );

        ensure_equal(
            report.verdict,
            InstallFreshnessVerdict::PathBinaryMissing,
            "freshness verdict",
        )?;
        ensure(!report.authoritative, "PATH-missing binary fails closed")?;
        ensure(
            report
                .blocking_findings
                .contains(&InstallFindingCode::BinaryNotOnPath),
            "PATH-missing finding code",
        )?;
        ensure_no_local_cargo_adoption_guidance(&report.repair, "PATH-missing repair")
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
                .any(|finding| matches!(finding.code, InstallFindingCode::BinaryNotOnPath)),
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
                .any(|finding| matches!(finding.code, InstallFindingCode::OfflineNoManifest)),
            "offline_no_manifest finding",
        )?;
        let next_action = report
            .findings
            .iter()
            .find(|finding| matches!(finding.code, InstallFindingCode::OfflineNoManifest))
            .map(|finding| finding.next_action.as_str())
            .unwrap_or_default();
        ensure(
            next_action.contains("operator-exception request"),
            "offline plan points to operator-exception path",
        )?;
        ensure_no_local_cargo_adoption_guidance(next_action, "offline plan next action")
    }

    #[test]
    fn install_check_rejects_relative_install_dir() -> TestResult {
        let options = InstallCheckOptions {
            install_dir: Some(PathBuf::from("relative-bin")),
            current_binary: Some(PathBuf::from("/tmp/not-ee")),
            path_env: Some(OsString::from("")),
            target_triple: Some("x86_64-unknown-linux-gnu".to_owned()),
            manifest: None,
            offline: true,
        };
        let report = check_install(&options);

        ensure(
            report
                .findings
                .iter()
                .any(|finding| matches!(finding.code, InstallFindingCode::UnsafeTargetPath)),
            "unsafe_target_path finding",
        )
    }

    #[test]
    fn install_plan_rejects_relative_install_dir() -> TestResult {
        let options = InstallPlanOptions {
            target_triple: Some("x86_64-unknown-linux-gnu".to_owned()),
            install_dir: Some(PathBuf::from("relative-bin")),
            offline: true,
            ..InstallPlanOptions::default()
        };
        let report = plan_install(&options);

        ensure(
            report
                .findings
                .iter()
                .any(|finding| matches!(finding.code, InstallFindingCode::UnsafeTargetPath)),
            "unsafe_target_path finding",
        )?;
        ensure_equal(report.status, InstallPlanStatus::Blocked, "status")
    }

    #[cfg(unix)]
    #[test]
    fn install_plan_rejects_symlinked_manifest_file() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside_manifest = tempdir.path().join("outside-manifest.json");
        let manifest = ReleaseManifest::new("9.9.9", "commit-a", Vec::new());
        fs::write(
            &outside_manifest,
            serde_json::to_vec(&manifest).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let manifest_link = tempdir.path().join("manifest-link.json");
        std::os::unix::fs::symlink(&outside_manifest, &manifest_link)
            .map_err(|error| error.to_string())?;

        let report = plan_install(&InstallPlanOptions {
            manifest: Some(manifest_link),
            install_dir: Some(tempdir.path().join("bin")),
            target_triple: Some("x86_64-unknown-linux-gnu".to_owned()),
            offline: true,
            ..InstallPlanOptions::default()
        });

        ensure_equal(report.status, InstallPlanStatus::Blocked, "status")?;
        ensure_equal(
            report.verification.manifest_status.as_str(),
            "invalid",
            "manifest status",
        )?;
        ensure(
            report.findings.iter().any(|finding| {
                matches!(finding.code, InstallFindingCode::ManifestInvalid)
                    && finding.message.contains("symbolic link")
            }),
            "symlinked manifest should be reported as invalid",
        )
    }

    #[cfg(unix)]
    #[test]
    fn install_plan_rejects_symlinked_manifest_parent() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let real_dir = tempdir.path().join("real-manifests");
        fs::create_dir_all(&real_dir).map_err(|error| error.to_string())?;
        let manifest = ReleaseManifest::new("9.9.9", "commit-a", Vec::new());
        fs::write(
            real_dir.join("manifest.json"),
            serde_json::to_vec(&manifest).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let manifest_dir_link = tempdir.path().join("manifest-dir-link");
        std::os::unix::fs::symlink(&real_dir, &manifest_dir_link)
            .map_err(|error| error.to_string())?;

        let report = plan_install(&InstallPlanOptions {
            manifest: Some(manifest_dir_link.join("manifest.json")),
            install_dir: Some(tempdir.path().join("bin")),
            target_triple: Some("x86_64-unknown-linux-gnu".to_owned()),
            offline: true,
            ..InstallPlanOptions::default()
        });

        ensure_equal(report.status, InstallPlanStatus::Blocked, "status")?;
        ensure_equal(
            report.verification.manifest_status.as_str(),
            "invalid",
            "manifest status",
        )?;
        ensure(
            report.findings.iter().any(|finding| {
                matches!(finding.code, InstallFindingCode::ManifestInvalid)
                    && finding.message.contains("symbolic link")
            }),
            "symlinked manifest parent should be reported as invalid",
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
                .any(|finding| matches!(finding.code, InstallFindingCode::NoArtifacts)),
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
                .any(|finding| matches!(finding.code, InstallFindingCode::DuplicateTarget)),
            "duplicate_target finding",
        )
    }

    #[test]
    fn install_plan_blocks_manifest_archive_formats_apply_cannot_extract() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let artifact_root = tempdir.path().join("artifacts");
        fs::create_dir_all(&artifact_root).map_err(|error| error.to_string())?;
        let artifact_bytes = b"windows zip artifact bytes";
        let artifact = crate::models::ReleaseArtifact::from_bytes(
            "9.9.9",
            "commit-a",
            "x86_64-pc-windows-msvc",
            artifact_bytes,
        );
        fs::write(artifact_root.join(&artifact.file_name), artifact_bytes)
            .map_err(|error| error.to_string())?;
        let manifest = ReleaseManifest::new("9.9.9", "commit-a", vec![artifact]);
        let manifest_path = tempdir.path().join("manifest.json");
        fs::write(
            &manifest_path,
            serde_json::to_vec(&manifest).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;

        let report = plan_install(&InstallPlanOptions {
            operation: InstallOperation::Update,
            manifest: Some(manifest_path),
            artifact_root: Some(artifact_root),
            install_dir: Some(tempdir.path().join("bin")),
            target_triple: Some("x86_64-pc-windows-msvc".to_owned()),
            offline: true,
            ..InstallPlanOptions::default()
        });

        ensure_equal(report.status, InstallPlanStatus::Blocked, "status")?;
        ensure(
            report.findings.iter().any(|finding| {
                matches!(finding.code, InstallFindingCode::UpdateApplyUnsupported)
                    && finding.message.contains("zip")
            }),
            "zip artifact apply should be blocked at plan time",
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
    fn extract_temp_dir_is_unique_and_prefixed() -> TestResult {
        let left = create_extract_temp_dir()?;
        let right = create_extract_temp_dir()?;

        ensure(
            left.path()
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(EXTRACT_TEMP_PREFIX)),
            "extract temp dir should use documented prefix",
        )?;
        ensure(
            left.path() != right.path(),
            "extract temp dirs should be unique",
        )
    }

    #[test]
    fn archive_format_aliases_cover_manifest_contract() -> TestResult {
        ensure_equal(
            supported_archive_format("tar_xz"),
            Some(SupportedArchiveFormat::TarXz),
            "manifest tar_xz alias",
        )?;
        ensure_equal(
            supported_archive_format("tar.xz"),
            Some(SupportedArchiveFormat::TarXz),
            "file extension tar.xz alias",
        )?;
        ensure_equal(
            supported_archive_format("tar_gz"),
            Some(SupportedArchiveFormat::TarGz),
            "manifest tar_gz alias",
        )?;
        ensure_equal(
            supported_archive_format("zip"),
            None,
            "zip apply stays unsupported until a verifier is implemented",
        )
    }

    #[test]
    fn tar_archive_member_listing_accepts_regular_files_and_dirs() -> TestResult {
        validate_tar_archive_member_listing(
            Path::new("artifact.tar.xz"),
            "ee/\nee/ee\n",
            "drwxr-xr-x 0/0 0 Jan 01 00:00 ee/\n-rwxr-xr-x 0/0 10 Jan 01 00:00 ee/ee\n",
        )
    }

    #[test]
    fn tar_archive_member_listing_accepts_current_dir_components() -> TestResult {
        validate_tar_archive_member_listing(
            Path::new("artifact.tar.xz"),
            "./ee\nee/./ee\n",
            "-rwxr-xr-x 0/0 10 Jan 01 00:00 ./ee\n-rwxr-xr-x 0/0 10 Jan 01 00:00 ee/./ee\n",
        )
    }

    #[test]
    fn tar_archive_member_listing_rejects_escape_paths() -> TestResult {
        let error = validate_tar_archive_member_listing(
            Path::new("artifact.tar.xz"),
            "ee\n../escape\n/absolute/escape\n",
            "-rwxr-xr-x 0/0 10 Jan 01 00:00 ee\n-rw-r--r-- 0/0 1 Jan 01 00:00 ../escape\n-rw-r--r-- 0/0 1 Jan 01 00:00 /absolute/escape\n",
        )
        .expect_err("unsafe archive member path should be rejected");

        ensure(
            error.contains("unsafe member path '../escape'"),
            "error should identify first unsafe path",
        )
    }

    #[test]
    fn tar_archive_member_path_rejects_control_characters() -> TestResult {
        for member in ["ee\tbin", "ee\rbin", "ee\u{7f}", "ee\u{1b}[31m"] {
            ensure(
                !is_safe_archive_member_path(member),
                &format!("archive member path {member:?} should reject control characters"),
            )?;
        }
        ensure(
            is_safe_archive_member_path("ee/bin"),
            "ordinary archive member remains allowed",
        )
    }

    #[test]
    fn tar_archive_member_listing_rejects_symlink_members() -> TestResult {
        let error = validate_tar_archive_member_listing(
            Path::new("artifact.tar.xz"),
            "ee\nlatest-ee\n",
            "-rwxr-xr-x 0/0 10 Jan 01 00:00 ee\nlrwxr-xr-x 0/0 0 Jan 01 00:00 latest-ee -> /tmp/ee\n",
        )
        .expect_err("archive symlink member should be rejected");

        ensure(
            error.contains("unsupported member type"),
            "error should identify unsupported member type",
        )
    }

    #[test]
    fn tar_archive_member_listing_rejects_ambiguous_metadata() -> TestResult {
        let error = validate_tar_archive_member_listing(
            Path::new("artifact.tar.xz"),
            "ee\nnested/ee\n",
            "-rwxr-xr-x 0/0 10 Jan 01 00:00 ee\n",
        )
        .expect_err("ambiguous archive listing should be rejected");

        ensure(
            error.contains("member listing is ambiguous"),
            "error should identify ambiguous listing",
        )
    }

    #[test]
    fn install_backup_path_does_not_clobber_existing_backup() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let install_path = tempdir.path().join("ee");
        let first_backup = install_path.with_extension("backup");
        fs::write(&first_backup, b"previous backup").map_err(|error| error.to_string())?;

        let selected = select_install_backup_path(&install_path)?;

        ensure_equal(
            selected,
            install_path.with_extension("backup.1"),
            "backup path",
        )?;
        let previous = fs::read(&first_backup).map_err(|error| error.to_string())?;
        ensure_equal(
            previous,
            b"previous backup".to_vec(),
            "existing backup bytes",
        )
    }

    #[cfg(unix)]
    #[test]
    fn extracted_binary_search_rejects_symlink_candidate() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside_binary = tempdir.path().join("outside-ee");
        fs::write(&outside_binary, b"outside binary").map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&outside_binary, tempdir.path().join("ee"))
            .map_err(|error| error.to_string())?;

        let result = find_binary_in_dir(tempdir.path(), "ee");

        ensure(
            result.is_err(),
            "symlink binary candidate should be rejected",
        )?;
        ensure(
            result
                .err()
                .is_some_and(|message| message.contains("not found")),
            "symlink candidate should not be reported as a regular binary",
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
                .any(|finding| matches!(finding.code, InstallFindingCode::WouldDowngrade)),
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
                .any(|finding| matches!(finding.code, InstallFindingCode::InstallDirNotWritable)),
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
    fn execute_install_plan_rejects_unsafe_target_path() -> TestResult {
        let artifact = InstallArtifactSelection {
            artifact_id: "ee-9.9.9-x86_64-unknown-linux-gnu".to_owned(),
            release_version: "9.9.9".to_owned(),
            file_name: "ee-x86_64-unknown-linux-gnu.tar.xz".to_owned(),
            target_triple: "x86_64-unknown-linux-gnu".to_owned(),
            archive_format: "tar_xz".to_owned(),
            checksum_algorithm: "blake3".to_owned(),
            checksum: "unused".to_owned(),
            signature: "missing".to_owned(),
        };
        let report = executable_plan_for_artifact(artifact, Path::new("relative-bin/ee"));

        let result = execute_install_plan(&report, Path::new("/tmp/artifacts"));

        ensure(!result.success, "unsafe target path should fail")?;
        ensure(
            !result.artifact_verified,
            "target path rejection must happen before artifact verification",
        )?;
        ensure(
            result
                .error_message
                .as_ref()
                .is_some_and(|message| message.contains("unsafe path")),
            "execute error should report unsafe target path",
        )
    }

    #[cfg(unix)]
    #[test]
    fn install_plan_rejects_symlink_artifact_root_file() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let artifact_root = tempdir.path().join("artifacts");
        fs::create_dir_all(&artifact_root).map_err(|error| error.to_string())?;
        let artifact_bytes = b"release archive bytes";
        let outside_artifact = tempdir.path().join("outside.tar.xz");
        fs::write(&outside_artifact, artifact_bytes).map_err(|error| error.to_string())?;
        let artifact = crate::models::ReleaseArtifact::from_bytes(
            "9.9.9",
            "commit-a",
            "x86_64-unknown-linux-gnu",
            artifact_bytes,
        );
        std::os::unix::fs::symlink(&outside_artifact, artifact_root.join(&artifact.file_name))
            .map_err(|error| error.to_string())?;
        let manifest = ReleaseManifest::new("9.9.9", "commit-a", vec![artifact]);
        let manifest_path = tempdir.path().join("manifest.json");
        fs::write(
            &manifest_path,
            serde_json::to_vec(&manifest).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;

        let report = plan_install(&InstallPlanOptions {
            manifest: Some(manifest_path),
            artifact_root: Some(artifact_root),
            install_dir: Some(tempdir.path().join("bin")),
            target_triple: Some("x86_64-unknown-linux-gnu".to_owned()),
            offline: true,
            ..InstallPlanOptions::default()
        });

        ensure_equal(report.status, InstallPlanStatus::Blocked, "plan status")?;
        ensure_equal(
            report.verification.checksum_status.as_str(),
            "failed",
            "checksum status",
        )?;
        ensure(
            report
                .findings
                .iter()
                .any(|finding| matches!(finding.code, InstallFindingCode::UnsafeArtifact)),
            "symlink artifact should map to unsafe artifact finding",
        )
    }

    #[cfg(unix)]
    #[test]
    fn execute_install_plan_rejects_symlink_artifact_file() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let artifact_root = tempdir.path().join("artifacts");
        fs::create_dir_all(&artifact_root).map_err(|error| error.to_string())?;
        let artifact_name = "ee-x86_64-unknown-linux-gnu.tar.xz";
        let artifact_bytes = b"not a real archive";
        let outside_artifact = tempdir.path().join("outside.tar.xz");
        fs::write(&outside_artifact, artifact_bytes).map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&outside_artifact, artifact_root.join(artifact_name))
            .map_err(|error| error.to_string())?;
        let artifact = InstallArtifactSelection {
            artifact_id: "ee-9.9.9-x86_64-unknown-linux-gnu".to_owned(),
            release_version: "9.9.9".to_owned(),
            file_name: artifact_name.to_owned(),
            target_triple: "x86_64-unknown-linux-gnu".to_owned(),
            archive_format: "tar_xz".to_owned(),
            checksum_algorithm: "blake3".to_owned(),
            checksum: blake3::hash(artifact_bytes).to_hex().to_string(),
            signature: "missing".to_owned(),
        };
        let report = executable_plan_for_artifact(artifact, &tempdir.path().join("bin/ee"));

        let result = execute_install_plan(&report, &artifact_root);

        ensure(!result.success, "symlink artifact execute should fail")?;
        ensure(
            !result.artifact_verified,
            "symlink target bytes must not count as verified artifact",
        )?;
        ensure(
            result
                .error_message
                .as_ref()
                .is_some_and(|message| message.contains("symbolic link")),
            "execute error should report symlink artifact",
        )
    }

    #[cfg(unix)]
    #[test]
    fn execute_install_plan_rejects_symlinked_install_parent_before_create() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let artifact_root = tempdir.path().join("artifacts");
        fs::create_dir_all(&artifact_root).map_err(|error| error.to_string())?;
        let artifact_name = "ee-x86_64-unknown-linux-gnu.tar.xz";
        let artifact_bytes = b"not a real archive";
        fs::write(artifact_root.join(artifact_name), artifact_bytes)
            .map_err(|error| error.to_string())?;
        let outside_dir = tempdir.path().join("outside-bin-root");
        fs::create_dir_all(&outside_dir).map_err(|error| error.to_string())?;
        let linked_bin = tempdir.path().join("bin");
        std::os::unix::fs::symlink(&outside_dir, &linked_bin).map_err(|error| error.to_string())?;
        let install_path = linked_bin.join("nested").join("ee");
        let artifact = InstallArtifactSelection {
            artifact_id: "ee-9.9.9-x86_64-unknown-linux-gnu".to_owned(),
            release_version: "9.9.9".to_owned(),
            file_name: artifact_name.to_owned(),
            target_triple: "x86_64-unknown-linux-gnu".to_owned(),
            archive_format: "tar_xz".to_owned(),
            checksum_algorithm: "blake3".to_owned(),
            checksum: blake3::hash(artifact_bytes).to_hex().to_string(),
            signature: "missing".to_owned(),
        };
        let report = executable_plan_for_artifact(artifact, &install_path);

        let result = execute_install_plan(&report, &artifact_root);

        ensure(!result.success, "symlink install parent should fail")?;
        ensure(
            result.artifact_verified,
            "artifact should verify before target guard",
        )?;
        ensure(
            result
                .error_message
                .as_ref()
                .is_some_and(|message| message.contains("symbolic link")),
            "execute error should report symlink parent",
        )?;
        ensure(
            !outside_dir.join("nested").exists(),
            "install must not create missing directories through symlinked parent",
        )
    }

    #[cfg(unix)]
    #[test]
    fn execute_install_plan_rejects_symlinked_install_target_before_backup() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let artifact_root = tempdir.path().join("artifacts");
        fs::create_dir_all(&artifact_root).map_err(|error| error.to_string())?;
        let artifact_name = "ee-x86_64-unknown-linux-gnu.tar.xz";
        let artifact_bytes = b"not a real archive";
        fs::write(artifact_root.join(artifact_name), artifact_bytes)
            .map_err(|error| error.to_string())?;
        let outside_binary = tempdir.path().join("outside-ee");
        fs::write(&outside_binary, b"outside binary").map_err(|error| error.to_string())?;
        let install_path = tempdir.path().join("bin").join("ee");
        if let Some(parent) = install_path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        std::os::unix::fs::symlink(&outside_binary, &install_path)
            .map_err(|error| error.to_string())?;
        let artifact = InstallArtifactSelection {
            artifact_id: "ee-9.9.9-x86_64-unknown-linux-gnu".to_owned(),
            release_version: "9.9.9".to_owned(),
            file_name: artifact_name.to_owned(),
            target_triple: "x86_64-unknown-linux-gnu".to_owned(),
            archive_format: "tar_xz".to_owned(),
            checksum_algorithm: "blake3".to_owned(),
            checksum: blake3::hash(artifact_bytes).to_hex().to_string(),
            signature: "missing".to_owned(),
        };
        let report = executable_plan_for_artifact(artifact, &install_path);

        let result = execute_install_plan(&report, &artifact_root);

        ensure(!result.success, "symlink install target should fail")?;
        ensure(
            result.artifact_verified,
            "artifact should verify before target guard",
        )?;
        ensure(
            result
                .error_message
                .as_ref()
                .is_some_and(|message| message.contains("symbolic link")),
            "execute error should report symlink target",
        )?;
        let outside = fs::read(&outside_binary).map_err(|error| error.to_string())?;
        ensure_equal(
            outside.as_slice(),
            b"outside binary".as_slice(),
            "symlink target must remain untouched",
        )
    }

    #[test]
    fn publish_extracted_binary_rejects_existing_temp_without_truncating() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let extracted_binary = tempdir.path().join("extracted-ee");
        let install_path = tempdir.path().join("bin").join("ee");
        let temp_path = install_temp_path(&install_path)?;
        fs::create_dir_all(install_path.parent().expect("install parent"))
            .map_err(|error| error.to_string())?;
        fs::write(&extracted_binary, b"new binary").map_err(|error| error.to_string())?;
        fs::write(&temp_path, b"stale temp binary").map_err(|error| error.to_string())?;

        let error = publish_extracted_binary(&extracted_binary, &install_path)
            .expect_err("existing install temp should reject publish");

        ensure(
            error.contains("already exists"),
            "existing temp error should mention already exists",
        )?;
        ensure_equal(
            fs::read(&temp_path).map_err(|error| error.to_string())?,
            b"stale temp binary".to_vec(),
            "stale temp content",
        )?;
        ensure(
            !install_path.exists(),
            "final install path must not be published when temp exists",
        )
    }

    struct FailingBinaryRead {
        wrote_prefix: bool,
    }

    impl io::Read for FailingBinaryRead {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.wrote_prefix {
                return Err(io::Error::other("planned install copy failure"));
            }
            let prefix = b"partial";
            buffer[..prefix.len()].copy_from_slice(prefix);
            self.wrote_prefix = true;
            Ok(prefix.len())
        }
    }

    #[test]
    fn publish_extracted_binary_removes_created_temp_after_copy_failure() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let install_path = tempdir.path().join("bin").join("ee");
        let temp_path = install_temp_path(&install_path)?;
        fs::create_dir_all(install_path.parent().expect("install parent"))
            .map_err(|error| error.to_string())?;

        let error = publish_extracted_binary_from_reader(
            FailingBinaryRead {
                wrote_prefix: false,
            },
            &install_path,
        )
        .expect_err("copy failure should reject publish");

        ensure(
            error.contains("failed to copy extracted binary"),
            "copy failure should mention copy stage",
        )?;
        ensure(
            error.contains("planned install copy failure"),
            "copy failure should preserve source error",
        )?;
        ensure(
            !temp_path.exists(),
            "created temp install binary should be removed after copy failure",
        )?;
        ensure(
            !install_path.exists(),
            "final install path must not be published after copy failure",
        )
    }

    #[cfg(unix)]
    #[test]
    fn publish_install_temp_rechecks_final_symlink_before_rename() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let install_path = tempdir.path().join("bin").join("ee");
        let temp_path = install_temp_path(&install_path)?;
        fs::create_dir_all(install_path.parent().expect("install parent"))
            .map_err(|error| error.to_string())?;
        fs::write(&temp_path, b"new binary").map_err(|error| error.to_string())?;
        let outside_binary = tempdir.path().join("outside-ee");
        fs::write(&outside_binary, b"outside binary").map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&outside_binary, &install_path)
            .map_err(|error| error.to_string())?;

        let error = publish_install_temp_binary(&temp_path, &install_path)
            .expect_err("symlinked final install path should reject before publish");

        ensure(
            error.contains("symbolic link"),
            "final symlink error should mention symbolic link",
        )?;
        ensure_equal(
            fs::read(&outside_binary).map_err(|error| error.to_string())?,
            b"outside binary".to_vec(),
            "outside binary content",
        )?;
        ensure(
            temp_path.is_file(),
            "temp install binary should remain after final path rejection",
        )?;
        ensure(
            fs::symlink_metadata(&install_path)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_symlink(),
            "final install symlink should remain untouched",
        )
    }

    #[cfg(unix)]
    #[test]
    fn publish_install_temp_rechecks_temp_symlink_before_rename() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let install_path = tempdir.path().join("bin").join("ee");
        let temp_path = install_temp_path(&install_path)?;
        fs::create_dir_all(install_path.parent().expect("install parent"))
            .map_err(|error| error.to_string())?;
        let outside_binary = tempdir.path().join("outside-ee");
        fs::write(&outside_binary, b"outside binary").map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&outside_binary, &temp_path)
            .map_err(|error| error.to_string())?;

        let error = publish_install_temp_binary(&temp_path, &install_path)
            .expect_err("symlinked temp install path should reject before publish");

        ensure(
            error.contains("symbolic link"),
            "temp symlink error should mention symbolic link",
        )?;
        ensure(
            !install_path.exists(),
            "final install path must not be published from a temp symlink",
        )?;
        ensure_equal(
            fs::read(&outside_binary).map_err(|error| error.to_string())?,
            b"outside binary".to_vec(),
            "outside binary content",
        )?;
        ensure(
            fs::symlink_metadata(&temp_path)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_symlink(),
            "temp install symlink should remain untouched",
        )
    }

    #[test]
    fn execute_install_plan_rejects_non_regular_install_target_before_backup() -> TestResult {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let artifact_root = tempdir.path().join("artifacts");
        fs::create_dir_all(&artifact_root).map_err(|error| error.to_string())?;
        let artifact_name = "ee-x86_64-unknown-linux-gnu.tar.xz";
        let artifact_bytes = b"not a real archive";
        fs::write(artifact_root.join(artifact_name), artifact_bytes)
            .map_err(|error| error.to_string())?;
        let install_path = tempdir.path().join("bin").join("ee");
        fs::create_dir_all(&install_path).map_err(|error| error.to_string())?;
        let artifact = InstallArtifactSelection {
            artifact_id: "ee-9.9.9-x86_64-unknown-linux-gnu".to_owned(),
            release_version: "9.9.9".to_owned(),
            file_name: artifact_name.to_owned(),
            target_triple: "x86_64-unknown-linux-gnu".to_owned(),
            archive_format: "tar_xz".to_owned(),
            checksum_algorithm: "blake3".to_owned(),
            checksum: blake3::hash(artifact_bytes).to_hex().to_string(),
            signature: "missing".to_owned(),
        };
        let report = executable_plan_for_artifact(artifact, &install_path);

        let result = execute_install_plan(&report, &artifact_root);

        ensure(!result.success, "non-regular install target should fail")?;
        ensure(
            result.artifact_verified,
            "artifact should verify before target guard",
        )?;
        ensure(
            result
                .error_message
                .as_ref()
                .is_some_and(|message| message.contains("not a regular file")),
            "execute error should report non-regular target",
        )?;
        ensure(
            install_path.is_dir(),
            "non-regular install target must remain untouched",
        )
    }

    #[test]
    fn verify_checksum_blake3_matches() -> TestResult {
        let temp_dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let test_file = temp_dir.path().join("test.bin");
        fs::write(&test_file, b"hello world").map_err(|error| error.to_string())?;

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

        Ok(())
    }

    #[test]
    fn verify_checksum_sha256_matches() -> TestResult {
        use sha2::{Digest, Sha256};

        let temp_dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let test_file = temp_dir.path().join("test.bin");
        fs::write(&test_file, b"hello world").map_err(|error| error.to_string())?;

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

        Ok(())
    }
}
