use std::process::ExitCode;

pub mod id;
pub mod memory;

pub use id::{
    AuditId, BackupId, CandidateId, EvidenceId, Id, IdKind, MemoryId, PackId, ParseIdError, RuleId,
    SessionId, WorkspaceId,
};
pub use memory::{
    Confidence, Importance, KNOWN_MEMORY_KINDS, MAX_CONTENT_BYTES, MAX_TAG_BYTES, MemoryContent,
    MemoryKind, MemoryLevel, MemoryValidationError, Tag, UnitScore, Utility,
};

pub const RESPONSE_SCHEMA_V1: &str = "ee.response.v1";
pub const ERROR_SCHEMA_V1: &str = "ee.error.v1";

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
    use super::{CapabilityStatus, ProcessExitCode};

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
}
