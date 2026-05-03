//! Certificate operations (EE-342).
//!
//! Provides list, show, and verify operations for certificate records.
//! Certificates are typed verification artifacts that make "alien artifact
//! math" inspectable and auditable.
//!
//! The certificate manifest store is not wired in this slice. Core operations
//! therefore return honest empty/not-found reports instead of sample records.

use crate::models::{Certificate, CertificateKind, CertificateStatus, DecisionPlaneMetadata};

/// Schema version for certificate list responses.
pub const CERTIFICATE_LIST_SCHEMA_V1: &str = "ee.certificate.list.v1";

/// Schema version for certificate show responses.
pub const CERTIFICATE_SHOW_SCHEMA_V1: &str = "ee.certificate.show.v1";

/// Schema version for certificate verify responses.
pub const CERTIFICATE_VERIFY_SCHEMA_V1: &str = "ee.certificate.verify.v1";

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
            failure_codes: vec!["failed_assumptions".to_owned()],
            message: "Certificate assumptions failed during verification".to_owned(),
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
pub fn list_certificates(_options: &CertificateListOptions) -> CertificateListReport {
    CertificateListReport::new()
}

/// Show a certificate by ID.
#[must_use]
pub fn show_certificate(certificate_id: &str) -> CertificateShowReport {
    CertificateShowReport::not_found(certificate_id)
}

/// Verify a certificate by ID.
#[must_use]
pub fn verify_certificate(certificate_id: &str) -> CertificateVerifyReport {
    CertificateVerifyReport::not_found(certificate_id)
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
}
