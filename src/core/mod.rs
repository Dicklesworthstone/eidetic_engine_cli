use std::future::Future;

pub mod agent_detect;
pub mod agent_docs;
pub mod budget;
pub mod capabilities;
pub mod check;
pub mod claims;
pub mod context;
pub mod degraded_honesty;
pub mod doctor;
pub mod effect;
pub mod health;
pub mod index;
pub mod init;
pub mod legacy_import;
pub mod memory;
pub mod outcome;
pub mod quarantine;
pub mod search;
pub mod situation;
pub mod status;
pub mod streams;
pub mod verify;
pub mod why;

pub use budget::{BudgetDimension, BudgetExceeded, BudgetSnapshot, RequestBudget};
pub use context::{AccessLevel, CapabilitySet, CommandContext};
pub use outcome::{
    CliCancelReason, CliOutcomeClass, CliOutcomeSummary, EXIT_CANCELLED, EXIT_PANICKED,
    OutcomeFeedbackSummary, OutcomeRecordOptions, OutcomeRecordReport, OutcomeRecordStatus,
    outcome_class, outcome_exit_code, record_outcome,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuildInfo {
    pub package: &'static str,
    pub version: &'static str,
}

#[must_use]
pub const fn build_info() -> BuildInfo {
    BuildInfo {
        package: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
    }
}

pub const CLI_RUNTIME_WORKERS: usize = 1;

pub type RuntimeResult<T> = Result<T, Box<asupersync::Error>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeProfile {
    CurrentThread,
}

impl RuntimeProfile {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CurrentThread => "current_thread",
        }
    }

    #[must_use]
    pub const fn worker_threads(self) -> usize {
        match self {
            Self::CurrentThread => CLI_RUNTIME_WORKERS,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeStatus {
    pub engine: &'static str,
    pub profile: RuntimeProfile,
    pub async_boundary: &'static str,
}

impl RuntimeStatus {
    #[must_use]
    pub const fn worker_threads(self) -> usize {
        self.profile.worker_threads()
    }
}

#[must_use]
pub const fn runtime_status() -> RuntimeStatus {
    RuntimeStatus {
        engine: "asupersync",
        profile: RuntimeProfile::CurrentThread,
        async_boundary: "core",
    }
}

pub fn build_cli_runtime() -> RuntimeResult<asupersync::runtime::Runtime> {
    asupersync::runtime::RuntimeBuilder::current_thread()
        .thread_name_prefix("ee-runtime")
        .build()
        .map_err(Box::new)
}

pub fn run_cli_future<F, T>(future: F) -> RuntimeResult<T>
where
    F: Future<Output = T>,
{
    let runtime = build_cli_runtime()?;
    Ok(runtime.block_on(future))
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use asupersync::{LabConfig, LabRuntime};

    use super::{RuntimeProfile, build_info, run_cli_future, runtime_status};

    type TestResult = Result<(), String>;

    fn ensure(condition: bool, message: impl Into<String>) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.into())
        }
    }

    fn ensure_equal<T>(actual: &T, expected: &T, context: &str) -> TestResult
    where
        T: Debug + PartialEq,
    {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{context}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn build_info_uses_cargo_metadata() -> TestResult {
        let info = build_info();
        ensure_equal(
            &info.package,
            &"ee",
            "package name must match Cargo metadata",
        )?;
        ensure(
            !info.version.is_empty(),
            "package version must not be empty",
        )
    }

    #[test]
    fn runtime_status_reports_asupersync_current_thread_bootstrap() -> TestResult {
        let status = runtime_status();
        ensure_equal(&status.engine, &"asupersync", "runtime engine")?;
        ensure_equal(
            &status.profile,
            &RuntimeProfile::CurrentThread,
            "runtime profile",
        )?;
        ensure_equal(
            &status.profile.as_str(),
            &"current_thread",
            "runtime profile label",
        )?;
        ensure_equal(&status.worker_threads(), &1, "runtime worker count")?;
        ensure_equal(&status.async_boundary, &"core", "runtime async boundary")
    }

    #[test]
    fn cli_runtime_executes_future_to_completion() -> TestResult {
        let result = run_cli_future(async { 42_u8 })
            .map_err(|error| format!("failed to build Asupersync runtime: {error}"))?;

        ensure_equal(&result, &42, "runtime future result")
    }

    #[test]
    fn lab_runtime_seed_is_deterministic_for_runtime_contract_tests() -> TestResult {
        let first = LabRuntime::new(LabConfig::new(7));
        let second = LabRuntime::new(LabConfig::new(7));

        ensure_equal(&first.now(), &second.now(), "lab runtime start time")?;
        ensure_equal(&first.steps(), &second.steps(), "lab runtime step count")
    }
}
