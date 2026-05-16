//! Capabilities command handler (EE-030).
//!
//! Reports feature availability, command status, and subsystem readiness.
//! Used by agents to discover what ee can do in its current configuration.

use std::path::Path;

use crate::models::CapabilityStatus;

use super::build_info;
use super::index::{IndexStatusOptions, get_index_status};
use super::status::{
    default_workspace_path, probe_cass_capability, probe_graph_capability, probe_mesh_capability,
    probe_runtime_capability, probe_search_capability, probe_storage_capability,
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

/// Build-time gap surfaced once through `ee capabilities`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnimplementedCapabilityEntry {
    pub code: &'static str,
    pub feature_flag: &'static str,
    pub ship_target: &'static str,
    pub tracking_bead: &'static str,
    pub user_message: &'static str,
}

impl UnimplementedCapabilityEntry {
    #[must_use]
    pub const fn new(
        code: &'static str,
        feature_flag: &'static str,
        ship_target: &'static str,
        tracking_bead: &'static str,
        user_message: &'static str,
    ) -> Self {
        Self {
            code,
            feature_flag,
            ship_target,
            tracking_bead,
            user_message,
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
    pub fn gather() -> Self {
        Self {
            available: crate::output::toon_output_available(),
            canonical_source_format: "json",
            dependency: ToonDependencySource::local(),
            supported_output_profiles: vec!["minimal", "summary", "standard", "full"],
            default_format_env: "TOON_DEFAULT_FORMAT",
            error_codes: vec!["toon_decode_failed", "toon_encoding_failed"],
        }
    }
}

/// Search-index metadata surfaced through `ee capabilities`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexCapabilitySummary {
    pub last_full_rebuild_at: Option<String>,
}

impl IndexCapabilitySummary {
    #[must_use]
    pub fn gather(workspace_path: Option<&Path>) -> Self {
        let last_full_rebuild_at = workspace_path.and_then(|workspace_path| {
            get_index_status(&IndexStatusOptions {
                workspace_path: workspace_path.to_path_buf(),
                database_path: None,
                index_dir: None,
            })
            .ok()
            .and_then(|report| report.last_rebuild_at)
        });

        Self {
            last_full_rebuild_at,
        }
    }
}

/// Full capabilities report returned by the capabilities command.
#[derive(Clone, Debug)]
pub struct CapabilitiesReport {
    pub version: &'static str,
    pub subsystems: Vec<CapabilityEntry>,
    pub features: Vec<FeatureEntry>,
    pub unimplemented: Vec<UnimplementedCapabilityEntry>,
    pub commands: Vec<CommandEntry>,
    pub output_formats: Vec<OutputFormatEntry>,
    pub index: IndexCapabilitySummary,
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
        let graph_status = probe_graph_capability();
        let mesh_status = probe_mesh_capability();
        let cass_status = probe_cass_capability();
        let index = IndexCapabilitySummary::gather(workspace_path);

        let subsystems = vec![
            CapabilityEntry::new("runtime", runtime_status, "Asupersync async runtime"),
            CapabilityEntry::new(
                "storage",
                storage_status,
                "FrankenSQLite/SQLModel persistence",
            ),
            CapabilityEntry::new("search", search_status, "Frankensearch hybrid retrieval"),
            CapabilityEntry::new("graph", graph_status, "FrankenNetworkX graph analytics"),
            CapabilityEntry::new(
                "mesh",
                mesh_status,
                "Optional peer mesh memory cache; disabled by default",
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
            FeatureEntry::new("mesh", false, "Optional mesh memory cache"),
            FeatureEntry::new("serve", cfg!(feature = "serve"), "HTTP serve adapter"),
        ];

        let mut unimplemented = Vec::new();
        if runtime_status == CapabilityStatus::Unimplemented {
            unimplemented.push(UnimplementedCapabilityEntry::new(
                "runtime_unavailable",
                "asupersync",
                "v0.2",
                "bd-17c65.5.5",
                "Asupersync runtime support is not available in this binary.",
            ));
        }
        if storage_status == CapabilityStatus::Unimplemented {
            unimplemented.push(UnimplementedCapabilityEntry::new(
                "storage_unimplemented",
                "fsqlite",
                "v0.2",
                "bd-17c65.5.5",
                "Storage support is not available in this binary.",
            ));
        }
        if search_status == CapabilityStatus::Unimplemented {
            unimplemented.push(UnimplementedCapabilityEntry::new(
                "search_unimplemented",
                "frankensearch",
                "v0.2",
                "bd-17c65.5.5",
                "Search support is not available in this binary.",
            ));
        }
        if !cfg!(feature = "lexical-bm25") {
            unimplemented.push(UnimplementedCapabilityEntry::new(
                "lexical_unavailable",
                "lexical-bm25",
                "v0.2",
                "bd-17c65.5.5",
                "BM25 lexical search is disabled in this build.",
            ));
        }
        if graph_status == CapabilityStatus::Unimplemented || !cfg!(feature = "graph") {
            unimplemented.push(UnimplementedCapabilityEntry::new(
                "graph_feature_disabled",
                "graph",
                "v0.2",
                "bd-17c65.5.5",
                "Graph algorithm execution is disabled in this build.",
            ));
        }
        if !cfg!(feature = "mcp") {
            unimplemented.push(UnimplementedCapabilityEntry::new(
                "mcp_feature_disabled",
                "mcp",
                "v0.2",
                "bd-17c65.5.5",
                "MCP stdio adapter support is disabled in this build.",
            ));
        }
        if mesh_status == CapabilityStatus::Unimplemented {
            unimplemented.push(UnimplementedCapabilityEntry::new(
                "mesh_feature_disabled",
                "mesh",
                "post-v0.5",
                "bd-x4hn7",
                "Optional mesh memory surfaces are disabled or not linked in this build.",
            ));
        }
        if !cfg!(feature = "serve") {
            unimplemented.push(UnimplementedCapabilityEntry::new(
                "daemon_background_mode_unimplemented",
                "serve",
                "v0.5",
                "bd-17c65.5.5",
                "Background daemon mode is not implemented in this build; bounded foreground mode is available.",
            ));
        }
        unimplemented.push(UnimplementedCapabilityEntry::new(
            "diagram_backend_unavailable",
            "franken-mermaid-adapter",
            "v0.3",
            "bd-17c65.5.5",
            "Diagram backend support is not linked in this build.",
        ));
        unimplemented.sort_by(|left, right| left.code.cmp(right.code));

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
            CommandEntry::new(
                "daemon foreground decay_sweep",
                true,
                "Bounded foreground score-decay steward job",
            ),
            CommandEntry::new(
                "daemon background",
                false,
                "Background daemon scheduling is unavailable",
            ),
            CommandEntry::new(
                "daemon foreground non-decay",
                false,
                "Non-decay steward jobs report unavailable until real handlers are wired",
            ),
        ];

        let output_formats = vec![
            OutputFormatEntry::new("json", true, true, "Canonical stable response envelope"),
            OutputFormatEntry::new(
                "toon",
                crate::output::toon_output_available(),
                false,
                "TOON renderer over canonical JSON",
            ),
            OutputFormatEntry::new("human", true, false, "Human-readable terminal output"),
            OutputFormatEntry::new("markdown", true, false, "Markdown context output"),
            OutputFormatEntry::new("mermaid", true, false, "Mermaid diagram output"),
            OutputFormatEntry::new("jsonl", true, true, "Line-delimited JSON stream output"),
            OutputFormatEntry::new("compact", true, true, "Compact machine-readable output"),
            OutputFormatEntry::new("hook", true, true, "Hook protocol output"),
        ];

        Self {
            version: info.version,
            subsystems,
            features,
            unimplemented,
            commands,
            output_formats,
            index,
            toon: ToonOutputCapability::gather(),
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

    /// Count of build-time gaps reported once through capabilities.
    #[must_use]
    pub fn unimplemented_count(&self) -> usize {
        self.unimplemented.len()
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
        ensure_at_least(report.output_formats.len(), 8, "all output formats")
    }

    #[test]
    fn capabilities_report_lists_all_global_output_formats() -> TestResult {
        let report = CapabilitiesReport::gather();
        let names = report
            .output_formats
            .iter()
            .map(|format| format.name)
            .collect::<Vec<_>>();
        ensure(
            names,
            vec![
                "json", "toon", "human", "markdown", "mermaid", "jsonl", "compact", "hook",
            ],
            "capabilities output formats",
        )
    }

    #[test]
    fn capabilities_report_has_runtime_ready() -> TestResult {
        let report = CapabilitiesReport::gather();

        let runtime = report
            .subsystems
            .iter()
            .find(|s| s.name == "runtime")
            .unwrap_or_else(|| panic!("runtime subsystem must exist")); // ubs:ignore
        ensure(runtime.status, CapabilityStatus::Ready, "runtime is ready")
    }

    #[test]
    fn capabilities_report_surfaces_mesh_as_default_off() -> TestResult {
        let report = CapabilitiesReport::gather();

        let mesh = report
            .subsystems
            .iter()
            .find(|s| s.name == "mesh")
            .unwrap_or_else(|| panic!("mesh subsystem must exist")); // ubs:ignore
        ensure(
            mesh.status,
            CapabilityStatus::Pending,
            "mesh defaults to pending",
        )?;

        let feature = report
            .features
            .iter()
            .find(|feature| feature.name == "mesh")
            .unwrap_or_else(|| panic!("mesh feature must exist")); // ubs:ignore
        ensure(feature.enabled, false, "mesh feature defaults off")
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
    fn capabilities_report_surfaces_build_time_gaps_once() -> TestResult {
        let report = CapabilitiesReport::gather();
        let codes = report
            .unimplemented
            .iter()
            .map(|entry| entry.code)
            .collect::<Vec<_>>();

        if !cfg!(feature = "mcp") && !codes.contains(&"mcp_feature_disabled") {
            return Err(format!(
                "mcp feature gap should be in capabilities.unimplemented; got {codes:?}"
            ));
        }
        if !cfg!(feature = "serve") && !codes.contains(&"daemon_background_mode_unimplemented") {
            return Err(format!(
                "daemon background gap should be in capabilities.unimplemented; got {codes:?}"
            ));
        }
        if !codes.contains(&"diagram_backend_unavailable") {
            return Err(format!(
                "diagram backend gap should be in capabilities.unimplemented; got {codes:?}"
            ));
        }
        ensure(report.unimplemented_count(), codes.len(), "gap count")
    }

    #[test]
    fn capabilities_report_includes_capabilities_command() -> TestResult {
        let report = CapabilitiesReport::gather();

        let cmd = report
            .commands
            .iter()
            .find(|c| c.name == "capabilities")
            .unwrap_or_else(|| panic!("capabilities command must exist")); // ubs:ignore
        ensure(cmd.available, true, "capabilities command is available")
    }

    #[test]
    fn capabilities_report_marks_daemon_maintenance_posture() -> TestResult {
        let report = CapabilitiesReport::gather();

        let decay = report
            .commands
            .iter()
            .find(|c| c.name == "daemon foreground decay_sweep")
            .unwrap_or_else(|| panic!("daemon foreground decay_sweep command must exist")); // ubs:ignore
        ensure(decay.available, true, "decay sweep daemon job is available")?;

        let background = report
            .commands
            .iter()
            .find(|c| c.name == "daemon background")
            .unwrap_or_else(|| panic!("daemon background command must exist")); // ubs:ignore
        ensure(background.available, false, "background daemon unavailable")?;

        let non_decay = report
            .commands
            .iter()
            .find(|c| c.name == "daemon foreground non-decay")
            .unwrap_or_else(|| panic!("daemon foreground non-decay command must exist")); // ubs:ignore
        ensure(
            non_decay.available,
            false,
            "non-decay daemon jobs unavailable",
        )
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
