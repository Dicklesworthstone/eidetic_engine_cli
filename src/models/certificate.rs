//! Certificate domain models (EE-340).
//!
//! Certificates are typed verification artifacts that prove a computation
//! or decision was performed correctly. They make "alien artifact math"
//! inspectable and auditable rather than opaque.
//!
//! Certificate types:
//! - Pack: context pack assembly verification
//! - Curation: curation decision verification
//! - TailRisk: risk assessment bounds
//! - PrivacyBudget: privacy budget consumption
//! - Lifecycle: lifecycle event verification

use std::fmt;
use std::str::FromStr;

/// Schema version for certificate JSON output.
pub const CERTIFICATE_SCHEMA_V1: &str = "ee.certificate.v1";

/// Certificate type discriminator.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CertificateKind {
    /// Context pack assembly certificate.
    Pack,
    /// Curation decision certificate.
    Curation,
    /// Tail-risk assessment certificate.
    TailRisk,
    /// Privacy budget consumption certificate.
    PrivacyBudget,
    /// Lifecycle event certificate.
    Lifecycle,
}

impl CertificateKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pack => "pack",
            Self::Curation => "curation",
            Self::TailRisk => "tail_risk",
            Self::PrivacyBudget => "privacy_budget",
            Self::Lifecycle => "lifecycle",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Pack,
            Self::Curation,
            Self::TailRisk,
            Self::PrivacyBudget,
            Self::Lifecycle,
        ]
    }
}

impl fmt::Display for CertificateKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error when parsing an invalid certificate kind string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseCertificateKindError {
    input: String,
}

impl ParseCertificateKindError {
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseCertificateKindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown certificate kind `{}`; expected one of pack, curation, tail_risk, privacy_budget, lifecycle",
            self.input
        )
    }
}

impl std::error::Error for ParseCertificateKindError {}

impl FromStr for CertificateKind {
    type Err = ParseCertificateKindError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "pack" => Ok(Self::Pack),
            "curation" => Ok(Self::Curation),
            "tail_risk" => Ok(Self::TailRisk),
            "privacy_budget" => Ok(Self::PrivacyBudget),
            "lifecycle" => Ok(Self::Lifecycle),
            _ => Err(ParseCertificateKindError {
                input: input.to_owned(),
            }),
        }
    }
}

/// Certificate verification status.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CertificateStatus {
    /// Certificate is valid and verified.
    Valid,
    /// Certificate is pending verification.
    Pending,
    /// Certificate verification failed.
    Invalid,
    /// Certificate has expired.
    Expired,
    /// Certificate was revoked.
    Revoked,
}

impl CertificateStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Pending => "pending",
            Self::Invalid => "invalid",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Valid,
            Self::Pending,
            Self::Invalid,
            Self::Expired,
            Self::Revoked,
        ]
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Invalid | Self::Expired | Self::Revoked)
    }

    #[must_use]
    pub const fn is_usable(self) -> bool {
        matches!(self, Self::Valid)
    }
}

impl fmt::Display for CertificateStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error when parsing an invalid certificate status string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseCertificateStatusError {
    input: String,
}

impl ParseCertificateStatusError {
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseCertificateStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown certificate status `{}`; expected one of valid, pending, invalid, expired, revoked",
            self.input
        )
    }
}

impl std::error::Error for ParseCertificateStatusError {}

impl FromStr for CertificateStatus {
    type Err = ParseCertificateStatusError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "valid" => Ok(Self::Valid),
            "pending" => Ok(Self::Pending),
            "invalid" => Ok(Self::Invalid),
            "expired" => Ok(Self::Expired),
            "revoked" => Ok(Self::Revoked),
            _ => Err(ParseCertificateStatusError {
                input: input.to_owned(),
            }),
        }
    }
}

/// Pack certificate payload - proves context pack assembly correctness.
#[derive(Clone, Debug, PartialEq)]
pub struct PackCertificate {
    /// Hash of the assembled pack content.
    pub pack_hash: String,
    /// Query that produced this pack.
    pub query: String,
    /// Token budget used.
    pub budget_used: u32,
    /// Token budget limit.
    pub budget_limit: u32,
    /// Number of items included.
    pub item_count: u32,
    /// Number of items omitted.
    pub omitted_count: u32,
    /// Section quotas were satisfied.
    pub quotas_satisfied: bool,
    /// Redundancy control was applied.
    pub redundancy_applied: bool,
    /// All items have provenance.
    pub provenance_complete: bool,
}

impl PackCertificate {
    /// Check if the pack certificate indicates a valid assembly.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.budget_used <= self.budget_limit && self.quotas_satisfied && self.provenance_complete
    }
}

/// Curation certificate payload - proves curation decision correctness.
#[derive(Clone, Debug, PartialEq)]
pub struct CurationCertificate {
    /// ID of the curation candidate.
    pub candidate_id: String,
    /// Type of curation action.
    pub action_type: String,
    /// Confidence score of the decision.
    pub confidence: f64,
    /// Minimum confidence threshold.
    pub threshold: f64,
    /// Evidence count supporting the decision.
    pub evidence_count: u32,
    /// Feedback events considered.
    pub feedback_count: u32,
    /// Human review required.
    pub requires_review: bool,
    /// Decision is reversible.
    pub reversible: bool,
}

impl CurationCertificate {
    /// Check if the curation meets the confidence threshold.
    #[must_use]
    pub fn meets_threshold(&self) -> bool {
        self.confidence >= self.threshold
    }

    /// Check if the curation has supporting evidence.
    #[must_use]
    pub const fn has_evidence(&self) -> bool {
        self.evidence_count > 0 || self.feedback_count > 0
    }
}

/// Tail-risk certificate payload - proves risk assessment bounds.
#[derive(Clone, Debug, PartialEq)]
pub struct TailRiskCertificate {
    /// Risk metric name.
    pub metric: String,
    /// Observed value.
    pub observed: f64,
    /// Threshold that triggers concern.
    pub threshold: f64,
    /// Confidence level (e.g., 0.95 for 95%).
    pub confidence_level: f64,
    /// Upper bound of the risk estimate.
    pub upper_bound: f64,
    /// Whether risk exceeds acceptable bounds.
    pub exceeds_bounds: bool,
    /// Recommended action if bounds exceeded.
    pub recommended_action: Option<String>,
}

impl TailRiskCertificate {
    /// Check if the risk is within acceptable bounds.
    #[must_use]
    pub const fn is_acceptable(&self) -> bool {
        !self.exceeds_bounds
    }

    /// Get the margin to threshold (positive = safe, negative = exceeded).
    #[must_use]
    pub fn margin(&self) -> f64 {
        self.threshold - self.observed
    }
}

/// Privacy budget certificate payload - proves privacy budget consumption.
#[derive(Clone, Debug, PartialEq)]
pub struct PrivacyBudgetCertificate {
    /// Budget category (e.g., "aggregation", "export").
    pub category: String,
    /// Budget consumed in this operation.
    pub consumed: f64,
    /// Total budget consumed so far.
    pub total_consumed: f64,
    /// Maximum allowed budget.
    pub budget_limit: f64,
    /// Remaining budget.
    pub remaining: f64,
    /// Whether operation was allowed.
    pub operation_allowed: bool,
    /// Reset timestamp if applicable.
    pub resets_at: Option<String>,
}

impl PrivacyBudgetCertificate {
    /// Check if budget is exhausted.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.remaining <= 0.0
    }

    /// Get utilization percentage (0.0 to 1.0+).
    #[must_use]
    pub fn utilization(&self) -> f64 {
        if self.budget_limit > 0.0 {
            self.total_consumed / self.budget_limit
        } else {
            0.0
        }
    }
}

/// Lifecycle event type for lifecycle certificates.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LifecycleEvent {
    /// Import operation completed.
    Import,
    /// Index was published.
    IndexPublish,
    /// Hook was executed.
    HookExecution,
    /// Backup was created.
    Backup,
    /// Daemon shutdown.
    Shutdown,
    /// Migration completed.
    Migration,
    /// Maintenance job completed.
    Maintenance,
}

impl LifecycleEvent {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::IndexPublish => "index_publish",
            Self::HookExecution => "hook_execution",
            Self::Backup => "backup",
            Self::Shutdown => "shutdown",
            Self::Migration => "migration",
            Self::Maintenance => "maintenance",
        }
    }

    #[must_use]
    pub const fn all() -> [Self; 7] {
        [
            Self::Import,
            Self::IndexPublish,
            Self::HookExecution,
            Self::Backup,
            Self::Shutdown,
            Self::Migration,
            Self::Maintenance,
        ]
    }
}

impl fmt::Display for LifecycleEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error when parsing an invalid lifecycle event string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseLifecycleEventError {
    input: String,
}

impl ParseLifecycleEventError {
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for ParseLifecycleEventError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown lifecycle event `{}`; expected one of import, index_publish, hook_execution, backup, shutdown, migration, maintenance",
            self.input
        )
    }
}

impl std::error::Error for ParseLifecycleEventError {}

impl FromStr for LifecycleEvent {
    type Err = ParseLifecycleEventError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "import" => Ok(Self::Import),
            "index_publish" => Ok(Self::IndexPublish),
            "hook_execution" => Ok(Self::HookExecution),
            "backup" => Ok(Self::Backup),
            "shutdown" => Ok(Self::Shutdown),
            "migration" => Ok(Self::Migration),
            "maintenance" => Ok(Self::Maintenance),
            _ => Err(ParseLifecycleEventError {
                input: input.to_owned(),
            }),
        }
    }
}

/// Lifecycle certificate payload - proves lifecycle event completion.
#[derive(Clone, Debug, PartialEq)]
pub struct LifecycleCertificate {
    /// Type of lifecycle event.
    pub event: LifecycleEvent,
    /// Start timestamp.
    pub started_at: String,
    /// Completion timestamp.
    pub completed_at: String,
    /// Duration in milliseconds.
    pub duration_ms: u64,
    /// Whether the event succeeded.
    pub success: bool,
    /// Items processed (if applicable).
    pub items_processed: Option<u32>,
    /// Error message if failed.
    pub error: Option<String>,
    /// Idempotency key for replay detection.
    pub idempotency_key: Option<String>,
}

impl LifecycleCertificate {
    /// Check if the lifecycle event completed successfully.
    #[must_use]
    pub const fn is_successful(&self) -> bool {
        self.success
    }

    /// Check if this certificate can be used for replay detection.
    #[must_use]
    pub fn has_idempotency_key(&self) -> bool {
        self.idempotency_key.is_some()
    }
}

/// Certificate envelope that wraps any certificate type.
#[derive(Clone, Debug, PartialEq)]
pub struct Certificate {
    /// Unique certificate ID.
    pub id: String,
    /// Certificate kind discriminator.
    pub kind: CertificateKind,
    /// Certificate status.
    pub status: CertificateStatus,
    /// Workspace this certificate belongs to.
    pub workspace_id: String,
    /// When the certificate was issued.
    pub issued_at: String,
    /// When the certificate expires (if applicable).
    pub expires_at: Option<String>,
    /// Hash of the certificate payload for integrity.
    pub payload_hash: String,
    /// Decision-plane tracking metadata (EE-364).
    /// Links the certificate to the policy, decision, and trace that produced it.
    pub decision_metadata: super::decision::DecisionPlaneMetadata,
}

impl Certificate {
    /// Check if the certificate is currently usable.
    #[must_use]
    pub const fn is_usable(&self) -> bool {
        self.status.is_usable()
    }

    /// Check if the certificate has expired based on status.
    #[must_use]
    pub const fn is_expired(&self) -> bool {
        matches!(self.status, CertificateStatus::Expired)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{
        CERTIFICATE_SCHEMA_V1, Certificate, CertificateKind, CertificateStatus,
        CurationCertificate, LifecycleCertificate, LifecycleEvent, PackCertificate,
        ParseCertificateKindError, ParseCertificateStatusError, ParseLifecycleEventError,
        PrivacyBudgetCertificate, TailRiskCertificate,
    };
    use crate::models::DecisionPlaneMetadata;

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
    fn certificate_schema_is_stable() -> TestResult {
        ensure_equal(
            &CERTIFICATE_SCHEMA_V1,
            &"ee.certificate.v1",
            "schema version",
        )
    }

    #[test]
    fn certificate_kind_round_trip_for_every_variant() -> TestResult {
        for kind in CertificateKind::all() {
            let rendered = kind.to_string();
            let parsed = CertificateKind::from_str(&rendered)
                .map_err(|e| format!("kind {kind:?} failed to round-trip: {e}"))?;
            ensure_equal(&parsed, &kind, &format!("round-trip for {kind:?}"))?;
        }
        Ok(())
    }

    #[test]
    fn certificate_kind_rejects_unknown_input() -> TestResult {
        let err = CertificateKind::from_str("unknown_kind");
        ensure(
            matches!(err, Err(ParseCertificateKindError { .. })),
            "should reject unknown kind",
        )
    }

    #[test]
    fn certificate_status_round_trip_for_every_variant() -> TestResult {
        for status in CertificateStatus::all() {
            let rendered = status.to_string();
            let parsed = CertificateStatus::from_str(&rendered)
                .map_err(|e| format!("status {status:?} failed to round-trip: {e}"))?;
            ensure_equal(&parsed, &status, &format!("round-trip for {status:?}"))?;
        }
        Ok(())
    }

    #[test]
    fn certificate_status_rejects_unknown_input() -> TestResult {
        let err = CertificateStatus::from_str("unknown_status");
        ensure(
            matches!(err, Err(ParseCertificateStatusError { .. })),
            "should reject unknown status",
        )
    }

    #[test]
    fn certificate_status_terminal_and_usable() -> TestResult {
        ensure(
            !CertificateStatus::Valid.is_terminal(),
            "valid is not terminal",
        )?;
        ensure(
            !CertificateStatus::Pending.is_terminal(),
            "pending is not terminal",
        )?;
        ensure(
            CertificateStatus::Invalid.is_terminal(),
            "invalid is terminal",
        )?;
        ensure(
            CertificateStatus::Expired.is_terminal(),
            "expired is terminal",
        )?;
        ensure(
            CertificateStatus::Revoked.is_terminal(),
            "revoked is terminal",
        )?;

        ensure(CertificateStatus::Valid.is_usable(), "valid is usable")?;
        ensure(
            !CertificateStatus::Pending.is_usable(),
            "pending is not usable",
        )?;
        ensure(
            !CertificateStatus::Invalid.is_usable(),
            "invalid is not usable",
        )
    }

    #[test]
    fn lifecycle_event_round_trip_for_every_variant() -> TestResult {
        for event in LifecycleEvent::all() {
            let rendered = event.to_string();
            let parsed = LifecycleEvent::from_str(&rendered)
                .map_err(|e| format!("event {event:?} failed to round-trip: {e}"))?;
            ensure_equal(&parsed, &event, &format!("round-trip for {event:?}"))?;
        }
        Ok(())
    }

    #[test]
    fn lifecycle_event_rejects_unknown_input() -> TestResult {
        let err = LifecycleEvent::from_str("unknown_event");
        ensure(
            matches!(err, Err(ParseLifecycleEventError { .. })),
            "should reject unknown event",
        )
    }

    #[test]
    fn pack_certificate_validity_checks() -> TestResult {
        let valid = PackCertificate {
            pack_hash: "hash123".to_string(),
            query: "test query".to_string(),
            budget_used: 100,
            budget_limit: 200,
            item_count: 5,
            omitted_count: 2,
            quotas_satisfied: true,
            redundancy_applied: true,
            provenance_complete: true,
        };
        ensure(valid.is_valid(), "should be valid")?;

        let over_budget = PackCertificate {
            budget_used: 300,
            budget_limit: 200,
            ..valid.clone()
        };
        ensure(!over_budget.is_valid(), "over budget should be invalid")?;

        let no_provenance = PackCertificate {
            provenance_complete: false,
            ..valid.clone()
        };
        ensure(
            !no_provenance.is_valid(),
            "missing provenance should be invalid",
        )?;

        let quotas_failed = PackCertificate {
            quotas_satisfied: false,
            ..valid
        };
        ensure(!quotas_failed.is_valid(), "failed quotas should be invalid")
    }

    #[test]
    fn curation_certificate_threshold_checks() -> TestResult {
        let cert = CurationCertificate {
            candidate_id: "curate_123".to_string(),
            action_type: "promote".to_string(),
            confidence: 0.8,
            threshold: 0.7,
            evidence_count: 3,
            feedback_count: 5,
            requires_review: false,
            reversible: true,
        };
        ensure(cert.meets_threshold(), "0.8 >= 0.7 should meet threshold")?;
        ensure(cert.has_evidence(), "should have evidence")?;

        let below_threshold = CurationCertificate {
            confidence: 0.5,
            ..cert.clone()
        };
        ensure(
            !below_threshold.meets_threshold(),
            "0.5 < 0.7 should not meet threshold",
        )?;

        let no_evidence = CurationCertificate {
            evidence_count: 0,
            feedback_count: 0,
            ..cert
        };
        ensure(!no_evidence.has_evidence(), "should not have evidence")
    }

    #[test]
    fn tail_risk_certificate_bounds_checks() -> TestResult {
        let acceptable = TailRiskCertificate {
            metric: "latency_p99".to_string(),
            observed: 50.0,
            threshold: 100.0,
            confidence_level: 0.95,
            upper_bound: 75.0,
            exceeds_bounds: false,
            recommended_action: None,
        };
        ensure(acceptable.is_acceptable(), "should be acceptable")?;
        ensure(acceptable.margin() > 0.0, "margin should be positive")?;

        let exceeded = TailRiskCertificate {
            observed: 120.0,
            exceeds_bounds: true,
            recommended_action: Some("scale up".to_string()),
            ..acceptable
        };
        ensure(!exceeded.is_acceptable(), "should not be acceptable")?;
        ensure(exceeded.margin() < 0.0, "margin should be negative")
    }

    #[test]
    fn privacy_budget_certificate_utilization() -> TestResult {
        let cert = PrivacyBudgetCertificate {
            category: "aggregation".to_string(),
            consumed: 0.1,
            total_consumed: 0.5,
            budget_limit: 1.0,
            remaining: 0.5,
            operation_allowed: true,
            resets_at: None,
        };
        ensure(!cert.is_exhausted(), "should not be exhausted")?;
        ensure_equal(&cert.utilization(), &0.5, "utilization should be 50%")?;

        let exhausted = PrivacyBudgetCertificate {
            remaining: 0.0,
            total_consumed: 1.0,
            operation_allowed: false,
            ..cert
        };
        ensure(exhausted.is_exhausted(), "should be exhausted")?;
        ensure_equal(&exhausted.utilization(), &1.0, "utilization should be 100%")
    }

    #[test]
    fn lifecycle_certificate_success_checks() -> TestResult {
        let successful = LifecycleCertificate {
            event: LifecycleEvent::Import,
            started_at: "2026-04-29T12:00:00Z".to_string(),
            completed_at: "2026-04-29T12:01:00Z".to_string(),
            duration_ms: 60000,
            success: true,
            items_processed: Some(100),
            error: None,
            idempotency_key: Some("import-abc123".to_string()),
        };
        ensure(successful.is_successful(), "should be successful")?;
        ensure(
            successful.has_idempotency_key(),
            "should have idempotency key",
        )?;

        let failed = LifecycleCertificate {
            success: false,
            error: Some("connection timeout".to_string()),
            ..successful.clone()
        };
        ensure(!failed.is_successful(), "should not be successful")?;

        let no_key = LifecycleCertificate {
            idempotency_key: None,
            ..successful
        };
        ensure(
            !no_key.has_idempotency_key(),
            "should not have idempotency key",
        )
    }

    #[test]
    fn certificate_envelope_usability() -> TestResult {
        let valid = Certificate {
            id: "cert_123".to_string(),
            kind: CertificateKind::Pack,
            status: CertificateStatus::Valid,
            workspace_id: "wsp_456".to_string(),
            issued_at: "2026-04-29T12:00:00Z".to_string(),
            expires_at: None,
            payload_hash: "hash789".to_string(),
            decision_metadata: DecisionPlaneMetadata::empty(),
        };
        ensure(valid.is_usable(), "valid cert should be usable")?;
        ensure(!valid.is_expired(), "valid cert should not be expired")?;

        let expired = Certificate {
            status: CertificateStatus::Expired,
            ..valid
        };
        ensure(!expired.is_usable(), "expired cert should not be usable")?;
        ensure(
            expired.is_expired(),
            "expired cert should report as expired",
        )
    }
}
