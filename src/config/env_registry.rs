//! Central registry for `EE_*` environment variables honored by ee.
//!
//! Adding a new `EE_*` environment variable requires adding a variant here.
//! Tests enforce that production code reads these variables through this
//! registry rather than spelling raw names at call sites.

use std::ffi::OsString;
use std::str::FromStr;

/// Every `EE_*` environment variable honored by ee.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EnvVar {
    /// `EE_AGENT_MODE`
    AgentMode,
    /// `EE_CASS_BINARY`
    CassBinary,
    /// `EE_DATABASE_PATH`
    DatabasePath,
    /// `EE_DEMO_EVIDENCE_ROOT`
    DemoEvidenceRoot,
    /// `EE_EXPERIMENTAL_TRIAD`
    ExperimentalTriad,
    /// `EE_FORMAT`
    Format,
    /// `EE_HARMFUL_BURST_WINDOW_SECONDS`
    HarmfulBurstWindowSeconds,
    /// `EE_HARMFUL_PER_SOURCE_PER_HOUR`
    HarmfulPerSourcePerHour,
    /// `EE_HOOK_MODE`
    HookMode,
    /// `EE_INDEX_DIR`
    IndexDir,
    /// `EE_JSON`
    Json,
    /// `EE_LOG_FORMAT`
    LogFormat,
    /// `EE_LOG_JSON`
    LogJson,
    /// `EE_MAX_TOKENS`
    MaxTokens,
    /// `EE_NO_COLOR`
    NoColor,
    /// `EE_OUTPUT_FORMAT`
    OutputFormat,
    /// `EE_PREFLIGHT_BYPASS_SECRET`
    PreflightBypassSecret,
    /// `EE_PROFILE`
    Profile,
    /// `EE_REMEMBER_CURATION_SYNC_BUDGET_MS`
    RememberCurationSyncBudgetMs,
    /// `EE_SECURITY_PROFILE`
    SecurityProfile,
    /// `EE_TEST_LOG_LEVEL`
    TestLogLevel,
    /// `EE_TEST_LOG_PATH`
    TestLogPath,
    /// `EE_TEST_LOG_TEST_ID`
    TestLogTestId,
    /// `EE_WORKSPACE`
    Workspace,
    /// `EE_WORKSPACE_REGISTRY`
    WorkspaceRegistry,
}

impl EnvVar {
    /// Return all registered variables in stable display order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::AgentMode,
            Self::CassBinary,
            Self::DatabasePath,
            Self::DemoEvidenceRoot,
            Self::ExperimentalTriad,
            Self::Format,
            Self::HarmfulBurstWindowSeconds,
            Self::HarmfulPerSourcePerHour,
            Self::HookMode,
            Self::IndexDir,
            Self::Json,
            Self::LogFormat,
            Self::LogJson,
            Self::MaxTokens,
            Self::NoColor,
            Self::OutputFormat,
            Self::PreflightBypassSecret,
            Self::Profile,
            Self::RememberCurationSyncBudgetMs,
            Self::SecurityProfile,
            Self::TestLogLevel,
            Self::TestLogPath,
            Self::TestLogTestId,
            Self::Workspace,
            Self::WorkspaceRegistry,
        ]
    }

    /// Stable environment variable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::AgentMode => "EE_AGENT_MODE",
            Self::CassBinary => "EE_CASS_BINARY",
            Self::DatabasePath => "EE_DATABASE_PATH",
            Self::DemoEvidenceRoot => "EE_DEMO_EVIDENCE_ROOT",
            Self::ExperimentalTriad => "EE_EXPERIMENTAL_TRIAD",
            Self::Format => "EE_FORMAT",
            Self::HarmfulBurstWindowSeconds => "EE_HARMFUL_BURST_WINDOW_SECONDS",
            Self::HarmfulPerSourcePerHour => "EE_HARMFUL_PER_SOURCE_PER_HOUR",
            Self::HookMode => "EE_HOOK_MODE",
            Self::IndexDir => "EE_INDEX_DIR",
            Self::Json => "EE_JSON",
            Self::LogFormat => "EE_LOG_FORMAT",
            Self::LogJson => "EE_LOG_JSON",
            Self::MaxTokens => "EE_MAX_TOKENS",
            Self::NoColor => "EE_NO_COLOR",
            Self::OutputFormat => "EE_OUTPUT_FORMAT",
            Self::PreflightBypassSecret => "EE_PREFLIGHT_BYPASS_SECRET",
            Self::Profile => "EE_PROFILE",
            Self::RememberCurationSyncBudgetMs => "EE_REMEMBER_CURATION_SYNC_BUDGET_MS",
            Self::SecurityProfile => "EE_SECURITY_PROFILE",
            Self::TestLogLevel => "EE_TEST_LOG_LEVEL",
            Self::TestLogPath => "EE_TEST_LOG_PATH",
            Self::TestLogTestId => "EE_TEST_LOG_TEST_ID",
            Self::Workspace => "EE_WORKSPACE",
            Self::WorkspaceRegistry => "EE_WORKSPACE_REGISTRY",
        }
    }

    /// Human-readable control surface description.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::AgentMode => "Use agent-oriented output defaults.",
            Self::CassBinary => "Override the trusted cass import binary path.",
            Self::DatabasePath => "Override the configured storage database path.",
            Self::DemoEvidenceRoot => "Override the demo evidence storage root.",
            Self::ExperimentalTriad => {
                "Compatibility no-op for the promoted ee pack/note/why aliases."
            }
            Self::Format => "Select the default output renderer.",
            Self::HarmfulBurstWindowSeconds => {
                "Override the harmful feedback burst window in seconds."
            }
            Self::HarmfulPerSourcePerHour => "Override the harmful feedback rate limit per source.",
            Self::HookMode => "Use hook-oriented machine output defaults.",
            Self::IndexDir => "Override the configured search index directory.",
            Self::Json => "Request JSON output from renderer auto-detection.",
            Self::LogFormat => "Select structured log format.",
            Self::LogJson => "Enable JSON command-start logs on stderr.",
            Self::MaxTokens => "Override the default context pack token budget.",
            Self::NoColor => "Disable colored diagnostics.",
            Self::OutputFormat => "Select the default output renderer.",
            Self::PreflightBypassSecret => "Supply preflight bypass secret material.",
            Self::Profile => "Override the default context pack profile.",
            Self::RememberCurationSyncBudgetMs => {
                "Override remember-time curation sync budget in milliseconds."
            }
            Self::SecurityProfile => "Select security profile.",
            Self::TestLogLevel => "Control structured test-log verbosity.",
            Self::TestLogPath => "Enable structured test logging at this JSONL path.",
            Self::TestLogTestId => "Name the active structured test-log scenario.",
            Self::Workspace => "Override workspace root discovery.",
            Self::WorkspaceRegistry => "Override the workspace alias registry database path.",
        }
    }

    /// Default value, when the variable has a registry-defined default.
    #[must_use]
    pub const fn default_value(self) -> Option<&'static str> {
        match self {
            Self::RememberCurationSyncBudgetMs => Some("50"),
            _ => None,
        }
    }

    /// Whether capabilities output may include this variable's current value.
    #[must_use]
    pub const fn exposes_value(self) -> bool {
        !matches!(self, Self::PreflightBypassSecret)
    }

    /// Broad documentation category for agent docs and env-var catalogs.
    #[must_use]
    pub const fn category(self) -> &'static str {
        match self {
            Self::CassBinary => "integration",
            Self::DatabasePath
            | Self::DemoEvidenceRoot
            | Self::IndexDir
            | Self::Workspace
            | Self::WorkspaceRegistry => "paths",
            Self::AgentMode
            | Self::ExperimentalTriad
            | Self::Format
            | Self::HookMode
            | Self::Json
            | Self::NoColor
            | Self::OutputFormat => "output",
            Self::LogFormat
            | Self::LogJson
            | Self::TestLogLevel
            | Self::TestLogPath
            | Self::TestLogTestId => "diagnostics",
            Self::HarmfulBurstWindowSeconds
            | Self::HarmfulPerSourcePerHour
            | Self::MaxTokens
            | Self::Profile
            | Self::RememberCurationSyncBudgetMs => "tuning",
            Self::PreflightBypassSecret | Self::SecurityProfile => "policy",
        }
    }

    /// Parse this variable through [`FromStr`].
    #[must_use]
    pub fn parse_into<T>(self) -> Option<T>
    where
        T: FromStr,
    {
        read(self).and_then(|value| value.parse::<T>().ok())
    }
}

/// Read an `EE_*` environment variable as UTF-8.
#[must_use]
pub fn read(var: EnvVar) -> Option<String> {
    read_os(var).and_then(|value| value.into_string().ok())
}

/// Read an `EE_*` environment variable as an OS string.
#[must_use]
pub fn read_os(var: EnvVar) -> Option<OsString> {
    let value = std::env::var_os(var.name());
    trace_env_read(var, value.as_ref(), "process_env");
    value
}

/// Read an `EE_*` environment variable or its registry-defined default.
#[must_use]
pub fn read_or_default(var: EnvVar) -> Option<String> {
    if let Some(value) = read(var) {
        return Some(value);
    }

    let default = var.default_value().map(str::to_owned);
    if let Some(value) = default.as_deref() {
        tracing::trace!(
            var_name = var.name(),
            found = true,
            value_hash = %hash_bytes(value.as_bytes()),
            source = "registry_default",
            "ee_env_registry_read"
        );
    }
    default
}

/// Return whether an `EE_*` environment variable is present.
#[must_use]
pub fn is_set(var: EnvVar) -> bool {
    read_os(var).is_some()
}

fn trace_env_read(var: EnvVar, value: Option<&OsString>, source: &'static str) {
    let value_hash = value.map(|value| hash_os_value(value.as_os_str()));
    tracing::trace!(
        var_name = var.name(),
        found = value.is_some(),
        value_hash = value_hash.as_deref().unwrap_or(""),
        source,
        "ee_env_registry_read"
    );
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

#[cfg(unix)]
fn hash_os_value(value: &std::ffi::OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;

    hash_bytes(value.as_bytes())
}

#[cfg(not(unix))]
fn hash_os_value(value: &std::ffi::OsStr) -> String {
    hash_bytes(value.to_string_lossy().as_bytes())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::EnvVar;

    type TestResult = Result<(), String>;

    #[test]
    fn every_env_var_has_name_and_description() -> TestResult {
        for var in EnvVar::all() {
            if !var.name().starts_with("EE_") {
                return Err(format!("{} does not start with EE_", var.name()));
            }
            if var.description().trim().is_empty() {
                return Err(format!("{} has an empty description", var.name()));
            }
        }
        Ok(())
    }

    #[test]
    fn env_var_names_are_unique() -> TestResult {
        let mut names = BTreeSet::new();
        for var in EnvVar::all() {
            if !names.insert(var.name()) {
                return Err(format!("duplicate env var registered: {}", var.name()));
            }
        }
        Ok(())
    }

    #[test]
    fn registry_default_is_available() -> TestResult {
        let value = EnvVar::RememberCurationSyncBudgetMs
            .default_value()
            .ok_or_else(|| "remember curation budget default missing".to_owned())?;
        if value == "50" {
            Ok(())
        } else {
            Err(format!(
                "unexpected remember curation budget default: {value}"
            ))
        }
    }

    #[test]
    fn sensitive_env_vars_do_not_expose_values() -> TestResult {
        if EnvVar::PreflightBypassSecret.exposes_value() {
            Err("EE_PREFLIGHT_BYPASS_SECRET must not expose currentValue".to_owned())
        } else {
            Ok(())
        }
    }
}
