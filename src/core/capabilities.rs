//! Capabilities command handler (EE-030).
//!
//! Reports feature availability, command status, and subsystem readiness.
//! Used by agents to discover what ee can do in its current configuration.

use crate::models::CapabilityStatus;

use super::{build_info, runtime_status};

/// A single capability entry describing a feature or subsystem.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityEntry {
    pub name: &'static str,
    pub status: CapabilityStatus,
    pub description: &'static str,
}

impl CapabilityEntry {
    #[must_use]
    pub const fn new(
        name: &'static str,
        status: CapabilityStatus,
        description: &'static str,
    ) -> Self {
        Self {
            name,
            status,
            description,
        }
    }
}

/// A command entry describing CLI command availability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandEntry {
    pub name: &'static str,
    pub available: bool,
    pub description: &'static str,
}

impl CommandEntry {
    #[must_use]
    pub const fn new(name: &'static str, available: bool, description: &'static str) -> Self {
        Self {
            name,
            available,
            description,
        }
    }
}

/// Feature flag entry from compile-time configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeatureEntry {
    pub name: &'static str,
    pub enabled: bool,
    pub description: &'static str,
}

impl FeatureEntry {
    #[must_use]
    pub const fn new(name: &'static str, enabled: bool, description: &'static str) -> Self {
        Self {
            name,
            enabled,
            description,
        }
    }
}

/// Full capabilities report returned by the capabilities command.
#[derive(Clone, Debug)]
pub struct CapabilitiesReport {
    pub version: &'static str,
    pub subsystems: Vec<CapabilityEntry>,
    pub features: Vec<FeatureEntry>,
    pub commands: Vec<CommandEntry>,
}

impl CapabilitiesReport {
    /// Gather current capabilities from compile-time and runtime state.
    #[must_use]
    pub fn gather() -> Self {
        let info = build_info();
        let _runtime = runtime_status();

        let subsystems = vec![
            CapabilityEntry::new(
                "runtime",
                CapabilityStatus::Ready,
                "Asupersync async runtime",
            ),
            CapabilityEntry::new(
                "storage",
                CapabilityStatus::Unimplemented,
                "FrankenSQLite/SQLModel persistence",
            ),
            CapabilityEntry::new(
                "search",
                CapabilityStatus::Unimplemented,
                "Frankensearch hybrid retrieval",
            ),
            CapabilityEntry::new(
                "graph",
                CapabilityStatus::Unimplemented,
                "FrankenNetworkX graph analytics",
            ),
            CapabilityEntry::new(
                "cass",
                CapabilityStatus::Degraded,
                "CASS session import adapter",
            ),
        ];

        let features = vec![
            FeatureEntry::new("fts5", cfg!(feature = "fts5"), "FTS5 full-text search"),
            FeatureEntry::new("json", cfg!(feature = "json"), "JSON extension support"),
            FeatureEntry::new(
                "embed-fast",
                cfg!(feature = "embed-fast"),
                "Fast embedding via model2vec",
            ),
            FeatureEntry::new(
                "embed-quality",
                false, // Feature blocked: pulls forbidden deps (reqwest/tokio/hyper)
                "Quality embedding via fastembed",
            ),
            FeatureEntry::new(
                "lexical-bm25",
                cfg!(feature = "lexical-bm25"),
                "BM25 lexical scoring",
            ),
            FeatureEntry::new("mcp", cfg!(feature = "mcp"), "MCP server adapter"),
            FeatureEntry::new("serve", cfg!(feature = "serve"), "HTTP serve adapter"),
        ];

        let commands = vec![
            CommandEntry::new("capabilities", true, "Report feature availability"),
            CommandEntry::new("check", true, "Quick posture summary"),
            CommandEntry::new("doctor", true, "Health checks"),
            CommandEntry::new("eval", true, "Evaluation scenarios"),
            CommandEntry::new("help", true, "Command help"),
            CommandEntry::new("import", true, "Import from external sources"),
            CommandEntry::new("remember", true, "Store memories"),
            CommandEntry::new("schema", true, "Schema registry"),
            CommandEntry::new("status", true, "Subsystem readiness"),
            CommandEntry::new("version", true, "Version info"),
            CommandEntry::new("context", false, "Context packing"),
            CommandEntry::new("search", false, "Memory search"),
            CommandEntry::new("why", false, "Explainability"),
            CommandEntry::new("init", false, "Workspace initialization"),
            CommandEntry::new("index", false, "Search index management"),
            CommandEntry::new("curate", false, "Rule curation"),
        ];

        Self {
            version: info.version,
            subsystems,
            features,
            commands,
        }
    }

    /// Count of ready subsystems.
    #[must_use]
    pub fn ready_subsystem_count(&self) -> usize {
        self.subsystems
            .iter()
            .filter(|s| s.status == CapabilityStatus::Ready)
            .count()
    }

    /// Count of enabled features.
    #[must_use]
    pub fn enabled_feature_count(&self) -> usize {
        self.features.iter().filter(|f| f.enabled).count()
    }

    /// Count of available commands.
    #[must_use]
    pub fn available_command_count(&self) -> usize {
        self.commands.iter().filter(|c| c.available).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn ensure_at_least<T: std::fmt::Debug + PartialOrd>(
        actual: T,
        minimum: T,
        ctx: &str,
    ) -> TestResult {
        if actual >= minimum {
            Ok(())
        } else {
            Err(format!(
                "{ctx}: expected at least {minimum:?}, got {actual:?}"
            ))
        }
    }

    #[test]
    fn capabilities_report_gather_returns_valid_report() -> TestResult {
        let report = CapabilitiesReport::gather();

        ensure(
            report.version,
            env!("CARGO_PKG_VERSION"),
            "version from cargo",
        )?;
        ensure_at_least(report.subsystems.len(), 3, "at least 3 subsystems")?;
        ensure_at_least(report.features.len(), 3, "at least 3 features")?;
        ensure_at_least(report.commands.len(), 5, "at least 5 commands")
    }

    #[test]
    #[expect(clippy::expect_used)]
    fn capabilities_report_has_runtime_ready() -> TestResult {
        let report = CapabilitiesReport::gather();

        let runtime = report
            .subsystems
            .iter()
            .find(|s| s.name == "runtime")
            .expect("runtime subsystem must exist");
        ensure(runtime.status, CapabilityStatus::Ready, "runtime is ready")
    }

    #[test]
    fn capabilities_report_counts_are_consistent() -> TestResult {
        let report = CapabilitiesReport::gather();

        ensure_at_least(report.ready_subsystem_count(), 1, "at least 1 ready")?;
        ensure_at_least(
            report.available_command_count(),
            5,
            "at least 5 available commands",
        )
    }

    #[test]
    #[expect(clippy::expect_used)]
    fn capabilities_report_includes_capabilities_command() -> TestResult {
        let report = CapabilitiesReport::gather();

        let cmd = report
            .commands
            .iter()
            .find(|c| c.name == "capabilities")
            .expect("capabilities command must exist");
        ensure(cmd.available, true, "capabilities command is available")
    }
}
