//! Certificate operations (EE-342).
//!
//! Provides list, show, and verify operations for certificate records.
//! Certificates are typed verification artifacts that make "alien artifact
//! math" inspectable and auditable.
//!
//! Without an explicit manifest path, core operations return honest
//! empty/not-found reports instead of sample records.
//!
//! # Honesty note: certificate "attestations" are content hashes, not signatures
//!
//! In this slice, ee certificates are **content-addressed**, not
//! cryptographically signed. The optional `signature` / `signature_algorithm`
//! / `signer` columns on the certificate record store an *attestation*: a
//! domain-separated SHA-256 digest of `(algorithm-tag, signer, payload_hash)`.
//!
//! That digest proves only that whoever wrote the record knew the same
//! `(signer, payload_hash)` triple — values that are themselves stored in
//! plaintext on the same record. There is **no key, no nonce, no secret
//! material**, so an attacker who can see a certificate can mint a fresh
//! attestation for any `signer` they like. Treat `attestation_ok` as
//! "the recorded attestation matches its own publicly-derivable form",
//! not as "an authorized key holder produced this".
//!
//! The single supported algorithm string is `ee.local-content-hash.v1`. The
//! previous `sigstore.bundle-sha256.v1` algorithm name has been removed —
//! it implied real sigstore bundle verification (rekor inclusion proof,
//! fulcio cert chain, OIDC binding) that this slice never performed.
//!
//! Real signing is implemented via ed25519 using the `ring` crate. The algorithm
//! string `ee.ed25519.v1` represents a genuine cryptographic signature where:
//!
//! - The workspace holds a secret key at `~/.config/ee/keys/<workspace>.ed25519`
//! - The signer field contains the public key fingerprint: `ed25519:fp:<hex>`
//! - The signature field contains the ed25519 signature over the payload hash
//! - Verification loads the public key by fingerprint and verifies the signature
//!
//! A forged signature produced from public inputs alone MUST fail verification.

use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use ring::rand::SystemRandom;
use ring::signature::{self, Ed25519KeyPair, KeyPair};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::db::{DbConnection, StoredCertificateRecord};
use crate::models::{Certificate, CertificateKind, CertificateStatus, DecisionPlaneMetadata};

/// Schema version for certificate list responses.
pub const CERTIFICATE_LIST_SCHEMA_V1: &str = "ee.certificate.list.v1";

/// Schema version for certificate show responses.
pub const CERTIFICATE_SHOW_SCHEMA_V1: &str = "ee.certificate.show.v1";

/// Schema version for certificate verify responses.
pub const CERTIFICATE_VERIFY_SCHEMA_V1: &str = "ee.certificate.verify.v1";

/// Schema version for certificate manifest stores consumed by the core.
pub const CERTIFICATE_MANIFEST_SCHEMA_V1: &str = "ee.certificate.manifest.v1";

/// Supported payload schema version for certificate hash verification.
pub const CERTIFICATE_PAYLOAD_SCHEMA_V1: &str = "ee.certificate.payload.v1";

/// Schema version for certificate sign responses.
pub const CERTIFICATE_SIGN_SCHEMA_V1: &str = "ee.certificate.sign.v1";

/// Schema version for certificate keygen responses.
pub const CERTIFICATE_KEYGEN_SCHEMA_V1: &str = "ee.certificate.keygen.v1";

/// Algorithm string for real ed25519 signatures.
pub const ED25519_ALGORITHM_V1: &str = "ee.ed25519.v1";

/// Default key directory relative to user config.
const KEY_DIR_NAME: &str = "ee/keys";

/// Options for listing certificates.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CertificateListOptions {
    /// Filter by certificate kind.
    pub kind: Option<CertificateKind>,
    /// Filter by certificate status.
    pub status: Option<CertificateStatus>,
    /// Maximum number of certificates to return.
    pub limit: Option<u32>,
    /// Include expired certificates.
    pub include_expired: bool,
    /// Optional explicit certificate manifest path.
    pub manifest_path: Option<PathBuf>,
    /// Optional workspace database path for persisted certificate records.
    pub database_path: Option<PathBuf>,
    /// Workspace ID for database-backed certificate queries.
    pub workspace_id: Option<String>,
}

impl CertificateListOptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_kind(mut self, kind: CertificateKind) -> Self {
        self.kind = Some(kind);
        self
    }

    #[must_use]
    pub fn with_status(mut self, status: CertificateStatus) -> Self {
        self.status = Some(status);
        self
    }

    #[must_use]
    pub fn with_limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    #[must_use]
    pub fn include_expired(mut self) -> Self {
        self.include_expired = true;
        self
    }

    #[must_use]
    pub fn with_manifest_path(mut self, manifest_path: impl Into<PathBuf>) -> Self {
        self.manifest_path = Some(manifest_path.into());
        self
    }

    #[must_use]
    pub fn with_optional_manifest_path(mut self, manifest_path: Option<&Path>) -> Self {
        if let Some(manifest_path) = manifest_path {
            self.manifest_path = Some(manifest_path.to_path_buf());
        }
        self
    }

    #[must_use]
    pub fn with_database_path(mut self, database_path: impl Into<PathBuf>) -> Self {
        self.database_path = Some(database_path.into());
        self
    }

    #[must_use]
    pub fn with_workspace_id(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = Some(workspace_id.into());
        self
    }
}

/// Options for showing or verifying one certificate from a manifest store.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CertificateLookupOptions {
    /// Optional explicit certificate manifest path.
    pub manifest_path: Option<PathBuf>,
    /// Optional workspace database path for persisted certificate records.
    pub database_path: Option<PathBuf>,
    /// Workspace ID for database-backed certificate queries.
    pub workspace_id: Option<String>,
    /// Certificate ID to show or verify.
    pub certificate_id: String,
}

impl CertificateLookupOptions {
    #[must_use]
    pub fn new(certificate_id: impl Into<String>) -> Self {
        Self {
            manifest_path: None,
            database_path: None,
            workspace_id: None,
            certificate_id: certificate_id.into(),
        }
    }

    #[must_use]
    pub fn with_manifest_path(mut self, manifest_path: impl Into<PathBuf>) -> Self {
        self.manifest_path = Some(manifest_path.into());
        self
    }

    #[must_use]
    pub fn with_optional_manifest_path(mut self, manifest_path: Option<&Path>) -> Self {
        if let Some(manifest_path) = manifest_path {
            self.manifest_path = Some(manifest_path.to_path_buf());
        }
        self
    }

    #[must_use]
    pub fn with_database_path(mut self, database_path: impl Into<PathBuf>) -> Self {
        self.database_path = Some(database_path.into());
        self
    }

    #[must_use]
    pub fn with_workspace_id(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = Some(workspace_id.into());
        self
    }
}

/// Certificate summary for list display.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertificateSummary {
    pub id: String,
    pub kind: CertificateKind,
    pub status: CertificateStatus,
    pub issued_at: String,
    pub workspace_id: String,
    pub is_usable: bool,
}

impl From<&Certificate> for CertificateSummary {
    fn from(cert: &Certificate) -> Self {
        Self {
            id: cert.id.clone(),
            kind: cert.kind,
            status: cert.status,
            issued_at: cert.issued_at.clone(),
            workspace_id: cert.workspace_id.clone(),
            is_usable: cert.is_usable(),
        }
    }
}

/// Result of listing certificates.
#[derive(Clone, Debug, Default)]
pub struct CertificateListReport {
    pub certificates: Vec<CertificateSummary>,
    pub total_count: u32,
    pub usable_count: u32,
    pub expired_count: u32,
    pub kinds_present: Vec<CertificateKind>,
}

impl CertificateListReport {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.certificates.is_empty()
    }

    #[must_use]
    pub fn schema() -> &'static str {
        CERTIFICATE_LIST_SCHEMA_V1
    }
}

/// Verification result for a certificate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerificationResult {
    /// Certificate verified successfully.
    Valid,
    /// Certificate payload hash mismatch.
    HashMismatch,
    /// Certificate payload hash points at stale source data.
    StalePayloadHash,
    /// Certificate was issued against an unsupported schema version.
    StaleSchemaVersion,
    /// Certificate assumptions no longer hold.
    FailedAssumptions,
    /// Certificate has expired.
    Expired,
    /// Certificate was revoked.
    Revoked,
    /// Certificate content-hash attestation did not verify.
    ///
    /// The recorded attestation hash did not match the digest derived from
    /// `(algorithm, signer, payload_hash)`. This is a structural integrity
    /// check on the attestation field; it is **not** a cryptographic
    /// signature verification (see module docs).
    AttestationMismatch,
    /// Certificate status is invalid.
    InvalidStatus,
    /// Certificate not found.
    NotFound,
}

impl VerificationResult {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::HashMismatch => "hash_mismatch",
            Self::StalePayloadHash => "stale_payload_hash",
            Self::StaleSchemaVersion => "stale_schema_version",
            Self::FailedAssumptions => "failed_assumptions",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
            Self::AttestationMismatch => "attestation_mismatch",
            Self::InvalidStatus => "invalid_status",
            Self::NotFound => "not_found",
        }
    }

    #[must_use]
    pub const fn is_valid(self) -> bool {
        matches!(self, Self::Valid)
    }

    #[must_use]
    pub const fn is_terminal_failure(self) -> bool {
        matches!(
            self,
            Self::HashMismatch
                | Self::StalePayloadHash
                | Self::StaleSchemaVersion
                | Self::FailedAssumptions
                | Self::Revoked
                | Self::AttestationMismatch
                | Self::NotFound
        )
    }
}

/// Report from verifying a certificate.
#[derive(Clone, Debug, PartialEq)]
pub struct CertificateVerifyReport {
    pub certificate_id: String,
    pub result: VerificationResult,
    pub checked_at: String,
    pub hash_verified: bool,
    pub payload_hash_fresh: bool,
    pub schema_version_valid: bool,
    pub assumptions_valid: bool,
    pub status_valid: bool,
    pub expiry_valid: bool,
    pub mismatches: Vec<String>,
    /// Whether the recorded content-hash attestation matched its
    /// publicly-derivable form. **Not a cryptographic signature check** —
    /// see module-level "Honesty note" for what this does and does not prove.
    pub attestation_ok: bool,
    pub signer: Option<String>,
    pub failure_codes: Vec<String>,
    pub message: String,
}

impl CertificateVerifyReport {
    #[must_use]
    pub fn valid(certificate_id: impl Into<String>) -> Self {
        Self {
            certificate_id: certificate_id.into(),
            result: VerificationResult::Valid,
            checked_at: chrono::Utc::now().to_rfc3339(),
            hash_verified: true,
            payload_hash_fresh: true,
            schema_version_valid: true,
            assumptions_valid: true,
            status_valid: true,
            expiry_valid: true,
            mismatches: Vec::new(),
            attestation_ok: true,
            signer: None,
            failure_codes: Vec::new(),
            message: "Certificate verification passed".to_owned(),
        }
    }

    #[must_use]
    pub fn not_found(certificate_id: impl Into<String>) -> Self {
        Self {
            certificate_id: certificate_id.into(),
            result: VerificationResult::NotFound,
            checked_at: chrono::Utc::now().to_rfc3339(),
            hash_verified: false,
            payload_hash_fresh: false,
            schema_version_valid: false,
            assumptions_valid: false,
            status_valid: false,
            expiry_valid: false,
            mismatches: vec!["not_found".to_owned()],
            attestation_ok: false,
            signer: None,
            failure_codes: vec!["not_found".to_owned()],
            message: "Certificate not found".to_owned(),
        }
    }

    #[must_use]
    pub fn expired(certificate_id: impl Into<String>) -> Self {
        Self {
            certificate_id: certificate_id.into(),
            result: VerificationResult::Expired,
            checked_at: chrono::Utc::now().to_rfc3339(),
            hash_verified: true,
            payload_hash_fresh: true,
            schema_version_valid: true,
            assumptions_valid: true,
            status_valid: false,
            expiry_valid: false,
            mismatches: vec!["expired".to_owned()],
            attestation_ok: true,
            signer: None,
            failure_codes: vec!["expired".to_owned()],
            message: "Certificate has expired".to_owned(),
        }
    }

    #[must_use]
    pub fn stale_payload_hash(certificate_id: impl Into<String>) -> Self {
        Self {
            certificate_id: certificate_id.into(),
            result: VerificationResult::StalePayloadHash,
            checked_at: chrono::Utc::now().to_rfc3339(),
            hash_verified: false,
            payload_hash_fresh: false,
            schema_version_valid: true,
            assumptions_valid: true,
            status_valid: true,
            expiry_valid: true,
            mismatches: vec!["stale_payload_hash".to_owned()],
            attestation_ok: true,
            signer: None,
            failure_codes: vec!["stale_payload_hash".to_owned()],
            message: "Certificate payload hash no longer matches the current payload".to_owned(),
        }
    }

    #[must_use]
    pub fn stale_schema_version(certificate_id: impl Into<String>) -> Self {
        Self {
            certificate_id: certificate_id.into(),
            result: VerificationResult::StaleSchemaVersion,
            checked_at: chrono::Utc::now().to_rfc3339(),
            hash_verified: true,
            payload_hash_fresh: true,
            schema_version_valid: false,
            assumptions_valid: true,
            status_valid: true,
            expiry_valid: true,
            mismatches: vec!["stale_schema_version".to_owned()],
            attestation_ok: true,
            signer: None,
            failure_codes: vec!["stale_schema_version".to_owned()],
            message: "Certificate schema version is no longer supported".to_owned(),
        }
    }

    #[must_use]
    pub fn failed_assumptions(certificate_id: impl Into<String>) -> Self {
        Self {
            certificate_id: certificate_id.into(),
            result: VerificationResult::FailedAssumptions,
            checked_at: chrono::Utc::now().to_rfc3339(),
            hash_verified: true,
            payload_hash_fresh: true,
            schema_version_valid: true,
            assumptions_valid: false,
            status_valid: true,
            expiry_valid: true,
            mismatches: vec!["failed_assumptions".to_owned()],
            attestation_ok: true,
            signer: None,
            failure_codes: vec!["failed_assumptions".to_owned()],
            message: "Certificate assumptions failed during verification".to_owned(),
        }
    }

    #[must_use]
    pub fn hash_mismatch(certificate_id: impl Into<String>) -> Self {
        Self {
            certificate_id: certificate_id.into(),
            result: VerificationResult::HashMismatch,
            checked_at: chrono::Utc::now().to_rfc3339(),
            hash_verified: false,
            payload_hash_fresh: false,
            schema_version_valid: true,
            assumptions_valid: true,
            status_valid: true,
            expiry_valid: true,
            mismatches: vec!["hash_mismatch".to_owned()],
            attestation_ok: true,
            signer: None,
            failure_codes: vec!["hash_mismatch".to_owned()],
            message: "Certificate payload hash does not match the manifest".to_owned(),
        }
    }

    #[must_use]
    pub fn revoked(certificate_id: impl Into<String>) -> Self {
        Self {
            certificate_id: certificate_id.into(),
            result: VerificationResult::Revoked,
            checked_at: chrono::Utc::now().to_rfc3339(),
            hash_verified: true,
            payload_hash_fresh: true,
            schema_version_valid: true,
            assumptions_valid: true,
            status_valid: false,
            expiry_valid: true,
            mismatches: vec!["revoked".to_owned()],
            attestation_ok: true,
            signer: None,
            failure_codes: vec!["revoked".to_owned()],
            message: "Certificate has been revoked".to_owned(),
        }
    }

    #[must_use]
    pub fn invalid_status(certificate_id: impl Into<String>) -> Self {
        Self {
            certificate_id: certificate_id.into(),
            result: VerificationResult::InvalidStatus,
            checked_at: chrono::Utc::now().to_rfc3339(),
            hash_verified: true,
            payload_hash_fresh: true,
            schema_version_valid: true,
            assumptions_valid: true,
            status_valid: false,
            expiry_valid: true,
            mismatches: vec!["invalid_status".to_owned()],
            attestation_ok: true,
            signer: None,
            failure_codes: vec!["invalid_status".to_owned()],
            message: "Certificate status is not valid for use".to_owned(),
        }
    }

    #[must_use]
    pub fn attestation_mismatch(
        certificate_id: impl Into<String>,
        signer: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            certificate_id: certificate_id.into(),
            result: VerificationResult::AttestationMismatch,
            checked_at: chrono::Utc::now().to_rfc3339(),
            hash_verified: true,
            payload_hash_fresh: true,
            schema_version_valid: true,
            assumptions_valid: true,
            status_valid: true,
            expiry_valid: true,
            mismatches: vec!["attestation_mismatch".to_owned()],
            attestation_ok: false,
            signer,
            failure_codes: vec!["attestation_mismatch".to_owned()],
            message: message.into(),
        }
    }

    #[must_use]
    pub fn invalid_manifest(certificate_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            certificate_id: certificate_id.into(),
            result: VerificationResult::StaleSchemaVersion,
            checked_at: chrono::Utc::now().to_rfc3339(),
            hash_verified: false,
            payload_hash_fresh: false,
            schema_version_valid: false,
            assumptions_valid: false,
            status_valid: false,
            expiry_valid: false,
            mismatches: vec!["invalid_manifest".to_owned()],
            attestation_ok: false,
            signer: None,
            failure_codes: vec!["invalid_manifest".to_owned()],
            message: message.into(),
        }
    }

    #[must_use]
    pub fn schema() -> &'static str {
        CERTIFICATE_VERIFY_SCHEMA_V1
    }

    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.result.is_valid()
    }
}

/// Show a single certificate with full details.
#[derive(Clone, Debug, PartialEq)]
pub struct CertificateShowReport {
    pub certificate: Certificate,
    pub verification_status: VerificationResult,
    pub payload_summary: String,
}

#[derive(Clone, Debug, PartialEq)]
struct ManifestCertificateRecord {
    certificate: Certificate,
    payload_path: Option<PathBuf>,
    payload_schema: Option<String>,
    assumptions_valid: bool,
    signature: Option<String>,
    signature_algorithm: Option<String>,
    signer: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawCertificateManifest {
    schema: Option<String>,
    #[serde(default)]
    certificates: Vec<RawCertificateRecord>,
}

#[derive(Debug, Deserialize)]
struct RawCertificateRecord {
    id: Option<String>,
    kind: Option<String>,
    status: Option<String>,
    #[serde(default, alias = "workspaceId")]
    workspace_id: Option<String>,
    #[serde(default, alias = "issuedAt")]
    issued_at: Option<String>,
    #[serde(default, alias = "expiresAt")]
    expires_at: Option<String>,
    #[serde(default, alias = "payloadHash")]
    payload_hash: Option<String>,
    #[serde(default, alias = "payloadPath")]
    payload_path: Option<String>,
    #[serde(default, alias = "payloadSchema")]
    payload_schema: Option<String>,
    #[serde(default, alias = "failedAssumptions")]
    failed_assumptions: bool,
    #[serde(default)]
    signature: Option<String>,
    #[serde(default, alias = "signatureAlgorithm")]
    signature_algorithm: Option<String>,
    #[serde(default)]
    signer: Option<String>,
    #[serde(default)]
    assumptions: Vec<RawCertificateAssumption>,
}

#[derive(Debug, Deserialize)]
struct RawCertificateAssumption {
    #[serde(default)]
    valid: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertificateManifestError {
    pub message: String,
}

impl CertificateManifestError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CertificateManifestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CertificateManifestError {}

impl CertificateShowReport {
    #[must_use]
    pub fn new(certificate: Certificate) -> Self {
        let verification_status = if certificate.is_usable() {
            VerificationResult::Valid
        } else if certificate.is_expired() {
            VerificationResult::Expired
        } else {
            VerificationResult::InvalidStatus
        };

        let payload_summary = format!(
            "{} certificate for workspace {}",
            certificate.kind.as_str(),
            certificate.workspace_id
        );

        Self {
            certificate,
            verification_status,
            payload_summary,
        }
    }

    #[must_use]
    pub fn not_found(id: impl Into<String>) -> Self {
        let placeholder = Certificate {
            id: id.into(),
            kind: CertificateKind::Pack,
            status: CertificateStatus::Invalid,
            workspace_id: String::new(),
            issued_at: String::new(),
            expires_at: None,
            payload_hash: String::new(),
            decision_metadata: DecisionPlaneMetadata::empty(),
        };
        Self {
            certificate: placeholder,
            verification_status: VerificationResult::NotFound,
            payload_summary: "Certificate not found".to_owned(),
        }
    }

    #[must_use]
    pub fn schema() -> &'static str {
        CERTIFICATE_SHOW_SCHEMA_V1
    }
}

/// List certificates with optional filters.
#[must_use]
pub fn list_certificates(options: &CertificateListOptions) -> CertificateListReport {
    let Some(manifest_path) = options.manifest_path.as_deref() else {
        return list_database_certificates(options);
    };

    let Ok(records) = read_certificate_manifest(manifest_path) else {
        return CertificateListReport::new();
    };

    let total_count = usize_to_u32(records.len());
    let usable_count = usize_to_u32(
        records
            .iter()
            .filter(|record| record.certificate.is_usable())
            .count(),
    );
    let expired_count = usize_to_u32(
        records
            .iter()
            .filter(|record| record.certificate.is_expired())
            .count(),
    );

    let mut kinds_present = Vec::new();
    for record in &records {
        if !kinds_present.contains(&record.certificate.kind) {
            kinds_present.push(record.certificate.kind);
        }
    }
    kinds_present.sort_by_key(|kind| kind.as_str());

    let mut certificates: Vec<CertificateSummary> = records
        .iter()
        .filter(|record| {
            options
                .kind
                .is_none_or(|kind| record.certificate.kind == kind)
        })
        .filter(|record| {
            options
                .status
                .is_none_or(|status| record.certificate.status == status)
        })
        .filter(|record| options.include_expired || !record.certificate.is_expired())
        .map(|record| CertificateSummary::from(&record.certificate))
        .collect();
    certificates.sort_by(|left, right| left.id.cmp(&right.id));
    if let Some(limit) = options.limit {
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        certificates.truncate(limit);
    }

    CertificateListReport {
        certificates,
        total_count,
        usable_count,
        expired_count,
        kinds_present,
    }
}

/// Show a certificate by ID.
#[must_use]
pub fn show_certificate(certificate_id: &str) -> CertificateShowReport {
    CertificateShowReport::not_found(certificate_id)
}

/// Show a certificate by ID from an explicit manifest path.
#[must_use]
pub fn show_certificate_with_options(options: &CertificateLookupOptions) -> CertificateShowReport {
    let Some(manifest_path) = options.manifest_path.as_deref() else {
        return show_database_certificate(options);
    };

    let Ok(records) = read_certificate_manifest(manifest_path) else {
        return CertificateShowReport::not_found(&options.certificate_id);
    };

    records
        .into_iter()
        .find(|record| record.certificate.id == options.certificate_id)
        .map_or_else(
            || CertificateShowReport::not_found(&options.certificate_id),
            |record| CertificateShowReport::new(record.certificate),
        )
}

/// Verify a certificate by ID.
#[must_use]
pub fn verify_certificate(certificate_id: &str) -> CertificateVerifyReport {
    CertificateVerifyReport::not_found(certificate_id)
}

/// Verify a certificate by ID from an explicit manifest path.
#[must_use]
pub fn verify_certificate_with_options(
    options: &CertificateLookupOptions,
) -> CertificateVerifyReport {
    let Some(manifest_path) = options.manifest_path.as_deref() else {
        return verify_database_certificate(options);
    };

    let manifest_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let records = match read_certificate_manifest(manifest_path) {
        Ok(records) => records,
        Err(error) => {
            return CertificateVerifyReport::invalid_manifest(
                &options.certificate_id,
                error.message,
            );
        }
    };

    let Some(record) = records
        .iter()
        .find(|record| record.certificate.id == options.certificate_id)
    else {
        return CertificateVerifyReport::not_found(&options.certificate_id);
    };

    verify_manifest_certificate(record, manifest_dir)
}

fn list_database_certificates(options: &CertificateListOptions) -> CertificateListReport {
    let (Some(database_path), Some(workspace_id)) = (
        options.database_path.as_deref(),
        options.workspace_id.as_deref(),
    ) else {
        return CertificateListReport::new();
    };
    if !database_path.exists() {
        return CertificateListReport::new();
    }

    let Ok(connection) = DbConnection::open_file(database_path) else {
        return CertificateListReport::new();
    };
    let Ok(records) =
        connection.list_certificates_for_workspace(workspace_id, None, None, u32::MAX)
    else {
        return CertificateListReport::new();
    };

    let total_count = usize_to_u32(records.len());
    let usable_count = usize_to_u32(
        records
            .iter()
            .map(certificate_from_stored_record)
            .filter(Certificate::is_usable)
            .count(),
    );
    let expired_count = usize_to_u32(
        records
            .iter()
            .map(certificate_from_stored_record)
            .filter(Certificate::is_expired)
            .count(),
    );

    let mut kinds_present = Vec::new();
    for record in &records {
        let kind = certificate_kind_from_target_kind(&record.target_kind);
        if !kinds_present.contains(&kind) {
            kinds_present.push(kind);
        }
    }
    kinds_present.sort_by_key(|kind| kind.as_str());

    let mut certificates: Vec<CertificateSummary> = records
        .iter()
        .map(certificate_from_stored_record)
        .filter(|certificate| options.kind.is_none_or(|kind| certificate.kind == kind))
        .filter(|certificate| {
            options
                .status
                .is_none_or(|status| certificate.status == status)
        })
        .filter(|certificate| options.include_expired || !certificate.is_expired())
        .map(|certificate| CertificateSummary::from(&certificate))
        .collect();
    certificates.sort_by(|left, right| left.id.cmp(&right.id));
    if let Some(limit) = options.limit {
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        certificates.truncate(limit);
    }

    CertificateListReport {
        certificates,
        total_count,
        usable_count,
        expired_count,
        kinds_present,
    }
}

fn show_database_certificate(options: &CertificateLookupOptions) -> CertificateShowReport {
    load_database_certificate(options).map_or_else(
        || CertificateShowReport::not_found(&options.certificate_id),
        |record| certificate_show_report_from_record(&record),
    )
}

fn verify_database_certificate(options: &CertificateLookupOptions) -> CertificateVerifyReport {
    let Some(record) = load_database_certificate(options) else {
        return CertificateVerifyReport::not_found(&options.certificate_id);
    };

    let metadata = stored_certificate_metadata(&record);
    if metadata.payload_schema.as_deref() != Some(CERTIFICATE_PAYLOAD_SCHEMA_V1) {
        return CertificateVerifyReport::stale_schema_version(&record.id);
    }

    match certificate_status_from_record(&record) {
        CertificateStatus::Revoked => return CertificateVerifyReport::revoked(&record.id),
        CertificateStatus::Expired => return CertificateVerifyReport::expired(&record.id),
        CertificateStatus::Valid => {}
        CertificateStatus::Pending | CertificateStatus::Invalid => {
            return CertificateVerifyReport::invalid_status(&record.id);
        }
    }

    if !expiry_valid(metadata.expires_at.as_deref()) {
        return CertificateVerifyReport::expired(&record.id);
    }

    if !metadata.assumptions_valid {
        return CertificateVerifyReport::failed_assumptions(&record.id);
    }

    match database_payload_hash_matches(&record) {
        Ok(true) => {}
        Ok(false) | Err(_) => return CertificateVerifyReport::hash_mismatch(&record.id),
    }

    match verify_local_content_attestation(
        record.signature.as_deref(),
        record.signature_algorithm.as_deref(),
        record.signer.as_deref(),
        &record.content_hash,
    ) {
        AttestationVerification::Ok { signer } => {
            let mut report = CertificateVerifyReport::valid(&record.id);
            report.attestation_ok = true;
            report.signer = signer;
            report
        }
        AttestationVerification::Mismatch { signer, message } => {
            CertificateVerifyReport::attestation_mismatch(&record.id, signer, message)
        }
    }
}

fn load_database_certificate(
    options: &CertificateLookupOptions,
) -> Option<StoredCertificateRecord> {
    let (Some(database_path), Some(workspace_id)) = (
        options.database_path.as_deref(),
        options.workspace_id.as_deref(),
    ) else {
        return None;
    };
    if !database_path.exists() {
        return None;
    }

    let connection = DbConnection::open_file(database_path).ok()?;
    let record = connection.get_certificate(&options.certificate_id).ok()??;
    (record.workspace_id == workspace_id).then_some(record)
}

fn certificate_show_report_from_record(record: &StoredCertificateRecord) -> CertificateShowReport {
    let certificate = certificate_from_stored_record(record);
    let verification_status = if certificate.is_usable() {
        VerificationResult::Valid
    } else if certificate.is_expired() {
        VerificationResult::Expired
    } else {
        VerificationResult::InvalidStatus
    };
    let payload_summary = format!(
        "{} certificate for {} `{}` in workspace {}",
        certificate.kind.as_str(),
        record.target_kind,
        record.target_id,
        record.workspace_id
    );

    CertificateShowReport {
        certificate,
        verification_status,
        payload_summary,
    }
}

fn certificate_from_stored_record(record: &StoredCertificateRecord) -> Certificate {
    let metadata = stored_certificate_metadata(record);
    Certificate {
        id: record.id.clone(),
        kind: certificate_kind_from_target_kind(&record.target_kind),
        status: certificate_status_from_record(record),
        workspace_id: record.workspace_id.clone(),
        issued_at: record
            .signed_at
            .clone()
            .unwrap_or_else(|| record.created_at.clone()),
        expires_at: metadata.expires_at,
        payload_hash: record.content_hash.clone(),
        decision_metadata: DecisionPlaneMetadata::empty(),
    }
}

fn certificate_kind_from_target_kind(target_kind: &str) -> CertificateKind {
    match target_kind {
        "pack" => CertificateKind::Pack,
        "curation" => CertificateKind::Curation,
        "tail_risk" => CertificateKind::TailRisk,
        "privacy_budget" => CertificateKind::PrivacyBudget,
        "backup" | "manifest" | "export" | "lifecycle" => CertificateKind::Lifecycle,
        _ => CertificateKind::Lifecycle,
    }
}

fn certificate_status_from_record(record: &StoredCertificateRecord) -> CertificateStatus {
    record
        .status
        .parse::<CertificateStatus>()
        .unwrap_or(CertificateStatus::Invalid)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StoredCertificateMetadata {
    expires_at: Option<String>,
    payload_schema: Option<String>,
    assumptions_valid: bool,
}

fn stored_certificate_metadata(record: &StoredCertificateRecord) -> StoredCertificateMetadata {
    let mut metadata = StoredCertificateMetadata {
        expires_at: None,
        payload_schema: Some(CERTIFICATE_PAYLOAD_SCHEMA_V1.to_owned()),
        assumptions_valid: true,
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&record.metadata_json) else {
        return metadata;
    };

    metadata.expires_at =
        json_string_field(&value, "expiresAt").or_else(|| json_string_field(&value, "expires_at"));
    metadata.payload_schema = json_string_field(&value, "payloadSchema")
        .or_else(|| json_string_field(&value, "payload_schema"))
        .or(metadata.payload_schema);
    if let Some(assumptions_valid) = json_bool_field(&value, "assumptionsValid")
        .or_else(|| json_bool_field(&value, "assumptions_valid"))
    {
        metadata.assumptions_valid = assumptions_valid;
    }
    if json_bool_field(&value, "failedAssumptions")
        .or_else(|| json_bool_field(&value, "failed_assumptions"))
        .unwrap_or(false)
    {
        metadata.assumptions_valid = false;
    }

    metadata
}

fn json_string_field(value: &serde_json::Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn json_bool_field(value: &serde_json::Value, field: &str) -> Option<bool> {
    value.get(field).and_then(serde_json::Value::as_bool)
}

fn read_certificate_manifest(
    manifest_path: &Path,
) -> Result<Vec<ManifestCertificateRecord>, CertificateManifestError> {
    reject_certificate_manifest_symlink_chain(manifest_path).map_err(|error| {
        CertificateManifestError::new(format!(
            "failed to inspect certificate manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    let metadata = fs::symlink_metadata(manifest_path).map_err(|error| {
        CertificateManifestError::new(format!(
            "failed to inspect certificate manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CertificateManifestError::new(format!(
            "certificate manifest {} is not a regular file",
            manifest_path.display()
        )));
    }
    let input = fs::read_to_string(manifest_path).map_err(|error| {
        CertificateManifestError::new(format!(
            "failed to read certificate manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    let raw: RawCertificateManifest = serde_json::from_str(&input).map_err(|error| {
        CertificateManifestError::new(format!(
            "failed to parse certificate manifest {}: {error}",
            manifest_path.display()
        ))
    })?;

    if raw.schema.as_deref() != Some(CERTIFICATE_MANIFEST_SCHEMA_V1) {
        return Err(CertificateManifestError::new(format!(
            "unsupported certificate manifest schema; expected `{CERTIFICATE_MANIFEST_SCHEMA_V1}`"
        )));
    }

    let mut records = Vec::with_capacity(raw.certificates.len());
    for (index, raw_record) in raw.certificates.into_iter().enumerate() {
        records.push(convert_raw_certificate(index, raw_record)?);
    }
    records.sort_by(|left, right| left.certificate.id.cmp(&right.certificate.id));
    Ok(records)
}

fn reject_certificate_manifest_symlink_chain(path: &Path) -> io::Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
            Component::Normal(component) => {
                current.push(component);
                match fs::symlink_metadata(&current) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!(
                                "certificate manifest symlink refused: {}",
                                current.display()
                            ),
                        ));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                    Err(error) => return Err(error),
                }
            }
            Component::CurDir | Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsafe certificate manifest path",
                ));
            }
        }
    }
    if current.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty certificate manifest path",
        ));
    }
    Ok(())
}

fn convert_raw_certificate(
    index: usize,
    raw: RawCertificateRecord,
) -> Result<ManifestCertificateRecord, CertificateManifestError> {
    let id = required_certificate_field(raw.id, "id", index)?;
    let raw_kind = required_certificate_field(raw.kind, "kind", index)?;
    let kind = raw_kind.parse::<CertificateKind>().map_err(|error| {
        CertificateManifestError::new(format!(
            "invalid certificate kind `{raw_kind}` at certificates[{index}]: {error}"
        ))
    })?;
    let raw_status = required_certificate_field(raw.status, "status", index)?;
    let status = raw_status.parse::<CertificateStatus>().map_err(|error| {
        CertificateManifestError::new(format!(
            "invalid certificate status `{raw_status}` at certificates[{index}]: {error}"
        ))
    })?;

    let workspace_id = required_certificate_field(raw.workspace_id, "workspaceId", index)?;
    let issued_at = required_certificate_field(raw.issued_at, "issuedAt", index)?;
    let payload_hash = required_certificate_field(raw.payload_hash, "payloadHash", index)?;
    let assumptions_valid = !raw.failed_assumptions
        && raw
            .assumptions
            .iter()
            .all(|assumption| assumption.valid.unwrap_or(true));

    Ok(ManifestCertificateRecord {
        certificate: Certificate {
            id,
            kind,
            status,
            workspace_id,
            issued_at,
            expires_at: raw.expires_at,
            payload_hash,
            decision_metadata: DecisionPlaneMetadata::empty(),
        },
        payload_path: safe_manifest_payload_path(raw.payload_path, index)?,
        payload_schema: raw.payload_schema,
        assumptions_valid,
        signature: normalize_optional_string(raw.signature),
        signature_algorithm: normalize_optional_string(raw.signature_algorithm),
        signer: normalize_optional_string(raw.signer),
    })
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn safe_manifest_payload_path(
    value: Option<String>,
    index: usize,
) -> Result<Option<PathBuf>, CertificateManifestError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(CertificateManifestError::new(format!(
            "unsafe certificate payloadPath at certificates[{index}]"
        )));
    }
    Ok(Some(path))
}

fn required_certificate_field(
    value: Option<String>,
    field_name: &str,
    index: usize,
) -> Result<String, CertificateManifestError> {
    match value {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(CertificateManifestError::new(format!(
            "missing certificate {field_name} at certificates[{index}]"
        ))),
    }
}

fn verify_manifest_certificate(
    record: &ManifestCertificateRecord,
    manifest_dir: &Path,
) -> CertificateVerifyReport {
    if record.payload_schema.as_deref() != Some(CERTIFICATE_PAYLOAD_SCHEMA_V1) {
        return CertificateVerifyReport::stale_schema_version(&record.certificate.id);
    }

    match record.certificate.status {
        CertificateStatus::Revoked => {
            return CertificateVerifyReport::revoked(&record.certificate.id);
        }
        CertificateStatus::Expired => {
            return CertificateVerifyReport::expired(&record.certificate.id);
        }
        CertificateStatus::Valid => {}
        CertificateStatus::Pending | CertificateStatus::Invalid => {
            return CertificateVerifyReport::invalid_status(&record.certificate.id);
        }
    }

    if !expiry_valid(record.certificate.expires_at.as_deref()) {
        return CertificateVerifyReport::expired(&record.certificate.id);
    }

    if !record.assumptions_valid {
        return CertificateVerifyReport::failed_assumptions(&record.certificate.id);
    }

    match payload_hash_matches(record, manifest_dir) {
        Ok(true) => {}
        Ok(false) | Err(_) => {
            return CertificateVerifyReport::hash_mismatch(&record.certificate.id);
        }
    }

    match verify_manifest_certificate_attestation(record) {
        AttestationVerification::Ok { signer } => {
            let mut report = CertificateVerifyReport::valid(&record.certificate.id);
            report.attestation_ok = true;
            report.signer = signer;
            report
        }
        AttestationVerification::Mismatch { signer, message } => {
            CertificateVerifyReport::attestation_mismatch(&record.certificate.id, signer, message)
        }
    }
}

enum AttestationVerification {
    Ok {
        signer: Option<String>,
    },
    Mismatch {
        signer: Option<String>,
        message: String,
    },
}

fn verify_manifest_certificate_attestation(
    record: &ManifestCertificateRecord,
) -> AttestationVerification {
    verify_local_content_attestation(
        record.signature.as_deref(),
        record.signature_algorithm.as_deref(),
        record.signer.as_deref(),
        &record.certificate.payload_hash,
    )
}

/// Verify a certificate's content-hash attestation.
///
/// This is **not** cryptographic signature verification. The attestation is a
/// domain-separated SHA-256 of `(algorithm-tag, signer, payload_hash)` — all
/// public inputs. A passing check proves only that the recorded attestation
/// hash matches its publicly-derivable form; it does **not** prove an
/// authorized key holder produced the certificate. See module docs.
///
/// Supported algorithms:
/// - `ee.local-content-hash.v1`: Content-hash attestation (NOT cryptographic)
/// - `ee.ed25519.v1`: Real ed25519 signature verification
///
/// The historically-misleading `sigstore.bundle-sha256.v1` is rejected.
fn verify_local_content_attestation(
    attestation: Option<&str>,
    attestation_algorithm: Option<&str>,
    signer: Option<&str>,
    payload_hash: &str,
) -> AttestationVerification {
    let signer = signer.map(str::to_owned);
    let Some(attestation) = attestation else {
        return AttestationVerification::Ok { signer: None };
    };
    let Some(algorithm) = attestation_algorithm else {
        return AttestationVerification::Mismatch {
            signer,
            message: "Certificate attestation is missing signatureAlgorithm".to_owned(),
        };
    };
    let Some(signer_value) = signer.as_deref() else {
        return AttestationVerification::Mismatch {
            signer,
            message: "Certificate attestation is missing signer".to_owned(),
        };
    };

    match algorithm {
        "ee.local-content-hash.v1" => {
            let expected = local_content_hash_attestation(signer_value, payload_hash);
            if constant_time_str_eq(attestation, &expected) {
                AttestationVerification::Ok { signer }
            } else {
                AttestationVerification::Mismatch {
                    signer,
                    message: "Certificate content-hash attestation does not match payload evidence"
                        .to_owned(),
                }
            }
        }
        "ee.ed25519.v1" => {
            // Real cryptographic signature verification
            verify_ed25519_signature(attestation, signer_value, payload_hash, None)
        }
        "sigstore.bundle-sha256.v1" => {
            AttestationVerification::Mismatch {
                signer,
                message:
                    "Certificate attestation algorithm `sigstore.bundle-sha256.v1` is no longer accepted: \
                     this slice does not perform sigstore bundle verification (rekor inclusion proof, \
                     fulcio cert chain, OIDC binding). Use `ee.ed25519.v1` for real signing."
                        .to_owned(),
            }
        }
        other => {
            AttestationVerification::Mismatch {
                signer,
                message: format!("Unsupported certificate attestation algorithm `{other}`"),
            }
        }
    }
}

/// Derive the `ee.local-content-hash.v1` attestation string for a record.
///
/// The output is `sha256:<hex>` over a domain-separated digest of
/// `(algorithm-tag, signer, payload_hash)`. All inputs are public and
/// stored on the certificate record itself, so this is a content-addressed
/// integrity tag, **not** a cryptographic signature. Anyone who can read
/// `(signer, payload_hash)` can recompute this string.
fn local_content_hash_attestation(signer: &str, payload_hash: &str) -> String {
    format!(
        "sha256:{}",
        attestation_digest("ee.certificate.local-content-hash.v1", signer, payload_hash)
    )
}

fn attestation_digest(domain: &str, signer: &str, payload_hash: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update(b"\n");
    hasher.update(signer.as_bytes());
    hasher.update(b"\n");
    hasher.update(payload_hash.as_bytes());
    hex_lower(&hasher.finalize())
}

fn constant_time_str_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let max_len = left.len().max(right.len());
    let mut diff = left.len() ^ right.len();
    for index in 0..max_len {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(left_byte ^ right_byte);
    }
    diff == 0
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn expiry_valid(expires_at: Option<&str>) -> bool {
    let Some(expires_at) = expires_at else {
        return true;
    };
    chrono::DateTime::parse_from_rfc3339(expires_at)
        .map(|expires_at| expires_at > chrono::Utc::now())
        .unwrap_or(false)
}

fn payload_hash_matches(
    record: &ManifestCertificateRecord,
    manifest_dir: &Path,
) -> Result<bool, std::io::Error> {
    let Some(payload_path) = record.payload_path.as_deref() else {
        return Ok(false);
    };
    let payload = read_manifest_payload(manifest_dir, payload_path)?;
    let actual = blake3::hash(&payload).to_hex().to_string();
    Ok(actual == record.certificate.payload_hash)
}

fn database_payload_hash_matches(record: &StoredCertificateRecord) -> Result<bool, io::Error> {
    let Some(payload_path) = record.payload_path.as_deref() else {
        return Ok(false);
    };
    let payload_path = resolve_database_payload_path_no_symlinks(record, Path::new(payload_path))?;
    ensure_certificate_payload_regular_file(&payload_path)?;
    let payload = fs::read(payload_path)?;
    let actual = match record.hash_algo.as_str() {
        "blake3" => format!("blake3:{}", blake3::hash(&payload).to_hex()),
        "sha256" => {
            let mut hasher = Sha256::new();
            hasher.update(&payload);
            format!("sha256:{}", hex_lower(&hasher.finalize()))
        }
        _ => return Ok(false),
    };
    Ok(actual == record.content_hash)
}

fn read_manifest_payload(manifest_dir: &Path, payload_path: &Path) -> io::Result<Vec<u8>> {
    let payload_path = resolve_manifest_payload_path_no_symlinks(manifest_dir, payload_path)?;
    ensure_certificate_payload_regular_file(&payload_path)?;
    fs::read(payload_path)
}

fn ensure_certificate_payload_regular_file(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "certificate payload path is not a regular file: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn resolve_database_payload_path_no_symlinks(
    record: &StoredCertificateRecord,
    payload_path: &Path,
) -> io::Result<PathBuf> {
    if payload_path.as_os_str().is_empty()
        || payload_path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsafe certificate payload_path",
        ));
    }

    let resolved = if payload_path.is_absolute() {
        payload_path.to_path_buf()
    } else if let Some(base) = database_certificate_payload_base(record) {
        base.join(payload_path)
    } else {
        payload_path.to_path_buf()
    };
    reject_payload_symlink_chain(&resolved)?;
    Ok(resolved)
}

fn database_certificate_payload_base(record: &StoredCertificateRecord) -> Option<PathBuf> {
    let manifest_path = record.manifest_path.as_deref()?;
    Path::new(manifest_path).parent().map(Path::to_path_buf)
}

fn resolve_manifest_payload_path_no_symlinks(
    manifest_dir: &Path,
    payload_path: &Path,
) -> io::Result<PathBuf> {
    reject_payload_symlink_component(manifest_dir)?;
    let mut resolved = manifest_dir.to_path_buf();
    for component in payload_path.components() {
        let Component::Normal(component) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsafe certificate payloadPath",
            ));
        };
        resolved.push(component);
        reject_payload_symlink_component(&resolved)?;
    }
    Ok(resolved)
}

fn reject_payload_symlink_chain(path: &Path) -> io::Result<()> {
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
            Component::RootDir => resolved.push(component.as_os_str()),
            Component::Normal(component) => {
                resolved.push(component);
                reject_payload_symlink_component(&resolved)?;
            }
            Component::CurDir | Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsafe certificate payload path",
                ));
            }
        }
    }
    if resolved.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty certificate payload path",
        ));
    }
    Ok(())
}

fn reject_payload_symlink_component(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("certificate payload symlink refused: {}", path.display()),
        ))
    } else {
        Ok(())
    }
}

fn usize_to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

// =============================================================================
// Ed25519 Signing Implementation
// =============================================================================

/// Options for key generation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct KeygenOptions {
    /// Workspace path for key naming.
    pub workspace_path: Option<PathBuf>,
    /// Force overwrite existing key.
    pub force: bool,
    /// Show public key fingerprint only, do not generate.
    pub show_only: bool,
}

/// Report from key generation.
#[derive(Clone, Debug, PartialEq)]
pub struct KeygenReport {
    pub key_path: PathBuf,
    pub fingerprint: String,
    pub signer: String,
    pub created: bool,
    pub message: String,
}

impl KeygenReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": CERTIFICATE_KEYGEN_SCHEMA_V1,
            "keyPath": self.key_path.display().to_string(),
            "fingerprint": self.fingerprint,
            "signer": self.signer,
            "created": self.created,
            "message": self.message,
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        if self.created {
            format!(
                "Generated ed25519 keypair:\n  Key: {}\n  Signer: {}",
                self.key_path.display(),
                self.signer
            )
        } else {
            format!(
                "Existing ed25519 keypair:\n  Key: {}\n  Signer: {}",
                self.key_path.display(),
                self.signer
            )
        }
    }
}

/// Options for signing a certificate.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SignOptions {
    /// Certificate ID to sign.
    pub certificate_id: String,
    /// Path to certificate manifest JSON.
    pub manifest_path: Option<PathBuf>,
    /// Path to ed25519 private key file.
    pub key_path: Option<PathBuf>,
    /// Workspace path for resolving default key location.
    pub workspace_path: Option<PathBuf>,
}

/// Report from signing a certificate.
#[derive(Clone, Debug, PartialEq)]
pub struct SignReport {
    pub certificate_id: String,
    pub signature: String,
    pub algorithm: String,
    pub signer: String,
    pub payload_hash: String,
    pub signed_at: String,
    pub success: bool,
    pub message: String,
}

impl SignReport {
    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": CERTIFICATE_SIGN_SCHEMA_V1,
            "certificateId": self.certificate_id,
            "signature": self.signature,
            "algorithm": self.algorithm,
            "signer": self.signer,
            "payloadHash": self.payload_hash,
            "signedAt": self.signed_at,
            "success": self.success,
            "message": self.message,
        })
    }

    #[must_use]
    pub fn human_summary(&self) -> String {
        if self.success {
            format!(
                "Signed certificate {}:\n  Algorithm: {}\n  Signer: {}\n  Signature: {}...",
                self.certificate_id,
                self.algorithm,
                self.signer,
                &self.signature[..self.signature.len().min(32)]
            )
        } else {
            format!(
                "Failed to sign certificate {}: {}",
                self.certificate_id, self.message
            )
        }
    }

    fn error(certificate_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            certificate_id: certificate_id.into(),
            signature: String::new(),
            algorithm: String::new(),
            signer: String::new(),
            payload_hash: String::new(),
            signed_at: chrono::Utc::now().to_rfc3339(),
            success: false,
            message: message.into(),
        }
    }
}

/// Get the default key directory path.
fn default_key_dir() -> io::Result<PathBuf> {
    let config_dir = dirs_config_dir()?;
    Ok(config_dir.join(KEY_DIR_NAME))
}

/// Get the platform-specific config directory (~/.config on Linux).
fn dirs_config_dir() -> io::Result<PathBuf> {
    std::env::var("HOME")
        .map(|home| PathBuf::from(home).join(".config"))
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "HOME environment variable not set"))
}

/// Resolve the key path for a workspace.
fn resolve_key_path(
    workspace_path: Option<&Path>,
    explicit_key_path: Option<&Path>,
) -> io::Result<PathBuf> {
    if let Some(key_path) = explicit_key_path {
        return Ok(key_path.to_owned());
    }
    let key_dir = default_key_dir()?;
    let workspace_name = workspace_path
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("default");
    Ok(key_dir.join(format!("{workspace_name}.ed25519")))
}

fn key_path_exists_no_symlinks(path: &Path) -> io::Result<bool> {
    reject_key_symlink_chain(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("certificate key symlink refused: {}", path.display()),
        )),
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("certificate key path is not a file: {}", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn key_dir_exists_no_symlinks(path: &Path) -> io::Result<bool> {
    reject_key_symlink_chain(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "certificate key directory symlink refused: {}",
                path.display()
            ),
        )),
        Ok(metadata) if metadata.file_type().is_dir() => Ok(true),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "certificate key directory is not a directory: {}",
                path.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn read_key_file_no_symlinks(path: &Path) -> io::Result<Vec<u8>> {
    reject_key_symlink_chain(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("certificate key symlink refused: {}", path.display()),
        ));
    }
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("certificate key path is not a file: {}", path.display()),
        ));
    }
    fs::read(path)
}

fn write_key_file_no_symlinks(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        reject_key_symlink_chain(parent)?;
        fs::create_dir_all(parent)?;
        reject_key_symlink_chain(parent)?;
    }
    let temp_path = key_temp_path(path)?;
    reject_key_symlink_chain(path)?;
    ensure_key_final_path_writable(path)?;
    reject_key_symlink_chain(&temp_path)?;
    ensure_key_temp_path_absent(&temp_path)?;

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);

    publish_key_temp_file(&temp_path, path)?;
    reject_key_symlink_chain(path)?;
    ensure_key_final_path_writable(path)
}

fn publish_key_temp_file(temp_path: &Path, path: &Path) -> io::Result<()> {
    reject_key_symlink_chain(path)?;
    ensure_key_final_path_writable(path)?;
    reject_key_symlink_chain(temp_path)?;
    ensure_key_created_temp_path_is_regular(temp_path)?;
    fs::rename(temp_path, path)?;
    reject_key_symlink_chain(path)?;
    ensure_key_final_path_writable(path)
}

fn ensure_key_final_path_writable(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("certificate key path is not a file: {}", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn ensure_key_temp_path_absent(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("certificate key temp symlink refused: {}", path.display()),
        )),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "certificate key temp path already exists: {}",
                path.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn ensure_key_created_temp_path_is_regular(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "certificate key temp path is not a file: {}",
                path.display()
            ),
        )),
        Err(error) => Err(error),
    }
}

fn key_temp_path(path: &Path) -> io::Result<PathBuf> {
    let Some(file_name) = path.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "certificate key path has no file name",
        ));
    };
    let mut temp_name = file_name.to_os_string();
    temp_name.push(".tmp");
    Ok(path.with_file_name(temp_name))
}

fn reject_key_symlink_chain(path: &Path) -> io::Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
            Component::Normal(component) => {
                current.push(component);
                match fs::symlink_metadata(&current) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("certificate key symlink refused: {}", current.display()),
                        ));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            Component::CurDir | Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsafe certificate key path",
                ));
            }
        }
    }
    if current.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty certificate key path",
        ));
    }
    Ok(())
}

/// Derive the signer string from a public key.
fn signer_from_public_key(public_key: &[u8]) -> String {
    let fingerprint = blake3::hash(public_key).to_hex();
    format!("ed25519:fp:{fingerprint}")
}

/// Generate or load a keypair.
pub fn keygen(options: &KeygenOptions) -> io::Result<KeygenReport> {
    let key_path = resolve_key_path(options.workspace_path.as_deref(), None)?;

    if key_path_exists_no_symlinks(&key_path)? {
        if options.show_only {
            // Load existing key and show fingerprint
            let key_data = read_key_file_no_symlinks(&key_path)?;
            let keypair = Ed25519KeyPair::from_pkcs8(&key_data).map_err(|err| {
                io::Error::new(io::ErrorKind::InvalidData, format!("invalid key: {err}"))
            })?;
            let public_key = keypair.public_key().as_ref();
            let fingerprint = blake3::hash(public_key).to_hex().to_string();
            let signer = signer_from_public_key(public_key);
            return Ok(KeygenReport {
                key_path,
                fingerprint,
                signer,
                created: false,
                message: "Existing keypair loaded".to_owned(),
            });
        }
        if !options.force {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "key already exists at {}, use --force to overwrite",
                    key_path.display()
                ),
            ));
        }
    }

    if options.show_only {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no key found at {}", key_path.display()),
        ));
    }

    // Generate new keypair
    let rng = SystemRandom::new();
    let pkcs8_bytes = Ed25519KeyPair::generate_pkcs8(&rng)
        .map_err(|err| io::Error::other(format!("key generation failed: {err}")))?;

    // Write key with restricted permissions
    write_key_file_no_symlinks(&key_path, pkcs8_bytes.as_ref())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))?;
    }

    let keypair = Ed25519KeyPair::from_pkcs8(pkcs8_bytes.as_ref()).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid generated key: {err}"),
        )
    })?;
    let public_key = keypair.public_key().as_ref();
    let fingerprint = blake3::hash(public_key).to_hex().to_string();
    let signer = signer_from_public_key(public_key);

    Ok(KeygenReport {
        key_path,
        fingerprint,
        signer,
        created: true,
        message: "New keypair generated".to_owned(),
    })
}

/// Sign a certificate with ed25519.
pub fn sign_certificate(options: &SignOptions) -> SignReport {
    let key_path = match resolve_key_path(
        options.workspace_path.as_deref(),
        options.key_path.as_deref(),
    ) {
        Ok(path) => path,
        Err(err) => {
            return SignReport::error(
                &options.certificate_id,
                format!("failed to resolve key path: {err}"),
            );
        }
    };

    // Load the keypair
    let key_data = match read_key_file_no_symlinks(&key_path) {
        Ok(data) => data,
        Err(err) => {
            return SignReport::error(
                &options.certificate_id,
                format!("failed to read key at {}: {err}", key_path.display()),
            );
        }
    };

    let keypair = match Ed25519KeyPair::from_pkcs8(&key_data) {
        Ok(kp) => kp,
        Err(err) => {
            return SignReport::error(
                &options.certificate_id,
                format!("invalid key format: {err}"),
            );
        }
    };

    // Load the certificate to get its payload_hash
    let mut lookup_options = CertificateLookupOptions::new(&options.certificate_id);
    if let Some(manifest_path) = options.manifest_path.as_deref() {
        lookup_options = lookup_options.with_manifest_path(manifest_path);
    }
    let show_report = show_certificate_with_options(&lookup_options);

    if show_report.verification_status == VerificationResult::NotFound {
        return SignReport::error(&options.certificate_id, "certificate not found");
    }
    let certificate = &show_report.certificate;

    let payload_hash = &certificate.payload_hash;

    // Sign the payload hash
    let signature_bytes = keypair.sign(payload_hash.as_bytes());
    let signature = format!("ed25519:{}", hex_lower(signature_bytes.as_ref()));
    let signer = signer_from_public_key(keypair.public_key().as_ref());

    SignReport {
        certificate_id: options.certificate_id.clone(),
        signature,
        algorithm: ED25519_ALGORITHM_V1.to_owned(),
        signer,
        payload_hash: payload_hash.to_owned(),
        signed_at: chrono::Utc::now().to_rfc3339(),
        success: true,
        message: "Certificate signed successfully".to_owned(),
    }
}

/// Verify an ed25519 signature.
fn verify_ed25519_signature(
    signature: &str,
    signer: &str,
    payload_hash: &str,
    key_dir: Option<&Path>,
) -> AttestationVerification {
    // Parse signer to extract fingerprint
    let Some(fingerprint) = signer.strip_prefix("ed25519:fp:") else {
        return AttestationVerification::Mismatch {
            signer: Some(signer.to_owned()),
            message: format!(
                "Invalid ed25519 signer format: expected 'ed25519:fp:<fingerprint>', got '{signer}'"
            ),
        };
    };

    // Parse signature
    let Some(sig_hex) = signature.strip_prefix("ed25519:") else {
        return AttestationVerification::Mismatch {
            signer: Some(signer.to_owned()),
            message: format!(
                "Invalid ed25519 signature format: expected 'ed25519:<hex>', got '{signature}'"
            ),
        };
    };

    let sig_bytes = match hex_decode(sig_hex) {
        Ok(bytes) => bytes,
        Err(err) => {
            return AttestationVerification::Mismatch {
                signer: Some(signer.to_owned()),
                message: format!("Invalid signature hex: {err}"),
            };
        }
    };

    // Find the public key by fingerprint
    let default_key_dir_buf;
    let key_dir = if let Some(dir) = key_dir {
        dir
    } else {
        default_key_dir_buf = match default_key_dir() {
            Ok(dir) => dir,
            Err(err) => {
                return AttestationVerification::Mismatch {
                    signer: Some(signer.to_owned()),
                    message: format!("Failed to resolve key directory: {err}"),
                };
            }
        };
        default_key_dir_buf.as_path()
    };

    // Search for key file matching the fingerprint
    let public_key = match find_public_key_by_fingerprint(key_dir, fingerprint) {
        Ok(Some(pk)) => pk,
        Ok(None) => {
            return AttestationVerification::Mismatch {
                signer: Some(signer.to_owned()),
                message: format!("No public key found for fingerprint {fingerprint}"),
            };
        }
        Err(err) => {
            return AttestationVerification::Mismatch {
                signer: Some(signer.to_owned()),
                message: format!("Failed to search for public key: {err}"),
            };
        }
    };

    // Verify the signature
    match signature::UnparsedPublicKey::new(&signature::ED25519, &public_key)
        .verify(payload_hash.as_bytes(), &sig_bytes)
    {
        Ok(()) => AttestationVerification::Ok {
            signer: Some(signer.to_owned()),
        },
        Err(_) => AttestationVerification::Mismatch {
            signer: Some(signer.to_owned()),
            message: "Ed25519 signature verification failed".to_owned(),
        },
    }
}

/// Find a public key by its fingerprint in the key directory.
fn find_public_key_by_fingerprint(
    key_dir: &Path,
    fingerprint: &str,
) -> io::Result<Option<Vec<u8>>> {
    if !key_dir_exists_no_symlinks(key_dir)? {
        return Ok(None);
    }

    for entry in fs::read_dir(key_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map(|e| e == "ed25519").unwrap_or(false) {
            if let Ok(key_data) = read_key_file_no_symlinks(&path) {
                if let Ok(keypair) = Ed25519KeyPair::from_pkcs8(&key_data) {
                    let public_key = keypair.public_key().as_ref();
                    let fp = blake3::hash(public_key).to_hex().to_string();
                    if fp == fingerprint {
                        return Ok(Some(public_key.to_vec()));
                    }
                }
            }
        }
    }

    Ok(None)
}

/// Decode hex string to bytes.
fn hex_decode(hex: &str) -> Result<Vec<u8>, String> {
    if hex.len() % 2 != 0 {
        return Err("odd-length hex string".to_owned());
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for chunk in hex.as_bytes().chunks(2) {
        let high = hex_digit(chunk[0])?;
        let low = hex_digit(chunk[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn hex_digit(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(format!("invalid hex digit: {}", char::from(byte))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn ensure_equal<T: std::fmt::Debug + PartialEq>(
        actual: &T,
        expected: &T,
        ctx: &str,
    ) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn certificate_list_schema_is_stable() -> TestResult {
        ensure_equal(
            &CERTIFICATE_LIST_SCHEMA_V1,
            &"ee.certificate.list.v1",
            "list schema",
        )
    }

    #[test]
    fn certificate_show_schema_is_stable() -> TestResult {
        ensure_equal(
            &CERTIFICATE_SHOW_SCHEMA_V1,
            &"ee.certificate.show.v1",
            "show schema",
        )
    }

    #[test]
    fn certificate_verify_schema_is_stable() -> TestResult {
        ensure_equal(
            &CERTIFICATE_VERIFY_SCHEMA_V1,
            &"ee.certificate.verify.v1",
            "verify schema",
        )
    }

    #[test]
    fn verification_result_strings_are_stable() -> TestResult {
        ensure_equal(&VerificationResult::Valid.as_str(), &"valid", "valid")?;
        ensure_equal(
            &VerificationResult::HashMismatch.as_str(),
            &"hash_mismatch",
            "hash_mismatch",
        )?;
        ensure_equal(
            &VerificationResult::StalePayloadHash.as_str(),
            &"stale_payload_hash",
            "stale_payload_hash",
        )?;
        ensure_equal(
            &VerificationResult::StaleSchemaVersion.as_str(),
            &"stale_schema_version",
            "stale_schema_version",
        )?;
        ensure_equal(
            &VerificationResult::FailedAssumptions.as_str(),
            &"failed_assumptions",
            "failed_assumptions",
        )?;
        ensure_equal(&VerificationResult::Expired.as_str(), &"expired", "expired")?;
        ensure_equal(&VerificationResult::Revoked.as_str(), &"revoked", "revoked")?;
        ensure_equal(
            &VerificationResult::AttestationMismatch.as_str(),
            &"attestation_mismatch",
            "attestation_mismatch",
        )?;
        ensure_equal(
            &VerificationResult::InvalidStatus.as_str(),
            &"invalid_status",
            "invalid_status",
        )?;
        ensure_equal(
            &VerificationResult::NotFound.as_str(),
            &"not_found",
            "not_found",
        )
    }

    #[test]
    fn verification_result_is_valid_check() -> TestResult {
        ensure(VerificationResult::Valid.is_valid(), "valid is valid")?;
        ensure(
            !VerificationResult::Expired.is_valid(),
            "expired is not valid",
        )?;
        ensure(
            !VerificationResult::NotFound.is_valid(),
            "not_found is not valid",
        )
    }

    #[test]
    fn verification_result_terminal_failures() -> TestResult {
        ensure(
            VerificationResult::HashMismatch.is_terminal_failure(),
            "hash_mismatch is terminal",
        )?;
        ensure(
            VerificationResult::StalePayloadHash.is_terminal_failure(),
            "stale_payload_hash is terminal",
        )?;
        ensure(
            VerificationResult::StaleSchemaVersion.is_terminal_failure(),
            "stale_schema_version is terminal",
        )?;
        ensure(
            VerificationResult::FailedAssumptions.is_terminal_failure(),
            "failed_assumptions is terminal",
        )?;
        ensure(
            VerificationResult::Revoked.is_terminal_failure(),
            "revoked is terminal",
        )?;
        ensure(
            VerificationResult::AttestationMismatch.is_terminal_failure(),
            "attestation_mismatch is terminal",
        )?;
        ensure(
            VerificationResult::NotFound.is_terminal_failure(),
            "not_found is terminal",
        )?;
        ensure(
            !VerificationResult::Valid.is_terminal_failure(),
            "valid is not terminal",
        )?;
        ensure(
            !VerificationResult::Expired.is_terminal_failure(),
            "expired is not terminal",
        )
    }

    #[test]
    fn list_certificates_returns_honest_empty_report_until_store_exists() -> TestResult {
        let options = CertificateListOptions::new();
        let report = list_certificates(&options);
        ensure(report.is_empty(), "certificate store is not wired yet")?;
        ensure_equal(&report.total_count, &0, "total count")?;
        ensure_equal(&report.usable_count, &0, "usable count")?;
        ensure_equal(&report.expired_count, &0, "expired count")?;
        ensure(
            report.kinds_present.is_empty(),
            "empty store must not advertise certificate kinds",
        )
    }

    #[test]
    fn list_certificates_filters_do_not_create_records() -> TestResult {
        let options = CertificateListOptions::new().with_kind(CertificateKind::Pack);
        let report = list_certificates(&options);
        ensure(report.certificates.is_empty(), "kind filter remains empty")?;

        let options = CertificateListOptions::new().with_status(CertificateStatus::Valid);
        let report = list_certificates(&options);
        ensure(
            report.certificates.is_empty(),
            "status filter remains empty",
        )
    }

    #[test]
    fn list_certificates_include_expired_and_limit_remain_empty() -> TestResult {
        let options = CertificateListOptions::new().include_expired();
        let report = list_certificates(&options);
        ensure(
            report.certificates.is_empty(),
            "include expired does not synthesize records",
        )?;

        let options = CertificateListOptions::new()
            .with_limit(2)
            .include_expired();
        let report = list_certificates(&options);
        ensure(report.certificates.is_empty(), "limit remains empty")
    }

    #[test]
    fn show_certificate_returns_not_found_for_legacy_mock_id() -> TestResult {
        let report = show_certificate("cert_pack_001");
        ensure_equal(
            &report.verification_status,
            &VerificationResult::NotFound,
            "legacy mock id is not found",
        )?;
        ensure_equal(
            &report.payload_summary,
            &"Certificate not found".to_owned(),
            "not-found summary",
        )
    }

    #[test]
    fn show_certificate_returns_not_found_for_invalid_id() -> TestResult {
        let report = show_certificate("nonexistent_cert");
        ensure_equal(
            &report.verification_status,
            &VerificationResult::NotFound,
            "should be not found",
        )
    }

    #[test]
    fn verify_certificate_reports_not_found_for_legacy_mock_ids() -> TestResult {
        let report = verify_certificate("cert_pack_001");
        ensure(!report.is_valid(), "should not be valid")?;
        ensure_equal(
            &report.result,
            &VerificationResult::NotFound,
            "legacy mock id is not found",
        )?;
        ensure(
            report.failure_codes.iter().any(|code| code == "not_found"),
            "not-found failure code",
        )?;

        let stale_payload = verify_certificate("cert_pack_stale_payload");
        ensure_equal(
            &stale_payload.result,
            &VerificationResult::NotFound,
            "legacy stale-payload fixture is not found",
        )
    }

    #[test]
    fn verify_certificate_not_found_for_invalid_id() -> TestResult {
        let report = verify_certificate("nonexistent_cert");
        ensure(!report.is_valid(), "should not be valid")?;
        ensure_equal(
            &report.result,
            &VerificationResult::NotFound,
            "should be not found",
        )
    }

    #[test]
    fn manifest_backed_certificate_lookup_and_verification_are_deterministic() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let payload = r#"{"packHash":"pack_valid","selected":["mem_01"]}"#;
        let payload_hash = blake3::hash(payload.as_bytes()).to_hex().to_string();
        fs::write(dir.path().join("payload.json"), payload).map_err(|error| error.to_string())?;
        let manifest = serde_json::json!({
            "schema": CERTIFICATE_MANIFEST_SCHEMA_V1,
            "certificates": [
                {
                    "id": "cert_pack_valid",
                    "kind": "pack",
                    "status": "valid",
                    "workspaceId": "workspace_main",
                    "issuedAt": "2026-05-01T00:00:00Z",
                    "expiresAt": "2999-01-01T00:00:00Z",
                    "payloadPath": "payload.json",
                    "payloadHash": payload_hash,
                    "payloadSchema": CERTIFICATE_PAYLOAD_SCHEMA_V1,
                    "assumptions": [{"valid": true}]
                },
                {
                    "id": "cert_pack_failed_assumptions",
                    "kind": "pack",
                    "status": "valid",
                    "workspaceId": "workspace_main",
                    "issuedAt": "2026-05-01T00:00:00Z",
                    "expiresAt": "2999-01-01T00:00:00Z",
                    "payloadPath": "payload.json",
                    "payloadHash": "wrong_hash",
                    "payloadSchema": CERTIFICATE_PAYLOAD_SCHEMA_V1,
                    "assumptions": [{"valid": false}]
                }
            ]
        });
        let manifest_path = dir.path().join("certificates.json");
        let manifest_json =
            serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?;
        fs::write(&manifest_path, manifest_json).map_err(|error| error.to_string())?;

        let list = list_certificates(
            &CertificateListOptions::new()
                .with_manifest_path(&manifest_path)
                .with_limit(1),
        );
        ensure_equal(&list.total_count, &2, "total count")?;
        ensure_equal(
            &list.certificates[0].id,
            &"cert_pack_failed_assumptions".to_owned(),
            "stable order",
        )?;

        let shown = show_certificate_with_options(
            &CertificateLookupOptions::new("cert_pack_valid").with_manifest_path(&manifest_path),
        );
        ensure_equal(
            &shown.verification_status,
            &VerificationResult::Valid,
            "show verification",
        )?;

        let valid = verify_certificate_with_options(
            &CertificateLookupOptions::new("cert_pack_valid").with_manifest_path(&manifest_path),
        );
        ensure_equal(&valid.result, &VerificationResult::Valid, "valid result")?;
        ensure(valid.hash_verified, "valid hash is verified")?;

        let failed = verify_certificate_with_options(
            &CertificateLookupOptions::new("cert_pack_failed_assumptions")
                .with_manifest_path(&manifest_path),
        );
        ensure_equal(
            &failed.result,
            &VerificationResult::FailedAssumptions,
            "failed assumptions win before hash mismatch",
        )
    }

    #[cfg(unix)]
    #[test]
    fn manifest_backed_certificate_rejects_symlinked_manifest_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let real_manifest = dir.path().join("real-certificates.json");
        let manifest = serde_json::json!({
            "schema": CERTIFICATE_MANIFEST_SCHEMA_V1,
            "certificates": []
        });
        fs::write(
            &real_manifest,
            serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let linked_manifest = dir.path().join("certificates.json");
        std::os::unix::fs::symlink(&real_manifest, &linked_manifest)
            .map_err(|error| error.to_string())?;

        let report = verify_certificate_with_options(
            &CertificateLookupOptions::new("cert_pack_valid").with_manifest_path(&linked_manifest),
        );

        ensure_equal(
            &report.result,
            &VerificationResult::StaleSchemaVersion,
            "symlinked manifest result",
        )?;
        ensure(
            report
                .failure_codes
                .iter()
                .any(|code| code == "invalid_manifest"),
            "symlinked manifest failure code",
        )?;
        ensure(
            report
                .message
                .contains("certificate manifest symlink refused"),
            "symlinked manifest message",
        )
    }

    #[cfg(unix)]
    #[test]
    fn manifest_backed_certificate_rejects_symlinked_manifest_parent() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let real_dir = dir.path().join("real-manifests");
        fs::create_dir_all(&real_dir).map_err(|error| error.to_string())?;
        let manifest = serde_json::json!({
            "schema": CERTIFICATE_MANIFEST_SCHEMA_V1,
            "certificates": []
        });
        fs::write(
            real_dir.join("certificates.json"),
            serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let linked_dir = dir.path().join("linked-manifests");
        std::os::unix::fs::symlink(&real_dir, &linked_dir).map_err(|error| error.to_string())?;

        let report = verify_certificate_with_options(
            &CertificateLookupOptions::new("cert_pack_valid")
                .with_manifest_path(linked_dir.join("certificates.json")),
        );

        ensure_equal(
            &report.result,
            &VerificationResult::StaleSchemaVersion,
            "symlinked manifest parent result",
        )?;
        ensure(
            report
                .failure_codes
                .iter()
                .any(|code| code == "invalid_manifest"),
            "symlinked manifest parent failure code",
        )?;
        ensure(
            report
                .message
                .contains("certificate manifest symlink refused"),
            "symlinked manifest parent message",
        )
    }

    #[test]
    fn manifest_backed_certificate_verifies_local_content_attestation() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let payload = r#"{"packHash":"signed","selected":["mem_01"]}"#;
        let payload_hash = blake3::hash(payload.as_bytes()).to_hex().to_string();
        let signer = "local:test-key";
        let attestation = local_content_hash_attestation(signer, &payload_hash);
        fs::write(dir.path().join("payload.json"), payload).map_err(|error| error.to_string())?;
        let manifest = serde_json::json!({
            "schema": CERTIFICATE_MANIFEST_SCHEMA_V1,
            "certificates": [
                {
                    "id": "cert_pack_signed",
                    "kind": "pack",
                    "status": "valid",
                    "workspaceId": "workspace_main",
                    "issuedAt": "2026-05-01T00:00:00Z",
                    "expiresAt": "2999-01-01T00:00:00Z",
                    "payloadPath": "payload.json",
                    "payloadHash": payload_hash,
                    "payloadSchema": CERTIFICATE_PAYLOAD_SCHEMA_V1,
                    "signature": attestation,
                    "signatureAlgorithm": "ee.local-content-hash.v1",
                    "signer": signer,
                    "assumptions": [{"valid": true}]
                }
            ]
        });
        let manifest_path = dir.path().join("certificates.json");
        let manifest_json =
            serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?;
        fs::write(&manifest_path, manifest_json).map_err(|error| error.to_string())?;

        let report = verify_certificate_with_options(
            &CertificateLookupOptions::new("cert_pack_signed").with_manifest_path(&manifest_path),
        );

        ensure_equal(&report.result, &VerificationResult::Valid, "attested valid")?;
        ensure(report.attestation_ok, "attestation ok")?;
        ensure_equal(&report.signer, &Some(signer.to_owned()), "signer")
    }

    #[test]
    fn manifest_backed_certificate_rejects_attestation_mismatch() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let payload = r#"{"packHash":"signed","selected":["mem_01"]}"#;
        let payload_hash = blake3::hash(payload.as_bytes()).to_hex().to_string();
        fs::write(dir.path().join("payload.json"), payload).map_err(|error| error.to_string())?;
        let manifest = serde_json::json!({
            "schema": CERTIFICATE_MANIFEST_SCHEMA_V1,
            "certificates": [
                {
                    "id": "cert_pack_bad_signature",
                    "kind": "pack",
                    "status": "valid",
                    "workspaceId": "workspace_main",
                    "issuedAt": "2026-05-01T00:00:00Z",
                    "expiresAt": "2999-01-01T00:00:00Z",
                    "payloadPath": "payload.json",
                    "payloadHash": payload_hash,
                    "payloadSchema": CERTIFICATE_PAYLOAD_SCHEMA_V1,
                    "signature": "sha256:bad",
                    "signatureAlgorithm": "ee.local-content-hash.v1",
                    "signer": "local:test-key",
                    "assumptions": [{"valid": true}]
                }
            ]
        });
        let manifest_path = dir.path().join("certificates.json");
        let manifest_json =
            serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?;
        fs::write(&manifest_path, manifest_json).map_err(|error| error.to_string())?;

        let report = verify_certificate_with_options(
            &CertificateLookupOptions::new("cert_pack_bad_signature")
                .with_manifest_path(&manifest_path),
        );

        ensure_equal(
            &report.result,
            &VerificationResult::AttestationMismatch,
            "attestation mismatch result",
        )?;
        ensure(!report.attestation_ok, "attestation not ok")?;
        ensure(
            report
                .mismatches
                .iter()
                .any(|code| code == "attestation_mismatch"),
            "attestation mismatch code",
        )
    }

    #[test]
    fn sigstore_bundle_algorithm_is_rejected_with_honest_message() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let payload = r#"{"packHash":"signed","selected":["mem_01"]}"#;
        let payload_hash = blake3::hash(payload.as_bytes()).to_hex().to_string();
        fs::write(dir.path().join("payload.json"), payload).map_err(|error| error.to_string())?;
        let manifest = serde_json::json!({
            "schema": CERTIFICATE_MANIFEST_SCHEMA_V1,
            "certificates": [
                {
                    "id": "cert_pack_sigstore_lie",
                    "kind": "pack",
                    "status": "valid",
                    "workspaceId": "workspace_main",
                    "issuedAt": "2026-05-01T00:00:00Z",
                    "expiresAt": "2999-01-01T00:00:00Z",
                    "payloadPath": "payload.json",
                    "payloadHash": payload_hash,
                    "payloadSchema": CERTIFICATE_PAYLOAD_SCHEMA_V1,
                    "signature": "sigstore-sha256:deadbeef",
                    "signatureAlgorithm": "sigstore.bundle-sha256.v1",
                    "signer": "local:test-key",
                    "assumptions": [{"valid": true}]
                }
            ]
        });
        let manifest_path = dir.path().join("certificates.json");
        let manifest_json =
            serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?;
        fs::write(&manifest_path, manifest_json).map_err(|error| error.to_string())?;

        let report = verify_certificate_with_options(
            &CertificateLookupOptions::new("cert_pack_sigstore_lie")
                .with_manifest_path(&manifest_path),
        );

        ensure_equal(
            &report.result,
            &VerificationResult::AttestationMismatch,
            "sigstore lie rejected",
        )?;
        ensure(!report.attestation_ok, "sigstore lie not ok")?;
        ensure(
            report.message.contains("sigstore.bundle-sha256.v1"),
            "honest mention of removed algorithm",
        )?;
        ensure(
            report.message.contains("ee.ed25519.v1"),
            "directs user to real signing implementation",
        )
    }

    #[test]
    fn manifest_payload_paths_stay_within_manifest_directory() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let manifest = serde_json::json!({
            "schema": CERTIFICATE_MANIFEST_SCHEMA_V1,
            "certificates": [
                {
                    "id": "cert_pack_unsafe_payload",
                    "kind": "pack",
                    "status": "valid",
                    "workspaceId": "workspace_main",
                    "issuedAt": "2026-05-01T00:00:00Z",
                    "expiresAt": "2999-01-01T00:00:00Z",
                    "payloadPath": "../payload.json",
                    "payloadHash": blake3::hash(b"payload").to_hex().to_string(),
                    "payloadSchema": CERTIFICATE_PAYLOAD_SCHEMA_V1,
                    "assumptions": [{"valid": true}]
                }
            ]
        });
        let manifest_path = dir.path().join("certificates.json");
        let manifest_json =
            serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?;
        fs::write(&manifest_path, manifest_json).map_err(|error| error.to_string())?;

        let report = verify_certificate_with_options(
            &CertificateLookupOptions::new("cert_pack_unsafe_payload")
                .with_manifest_path(&manifest_path),
        );
        ensure_equal(
            &report.result,
            &VerificationResult::StaleSchemaVersion,
            "invalid manifest result",
        )?;
        ensure(
            report
                .failure_codes
                .iter()
                .any(|code| code == "invalid_manifest"),
            "invalid manifest failure code",
        )?;
        ensure(
            report.message.contains("unsafe certificate payloadPath"),
            "unsafe payload path message",
        )
    }

    #[test]
    fn manifest_payload_read_rejects_non_regular_payload_path() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        fs::create_dir(dir.path().join("payload.json")).map_err(|error| error.to_string())?;

        let error = read_manifest_payload(dir.path(), Path::new("payload.json"))
            .expect_err("directory payload path must be rejected");

        ensure_equal(
            &error.kind(),
            &io::ErrorKind::InvalidInput,
            "non-regular manifest payload error kind",
        )?;
        ensure(
            error.to_string().contains("not a regular file"),
            "non-regular manifest payload message",
        )
    }

    #[test]
    fn database_payload_hash_rejects_non_regular_payload_path() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        fs::create_dir(dir.path().join("payload.json")).map_err(|error| error.to_string())?;
        let manifest_path = dir.path().join("certificates.json");
        let record = StoredCertificateRecord {
            id: "cert_pack_directory_payload".to_owned(),
            workspace_id: "workspace_main".to_owned(),
            target_kind: "pack".to_owned(),
            target_id: "pack_directory_payload".to_owned(),
            hash_algo: "blake3".to_owned(),
            content_hash: format!("blake3:{}", blake3::hash(b"payload").to_hex()),
            signature: None,
            signature_algorithm: None,
            signer: None,
            signed_at: None,
            verified_at: None,
            status: "valid".to_owned(),
            manifest_path: Some(manifest_path.to_string_lossy().into_owned()),
            payload_path: Some("payload.json".to_owned()),
            metadata_json: "{}".to_owned(),
            created_at: "2026-05-01T00:00:00Z".to_owned(),
            updated_at: "2026-05-01T00:00:00Z".to_owned(),
        };

        let error = database_payload_hash_matches(&record)
            .expect_err("directory database payload path must be rejected");

        ensure_equal(
            &error.kind(),
            &io::ErrorKind::InvalidInput,
            "non-regular database payload error kind",
        )?;
        ensure(
            error.to_string().contains("not a regular file"),
            "non-regular database payload message",
        )
    }

    #[test]
    fn certificate_summary_from_certificate() -> TestResult {
        let cert = Certificate {
            id: "test_cert".to_string(),
            kind: CertificateKind::Curation,
            status: CertificateStatus::Valid,
            workspace_id: "wsp_test".to_string(),
            issued_at: "2026-04-30T12:00:00Z".to_string(),
            expires_at: None,
            payload_hash: "abc123".to_string(),
            decision_metadata: DecisionPlaneMetadata::empty(),
        };
        let summary = CertificateSummary::from(&cert);
        ensure_equal(&summary.id, &cert.id, "id")?;
        ensure_equal(&summary.kind, &cert.kind, "kind")?;
        ensure_equal(&summary.status, &cert.status, "status")?;
        ensure(summary.is_usable, "should be usable")
    }

    #[test]
    fn ed25519_round_trip_sign_verify_succeeds() -> TestResult {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let key_dir = dir.path().join("keys");
        fs::create_dir_all(&key_dir).map_err(|e| e.to_string())?;

        let key_path = key_dir.join("test_workspace.ed25519");
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 =
            ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).map_err(|e| e.to_string())?;
        fs::write(&key_path, pkcs8.as_ref()).map_err(|e| e.to_string())?;

        let keypair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
            .map_err(|e| e.to_string())?;
        let public_key = keypair.public_key().as_ref();
        let fingerprint = blake3::hash(public_key).to_hex().to_string();
        let signer = format!("ed25519:fp:{fingerprint}");

        let payload_hash = "test_payload_hash_abc123";
        let sig_bytes = keypair.sign(payload_hash.as_bytes());
        let signature = format!("ed25519:{}", hex_lower(sig_bytes.as_ref()));

        let result = verify_ed25519_signature(&signature, &signer, payload_hash, Some(&key_dir));
        match result {
            AttestationVerification::Ok { .. } => Ok(()),
            AttestationVerification::Mismatch { message, .. } => {
                Err(format!("round-trip verification should pass: {message}"))
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn sign_certificate_rejects_symlinked_key_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let key_dir = dir.path().join("keys");
        let outside_dir = dir.path().join("outside");
        fs::create_dir_all(&key_dir).map_err(|e| e.to_string())?;
        fs::create_dir_all(&outside_dir).map_err(|e| e.to_string())?;

        let real_key_path = outside_dir.join("real.ed25519");
        let linked_key_path = key_dir.join("linked.ed25519");
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 =
            ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).map_err(|e| e.to_string())?;
        fs::write(&real_key_path, pkcs8.as_ref()).map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink(&real_key_path, &linked_key_path).map_err(|e| e.to_string())?;

        let payload = r#"{"packHash":"signed","selected":["mem_01"]}"#;
        let payload_hash = blake3::hash(payload.as_bytes()).to_hex().to_string();
        fs::write(dir.path().join("payload.json"), payload).map_err(|e| e.to_string())?;
        let manifest = serde_json::json!({
            "schema": CERTIFICATE_MANIFEST_SCHEMA_V1,
            "certificates": [
                {
                    "id": "cert_pack_signed",
                    "kind": "pack",
                    "status": "valid",
                    "workspaceId": "workspace_main",
                    "issuedAt": "2026-05-01T00:00:00Z",
                    "expiresAt": "2999-01-01T00:00:00Z",
                    "payloadPath": "payload.json",
                    "payloadHash": payload_hash,
                    "payloadSchema": CERTIFICATE_PAYLOAD_SCHEMA_V1,
                    "assumptions": [{"valid": true}]
                }
            ]
        });
        let manifest_path = dir.path().join("certificates.json");
        let manifest_json =
            serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?;
        fs::write(&manifest_path, manifest_json).map_err(|e| e.to_string())?;

        let report = sign_certificate(&SignOptions {
            certificate_id: "cert_pack_signed".to_owned(),
            manifest_path: Some(manifest_path),
            key_path: Some(linked_key_path),
            workspace_path: None,
        });

        ensure(!report.success, "symlinked key must not sign")?;
        ensure(
            report.message.contains("symlink"),
            "sign error should mention symlink",
        )
    }

    #[test]
    fn certificate_key_write_rejects_non_regular_final_path() -> TestResult {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let key_path = dir.path().join("keys").join("workspace.ed25519");
        fs::create_dir_all(&key_path).map_err(|error| error.to_string())?;

        let error = write_key_file_no_symlinks(&key_path, b"not a real key")
            .expect_err("non-regular key path should reject key write");

        ensure_equal(&error.kind(), &io::ErrorKind::InvalidInput, "error kind")?;
        ensure(
            error.to_string().contains("not a file"),
            "error should mention non-file key path",
        )?;
        ensure(
            fs::symlink_metadata(&key_path)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_dir(),
            "non-regular key path must remain a directory",
        )
    }

    #[test]
    fn certificate_key_write_rejects_existing_temp_without_truncating() -> TestResult {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let key_path = dir.path().join("keys").join("workspace.ed25519");
        let temp_path = key_temp_path(&key_path).map_err(|error| error.to_string())?;
        fs::create_dir_all(key_path.parent().expect("key parent"))
            .map_err(|error| error.to_string())?;
        fs::write(&temp_path, b"stale key temp").map_err(|error| error.to_string())?;

        let error = write_key_file_no_symlinks(&key_path, b"new key bytes")
            .expect_err("existing key temp path should reject key write");

        ensure_equal(&error.kind(), &io::ErrorKind::AlreadyExists, "error kind")?;
        ensure_equal(
            &fs::read(&temp_path).map_err(|error| error.to_string())?,
            &b"stale key temp".to_vec(),
            "stale temp content",
        )?;
        match fs::symlink_metadata(&key_path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err("final key path must not be published".to_owned()),
            Err(error) => Err(format!("final key metadata failed: {error}")),
        }
    }

    #[cfg(unix)]
    #[test]
    fn certificate_key_publish_rechecks_final_symlink_before_rename() -> TestResult {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let key_path = dir.path().join("keys").join("workspace.ed25519");
        let temp_path = key_temp_path(&key_path).map_err(|error| error.to_string())?;
        fs::create_dir_all(key_path.parent().expect("key parent"))
            .map_err(|error| error.to_string())?;
        fs::write(&temp_path, b"new key bytes").map_err(|error| error.to_string())?;
        let outside_path = dir.path().join("outside-key.ed25519");
        fs::write(&outside_path, b"outside key").map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&outside_path, &key_path).map_err(|error| error.to_string())?;

        let error = publish_key_temp_file(&temp_path, &key_path)
            .expect_err("symlinked final key path should reject before publish");

        ensure_equal(&error.kind(), &io::ErrorKind::InvalidInput, "error kind")?;
        ensure(
            error.to_string().contains("symlink"),
            "error should mention final key symlink",
        )?;
        ensure_equal(
            &fs::read(&outside_path).map_err(|error| error.to_string())?,
            &b"outside key".to_vec(),
            "outside key content",
        )?;
        ensure(
            temp_path.is_file(),
            "temp key file should remain after final path rejection",
        )?;
        ensure(
            fs::symlink_metadata(&key_path)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_symlink(),
            "final key symlink should remain untouched",
        )
    }

    #[cfg(unix)]
    #[test]
    fn certificate_key_publish_rechecks_temp_symlink_before_rename() -> TestResult {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let key_path = dir.path().join("keys").join("workspace.ed25519");
        let temp_path = key_temp_path(&key_path).map_err(|error| error.to_string())?;
        fs::create_dir_all(key_path.parent().expect("key parent"))
            .map_err(|error| error.to_string())?;
        let outside_path = dir.path().join("outside-key.ed25519");
        fs::write(&outside_path, b"outside key").map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&outside_path, &temp_path).map_err(|error| error.to_string())?;

        let error = publish_key_temp_file(&temp_path, &key_path)
            .expect_err("symlinked temp key path should reject before publish");

        ensure_equal(&error.kind(), &io::ErrorKind::InvalidInput, "error kind")?;
        ensure(
            error.to_string().contains("symlink"),
            "error should mention temp key symlink",
        )?;
        ensure_equal(
            &fs::read(&outside_path).map_err(|error| error.to_string())?,
            &b"outside key".to_vec(),
            "outside key content",
        )?;
        match fs::symlink_metadata(&key_path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => return Err("final key path must not be published".to_owned()),
            Err(error) => return Err(format!("final key metadata failed: {error}")),
        }
        ensure(
            fs::symlink_metadata(&temp_path)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_symlink(),
            "temp key symlink should remain untouched",
        )
    }

    #[test]
    fn certificate_key_exists_rejects_non_regular_key_path() -> TestResult {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let key_path = dir.path().join("keys").join("workspace.ed25519");
        fs::create_dir_all(&key_path).map_err(|error| error.to_string())?;

        let error = key_path_exists_no_symlinks(&key_path)
            .expect_err("non-regular key path should reject key existence preflight");

        ensure_equal(&error.kind(), &io::ErrorKind::InvalidInput, "error kind")?;
        ensure(
            error.to_string().contains("not a file"),
            "error should mention non-file key path",
        )?;
        ensure(
            fs::symlink_metadata(&key_path)
                .map_err(|error| error.to_string())?
                .file_type()
                .is_dir(),
            "non-regular key path must remain a directory",
        )
    }

    #[test]
    fn certificate_key_directory_accepts_real_directory_for_lookup() -> TestResult {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let key_dir = dir.path().join("keys");
        fs::create_dir_all(&key_dir).map_err(|error| error.to_string())?;

        let exists = key_dir_exists_no_symlinks(&key_dir).map_err(|error| error.to_string())?;

        ensure(exists, "real key directory should be accepted")
    }

    #[test]
    fn certificate_key_directory_rejects_non_directory_lookup_path() -> TestResult {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let key_dir = dir.path().join("keys");
        fs::write(&key_dir, b"not a directory").map_err(|error| error.to_string())?;

        let error = key_dir_exists_no_symlinks(&key_dir)
            .expect_err("file key directory path should reject lookup preflight");

        ensure_equal(&error.kind(), &io::ErrorKind::InvalidInput, "error kind")?;
        ensure(
            error.to_string().contains("not a directory"),
            "error should mention non-directory key path",
        )
    }

    #[cfg(unix)]
    #[test]
    fn ed25519_verification_ignores_symlinked_key_entries() -> TestResult {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let key_dir = dir.path().join("keys");
        let outside_dir = dir.path().join("outside");
        fs::create_dir_all(&key_dir).map_err(|e| e.to_string())?;
        fs::create_dir_all(&outside_dir).map_err(|e| e.to_string())?;

        let real_key_path = outside_dir.join("real.ed25519");
        let linked_key_path = key_dir.join("linked.ed25519");
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 =
            ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).map_err(|e| e.to_string())?;
        fs::write(&real_key_path, pkcs8.as_ref()).map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink(&real_key_path, &linked_key_path).map_err(|e| e.to_string())?;

        let keypair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
            .map_err(|e| e.to_string())?;
        let signer = signer_from_public_key(keypair.public_key().as_ref());
        let payload_hash = "payload_hash_from_untrusted_key_entry";
        let signature = format!(
            "ed25519:{}",
            hex_lower(keypair.sign(payload_hash.as_bytes()).as_ref())
        );

        let result = verify_ed25519_signature(&signature, &signer, payload_hash, Some(&key_dir));
        match result {
            AttestationVerification::Mismatch { message, .. } => ensure(
                message.contains("No public key found"),
                "symlinked key entry should be ignored",
            ),
            AttestationVerification::Ok { .. } => {
                Err("symlinked key entry must not verify".to_owned())
            }
        }
    }

    #[test]
    fn ed25519_forged_signature_from_public_inputs_fails() -> TestResult {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let key_dir = dir.path().join("keys");
        fs::create_dir_all(&key_dir).map_err(|e| e.to_string())?;

        let key_path = key_dir.join("test_workspace.ed25519");
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 =
            ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).map_err(|e| e.to_string())?;
        fs::write(&key_path, pkcs8.as_ref()).map_err(|e| e.to_string())?;

        let keypair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
            .map_err(|e| e.to_string())?;
        let public_key = keypair.public_key().as_ref();
        let fingerprint = blake3::hash(public_key).to_hex().to_string();
        let signer = format!("ed25519:fp:{fingerprint}");
        let payload_hash = "victim_payload_hash_xyz789";

        let forged_sig = "ed25519:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

        let result = verify_ed25519_signature(forged_sig, &signer, payload_hash, Some(&key_dir));
        match result {
            AttestationVerification::Mismatch { .. } => Ok(()),
            AttestationVerification::Ok { .. } => {
                Err("forged signature should NOT pass verification".to_string())
            }
        }
    }

    #[test]
    fn ed25519_mutated_payload_verification_fails() -> TestResult {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let key_dir = dir.path().join("keys");
        fs::create_dir_all(&key_dir).map_err(|e| e.to_string())?;

        let key_path = key_dir.join("test_workspace.ed25519");
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 =
            ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).map_err(|e| e.to_string())?;
        fs::write(&key_path, pkcs8.as_ref()).map_err(|e| e.to_string())?;

        let keypair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
            .map_err(|e| e.to_string())?;
        let public_key = keypair.public_key().as_ref();
        let fingerprint = blake3::hash(public_key).to_hex().to_string();
        let signer = format!("ed25519:fp:{fingerprint}");

        let original_payload = "original_payload_hash";
        let sig_bytes = keypair.sign(original_payload.as_bytes());
        let signature = format!("ed25519:{}", hex_lower(sig_bytes.as_ref()));

        let mutated_payload = "mutated_payload_hash";
        let result = verify_ed25519_signature(&signature, &signer, mutated_payload, Some(&key_dir));
        match result {
            AttestationVerification::Mismatch { .. } => Ok(()),
            AttestationVerification::Ok { .. } => {
                Err("mutated payload should NOT pass verification".to_string())
            }
        }
    }

    #[test]
    fn ed25519_adversarial_attacker_cannot_mint_valid_attestation() -> TestResult {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let key_dir = dir.path().join("keys");
        fs::create_dir_all(&key_dir).map_err(|e| e.to_string())?;

        let victim_key_path = key_dir.join("victim.ed25519");
        let rng = ring::rand::SystemRandom::new();
        let victim_pkcs8 =
            ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).map_err(|e| e.to_string())?;
        fs::write(&victim_key_path, victim_pkcs8.as_ref()).map_err(|e| e.to_string())?;

        let victim_keypair = ring::signature::Ed25519KeyPair::from_pkcs8(victim_pkcs8.as_ref())
            .map_err(|e| e.to_string())?;
        let victim_public = victim_keypair.public_key().as_ref();
        let victim_fp = blake3::hash(victim_public).to_hex().to_string();
        let victim_signer = format!("ed25519:fp:{victim_fp}");

        let payload_hash = "sensitive_data_hash";

        let attacker_pkcs8 =
            ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).map_err(|e| e.to_string())?;
        let attacker_keypair = ring::signature::Ed25519KeyPair::from_pkcs8(attacker_pkcs8.as_ref())
            .map_err(|e| e.to_string())?;
        let attacker_sig = attacker_keypair.sign(payload_hash.as_bytes());
        let forged_signature = format!("ed25519:{}", hex_lower(attacker_sig.as_ref()));

        let result = verify_ed25519_signature(
            &forged_signature,
            &victim_signer,
            payload_hash,
            Some(&key_dir),
        );
        match result {
            AttestationVerification::Mismatch { .. } => Ok(()),
            AttestationVerification::Ok { .. } => Err(
                "attacker signature with victim's signer should NOT pass verification".to_string(),
            ),
        }
    }
}
