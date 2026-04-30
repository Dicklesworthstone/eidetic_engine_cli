//! Doctor command handler (EE-025, EE-241).
//!
//! Performs health checks on workspace subsystems and returns a structured
//! report with issues and repair suggestions.
//!
//! The `--fix-plan` flag (EE-241) outputs a structured repair plan that
//! agents can execute step-by-step.

use crate::models::error_codes::{self, ErrorCode};

pub const DEPENDENCY_DIAGNOSTICS_SCHEMA_V1: &str = "ee.diag.dependencies.v1";
pub const FRANKEN_HEALTH_SCHEMA_V1: &str = "ee.doctor.franken_health.v1";
pub const DEPENDENCY_MATRIX_REVISION: u32 = 1;
pub const DEPENDENCY_MATRIX_SOURCE_BEAD: &str = "eidetic_engine_cli-ilcq";
pub const DEPENDENCY_MATRIX_SOURCE_PLAN_ITEM: &str = "EE-307";
pub const DEPENDENCY_MATRIX_DEFAULT_FEATURE_PROFILE: &str = "default";

pub const FORBIDDEN_CRATES: &[&str] = &[
    "tokio",
    "tokio-util",
    "async-std",
    "smol",
    "rusqlite",
    "sqlx",
    "diesel",
    "sea-orm",
    "petgraph",
    "hyper",
    "axum",
    "tower",
    "reqwest",
];

/// Severity of a doctor check issue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckSeverity {
    Ok,
    Warning,
    Error,
}

impl CheckSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }

    #[must_use]
    pub const fn is_healthy(self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// Result of a single health check.
#[derive(Clone, Debug)]
pub struct CheckResult {
    pub name: &'static str,
    pub severity: CheckSeverity,
    pub message: String,
    pub error_code: Option<ErrorCode>,
    pub repair: Option<&'static str>,
}

impl CheckResult {
    #[must_use]
    pub fn ok(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            severity: CheckSeverity::Ok,
            message: message.into(),
            error_code: None,
            repair: None,
        }
    }

    #[must_use]
    pub fn warning(name: &'static str, message: impl Into<String>, error_code: ErrorCode) -> Self {
        Self {
            name,
            severity: CheckSeverity::Warning,
            message: message.into(),
            error_code: Some(error_code),
            repair: error_code.default_repair,
        }
    }

    #[must_use]
    pub fn error(name: &'static str, message: impl Into<String>, error_code: ErrorCode) -> Self {
        Self {
            name,
            severity: CheckSeverity::Error,
            message: message.into(),
            error_code: Some(error_code),
            repair: error_code.default_repair,
        }
    }
}

/// Full doctor report.
#[derive(Clone, Debug)]
pub struct DoctorReport {
    pub version: &'static str,
    pub overall_healthy: bool,
    pub checks: Vec<CheckResult>,
}

impl DoctorReport {
    /// Run all health checks and return a report.
    #[must_use]
    pub fn gather() -> Self {
        let checks = vec![
            check_runtime(),
            check_workspace(),
            check_database(),
            check_search_index(),
            check_cass(),
        ];

        let overall_healthy = checks.iter().all(|c| c.severity.is_healthy());

        Self {
            version: env!("CARGO_PKG_VERSION"),
            overall_healthy,
            checks,
        }
    }

    /// Convert the doctor report into a structured fix plan.
    #[must_use]
    pub fn to_fix_plan(&self) -> FixPlan {
        let steps: Vec<FixStep> = self
            .checks
            .iter()
            .filter(|c| !c.severity.is_healthy() && c.repair.is_some())
            .enumerate()
            .map(|(idx, check)| FixStep {
                order: idx + 1,
                subsystem: check.name,
                severity: check.severity,
                issue: check.message.clone(),
                error_code: check.error_code,
                command: check.repair.unwrap_or_default(),
            })
            .collect();

        let total_issues = self
            .checks
            .iter()
            .filter(|c| !c.severity.is_healthy())
            .count();
        let fixable_issues = steps.len();

        FixPlan {
            version: self.version,
            total_issues,
            fixable_issues,
            steps,
        }
    }
}

/// A structured repair plan generated from doctor checks.
#[derive(Clone, Debug)]
pub struct FixPlan {
    pub version: &'static str,
    pub total_issues: usize,
    pub fixable_issues: usize,
    pub steps: Vec<FixStep>,
}

impl FixPlan {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// A single repair step in a fix plan.
#[derive(Clone, Debug)]
pub struct FixStep {
    pub order: usize,
    pub subsystem: &'static str,
    pub severity: CheckSeverity,
    pub issue: String,
    pub error_code: Option<ErrorCode>,
    pub command: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DependencySource {
    pub kind: &'static str,
    pub version: &'static str,
    pub path: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DependencyFeatureProfile {
    pub default_features: bool,
    pub features: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DependencyOptionalFeatureProfile {
    pub name: &'static str,
    pub features: &'static [&'static str],
    pub status: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DependencyBlockedFeature {
    pub name: &'static str,
    pub forbidden_crates: &'static [&'static str],
    pub action: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DependencyContractEntry {
    pub name: &'static str,
    pub kind: &'static str,
    pub owning_surface: &'static str,
    pub status: &'static str,
    pub enabled_by_default: bool,
    pub source: DependencySource,
    pub default_feature_profile: DependencyFeatureProfile,
    pub optional_feature_profiles: &'static [DependencyOptionalFeatureProfile],
    pub blocked_features: &'static [DependencyBlockedFeature],
    pub forbidden_transitive_dependencies: &'static [&'static str],
    pub minimum_smoke_test: &'static str,
    pub degradation_code: &'static str,
    pub status_fields: &'static [&'static str],
    pub diagnostic_command: &'static str,
    pub release_pin_decision: &'static str,
}

impl DependencyContractEntry {
    #[must_use]
    pub fn has_default_forbidden_transitives(self) -> bool {
        self.enabled_by_default
            && self
                .forbidden_transitive_dependencies
                .iter()
                .any(|candidate| FORBIDDEN_CRATES.contains(candidate))
    }

    #[must_use]
    pub fn readiness(self) -> &'static str {
        match (self.status, self.enabled_by_default) {
            ("accepted_default", true) | ("accepted_external", true) => "ready",
            ("optional_feature_gated", false) => "feature_gated",
            ("planned_not_linked", false) => "not_linked",
            _ => "review_required",
        }
    }

    #[must_use]
    pub fn is_franken_health_dependency(self) -> bool {
        matches!(
            self.name,
            "asupersync"
                | "frankensqlite"
                | "sqlmodel_rust"
                | "frankensearch"
                | "franken_networkx"
                | "franken_agent_detection"
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DependencyDriftPolicy {
    pub cargo_update_dry_run: &'static str,
    pub fail_conditions: &'static [&'static str],
    pub runtime_diagnostic_owner: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyDiagnosticsSummary {
    pub total_dependencies: usize,
    pub accepted_default_count: usize,
    pub accepted_external_count: usize,
    pub optional_feature_gated_count: usize,
    pub planned_not_linked_count: usize,
    pub default_enabled_count: usize,
    pub forbidden_default_hit_count: usize,
    pub blocked_feature_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyDiagnosticsReport {
    pub version: &'static str,
    pub schema: &'static str,
    pub matrix_revision: u32,
    pub source_bead: &'static str,
    pub source_plan_item: &'static str,
    pub default_feature_profile: &'static str,
    pub forbidden_crates: &'static [&'static str],
    pub entries: &'static [DependencyContractEntry],
    pub drift_policy: DependencyDriftPolicy,
    pub summary: DependencyDiagnosticsSummary,
}

impl DependencyDiagnosticsReport {
    #[must_use]
    pub fn gather() -> Self {
        let entries = DEPENDENCY_CONTRACT_ENTRIES;
        Self {
            version: env!("CARGO_PKG_VERSION"),
            schema: DEPENDENCY_DIAGNOSTICS_SCHEMA_V1,
            matrix_revision: DEPENDENCY_MATRIX_REVISION,
            source_bead: DEPENDENCY_MATRIX_SOURCE_BEAD,
            source_plan_item: DEPENDENCY_MATRIX_SOURCE_PLAN_ITEM,
            default_feature_profile: DEPENDENCY_MATRIX_DEFAULT_FEATURE_PROFILE,
            forbidden_crates: FORBIDDEN_CRATES,
            entries,
            drift_policy: DEPENDENCY_DRIFT_POLICY,
            summary: DependencyDiagnosticsSummary::from_entries(entries),
        }
    }
}

impl DependencyDiagnosticsSummary {
    #[must_use]
    pub fn from_entries(entries: &[DependencyContractEntry]) -> Self {
        Self {
            total_dependencies: entries.len(),
            accepted_default_count: count_status(entries, "accepted_default"),
            accepted_external_count: count_status(entries, "accepted_external"),
            optional_feature_gated_count: count_status(entries, "optional_feature_gated"),
            planned_not_linked_count: count_status(entries, "planned_not_linked"),
            default_enabled_count: entries
                .iter()
                .filter(|entry| entry.enabled_by_default)
                .count(),
            forbidden_default_hit_count: entries
                .iter()
                .filter(|entry| entry.has_default_forbidden_transitives())
                .count(),
            blocked_feature_count: entries
                .iter()
                .map(|entry| entry.blocked_features.len())
                .sum(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrankenHealthSummary {
    pub total_dependencies: usize,
    pub ready_count: usize,
    pub feature_gated_count: usize,
    pub not_linked_count: usize,
    pub default_enabled_count: usize,
    pub local_source_count: usize,
    pub forbidden_default_hit_count: usize,
    pub blocked_feature_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrankenDependencyHealth {
    pub name: &'static str,
    pub owning_surface: &'static str,
    pub status: &'static str,
    pub readiness: &'static str,
    pub enabled_by_default: bool,
    pub source: DependencySource,
    pub default_feature_profile: DependencyFeatureProfile,
    pub blocked_features: &'static [DependencyBlockedFeature],
    pub forbidden_transitive_dependencies: &'static [&'static str],
    pub degradation_code: &'static str,
    pub diagnostic_command: &'static str,
    pub minimum_smoke_test: &'static str,
    pub release_pin_decision: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrankenHealthReport {
    pub version: &'static str,
    pub schema: &'static str,
    pub healthy: bool,
    pub summary: FrankenHealthSummary,
    pub dependencies: Vec<FrankenDependencyHealth>,
}

impl FrankenHealthReport {
    #[must_use]
    pub fn gather() -> Self {
        let dependencies: Vec<FrankenDependencyHealth> = DEPENDENCY_CONTRACT_ENTRIES
            .iter()
            .copied()
            .filter(|entry| entry.is_franken_health_dependency())
            .map(FrankenDependencyHealth::from_entry)
            .collect();
        let summary = FrankenHealthSummary::from_dependencies(&dependencies);
        let healthy = summary.forbidden_default_hit_count == 0 && summary.not_linked_count == 0;

        Self {
            version: env!("CARGO_PKG_VERSION"),
            schema: FRANKEN_HEALTH_SCHEMA_V1,
            healthy,
            summary,
            dependencies,
        }
    }
}

impl FrankenDependencyHealth {
    #[must_use]
    pub fn from_entry(entry: DependencyContractEntry) -> Self {
        Self {
            name: entry.name,
            owning_surface: entry.owning_surface,
            status: entry.status,
            readiness: entry.readiness(),
            enabled_by_default: entry.enabled_by_default,
            source: entry.source,
            default_feature_profile: entry.default_feature_profile,
            blocked_features: entry.blocked_features,
            forbidden_transitive_dependencies: entry.forbidden_transitive_dependencies,
            degradation_code: entry.degradation_code,
            diagnostic_command: entry.diagnostic_command,
            minimum_smoke_test: entry.minimum_smoke_test,
            release_pin_decision: entry.release_pin_decision,
        }
    }
}

impl FrankenHealthSummary {
    #[must_use]
    pub fn from_dependencies(dependencies: &[FrankenDependencyHealth]) -> Self {
        Self {
            total_dependencies: dependencies.len(),
            ready_count: dependencies
                .iter()
                .filter(|dependency| dependency.readiness == "ready")
                .count(),
            feature_gated_count: dependencies
                .iter()
                .filter(|dependency| dependency.readiness == "feature_gated")
                .count(),
            not_linked_count: dependencies
                .iter()
                .filter(|dependency| dependency.readiness == "not_linked")
                .count(),
            default_enabled_count: dependencies
                .iter()
                .filter(|dependency| dependency.enabled_by_default)
                .count(),
            local_source_count: dependencies
                .iter()
                .filter(|dependency| {
                    matches!(dependency.source.kind, "path_dependency" | "path_patch")
                })
                .count(),
            forbidden_default_hit_count: dependencies
                .iter()
                .filter(|dependency| {
                    dependency.enabled_by_default
                        && dependency
                            .forbidden_transitive_dependencies
                            .iter()
                            .any(|candidate| FORBIDDEN_CRATES.contains(candidate))
                })
                .count(),
            blocked_feature_count: dependencies
                .iter()
                .map(|dependency| dependency.blocked_features.len())
                .sum(),
        }
    }
}

fn check_runtime() -> CheckResult {
    CheckResult::ok("runtime", "Asupersync runtime is available.")
}

fn check_workspace() -> CheckResult {
    CheckResult::warning(
        "workspace",
        "No workspace specified. Use --workspace or run from a workspace directory.",
        error_codes::WORKSPACE_NOT_SPECIFIED,
    )
}

fn check_database() -> CheckResult {
    CheckResult::warning(
        "database",
        "Database subsystem is not yet implemented.",
        error_codes::DATABASE_NOT_FOUND,
    )
}

fn check_search_index() -> CheckResult {
    CheckResult::warning(
        "search_index",
        "Search index subsystem is not yet implemented.",
        error_codes::INDEX_NOT_FOUND,
    )
}

fn check_cass() -> CheckResult {
    CheckResult::ok("cass", "CASS binary discovery is available.")
}

fn count_status(entries: &[DependencyContractEntry], status: &str) -> usize {
    entries
        .iter()
        .filter(|entry| entry.status == status)
        .count()
}

pub const DEPENDENCY_DRIFT_POLICY: DependencyDriftPolicy = DependencyDriftPolicy {
    cargo_update_dry_run: "advisory_only",
    fail_conditions: &[
        "introduces_forbidden_crate",
        "duplicates_franken_stack_family",
        "invalidates_accepted_feature_profile",
    ],
    runtime_diagnostic_owner: "EE-308",
};

pub const DEPENDENCY_CONTRACT_ENTRIES: &[DependencyContractEntry] = &[
    DependencyContractEntry {
        name: "asupersync",
        kind: "rust_crate",
        owning_surface: "ee-runtime",
        status: "accepted_default",
        enabled_by_default: true,
        source: DependencySource {
            kind: "registry",
            version: "0.3.1",
            path: "/dp/asupersync/asupersync",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: false,
            features: &["tracing-integration"],
        },
        optional_feature_profiles: &[DependencyOptionalFeatureProfile {
            name: "deterministic-tests",
            features: &["deterministic-mode"],
            status: "test_only",
        }],
        blocked_features: &[DependencyBlockedFeature {
            name: "sqlite",
            forbidden_crates: &["rusqlite"],
            action: "do_not_enable_in_ee",
        }],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "runtime_status_reports_asupersync_engine",
        degradation_code: "runtime_unavailable",
        status_fields: &[
            "runtime.engine",
            "runtime.profile",
            "runtime.async_boundary",
        ],
        diagnostic_command: "ee status --json",
        release_pin_decision: "Registry version 0.3.1 is accepted; /dp/asupersync remains the local source reference for API checks.",
    },
    DependencyContractEntry {
        name: "frankensqlite",
        kind: "rust_crate_family",
        owning_surface: "ee-db",
        status: "accepted_default",
        enabled_by_default: true,
        source: DependencySource {
            kind: "path_patch",
            version: "0.1.2",
            path: "/data/projects/frankensqlite",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: true,
            features: &["json", "fts5"],
        },
        optional_feature_profiles: &[DependencyOptionalFeatureProfile {
            name: "extended-sqlite-extensions",
            features: &["rtree", "session", "icu", "misc"],
            status: "not_in_default_profile",
        }],
        blocked_features: &[],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "default_feature_tree_excludes_forbidden_crates and migration tests",
        degradation_code: "storage_unavailable",
        status_fields: &["capabilities.storage", "degraded[].code"],
        diagnostic_command: "ee doctor --json",
        release_pin_decision: "Local path patches are accepted only for development; release must record a registry pin or ADR-backed local source policy.",
    },
    DependencyContractEntry {
        name: "sqlmodel_rust",
        kind: "rust_crate_family",
        owning_surface: "ee-db",
        status: "accepted_default",
        enabled_by_default: true,
        source: DependencySource {
            kind: "path_dependency",
            version: "0.2.2",
            path: "/data/projects/sqlmodel_rust",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: true,
            features: &["sqlmodel-core", "sqlmodel-frankensqlite"],
        },
        optional_feature_profiles: &[],
        blocked_features: &[DependencyBlockedFeature {
            name: "c-sqlite-tests",
            forbidden_crates: &["rusqlite"],
            action: "parity_only_do_not_enable_in_ee",
        }],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "migration_sequence_is_contiguous and repository tests",
        degradation_code: "storage_unavailable",
        status_fields: &["capabilities.storage", "database.schema_version"],
        diagnostic_command: "ee db status --json",
        release_pin_decision: "Local path dependencies are accepted only for development; release must record a registry pin or ADR-backed local source policy.",
    },
    DependencyContractEntry {
        name: "frankensearch",
        kind: "rust_crate_family",
        owning_surface: "ee-search",
        status: "accepted_default",
        enabled_by_default: true,
        source: DependencySource {
            kind: "path_dependency",
            version: "0.3.0",
            path: "/data/projects/frankensearch",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: false,
            features: &["hash", "storage", "model2vec", "lexical", "fts5"],
        },
        optional_feature_profiles: &[],
        blocked_features: &[
            DependencyBlockedFeature {
                name: "fastembed",
                forbidden_crates: &["tokio", "tokio-util", "hyper", "tower", "reqwest"],
                action: "block_embed_quality_until_upstream_has_clean_local_profile",
            },
            DependencyBlockedFeature {
                name: "download_api",
                forbidden_crates: &["reqwest"],
                action: "no_network_stack_in_core",
            },
        ],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "search/index smoke tests with deterministic hash embeddings",
        degradation_code: "search_unavailable",
        status_fields: &["capabilities.search", "index.generation", "degraded[].code"],
        diagnostic_command: "ee index status --json",
        release_pin_decision: "Local path dependencies are accepted only for development; release must record a registry pin or ADR-backed local source policy.",
    },
    DependencyContractEntry {
        name: "franken_networkx",
        kind: "rust_crate_family",
        owning_surface: "ee-graph",
        status: "optional_feature_gated",
        enabled_by_default: false,
        source: DependencySource {
            kind: "path_dependency",
            version: "0.1.0",
            path: "/data/projects/franken_networkx",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: false,
            features: &[],
        },
        optional_feature_profiles: &[DependencyOptionalFeatureProfile {
            name: "graph",
            features: &[
                "fnx-runtime/asupersync-integration",
                "fnx-classes",
                "fnx-algorithms",
            ],
            status: "accepted_optional",
        }],
        blocked_features: &[DependencyBlockedFeature {
            name: "ftui-integration",
            forbidden_crates: &[],
            action: "not_part_of_ee_graph_contract",
        }],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "graph projection and centrality tests",
        degradation_code: "graph_unavailable",
        status_fields: &["capabilities.graph", "graph.snapshot_generation"],
        diagnostic_command: "ee graph status --json",
        release_pin_decision: "Local path dependencies are accepted only for development; release must record a registry pin or ADR-backed local source policy.",
    },
    DependencyContractEntry {
        name: "coding_agent_session_search",
        kind: "external_process",
        owning_surface: "ee-cass",
        status: "accepted_external",
        enabled_by_default: true,
        source: DependencySource {
            kind: "external_binary",
            version: "0.4.1",
            path: "/dp/coding_agent_session_search",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: false,
            features: &["cass --robot", "cass --json"],
        },
        optional_feature_profiles: &[],
        blocked_features: &[DependencyBlockedFeature {
            name: "interactive-output",
            forbidden_crates: &[],
            action: "never_parse_bare_cass_output",
        }],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "CASS fixture parsing for capabilities, health, and API version",
        degradation_code: "cass_unavailable",
        status_fields: &["capabilities.cass", "degraded[].code"],
        diagnostic_command: "ee import cass --dry-run --json",
        release_pin_decision: "External process contract is accepted; no Rust crate is linked into ee.",
    },
    DependencyContractEntry {
        name: "toon_rust",
        kind: "rust_crate",
        owning_surface: "ee-output",
        status: "accepted_default",
        enabled_by_default: true,
        source: DependencySource {
            kind: "path_dependency",
            version: "0.2.2",
            path: "/data/projects/toon_rust",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: false,
            features: &[],
        },
        optional_feature_profiles: &[],
        blocked_features: &[],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "TOON renderer round-trip and golden parity tests",
        degradation_code: "toon_unavailable",
        status_fields: &["capabilities.output.toon"],
        diagnostic_command: "ee status --json",
        release_pin_decision: "Local path dependency is accepted only for development; release must record a registry pin or ADR-backed local source policy.",
    },
    DependencyContractEntry {
        name: "franken_agent_detection",
        kind: "rust_crate",
        owning_surface: "ee-agent-detect",
        status: "accepted_default",
        enabled_by_default: true,
        source: DependencySource {
            kind: "path_dependency",
            version: "0.1.3",
            path: "/data/projects/franken_agent_detection",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: false,
            features: &[],
        },
        optional_feature_profiles: &[],
        blocked_features: &[DependencyBlockedFeature {
            name: "connector-backed-scans",
            forbidden_crates: &[],
            action: "requires_privacy_and_dependency_gates_before_default_use",
        }],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "agent detection fixture tests with root overrides",
        degradation_code: "agent_detection_unavailable",
        status_fields: &["capabilities.agent_detection"],
        diagnostic_command: "ee agent sources --json",
        release_pin_decision: "Local path dependency is accepted only for development; release must record a registry pin or ADR-backed local source policy.",
    },
    DependencyContractEntry {
        name: "fastmcp-rust",
        kind: "planned_rust_crate",
        owning_surface: "ee-mcp",
        status: "planned_not_linked",
        enabled_by_default: false,
        source: DependencySource {
            kind: "not_linked",
            version: "unresolved",
            path: "/dp/fastmcp-rust",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: false,
            features: &[],
        },
        optional_feature_profiles: &[DependencyOptionalFeatureProfile {
            name: "mcp",
            features: &["stdio"],
            status: "blocked_until_dependency_audit",
        }],
        blocked_features: &[],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "MCP stdio initialize/tools/resources golden tests",
        degradation_code: "mcp_unavailable",
        status_fields: &["capabilities.mcp"],
        diagnostic_command: "ee doctor --json",
        release_pin_decision: "Do not link before a clean feature-tree audit and ADR-backed release pin.",
    },
];

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

    #[test]
    fn doctor_report_gather_returns_checks() -> TestResult {
        let report = DoctorReport::gather();

        ensure(
            report.checks.len() >= 5,
            true,
            "should have at least 5 checks",
        )?;

        let runtime = report.checks.iter().find(|c| c.name == "runtime");
        ensure(runtime.is_some(), true, "runtime check exists")?;
        ensure(
            runtime.map(|c| c.severity),
            Some(CheckSeverity::Ok),
            "runtime is ok",
        )?;

        Ok(())
    }

    #[test]
    fn doctor_report_overall_healthy_reflects_all_checks() -> TestResult {
        let report = DoctorReport::gather();

        let has_issues = report.checks.iter().any(|c| !c.severity.is_healthy());

        ensure(
            report.overall_healthy,
            !has_issues,
            "overall_healthy matches check status",
        )
    }

    #[test]
    fn check_result_ok_has_no_error_code() -> TestResult {
        let check = CheckResult::ok("test", "All good");
        ensure(check.error_code.is_none(), true, "ok has no error code")?;
        ensure(check.repair.is_none(), true, "ok has no repair")
    }

    #[test]
    fn check_result_warning_has_error_code_and_repair() -> TestResult {
        let check = CheckResult::warning("test", "Issue found", error_codes::DATABASE_NOT_FOUND);
        ensure(check.error_code.is_some(), true, "warning has error code")?;
        ensure(check.repair.is_some(), true, "warning has repair from code")
    }

    #[test]
    fn check_severity_strings_are_stable() -> TestResult {
        ensure(CheckSeverity::Ok.as_str(), "ok", "ok")?;
        ensure(CheckSeverity::Warning.as_str(), "warning", "warning")?;
        ensure(CheckSeverity::Error.as_str(), "error", "error")
    }

    #[test]
    fn fix_plan_contains_only_fixable_issues() -> TestResult {
        let report = DoctorReport::gather();
        let plan = report.to_fix_plan();

        for step in &plan.steps {
            ensure(!step.command.is_empty(), true, "step has a command")?;
            ensure(
                step.severity != CheckSeverity::Ok,
                true,
                "step is not an ok check",
            )?;
        }

        Ok(())
    }

    #[test]
    fn fix_plan_steps_are_ordered() -> TestResult {
        let report = DoctorReport::gather();
        let plan = report.to_fix_plan();

        for (idx, step) in plan.steps.iter().enumerate() {
            ensure(step.order, idx + 1, "step order is sequential")?;
        }

        Ok(())
    }

    #[test]
    fn fix_plan_counts_match() -> TestResult {
        let report = DoctorReport::gather();
        let plan = report.to_fix_plan();

        let unhealthy_count = report
            .checks
            .iter()
            .filter(|c| !c.severity.is_healthy())
            .count();
        ensure(plan.total_issues, unhealthy_count, "total_issues matches")?;

        let fixable_count = report
            .checks
            .iter()
            .filter(|c| !c.severity.is_healthy() && c.repair.is_some())
            .count();
        ensure(plan.fixable_issues, fixable_count, "fixable_issues matches")?;
        ensure(plan.steps.len(), fixable_count, "steps count matches")?;

        Ok(())
    }

    #[test]
    fn fix_plan_is_empty_when_all_healthy() -> TestResult {
        let report = DoctorReport {
            version: "0.1.0",
            overall_healthy: true,
            checks: vec![
                CheckResult::ok("test1", "All good"),
                CheckResult::ok("test2", "Also good"),
            ],
        };
        let plan = report.to_fix_plan();

        ensure(plan.is_empty(), true, "plan is empty when all healthy")?;
        ensure(plan.total_issues, 0, "no total issues")?;
        ensure(plan.fixable_issues, 0, "no fixable issues")
    }

    #[test]
    fn dependency_diagnostics_report_summarizes_matrix() -> TestResult {
        let report = DependencyDiagnosticsReport::gather();

        ensure(
            report.schema,
            DEPENDENCY_DIAGNOSTICS_SCHEMA_V1,
            "dependency schema",
        )?;
        ensure(
            report.source_bead,
            DEPENDENCY_MATRIX_SOURCE_BEAD,
            "source bead",
        )?;
        ensure(report.entries.len(), 9, "matrix row count")?;
        ensure(
            report.summary.total_dependencies,
            9,
            "summary total dependencies",
        )?;
        ensure(
            report.summary.accepted_default_count,
            6,
            "accepted default count",
        )?;
        ensure(
            report.summary.forbidden_default_hit_count,
            0,
            "default forbidden hit count",
        )?;
        ensure(
            report.summary.blocked_feature_count,
            7,
            "blocked feature count",
        )?;

        Ok(())
    }

    #[test]
    fn dependency_diagnostics_rows_keep_required_entries() -> TestResult {
        let report = DependencyDiagnosticsReport::gather();

        for expected in [
            "asupersync",
            "frankensqlite",
            "sqlmodel_rust",
            "frankensearch",
            "franken_networkx",
            "coding_agent_session_search",
            "toon_rust",
            "franken_agent_detection",
            "fastmcp-rust",
        ] {
            ensure(
                report.entries.iter().any(|entry| entry.name == expected),
                true,
                &format!("dependency row {expected} exists"),
            )?;
        }

        Ok(())
    }

    #[test]
    fn default_dependency_rows_have_no_forbidden_hits() -> TestResult {
        let report = DependencyDiagnosticsReport::gather();

        for entry in report
            .entries
            .iter()
            .filter(|entry| entry.enabled_by_default)
        {
            ensure(
                entry.has_default_forbidden_transitives(),
                false,
                &format!("{} has no forbidden default transitives", entry.name),
            )?;
        }

        Ok(())
    }

    #[test]
    fn franken_health_report_tracks_default_and_feature_gated_stack() -> TestResult {
        let report = FrankenHealthReport::gather();

        ensure(report.schema, FRANKEN_HEALTH_SCHEMA_V1, "franken schema")?;
        ensure(report.healthy, true, "franken health")?;
        ensure(
            report.summary.total_dependencies,
            6,
            "franken dependency count",
        )?;
        ensure(report.summary.ready_count, 5, "ready count")?;
        ensure(report.summary.feature_gated_count, 1, "feature gated count")?;
        ensure(report.summary.not_linked_count, 0, "not linked count")?;
        ensure(
            report.summary.forbidden_default_hit_count,
            0,
            "forbidden default hits",
        )?;

        let graph = report
            .dependencies
            .iter()
            .find(|dependency| dependency.name == "franken_networkx")
            .ok_or_else(|| "franken_networkx health row missing".to_string())?;
        ensure(graph.readiness, "feature_gated", "graph readiness")
    }
}
