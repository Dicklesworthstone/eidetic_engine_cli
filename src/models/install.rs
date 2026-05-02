//! Agent-safe install and update planning contracts (EE-DIST-003).
//!
//! These contracts describe what an installer or updater would inspect or
//! mutate. They are intentionally data-only so command paths can stay dry-run
//! and auditable until a verified apply path exists.

use std::cmp::Ordering;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Schema for `ee install check --json`.
pub const INSTALL_CHECK_SCHEMA_V1: &str = "ee.install.check.v1";

/// Schema for `ee install plan --json`.
pub const INSTALL_PLAN_SCHEMA_V1: &str = "ee.install.plan.v1";

/// Schema for `ee update --dry-run --json`.
pub const UPDATE_PLAN_SCHEMA_V1: &str = "ee.update.plan.v1";

/// Stable finding codes for install/update diagnostics.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallFindingCode {
    ArtifactChecksumMismatch,
    ArtifactMissing,
    BinaryNotOnPath,
    ChecksumVerificationPending,
    DuplicatePathBinary,
    ExistingUnknownFile,
    InstallDirMissing,
    InstallDirNotWritable,
    DuplicateTarget,
    ManifestInvalid,
    ManifestMissing,
    NoArtifacts,
    NoUpdateSourceConfigured,
    OfflineNoManifest,
    SignatureMissing,
    TargetMismatch,
    UnsupportedTarget,
    UnsafeArtifact,
    UnsafeTargetPath,
    UpdateApplyUnsupported,
    WouldDowngrade,
}

impl InstallFindingCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ArtifactChecksumMismatch => "artifact_checksum_mismatch",
            Self::ArtifactMissing => "artifact_missing",
            Self::BinaryNotOnPath => "binary_not_on_path",
            Self::ChecksumVerificationPending => "checksum_verification_pending",
            Self::DuplicatePathBinary => "duplicate_path_binary",
            Self::ExistingUnknownFile => "existing_unknown_file",
            Self::InstallDirMissing => "install_dir_missing",
            Self::InstallDirNotWritable => "install_dir_not_writable",
            Self::DuplicateTarget => "duplicate_target",
            Self::ManifestInvalid => "manifest_invalid",
            Self::ManifestMissing => "manifest_missing",
            Self::NoArtifacts => "no_artifacts",
            Self::NoUpdateSourceConfigured => "no_update_source_configured",
            Self::OfflineNoManifest => "offline_no_manifest",
            Self::SignatureMissing => "signature_missing",
            Self::TargetMismatch => "target_mismatch",
            Self::UnsupportedTarget => "unsupported_target",
            Self::UnsafeArtifact => "unsafe_artifact",
            Self::UnsafeTargetPath => "unsafe_target_path",
            Self::UpdateApplyUnsupported => "update_apply_unsupported",
            Self::WouldDowngrade => "would_downgrade",
        }
    }
}

impl fmt::Display for InstallFindingCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Severity of an install/update finding.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallFindingSeverity {
    Info,
    Warning,
    Error,
}

impl InstallFindingSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// One actionable diagnostic emitted by install/update planning.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallFinding {
    pub code: InstallFindingCode,
    pub severity: InstallFindingSeverity,
    pub message: String,
    pub next_action: String,
}

impl InstallFinding {
    #[must_use]
    pub fn info(
        code: InstallFindingCode,
        message: impl Into<String>,
        next_action: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity: InstallFindingSeverity::Info,
            message: message.into(),
            next_action: next_action.into(),
        }
    }

    #[must_use]
    pub fn warning(
        code: InstallFindingCode,
        message: impl Into<String>,
        next_action: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity: InstallFindingSeverity::Warning,
            message: message.into(),
            next_action: next_action.into(),
        }
    }

    #[must_use]
    pub fn error(
        code: InstallFindingCode,
        message: impl Into<String>,
        next_action: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity: InstallFindingSeverity::Error,
            message: message.into(),
            next_action: next_action.into(),
        }
    }
}

/// PATH posture for the `ee` binary.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallPathStatus {
    Ok,
    Missing,
    Duplicate,
    Shadowed,
}

impl InstallPathStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Missing => "missing",
            Self::Duplicate => "duplicate",
            Self::Shadowed => "shadowed",
        }
    }
}

/// Conservative writability posture for the configured install target.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallPermissionStatus {
    Writable,
    MissingParentWritable,
    MissingParentUnknown,
    NotWritable,
}

impl InstallPermissionStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Writable => "writable",
            Self::MissingParentWritable => "missing_parent_writable",
            Self::MissingParentUnknown => "missing_parent_unknown",
            Self::NotWritable => "not_writable",
        }
    }
}

/// High-level decision for an install or update plan.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallPlanStatus {
    Ready,
    Blocked,
    Degraded,
    Idempotent,
}

impl InstallPlanStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Blocked => "blocked",
            Self::Degraded => "degraded",
            Self::Idempotent => "idempotent",
        }
    }
}

/// Type of install/update operation being planned.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallOperation {
    Install,
    Update,
}

impl InstallOperation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Update => "update",
        }
    }
}

/// One observed `ee` binary in PATH.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathBinary {
    pub path: String,
    pub ordinal: usize,
    pub is_current_binary: bool,
}

/// PATH analysis for an install check.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallPathAnalysis {
    pub status: InstallPathStatus,
    pub path_entries: Vec<String>,
    pub binaries: Vec<PathBinary>,
    pub first_binary: Option<String>,
    pub current_binary_on_path: bool,
    pub duplicate_count: usize,
}

/// Permission check for the intended install directory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallPermissionCheck {
    pub status: InstallPermissionStatus,
    pub install_dir: String,
    pub target_path: String,
    pub exists: bool,
    pub writable: bool,
}

/// Target platform selected for a plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallTarget {
    pub target_triple: String,
    pub supported: bool,
    pub binary_name: String,
    pub executable_name: String,
    pub install_dir: String,
    pub install_path: String,
}

/// Current binary observation for `install check`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentBinary {
    pub path: Option<String>,
    pub version: String,
    pub source: String,
}

/// Update-source posture for read-only install checks.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSourcePosture {
    pub configured: bool,
    pub offline: bool,
    pub source: Option<String>,
    pub status: String,
}

/// Report emitted by `ee install check`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallCheckReport {
    pub command: String,
    pub schema: String,
    pub version: String,
    pub current_binary: CurrentBinary,
    pub target: InstallTarget,
    pub path: InstallPathAnalysis,
    pub permissions: InstallPermissionCheck,
    pub update_source: UpdateSourcePosture,
    pub findings: Vec<InstallFinding>,
}

impl InstallCheckReport {
    #[must_use]
    pub fn status(&self) -> InstallPlanStatus {
        findings_status(&self.findings)
    }
}

/// Selected release artifact for a dry-run plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallArtifactSelection {
    pub artifact_id: String,
    pub release_version: String,
    pub file_name: String,
    pub target_triple: String,
    pub archive_format: String,
    pub checksum_algorithm: String,
    pub checksum: String,
    pub signature: String,
}

/// One planned file operation. Planning commands never execute these.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedInstallOperation {
    pub action: String,
    pub path: String,
    pub mode: String,
    pub requires_verification: bool,
}

/// Verification posture for the selected artifact and target path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallVerificationPlan {
    pub manifest_status: String,
    pub checksum_status: String,
    pub signature_status: String,
    pub target_status: String,
    pub overwrite_status: String,
}

/// Dry-run report emitted by install planning and update planning.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallPlanReport {
    pub command: String,
    pub schema: String,
    pub version: String,
    pub operation: InstallOperation,
    pub dry_run: bool,
    pub status: InstallPlanStatus,
    pub current_version: String,
    pub target_version: Option<String>,
    pub pinned_version: Option<String>,
    pub target: InstallTarget,
    pub artifact: Option<InstallArtifactSelection>,
    pub verification: InstallVerificationPlan,
    pub planned_operations: Vec<PlannedInstallOperation>,
    pub idempotency_key: String,
    pub rollback: String,
    pub findings: Vec<InstallFinding>,
}

/// Compare package versions conservatively without pulling in a semver parser.
#[must_use]
pub fn compare_versions(current: &str, target: &str) -> Ordering {
    let current_parts = version_parts(current);
    let target_parts = version_parts(target);
    let width = current_parts.len().max(target_parts.len());
    for index in 0..width {
        let left = current_parts.get(index).copied().unwrap_or(0);
        let right = target_parts.get(index).copied().unwrap_or(0);
        match left.cmp(&right) {
            Ordering::Equal => {}
            ordering => return ordering,
        }
    }
    Ordering::Equal
}

#[must_use]
pub fn is_safe_install_path(path: &Path) -> bool {
    path.components().all(|component| {
        !matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    })
}

#[must_use]
pub fn findings_status(findings: &[InstallFinding]) -> InstallPlanStatus {
    if findings
        .iter()
        .any(|finding| finding.severity == InstallFindingSeverity::Error)
    {
        InstallPlanStatus::Blocked
    } else if findings
        .iter()
        .any(|finding| finding.severity == InstallFindingSeverity::Warning)
    {
        InstallPlanStatus::Degraded
    } else {
        InstallPlanStatus::Ready
    }
}

fn version_parts(version: &str) -> Vec<u64> {
    version
        .trim()
        .trim_start_matches('v')
        .split(['.', '-', '+'])
        .map(|part| {
            part.chars()
                .take_while(|ch| ch.is_ascii_digit())
                .collect::<String>()
        })
        .take_while(|part| !part.is_empty())
        .filter_map(|part| part.parse::<u64>().ok())
        .collect()
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
    fn version_comparison_orders_patch_releases() -> TestResult {
        ensure_equal(
            compare_versions("0.1.9", "0.1.10"),
            Ordering::Less,
            "patch ordering",
        )?;
        ensure_equal(
            compare_versions("v0.2.0", "0.1.10"),
            Ordering::Greater,
            "v prefix ordering",
        )?;
        ensure_equal(
            compare_versions("0.2.0", "0.2.0+build"),
            Ordering::Equal,
            "build metadata ignored",
        )
    }

    #[test]
    fn findings_status_is_conservative() -> TestResult {
        ensure_equal(findings_status(&[]), InstallPlanStatus::Ready, "empty")?;
        ensure_equal(
            findings_status(&[InstallFinding::warning(
                InstallFindingCode::SignatureMissing,
                "missing",
                "attach signature",
            )]),
            InstallPlanStatus::Degraded,
            "warning",
        )?;
        ensure_equal(
            findings_status(&[InstallFinding::error(
                InstallFindingCode::UnsupportedTarget,
                "unsupported",
                "pick supported target",
            )]),
            InstallPlanStatus::Blocked,
            "error",
        )
    }

    #[test]
    fn safe_install_path_rejects_relative_traversal() -> TestResult {
        ensure(is_safe_install_path(Path::new("/tmp/ee")), "absolute path")?;
        ensure(
            !is_safe_install_path(Path::new("/tmp/../ee")),
            "parent traversal",
        )
    }
}
