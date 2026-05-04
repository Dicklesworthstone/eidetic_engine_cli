//! Capabilities command handler (EE-030).
//!
//! Reports feature availability, command status, and subsystem readiness.
//! Used by agents to discover what ee can do in its current configuration.

use std::path::Path;

use crate::models::CapabilityStatus;

use super::build_info;
use super::status::{
    default_workspace_path, probe_cass_capability, probe_runtime_capability,
    probe_search_capability, probe_storage_capability,
};

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

/// Output format entry from the renderer registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputFormatEntry {
    pub name: &'static str,
    pub available: bool,
    pub machine_readable: bool,
    pub description: &'static str,
}

impl OutputFormatEntry {
    #[must_use]
    pub const fn new(
        name: &'static str,
        available: bool,
        machine_readable: bool,
        description: &'static str,
    ) -> Self {
        Self {
            name,
            available,
            machine_readable,
            description,
        }
    }
}

/// Resolved TOON dependency metadata reported by `ee capabilities --json`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToonDependencySource {
    pub crate_name: &'static str,
    pub package: &'static str,
    pub version: &'static str,
    pub source_kind: &'static str,
    pub path: &'static str,
    pub default_features: bool,
}

impl ToonDependencySource {
    #[must_use]
    pub const fn local() -> Self {
        Self {
            crate_name: "toon",
            package: "tru",
            version: "0.2.3",
            source_kind: "path",
            path: "/data/projects/toon_rust",
            default_features: false,
        }
    }
}

/// TOON output adapter readiness metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToonOutputCapability {
    pub available: bool,
    pub canonical_source_format: &'static str,
    pub dependency: ToonDependencySource,
    pub supported_output_profiles: Vec<&'static str>,
    pub default_format_env: &'static str,
    pub error_codes: Vec<&'static str>,
}

impl ToonOutputCapability {
    #[must_use]
    pub fn ready() -> Self {
        Self {
            available: true,
            canonical_source_format: "json",
            dependency: ToonDependencySource::local(),
            supported_output_profiles: vec!["minimal", "summary", "standard", "full"],
            default_format_env: "TOON_DEFAULT_FORMAT",
            error_codes: vec!["toon_decode_failed", "toon_encoding_failed"],
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
    pub output_formats: Vec<OutputFormatEntry>,
    pub toon: ToonOutputCapability,
}

impl CapabilitiesReport {
    /// Gather current capabilities from compile-time and runtime state.
    #[must_use]
    pub fn gather() -> Self {
        let workspace_path = default_workspace_path();
        Self::gather_with_workspace(workspace_path.as_deref())
    }

    #[must_use]
    pub fn gather_for_workspace(workspace_path: &Path) -> Self {
        Self::gather_with_workspace(Some(workspace_path))
    }

    #[must_use]
    pub fn gather_with_workspace(workspace_path: Option<&Path>) -> Self {
        let info = build_info();
        let runtime_status = probe_runtime_capability();
        let storage_status = probe_storage_capability(workspace_path);
        let search_status = probe_search_capability(workspace_path);
        let cass_status = probe_cass_capability();

        let subsystems = vec![
            CapabilityEntry::new("runtime", runtime_status, "Asupersync async runtime"),
            CapabilityEntry::new(
                "storage",
                storage_status,
                "FrankenSQLite/SQLModel persistence",
            ),
            CapabilityEntry::new("search", search_status, "Frankensearch hybrid retrieval"),
            CapabilityEntry::new(
                "graph",
                if cfg!(feature = "graph") {
                    CapabilityStatus::Ready
                } else {
                    CapabilityStatus::Pending
                },
                "FrankenNetworkX graph analytics",
            ),
            CapabilityEntry::new("cass", cass_status, "CASS session import adapter"),
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
            CommandEntry::new("rule", true, "Manage procedural rules"),
            CommandEntry::new("schema", true, "Schema registry"),
            CommandEntry::new("status", true, "Subsystem readiness"),
            CommandEntry::new("version", true, "Version info"),
            CommandEntry::new("context", true, "Context packing"),
            CommandEntry::new("search", true, "Memory search"),
            CommandEntry::new("why", true, "Explainability"),
            CommandEntry::new("init", true, "Workspace initialization"),
            CommandEntry::new("index", true, "Search index management"),
            CommandEntry::new("curate", true, "Rule curation"),
        ];

        let output_formats = vec![
            OutputFormatEntry::new("json", true, true, "Canonical stable response envelope"),
            OutputFormatEntry::new("toon", true, false, "TOON renderer over canonical JSON"),
            OutputFormatEntry::new("human", true, false, "Human-readable terminal output"),
            OutputFormatEntry::new("markdown", true, false, "Markdown context output"),
            OutputFormatEntry::new("jsonl", true, true, "Line-delimited JSON stream output"),
            OutputFormatEntry::new("compact", true, true, "Compact machine-readable output"),
            OutputFormatEntry::new("hook", true, true, "Hook protocol output"),
        ];

        Self {
            version: info.version,
            subsystems,
            features,
            commands,
            output_formats,
            toon: ToonOutputCapability::ready(),
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
        ensure_at_least(report.commands.len(), 5, "at least 5 commands")?;
        ensure_at_least(report.output_formats.len(), 5, "at least 5 output formats")
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

    #[test]
    fn capabilities_report_includes_toon_output_metadata() -> TestResult {
        let report = CapabilitiesReport::gather();

        ensure(report.toon.available, true, "toon output is available")?;
        ensure(
            report.toon.dependency.package,
            "tru",
            "toon dependency package",
        )?;
        ensure(
            report.toon.dependency.path,
            "/data/projects/toon_rust",
            "toon dependency path",
        )?;
        ensure_at_least(
            report.toon.supported_output_profiles.len(),
            4,
            "toon supported profiles",
        )
    }
}
