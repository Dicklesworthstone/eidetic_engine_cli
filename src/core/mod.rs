use std::future::Future;

use crate::models::{ERROR_SCHEMA_V1, RESPONSE_SCHEMA_V1};

pub mod agent_detect;
pub mod agent_docs;
pub mod budget;
pub mod capabilities;
pub mod check;
pub mod claims;
pub mod context;
pub mod degraded_honesty;
pub mod doctor;
pub mod economy;
pub mod effect;
pub mod health;
pub mod index;
pub mod init;
pub mod lab;
pub mod legacy_import;
pub mod memory;
pub mod outcome;
pub mod quarantine;
pub mod repro;
pub mod search;
pub mod situation;
pub mod status;
pub mod streams;
pub mod support_bundle;
pub mod verify;
pub mod why;

pub use budget::{BudgetDimension, BudgetExceeded, BudgetSnapshot, RequestBudget};
pub use context::{AccessLevel, CapabilitySet, CommandContext};
pub use outcome::{
    CliCancelReason, CliOutcomeClass, CliOutcomeSummary, EXIT_CANCELLED, EXIT_PANICKED,
    OutcomeFeedbackSummary, OutcomeRecordOptions, OutcomeRecordReport, OutcomeRecordStatus,
    outcome_class, outcome_exit_code, record_outcome,
};

pub const VERSION_PROVENANCE_SCHEMA_V1: &str = "ee.version.provenance.v1";
pub const BUILD_TIMESTAMP_POLICY: &str = "omitted_for_reproducibility";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuildFeature {
    pub name: &'static str,
    pub enabled: bool,
}

impl BuildFeature {
    #[must_use]
    pub const fn new(name: &'static str, enabled: bool) -> Self {
        Self { name, enabled }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SupportedSchema {
    pub name: &'static str,
    pub schema: &'static str,
}

impl SupportedSchema {
    #[must_use]
    pub const fn new(name: &'static str, schema: &'static str) -> Self {
        Self { name, schema }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildProvenanceDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: &'static str,
    pub repair: &'static str,
}

impl BuildProvenanceDegradation {
    #[must_use]
    pub const fn new(
        code: &'static str,
        severity: &'static str,
        message: &'static str,
        repair: &'static str,
    ) -> Self {
        Self {
            code,
            severity,
            message,
            repair,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuildInfo {
    pub package: &'static str,
    pub version: &'static str,
    pub git_commit: Option<&'static str>,
    pub git_tag: Option<&'static str>,
    pub git_dirty: Option<bool>,
    pub target_triple: &'static str,
    pub target_arch: &'static str,
    pub target_os: &'static str,
    pub build_profile: &'static str,
    pub release_channel: &'static str,
    pub build_timestamp_policy: &'static str,
    pub min_db_migration: u32,
    pub max_db_migration: u32,
}

#[must_use]
pub fn build_info() -> BuildInfo {
    let (min_db_migration, max_db_migration) = db_migration_range();
    BuildInfo {
        package: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
        git_commit: clean_build_metadata(option_env!("VERGEN_GIT_SHA")),
        git_tag: clean_build_metadata(option_env!("VERGEN_GIT_DESCRIBE")),
        git_dirty: parse_build_bool(option_env!("VERGEN_GIT_DIRTY")),
        target_triple: clean_build_metadata(option_env!("EE_BUILD_TARGET")).unwrap_or("unknown"),
        target_arch: std::env::consts::ARCH,
        target_os: std::env::consts::OS,
        build_profile: build_profile(),
        release_channel: release_channel(),
        build_timestamp_policy: BUILD_TIMESTAMP_POLICY,
        min_db_migration,
        max_db_migration,
    }
}

#[must_use]
pub fn build_features() -> Vec<BuildFeature> {
    vec![
        BuildFeature::new("fts5", cfg!(feature = "fts5")),
        BuildFeature::new("json", cfg!(feature = "json")),
        BuildFeature::new("embed-fast", cfg!(feature = "embed-fast")),
        BuildFeature::new("lexical-bm25", cfg!(feature = "lexical-bm25")),
        BuildFeature::new("graph", cfg!(feature = "graph")),
        BuildFeature::new("mcp", cfg!(feature = "mcp")),
        BuildFeature::new("serve", cfg!(feature = "serve")),
        BuildFeature::new("science-analytics", cfg!(feature = "science-analytics")),
    ]
}

#[must_use]
pub fn supported_schemas() -> Vec<SupportedSchema> {
    vec![
        SupportedSchema::new("response", RESPONSE_SCHEMA_V1),
        SupportedSchema::new("error", ERROR_SCHEMA_V1),
        SupportedSchema::new("version_provenance", VERSION_PROVENANCE_SCHEMA_V1),
    ]
}

#[must_use]
pub fn db_migration_range() -> (u32, u32) {
    let min = crate::db::MIGRATIONS
        .first()
        .map_or(0, crate::db::Migration::version);
    let max = crate::db::MIGRATIONS
        .last()
        .map_or(0, crate::db::Migration::version);
    (min, max)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionReport {
    pub build: BuildInfo,
    pub features: Vec<BuildFeature>,
    pub schemas: Vec<SupportedSchema>,
    pub degradations: Vec<BuildProvenanceDegradation>,
}

impl VersionReport {
    #[must_use]
    pub fn gather() -> Self {
        let build = build_info();
        let mut degradations = Vec::new();

        if build.git_commit.is_none() && build.git_tag.is_none() && build.git_dirty.is_none() {
            degradations.push(BuildProvenanceDegradation::new(
                "git_metadata_unavailable",
                "low",
                "Git source metadata was not provided by the build.",
                "Build with VERGEN_GIT_SHA, VERGEN_GIT_DESCRIBE, and VERGEN_GIT_DIRTY set.",
            ));
        }

        if build.target_triple == "unknown" {
            degradations.push(BuildProvenanceDegradation::new(
                "target_triple_unavailable",
                "low",
                "Target triple was not provided by the build.",
                "Build with EE_BUILD_TARGET set to the target triple.",
            ));
        }

        Self {
            build,
            features: build_features(),
            schemas: supported_schemas(),
            degradations,
        }
    }

    #[must_use]
    pub fn provenance_available(&self) -> bool {
        self.degradations.is_empty()
    }
}

fn clean_build_metadata(value: Option<&'static str>) -> Option<&'static str> {
    match value {
        Some(value)
            if !value.is_empty()
                && !value.contains('/')
                && !value.contains('\\')
                && !value.contains('=')
                && !value.contains('\n')
                && !value.contains('\r') =>
        {
            Some(value)
        }
        _ => None,
    }
}

fn parse_build_bool(value: Option<&'static str>) -> Option<bool> {
    match clean_build_metadata(value) {
        Some("true" | "1" | "yes") => Some(true),
        Some("false" | "0" | "no") => Some(false),
        _ => None,
    }
}

const fn build_profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

fn release_channel() -> &'static str {
    match option_env!("EE_RELEASE_CHANNEL") {
        Some("stable") => "stable",
        Some("beta") => "beta",
        Some("nightly") => "nightly",
        Some("dev") => "dev",
        _ if cfg!(debug_assertions) => "dev",
        _ => "stable",
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

    use super::{
        BUILD_TIMESTAMP_POLICY, RuntimeProfile, VERSION_PROVENANCE_SCHEMA_V1, VersionReport,
        build_features, build_info, clean_build_metadata, db_migration_range, parse_build_bool,
        run_cli_future, runtime_status, supported_schemas,
    };

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
        )?;
        ensure_equal(
            &info.build_timestamp_policy,
            &BUILD_TIMESTAMP_POLICY,
            "timestamp policy",
        )?;
        ensure(
            info.min_db_migration <= info.max_db_migration,
            "database migration range must be ordered",
        )
    }

    #[test]
    fn version_report_uses_stable_ordered_contracts() -> TestResult {
        let report = VersionReport::gather();
        ensure_equal(
            &report.features.first().map(|feature| feature.name),
            &Some("fts5"),
            "first feature",
        )?;
        ensure_equal(
            &report
                .schemas
                .iter()
                .any(|schema| schema.schema == VERSION_PROVENANCE_SCHEMA_V1),
            &true,
            "version schema advertised",
        )?;
        ensure_equal(
            &report.provenance_available(),
            &report.degradations.is_empty(),
            "availability mirrors degradations",
        )
    }

    #[test]
    fn build_feature_flags_are_deterministically_ordered() -> TestResult {
        let names: Vec<&str> = build_features()
            .iter()
            .map(|feature| feature.name)
            .collect();
        ensure_equal(
            &names,
            &vec![
                "fts5",
                "json",
                "embed-fast",
                "lexical-bm25",
                "graph",
                "mcp",
                "serve",
                "science-analytics",
            ],
            "feature order",
        )
    }

    #[test]
    fn supported_schemas_include_response_error_and_version() -> TestResult {
        let schemas: Vec<&str> = supported_schemas()
            .iter()
            .map(|schema| schema.name)
            .collect();
        ensure_equal(
            &schemas,
            &vec!["response", "error", "version_provenance"],
            "schema names",
        )
    }

    #[test]
    fn db_migration_range_matches_declared_migrations() -> TestResult {
        let (min, max) = db_migration_range();
        ensure(min > 0, "minimum migration should be known")?;
        ensure(max >= min, "maximum migration should be >= minimum")
    }

    #[test]
    fn build_metadata_sanitizer_rejects_path_like_values() -> TestResult {
        ensure_equal(
            &clean_build_metadata(Some("abc123")),
            &Some("abc123"),
            "plain metadata",
        )?;
        ensure_equal(
            &clean_build_metadata(Some("/tmp/build/secret")),
            &None,
            "path metadata must be redacted",
        )?;
        ensure_equal(
            &clean_build_metadata(Some("TOKEN=value")),
            &None,
            "assignment metadata must be redacted",
        )
    }

    #[test]
    fn build_bool_parser_accepts_stable_literals() -> TestResult {
        ensure_equal(&parse_build_bool(Some("true")), &Some(true), "true")?;
        ensure_equal(&parse_build_bool(Some("0")), &Some(false), "zero")?;
        ensure_equal(&parse_build_bool(Some("maybe")), &None, "unknown bool")
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
