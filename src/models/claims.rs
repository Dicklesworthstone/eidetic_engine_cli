//! Claims schema and manifest verification rules (EE-361).
//!
//! This module defines the claims.yaml schema and artifact manifest verification
//! rules for executable claims in the Alien Graveyard system.
//!
//! # Claims.yaml Schema
//!
//! The claims.yaml file defines product claims that can be verified through
//! executable evidence. Each claim has an ID, description, verification policy,
//! and links to evidence artifacts.
//!
//! # Artifact Manifest Schema
//!
//! Each claim's evidence lives in `artifacts/<claim_id>/manifest.json`, which
//! describes the evidence files, their checksums, and verification status.

use std::fmt;

use super::{ClaimId, DemoId, EvidenceId, PolicyId, TraceId};

pub const CLAIMS_FILE_SCHEMA_V1: &str = "ee.claims_file.v1";
pub const CLAIM_ENTRY_SCHEMA_V1: &str = "ee.claim_entry.v1";
pub const CLAIM_MANIFEST_SCHEMA_V1: &str = "ee.claim_manifest.v1";
pub const MANIFEST_ARTIFACT_SCHEMA_V1: &str = "ee.manifest_artifact.v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaimStatus {
    Draft,
    Active,
    Verified,
    Stale,
    Regressed,
    Retired,
}

impl ClaimStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Verified => "verified",
            Self::Stale => "stale",
            Self::Regressed => "regressed",
            Self::Retired => "retired",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 6] {
        [
            Self::Draft,
            Self::Active,
            Self::Verified,
            Self::Stale,
            Self::Regressed,
            Self::Retired,
        ]
    }
}

impl fmt::Display for ClaimStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseClaimStatusError(pub String);

impl fmt::Display for ParseClaimStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown claim status: {}", self.0)
    }
}

impl std::error::Error for ParseClaimStatusError {}

impl std::str::FromStr for ClaimStatus {
    type Err = ParseClaimStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "draft" => Ok(Self::Draft),
            "active" => Ok(Self::Active),
            "verified" => Ok(Self::Verified),
            "stale" => Ok(Self::Stale),
            "regressed" => Ok(Self::Regressed),
            "retired" => Ok(Self::Retired),
            other => Err(ParseClaimStatusError(other.to_owned())),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerificationFrequency {
    OnChange,
    Daily,
    Weekly,
    Manual,
}

impl VerificationFrequency {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OnChange => "on_change",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Manual => "manual",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::OnChange, Self::Daily, Self::Weekly, Self::Manual]
    }
}

impl fmt::Display for VerificationFrequency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseVerificationFrequencyError(pub String);

impl fmt::Display for ParseVerificationFrequencyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown verification frequency: {}", self.0)
    }
}

impl std::error::Error for ParseVerificationFrequencyError {}

impl std::str::FromStr for VerificationFrequency {
    type Err = ParseVerificationFrequencyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "on_change" => Ok(Self::OnChange),
            "daily" => Ok(Self::Daily),
            "weekly" => Ok(Self::Weekly),
            "manual" => Ok(Self::Manual),
            other => Err(ParseVerificationFrequencyError(other.to_owned())),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactType {
    GoldenFixture,
    SchemaContract,
    Screenshot,
    Log,
    Benchmark,
    Replay,
    Report,
}

impl ArtifactType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GoldenFixture => "golden_fixture",
            Self::SchemaContract => "schema_contract",
            Self::Screenshot => "screenshot",
            Self::Log => "log",
            Self::Benchmark => "benchmark",
            Self::Replay => "replay",
            Self::Report => "report",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 7] {
        [
            Self::GoldenFixture,
            Self::SchemaContract,
            Self::Screenshot,
            Self::Log,
            Self::Benchmark,
            Self::Replay,
            Self::Report,
        ]
    }
}

impl fmt::Display for ArtifactType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseArtifactTypeError(pub String);

impl fmt::Display for ParseArtifactTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown artifact type: {}", self.0)
    }
}

impl std::error::Error for ParseArtifactTypeError {}

impl std::str::FromStr for ArtifactType {
    type Err = ParseArtifactTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "golden_fixture" => Ok(Self::GoldenFixture),
            "schema_contract" => Ok(Self::SchemaContract),
            "screenshot" => Ok(Self::Screenshot),
            "log" => Ok(Self::Log),
            "benchmark" => Ok(Self::Benchmark),
            "replay" => Ok(Self::Replay),
            "report" => Ok(Self::Report),
            other => Err(ParseArtifactTypeError(other.to_owned())),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimEntry {
    pub id: ClaimId,
    pub title: String,
    pub description: String,
    pub status: ClaimStatus,
    pub policy_id: Option<PolicyId>,
    pub frequency: VerificationFrequency,
    pub evidence_ids: Vec<EvidenceId>,
    pub demo_ids: Vec<DemoId>,
    pub tags: Vec<String>,
}

impl ClaimEntry {
    #[must_use]
    pub fn new(id: ClaimId, title: String, description: String) -> Self {
        Self {
            id,
            title,
            description,
            status: ClaimStatus::Draft,
            policy_id: None,
            frequency: VerificationFrequency::OnChange,
            evidence_ids: Vec::new(),
            demo_ids: Vec::new(),
            tags: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimsFile {
    pub schema: &'static str,
    pub version: u32,
    pub claims: Vec<ClaimEntry>,
}

impl Default for ClaimsFile {
    fn default() -> Self {
        Self {
            schema: CLAIMS_FILE_SCHEMA_V1,
            version: 1,
            claims: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestArtifact {
    pub path: String,
    pub artifact_type: ArtifactType,
    pub blake3_hash: String,
    pub size_bytes: u64,
    pub created_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimManifest {
    pub schema: &'static str,
    pub claim_id: ClaimId,
    pub artifacts: Vec<ManifestArtifact>,
    pub last_verified_at: Option<String>,
    pub last_trace_id: Option<TraceId>,
    pub verification_status: ManifestVerificationStatus,
}

impl ClaimManifest {
    #[must_use]
    pub fn new(claim_id: ClaimId) -> Self {
        Self {
            schema: CLAIM_MANIFEST_SCHEMA_V1,
            claim_id,
            artifacts: Vec::new(),
            last_verified_at: None,
            last_trace_id: None,
            verification_status: ManifestVerificationStatus::Unverified,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManifestVerificationStatus {
    Unverified,
    Passing,
    Failing,
    Stale,
    Incomplete,
}

impl ManifestVerificationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::Passing => "passing",
            Self::Failing => "failing",
            Self::Stale => "stale",
            Self::Incomplete => "incomplete",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Unverified,
            Self::Passing,
            Self::Failing,
            Self::Stale,
            Self::Incomplete,
        ]
    }
}

impl fmt::Display for ManifestVerificationStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseManifestVerificationStatusError(pub String);

impl fmt::Display for ParseManifestVerificationStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown manifest verification status: {}", self.0)
    }
}

impl std::error::Error for ParseManifestVerificationStatusError {}

impl std::str::FromStr for ManifestVerificationStatus {
    type Err = ParseManifestVerificationStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "unverified" => Ok(Self::Unverified),
            "passing" => Ok(Self::Passing),
            "failing" => Ok(Self::Failing),
            "stale" => Ok(Self::Stale),
            "incomplete" => Ok(Self::Incomplete),
            other => Err(ParseManifestVerificationStatusError(other.to_owned())),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManifestValidationErrorKind {
    MissingSchema,
    InvalidSchema,
    MissingClaimId,
    InvalidClaimId,
    MissingArtifacts,
    InvalidArtifactPath,
    InvalidHash,
    HashMismatch,
    ArtifactNotFound,
    DuplicatePath,
}

impl ManifestValidationErrorKind {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::MissingSchema => "missing_schema",
            Self::InvalidSchema => "invalid_schema",
            Self::MissingClaimId => "missing_claim_id",
            Self::InvalidClaimId => "invalid_claim_id",
            Self::MissingArtifacts => "missing_artifacts",
            Self::InvalidArtifactPath => "invalid_artifact_path",
            Self::InvalidHash => "invalid_hash",
            Self::HashMismatch => "hash_mismatch",
            Self::ArtifactNotFound => "artifact_not_found",
            Self::DuplicatePath => "duplicate_path",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestValidationError {
    pub kind: ManifestValidationErrorKind,
    pub message: String,
    pub path: Option<String>,
}

impl fmt::Display for ManifestValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(path) = &self.path {
            write!(f, "{}: {} (at {})", self.kind.code(), self.message, path)
        } else {
            write!(f, "{}: {}", self.kind.code(), self.message)
        }
    }
}

impl std::error::Error for ManifestValidationError {}

impl ManifestValidationError {
    #[must_use]
    pub fn missing_schema() -> Self {
        Self {
            kind: ManifestValidationErrorKind::MissingSchema,
            message: "manifest.json must have a 'schema' field".to_owned(),
            path: None,
        }
    }

    #[must_use]
    pub fn invalid_schema(actual: &str) -> Self {
        Self {
            kind: ManifestValidationErrorKind::InvalidSchema,
            message: format!(
                "expected schema '{}', got '{}'",
                CLAIM_MANIFEST_SCHEMA_V1, actual
            ),
            path: None,
        }
    }

    #[must_use]
    pub fn missing_claim_id() -> Self {
        Self {
            kind: ManifestValidationErrorKind::MissingClaimId,
            message: "manifest.json must have a 'claimId' field".to_owned(),
            path: None,
        }
    }

    #[must_use]
    pub fn invalid_claim_id(id: &str, reason: &str) -> Self {
        Self {
            kind: ManifestValidationErrorKind::InvalidClaimId,
            message: format!("invalid claim ID '{}': {}", id, reason),
            path: None,
        }
    }

    #[must_use]
    pub fn missing_artifacts() -> Self {
        Self {
            kind: ManifestValidationErrorKind::MissingArtifacts,
            message: "manifest.json must have an 'artifacts' array".to_owned(),
            path: None,
        }
    }

    #[must_use]
    pub fn invalid_artifact_path(path: &str, reason: &str) -> Self {
        Self {
            kind: ManifestValidationErrorKind::InvalidArtifactPath,
            message: format!("invalid artifact path '{}': {}", path, reason),
            path: Some(path.to_owned()),
        }
    }

    #[must_use]
    pub fn invalid_hash(path: &str, hash: &str) -> Self {
        Self {
            kind: ManifestValidationErrorKind::InvalidHash,
            message: format!("invalid blake3 hash '{}' for artifact '{}'", hash, path),
            path: Some(path.to_owned()),
        }
    }

    #[must_use]
    pub fn hash_mismatch(path: &str, expected: &str, actual: &str) -> Self {
        Self {
            kind: ManifestValidationErrorKind::HashMismatch,
            message: format!(
                "hash mismatch for '{}': expected {}, got {}",
                path, expected, actual
            ),
            path: Some(path.to_owned()),
        }
    }

    #[must_use]
    pub fn artifact_not_found(path: &str) -> Self {
        Self {
            kind: ManifestValidationErrorKind::ArtifactNotFound,
            message: format!("artifact file not found: {}", path),
            path: Some(path.to_owned()),
        }
    }

    #[must_use]
    pub fn duplicate_path(path: &str) -> Self {
        Self {
            kind: ManifestValidationErrorKind::DuplicatePath,
            message: format!("duplicate artifact path: {}", path),
            path: Some(path.to_owned()),
        }
    }
}

pub const BLAKE3_HEX_LEN: usize = 64;

#[must_use]
pub fn is_valid_blake3_hex(s: &str) -> bool {
    s.len() == BLAKE3_HEX_LEN && s.chars().all(|c| c.is_ascii_hexdigit())
}

#[must_use]
pub fn is_valid_artifact_path(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    if path.starts_with('/') || path.starts_with("..") {
        return false;
    }
    if path.contains("..") {
        return false;
    }
    true
}

pub fn validate_manifest_structure(
    schema: Option<&str>,
    claim_id: Option<&str>,
    artifacts_present: bool,
) -> Result<(), ManifestValidationError> {
    let schema = schema.ok_or_else(ManifestValidationError::missing_schema)?;
    if schema != CLAIM_MANIFEST_SCHEMA_V1 {
        return Err(ManifestValidationError::invalid_schema(schema));
    }

    let claim_id = claim_id.ok_or_else(ManifestValidationError::missing_claim_id)?;
    if claim_id.parse::<ClaimId>().is_err() {
        return Err(ManifestValidationError::invalid_claim_id(
            claim_id,
            "must match claim_<26-char-base32> format",
        ));
    }

    if !artifacts_present {
        return Err(ManifestValidationError::missing_artifacts());
    }

    Ok(())
}

pub fn validate_artifact_entry(
    path: &str,
    hash: &str,
    seen_paths: &mut std::collections::HashSet<String>,
) -> Result<(), ManifestValidationError> {
    if !is_valid_artifact_path(path) {
        return Err(ManifestValidationError::invalid_artifact_path(
            path,
            "path must be relative and cannot contain '..'",
        ));
    }

    if !is_valid_blake3_hex(hash) {
        return Err(ManifestValidationError::invalid_hash(path, hash));
    }

    if !seen_paths.insert(path.to_owned()) {
        return Err(ManifestValidationError::duplicate_path(path));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::testing::{TestResult, ensure, ensure_equal};

    use super::*;

    #[test]
    fn claim_status_roundtrip() -> TestResult {
        for status in ClaimStatus::all() {
            let s = status.as_str();
            let parsed: ClaimStatus = s
                .parse()
                .map_err(|e: ParseClaimStatusError| e.to_string())?;
            ensure_equal(&parsed, &status, &format!("roundtrip for {s}"))?;
        }
        Ok(())
    }

    #[test]
    fn claim_status_parse_unknown_returns_error() -> TestResult {
        let result = "unknown".parse::<ClaimStatus>();
        ensure(result.is_err(), "should error on unknown status")
    }

    #[test]
    fn verification_frequency_roundtrip() -> TestResult {
        for freq in VerificationFrequency::all() {
            let s = freq.as_str();
            let parsed: VerificationFrequency = s
                .parse()
                .map_err(|e: ParseVerificationFrequencyError| e.to_string())?;
            ensure_equal(&parsed, &freq, &format!("roundtrip for {s}"))?;
        }
        Ok(())
    }

    #[test]
    fn artifact_type_roundtrip() -> TestResult {
        for t in ArtifactType::all() {
            let s = t.as_str();
            let parsed: ArtifactType = s
                .parse()
                .map_err(|e: ParseArtifactTypeError| e.to_string())?;
            ensure_equal(&parsed, &t, &format!("roundtrip for {s}"))?;
        }
        Ok(())
    }

    #[test]
    fn manifest_verification_status_roundtrip() -> TestResult {
        for status in ManifestVerificationStatus::all() {
            let s = status.as_str();
            let parsed: ManifestVerificationStatus = s
                .parse()
                .map_err(|e: ParseManifestVerificationStatusError| e.to_string())?;
            ensure_equal(&parsed, &status, &format!("roundtrip for {s}"))?;
        }
        Ok(())
    }

    #[test]
    fn is_valid_blake3_hex_accepts_valid() -> TestResult {
        let valid = "a".repeat(64);
        ensure(is_valid_blake3_hex(&valid), "should accept 64 hex chars")?;
        let valid_mixed = "0123456789abcdefABCDEF".repeat(3)[..64].to_string();
        ensure(
            is_valid_blake3_hex(&valid_mixed),
            "should accept mixed case hex",
        )
    }

    #[test]
    fn is_valid_blake3_hex_rejects_invalid() -> TestResult {
        ensure(!is_valid_blake3_hex(""), "should reject empty")?;
        ensure(
            !is_valid_blake3_hex(&"a".repeat(63)),
            "should reject 63 chars",
        )?;
        ensure(
            !is_valid_blake3_hex(&"a".repeat(65)),
            "should reject 65 chars",
        )?;
        ensure(
            !is_valid_blake3_hex(&format!("{}g", "a".repeat(63))),
            "should reject non-hex",
        )
    }

    #[test]
    fn is_valid_artifact_path_accepts_valid() -> TestResult {
        ensure(is_valid_artifact_path("file.txt"), "simple filename")?;
        ensure(is_valid_artifact_path("dir/file.txt"), "with directory")?;
        ensure(is_valid_artifact_path("a/b/c/d.json"), "nested directories")
    }

    #[test]
    fn is_valid_artifact_path_rejects_invalid() -> TestResult {
        ensure(!is_valid_artifact_path(""), "empty path")?;
        ensure(!is_valid_artifact_path("/absolute"), "absolute path")?;
        ensure(!is_valid_artifact_path("../escape"), "parent escape")?;
        ensure(!is_valid_artifact_path("dir/../escape"), "embedded escape")
    }

    #[test]
    fn validate_manifest_structure_requires_schema() -> TestResult {
        let result =
            validate_manifest_structure(None, Some("claim_00000000000000000000000000"), true);
        ensure(
            matches!(
                result,
                Err(ManifestValidationError {
                    kind: ManifestValidationErrorKind::MissingSchema,
                    ..
                })
            ),
            "should error on missing schema",
        )
    }

    #[test]
    fn validate_manifest_structure_requires_correct_schema() -> TestResult {
        let result = validate_manifest_structure(
            Some("wrong.schema"),
            Some("claim_00000000000000000000000000"),
            true,
        );
        ensure(
            matches!(
                result,
                Err(ManifestValidationError {
                    kind: ManifestValidationErrorKind::InvalidSchema,
                    ..
                })
            ),
            "should error on wrong schema",
        )
    }

    #[test]
    fn validate_manifest_structure_requires_claim_id() -> TestResult {
        let result = validate_manifest_structure(Some(CLAIM_MANIFEST_SCHEMA_V1), None, true);
        ensure(
            matches!(
                result,
                Err(ManifestValidationError {
                    kind: ManifestValidationErrorKind::MissingClaimId,
                    ..
                })
            ),
            "should error on missing claim_id",
        )
    }

    #[test]
    fn validate_manifest_structure_requires_valid_claim_id() -> TestResult {
        let result = validate_manifest_structure(
            Some(CLAIM_MANIFEST_SCHEMA_V1),
            Some("invalid_claim_id"),
            true,
        );
        ensure(
            matches!(
                result,
                Err(ManifestValidationError {
                    kind: ManifestValidationErrorKind::InvalidClaimId,
                    ..
                })
            ),
            "should error on invalid claim_id format",
        )
    }

    #[test]
    fn validate_manifest_structure_requires_artifacts() -> TestResult {
        let result = validate_manifest_structure(
            Some(CLAIM_MANIFEST_SCHEMA_V1),
            Some("claim_00000000000000000000000000"),
            false,
        );
        ensure(
            matches!(
                result,
                Err(ManifestValidationError {
                    kind: ManifestValidationErrorKind::MissingArtifacts,
                    ..
                })
            ),
            "should error on missing artifacts",
        )
    }

    #[test]
    fn validate_manifest_structure_accepts_valid() -> TestResult {
        let result = validate_manifest_structure(
            Some(CLAIM_MANIFEST_SCHEMA_V1),
            Some("claim_00000000000000000000000000"),
            true,
        );
        ensure(result.is_ok(), "should accept valid manifest structure")
    }

    #[test]
    fn validate_artifact_entry_catches_invalid_path() -> TestResult {
        let mut seen = std::collections::HashSet::new();
        let hash = "a".repeat(64);
        let result = validate_artifact_entry("../escape.txt", &hash, &mut seen);
        ensure(
            matches!(
                result,
                Err(ManifestValidationError {
                    kind: ManifestValidationErrorKind::InvalidArtifactPath,
                    ..
                })
            ),
            "should catch path traversal",
        )
    }

    #[test]
    fn validate_artifact_entry_catches_invalid_hash() -> TestResult {
        let mut seen = std::collections::HashSet::new();
        let result = validate_artifact_entry("file.txt", "tooshort", &mut seen);
        ensure(
            matches!(
                result,
                Err(ManifestValidationError {
                    kind: ManifestValidationErrorKind::InvalidHash,
                    ..
                })
            ),
            "should catch invalid hash",
        )
    }

    #[test]
    fn validate_artifact_entry_catches_duplicate_path() -> TestResult {
        let mut seen = std::collections::HashSet::new();
        let hash = "a".repeat(64);
        validate_artifact_entry("file.txt", &hash, &mut seen).map_err(|e| e.to_string())?;
        let result = validate_artifact_entry("file.txt", &hash, &mut seen);
        ensure(
            matches!(
                result,
                Err(ManifestValidationError {
                    kind: ManifestValidationErrorKind::DuplicatePath,
                    ..
                })
            ),
            "should catch duplicate path",
        )
    }

    #[test]
    fn claim_entry_default_values() -> TestResult {
        let id = ClaimId::now();
        let entry = ClaimEntry::new(id, "Test".to_owned(), "Description".to_owned());
        ensure_equal(&entry.id, &id, "id")?;
        ensure_equal(&entry.status, &ClaimStatus::Draft, "default status")?;
        ensure_equal(
            &entry.frequency,
            &VerificationFrequency::OnChange,
            "default frequency",
        )?;
        ensure(entry.evidence_ids.is_empty(), "evidence_ids empty")?;
        ensure(entry.demo_ids.is_empty(), "demo_ids empty")?;
        ensure(entry.tags.is_empty(), "tags empty")
    }

    #[test]
    fn claim_manifest_new_has_correct_defaults() -> TestResult {
        let id = ClaimId::now();
        let manifest = ClaimManifest::new(id);
        ensure_equal(&manifest.schema, &CLAIM_MANIFEST_SCHEMA_V1, "schema")?;
        ensure_equal(&manifest.claim_id, &id, "claim_id")?;
        ensure(manifest.artifacts.is_empty(), "artifacts empty")?;
        ensure(manifest.last_verified_at.is_none(), "last_verified_at none")?;
        ensure(manifest.last_trace_id.is_none(), "last_trace_id none")?;
        ensure_equal(
            &manifest.verification_status,
            &ManifestVerificationStatus::Unverified,
            "default status",
        )
    }

    #[test]
    fn manifest_validation_error_codes_are_stable() -> TestResult {
        ensure_equal(
            &ManifestValidationErrorKind::MissingSchema.code(),
            &"missing_schema",
            "missing_schema code",
        )?;
        ensure_equal(
            &ManifestValidationErrorKind::InvalidSchema.code(),
            &"invalid_schema",
            "invalid_schema code",
        )?;
        ensure_equal(
            &ManifestValidationErrorKind::HashMismatch.code(),
            &"hash_mismatch",
            "hash_mismatch code",
        )
    }

    #[test]
    fn schema_constants_follow_convention() -> TestResult {
        ensure(
            CLAIMS_FILE_SCHEMA_V1.starts_with("ee."),
            "claims file schema prefix",
        )?;
        ensure(
            CLAIM_ENTRY_SCHEMA_V1.starts_with("ee."),
            "claim entry schema prefix",
        )?;
        ensure(
            CLAIM_MANIFEST_SCHEMA_V1.starts_with("ee."),
            "claim manifest schema prefix",
        )?;
        ensure(
            MANIFEST_ARTIFACT_SCHEMA_V1.starts_with("ee."),
            "manifest artifact schema prefix",
        )
    }
}
