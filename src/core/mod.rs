use std::future::Future;
use std::time::Duration;

use crate::models::{
    ARTIFACT_SUMMARY_SCHEMA_V1, ERROR_SCHEMA_V2, INSTALL_CHECK_SCHEMA_V1, INSTALL_PLAN_SCHEMA_V1,
    MESH_EVENT_SCHEMA_V1, MESH_PEER_GROUP_BINDING_SCHEMA_V1, MESH_PEER_POLICY_SCHEMA_V1,
    MESH_POLICY_DECISION_SCHEMA_V1, MESH_POLICY_FAILURE_SURFACE_SCHEMA_V1,
    MESH_STORAGE_STATUS_SCHEMA_V1, RESPONSE_SCHEMA_V2, SINGLEFLIGHT_KEY_SCHEMA_V1,
    SINGLEFLIGHT_POSTURE_SCHEMA_V1, SYMBOL_EVIDENCE_LINKS_SCHEMA_V1, SYMBOL_SNAPSHOT_SCHEMA_V1,
    UPDATE_PLAN_SCHEMA_V1,
};

pub mod adaptive_scheduler;
pub mod agent_detect;
pub mod agent_docs;
pub mod agentsmd;
pub mod artifact;
pub mod artifact_relocation;
pub mod ask;
pub mod attest;
pub mod audit;
pub mod audit_lane;
pub mod backup;
pub mod bayes;
pub mod bayes_backfill;
pub mod beads_integrity;
pub mod budget;
pub mod budget_delta_recommender;
pub mod capabilities;
pub mod cass_prefetch;
pub mod causal;
pub mod certificate;
pub mod check;
pub mod claims;
pub mod completion_audit;
pub mod config_explain;
pub mod config_surface;
pub mod conformal;
pub mod contention;
pub mod context;
pub mod context_delta;
pub mod contradiction_detect;
pub mod contradiction_guard;
pub mod contradiction_resolution;
pub mod curate;
pub mod decide;
pub mod degraded_aggregation;
pub mod degraded_honesty;
pub mod derived_asset;
pub mod derived_asset_freshness;
pub mod determinism;
pub mod disk_pressure;
pub mod docs_bootstrap;
pub mod doctor;
pub mod doctor_fixers;
pub mod doctor_runtime;
pub mod duplicate_work_detector;
pub mod economy;
pub mod effect;
pub mod environment_attestation;
pub mod error_diagnosis;
pub mod error_recall;
pub mod explanation_latency_budget;
pub mod feedback;
pub mod focus;
pub mod focus_suggest;
pub mod git_ahead;
pub mod global_promotion;
pub mod global_store;
pub mod graph_audit;
pub mod graph_diff;
pub mod graph_memory_budget;
pub mod graph_telemetry;
pub mod handoff;
pub mod health;
pub mod house_rules;
pub mod hygiene_beads_state;
pub mod hygiene_classifier;
pub mod hygiene_coordination;
pub mod impact;
pub mod index;
pub mod influence;
pub mod init;
pub mod install;
pub mod journal;
pub mod jsonl_import;
pub mod lab;
pub mod learn;
pub mod legacy_import;
pub mod memory;
pub mod memory_debt;
pub mod memory_drift;
pub mod memory_lifecycle;
pub mod memory_scope;
pub mod model;
pub mod orient;
pub mod outcome;
pub mod ownership_snapshot;
pub mod path_safety;
pub mod perf_forensics;
pub mod perf_live;
pub mod plan;
pub mod preflight;
pub mod preflight_guard;
pub mod primer;
pub mod procedure;
pub mod profile;
pub mod proof_verify;
pub mod provenance_health;
pub mod qos;
pub mod quarantine;
pub mod query_miss_cluster;
pub mod read_fence;
pub mod recall;
pub mod recorder;
pub mod rehearse;
pub mod repro;
pub mod resume;
pub mod retrieval_affinity;
pub mod rule;
pub mod sandbox;
pub mod search;
pub mod sentinel;
pub mod session_budget;
pub mod shadow_tuning;
pub mod singleflight;
pub mod situation;
pub mod source_run;
pub mod sprt;
pub mod status;
pub mod store_integrity;
pub mod streams;
pub mod subscribe;
pub mod suggest_links;
pub mod support_bundle;
pub mod swarm_brief;
pub mod swarm_brief_delta;
pub mod swarm_next_action;
pub mod symbol_graph;
pub mod tailscale_probe;
pub mod task_frame;
pub mod trauma_guard;
pub mod tripwire;
pub mod trust_report;
pub mod unsafe_claim_planner;
pub mod verify;
pub mod verify_ledger;
pub mod why;
pub mod witness_retention;
pub mod workspace;
pub mod write_owner;

pub use budget::{BudgetDimension, BudgetExceeded, BudgetSnapshot, RequestBudget};
pub use context::{AccessLevel, CapabilitySet, CommandCancellation, CommandContext};
pub use outcome::{
    CliCancelReason, CliOutcomeClass, CliOutcomeSummary, EXIT_CANCELLED, EXIT_PANICKED,
    OutcomeFeedbackSummary, OutcomeRecordOptions, OutcomeRecordReport, OutcomeRecordStatus,
    outcome_class, outcome_exit_code, record_outcome,
};
pub use write_owner::{
    WRITE_OWNER_STATUS_SCHEMA_V1, WRITE_SPOOL_BACKPRESSURE_CODE,
    WRITE_SPOOL_BACKPRESSURE_SCHEMA_V1, WRITE_SPOOL_STATUS_SCHEMA_V1, WriteHandle, WriteOperation,
    WriteOwner, WriteOwnerStatus, WriteResult, WriteSpool, WriteSpoolBackpressureError,
    WriteSpoolBackpressureReason, WriteSpoolBatch, WriteSpoolConfig, WriteSpoolDurability,
    WriteSpoolFailure, WriteSpoolIntent, WriteSpoolIntentKind, WriteSpoolRecord,
    WriteSpoolRecordStatus, WriteSpoolStatus, WriteSpoolTicket,
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
        BuildFeature::new(
            "differential-networkx",
            cfg!(feature = "differential-networkx"),
        ),
        BuildFeature::new("mcp", cfg!(feature = "mcp")),
        BuildFeature::new("serve", cfg!(feature = "serve")),
        BuildFeature::new("science-analytics", cfg!(feature = "science-analytics")),
    ]
}

#[must_use]
pub fn supported_schemas() -> Vec<SupportedSchema> {
    vec![
        SupportedSchema::new("response", RESPONSE_SCHEMA_V2),
        SupportedSchema::new("error", ERROR_SCHEMA_V2),
        SupportedSchema::new(
            "typed_memory_fields",
            crate::models::memory::TYPED_MEMORY_FIELDS_SCHEMA_V2,
        ),
        SupportedSchema::new("decide_record", decide::DECIDE_RECORD_SCHEMA_V1),
        SupportedSchema::new("decide_list", decide::DECIDE_LIST_SCHEMA_V1),
        SupportedSchema::new("decide_revisit", decide::DECIDE_REVISIT_SCHEMA_V1),
        SupportedSchema::new("version_provenance", VERSION_PROVENANCE_SCHEMA_V1),
        SupportedSchema::new("symbol_snapshot", SYMBOL_SNAPSHOT_SCHEMA_V1),
        SupportedSchema::new("symbol_evidence_links", SYMBOL_EVIDENCE_LINKS_SCHEMA_V1),
        SupportedSchema::new(
            "memory_drift_snapshot",
            memory_drift::MEMORY_DRIFT_SNAPSHOT_SCHEMA_V1,
        ),
        SupportedSchema::new(
            "memory_drift_queue",
            memory_drift::MEMORY_DRIFT_QUEUE_SCHEMA_V1,
        ),
        SupportedSchema::new(
            "memory_drift_report",
            memory_drift::MEMORY_DRIFT_REPORT_SCHEMA_V1,
        ),
        SupportedSchema::new("install_check", INSTALL_CHECK_SCHEMA_V1),
        SupportedSchema::new("install_plan", INSTALL_PLAN_SCHEMA_V1),
        SupportedSchema::new("host_profile", profile::HOST_PROFILE_PROBE_SCHEMA_V1),
        SupportedSchema::new(
            "profile_config_plan",
            profile::PROFILE_CONFIG_PLAN_SCHEMA_V1,
        ),
        SupportedSchema::new("config_get", config_surface::CONFIG_GET_SCHEMA_V1),
        SupportedSchema::new("config_set", config_surface::CONFIG_SET_SCHEMA_V1),
        SupportedSchema::new(
            "verification_recipe",
            profile::VERIFICATION_RECIPE_SCHEMA_V1,
        ),
        SupportedSchema::new("runtime_profile", profile::RUNTIME_PROFILE_SCHEMA_V1),
        SupportedSchema::new("update_plan", UPDATE_PLAN_SCHEMA_V1),
        SupportedSchema::new("artifact_summary", ARTIFACT_SUMMARY_SCHEMA_V1),
        SupportedSchema::new(
            "derived_asset_store_summary",
            derived_asset::DERIVED_ASSET_STORE_SUMMARY_SCHEMA_V1,
        ),
        SupportedSchema::new(
            "artifact_relocation",
            artifact_relocation::ARTIFACT_RELOCATION_SCHEMA_V1,
        ),
        SupportedSchema::new("compare_result", perf_forensics::COMPARE_RESULT_SCHEMA_V1),
        SupportedSchema::new("budget_check", perf_forensics::BUDGET_CHECK_SCHEMA_V1),
        SupportedSchema::new("perf_live", perf_live::PERF_LIVE_SCHEMA_V1),
        SupportedSchema::new("swarm_brief", swarm_brief::SWARM_BRIEF_SCHEMA_V1),
        SupportedSchema::new("insights", "ee.insights.v1"),
        SupportedSchema::new("context_pack_dna", "ee.context.pack_dna.v1"),
        SupportedSchema::new("why_causal", "ee.why.causal.v1"),
        SupportedSchema::new("health_structural", "ee.health.structural.v1"),
        SupportedSchema::new("status_skyline", "ee.status.skyline.v1"),
        SupportedSchema::new("memory_impact_analysis", "ee.memory.impact_analysis.v1"),
        SupportedSchema::new("proximity", "ee.proximity.v1"),
        SupportedSchema::new(
            "graph_witness_prune",
            witness_retention::WITNESS_PRUNE_REPORT_SCHEMA_V1,
        ),
        SupportedSchema::new("why_augmented", "ee.why.v1"),
        SupportedSchema::new("context_augmented", "ee.context.v1"),
        SupportedSchema::new("mesh_event", MESH_EVENT_SCHEMA_V1),
        SupportedSchema::new("mesh_peer_group_binding", MESH_PEER_GROUP_BINDING_SCHEMA_V1),
        SupportedSchema::new("mesh_peer_policy", MESH_PEER_POLICY_SCHEMA_V1),
        SupportedSchema::new("mesh_policy_decision", MESH_POLICY_DECISION_SCHEMA_V1),
        SupportedSchema::new(
            "mesh_policy_failure_surface",
            MESH_POLICY_FAILURE_SURFACE_SCHEMA_V1,
        ),
        SupportedSchema::new("mesh_storage_status", MESH_STORAGE_STATUS_SCHEMA_V1),
        SupportedSchema::new("singleflight_key", SINGLEFLIGHT_KEY_SCHEMA_V1),
        SupportedSchema::new("singleflight_posture", SINGLEFLIGHT_POSTURE_SCHEMA_V1),
        SupportedSchema::new("proof_check", proof_verify::PROOF_CHECK_SCHEMA_V1),
        SupportedSchema::new(
            "disk_pressure_diagnostics",
            disk_pressure::DISK_PRESSURE_DIAGNOSTICS_SCHEMA_V1,
        ),
        SupportedSchema::new(
            "agent_harness_log_classifier",
            disk_pressure::AGENT_HARNESS_LOG_CLASSIFIER_SCHEMA_V1,
        ),
        SupportedSchema::new(
            "artifact_retention_diagnostics",
            disk_pressure::ARTIFACT_RETENTION_DIAGNOSTICS_SCHEMA_V1,
        ),
        SupportedSchema::new(
            "build_admission_diagnostics",
            disk_pressure::BUILD_ADMISSION_DIAGNOSTICS_SCHEMA_V1,
        ),
        SupportedSchema::new(
            "pack_quality_report",
            crate::eval::PACK_QUALITY_REPORT_SCHEMA_V1,
        ),
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

/// Run a synchronous CLI operation with an explicit, bounded production
/// request context.
///
/// The context is minted by [`asupersync::runtime::Runtime`] so it inherits
/// the runtime's drivers and capability mask. This is the production root for
/// sync adapters that must pass a caller-owned [`asupersync::Cx`] into async
/// search, pack, or index work; those adapters must not recover authority via
/// `Cx::current()` or manufacture it with a test constructor.
pub fn run_cli_with_cx<F, Fut, T>(timeout: Duration, operation: F) -> RuntimeResult<T>
where
    F: FnOnce(asupersync::Cx) -> Fut,
    Fut: Future<Output = T>,
{
    let runtime = build_cli_runtime()?;
    let bootstrap = runtime.request_cx_with_budget(asupersync::Budget::INFINITE);
    let budget = bootstrap.budget_for_timeout(timeout);
    let cx = runtime.request_cx_with_budget(budget);
    Ok(runtime.block_on(async move {
        let _ambient = asupersync::Cx::set_current(Some(cx.clone()));
        operation(cx).await
    }))
}

/// Repair guidance for a storeless-workspace miss with safe ordering
/// (bd-workspace-miss-init-suggestion-sfjvq): re-check the addressing
/// first, point at nearby populated stores when any exist, and mention
/// `ee init` LAST and conditionally — state creation must never read as
/// the first remedy for a lookup miss (agents follow error hints
/// mechanically, and a mechanical `ee init` at a wrong path plants a junk
/// store that discovery then finds forever).
#[must_use]
pub fn storeless_workspace_repair(database_path: &std::path::Path) -> String {
    let workspace_dir = database_path
        .parent()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf);
    let nearby = workspace_dir.as_deref().map(|dir| {
        crate::core::orient::discover_nearby_stores(
            dir,
            std::time::Duration::from_millis(crate::core::orient::NEARBY_STORE_SCAN_BUDGET_MS),
        )
    });
    let looked_for = database_path.display();
    if let Some(scan) = nearby
        && let Some(best) = scan.stores.first()
    {
        return format!(
            "Re-check --workspace addressing (looked for {looked_for}); a populated store exists at {} ({} docs) — retarget with --workspace {}. Only if you intended to create a NEW store here: ee init --workspace .",
            best.workspace_root, best.documents, best.workspace_root,
        );
    }
    format!(
        "Re-check --workspace addressing (looked for {looked_for}). Only if you intended to create a NEW store here: ee init --workspace ."
    )
}

/// Canonical storeless-workspace miss error
/// (bd-workspace-miss-init-suggestion-sfjvq): the stable
/// `workspace_store_missing` identity with its own process exit code, the
/// exact looked-for path in the message, and the safe recovery ordering from
/// [`storeless_workspace_repair`] — re-check addressing first, nearby
/// populated stores second, `ee init` last and explicitly conditional.
#[must_use]
pub fn storeless_workspace_error(database_path: &std::path::Path) -> crate::models::DomainError {
    crate::models::DomainError::WorkspaceStoreMissing {
        message: format!("Database not found at {}", database_path.display()),
        repair: Some(storeless_workspace_repair(database_path)),
    }
}

/// Serialize a value to JSON, returning a stable error envelope on failure.
///
/// Use for display-only `to_json()` methods where failure should produce valid JSON
/// rather than an empty string. For machine-facing APIs, prefer returning `Result`.
#[must_use]
pub fn serialize_or_error<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|error| {
        serde_json::json!({
            "error": "serialization_failed",
            "message": error.to_string(),
        })
        .to_string()
    })
}

/// Serialize a value to pretty JSON, returning a stable error envelope on failure.
#[must_use]
pub fn serialize_pretty_or_error<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|error| {
        serde_json::to_string_pretty(&serde_json::json!({
            "error": "serialization_failed",
            "message": error.to_string(),
        }))
        .unwrap_or_else(|_| {
            r#"{"error":"serialization_failed","message":"serialization fallback failed"}"#
                .to_owned()
        })
    })
}

/// Convert a duration to milliseconds without wrapping on long-running processes.
#[must_use]
pub(crate) fn duration_millis_saturating(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::time::Duration;

    use asupersync::{LabConfig, LabRuntime};

    use super::{
        BUILD_TIMESTAMP_POLICY, RuntimeProfile, VERSION_PROVENANCE_SCHEMA_V1, VersionReport,
        build_features, build_info, clean_build_metadata, db_migration_range,
        duration_millis_saturating, parse_build_bool, run_cli_future, runtime_status,
        serialize_or_error, serialize_pretty_or_error, supported_schemas,
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
            &"eidetic-engine",
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
                "differential-networkx",
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
            &vec![
                "response",
                "error",
                "typed_memory_fields",
                "decide_record",
                "decide_list",
                "decide_revisit",
                "version_provenance",
                "symbol_snapshot",
                "symbol_evidence_links",
                "memory_drift_snapshot",
                "memory_drift_queue",
                "memory_drift_report",
                "install_check",
                "install_plan",
                "host_profile",
                "profile_config_plan",
                "config_get",
                "config_set",
                "verification_recipe",
                "runtime_profile",
                "update_plan",
                "artifact_summary",
                "derived_asset_store_summary",
                "artifact_relocation",
                "compare_result",
                "budget_check",
                "perf_live",
                "swarm_brief",
                "insights",
                "context_pack_dna",
                "why_causal",
                "health_structural",
                "status_skyline",
                "memory_impact_analysis",
                "proximity",
                "graph_witness_prune",
                "why_augmented",
                "context_augmented",
                "mesh_event",
                "mesh_peer_group_binding",
                "mesh_peer_policy",
                "mesh_policy_decision",
                "mesh_policy_failure_surface",
                "mesh_storage_status",
                "singleflight_key",
                "singleflight_posture",
                "proof_check",
                "disk_pressure_diagnostics",
                "agent_harness_log_classifier",
                "artifact_retention_diagnostics",
                "build_admission_diagnostics",
                "pack_quality_report",
            ],
            "schema names",
        )
    }

    #[test]
    fn supported_schemas_advertise_current_response_envelope() -> TestResult {
        let response = supported_schemas()
            .into_iter()
            .find(|schema| schema.name == "response")
            .ok_or_else(|| "response schema should be advertised".to_string())?;
        let legacy_schema = ["ee", "response", "v1"].join(".");

        ensure_equal(
            &response.schema,
            &crate::models::RESPONSE_SCHEMA_V2,
            "response schema id",
        )?;
        ensure(
            response.schema != legacy_schema,
            "supported schemas must not advertise the legacy response envelope",
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
            &clean_build_metadata(Some("/tmp/build/path-like-value")),
            &None,
            "path metadata must be redacted",
        )?;
        ensure_equal(
            &clean_build_metadata(Some("BUILD_META=value")),
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

    #[test]
    fn serialize_or_error_produces_valid_json_on_success() -> TestResult {
        #[derive(serde::Serialize)]
        struct TestData {
            name: String,
            count: u32,
        }
        let data = TestData {
            name: "test".to_string(),
            count: 42,
        };
        let json = serialize_or_error(&data);
        ensure(
            json.contains("\"name\":\"test\""),
            "should contain name field",
        )?;
        ensure(json.contains("\"count\":42"), "should contain count field")
    }

    #[test]
    fn serialize_or_error_produces_error_envelope_on_failure() -> TestResult {
        use std::collections::HashMap;
        let mut map: HashMap<Vec<u8>, String> = HashMap::new();
        map.insert(vec![0xFF], "invalid key".to_string());
        let json = serialize_or_error(&map);
        let parsed: serde_json::Value =
            serde_json::from_str(&json).map_err(|error| error.to_string())?;
        ensure(
            parsed.get("error").and_then(serde_json::Value::as_str) == Some("serialization_failed"),
            "should contain error marker in valid JSON",
        )
    }

    #[test]
    fn serialization_error_fallback_escapes_error_messages() -> TestResult {
        use serde::ser::{Error as _, Serialize, Serializer};

        struct FailsWithQuotedMessage;

        impl Serialize for FailsWithQuotedMessage {
            fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                Err(S::Error::custom("bad \"quoted\" serializer\nmessage"))
            }
        }

        let compact = serialize_or_error(&FailsWithQuotedMessage);
        let compact_json: serde_json::Value =
            serde_json::from_str(&compact).map_err(|error| error.to_string())?;
        ensure_equal(
            &compact_json
                .get("error")
                .and_then(serde_json::Value::as_str),
            &Some("serialization_failed"),
            "compact fallback error code",
        )?;
        ensure(
            compact_json
                .get("message")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|message| message.contains("\"quoted\"")),
            "compact fallback preserves quoted error text",
        )?;

        let pretty = serialize_pretty_or_error(&FailsWithQuotedMessage);
        let pretty_json: serde_json::Value =
            serde_json::from_str(&pretty).map_err(|error| error.to_string())?;
        ensure_equal(
            &pretty_json.get("error").and_then(serde_json::Value::as_str),
            &Some("serialization_failed"),
            "pretty fallback error code",
        )
    }

    #[test]
    fn serialize_pretty_or_error_produces_formatted_json() -> TestResult {
        #[derive(serde::Serialize)]
        struct TestData {
            value: u32,
        }
        let data = TestData { value: 1 };
        let json = serialize_pretty_or_error(&data);
        ensure(json.contains('\n'), "pretty output should have newlines")
    }

    #[test]
    fn duration_millis_saturating_caps_overflow() -> TestResult {
        ensure_equal(
            &duration_millis_saturating(Duration::from_millis(42)),
            &42,
            "ordinary millisecond duration",
        )?;
        ensure_equal(
            &duration_millis_saturating(Duration::from_secs(u64::MAX)),
            &u64::MAX,
            "overflowing millisecond duration saturates",
        )
    }
}
