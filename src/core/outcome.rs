//! Outcome to CLI boundary mapping (EE-009).
//!
//! Maps Asupersync's `Outcome<T, E>` to CLI exit codes and error responses.
//!
//! # Outcome Severity Lattice
//!
//! Asupersync defines a severity lattice where worse outcomes dominate:
//! `Ok < Err < Cancelled < Panicked`
//!
//! This module maps that lattice to CLI exit codes:
//! - `Ok(T)` → exit 0 (success)
//! - `Err(DomainError)` → exit 1-8 (domain-specific errors)
//! - `Cancelled` → exit 130 (SIGINT convention)
//! - `Panicked` → exit 101 (Rust panic convention)
//!
//! # Usage
//!
//! ```ignore
//! use ee::core::outcome::{CliOutcome, outcome_exit_code};
//! use asupersync::Outcome;
//!
//! let outcome: Outcome<(), DomainError> = Outcome::ok(());
//! let exit_code = outcome_exit_code(&outcome);
//! ```

use asupersync::Outcome;
use asupersync::types::{CancelKind, CancelReason, PanicPayload};

use crate::models::{DomainError, ProcessExitCode};

/// Exit code for cancelled operations (SIGINT convention).
pub const EXIT_CANCELLED: u8 = 130;

/// Exit code for panicked operations (Rust panic convention).
pub const EXIT_PANICKED: u8 = 101;

/// CLI outcome classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliOutcomeClass {
    /// Operation succeeded.
    Success,
    /// Domain-level error (usage, config, storage, etc.).
    DomainError,
    /// Operation was cancelled (budget exhausted, timeout, signal).
    Cancelled,
    /// Operation panicked.
    Panicked,
}

impl CliOutcomeClass {
    /// Stable string form for JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::DomainError => "domain_error",
            Self::Cancelled => "cancelled",
            Self::Panicked => "panicked",
        }
    }

    /// Whether this outcome class is terminal (no further progress possible).
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Success)
    }
}

/// Cancel reason classification for CLI output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliCancelReason {
    /// Budget exhausted (time, polls, cost).
    BudgetExhausted,
    /// Explicit cancellation requested.
    UserRequested,
    /// Timeout or deadline exceeded.
    Timeout,
    /// Parent scope was cancelled.
    ParentCancelled,
    /// Shutdown requested.
    Shutdown,
    /// Other cancellation reason.
    Other,
}

impl CliCancelReason {
    /// Stable string form for JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BudgetExhausted => "budget_exhausted",
            Self::UserRequested => "user_requested",
            Self::Timeout => "timeout",
            Self::ParentCancelled => "parent_cancelled",
            Self::Shutdown => "shutdown",
            Self::Other => "other",
        }
    }
}

impl From<&CancelReason> for CliCancelReason {
    fn from(reason: &CancelReason) -> Self {
        match reason.kind {
            CancelKind::PollQuota | CancelKind::CostBudget | CancelKind::Deadline => {
                Self::BudgetExhausted
            }
            CancelKind::User => Self::UserRequested,
            CancelKind::Timeout => Self::Timeout,
            CancelKind::ParentCancelled => Self::ParentCancelled,
            CancelKind::Shutdown => Self::Shutdown,
            CancelKind::FailFast
            | CancelKind::RaceLost
            | CancelKind::ResourceUnavailable
            | CancelKind::LinkedExit => Self::Other,
        }
    }
}

/// Get the exit code for an Outcome.
///
/// Maps the Outcome severity lattice to Unix exit codes:
/// - `Ok` → 0
/// - `Err(DomainError)` → domain-specific exit code (1-8)
/// - `Cancelled` → 130 (SIGINT convention)
/// - `Panicked` → 101 (Rust panic convention)
#[must_use]
pub fn outcome_exit_code<T>(outcome: &Outcome<T, DomainError>) -> u8 {
    match outcome {
        Outcome::Ok(_) => ProcessExitCode::Success as u8,
        Outcome::Err(e) => e.exit_code() as u8,
        Outcome::Cancelled(_) => EXIT_CANCELLED,
        Outcome::Panicked(_) => EXIT_PANICKED,
    }
}

/// Get the outcome class for an Outcome.
#[must_use]
pub fn outcome_class<T, E>(outcome: &Outcome<T, E>) -> CliOutcomeClass {
    match outcome {
        Outcome::Ok(_) => CliOutcomeClass::Success,
        Outcome::Err(_) => CliOutcomeClass::DomainError,
        Outcome::Cancelled(_) => CliOutcomeClass::Cancelled,
        Outcome::Panicked(_) => CliOutcomeClass::Panicked,
    }
}

/// Extract a human-readable message from a cancelled outcome.
#[must_use]
pub fn cancel_message(reason: &CancelReason) -> String {
    if let Some(msg) = &reason.message {
        return msg.clone();
    }
    match reason.kind {
        CancelKind::User => "Cancellation requested.".to_string(),
        CancelKind::Timeout => "Operation timed out.".to_string(),
        CancelKind::Deadline => "Deadline exceeded.".to_string(),
        CancelKind::PollQuota => "Poll budget exhausted.".to_string(),
        CancelKind::CostBudget => "Cost budget exhausted.".to_string(),
        CancelKind::FailFast => "Sibling operation failed.".to_string(),
        CancelKind::RaceLost => "Lost race to another operation.".to_string(),
        CancelKind::ParentCancelled => "Parent operation was cancelled.".to_string(),
        CancelKind::ResourceUnavailable => "Resource unavailable.".to_string(),
        CancelKind::Shutdown => "Runtime shutdown.".to_string(),
        CancelKind::LinkedExit => "Linked task exited.".to_string(),
    }
}

/// Extract a human-readable message from a panicked outcome.
#[must_use]
pub fn panic_message(payload: &PanicPayload) -> String {
    payload.message().to_string()
}

/// A CLI-ready outcome summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliOutcomeSummary {
    pub class: CliOutcomeClass,
    pub exit_code: u8,
    pub message: Option<String>,
    pub cancel_reason: Option<CliCancelReason>,
}

impl CliOutcomeSummary {
    /// Create a summary from an Outcome.
    #[must_use]
    pub fn from_outcome<T>(outcome: &Outcome<T, DomainError>) -> Self {
        match outcome {
            Outcome::Ok(_) => Self {
                class: CliOutcomeClass::Success,
                exit_code: 0,
                message: None,
                cancel_reason: None,
            },
            Outcome::Err(e) => Self {
                class: CliOutcomeClass::DomainError,
                exit_code: e.exit_code() as u8,
                message: Some(e.message().to_string()),
                cancel_reason: None,
            },
            Outcome::Cancelled(reason) => Self {
                class: CliOutcomeClass::Cancelled,
                exit_code: EXIT_CANCELLED,
                message: Some(cancel_message(reason)),
                cancel_reason: Some(CliCancelReason::from(reason)),
            },
            Outcome::Panicked(payload) => Self {
                class: CliOutcomeClass::Panicked,
                exit_code: EXIT_PANICKED,
                message: Some(panic_message(payload)),
                cancel_reason: None,
            },
        }
    }

    /// Whether this outcome represents success.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        matches!(self.class, CliOutcomeClass::Success)
    }
}

#[cfg(test)]
mod tests {
    use asupersync::Outcome;
    use asupersync::types::{CancelKind, CancelReason, PanicPayload, RegionId, Time};

    use super::{
        CliCancelReason, CliOutcomeClass, CliOutcomeSummary, EXIT_CANCELLED, EXIT_PANICKED,
        outcome_class, outcome_exit_code,
    };
    use crate::models::{DomainError, ProcessExitCode};

    type TestResult = Result<(), String>;

    fn ensure_equal<T: std::fmt::Debug + PartialEq>(
        actual: &T,
        expected: &T,
        context: &str,
    ) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn test_cancel_reason(kind: CancelKind) -> CancelReason {
        CancelReason::with_origin(kind, RegionId::testing_default(), Time::ZERO)
    }

    #[test]
    fn exit_code_constants_follow_conventions() -> TestResult {
        ensure_equal(&EXIT_CANCELLED, &130, "SIGINT convention")?;
        ensure_equal(&EXIT_PANICKED, &101, "Rust panic convention")
    }

    #[test]
    fn outcome_class_strings_are_stable() -> TestResult {
        ensure_equal(&CliOutcomeClass::Success.as_str(), &"success", "success")?;
        ensure_equal(
            &CliOutcomeClass::DomainError.as_str(),
            &"domain_error",
            "domain_error",
        )?;
        ensure_equal(
            &CliOutcomeClass::Cancelled.as_str(),
            &"cancelled",
            "cancelled",
        )?;
        ensure_equal(&CliOutcomeClass::Panicked.as_str(), &"panicked", "panicked")
    }

    #[test]
    fn cancel_reason_strings_are_stable() -> TestResult {
        ensure_equal(
            &CliCancelReason::BudgetExhausted.as_str(),
            &"budget_exhausted",
            "budget",
        )?;
        ensure_equal(
            &CliCancelReason::UserRequested.as_str(),
            &"user_requested",
            "user",
        )?;
        ensure_equal(&CliCancelReason::Timeout.as_str(), &"timeout", "timeout")?;
        ensure_equal(
            &CliCancelReason::ParentCancelled.as_str(),
            &"parent_cancelled",
            "parent",
        )?;
        ensure_equal(&CliCancelReason::Shutdown.as_str(), &"shutdown", "shutdown")?;
        ensure_equal(&CliCancelReason::Other.as_str(), &"other", "other")
    }

    #[test]
    fn outcome_ok_maps_to_exit_zero() -> TestResult {
        let outcome: Outcome<(), DomainError> = Outcome::ok(());
        ensure_equal(&outcome_exit_code(&outcome), &0, "ok exit code")?;
        ensure_equal(
            &outcome_class(&outcome),
            &CliOutcomeClass::Success,
            "ok class",
        )
    }

    #[test]
    fn outcome_err_maps_to_domain_exit_code() -> TestResult {
        let error = DomainError::Usage {
            message: "test".to_string(),
            repair: None,
        };
        let outcome: Outcome<(), DomainError> = Outcome::err(error);
        ensure_equal(
            &outcome_exit_code(&outcome),
            &(ProcessExitCode::Usage as u8),
            "usage exit code",
        )?;
        ensure_equal(
            &outcome_class(&outcome),
            &CliOutcomeClass::DomainError,
            "err class",
        )
    }

    #[test]
    fn outcome_cancelled_maps_to_130() -> TestResult {
        let reason = test_cancel_reason(CancelKind::User);
        let outcome: Outcome<(), DomainError> = Outcome::Cancelled(reason);
        ensure_equal(&outcome_exit_code(&outcome), &130, "cancelled exit code")?;
        ensure_equal(
            &outcome_class(&outcome),
            &CliOutcomeClass::Cancelled,
            "cancelled class",
        )
    }

    #[test]
    fn outcome_panicked_maps_to_101() -> TestResult {
        let payload = PanicPayload::new("test panic");
        let outcome: Outcome<(), DomainError> = Outcome::Panicked(payload);
        ensure_equal(&outcome_exit_code(&outcome), &101, "panicked exit code")?;
        ensure_equal(
            &outcome_class(&outcome),
            &CliOutcomeClass::Panicked,
            "panicked class",
        )
    }

    #[test]
    fn cli_outcome_summary_from_ok() -> TestResult {
        let outcome: Outcome<i32, DomainError> = Outcome::ok(42);
        let summary = CliOutcomeSummary::from_outcome(&outcome);
        ensure_equal(&summary.class, &CliOutcomeClass::Success, "class")?;
        ensure_equal(&summary.exit_code, &0, "exit code")?;
        ensure_equal(&summary.is_success(), &true, "is_success")
    }

    #[test]
    fn cli_outcome_summary_from_err() -> TestResult {
        let error = DomainError::Storage {
            message: "Database locked".to_string(),
            repair: Some("ee db unlock".to_string()),
        };
        let outcome: Outcome<(), DomainError> = Outcome::err(error);
        let summary = CliOutcomeSummary::from_outcome(&outcome);
        ensure_equal(&summary.class, &CliOutcomeClass::DomainError, "class")?;
        ensure_equal(
            &summary.exit_code,
            &(ProcessExitCode::Storage as u8),
            "exit code",
        )?;
        ensure_equal(
            &summary.message,
            &Some("Database locked".to_string()),
            "message",
        )?;
        ensure_equal(&summary.is_success(), &false, "is_success")
    }

    #[test]
    fn cli_outcome_summary_from_cancelled() -> TestResult {
        let reason = test_cancel_reason(CancelKind::PollQuota);
        let outcome: Outcome<(), DomainError> = Outcome::Cancelled(reason);
        let summary = CliOutcomeSummary::from_outcome(&outcome);
        ensure_equal(&summary.class, &CliOutcomeClass::Cancelled, "class")?;
        ensure_equal(&summary.exit_code, &130, "exit code")?;
        ensure_equal(
            &summary.cancel_reason,
            &Some(CliCancelReason::BudgetExhausted),
            "cancel reason",
        )?;
        ensure_equal(&summary.is_success(), &false, "is_success")
    }

    #[test]
    fn outcome_class_is_terminal_classification() -> TestResult {
        ensure_equal(&CliOutcomeClass::Success.is_terminal(), &false, "success")?;
        ensure_equal(
            &CliOutcomeClass::DomainError.is_terminal(),
            &true,
            "domain error",
        )?;
        ensure_equal(
            &CliOutcomeClass::Cancelled.is_terminal(),
            &true,
            "cancelled",
        )?;
        ensure_equal(&CliOutcomeClass::Panicked.is_terminal(), &true, "panicked")
    }

    #[test]
    fn cancel_kind_to_cli_reason_mapping() -> TestResult {
        let cases = [
            (CancelKind::PollQuota, CliCancelReason::BudgetExhausted),
            (CancelKind::CostBudget, CliCancelReason::BudgetExhausted),
            (CancelKind::Deadline, CliCancelReason::BudgetExhausted),
            (CancelKind::User, CliCancelReason::UserRequested),
            (CancelKind::Timeout, CliCancelReason::Timeout),
            (
                CancelKind::ParentCancelled,
                CliCancelReason::ParentCancelled,
            ),
            (CancelKind::Shutdown, CliCancelReason::Shutdown),
            (CancelKind::FailFast, CliCancelReason::Other),
            (CancelKind::RaceLost, CliCancelReason::Other),
        ];

        for (kind, expected) in cases {
            let reason = test_cancel_reason(kind);
            let cli_reason = CliCancelReason::from(&reason);
            ensure_equal(&cli_reason, &expected, &format!("{kind:?}"))?;
        }
        Ok(())
    }
}
