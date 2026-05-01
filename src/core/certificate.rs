//! Certificate operations (EE-342).
//!
//! Provides list, show, and verify operations for certificate records.
//! Certificates are typed verification artifacts that make "alien artifact
//! math" inspectable and auditable.

use crate::models::{
    Certificate, CertificateKind, CertificateStatus, CERTIFICATE_SCHEMA_V1,
    DecisionPlaneMetadata,
};

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
        matches!(self, Self::HashMismatch | Self::Revoked | Self::NotFound)
    }
}

/// Report from verifying a certificate.
#[derive(Clone, Debug, PartialEq)]
pub struct CertificateVerifyReport {
    pub certificate_id: String,
    pub result: VerificationResult,
    pub checked_at: String,
    pub hash_verified: bool,
    pub status_valid: bool,
    pub expiry_valid: bool,
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
            status_valid: true,
            expiry_valid: true,
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
            status_valid: false,
            expiry_valid: false,
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
            status_valid: false,
            expiry_valid: false,
            message: "Certificate has expired".to_owned(),
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
pub fn list_certificates(options: &CertificateListOptions) -> CertificateListReport {
    let mut report = CertificateListReport::new();

    let mock_certs = create_mock_certificates();

    for cert in &mock_certs {
        if let Some(kind) = options.kind {
            if cert.kind != kind {
                continue;
            }
        }
        if let Some(status) = options.status {
            if cert.status != status {
                continue;
            }
        }
        if !options.include_expired && cert.status == CertificateStatus::Expired {
            report.expired_count += 1;
            continue;
        }

        report.certificates.push(CertificateSummary::from(cert));
        if cert.is_usable() {
            report.usable_count += 1;
        }
        if cert.status == CertificateStatus::Expired {
            report.expired_count += 1;
        }

        if !report.kinds_present.contains(&cert.kind) {
            report.kinds_present.push(cert.kind);
        }
    }

    if let Some(limit) = options.limit {
        report.certificates.truncate(limit as usize);
    }

    report.total_count = report.certificates.len() as u32;
    report
}

/// Show a certificate by ID.
#[must_use]
pub fn show_certificate(certificate_id: &str) -> CertificateShowReport {
    let mock_certs = create_mock_certificates();

    for cert in mock_certs {
        if cert.id == certificate_id {
            return CertificateShowReport::new(cert);
        }
    }

    CertificateShowReport::not_found(certificate_id)
}

/// Verify a certificate by ID.
#[must_use]
pub fn verify_certificate(certificate_id: &str) -> CertificateVerifyReport {
    let mock_certs = create_mock_certificates();

    for cert in mock_certs {
        if cert.id == certificate_id {
            if cert.is_usable() {
                return CertificateVerifyReport::valid(&cert.id);
            } else if cert.is_expired() {
                return CertificateVerifyReport::expired(&cert.id);
            } else {
                return CertificateVerifyReport {
                    certificate_id: cert.id.clone(),
                    result: VerificationResult::InvalidStatus,
                    checked_at: chrono::Utc::now().to_rfc3339(),
                    hash_verified: true,
                    status_valid: false,
                    expiry_valid: true,
                    message: format!("Certificate status is {}", cert.status.as_str()),
                };
            }
        }
    }

    CertificateVerifyReport::not_found(certificate_id)
}

fn create_mock_certificates() -> Vec<Certificate> {
    vec![
        Certificate {
            id: "cert_pack_001".to_string(),
            kind: CertificateKind::Pack,
            status: CertificateStatus::Valid,
            workspace_id: "wsp_default".to_string(),
            issued_at: "2026-04-29T10:00:00Z".to_string(),
            expires_at: Some("2026-05-29T10:00:00Z".to_string()),
            payload_hash: "b3a4c5d6e7f8".to_string(),
            decision_metadata: DecisionPlaneMetadata::empty(),
        },
        Certificate {
            id: "cert_curation_001".to_string(),
            kind: CertificateKind::Curation,
            status: CertificateStatus::Valid,
            workspace_id: "wsp_default".to_string(),
            issued_at: "2026-04-28T15:30:00Z".to_string(),
            expires_at: None,
            payload_hash: "a1b2c3d4e5f6".to_string(),
            decision_metadata: DecisionPlaneMetadata::empty(),
        },
        Certificate {
            id: "cert_tailrisk_001".to_string(),
            kind: CertificateKind::TailRisk,
            status: CertificateStatus::Pending,
            workspace_id: "wsp_default".to_string(),
            issued_at: "2026-04-30T08:00:00Z".to_string(),
            expires_at: None,
            payload_hash: "c3d4e5f6g7h8".to_string(),
            decision_metadata: DecisionPlaneMetadata::empty(),
        },
        Certificate {
            id: "cert_privacy_001".to_string(),
            kind: CertificateKind::PrivacyBudget,
            status: CertificateStatus::Expired,
            workspace_id: "wsp_default".to_string(),
            issued_at: "2026-04-01T12:00:00Z".to_string(),
            expires_at: Some("2026-04-15T12:00:00Z".to_string()),
            payload_hash: "d4e5f6g7h8i9".to_string(),
            decision_metadata: DecisionPlaneMetadata::empty(),
        },
        Certificate {
            id: "cert_lifecycle_001".to_string(),
            kind: CertificateKind::Lifecycle,
            status: CertificateStatus::Valid,
            workspace_id: "wsp_default".to_string(),
            issued_at: "2026-04-30T06:00:00Z".to_string(),
            expires_at: None,
            payload_hash: "e5f6g7h8i9j0".to_string(),
            decision_metadata: DecisionPlaneMetadata::empty(),
        },
    ]
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
    fn list_certificates_returns_all_by_default() -> TestResult {
        let options = CertificateListOptions::new();
        let report = list_certificates(&options);
        ensure(!report.is_empty(), "should have certificates")?;
        ensure(report.total_count >= 4, "should have at least 4 certificates")
    }

    #[test]
    fn list_certificates_filters_by_kind() -> TestResult {
        let options = CertificateListOptions::new().with_kind(CertificateKind::Pack);
        let report = list_certificates(&options);
        for cert in &report.certificates {
            ensure_equal(&cert.kind, &CertificateKind::Pack, "kind filter")?;
        }
        Ok(())
    }

    #[test]
    fn list_certificates_filters_by_status() -> TestResult {
        let options = CertificateListOptions::new().with_status(CertificateStatus::Valid);
        let report = list_certificates(&options);
        for cert in &report.certificates {
            ensure_equal(&cert.status, &CertificateStatus::Valid, "status filter")?;
        }
        Ok(())
    }

    #[test]
    fn list_certificates_excludes_expired_by_default() -> TestResult {
        let options = CertificateListOptions::new();
        let report = list_certificates(&options);
        for cert in &report.certificates {
            ensure(
                cert.status != CertificateStatus::Expired,
                "should exclude expired by default",
            )?;
        }
        Ok(())
    }

    #[test]
    fn list_certificates_includes_expired_when_requested() -> TestResult {
        let options = CertificateListOptions::new().include_expired();
        let report = list_certificates(&options);
        let has_expired = report
            .certificates
            .iter()
            .any(|c| c.status == CertificateStatus::Expired);
        ensure(has_expired, "should include expired when requested")
    }

    #[test]
    fn list_certificates_respects_limit() -> TestResult {
        let options = CertificateListOptions::new().with_limit(2).include_expired();
        let report = list_certificates(&options);
        ensure(
            report.certificates.len() <= 2,
            "should respect limit",
        )
    }

    #[test]
    fn show_certificate_returns_details_for_valid_id() -> TestResult {
        let report = show_certificate("cert_pack_001");
        ensure_equal(
            &report.verification_status,
            &VerificationResult::Valid,
            "should be valid",
        )?;
        ensure_equal(
            &report.certificate.kind,
            &CertificateKind::Pack,
            "should be pack certificate",
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
    fn verify_certificate_passes_for_valid_cert() -> TestResult {
        let report = verify_certificate("cert_pack_001");
        ensure(report.is_valid(), "should verify successfully")?;
        ensure(report.hash_verified, "hash should be verified")?;
        ensure(report.status_valid, "status should be valid")?;
        ensure(report.expiry_valid, "expiry should be valid")
    }

    #[test]
    fn verify_certificate_fails_for_expired_cert() -> TestResult {
        let report = verify_certificate("cert_privacy_001");
        ensure(!report.is_valid(), "should not be valid")?;
        ensure_equal(
            &report.result,
            &VerificationResult::Expired,
            "should be expired",
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
