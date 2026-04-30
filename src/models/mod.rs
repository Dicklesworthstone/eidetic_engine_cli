use std::process::ExitCode;

pub mod certificate;
pub mod degradation;
pub mod error_codes;
pub mod id;
pub mod memory;
pub mod provenance;
pub mod revision;
pub mod rule;
pub mod timing;
pub mod trust;

pub use certificate::{
    CERTIFICATE_SCHEMA_V1, Certificate, CertificateKind, CertificateStatus, CurationCertificate,
    LifecycleCertificate, LifecycleEvent, PackCertificate, ParseCertificateKindError,
    ParseCertificateStatusError, ParseLifecycleEventError, PrivacyBudgetCertificate,
    TailRiskCertificate,
};
pub use degradation::{
    ALL_DEGRADATION_CODES, ActiveDegradation, DegradationCode, DegradationSeverity,
    DegradedSubsystem,
};
pub use id::{
    AuditId, BackupId, CandidateId, ClaimId, DemoId, EXECUTABLE_ID_SCHEMA_V1, EvidenceId,
    ExecutableIdKind, Id, IdJsonSchema, IdKind, MemoryId, MemoryLinkId, PackId,
    ParseExecutableIdKindError, ParseIdError, PolicyId, RuleId, SessionId, TraceId, WorkspaceId,
    executable_id_schema_catalog_json, executable_id_schemas,
};
pub use memory::{
    Confidence, Importance, KNOWN_MEMORY_KINDS, MAX_CONTENT_BYTES, MAX_TAG_BYTES, MemoryContent,
    MemoryKind, MemoryLevel, MemoryValidationError, Tag, UnitScore, Utility,
};
pub use provenance::{LineSpan, ProvenanceUri, ProvenanceUriError};
pub use revision::{
    IdempotencyKey, IdempotencyKeyError, LEGAL_HOLD_ID_LEN, LEGAL_HOLD_PREFIX, LegalHold,
    LegalHoldId, REVISION_GROUP_ID_LEN, REVISION_GROUP_PREFIX, RevisionGroupId, RevisionIdError,
    RevisionMeta, SupersessionLink, SupersessionReason,
};
pub use rule::{ParseRuleMaturityError, ParseRuleScopeError, RuleMaturity, RuleScope};
pub use timing::{DiagnosticTiming, TimingCapture, TimingPhase};
pub use trust::{ParseTrustClassError, TrustClass};

// ============================================================================
// Public JSON Contract Schema Constants
//
// These constants define the schema identifiers for all public JSON contracts.
// They MUST be used instead of inline string literals to ensure consistency
// and enable schema drift detection.
// ============================================================================

/// Response envelope schema for successful command output.
pub const RESPONSE_SCHEMA_V1: &str = "ee.response.v1";

/// Error envelope schema for failed command output.
pub const ERROR_SCHEMA_V1: &str = "ee.error.v1";

/// Schema for CASS import reports (`ee import cass`).
pub const IMPORT_CASS_SCHEMA_V1: &str = "ee.import.cass.v1";

/// Schema for import ledger entries.
pub const IMPORT_LEDGER_SCHEMA_V1: &str = "ee.import_ledger.v1";

/// Schema for CASS-specific import ledger entries.
pub const IMPORT_LEDGER_CASS_SCHEMA_V1: &str = "ee.import_ledger.cass.v1";

/// Schema for imported CASS session metadata.
pub const CASS_SESSION_SCHEMA_V1: &str = "ee.cass_session.v1";

/// Schema for CASS evidence span entries.
pub const CASS_EVIDENCE_SPAN_SCHEMA_V1: &str = "ee.cass_evidence_span.v1";

/// Schema for search module readiness.
pub const SEARCH_MODULE_SCHEMA_V1: &str = "ee.search.module.v1";

/// Schema for canonical search documents.
pub const SEARCH_DOCUMENT_SCHEMA_V1: &str = "ee.search.document.v1";

/// Schema for graph module readiness.
pub const GRAPH_MODULE_SCHEMA_V1: &str = "ee.graph.module.v1";

/// Schema for evaluation fixtures.
pub const EVAL_FIXTURE_SCHEMA_V1: &str = "ee.eval_fixture.v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DomainError {
    Usage {
        message: String,
        repair: Option<String>,
    },
    Configuration {
        message: String,
        repair: Option<String>,
    },
    Storage {
        message: String,
        repair: Option<String>,
    },
    SearchIndex {
        message: String,
        repair: Option<String>,
    },
    Import {
        message: String,
        repair: Option<String>,
    },
    NotFound {
        resource: String,
        id: String,
        repair: Option<String>,
    },
    UnsatisfiedDegradedMode {
        message: String,
        repair: Option<String>,
    },
    PolicyDenied {
        message: String,
        repair: Option<String>,
    },
    MigrationRequired {
        message: String,
        repair: Option<String>,
    },
}

impl DomainError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Usage { .. } => "usage",
            Self::Configuration { .. } => "configuration",
            Self::Storage { .. } => "storage",
            Self::SearchIndex { .. } => "search_index",
            Self::Import { .. } => "import",
            Self::NotFound { .. } => "not_found",
            Self::UnsatisfiedDegradedMode { .. } => "unsatisfied_degraded_mode",
            Self::PolicyDenied { .. } => "policy_denied",
            Self::MigrationRequired { .. } => "migration_required",
        }
    }

    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Usage { message, .. }
            | Self::Configuration { message, .. }
            | Self::Storage { message, .. }
            | Self::SearchIndex { message, .. }
            | Self::Import { message, .. }
            | Self::UnsatisfiedDegradedMode { message, .. }
            | Self::PolicyDenied { message, .. }
            | Self::MigrationRequired { message, .. } => message.clone(),
            Self::NotFound { resource, id, .. } => {
                format!("{resource} not found: {id}")
            }
        }
    }

    #[must_use]
    pub fn repair(&self) -> Option<&str> {
        match self {
            Self::Usage { repair, .. }
            | Self::Configuration { repair, .. }
            | Self::Storage { repair, .. }
            | Self::SearchIndex { repair, .. }
            | Self::Import { repair, .. }
            | Self::NotFound { repair, .. }
            | Self::UnsatisfiedDegradedMode { repair, .. }
            | Self::PolicyDenied { repair, .. }
            | Self::MigrationRequired { repair, .. } => repair.as_deref(),
        }
    }

    #[must_use]
    pub const fn exit_code(&self) -> ProcessExitCode {
        match self {
            Self::Usage { .. } => ProcessExitCode::Usage,
            Self::Configuration { .. } => ProcessExitCode::Configuration,
            Self::Storage { .. } => ProcessExitCode::Storage,
            Self::SearchIndex { .. } => ProcessExitCode::SearchIndex,
            Self::Import { .. } => ProcessExitCode::Import,
            Self::NotFound { .. } => ProcessExitCode::NotFound,
            Self::UnsatisfiedDegradedMode { .. } => ProcessExitCode::UnsatisfiedDegradedMode,
            Self::PolicyDenied { .. } => ProcessExitCode::PolicyDenied,
            Self::MigrationRequired { .. } => ProcessExitCode::MigrationRequired,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProcessExitCode {
    Success = 0,
    Usage = 1,
    Configuration = 2,
    Storage = 3,
    SearchIndex = 4,
    Import = 5,
    UnsatisfiedDegradedMode = 6,
    PolicyDenied = 7,
    MigrationRequired = 8,
    NotFound = 9,
}

impl From<ProcessExitCode> for ExitCode {
    fn from(value: ProcessExitCode) -> Self {
        Self::from(value as u8)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityStatus {
    Ready,
    Pending,
    Degraded,
    Unimplemented,
}

impl CapabilityStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Pending => "pending",
            Self::Degraded => "degraded",
            Self::Unimplemented => "unimplemented",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CapabilityStatus, DomainError, ProcessExitCode};

    type TestResult = Result<(), String>;

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
    fn exit_codes_match_project_contract() {
        assert_eq!(ProcessExitCode::Success as u8, 0);
        assert_eq!(ProcessExitCode::Usage as u8, 1);
        assert_eq!(ProcessExitCode::MigrationRequired as u8, 8);
    }

    #[test]
    fn capability_status_strings_are_stable() {
        assert_eq!(CapabilityStatus::Ready.as_str(), "ready");
        assert_eq!(CapabilityStatus::Pending.as_str(), "pending");
        assert_eq!(CapabilityStatus::Degraded.as_str(), "degraded");
        assert_eq!(CapabilityStatus::Unimplemented.as_str(), "unimplemented");
    }

    #[test]
    fn domain_error_codes_are_stable() -> TestResult {
        let cases = [
            (
                DomainError::Usage {
                    message: String::new(),
                    repair: None,
                },
                "usage",
                ProcessExitCode::Usage,
            ),
            (
                DomainError::Configuration {
                    message: String::new(),
                    repair: None,
                },
                "configuration",
                ProcessExitCode::Configuration,
            ),
            (
                DomainError::Storage {
                    message: String::new(),
                    repair: None,
                },
                "storage",
                ProcessExitCode::Storage,
            ),
            (
                DomainError::SearchIndex {
                    message: String::new(),
                    repair: None,
                },
                "search_index",
                ProcessExitCode::SearchIndex,
            ),
            (
                DomainError::Import {
                    message: String::new(),
                    repair: None,
                },
                "import",
                ProcessExitCode::Import,
            ),
            (
                DomainError::UnsatisfiedDegradedMode {
                    message: String::new(),
                    repair: None,
                },
                "unsatisfied_degraded_mode",
                ProcessExitCode::UnsatisfiedDegradedMode,
            ),
            (
                DomainError::PolicyDenied {
                    message: String::new(),
                    repair: None,
                },
                "policy_denied",
                ProcessExitCode::PolicyDenied,
            ),
            (
                DomainError::MigrationRequired {
                    message: String::new(),
                    repair: None,
                },
                "migration_required",
                ProcessExitCode::MigrationRequired,
            ),
        ];
        for (error, expected_code, expected_exit) in cases {
            ensure_equal(&error.code(), &expected_code, "code")?;
            ensure_equal(&error.exit_code(), &expected_exit, "exit_code")?;
        }
        Ok(())
    }

    #[test]
    fn domain_error_message_and_repair_accessors() -> TestResult {
        let err = DomainError::Storage {
            message: "Database locked".to_string(),
            repair: Some("ee db unlock".to_string()),
        };
        ensure_equal(&err.message(), &"Database locked".to_string(), "message")?;
        ensure_equal(&err.repair(), &Some("ee db unlock"), "repair")
    }
}
