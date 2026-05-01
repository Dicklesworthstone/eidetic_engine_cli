//! Release artifact manifest contracts (EE-DIST-002).
//!
//! The manifest is the local, auditable contract for release packaging. Upload
//! channels can be added later; artifact names, checksums, targets, and install
//! assumptions belong here first so agents can verify a release directory
//! without network access.

use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Schema for EE release artifact manifests.
pub const RELEASE_MANIFEST_SCHEMA_V1: &str = "ee.release_manifest.v1";

/// Schema for one release artifact entry inside a manifest.
pub const RELEASE_ARTIFACT_SCHEMA_V1: &str = "ee.release_artifact.v1";

/// Schema for release manifest verification reports.
pub const RELEASE_MANIFEST_VERIFICATION_SCHEMA_V1: &str = "ee.release_manifest.verification.v1";

/// Schema for the release schema catalog.
pub const RELEASE_SCHEMA_CATALOG_V1: &str = "ee.release.schemas.v1";

/// Canonical binary name used by distribution artifacts.
pub const RELEASE_BINARY_NAME: &str = "ee";

const SHA256_HEX_LEN: usize = 64;
const BLAKE3_HEX_LEN: usize = 64;

/// Archive formats supported by the release manifest.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseArchiveFormat {
    TarXz,
    Zip,
}

impl ReleaseArchiveFormat {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TarXz => "tar_xz",
            Self::Zip => "zip",
        }
    }

    #[must_use]
    pub const fn file_extension(self) -> &'static str {
        match self {
            Self::TarXz => "tar.xz",
            Self::Zip => "zip",
        }
    }
}

impl fmt::Display for ReleaseArchiveFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Checksum algorithms supported by release manifests.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseChecksumAlgorithm {
    Sha256,
    Blake3,
}

impl ReleaseChecksumAlgorithm {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Blake3 => "blake3",
        }
    }

    #[must_use]
    pub const fn expected_hex_len(self) -> usize {
        match self {
            Self::Sha256 => SHA256_HEX_LEN,
            Self::Blake3 => BLAKE3_HEX_LEN,
        }
    }
}

impl fmt::Display for ReleaseChecksumAlgorithm {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Stable checksum field for archives, signatures, and provenance blobs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseChecksum {
    pub algorithm: ReleaseChecksumAlgorithm,
    pub value: String,
}

impl ReleaseChecksum {
    #[must_use]
    pub fn sha256_bytes(bytes: &[u8]) -> Self {
        Self {
            algorithm: ReleaseChecksumAlgorithm::Sha256,
            value: sha256_hex(bytes),
        }
    }

    #[must_use]
    pub fn blake3_bytes(bytes: &[u8]) -> Self {
        Self {
            algorithm: ReleaseChecksumAlgorithm::Blake3,
            value: blake3::hash(bytes).to_hex().to_string(),
        }
    }

    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.value.len() == self.algorithm.expected_hex_len()
            && self
                .value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    }
}

/// Optional signature/provenance sidecar for a release artifact.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSignature {
    pub kind: String,
    pub file_name: String,
    pub checksum: Option<ReleaseChecksum>,
}

impl ReleaseSignature {
    #[must_use]
    pub fn sigstore(file_name: impl Into<String>, checksum: Option<ReleaseChecksum>) -> Self {
        Self {
            kind: "sigstore".to_owned(),
            file_name: file_name.into(),
            checksum,
        }
    }
}

/// Build provenance fields that can be checked against the release manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseProvenance {
    pub source_commit: String,
    pub source_tag: Option<String>,
    pub build_profile: String,
    pub release_channel: String,
    pub dirty: bool,
}

impl ReleaseProvenance {
    #[must_use]
    pub fn new(
        source_commit: impl Into<String>,
        source_tag: Option<String>,
        build_profile: impl Into<String>,
        release_channel: impl Into<String>,
        dirty: bool,
    ) -> Self {
        Self {
            source_commit: source_commit.into(),
            source_tag,
            build_profile: build_profile.into(),
            release_channel: release_channel.into(),
            dirty,
        }
    }
}

/// Install layout advertised by one release artifact.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseInstallLayout {
    pub binary_name: String,
    pub executable_name: String,
    pub install_path: String,
}

impl ReleaseInstallLayout {
    #[must_use]
    pub fn for_target(binary_name: &str, target_triple: &str) -> Self {
        Self {
            binary_name: binary_name.to_owned(),
            executable_name: release_executable_name(binary_name, target_triple),
            install_path: default_install_path(binary_name, target_triple),
        }
    }
}

/// One archive entry in a release manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseArtifact {
    pub schema: String,
    pub artifact_id: String,
    pub release_version: String,
    pub file_name: String,
    pub target_triple: String,
    pub archive_format: ReleaseArchiveFormat,
    pub binary_name: String,
    pub checksum: ReleaseChecksum,
    pub signature: Option<ReleaseSignature>,
    pub provenance: Option<ReleaseProvenance>,
    pub install: ReleaseInstallLayout,
    pub minimum_os: Vec<String>,
    pub compatibility_notes: Vec<String>,
}

impl ReleaseArtifact {
    #[must_use]
    pub fn from_bytes(
        release_version: &str,
        git_commit: &str,
        target_triple: &str,
        bytes: &[u8],
    ) -> Self {
        let archive_format = default_archive_format(target_triple);
        let binary_name = RELEASE_BINARY_NAME;
        Self {
            schema: RELEASE_ARTIFACT_SCHEMA_V1.to_owned(),
            artifact_id: release_artifact_id(binary_name, release_version, target_triple),
            release_version: release_version.to_owned(),
            file_name: release_artifact_file_name(binary_name, target_triple, archive_format),
            target_triple: target_triple.to_owned(),
            archive_format,
            binary_name: binary_name.to_owned(),
            checksum: ReleaseChecksum::sha256_bytes(bytes),
            signature: None,
            provenance: Some(ReleaseProvenance::new(
                git_commit,
                release_tag(release_version),
                "release",
                "stable",
                false,
            )),
            install: ReleaseInstallLayout::for_target(binary_name, target_triple),
            minimum_os: minimum_os_assumptions(target_triple),
            compatibility_notes: compatibility_notes_for_target(target_triple),
        }
    }
}

/// Versioned manifest for all artifacts attached to one release.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseManifest {
    pub schema: String,
    pub release_version: String,
    pub git_commit: String,
    pub git_tag: Option<String>,
    pub binary_name: String,
    pub artifact_count: usize,
    pub artifacts: Vec<ReleaseArtifact>,
    pub compatibility_notes: Vec<String>,
}

impl ReleaseManifest {
    #[must_use]
    pub fn new(
        release_version: impl Into<String>,
        git_commit: impl Into<String>,
        artifacts: Vec<ReleaseArtifact>,
    ) -> Self {
        let release_version = release_version.into();
        let mut artifacts = artifacts;
        artifacts.sort_by(|left, right| {
            left.artifact_id
                .cmp(&right.artifact_id)
                .then_with(|| left.file_name.cmp(&right.file_name))
        });
        Self {
            schema: RELEASE_MANIFEST_SCHEMA_V1.to_owned(),
            git_tag: release_tag(&release_version),
            release_version,
            git_commit: git_commit.into(),
            binary_name: RELEASE_BINARY_NAME.to_owned(),
            artifact_count: artifacts.len(),
            artifacts,
            compatibility_notes: vec![
                "Release indexes are derived assets and are not packaged.".to_owned(),
                "Installers must verify archive checksums before extraction.".to_owned(),
            ],
        }
    }

    #[must_use]
    pub fn verify(&self, artifact_root: Option<&Path>) -> ReleaseVerificationReport {
        let mut findings = Vec::new();

        if self.schema != RELEASE_MANIFEST_SCHEMA_V1 {
            findings.push(ReleaseVerificationFinding::error(
                ReleaseVerificationCode::UnsupportedFutureManifestVersion,
                None,
                format!("unsupported release manifest schema '{}'", self.schema),
                "Upgrade ee or regenerate the manifest with release_manifest.v1.",
            ));
            return ReleaseVerificationReport::from_findings(
                Some(self.schema.clone()),
                Some(self.release_version.clone()),
                0,
                findings,
            );
        }

        if self.artifact_count != self.artifacts.len() {
            findings.push(ReleaseVerificationFinding::error(
                ReleaseVerificationCode::ArtifactCountMismatch,
                None,
                format!(
                    "manifest declares {} artifact(s), but contains {}",
                    self.artifact_count,
                    self.artifacts.len()
                ),
                "Regenerate the release manifest from the artifact directory.",
            ));
        }

        let mut artifact_ids = HashSet::new();
        let mut file_names = HashSet::new();
        for artifact in &self.artifacts {
            self.verify_artifact(
                artifact,
                artifact_root,
                &mut artifact_ids,
                &mut file_names,
                &mut findings,
            );
        }

        ReleaseVerificationReport::from_findings(
            Some(self.schema.clone()),
            Some(self.release_version.clone()),
            self.artifacts.len(),
            findings,
        )
    }

    fn verify_artifact(
        &self,
        artifact: &ReleaseArtifact,
        artifact_root: Option<&Path>,
        artifact_ids: &mut HashSet<String>,
        file_names: &mut HashSet<String>,
        findings: &mut Vec<ReleaseVerificationFinding>,
    ) {
        if artifact.schema != RELEASE_ARTIFACT_SCHEMA_V1 {
            findings.push(ReleaseVerificationFinding::error(
                ReleaseVerificationCode::InvalidArtifactSchema,
                Some(artifact.artifact_id.clone()),
                format!("artifact uses unsupported schema '{}'", artifact.schema),
                "Regenerate the artifact entry with release_artifact.v1.",
            ));
        }

        if !artifact_ids.insert(artifact.artifact_id.clone()) {
            findings.push(ReleaseVerificationFinding::error(
                ReleaseVerificationCode::DuplicateArtifactId,
                Some(artifact.artifact_id.clone()),
                format!("duplicate artifact id '{}'", artifact.artifact_id),
                "Use one stable artifact id per target archive.",
            ));
        }

        if !file_names.insert(artifact.file_name.clone()) {
            findings.push(ReleaseVerificationFinding::error(
                ReleaseVerificationCode::DuplicateArtifactFileName,
                Some(artifact.artifact_id.clone()),
                format!("duplicate artifact file name '{}'", artifact.file_name),
                "Use one archive file name per target.",
            ));
        }

        if artifact.release_version != self.release_version {
            findings.push(ReleaseVerificationFinding::error(
                ReleaseVerificationCode::StaleVersionMetadata,
                Some(artifact.artifact_id.clone()),
                format!(
                    "artifact release version '{}' does not match manifest '{}'",
                    artifact.release_version, self.release_version
                ),
                "Rebuild the artifact from the same version metadata as the manifest.",
            ));
        }

        if artifact.provenance.as_ref().is_some_and(|provenance| {
            provenance.source_commit != self.git_commit || provenance.source_tag != self.git_tag
        }) {
            findings.push(ReleaseVerificationFinding::error(
                ReleaseVerificationCode::StaleVersionMetadata,
                Some(artifact.artifact_id.clone()),
                "artifact provenance does not match manifest git metadata",
                "Rebuild the artifact and manifest from the same commit and tag.",
            ));
        }

        if !is_supported_release_target(&artifact.target_triple) {
            findings.push(ReleaseVerificationFinding::error(
                ReleaseVerificationCode::UnsupportedTarget,
                Some(artifact.artifact_id.clone()),
                format!("unsupported target triple '{}'", artifact.target_triple),
                "Use a supported target triple or add an explicit compatibility contract.",
            ));
        }

        let safe_artifact_path = is_safe_release_artifact_path(&artifact.file_name);
        if !safe_artifact_path {
            findings.push(ReleaseVerificationFinding::error(
                ReleaseVerificationCode::UnsafeArtifactPath,
                Some(artifact.artifact_id.clone()),
                format!("unsafe artifact file path '{}'", artifact.file_name),
                "Artifact file names must be relative release archive names.",
            ));
        }

        let checksum_well_formed = artifact.checksum.is_well_formed();
        if !checksum_well_formed {
            findings.push(ReleaseVerificationFinding::error(
                ReleaseVerificationCode::InvalidChecksum,
                Some(artifact.artifact_id.clone()),
                "artifact checksum is not a valid lowercase hex digest",
                "Regenerate the checksum with the declared checksum algorithm.",
            ));
        }

        if artifact.signature.is_none() {
            findings.push(ReleaseVerificationFinding::warning(
                ReleaseVerificationCode::SignatureMissing,
                Some(artifact.artifact_id.clone()),
                "artifact has no signature/provenance sidecar",
                "Attach a Sigstore bundle before publishing a public release.",
            ));
        }

        if safe_artifact_path
            && checksum_well_formed
            && let Some(root) = artifact_root
        {
            verify_artifact_file(root, artifact, findings);
        }
    }
}

/// Overall result for manifest verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseVerificationStatus {
    Passed,
    Warning,
    Failed,
}

impl ReleaseVerificationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Warning => "warning",
            Self::Failed => "failed",
        }
    }
}

/// Severity of one release verification finding.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseVerificationSeverity {
    Warning,
    Error,
}

/// Stable finding codes for release manifest verification.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseVerificationCode {
    ArtifactCountMismatch,
    ChecksumMismatch,
    DuplicateArtifactFileName,
    DuplicateArtifactId,
    InvalidArtifactSchema,
    InvalidChecksum,
    InvalidManifestJson,
    InvalidManifestSchema,
    MissingArtifact,
    SignatureMissing,
    StaleVersionMetadata,
    UnsupportedFutureManifestVersion,
    UnsupportedTarget,
    UnsafeArtifactPath,
}

impl ReleaseVerificationCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ArtifactCountMismatch => "artifact_count_mismatch",
            Self::ChecksumMismatch => "checksum_mismatch",
            Self::DuplicateArtifactFileName => "duplicate_artifact_file_name",
            Self::DuplicateArtifactId => "duplicate_artifact_id",
            Self::InvalidArtifactSchema => "invalid_artifact_schema",
            Self::InvalidChecksum => "invalid_checksum",
            Self::InvalidManifestJson => "invalid_manifest_json",
            Self::InvalidManifestSchema => "invalid_manifest_schema",
            Self::MissingArtifact => "missing_artifact",
            Self::SignatureMissing => "signature_missing",
            Self::StaleVersionMetadata => "stale_version_metadata",
            Self::UnsupportedFutureManifestVersion => "unsupported_future_manifest_version",
            Self::UnsupportedTarget => "unsupported_target",
            Self::UnsafeArtifactPath => "unsafe_artifact_path",
        }
    }
}

/// One finding emitted while verifying a release manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseVerificationFinding {
    pub code: ReleaseVerificationCode,
    pub severity: ReleaseVerificationSeverity,
    pub artifact_id: Option<String>,
    pub message: String,
    pub repair: String,
}

impl ReleaseVerificationFinding {
    #[must_use]
    pub fn warning(
        code: ReleaseVerificationCode,
        artifact_id: Option<String>,
        message: impl Into<String>,
        repair: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity: ReleaseVerificationSeverity::Warning,
            artifact_id,
            message: message.into(),
            repair: repair.into(),
        }
    }

    #[must_use]
    pub fn error(
        code: ReleaseVerificationCode,
        artifact_id: Option<String>,
        message: impl Into<String>,
        repair: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity: ReleaseVerificationSeverity::Error,
            artifact_id,
            message: message.into(),
            repair: repair.into(),
        }
    }
}

/// Stable verification report for a manifest or manifest JSON payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseVerificationReport {
    pub schema: String,
    pub manifest_schema: Option<String>,
    pub release_version: Option<String>,
    pub status: ReleaseVerificationStatus,
    pub artifacts_checked: usize,
    pub artifacts_passed: usize,
    pub artifacts_failed: usize,
    pub findings: Vec<ReleaseVerificationFinding>,
}

impl ReleaseVerificationReport {
    #[must_use]
    pub fn from_findings(
        manifest_schema: Option<String>,
        release_version: Option<String>,
        artifacts_checked: usize,
        findings: Vec<ReleaseVerificationFinding>,
    ) -> Self {
        let mut failed_artifacts = BTreeSet::new();
        let mut has_error = false;
        let mut has_warning = false;
        for finding in &findings {
            match finding.severity {
                ReleaseVerificationSeverity::Error => {
                    has_error = true;
                    if let Some(artifact_id) = &finding.artifact_id {
                        failed_artifacts.insert(artifact_id.as_str());
                    }
                }
                ReleaseVerificationSeverity::Warning => {
                    has_warning = true;
                }
            }
        }

        let status = if has_error {
            ReleaseVerificationStatus::Failed
        } else if has_warning {
            ReleaseVerificationStatus::Warning
        } else {
            ReleaseVerificationStatus::Passed
        };
        let artifacts_failed = failed_artifacts.len();
        let artifacts_passed = artifacts_checked.saturating_sub(artifacts_failed);

        Self {
            schema: RELEASE_MANIFEST_VERIFICATION_SCHEMA_V1.to_owned(),
            manifest_schema,
            release_version,
            status,
            artifacts_checked,
            artifacts_passed,
            artifacts_failed,
            findings,
        }
    }
}

/// Verify a manifest JSON payload while detecting future versions before typed parsing.
#[must_use]
pub fn verify_release_manifest_json(
    manifest_json: &str,
    artifact_root: Option<&Path>,
) -> ReleaseVerificationReport {
    let value: serde_json::Value = match serde_json::from_str(manifest_json) {
        Ok(value) => value,
        Err(error) => {
            return ReleaseVerificationReport::from_findings(
                None,
                None,
                0,
                vec![ReleaseVerificationFinding::error(
                    ReleaseVerificationCode::InvalidManifestJson,
                    None,
                    format!("manifest JSON could not be parsed: {error}"),
                    "Regenerate the manifest as valid JSON.",
                )],
            );
        }
    };

    let manifest_schema = value
        .get("schema")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    if manifest_schema.as_deref() != Some(RELEASE_MANIFEST_SCHEMA_V1) {
        let code = if manifest_schema
            .as_deref()
            .is_some_and(|schema| schema.starts_with("ee.release_manifest.v"))
        {
            ReleaseVerificationCode::UnsupportedFutureManifestVersion
        } else {
            ReleaseVerificationCode::InvalidManifestSchema
        };
        return ReleaseVerificationReport::from_findings(
            manifest_schema.clone(),
            None,
            0,
            vec![ReleaseVerificationFinding::error(
                code,
                None,
                format!(
                    "manifest schema '{}' is not supported",
                    manifest_schema.as_deref().unwrap_or("<missing>")
                ),
                "Use ee.release_manifest.v1 or upgrade ee for newer manifests.",
            )],
        );
    }

    match serde_json::from_value::<ReleaseManifest>(value) {
        Ok(manifest) => manifest.verify(artifact_root),
        Err(error) => ReleaseVerificationReport::from_findings(
            manifest_schema,
            None,
            0,
            vec![ReleaseVerificationFinding::error(
                ReleaseVerificationCode::InvalidManifestJson,
                None,
                format!("manifest JSON did not match release_manifest.v1: {error}"),
                "Regenerate the manifest from the release packaging command.",
            )],
        ),
    }
}

#[must_use]
pub fn release_artifact_id(
    binary_name: &str,
    release_version: &str,
    target_triple: &str,
) -> String {
    format!(
        "{}-{}-{}",
        binary_name,
        release_version.trim_start_matches('v'),
        target_triple
    )
}

#[must_use]
pub fn release_artifact_file_name(
    binary_name: &str,
    target_triple: &str,
    archive_format: ReleaseArchiveFormat,
) -> String {
    format!(
        "{}-{}.{}",
        binary_name,
        target_triple,
        archive_format.file_extension()
    )
}

#[must_use]
pub fn default_archive_format(target_triple: &str) -> ReleaseArchiveFormat {
    if target_triple.contains("windows") {
        ReleaseArchiveFormat::Zip
    } else {
        ReleaseArchiveFormat::TarXz
    }
}

#[must_use]
pub fn release_executable_name(binary_name: &str, target_triple: &str) -> String {
    if target_triple.contains("windows") {
        format!("{binary_name}.exe")
    } else {
        binary_name.to_owned()
    }
}

#[must_use]
pub fn default_install_path(binary_name: &str, target_triple: &str) -> String {
    if target_triple.contains("windows") {
        format!("%LOCALAPPDATA%\\Programs\\{binary_name}\\{binary_name}.exe")
    } else {
        format!("~/.local/bin/{binary_name}")
    }
}

#[must_use]
pub fn release_tag(release_version: &str) -> Option<String> {
    let trimmed = release_version.trim();
    if trimmed.is_empty() {
        None
    } else if trimmed.starts_with('v') {
        Some(trimmed.to_owned())
    } else {
        Some(format!("v{trimmed}"))
    }
}

#[must_use]
pub fn is_supported_release_target(target_triple: &str) -> bool {
    matches!(
        target_triple,
        "x86_64-unknown-linux-gnu"
            | "aarch64-unknown-linux-gnu"
            | "x86_64-unknown-linux-musl"
            | "aarch64-unknown-linux-musl"
            | "x86_64-apple-darwin"
            | "aarch64-apple-darwin"
            | "x86_64-pc-windows-msvc"
    )
}

#[must_use]
pub fn minimum_os_assumptions(target_triple: &str) -> Vec<String> {
    if target_triple.contains("apple-darwin") {
        vec!["macOS 12+".to_owned()]
    } else if target_triple.contains("windows") {
        vec!["Windows 10+".to_owned()]
    } else if target_triple.contains("musl") {
        vec!["Linux kernel 4.19+ with musl-compatible userspace".to_owned()]
    } else {
        vec!["Linux kernel 4.19+ with glibc 2.31+".to_owned()]
    }
}

#[must_use]
pub fn compatibility_notes_for_target(target_triple: &str) -> Vec<String> {
    if target_triple.contains("musl") {
        vec!["musl builds avoid host glibc requirements.".to_owned()]
    } else if target_triple.contains("windows") {
        vec!["PowerShell installer should place ee.exe on PATH.".to_owned()]
    } else {
        Vec::new()
    }
}

#[must_use]
pub fn is_safe_release_artifact_path(path: &str) -> bool {
    if !is_safe_relative_path(path) {
        return false;
    }
    path.ends_with(".tar.xz")
        || path.ends_with(".zip")
        || path.ends_with(".sha256")
        || path.ends_with(".sigstore.json")
        || path.ends_with(".intoto.jsonl")
}

#[must_use]
pub fn is_allowed_package_member_path(path: &str) -> bool {
    if !is_safe_relative_path(path) {
        return false;
    }
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    if normalized.starts_with(".git/")
        || normalized.starts_with(".ee/")
        || normalized.starts_with("target/")
        || normalized.starts_with("tests/")
        || normalized.starts_with("tmp/")
        || normalized.starts_with("temp/")
    {
        return false;
    }
    let file_name = normalized.rsplit('/').next().unwrap_or("");
    if file_name == ".env"
        || file_name.ends_with(".env")
        || file_name == "config.toml"
        || file_name.contains("secret")
        || file_name.contains("token")
        || file_name.ends_with(".db")
        || file_name.ends_with(".sqlite")
        || file_name.ends_with(".sqlite3")
        || file_name.ends_with(".pem")
        || file_name.ends_with(".key")
    {
        return false;
    }
    true
}

#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    lower_hex(digest.as_slice())
}

fn verify_artifact_file(
    artifact_root: &Path,
    artifact: &ReleaseArtifact,
    findings: &mut Vec<ReleaseVerificationFinding>,
) {
    let artifact_path = artifact_root.join(&artifact.file_name);
    let mut file = match File::open(&artifact_path) {
        Ok(file) => file,
        Err(_) => {
            findings.push(ReleaseVerificationFinding::error(
                ReleaseVerificationCode::MissingArtifact,
                Some(artifact.artifact_id.clone()),
                format!("artifact file '{}' is missing", artifact.file_name),
                "Place the archive next to the manifest or regenerate packaging output.",
            ));
            return;
        }
    };

    let mut bytes = Vec::new();
    if let Err(error) = file.read_to_end(&mut bytes) {
        findings.push(ReleaseVerificationFinding::error(
            ReleaseVerificationCode::MissingArtifact,
            Some(artifact.artifact_id.clone()),
            format!(
                "artifact file '{}' could not be read: {error}",
                artifact.file_name
            ),
            "Check artifact permissions and rerun verification.",
        ));
        return;
    }

    let actual = match artifact.checksum.algorithm {
        ReleaseChecksumAlgorithm::Sha256 => sha256_hex(&bytes),
        ReleaseChecksumAlgorithm::Blake3 => blake3::hash(&bytes).to_hex().to_string(),
    };
    if actual != artifact.checksum.value {
        findings.push(ReleaseVerificationFinding::error(
            ReleaseVerificationCode::ChecksumMismatch,
            Some(artifact.artifact_id.clone()),
            format!("checksum mismatch for '{}'", artifact.file_name),
            "Rebuild the archive or update the manifest checksum from trusted inputs.",
        ));
    }
}

fn is_safe_relative_path(path: &str) -> bool {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed.contains('\0') || trimmed.contains('\\') {
        return false;
    }
    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        return false;
    }
    let mut has_normal = false;
    for component in candidate.components() {
        match component {
            Component::Normal(_) => has_normal = true,
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return false;
            }
        }
    }
    has_normal
}

fn lower_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        push_hex_nibble(&mut output, byte >> 4);
        push_hex_nibble(&mut output, byte & 0x0f);
    }
    output
}

fn push_hex_nibble(output: &mut String, nibble: u8) {
    let digit = match nibble {
        0..=9 => b'0' + nibble,
        10..=15 => b'a' + (nibble - 10),
        _ => b'0',
    };
    output.push(char::from(digit));
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

    fn ensure_equal<T: fmt::Debug + PartialEq>(
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

    fn finding_codes(report: &ReleaseVerificationReport) -> Vec<ReleaseVerificationCode> {
        report.findings.iter().map(|finding| finding.code).collect()
    }

    #[test]
    fn artifact_names_are_deterministic_and_match_release_assets() -> TestResult {
        ensure_equal(
            release_artifact_file_name(
                RELEASE_BINARY_NAME,
                "x86_64-unknown-linux-gnu",
                ReleaseArchiveFormat::TarXz,
            ),
            "ee-x86_64-unknown-linux-gnu.tar.xz".to_owned(),
            "linux archive name",
        )?;
        ensure_equal(
            release_artifact_file_name(
                RELEASE_BINARY_NAME,
                "x86_64-pc-windows-msvc",
                ReleaseArchiveFormat::Zip,
            ),
            "ee-x86_64-pc-windows-msvc.zip".to_owned(),
            "windows archive name",
        )?;
        ensure_equal(
            release_artifact_id(RELEASE_BINARY_NAME, "v0.1.0", "aarch64-apple-darwin"),
            "ee-0.1.0-aarch64-apple-darwin".to_owned(),
            "artifact id strips tag prefix",
        )
    }

    #[test]
    fn manifest_sorts_artifacts_for_stable_serialization() -> TestResult {
        let linux = ReleaseArtifact::from_bytes(
            "0.1.0",
            "0123456789abcdef0123456789abcdef01234567",
            "x86_64-unknown-linux-gnu",
            b"linux",
        );
        let mac = ReleaseArtifact::from_bytes(
            "0.1.0",
            "0123456789abcdef0123456789abcdef01234567",
            "aarch64-apple-darwin",
            b"mac",
        );
        let manifest = ReleaseManifest::new(
            "0.1.0",
            "0123456789abcdef0123456789abcdef01234567",
            vec![linux, mac],
        );

        let first = manifest
            .artifacts
            .first()
            .ok_or_else(|| "missing first artifact".to_owned())?;
        let second = manifest
            .artifacts
            .get(1)
            .ok_or_else(|| "missing second artifact".to_owned())?;
        ensure_equal(
            first.target_triple.as_str(),
            "aarch64-apple-darwin",
            "sorted first target",
        )?;
        ensure_equal(
            second.target_triple.as_str(),
            "x86_64-unknown-linux-gnu",
            "sorted second target",
        )?;
        ensure_equal(manifest.artifact_count, 2, "artifact count")
    }

    #[test]
    fn checksum_generation_is_stable() -> TestResult {
        ensure_equal(
            sha256_hex(b"ee release artifact\n"),
            "d9bbbfc6ff01443643ed713ffc96aec8f7406d210b53bac57baa1ebd0e795778".to_owned(),
            "sha256",
        )?;
        ensure(
            ReleaseChecksum::sha256_bytes(b"abc").is_well_formed(),
            "sha256 well formed",
        )?;
        ensure(
            !ReleaseChecksum {
                algorithm: ReleaseChecksumAlgorithm::Sha256,
                value: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
                    .to_owned(),
            }
            .is_well_formed(),
            "uppercase checksum rejected",
        )
    }

    #[test]
    fn unsafe_package_members_are_excluded() -> TestResult {
        ensure(
            is_allowed_package_member_path("bin/ee"),
            "binary member allowed",
        )?;
        ensure(
            is_allowed_package_member_path("README.md"),
            "readme allowed",
        )?;
        ensure(
            is_allowed_package_member_path("schemas/release_manifest.v1.json"),
            "schema member allowed",
        )?;
        ensure(
            !is_allowed_package_member_path(".ee/config.toml"),
            "ee config denied",
        )?;
        ensure(
            !is_allowed_package_member_path("target/release/ee"),
            "target denied",
        )?;
        ensure(
            !is_allowed_package_member_path("tests/output.json"),
            "test artifact denied",
        )?;
        ensure(
            !is_allowed_package_member_path("../escape"),
            "parent escape denied",
        )?;
        ensure(
            !is_allowed_package_member_path("..\\escape"),
            "windows parent escape denied",
        )?;
        ensure(
            !is_allowed_package_member_path("workspace.sqlite"),
            "sqlite denied",
        )
    }

    #[test]
    fn verification_detects_duplicate_unsupported_and_stale_metadata() -> TestResult {
        let mut artifact =
            ReleaseArtifact::from_bytes("0.1.0", "commit-a", "x86_64-unknown-linux-gnu", b"binary");
        artifact.target_triple = "sparc64-unknown-plan9".to_owned();
        artifact.release_version = "0.0.9".to_owned();
        let duplicate = artifact.clone();
        let manifest = ReleaseManifest {
            artifact_count: 3,
            artifacts: vec![artifact, duplicate],
            ..ReleaseManifest::new("0.1.0", "commit-a", Vec::new())
        };

        let codes = finding_codes(&manifest.verify(None));
        ensure(
            codes.contains(&ReleaseVerificationCode::ArtifactCountMismatch),
            "artifact count mismatch detected",
        )?;
        ensure(
            codes.contains(&ReleaseVerificationCode::DuplicateArtifactId),
            "duplicate id detected",
        )?;
        ensure(
            codes.contains(&ReleaseVerificationCode::UnsupportedTarget),
            "unsupported target detected",
        )?;
        ensure(
            codes.contains(&ReleaseVerificationCode::StaleVersionMetadata),
            "stale version detected",
        )
    }

    #[test]
    fn unsafe_artifact_paths_are_not_opened_from_artifact_root() -> TestResult {
        let artifact =
            ReleaseArtifact::from_bytes("0.1.0", "commit-a", "x86_64-unknown-linux-gnu", b"binary");
        let manifest = ReleaseManifest {
            artifact_count: 1,
            artifacts: vec![ReleaseArtifact {
                file_name: "../escape.tar.xz".to_owned(),
                ..artifact
            }],
            ..ReleaseManifest::new("0.1.0", "commit-a", Vec::new())
        };
        let report = manifest.verify(Some(Path::new("/")));
        let codes = finding_codes(&report);

        ensure(
            codes.contains(&ReleaseVerificationCode::UnsafeArtifactPath),
            "unsafe artifact path detected",
        )?;
        ensure(
            !codes.contains(&ReleaseVerificationCode::MissingArtifact),
            "unsafe artifact path is not opened",
        )
    }
}
