use std::process::ExitCode;

pub mod certificate;
pub mod error_codes;
pub mod id;
pub mod memory;
pub mod provenance;
pub mod rule;
pub mod trust;

pub use certificate::{
    CERTIFICATE_SCHEMA_V1, Certificate, CertificateKind, CertificateStatus,
    CurationCertificate, LifecycleCertificate, LifecycleEvent, PackCertificate,
    ParseCertificateKindError, ParseCertificateStatusError, ParseLifecycleEventError,
    PrivacyBudgetCertificate, TailRiskCertificate,
};
pub use id::{
    AuditId, BackupId, CandidateId, EvidenceId, Id, IdKind, MemoryId, PackId, ParseIdError, RuleId,
    SessionId, WorkspaceId,
};
pub use memory::{
    Confidence, Importance, KNOWN_MEMORY_KINDS, MAX_CONTENT_BYTES, MAX_TAG_BYTES, MemoryContent,
    MemoryKind, MemoryLevel, MemoryValidationError, Tag, UnitScore, Utility,
};
pub use provenance::{LineSpan, ProvenanceUri, ProvenanceUriError};
pub use rule::{ParseRuleMaturityError, ParseRuleScopeError, RuleMaturity, RuleScope};
pub use trust::{ParseTrustClassError, TrustClass};

pub const RESPONSE_SCHEMA_V1: &str = "ee.response.v1";
pub const ERROR_SCHEMA_V1: &str = "ee.error.v1";

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
            Self::UnsatisfiedDegradedMode { .. } => "unsatisfied_degraded_mode",
            Self::PolicyDenied { .. } => "policy_denied",
            Self::MigrationRequired { .. } => "migration_required",
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Usage { message, .. }
            | Self::Configuration { message, .. }
            | Self::Storage { message, .. }
            | Self::SearchIndex { message, .. }
            | Self::Import { message, .. }
            | Self::UnsatisfiedDegradedMode { message, .. }
            | Self::PolicyDenied { message, .. }
            | Self::MigrationRequired { message, .. } => message,
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
        ensure_equal(&err.message(), &"Database locked", "message")?;
        ensure_equal(&err.repair(), &Some("ee db unlock"), "repair")
    }
}
