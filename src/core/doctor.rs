//! Doctor command handler (EE-025, EE-241).
//!
//! Performs health checks on workspace subsystems and returns a structured
//! report with issues and repair suggestions.
//!
//! The `--fix-plan` flag (EE-241) outputs a structured repair plan that
//! agents can execute step-by-step.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::Serialize;
#[cfg(unix)]
use std::os::unix::{fs::FileTypeExt, net::UnixStream};

use crate::config::{EmbeddingTrapEnvVar, EnvVar, read_env_var, read_env_var_os, workspace_config};
use crate::core::agent_detect::{AgentInventoryReport, AgentInventoryStatus};
use crate::db::{
    CreateMemoryInput, DbConnection, ForeignKeyCheckResult, IntegrityCheckResult,
    ProvenanceSampleVerificationReport, ReferenceIntegrityReport,
    shard::{
        ShardFanoutPosture, ShardFanoutResolverInput, resolve_shard_fanout_status,
        shard_fanout_enabled_from_env_value,
    },
};
use crate::graph::numa_pin::{
    NUMA_PIN_DISABLE_ENV, NUMA_PIN_NODE_ENV, NUMA_PIN_POPULATE_ENV, NumaPinConfig, NumaPinResult,
    pin_snapshot_blob,
};
use crate::mesh::hello_responder::HelloResponderStatusReport;
use crate::mesh::repair_action_graph::{
    ActionKind, ExecutionContext, ExpectedOutcome, Priority, REPAIR_ACTION_GRAPH_SCHEMA_V1,
    RepairAction, RepairActionGraph, build_repair_action_graph,
};
use crate::models::error_codes::{self, ErrorCode};
use crate::models::{
    EMBEDDING_POSTURE_MODE_NEURAL_LOCAL_PENDING, SingleFlightPostureReport, TrustClass,
};
use crate::search::lexical_ram_tier::{
    LEXICAL_RAM_TIER_HUGEPAGES_ENV, LEXICAL_RAM_TIER_PIN_RAM_ENV, LexicalRamTierConfig,
    LexicalRamTierResult, pin_lexical_index_files,
};

use super::budget_delta_recommender::{
    HostCalibrationPostureReport, gather_host_calibration_posture,
};
use super::build_cli_runtime;
use super::curate::stable_workspace_id;
use super::index::{DEFAULT_INDEX_SUBDIR, IndexHealth, IndexStatusOptions, get_index_status};
use super::qos::{QosLaneSummary, summarize_qos_lane_registry};
use super::singleflight::singleflight_posture_report;
use super::status::{
    FlightRecorderStatusReport, default_workspace_path, gather_flight_recorder_status,
    gather_rch_verify_ledger_status, gather_rch_worker_pressure, probe_cass_capability,
};
use super::swarm_brief::RchWorkerPressureReport;
use super::tailscale_probe::{
    SystemTailscaleCliProbeRunner, SystemTailscaleSocketProbeRunner, TailscaleCliProbeConfig,
    TailscaleLocalReport, TailscalePlatform, TailscaleSocketProbeConfig,
    probe_tailscale_local_with_runners, tailscale_probe_timeout_ms_from_env_value,
};
use super::verify::{VerificationPostureReport, gather_verification_posture};
use super::verify_ledger::RchVerifyLedgerStatusReport;

pub const DEPENDENCY_DIAGNOSTICS_SCHEMA_V1: &str = "ee.diag.dependencies.v1";
pub const FRANKEN_HEALTH_SCHEMA_V1: &str = "ee.doctor.franken_health.v1";
pub const INTEGRITY_DIAGNOSTICS_SCHEMA_V1: &str = "ee.diag.integrity.v1";
pub const DEPENDENCY_MATRIX_REVISION: u32 = 1;
pub const DEPENDENCY_MATRIX_SOURCE_BEAD: &str = "eidetic_engine_cli-ilcq";
pub const DEPENDENCY_MATRIX_SOURCE_PLAN_ITEM: &str = "EE-307";
pub const DEPENDENCY_MATRIX_DEFAULT_FEATURE_PROFILE: &str = "default";
pub const INTEGRITY_CANARY_MEMORY_ID: &str = "mem_integritycanary00000000000";
const INTEGRITY_CANARY_CONTENT: &str = "EE integrity canary memory. Safe to ignore; verifies memory table write/read/provenance chain.";
pub const DOCTOR_MESH_AUTO_ENROLLMENT_SCHEMA_V1: &str = "ee.doctor.mesh_auto_enrollment.v1";

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

/// Whether a doctor check participates in the top-line memory health verdict.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckTier {
    Core,
    Advisory,
}

impl CheckTier {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Advisory => "advisory",
        }
    }
}

/// Three-state aggregate posture for `ee doctor` / `ee status`.
///
/// Bead bd-17c65.5.1 (E1). Replaces the boolean `healthy` field that
/// conflated "everything is perfect" with "the operation succeeded but a
/// fallback was taken". `healthy: bool` is kept alongside `posture` for
/// the v0.1 → v0.2 transition window (consumers reading `healthy` can
/// continue; new consumers should read `posture`).
///
/// Aggregation rule (`Posture::from_checks`):
/// - any core check `severity == Error` (critical) → [`Posture::Blocked`]
/// - any core check `severity == Warning` (and not marked transient) →
///   [`Posture::DegradedRecoverable`]
/// - else → [`Posture::Ok`]
///
/// Advisory checks remain visible in `checks[]` but do not drive the top-line
/// memory-recall verdict. They cover operator ergonomics and optional
/// subsystem posture such as PATH shadowing.
///
/// Transient warnings (e.g. an index that is 100ms behind writes and
/// resolves itself on the next sync) are marked at the check site;
/// they DO NOT downgrade the aggregate posture below `ok`. The check
/// remains visible in `checks[]` for diagnostic readers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Posture {
    /// Every check is `ok` (or `info` with `transient: true`).
    Ok,
    /// At least one non-transient warning; no critical errors. The
    /// operation completed and produced honest results, but a fallback
    /// or repair signal is active.
    DegradedRecoverable,
    /// At least one critical error. The operation could not complete.
    Blocked,
}

impl Posture {
    /// Stable lowercase wire form. Do not rename without contract bump.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::DegradedRecoverable => "degraded_recoverable",
            Self::Blocked => "blocked",
        }
    }

    /// Aggregate checks into a single posture.
    ///
    /// `transient_predicate` returns `true` for checks that are
    /// transient and should not downgrade the aggregate (e.g. stale
    /// indexes that auto-resolve). When `None`, every warning counts.
    #[must_use]
    pub fn from_checks(
        checks: &[CheckResult],
        transient_predicate: Option<&dyn Fn(&CheckResult) -> bool>,
    ) -> Self {
        let mut any_warning = false;
        for check in checks {
            if check.tier == CheckTier::Advisory {
                continue;
            }
            match check.severity {
                CheckSeverity::Error => return Self::Blocked,
                CheckSeverity::Warning => {
                    let is_transient = transient_predicate.is_some_and(|pred| pred(check));
                    if !is_transient {
                        any_warning = true;
                    }
                }
                CheckSeverity::Ok => {}
            }
        }
        if any_warning {
            Self::DegradedRecoverable
        } else {
            Self::Ok
        }
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
    pub tier: CheckTier,
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
            tier: CheckTier::Core,
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
            tier: CheckTier::Core,
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
            tier: CheckTier::Core,
        }
    }

    #[must_use]
    pub fn advisory(mut self) -> Self {
        self.tier = CheckTier::Advisory;
        self
    }

    #[must_use]
    pub fn is_topline_healthy(&self) -> bool {
        self.tier == CheckTier::Advisory || self.severity.is_healthy()
    }
}

/// Full doctor report.
#[derive(Clone, Debug)]
pub struct DoctorReport {
    pub version: &'static str,
    /// Legacy boolean (`true` when every check is `ok`). Kept for the
    /// v0.1 → v0.2 transition window. New consumers should read
    /// `posture` instead. Bead bd-17c65.5.1 (E1).
    pub overall_healthy: bool,
    /// Three-state aggregate posture (E1). Authoritative going forward.
    pub posture: Posture,
    /// Redaction-safe duplicate-work coalescing posture for agent operators.
    pub singleflight_posture: SingleFlightPostureReport,
    /// Redaction-safe foreground/background QoS lane posture for agent operators.
    pub qos_posture: QosLaneSummary,
    /// Redaction-safe remote compilation worker pressure posture.
    pub rch_worker_pressure: RchWorkerPressureReport,
    /// Redaction-safe verification evidence reuse posture.
    pub verification_posture: VerificationPostureReport,
    /// Redaction-safe durable RCH verifier blocker posture.
    pub verification_ledger: RchVerifyLedgerStatusReport,
    /// Redaction-safe host calibration posture and budget-delta guidance.
    pub host_calibration: Option<HostCalibrationPostureReport>,
    /// Redaction-safe command flight-recorder posture.
    pub flight_recorder: FlightRecorderStatusReport,
    pub checks: Vec<CheckResult>,
}

impl DoctorReport {
    /// Run all health checks and return a report.
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
        let canonical_workspace =
            workspace_path.map(crate::config::workspace::canonical_workspace_root_or_lexical);
        let workspace_path = canonical_workspace.as_deref();
        let singleflight_posture = singleflight_posture_report();
        let qos_posture = gather_qos_posture(workspace_path);
        let rch_worker_pressure = gather_rch_worker_pressure(workspace_path);
        let verification_posture = gather_verification_posture(workspace_path);
        let verification_ledger = gather_rch_verify_ledger_status(workspace_path);
        let host_calibration = gather_host_calibration_status(workspace_path);
        let flight_recorder = gather_flight_recorder_status(workspace_path);
        // CORE vs ADVISORY tiering (ADR 0081, bd-1et0v.12). CORE checks
        // (runtime / workspace / database / search index) answer the single
        // question "can ee store and retrieve memory right now?" and drive the
        // top-line posture + overall_healthy. ADVISORY checks (`.advisory()`)
        // report optional-subsystem and operator-ergonomics posture; they stay
        // visible in `checks[]` but NEVER flip the top-line unless they actually
        // break the memory loop. This call site is the single source of truth
        // for the CORE/ADVISORY split — keep it in sync with ADR 0081 and the
        // status-surface CORE subsystem set (src/core/status.rs). Check ORDER is
        // preserved (goldens assert it); the tier is what changed, not position.
        //
        // CORE:     check_runtime, check_workspace, check_database, check_search_index
        // ADVISORY: check_ee_install_path (advisory via its own builder, bd-1et0v.18),
        //           check_shard_fanout, check_flight_recorder, check_lexical_ram_tier,
        //           check_graph_numa_pin, check_daemon_socket_reachable,
        //           check_rch_worker_pressure, check_rch_verify_ledger, check_cass
        let checks = vec![
            check_runtime(),
            check_ee_install_path(),
            check_embedding_posture(workspace_path),
            check_workspace(workspace_path),
            check_database(workspace_path),
            check_shard_fanout(workspace_path).advisory(),
            check_flight_recorder(&flight_recorder).advisory(),
            check_search_index(workspace_path),
            check_lexical_ram_tier(workspace_path).advisory(),
            check_graph_numa_pin(workspace_path).advisory(),
            check_daemon_socket_reachable().advisory(),
            check_rch_worker_pressure(&rch_worker_pressure).advisory(),
            check_rch_verify_ledger(&verification_ledger).advisory(),
            check_cass().advisory(),
        ];

        let overall_healthy = checks.iter().all(CheckResult::is_topline_healthy);
        // E1: aggregate into three-state posture. For now no transient
        // predicate (the existing severity calibration already moves
        // truly transient signals to `Ok`); future bead E2 / E3 can
        // refine which check codes are transient (e.g. search_index
        // stale that auto-resolves on next sync).
        let posture = Posture::from_checks(&checks, None);

        Self {
            version: env!("CARGO_PKG_VERSION"),
            overall_healthy,
            posture,
            singleflight_posture,
            qos_posture,
            rch_worker_pressure,
            verification_posture,
            verification_ledger,
            host_calibration,
            flight_recorder,
            checks,
        }
    }

    /// Convert the doctor report into a structured fix plan.
    #[must_use]
    pub fn to_fix_plan(&self) -> FixPlan {
        self.to_fix_plan_with_agent_inventory(&AgentInventoryReport::not_inspected())
    }

    /// Convert the doctor report into a structured fix plan with optional
    /// agent-root guidance for CASS import dry runs.
    #[must_use]
    pub fn to_fix_plan_with_agent_inventory(
        &self,
        agent_inventory: &AgentInventoryReport,
    ) -> FixPlan {
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
            cass_import_guidance: CassImportGuidance::from_agent_inventory(agent_inventory),
        }
    }
}

fn gather_host_calibration_status(
    workspace_path: Option<&Path>,
) -> Option<HostCalibrationPostureReport> {
    let workspace = workspace_path?;
    let runtime = super::profile::runtime_profile_for_workspace(workspace);
    Some(gather_host_calibration_posture(
        workspace,
        runtime.active_profile,
    ))
}

/// Doctor-local status vocabulary for SRR6.46 auto-enrollment readiness checks.
///
/// This is intentionally separate from the legacy top-level [`CheckSeverity`]:
/// the SRR6.46 doctor block needs an explicit `skipped` state when mesh is
/// disabled and a `fail` state for per-check readiness, while the existing
/// doctor aggregate keeps the historical `ok | warning | error` wire values.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorMeshAutoEnrollmentCheckStatus {
    Ok,
    Warning,
    Fail,
    Skipped,
}

impl DoctorMeshAutoEnrollmentCheckStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Fail => "fail",
            Self::Skipped => "skipped",
        }
    }

    #[must_use]
    pub const fn needs_attention(self) -> bool {
        matches!(self, Self::Warning | Self::Fail)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorMeshAutoEnrollmentEvidence {
    pub key: String,
    pub value: String,
}

impl DoctorMeshAutoEnrollmentEvidence {
    #[must_use]
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorMeshAutoEnrollmentCheck {
    pub name: &'static str,
    pub status: DoctorMeshAutoEnrollmentCheckStatus,
    pub message: String,
    pub evidence: Vec<DoctorMeshAutoEnrollmentEvidence>,
    pub fix_action_id: Option<String>,
}

impl DoctorMeshAutoEnrollmentCheck {
    #[must_use]
    pub fn new(
        name: &'static str,
        status: DoctorMeshAutoEnrollmentCheckStatus,
        message: impl Into<String>,
        evidence: Vec<DoctorMeshAutoEnrollmentEvidence>,
        fix_action_id: Option<&str>,
    ) -> Self {
        Self {
            name,
            status,
            message: message.into(),
            evidence,
            fix_action_id: fix_action_id.map(str::to_owned),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorMeshAutoEnrollmentSummary {
    pub ok: u32,
    pub warning: u32,
    pub fail: u32,
    pub skipped: u32,
    pub total: u32,
}

impl DoctorMeshAutoEnrollmentSummary {
    #[must_use]
    pub fn from_checks(checks: &[DoctorMeshAutoEnrollmentCheck]) -> Self {
        let mut summary = Self::default();
        for check in checks {
            match check.status {
                DoctorMeshAutoEnrollmentCheckStatus::Ok => summary.ok += 1,
                DoctorMeshAutoEnrollmentCheckStatus::Warning => summary.warning += 1,
                DoctorMeshAutoEnrollmentCheckStatus::Fail => summary.fail += 1,
                DoctorMeshAutoEnrollmentCheckStatus::Skipped => summary.skipped += 1,
            }
        }
        summary.total = checks.len() as u32;
        summary
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorMeshAutoEnrollmentDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorMeshTailscaleReadiness {
    pub schema: &'static str,
    pub installed: Option<bool>,
    pub daemon_running: Option<bool>,
    pub authenticated: Option<bool>,
    pub binary_authentic: Option<bool>,
    pub shields_up: Option<bool>,
    pub peer_count: u32,
    pub probe_method: &'static str,
    pub platform: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorMeshHelloResponderReadiness {
    pub schema: &'static str,
    pub running: Option<bool>,
    pub crash_loop_detected: Option<bool>,
    pub listen_address: Option<String>,
    pub crash_count_24h: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorMeshDiscoveryCacheReadiness {
    pub schema: &'static str,
    pub status: &'static str,
    pub ttl_seconds: u64,
    pub stale_beyond_workspace: Option<bool>,
    pub hit: Option<bool>,
    pub refreshed_at: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorMeshMaterializedConfigReadiness {
    pub present: bool,
    pub peer_group_count: u32,
    pub peer_count: u32,
    pub consistent: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorMeshDriftPostureReadiness {
    pub status: &'static str,
    pub new_peer_count: u32,
    pub stale_peer_count: u32,
    pub tailnet_changed: Option<bool>,
    pub node_key_changed: Option<bool>,
    pub manual_conflict_present: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorMeshAutoEnrollmentReport {
    pub schema: &'static str,
    pub enabled: bool,
    pub posture: &'static str,
    pub workspace_path: String,
    pub tailscale: DoctorMeshTailscaleReadiness,
    pub hello_responder: DoctorMeshHelloResponderReadiness,
    pub discovery_cache: DoctorMeshDiscoveryCacheReadiness,
    pub materialized_config: DoctorMeshMaterializedConfigReadiness,
    pub drift_posture: DoctorMeshDriftPostureReadiness,
    pub checks: Vec<DoctorMeshAutoEnrollmentCheck>,
    pub categorized_summary: DoctorMeshAutoEnrollmentSummary,
    pub action_graph: RepairActionGraph,
    pub degraded: Vec<DoctorMeshAutoEnrollmentDegradation>,
}

impl DoctorMeshAutoEnrollmentReport {
    #[must_use]
    pub fn gather(workspace_path: Option<&Path>) -> Self {
        let probe = DoctorMeshAutoEnrollmentProbe::gather(workspace_path);
        Self::from_probe(&probe)
    }

    fn from_probe(probe: &DoctorMeshAutoEnrollmentProbe) -> Self {
        let checks = doctor_mesh_auto_enrollment_checks(probe);
        let categorized_summary = DoctorMeshAutoEnrollmentSummary::from_checks(&checks);
        let posture = doctor_mesh_auto_enrollment_posture(&categorized_summary);
        let action_graph = doctor_mesh_auto_enrollment_action_graph(&probe.workspace_path, &checks);
        let degraded = doctor_mesh_auto_enrollment_degraded(&checks);

        Self {
            schema: DOCTOR_MESH_AUTO_ENROLLMENT_SCHEMA_V1,
            enabled: probe.mesh_enabled,
            posture,
            workspace_path: probe.workspace_path.clone(),
            tailscale: DoctorMeshTailscaleReadiness {
                schema: "ee.tailscale.local.v1",
                installed: probe.tailscale.as_ref().map(|report| report.installed),
                daemon_running: probe
                    .tailscale
                    .as_ref()
                    .map(|report| report.daemon_reachable),
                authenticated: probe.tailscale.as_ref().map(|report| report.authenticated),
                binary_authentic: probe
                    .tailscale
                    .as_ref()
                    .map(|report| report.binary_authentic),
                shields_up: probe
                    .tailscale
                    .as_ref()
                    .and_then(|report| report.shields_up),
                peer_count: probe.discovered_peer_count(),
                probe_method: probe
                    .tailscale
                    .as_ref()
                    .map_or("skipped", |report| report.probe_method.as_str()),
                platform: probe
                    .tailscale
                    .as_ref()
                    .map_or("other", |report| report.platform.as_str()),
            },
            hello_responder: DoctorMeshHelloResponderReadiness {
                schema: "ee.mesh.hello_responder.status.v1",
                running: probe.hello_responder.as_ref().map(|report| report.running),
                crash_loop_detected: probe
                    .hello_responder
                    .as_ref()
                    .map(|report| report.crash_count_24h >= 3),
                listen_address: probe
                    .hello_responder
                    .as_ref()
                    .and_then(|report| report.listen_address.clone()),
                crash_count_24h: probe
                    .hello_responder
                    .as_ref()
                    .map(|report| report.crash_count_24h),
            },
            discovery_cache: DoctorMeshDiscoveryCacheReadiness {
                schema: "ee.mesh.discovery_cache.status.v1",
                status: if probe.mesh_enabled {
                    "not_loaded"
                } else {
                    "skipped"
                },
                ttl_seconds: crate::mesh::discovery_cache::DEFAULT_DISCOVERY_CACHE_TTL_SECONDS,
                stale_beyond_workspace: probe.discovery_cache_stale_beyond_workspace,
                hit: None,
                refreshed_at: None,
            },
            materialized_config: DoctorMeshMaterializedConfigReadiness {
                present: probe.materialized_peer_group_count > 0,
                peer_group_count: probe.materialized_peer_group_count,
                peer_count: probe.materialized_peer_count,
                consistent: probe.materialized_peer_group_consistent,
            },
            drift_posture: DoctorMeshDriftPostureReadiness {
                status: if !probe.mesh_enabled {
                    "skipped"
                } else if categorized_summary.fail > 0 {
                    "blocked"
                } else if categorized_summary.warning > 0 {
                    "actionable"
                } else {
                    "stable"
                },
                new_peer_count: probe.discovered_peer_count(),
                stale_peer_count: 0,
                tailnet_changed: None,
                node_key_changed: None,
                manual_conflict_present: None,
            },
            checks,
            categorized_summary,
            action_graph,
            degraded,
        }
    }
}

#[derive(Clone, Debug)]
struct DoctorMeshAutoEnrollmentProbe {
    workspace_path: String,
    mesh_enabled: bool,
    mesh_enabled_source: &'static str,
    tailscale: Option<TailscaleLocalReport>,
    hello_responder: Option<HelloResponderStatusReport>,
    audit_chain_intact: Option<bool>,
    steward_consecutive_failures_24h: Option<u64>,
    steward_state_file_readable: Option<bool>,
    discovery_cache_stale_beyond_workspace: Option<bool>,
    materialized_peer_group_count: u32,
    materialized_peer_count: u32,
    materialized_peer_group_consistent: Option<bool>,
    mcp_parity_present: Option<bool>,
}

impl DoctorMeshAutoEnrollmentProbe {
    fn gather(workspace_path: Option<&Path>) -> Self {
        let (mesh_enabled, mesh_enabled_source) = doctor_mesh_enabled(workspace_path);
        let workspace_path = workspace_path
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| ".".to_owned());
        let tailscale = mesh_enabled.then(gather_doctor_tailscale_local_report);
        let hello_responder = if mesh_enabled {
            HelloResponderStatusReport::from_environment(true).ok()
        } else {
            None
        };
        let (materialized_peer_group_count, materialized_peer_count) =
            materialized_peer_group_counts(workspace_path.as_ref());
        let materialized_peer_group_consistent = mesh_enabled
            .then_some(materialized_peer_group_count == 1 && materialized_peer_count > 0);

        Self {
            workspace_path,
            mesh_enabled,
            mesh_enabled_source,
            tailscale,
            hello_responder,
            audit_chain_intact: None,
            steward_consecutive_failures_24h: None,
            steward_state_file_readable: mesh_enabled.then(steward_state_file_readable),
            discovery_cache_stale_beyond_workspace: None,
            materialized_peer_group_count,
            materialized_peer_count,
            materialized_peer_group_consistent,
            mcp_parity_present: None,
        }
    }

    fn discovered_peer_count(&self) -> u32 {
        self.tailscale
            .as_ref()
            .map_or(0, |report| report.peers.len() as u32)
    }
}

fn doctor_mesh_enabled(workspace_path: Option<&Path>) -> (bool, &'static str) {
    if let Some(raw) = read_env_var(EnvVar::MeshEnabled) {
        return (
            parse_doctor_env_bool(&raw).unwrap_or(false),
            if parse_doctor_env_bool(&raw).is_some() {
                "env"
            } else {
                "env_invalid"
            },
        );
    }

    if let Some(enabled) = workspace_path
        .and_then(workspace_config)
        .and_then(|config| config.mesh.enabled)
    {
        return (enabled, "workspace_config");
    }

    (false, "default")
}

fn parse_doctor_env_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn gather_doctor_tailscale_local_report() -> TailscaleLocalReport {
    let timeout_ms = tailscale_probe_timeout_ms_from_env_value(
        read_env_var(EnvVar::TailscaleProbeTimeoutMs).as_deref(),
    );
    let mut cli_config = TailscaleCliProbeConfig::mesh_enabled();
    cli_config.timeout_ms = timeout_ms;
    cli_config.binary_override = read_env_var(EnvVar::TailscaleBinaryOverride).map(PathBuf::from);
    cli_config.platform_hint = current_doctor_tailscale_platform();

    let mut socket_config = TailscaleSocketProbeConfig::mesh_enabled();
    socket_config.timeout_ms = timeout_ms;
    socket_config.platform_hint = current_doctor_tailscale_platform();
    if let Some(override_path) = read_env_var(EnvVar::TailscaleProbeSocketOverride)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    {
        socket_config.socket_candidates = vec![PathBuf::from(override_path)];
    }

    let mut socket_runner = SystemTailscaleSocketProbeRunner;
    let mut cli_runner = SystemTailscaleCliProbeRunner;
    probe_tailscale_local_with_runners(
        &socket_config,
        &cli_config,
        &mut socket_runner,
        &mut cli_runner,
    )
}

fn current_doctor_tailscale_platform() -> TailscalePlatform {
    if cfg!(target_os = "linux") {
        TailscalePlatform::Linux
    } else if cfg!(target_os = "macos") {
        TailscalePlatform::MacosOpen
    } else if cfg!(target_os = "windows") {
        TailscalePlatform::Windows
    } else {
        TailscalePlatform::Other
    }
}

fn materialized_peer_group_counts(workspace_path: &str) -> (u32, u32) {
    let path = Path::new(workspace_path);
    let Some(config) = workspace_config(path) else {
        return (0, 0);
    };
    let Some(bindings) = config.mesh.peer_group_bindings.as_ref() else {
        return (0, 0);
    };
    let peer_count = bindings
        .iter()
        .map(|binding| binding.peer_ids.as_ref().map_or(0, Vec::len))
        .sum::<usize>() as u32;
    (bindings.len() as u32, peer_count)
}

fn steward_state_file_readable() -> bool {
    let Some(base) = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
    else {
        return false;
    };
    // The doctor check `steward_state_file_readable` (rendered as
    // "Auto-enrollment steward state file is readable.") is a binary
    // "does this state file exist as a regular file?" probe. The
    // previous shape ran `fs::read_to_string(...).is_ok()` which
    // allocated the WHOLE file body just to discard it — a
    // peer-planted multi-GB `auto_enroll_state.json` (corrupt
    // steward write, `cat /dev/urandom > auto_enroll_state.json`,
    // hostile multi-agent host that shares $HOME) would OOM every
    // `ee doctor` invocation. Switch to `fs::metadata` + `is_file()`,
    // which returns the same "exists as a regular file" verdict
    // with no body allocation. Behavior change for one edge case:
    // a non-UTF-8 regular file now reports `true` (the steward
    // can still inspect it; the doctor check is about presence,
    // not content shape). Mirrors the bounded-read pass landed on
    // `.ee/config.toml`, `.ee/index/meta.json` (ad2d302e), and the
    // procedure verification sources (131fd011).
    let path = base.join("ee/steward/auto_enroll_state.json");
    std::fs::metadata(&path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
}

fn doctor_mesh_auto_enrollment_checks(
    probe: &DoctorMeshAutoEnrollmentProbe,
) -> Vec<DoctorMeshAutoEnrollmentCheck> {
    if !probe.mesh_enabled {
        return doctor_mesh_auto_enrollment_skipped_checks(probe);
    }

    let tailscale = probe.tailscale.as_ref();
    let hello = probe.hello_responder.as_ref();
    let mut checks = Vec::with_capacity(15);

    checks.push(DoctorMeshAutoEnrollmentCheck::new(
        "mesh_enabled",
        DoctorMeshAutoEnrollmentCheckStatus::Ok,
        "Mesh auto-enrollment checks are enabled for this doctor run.",
        vec![
            DoctorMeshAutoEnrollmentEvidence::new("source", probe.mesh_enabled_source),
            DoctorMeshAutoEnrollmentEvidence::new("enabled", "true"),
        ],
        None,
    ));
    checks.push(doctor_bool_check(
        "tailscale_installed",
        tailscale.map(|report| report.installed),
        true,
        "Tailscale is installed.",
        "Tailscale is not installed or the local socket was not found.",
        "Tailscale installation was not inspected.",
        "tailscale_install",
    ));
    checks.push(doctor_bool_check(
        "tailscale_daemon_running",
        tailscale.map(|report| report.daemon_reachable),
        true,
        "Tailscale daemon is reachable.",
        "Tailscale daemon is not reachable.",
        "Tailscale daemon reachability was not inspected.",
        "tailscale_up",
    ));
    checks.push(doctor_bool_check(
        "tailscale_authenticated",
        tailscale.map(|report| report.authenticated),
        true,
        "Tailscale is authenticated.",
        "Tailscale is not authenticated.",
        "Tailscale authentication was not inspected.",
        "tailscale_up",
    ));
    checks.push(doctor_bool_check(
        "tailscale_binary_authentic",
        tailscale.map(|report| report.binary_authentic),
        true,
        "Tailscale binary authenticity check passed.",
        "Tailscale binary authenticity check failed.",
        "Tailscale binary authenticity was not inspected.",
        "tailscale_install",
    ));
    checks.push(doctor_bool_check(
        "tailscale_shields_not_up",
        tailscale
            .and_then(|report| report.shields_up)
            .map(|value| !value),
        true,
        "Tailscale shields-up is disabled.",
        "Tailscale shields-up is enabled; peers cannot initiate discovery.",
        "Tailscale shields-up posture was not inspected.",
        "tailscale_disable_shields_up",
    ));
    checks.push(doctor_bool_check(
        "hello_responder_running",
        hello.map(|report| report.running),
        true,
        "The mesh hello responder is running.",
        "The mesh hello responder is not running.",
        "The mesh hello responder lifecycle was not inspected.",
        "ee_daemon_start",
    ));
    checks.push(doctor_bool_check(
        "hello_responder_no_crash_loop",
        hello.map(|report| report.crash_count_24h < 3),
        true,
        "The mesh hello responder is not crash-looping.",
        "The mesh hello responder appears to be crash-looping.",
        "The mesh hello responder crash history was not inspected.",
        "inspect_hello_responder_audit",
    ));
    checks.push(doctor_count_check(
        "discovery_returns_at_least_one_peer",
        probe.discovered_peer_count(),
        "At least one Tailscale peer is visible to discovery.",
        "No Tailscale peers were visible to auto-enrollment discovery.",
        "ee_mesh_discovery_refresh",
    ));
    checks.push(doctor_bool_check(
        "auto_enrollment_audit_chain_intact",
        probe.audit_chain_intact,
        true,
        "Auto-enrollment audit chain is intact.",
        "Auto-enrollment audit chain is not intact.",
        "Auto-enrollment audit chain was not inspected.",
        "inspect_auto_enrollment_audit",
    ));
    checks.push(doctor_bool_check(
        "auto_enrollment_no_consecutive_failures",
        probe
            .steward_consecutive_failures_24h
            .map(|failures| failures == 0),
        true,
        "Auto-enrollment steward has no consecutive failures.",
        "Auto-enrollment steward has consecutive failures.",
        "Auto-enrollment steward failure state was not inspected.",
        "inspect_steward_failures",
    ));
    checks.push(doctor_bool_check(
        "steward_state_file_readable",
        probe.steward_state_file_readable,
        true,
        "Auto-enrollment steward state file is readable.",
        "Auto-enrollment steward state file is missing or unreadable.",
        "Auto-enrollment steward state file was not inspected.",
        "inspect_steward_state",
    ));
    checks.push(doctor_bool_check(
        "discovery_cache_not_stale_beyond_workspace",
        probe
            .discovery_cache_stale_beyond_workspace
            .map(|stale| !stale),
        true,
        "Discovery cache is not stale beyond the selected workspace.",
        "Discovery cache is stale or belongs to another workspace.",
        "Discovery cache staleness was not inspected.",
        "ee_mesh_discovery_refresh",
    ));
    checks.push(doctor_bool_check(
        "materialized_peer_group_consistent",
        probe.materialized_peer_group_consistent,
        true,
        "Materialized auto-enrollment peer group is consistent.",
        "Materialized auto-enrollment peer group is missing or inconsistent.",
        "Materialized auto-enrollment peer group was not inspected.",
        "ee_mesh_auto_enroll",
    ));
    checks.push(doctor_bool_check(
        "mcp_parity_present",
        probe.mcp_parity_present,
        true,
        "MCP parity coverage for mesh auto-enrollment is present.",
        "MCP parity coverage for mesh auto-enrollment is missing.",
        "MCP parity coverage for mesh auto-enrollment was not inspected.",
        "ee_mcp_parity_check",
    ));

    checks
}

fn doctor_mesh_auto_enrollment_skipped_checks(
    probe: &DoctorMeshAutoEnrollmentProbe,
) -> Vec<DoctorMeshAutoEnrollmentCheck> {
    const CHECK_NAMES: [&str; 15] = [
        "mesh_enabled",
        "tailscale_installed",
        "tailscale_daemon_running",
        "tailscale_authenticated",
        "tailscale_binary_authentic",
        "tailscale_shields_not_up",
        "hello_responder_running",
        "hello_responder_no_crash_loop",
        "discovery_returns_at_least_one_peer",
        "auto_enrollment_audit_chain_intact",
        "auto_enrollment_no_consecutive_failures",
        "steward_state_file_readable",
        "discovery_cache_not_stale_beyond_workspace",
        "materialized_peer_group_consistent",
        "mcp_parity_present",
    ];

    CHECK_NAMES
        .into_iter()
        .map(|name| {
            DoctorMeshAutoEnrollmentCheck::new(
                name,
                DoctorMeshAutoEnrollmentCheckStatus::Skipped,
                "Mesh auto-enrollment is disabled; readiness check skipped.",
                vec![
                    DoctorMeshAutoEnrollmentEvidence::new("source", probe.mesh_enabled_source),
                    DoctorMeshAutoEnrollmentEvidence::new("enabled", "false"),
                ],
                None,
            )
        })
        .collect()
}

fn doctor_bool_check(
    name: &'static str,
    actual: Option<bool>,
    expected: bool,
    ok_message: &'static str,
    fail_message: &'static str,
    unknown_message: &'static str,
    fix_action_id: &'static str,
) -> DoctorMeshAutoEnrollmentCheck {
    match actual {
        Some(value) if value == expected => DoctorMeshAutoEnrollmentCheck::new(
            name,
            DoctorMeshAutoEnrollmentCheckStatus::Ok,
            ok_message,
            vec![DoctorMeshAutoEnrollmentEvidence::new(
                "observed",
                value.to_string(),
            )],
            None,
        ),
        Some(value) => DoctorMeshAutoEnrollmentCheck::new(
            name,
            DoctorMeshAutoEnrollmentCheckStatus::Fail,
            fail_message,
            vec![DoctorMeshAutoEnrollmentEvidence::new(
                "observed",
                value.to_string(),
            )],
            Some(fix_action_id),
        ),
        None => DoctorMeshAutoEnrollmentCheck::new(
            name,
            DoctorMeshAutoEnrollmentCheckStatus::Warning,
            unknown_message,
            vec![DoctorMeshAutoEnrollmentEvidence::new(
                "observed",
                "not_inspected",
            )],
            Some(fix_action_id),
        ),
    }
}

fn doctor_count_check(
    name: &'static str,
    count: u32,
    ok_message: &'static str,
    fail_message: &'static str,
    fix_action_id: &'static str,
) -> DoctorMeshAutoEnrollmentCheck {
    if count > 0 {
        DoctorMeshAutoEnrollmentCheck::new(
            name,
            DoctorMeshAutoEnrollmentCheckStatus::Ok,
            ok_message,
            vec![DoctorMeshAutoEnrollmentEvidence::new(
                "peerCount",
                count.to_string(),
            )],
            None,
        )
    } else {
        DoctorMeshAutoEnrollmentCheck::new(
            name,
            DoctorMeshAutoEnrollmentCheckStatus::Warning,
            fail_message,
            vec![DoctorMeshAutoEnrollmentEvidence::new("peerCount", "0")],
            Some(fix_action_id),
        )
    }
}

fn doctor_mesh_auto_enrollment_posture(summary: &DoctorMeshAutoEnrollmentSummary) -> &'static str {
    if summary.total == summary.skipped {
        "skipped"
    } else if summary.fail > 0 {
        "fail"
    } else if summary.warning > 0 {
        "warning"
    } else {
        "ok"
    }
}

fn doctor_mesh_auto_enrollment_degraded(
    checks: &[DoctorMeshAutoEnrollmentCheck],
) -> Vec<DoctorMeshAutoEnrollmentDegradation> {
    checks
        .iter()
        .filter(|check| check.status.needs_attention())
        .map(|check| DoctorMeshAutoEnrollmentDegradation {
            code: check.name,
            severity: check.status.as_str(),
            message: check.message.clone(),
            repair: check
                .fix_action_id
                .as_ref()
                .map(|id| format!("Run action graph step `{id}`.")),
        })
        .collect()
}

fn doctor_mesh_auto_enrollment_action_graph(
    workspace_path: &str,
    checks: &[DoctorMeshAutoEnrollmentCheck],
) -> RepairActionGraph {
    let mut required: BTreeSet<String> = checks
        .iter()
        .filter_map(|check| check.fix_action_id.clone())
        .collect();
    if required.is_empty() {
        return empty_doctor_repair_action_graph();
    }

    close_action_dependencies(&mut required);

    let actions = required
        .iter()
        .filter_map(|id| doctor_mesh_auto_enrollment_action(id, workspace_path, &required))
        .collect();
    build_repair_action_graph(actions).unwrap_or_else(|error| {
        tracing::warn!(
            error = %error,
            "doctor mesh auto-enrollment built an invalid repair action graph"
        );
        empty_doctor_repair_action_graph()
    })
}

fn close_action_dependencies(required: &mut BTreeSet<String>) {
    let mut changed = true;
    while changed {
        changed = false;
        let current: Vec<String> = required.iter().cloned().collect();
        for id in current {
            let deps = match id.as_str() {
                "tailscale_up" => vec!["tailscale_install"],
                "tailscale_disable_shields_up" => vec!["tailscale_up"],
                "ee_daemon_start" => vec!["tailscale_up", "tailscale_disable_shields_up"],
                "ee_mesh_discovery_refresh" => vec!["tailscale_up", "ee_daemon_start"],
                "ee_mesh_auto_enroll" => {
                    vec![
                        "ee_mesh_discovery_refresh",
                        "ee_daemon_start",
                        "ee_mesh_disable",
                    ]
                }
                "ee_mcp_parity_check" => vec!["ee_mesh_auto_enroll"],
                _ => Vec::new(),
            };
            for dep in deps {
                changed |= required.insert(dep.to_owned());
            }
        }
    }
}

fn empty_doctor_repair_action_graph() -> RepairActionGraph {
    RepairActionGraph {
        schema: REPAIR_ACTION_GRAPH_SCHEMA_V1.to_owned(),
        actions: Vec::new(),
        topologically_ordered_execution: Vec::new(),
        parallelizable_groups: Vec::new(),
        estimated_total_duration_seconds: 0,
    }
}

fn doctor_mesh_auto_enrollment_action(
    id: &str,
    workspace_path: &str,
    required: &BTreeSet<String>,
) -> Option<RepairAction> {
    let workspace_arg = doctor_shell_quote_arg(workspace_path);
    let present_deps = |deps: &[&str]| -> Vec<String> {
        deps.iter()
            .filter(|dep| required.contains(**dep))
            .map(|dep| (*dep).to_owned())
            .collect()
    };

    let action = match id {
        "tailscale_install" => RepairAction {
            id: id.to_owned(),
            kind: ActionKind::ExternalTool,
            command: "Install Tailscale from https://tailscale.com/download".to_owned(),
            human_readable: "Install the Tailscale client on this host.".to_owned(),
            prerequisites: Vec::new(),
            expected_outcome: ExpectedOutcome {
                resolves_checks: vec![
                    "tailscale_installed".to_owned(),
                    "tailscale_binary_authentic".to_owned(),
                ],
                preconditions_for_next_actions: Vec::new(),
            },
            priority: Priority::Critical,
            estimated_duration_seconds: 180,
            reversible: false,
            reversal_command: None,
            requires_user_confirmation: true,
            execution_context: ExecutionContext::ExternalTool,
        },
        "tailscale_up" => RepairAction {
            id: id.to_owned(),
            kind: ActionKind::ShellCommand,
            command: "tailscale up".to_owned(),
            human_readable: "Authenticate this host with Tailscale.".to_owned(),
            prerequisites: present_deps(&["tailscale_install"]),
            expected_outcome: ExpectedOutcome {
                resolves_checks: vec![
                    "tailscale_daemon_running".to_owned(),
                    "tailscale_authenticated".to_owned(),
                ],
                preconditions_for_next_actions: Vec::new(),
            },
            priority: Priority::Critical,
            estimated_duration_seconds: 60,
            reversible: false,
            reversal_command: None,
            requires_user_confirmation: true,
            execution_context: ExecutionContext::UserShell,
        },
        "tailscale_disable_shields_up" => RepairAction {
            id: id.to_owned(),
            kind: ActionKind::ShellCommand,
            command: "tailscale set --shields-up=false".to_owned(),
            human_readable: "Allow trusted peers to initiate Tailscale discovery.".to_owned(),
            prerequisites: present_deps(&["tailscale_up"]),
            expected_outcome: ExpectedOutcome {
                resolves_checks: vec!["tailscale_shields_not_up".to_owned()],
                preconditions_for_next_actions: Vec::new(),
            },
            priority: Priority::High,
            estimated_duration_seconds: 5,
            reversible: true,
            reversal_command: Some("tailscale set --shields-up=true".to_owned()),
            requires_user_confirmation: false,
            execution_context: ExecutionContext::UserShell,
        },
        "ee_daemon_start" => RepairAction {
            id: id.to_owned(),
            kind: ActionKind::EeSubcommand,
            command: format!("ee daemon --foreground --workspace {workspace_arg}"),
            human_readable: "Start the foreground ee daemon and hello responder.".to_owned(),
            prerequisites: present_deps(&["tailscale_up", "tailscale_disable_shields_up"]),
            expected_outcome: ExpectedOutcome {
                resolves_checks: vec!["hello_responder_running".to_owned()],
                preconditions_for_next_actions: Vec::new(),
            },
            priority: Priority::High,
            estimated_duration_seconds: 10,
            reversible: true,
            reversal_command: Some("Stop the foreground daemon with Ctrl-C.".to_owned()),
            requires_user_confirmation: false,
            execution_context: ExecutionContext::EeSubcommand,
        },
        "inspect_hello_responder_audit" => RepairAction {
            id: id.to_owned(),
            kind: ActionKind::EeSubcommand,
            command: format!(
                "ee audit timeline --workspace {workspace_arg} --event-type mesh.hello_responder_crashed_restarted --json"
            ),
            human_readable: "Inspect hello-responder restart audit rows.".to_owned(),
            prerequisites: Vec::new(),
            expected_outcome: ExpectedOutcome {
                resolves_checks: vec!["hello_responder_no_crash_loop".to_owned()],
                preconditions_for_next_actions: Vec::new(),
            },
            priority: Priority::High,
            estimated_duration_seconds: 15,
            reversible: true,
            reversal_command: None,
            requires_user_confirmation: false,
            execution_context: ExecutionContext::EeSubcommand,
        },
        "ee_mesh_discovery_refresh" => RepairAction {
            id: id.to_owned(),
            kind: ActionKind::EeSubcommand,
            command: format!("ee mesh status --workspace {workspace_arg} --json"),
            human_readable: "Refresh the read-only mesh discovery posture.".to_owned(),
            prerequisites: present_deps(&["tailscale_up", "ee_daemon_start"]),
            expected_outcome: ExpectedOutcome {
                resolves_checks: vec![
                    "discovery_returns_at_least_one_peer".to_owned(),
                    "discovery_cache_not_stale_beyond_workspace".to_owned(),
                ],
                preconditions_for_next_actions: Vec::new(),
            },
            priority: Priority::Medium,
            estimated_duration_seconds: 5,
            reversible: true,
            reversal_command: None,
            requires_user_confirmation: false,
            execution_context: ExecutionContext::EeSubcommand,
        },
        "inspect_auto_enrollment_audit" => RepairAction {
            id: id.to_owned(),
            kind: ActionKind::EeSubcommand,
            command: format!("ee audit verify --workspace {workspace_arg} --json"),
            human_readable: "Verify the auto-enrollment audit chain.".to_owned(),
            prerequisites: Vec::new(),
            expected_outcome: ExpectedOutcome {
                resolves_checks: vec!["auto_enrollment_audit_chain_intact".to_owned()],
                preconditions_for_next_actions: Vec::new(),
            },
            priority: Priority::High,
            estimated_duration_seconds: 20,
            reversible: true,
            reversal_command: None,
            requires_user_confirmation: false,
            execution_context: ExecutionContext::EeSubcommand,
        },
        "inspect_steward_failures" => RepairAction {
            id: id.to_owned(),
            kind: ActionKind::EeSubcommand,
            command: format!(
                "ee audit timeline --workspace {workspace_arg} --event-type mesh.steward_reconciliation_failed --json"
            ),
            human_readable: "Inspect recent steward reconciliation failures.".to_owned(),
            prerequisites: Vec::new(),
            expected_outcome: ExpectedOutcome {
                resolves_checks: vec!["auto_enrollment_no_consecutive_failures".to_owned()],
                preconditions_for_next_actions: Vec::new(),
            },
            priority: Priority::Medium,
            estimated_duration_seconds: 15,
            reversible: true,
            reversal_command: None,
            requires_user_confirmation: false,
            execution_context: ExecutionContext::EeSubcommand,
        },
        "inspect_steward_state" => RepairAction {
            id: id.to_owned(),
            kind: ActionKind::ManualStep,
            command: "Inspect ~/.local/share/ee/steward/auto_enroll_state.json".to_owned(),
            human_readable: "Inspect the local auto-enrollment steward state file.".to_owned(),
            prerequisites: Vec::new(),
            expected_outcome: ExpectedOutcome {
                resolves_checks: vec!["steward_state_file_readable".to_owned()],
                preconditions_for_next_actions: Vec::new(),
            },
            priority: Priority::Medium,
            estimated_duration_seconds: 30,
            reversible: true,
            reversal_command: None,
            requires_user_confirmation: false,
            execution_context: ExecutionContext::ExternalTool,
        },
        "ee_mesh_auto_enroll" => RepairAction {
            id: id.to_owned(),
            kind: ActionKind::EeSubcommand,
            command: format!("ee mesh auto-enroll --workspace {workspace_arg}"),
            human_readable: "Materialize discovered ee-capable peers into the mesh peer set."
                .to_owned(),
            prerequisites: present_deps(&[
                "ee_mesh_discovery_refresh",
                "ee_daemon_start",
                "ee_mesh_disable",
            ]),
            expected_outcome: ExpectedOutcome {
                resolves_checks: vec!["materialized_peer_group_consistent".to_owned()],
                preconditions_for_next_actions: Vec::new(),
            },
            priority: Priority::Medium,
            estimated_duration_seconds: 5,
            reversible: true,
            reversal_command: Some(format!(
                "ee mesh disable --workspace {workspace_arg} --reason \"revert auto-enrollment\""
            )),
            requires_user_confirmation: false,
            execution_context: ExecutionContext::EeSubcommand,
        },
        "ee_mesh_disable" => RepairAction {
            id: id.to_owned(),
            kind: ActionKind::EeSubcommand,
            command: format!(
                "ee mesh disable --workspace {workspace_arg} --reason \"reset inconsistent auto-enrollment\""
            ),
            human_readable: "Disable stale materialized mesh configuration before re-enrolling."
                .to_owned(),
            prerequisites: Vec::new(),
            expected_outcome: ExpectedOutcome {
                resolves_checks: vec!["materialized_peer_group_consistent".to_owned()],
                preconditions_for_next_actions: Vec::new(),
            },
            priority: Priority::High,
            estimated_duration_seconds: 5,
            reversible: false,
            reversal_command: None,
            requires_user_confirmation: true,
            execution_context: ExecutionContext::EeSubcommand,
        },
        "ee_mcp_parity_check" => RepairAction {
            id: id.to_owned(),
            kind: ActionKind::EeSubcommand,
            command: "ee mcp manifest --json".to_owned(),
            human_readable: "Inspect MCP manifest coverage for mesh auto-enrollment surfaces."
                .to_owned(),
            prerequisites: present_deps(&["ee_mesh_auto_enroll"]),
            expected_outcome: ExpectedOutcome {
                resolves_checks: vec!["mcp_parity_present".to_owned()],
                preconditions_for_next_actions: Vec::new(),
            },
            priority: Priority::Low,
            estimated_duration_seconds: 5,
            reversible: true,
            reversal_command: None,
            requires_user_confirmation: false,
            execution_context: ExecutionContext::EeSubcommand,
        },
        _ => return None,
    };

    Some(action)
}

fn doctor_shell_quote_arg(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for ch in value.chars() {
        if matches!(ch, '"' | '$' | '`' | '\\') {
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    quoted
}

fn gather_qos_posture(workspace_path: Option<&Path>) -> QosLaneSummary {
    let workspace = workspace_path.unwrap_or_else(|| Path::new("."));
    let workspace_identity = workspace
        .to_str()
        .filter(|value| !value.is_empty())
        .unwrap_or(".");
    let now_epoch_ms = Utc::now().timestamp_millis().try_into().unwrap_or_default();
    summarize_qos_lane_registry(workspace, workspace_identity, now_epoch_ms)
}

/// A structured repair plan generated from doctor checks.
#[derive(Clone, Debug)]
pub struct FixPlan {
    pub version: &'static str,
    pub total_issues: usize,
    pub fixable_issues: usize,
    pub steps: Vec<FixStep>,
    pub cass_import_guidance: CassImportGuidance,
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

/// CASS import guidance status derived from agent detection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CassImportGuidanceStatus {
    AgentRootsDetected,
    NoAgentRootsDetected,
    NotInspected,
    Unavailable,
}

impl CassImportGuidanceStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentRootsDetected => "agent_roots_detected",
            Self::NoAgentRootsDetected => "no_agent_roots_detected",
            Self::NotInspected => "not_inspected",
            Self::Unavailable => "unavailable",
        }
    }
}

/// One detected local agent source root relevant to CASS import review.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CassImportRootGuidance {
    pub connector: String,
    pub root_path: String,
    pub guidance: String,
}

/// Agent-root guidance shown by `ee doctor --fix-plan`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CassImportGuidance {
    pub status: CassImportGuidanceStatus,
    pub detected_agent_count: usize,
    pub detected_root_count: usize,
    pub roots: Vec<CassImportRootGuidance>,
    pub suggested_commands: Vec<String>,
    pub message: String,
}

impl CassImportGuidance {
    #[must_use]
    pub fn from_agent_inventory(agent_inventory: &AgentInventoryReport) -> Self {
        let mut roots: Vec<CassImportRootGuidance> = agent_inventory
            .installed_agents
            .iter()
            .filter(|agent| agent.detected)
            .flat_map(|agent| {
                agent.root_paths.iter().map(|root_path| CassImportRootGuidance {
                    connector: agent.slug.clone(),
                    root_path: root_path.clone(),
                    guidance: format!(
                        "Review CASS dry-run coverage for {connector} history rooted at {root_path}.",
                        connector = agent.slug
                    ),
                })
            })
            .collect();
        roots.sort_by(|left, right| {
            left.connector
                .cmp(&right.connector)
                .then(left.root_path.cmp(&right.root_path))
        });

        let status = match agent_inventory.status {
            AgentInventoryStatus::Ready if roots.is_empty() => {
                CassImportGuidanceStatus::NoAgentRootsDetected
            }
            AgentInventoryStatus::Ready => CassImportGuidanceStatus::AgentRootsDetected,
            AgentInventoryStatus::Empty => CassImportGuidanceStatus::NoAgentRootsDetected,
            AgentInventoryStatus::NotInspected => CassImportGuidanceStatus::NotInspected,
            AgentInventoryStatus::Unavailable => CassImportGuidanceStatus::Unavailable,
        };

        let detected_root_count = roots.len();
        let suggested_commands = match status {
            CassImportGuidanceStatus::AgentRootsDetected => vec![
                "ee agent status --json".to_string(),
                "ee import cass --dry-run --json".to_string(),
                "ee import cass --json".to_string(),
            ],
            CassImportGuidanceStatus::NoAgentRootsDetected => vec![
                "ee agent scan --existing-only --json".to_string(),
                "ee import cass --dry-run --json".to_string(),
            ],
            CassImportGuidanceStatus::NotInspected => vec![
                "ee agent status --json".to_string(),
                "ee agent scan --existing-only --json".to_string(),
                "ee import cass --dry-run --json".to_string(),
            ],
            CassImportGuidanceStatus::Unavailable => vec![
                "ee agent sources --json".to_string(),
                "ee import cass --dry-run --json".to_string(),
            ],
        };

        let message = match status {
            CassImportGuidanceStatus::AgentRootsDetected => format!(
                "Detected {detected_root_count} local agent source root(s); run a CASS dry-run before importing evidence."
            ),
            CassImportGuidanceStatus::NoAgentRootsDetected => {
                "No local agent source roots were detected; CASS import can still report available sessions.".to_string()
            }
            CassImportGuidanceStatus::NotInspected => {
                "Agent source roots were not inspected for this fix plan; run agent status for root-level guidance.".to_string()
            }
            CassImportGuidanceStatus::Unavailable => {
                "Agent source root detection is unavailable; use the static source catalog and CASS dry-run output.".to_string()
            }
        };

        Self {
            status,
            detected_agent_count: agent_inventory.summary.detected_count,
            detected_root_count,
            roots,
            suggested_commands,
            message,
        }
    }
}

/// Options for `ee diag integrity`.
#[derive(Clone, Debug)]
pub struct IntegrityDiagnosticsOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub workspace_id: String,
    pub sample_size: u32,
    pub create_canary: bool,
    pub dry_run: bool,
}

impl IntegrityDiagnosticsOptions {
    #[must_use]
    pub fn resolved_database_path(&self) -> PathBuf {
        self.database_path
            .clone()
            .unwrap_or_else(|| self.workspace_path.join(".ee").join("ee.db"))
    }
}

/// Overall integrity diagnostic posture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegrityDiagnosticsStatus {
    Ok,
    Degraded,
    Failed,
}

impl IntegrityDiagnosticsStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Failed => "failed",
        }
    }
}

/// Severity for an integrity diagnostic check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegrityDiagnosticSeverity {
    Ok,
    Warning,
    Error,
}

impl IntegrityDiagnosticSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// A single integrity check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntegrityDiagnosticCheck {
    pub name: &'static str,
    pub severity: IntegrityDiagnosticSeverity,
    pub message: String,
    pub repair: Option<&'static str>,
}

impl IntegrityDiagnosticCheck {
    #[must_use]
    pub fn ok(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            severity: IntegrityDiagnosticSeverity::Ok,
            message: message.into(),
            repair: None,
        }
    }

    #[must_use]
    pub fn warning(
        name: &'static str,
        message: impl Into<String>,
        repair: Option<&'static str>,
    ) -> Self {
        Self {
            name,
            severity: IntegrityDiagnosticSeverity::Warning,
            message: message.into(),
            repair,
        }
    }

    #[must_use]
    pub fn error(
        name: &'static str,
        message: impl Into<String>,
        repair: Option<&'static str>,
    ) -> Self {
        Self {
            name,
            severity: IntegrityDiagnosticSeverity::Error,
            message: message.into(),
            repair,
        }
    }
}

/// Explicit canary-memory mutation posture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegrityCanaryStatus {
    NotRequested,
    DryRun,
    Created,
    AlreadyExists,
    Skipped,
    Failed,
}

impl IntegrityCanaryStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRequested => "not_requested",
            Self::DryRun => "dry_run",
            Self::Created => "created",
            Self::AlreadyExists => "already_exists",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

/// Canary-memory creation result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntegrityCanaryReport {
    pub requested: bool,
    pub dry_run: bool,
    pub memory_id: &'static str,
    pub status: IntegrityCanaryStatus,
    pub message: String,
    pub repair: Option<&'static str>,
}

impl IntegrityCanaryReport {
    #[must_use]
    pub fn not_requested() -> Self {
        Self {
            requested: false,
            dry_run: false,
            memory_id: INTEGRITY_CANARY_MEMORY_ID,
            status: IntegrityCanaryStatus::NotRequested,
            message: "Canary memory creation was not requested.".to_string(),
            repair: Some("Use `ee diag integrity --create-canary --json` to write one."),
        }
    }
}

/// Non-fatal integrity diagnostic degradation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntegrityDiagnosticDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: Option<&'static str>,
}

/// Full `ee diag integrity` report.
#[derive(Clone, Debug)]
pub struct IntegrityDiagnosticsReport {
    pub version: &'static str,
    pub schema: &'static str,
    pub status: IntegrityDiagnosticsStatus,
    pub workspace_id: String,
    pub database_path: PathBuf,
    pub sample_size: u32,
    pub checks: Vec<IntegrityDiagnosticCheck>,
    pub provenance_sample: Option<ProvenanceSampleVerificationReport>,
    pub canary: IntegrityCanaryReport,
    pub degraded: Vec<IntegrityDiagnosticDegradation>,
}

impl IntegrityDiagnosticsReport {
    #[must_use]
    pub fn gather(options: &IntegrityDiagnosticsOptions) -> Self {
        let database_path = options.resolved_database_path();
        let mut checks = Vec::new();
        let mut degraded = Vec::new();
        let mut provenance_sample = None;

        if !database_path.exists() {
            checks.push(IntegrityDiagnosticCheck::warning(
                "database_exists",
                format!("Database not found at {}.", database_path.display()),
                Some("ee init --workspace ."),
            ));
            degraded.push(IntegrityDiagnosticDegradation {
                code: "integrity_database_missing",
                severity: "medium",
                message: "Integrity checks require an initialized ee database.".to_string(),
                repair: Some("ee init --workspace ."),
            });
            let canary = canary_for_missing_database(options);
            return Self::finalize(
                options,
                database_path,
                checks,
                provenance_sample,
                canary,
                degraded,
            );
        }

        checks.push(IntegrityDiagnosticCheck::ok(
            "database_exists",
            format!("Database found at {}.", database_path.display()),
        ));

        let connection = match DbConnection::open_file(&database_path) {
            Ok(connection) => connection,
            Err(error) => {
                checks.push(IntegrityDiagnosticCheck::error(
                    "database_open",
                    format!("Failed to open database: {error}"),
                    Some("ee doctor --json"),
                ));
                degraded.push(IntegrityDiagnosticDegradation {
                    code: "integrity_database_open_failed",
                    severity: "high",
                    message: "The database exists but could not be opened.".to_string(),
                    repair: Some("ee doctor --json"),
                });
                let canary = canary_for_open_failure(options);
                return Self::finalize(
                    options,
                    database_path,
                    checks,
                    provenance_sample,
                    canary,
                    degraded,
                );
            }
        };

        checks.push(IntegrityDiagnosticCheck::ok(
            "database_open",
            "Database opened through FrankenSQLite/SQLModel.",
        ));

        checks.push(check_sqlite_integrity(&connection));
        checks.push(check_foreign_keys(&connection));
        match connection.check_reference_integrity() {
            Ok(reference_report) => {
                checks.push(check_reference_integrity(&reference_report));
                if !reference_report.is_clean() {
                    degraded.push(IntegrityDiagnosticDegradation {
                        code: "integrity_reference_issues",
                        severity: "medium",
                        message: format!(
                            "Found {} link/pack reference integrity issue(s).",
                            reference_report.issue_count
                        ),
                        repair: Some("ee diag integrity --json"),
                    });
                }
            }
            Err(error) => {
                checks.push(IntegrityDiagnosticCheck::warning(
                    "reference_integrity",
                    format!("Could not evaluate link and pack reference integrity: {error}"),
                    Some("ee diag integrity --json"),
                ));
                degraded.push(IntegrityDiagnosticDegradation {
                    code: "integrity_reference_check_unavailable",
                    severity: "medium",
                    message: "Link and pack reference integrity checks could not be evaluated."
                        .to_string(),
                    repair: Some("ee diag integrity --json"),
                });
            }
        }

        match connection.needs_migration() {
            Ok(false) => checks.push(IntegrityDiagnosticCheck::ok(
                "schema_current",
                "Database schema is current.",
            )),
            Ok(true) => {
                checks.push(IntegrityDiagnosticCheck::warning(
                    "schema_current",
                    "Database schema has pending migrations.",
                    Some("ee init --workspace ."),
                ));
                degraded.push(IntegrityDiagnosticDegradation {
                    code: "integrity_schema_migration_required",
                    severity: "medium",
                    message: "Integrity diagnostics require the current ee schema before sampling provenance or writing the canary."
                        .to_string(),
                    repair: Some("ee init --workspace ."),
                });
                let canary = canary_for_pending_migration(options);
                return Self::finalize(
                    options,
                    database_path,
                    checks,
                    provenance_sample,
                    canary,
                    degraded,
                );
            }
            Err(error) => {
                checks.push(IntegrityDiagnosticCheck::warning(
                    "schema_current",
                    format!("Could not inspect migration state: {error}"),
                    Some("ee doctor --json"),
                ));
                degraded.push(IntegrityDiagnosticDegradation {
                    code: "integrity_schema_check_unavailable",
                    severity: "medium",
                    message: "The database migration state could not be inspected.".to_string(),
                    repair: Some("ee doctor --json"),
                });
                let canary = canary_for_pending_migration(options);
                return Self::finalize(
                    options,
                    database_path,
                    checks,
                    provenance_sample,
                    canary,
                    degraded,
                );
            }
        }

        match connection
            .inspect_sampled_memory_provenance(&options.workspace_id, options.sample_size)
        {
            Ok(report) => {
                checks.push(check_provenance_sample(&report));
                provenance_sample = Some(report);
            }
            Err(error) => {
                checks.push(IntegrityDiagnosticCheck::warning(
                    "provenance_sample",
                    format!("Could not inspect sampled provenance chains: {error}"),
                    Some("ee diag integrity --json"),
                ));
                degraded.push(IntegrityDiagnosticDegradation {
                    code: "integrity_provenance_sample_unavailable",
                    severity: "medium",
                    message: "The provenance-chain sample could not be inspected.".to_string(),
                    repair: Some("ee diag integrity --json"),
                });
            }
        }

        let canary = maybe_create_canary(&connection, options);
        if canary.status == IntegrityCanaryStatus::Failed {
            checks.push(IntegrityDiagnosticCheck::warning(
                "canary_memory",
                canary.message.clone(),
                canary.repair,
            ));
        }

        Self::finalize(
            options,
            database_path,
            checks,
            provenance_sample,
            canary,
            degraded,
        )
    }

    #[must_use]
    pub fn success(&self) -> bool {
        self.status != IntegrityDiagnosticsStatus::Failed
    }

    fn finalize(
        options: &IntegrityDiagnosticsOptions,
        database_path: PathBuf,
        checks: Vec<IntegrityDiagnosticCheck>,
        provenance_sample: Option<ProvenanceSampleVerificationReport>,
        canary: IntegrityCanaryReport,
        degraded: Vec<IntegrityDiagnosticDegradation>,
    ) -> Self {
        let status = if checks
            .iter()
            .any(|check| check.severity == IntegrityDiagnosticSeverity::Error)
        {
            IntegrityDiagnosticsStatus::Failed
        } else if !degraded.is_empty()
            || checks
                .iter()
                .any(|check| check.severity == IntegrityDiagnosticSeverity::Warning)
        {
            IntegrityDiagnosticsStatus::Degraded
        } else {
            IntegrityDiagnosticsStatus::Ok
        };

        Self {
            version: env!("CARGO_PKG_VERSION"),
            schema: INTEGRITY_DIAGNOSTICS_SCHEMA_V1,
            status,
            workspace_id: options.workspace_id.clone(),
            database_path,
            sample_size: options.sample_size,
            checks,
            provenance_sample,
            canary,
            degraded,
        }
    }
}

fn canary_for_missing_database(options: &IntegrityDiagnosticsOptions) -> IntegrityCanaryReport {
    if !options.create_canary {
        return IntegrityCanaryReport::not_requested();
    }

    if options.dry_run {
        return IntegrityCanaryReport {
            requested: true,
            dry_run: true,
            memory_id: INTEGRITY_CANARY_MEMORY_ID,
            status: IntegrityCanaryStatus::DryRun,
            message: "Would create the integrity canary after the database exists.".to_string(),
            repair: Some("ee init --workspace ."),
        };
    }

    IntegrityCanaryReport {
        requested: true,
        dry_run: false,
        memory_id: INTEGRITY_CANARY_MEMORY_ID,
        status: IntegrityCanaryStatus::Skipped,
        message: "Canary creation skipped because the database is missing.".to_string(),
        repair: Some("ee init --workspace ."),
    }
}

fn canary_for_open_failure(options: &IntegrityDiagnosticsOptions) -> IntegrityCanaryReport {
    if !options.create_canary {
        return IntegrityCanaryReport::not_requested();
    }

    IntegrityCanaryReport {
        requested: true,
        dry_run: options.dry_run,
        memory_id: INTEGRITY_CANARY_MEMORY_ID,
        status: IntegrityCanaryStatus::Skipped,
        message: "Canary creation skipped because the database could not be opened.".to_string(),
        repair: Some("ee doctor --json"),
    }
}

fn canary_for_pending_migration(options: &IntegrityDiagnosticsOptions) -> IntegrityCanaryReport {
    if !options.create_canary {
        return IntegrityCanaryReport::not_requested();
    }

    IntegrityCanaryReport {
        requested: true,
        dry_run: options.dry_run,
        memory_id: INTEGRITY_CANARY_MEMORY_ID,
        status: IntegrityCanaryStatus::Skipped,
        message: "Canary creation skipped until database migrations are current.".to_string(),
        repair: Some("ee init --workspace ."),
    }
}

fn check_sqlite_integrity(connection: &DbConnection) -> IntegrityDiagnosticCheck {
    match connection.check_integrity() {
        Ok(IntegrityCheckResult { passed: true, .. }) => {
            IntegrityDiagnosticCheck::ok("sqlite_integrity", "SQLite integrity_check returned ok.")
        }
        Ok(IntegrityCheckResult { issues, .. }) => IntegrityDiagnosticCheck::error(
            "sqlite_integrity",
            format!("SQLite integrity_check reported {} issue(s).", issues.len()),
            Some("Restore from backup or inspect with sqlite integrity_check."),
        ),
        Err(error) => IntegrityDiagnosticCheck::error(
            "sqlite_integrity",
            format!("Failed to run SQLite integrity_check: {error}"),
            Some("ee doctor --json"),
        ),
    }
}

fn check_foreign_keys(connection: &DbConnection) -> IntegrityDiagnosticCheck {
    match connection.check_foreign_keys() {
        Ok(ForeignKeyCheckResult { passed: true, .. }) => IntegrityDiagnosticCheck::ok(
            "foreign_keys",
            "SQLite foreign_key_check returned no violations.",
        ),
        Ok(ForeignKeyCheckResult { violations, .. }) => IntegrityDiagnosticCheck::error(
            "foreign_keys",
            format!(
                "SQLite foreign_key_check reported {} violation(s).",
                violations.len()
            ),
            Some("Inspect foreign_key_check output before further writes."),
        ),
        Err(error) => IntegrityDiagnosticCheck::error(
            "foreign_keys",
            format!("Failed to run SQLite foreign_key_check: {error}"),
            Some("ee doctor --json"),
        ),
    }
}

fn check_reference_integrity(report: &ReferenceIntegrityReport) -> IntegrityDiagnosticCheck {
    if report.is_clean() {
        IntegrityDiagnosticCheck::ok(
            "reference_integrity",
            "Link and pack references are internally consistent.",
        )
    } else {
        IntegrityDiagnosticCheck::warning(
            "reference_integrity",
            format!(
                "Detected {} link/pack reference integrity issue(s).",
                report.issue_count
            ),
            Some("ee diag integrity --json"),
        )
    }
}

fn check_provenance_sample(
    report: &ProvenanceSampleVerificationReport,
) -> IntegrityDiagnosticCheck {
    if report.is_clean() {
        IntegrityDiagnosticCheck::ok(
            "provenance_sample",
            format!(
                "Sampled {} memory provenance chain(s); all matched.",
                report.checked_count
            ),
        )
    } else {
        IntegrityDiagnosticCheck::warning(
            "provenance_sample",
            format!(
                "Sampled provenance found {} missing and {} mismatched chain hash(es).",
                report.missing_count, report.mismatch_count
            ),
            Some("ee diag integrity --create-canary --json"),
        )
    }
}

fn maybe_create_canary(
    connection: &DbConnection,
    options: &IntegrityDiagnosticsOptions,
) -> IntegrityCanaryReport {
    if !options.create_canary {
        return IntegrityCanaryReport::not_requested();
    }

    if options.dry_run {
        return IntegrityCanaryReport {
            requested: true,
            dry_run: true,
            memory_id: INTEGRITY_CANARY_MEMORY_ID,
            status: IntegrityCanaryStatus::DryRun,
            message: "Would create the integrity canary memory.".to_string(),
            repair: None,
        };
    }

    match connection.get_memory(INTEGRITY_CANARY_MEMORY_ID) {
        Ok(Some(_)) => IntegrityCanaryReport {
            requested: true,
            dry_run: false,
            memory_id: INTEGRITY_CANARY_MEMORY_ID,
            status: IntegrityCanaryStatus::AlreadyExists,
            message: "Integrity canary memory already exists.".to_string(),
            repair: None,
        },
        Ok(None) => insert_canary_memory(connection, &options.workspace_id),
        Err(error) => IntegrityCanaryReport {
            requested: true,
            dry_run: false,
            memory_id: INTEGRITY_CANARY_MEMORY_ID,
            status: IntegrityCanaryStatus::Failed,
            message: format!("Could not check for existing canary memory: {error}"),
            repair: Some("ee diag integrity --json"),
        },
    }
}

fn insert_canary_memory(connection: &DbConnection, workspace_id: &str) -> IntegrityCanaryReport {
    let input = CreateMemoryInput {
        workspace_id: workspace_id.to_string(),
        level: "semantic".to_string(),
        kind: "fact".to_string(),
        content: INTEGRITY_CANARY_CONTENT.to_string(),
        workflow_id: None,
        confidence: TrustClass::AgentAssertion.initial_confidence(),
        utility: 0.0,
        importance: 0.0,
        provenance_uri: Some("ee://diag/integrity/canary/v1".to_string()),
        trust_class: TrustClass::AgentAssertion.as_str().to_string(),
        trust_subclass: Some("integrity-canary".to_string()),
        tags: vec!["ee-canary".to_string(), "integrity".to_string()],
        valid_from: None,
        valid_to: None,
    };

    match connection.insert_memory(INTEGRITY_CANARY_MEMORY_ID, &input) {
        Ok(()) => IntegrityCanaryReport {
            requested: true,
            dry_run: false,
            memory_id: INTEGRITY_CANARY_MEMORY_ID,
            status: IntegrityCanaryStatus::Created,
            message: "Created integrity canary memory.".to_string(),
            repair: None,
        },
        Err(error) => IntegrityCanaryReport {
            requested: true,
            dry_run: false,
            memory_id: INTEGRITY_CANARY_MEMORY_ID,
            status: IntegrityCanaryStatus::Failed,
            message: format!("Failed to create integrity canary memory: {error}"),
            repair: Some("ee diag integrity --json"),
        },
    }
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
    match build_cli_runtime() {
        Ok(_runtime) => CheckResult::ok("runtime", "Asupersync runtime initialized successfully."),
        Err(error) => CheckResult::error(
            "runtime",
            format!("Asupersync runtime initialization failed: {error}"),
            error_codes::RUNTIME_UNAVAILABLE,
        ),
    }
}

fn check_ee_install_path() -> CheckResult {
    let options = super::install::InstallCheckOptions {
        offline: true,
        ..Default::default()
    };
    let report = super::install::check_install(&options);
    ee_install_path_check_from_report(&report)
}

fn ee_install_path_check_from_report(report: &crate::models::InstallCheckReport) -> CheckResult {
    let findings = report
        .findings
        .iter()
        .filter(|finding| ee_install_path_finding_is_doctor_advisory(finding.code))
        .collect::<Vec<_>>();

    if findings.is_empty() {
        return CheckResult::ok(
            "ee_install_path",
            format!(
                "ee install posture is advisory-only and local: {}, PATH status {}, no shadowed or stale ee binary detected.",
                ee_install_path_version_summary(report),
                report.path.status.as_str()
            ),
        )
        .advisory();
    }

    let finding_summary = findings
        .iter()
        .take(3)
        .map(|finding| format!("{}: {}", finding.code.as_str(), finding.message))
        .collect::<Vec<_>>()
        .join("; ");
    let omitted = findings.len().saturating_sub(3);
    let finding_summary = if omitted == 0 {
        finding_summary
    } else {
        format!("{finding_summary}; {omitted} additional install advisory finding(s) omitted")
    };

    CheckResult {
        name: "ee_install_path",
        severity: CheckSeverity::Warning,
        message: format!(
            "Advisory-only ee install posture needs attention: {finding_summary}. {}; PATH status {}; {}. This check uses only local PATH/source evidence and performs no network lookup.",
            ee_install_path_version_summary(report),
            report.path.status.as_str(),
            ee_install_path_binary_summary(report)
        ),
        error_code: None,
        repair: Some(
            "Run `ee install check --json --offline` and fix PATH ordering or adopt a verified release artifact; do not use local Cargo.",
        ),
        tier: CheckTier::Advisory,
    }
}

fn ee_install_path_finding_is_doctor_advisory(code: crate::models::InstallFindingCode) -> bool {
    use crate::models::InstallFindingCode;
    matches!(
        code,
        InstallFindingCode::BinaryNotOnPath
            | InstallFindingCode::CurrentBinaryShadowed
            | InstallFindingCode::DuplicatePathBinary
            | InstallFindingCode::InstalledBinaryStale
            | InstallFindingCode::InstalledVersionUnknown
            | InstallFindingCode::PathBinaryVersionMismatch
    )
}

fn ee_install_path_binary_summary(report: &crate::models::InstallCheckReport) -> String {
    if report.path.binaries.is_empty() {
        return "No ee binary was found on PATH".to_owned();
    }

    let rendered = report
        .path
        .binaries
        .iter()
        .take(4)
        .map(|binary| {
            let version = binary.version.as_deref().map_or_else(
                || {
                    format!(
                        "version {}",
                        binary.version_status.as_deref().unwrap_or("unknown")
                    )
                },
                |version| format!("version {version}"),
            );
            let current = if binary.is_current_binary {
                ", current process"
            } else {
                ""
            };
            format!("#{} {} ({version}{current})", binary.ordinal, binary.path)
        })
        .collect::<Vec<_>>()
        .join("; ");
    let omitted = report.path.binaries.len().saturating_sub(4);
    if omitted == 0 {
        format!("PATH ee binaries: {rendered}")
    } else {
        format!(
            "PATH ee binaries: {rendered}; {omitted} additional PATH ee binary/binaries omitted"
        )
    }
}

fn ee_install_path_version_summary(report: &crate::models::InstallCheckReport) -> String {
    let source = &report.freshness.source_version;
    let installed = &report.freshness.installed_version;
    format!(
        "running version {}; local source version {} via {}{}; installed/current version {} via {}{}; freshness {}",
        report.version,
        source.version.as_deref().unwrap_or("unknown"),
        source.source,
        source
            .path
            .as_deref()
            .map(|path| format!(" at {path}"))
            .unwrap_or_default(),
        installed.version.as_deref().unwrap_or("unknown"),
        installed.source,
        installed
            .path
            .as_deref()
            .map(|path| format!(" at {path}"))
            .unwrap_or_default(),
        report.freshness.verdict.as_str()
    )
}

fn check_workspace(workspace_path: Option<&Path>) -> CheckResult {
    let Some(workspace_path) = workspace_path else {
        return CheckResult::warning(
            "workspace",
            "No workspace path was provided for inspection.",
            error_codes::WORKSPACE_NOT_SPECIFIED,
        );
    };

    if workspace_path.join(".ee").exists() {
        CheckResult::ok(
            "workspace",
            format!("Workspace inspected at {}.", workspace_path.display()),
        )
    } else {
        CheckResult::warning(
            "workspace",
            format!(
                "Selected workspace has no .ee state at {}.",
                workspace_path.display()
            ),
            error_codes::WORKSPACE_NOT_SPECIFIED,
        )
    }
}

fn check_database(workspace_path: Option<&Path>) -> CheckResult {
    let Some(workspace_path) = workspace_path else {
        return CheckResult::warning(
            "database",
            "Database could not be inspected without a workspace path.",
            error_codes::DATABASE_NOT_FOUND,
        );
    };
    let database_path = workspace_path.join(".ee").join("ee.db");
    if !database_path.exists() {
        return CheckResult::warning(
            "database",
            format!("Database file not found at {}.", database_path.display()),
            error_codes::DATABASE_NOT_FOUND,
        );
    }

    match DbConnection::open_file(&database_path).and_then(|connection| {
        connection.ping()?;
        connection.needs_migration()
    }) {
        Ok(false) => CheckResult::ok(
            "database",
            format!(
                "Database opened and schema is current at {}.",
                database_path.display()
            ),
        ),
        Ok(true) => CheckResult::warning(
            "database",
            "Database opened but schema migrations are pending.",
            error_codes::MIGRATION_REQUIRED,
        ),
        Err(error) => CheckResult::error(
            "database",
            format!("Database readiness check failed: {error}"),
            error_codes::DATABASE_CORRUPTED,
        ),
    }
}

/// Presence-only scan of the foreign embedding-config env vars (never reads a
/// value, so it is redaction-safe).
fn present_embedding_trap_env_vars() -> Vec<&'static str> {
    EmbeddingTrapEnvVar::all()
        .iter()
        .copied()
        .filter(|var| var.is_present())
        .map(EmbeddingTrapEnvVar::name)
        .collect()
}

/// Build the "EMBEDDING_MODEL is set but ee ignores it" disclosure note, or
/// `None` when no foreign embedding-config var is present.
fn embedding_env_trap_note(trap_present: &[&str], mode: &str) -> Option<String> {
    if trap_present.is_empty() {
        return None;
    }
    let names = trap_present.join(", ");
    let pronoun = if trap_present.len() == 1 {
        "it"
    } else {
        "them"
    };
    Some(format!(
        "Note: {names} set but ee does not consume {pronoun} — ee uses its own bundled local embedder (current retrieval mode: {mode})."
    ))
}

/// Render the advisory embedding-posture check from already-resolved posture
/// facts. Pure and deterministic (no DB, no env) so the rendering is unit
/// testable; the `*_present` trap list is injected by the caller.
///
/// Always [`CheckTier::Advisory`] — it discloses the active retrieval mode and
/// the env trap but MUST NOT flip the top-line memory-health verdict (per the
/// doctor-health tiering leaf, bd-1et0v.12). Semantic-ready is `Ok` (info);
/// hash fallback and pending first-use downloads are `Warning` (still advisory).
fn embedding_posture_check_result(
    semantic: bool,
    mode: &str,
    fast_model_id: &str,
    fast_dimension: usize,
    deterministic: bool,
    trap_present: &[&str],
) -> CheckResult {
    let trap_note = embedding_env_trap_note(trap_present, mode);
    let check = if semantic {
        let determinism = if deterministic { ", deterministic" } else { "" };
        let mut message = format!(
            "Semantic retrieval: ready ({mode}, {fast_model_id}, {fast_dimension}d{determinism}). Inspect the active mode with `ee model status` / `ee index status`."
        );
        if let Some(note) = trap_note {
            message.push(' ');
            message.push_str(&note);
        }
        CheckResult::ok("embedding_posture", message)
    } else if mode == EMBEDDING_POSTURE_MODE_NEURAL_LOCAL_PENDING {
        let mut message = format!(
            "Semantic retrieval: bundled neural model pending first-use download (mode={mode}, {fast_model_id}, {fast_dimension}d). This is degraded-but-improving and is not deterministic-hash fallback."
        );
        if let Some(note) = trap_note {
            message.push(' ');
            message.push_str(&note);
        }
        CheckResult {
            name: "embedding_posture",
            severity: CheckSeverity::Warning,
            message,
            error_code: None,
            repair: Some(
                "Run an embedding operation or pre-download the bundled model with `ee model fetch`; use EE_EMBED_DOWNLOAD=off only for intentional offline hash fallback.",
            ),
            tier: CheckTier::Advisory,
        }
    } else {
        let mut message = format!(
            "Semantic retrieval: deterministic-hash fallback (non-semantic) — the bundled neural model is not active (mode={mode}, {fast_model_id}, {fast_dimension}d). Degraded code: embed_model_unavailable."
        );
        if let Some(note) = trap_note {
            message.push(' ');
            message.push_str(&note);
        }
        CheckResult {
            name: "embedding_posture",
            severity: CheckSeverity::Warning,
            message,
            error_code: None,
            repair: Some(
                "Inspect with `ee model status`; pre-download the bundled model with `ee model fetch`; then `ee index rebuild` to re-embed. Memory still works on the deterministic-hash tier meanwhile.",
            ),
            tier: CheckTier::Advisory,
        }
    };
    // Belt-and-suspenders: this check never participates in top-line health.
    check.advisory()
}

/// Advisory result for when the active retrieval mode cannot be determined
/// (no workspace, or the index status read failed). Still `Ok`/advisory and
/// still discloses the env trap; points to the canonical inspection surfaces.
fn embedding_posture_unavailable_check(trap_present: &[&str], reason: &str) -> CheckResult {
    let mut message = format!(
        "Semantic retrieval mode could not be determined ({reason}); inspect with `ee model status` / `ee index status`."
    );
    if let Some(note) = embedding_env_trap_note(trap_present, "unknown") {
        message.push(' ');
        message.push_str(&note);
    }
    CheckResult::ok("embedding_posture", message).advisory()
}

/// Advisory doctor check (bd-1et0v.8): honestly disclose the active retrieval
/// mode (neural_local vs deterministic-hash) and warn when a foreign
/// `EMBEDDING_MODEL` / `OPENAI_API_KEY` is set that ee silently ignores.
/// Reuses the bd-1et0v.6 `EmbeddingPosture` helper (one source of truth) via
/// [`get_index_status`]. Never flips top-line health.
fn check_embedding_posture(workspace_path: Option<&Path>) -> CheckResult {
    let trap_present = present_embedding_trap_env_vars();
    let Some(workspace_path) = workspace_path else {
        return embedding_posture_unavailable_check(&trap_present, "no workspace path");
    };
    let options = IndexStatusOptions {
        workspace_path: workspace_path.to_path_buf(),
        database_path: None,
        index_dir: None,
    };
    match get_index_status(&options) {
        Ok(report) => match &report.embedding {
            Some(posture) => embedding_posture_check_result(
                posture.semantic,
                posture.mode,
                &posture.fast_model_id,
                posture.fast_dimension,
                posture.deterministic,
                &trap_present,
            ),
            None => embedding_posture_unavailable_check(
                &trap_present,
                "index has no embedding posture yet (run `ee index rebuild`)",
            ),
        },
        Err(error) => {
            embedding_posture_unavailable_check(&trap_present, &format!("index status: {error}"))
        }
    }
}

fn check_shard_fanout(workspace_path: Option<&Path>) -> CheckResult {
    let enabled =
        shard_fanout_enabled_from_env_value(read_env_var(EnvVar::ShardFanoutEnabled).as_deref());
    let workspace_root = workspace_path.map(Path::to_path_buf);
    let workspace_id = workspace_path.map(|path| {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        stable_workspace_id(&canonical)
    });
    let report = resolve_shard_fanout_status(ShardFanoutResolverInput {
        enabled,
        workspace_id,
        workspace_root,
        shards_dir_override: read_env_var_os(EnvVar::ShardsDir).map(PathBuf::from),
    });

    match report.posture {
        ShardFanoutPosture::Disabled => CheckResult::ok(
            "shard_fanout",
            "Shard fan-out is disabled; legacy workspace database routing remains authoritative.",
        ),
        ShardFanoutPosture::Enabled => CheckResult::ok(
            "shard_fanout",
            format!(
                "Shard fan-out is enabled for {}.",
                report
                    .shard_path
                    .as_ref()
                    .map_or_else(|| "<unknown>".into(), |path| path.display().to_string())
            ),
        ),
        ShardFanoutPosture::MigrationRequired => CheckResult::warning(
            "shard_fanout",
            "Shard fan-out is enabled but the catalog or workspace shard is not ready.",
            error_codes::MIGRATION_REQUIRED,
        ),
        ShardFanoutPosture::Degraded => CheckResult::warning(
            "shard_fanout",
            "Shard fan-out configuration is unsafe and routing is blocked.",
            error_codes::CONFIG_INVALID_VALUE,
        ),
        ShardFanoutPosture::NotInspected => CheckResult::warning(
            "shard_fanout",
            "Shard fan-out is enabled but no workspace was selected for inspection.",
            error_codes::WORKSPACE_NOT_SPECIFIED,
        ),
    }
}

fn check_flight_recorder(report: &FlightRecorderStatusReport) -> CheckResult {
    let posture = report.posture.as_str();
    let detail = format!(
        "posture={posture}; retentionDays={}; maxBytes={}; redaction={}; directory={}",
        report.retention_days,
        report.max_bytes,
        report.redaction_level,
        report.directory.display()
    );
    match report.posture {
        crate::obs::FlightRecorderPosture::Disabled => CheckResult::ok(
            "flight_recorder",
            format!("Flight recorder is disabled by default; {detail}."),
        ),
        crate::obs::FlightRecorderPosture::Enabled => CheckResult::ok(
            "flight_recorder",
            format!("Flight recorder is enabled and writable; {detail}."),
        ),
        crate::obs::FlightRecorderPosture::RetentionOutOfRange => CheckResult::warning(
            "flight_recorder",
            format!("Flight recorder retention is outside the supported range; {detail}."),
            error_codes::CONFIG_INVALID_VALUE,
        ),
        crate::obs::FlightRecorderPosture::DirectoryUnwritable => CheckResult::warning(
            "flight_recorder",
            format!("Flight recorder directory is not writable; {detail}."),
            error_codes::CONFIG_INVALID_VALUE,
        ),
        crate::obs::FlightRecorderPosture::DirectoryInsideGit => CheckResult::warning(
            "flight_recorder",
            format!("Flight recorder directory points inside .git; {detail}."),
            error_codes::CONFIG_INVALID_VALUE,
        ),
    }
}

fn check_search_index(workspace_path: Option<&Path>) -> CheckResult {
    let Some(workspace_path) = workspace_path else {
        return CheckResult::warning(
            "search_index",
            "Search index could not be inspected without a workspace path.",
            error_codes::INDEX_NOT_FOUND,
        );
    };
    let options = IndexStatusOptions {
        workspace_path: workspace_path.to_path_buf(),
        database_path: None,
        index_dir: None,
    };

    match get_index_status(&options) {
        Ok(report) => match report.health {
            IndexHealth::Ready => CheckResult::ok(
                "search_index",
                format!("Search index is ready at {}.", report.index_dir.display()),
            ),
            IndexHealth::Stale => CheckResult::warning(
                "search_index",
                "Search index exists but is stale.",
                error_codes::INDEX_STALE,
            ),
            IndexHealth::Missing => CheckResult::warning(
                "search_index",
                "Search index is missing for the current workspace.",
                error_codes::INDEX_NOT_FOUND,
            ),
            IndexHealth::Corrupt => CheckResult::error(
                "search_index",
                "Search index failed integrity checks.",
                error_codes::INDEX_CORRUPTED,
            ),
        },
        Err(error) => CheckResult::warning(
            "search_index",
            format!("Search index readiness check failed: {error}"),
            error_codes::INDEX_NOT_FOUND,
        ),
    }
}

fn check_lexical_ram_tier(workspace_path: Option<&Path>) -> CheckResult {
    check_lexical_ram_tier_with_config(
        workspace_path,
        lexical_ram_tier_config_for_doctor(workspace_path),
    )
}

fn check_lexical_ram_tier_with_config(
    workspace_path: Option<&Path>,
    config: LexicalRamTierConfig,
) -> CheckResult {
    let index_path = lexical_ram_tier_index_path(workspace_path);
    let result = pin_lexical_index_files(&index_path, &config);
    lexical_ram_tier_check_from_result(&result)
}

fn lexical_ram_tier_check_from_result(result: &LexicalRamTierResult) -> CheckResult {
    let index_path = result
        .index_path
        .as_deref()
        .map(Path::display)
        .map(|display| display.to_string())
        .unwrap_or_else(|| "unknown".to_owned());

    if !result.enabled {
        return CheckResult::ok(
            "lexical_ram_tier",
            format!("Lexical RAM-tier pinning is disabled for {index_path}."),
        );
    }

    if result.succeeded {
        return CheckResult::ok(
            "lexical_ram_tier",
            format!("Lexical RAM-tier pinning succeeded for {index_path}."),
        );
    }

    CheckResult {
        name: "lexical_ram_tier",
        severity: CheckSeverity::Warning,
        message: format!(
            "Lexical RAM-tier pinning is enabled but degraded for {index_path}; degraded codes: {}.",
            format_string_codes(&result.degraded_codes)
        ),
        error_code: None,
        repair: Some(
            "Inspect `ee status --json` search.lexicalRamTier and lexical RAM-tier env/config.",
        ),
        tier: CheckTier::Core,
    }
}

fn lexical_ram_tier_config_for_doctor(workspace_path: Option<&Path>) -> LexicalRamTierConfig {
    if let Some(workspace_path) = workspace_path
        && let Ok(merged) = crate::core::config_surface::merged_workspace_config(workspace_path)
    {
        return LexicalRamTierConfig::from_config_overrides(&merged.values.search.lexical_ram_tier);
    }

    LexicalRamTierConfig::from_environment_with_reader(
        |name| match name {
            LEXICAL_RAM_TIER_PIN_RAM_ENV => read_env_var(EnvVar::LexicalIndexPinRam),
            LEXICAL_RAM_TIER_HUGEPAGES_ENV => read_env_var(EnvVar::LexicalIndexHugepages),
            _ => None,
        },
        |_name, _raw| {},
    )
}

fn lexical_ram_tier_index_path(workspace_path: Option<&Path>) -> PathBuf {
    workspace_path
        .map(|path| path.join(".ee").join(DEFAULT_INDEX_SUBDIR).join("lexical"))
        .unwrap_or_else(|| {
            PathBuf::from(".ee")
                .join(DEFAULT_INDEX_SUBDIR)
                .join("lexical")
        })
}

fn check_graph_numa_pin(workspace_path: Option<&Path>) -> CheckResult {
    check_graph_numa_pin_with_config(workspace_path, graph_numa_pin_config_for_doctor())
}

fn check_graph_numa_pin_with_config(
    workspace_path: Option<&Path>,
    config: NumaPinConfig,
) -> CheckResult {
    let snapshot_path = graph_numa_pin_snapshot_path(workspace_path);
    let result = pin_snapshot_blob(&snapshot_path, &config);
    graph_numa_pin_check_from_result(&result)
}

fn graph_numa_pin_check_from_result(result: &NumaPinResult) -> CheckResult {
    let snapshot_path = result
        .snapshot_path
        .as_deref()
        .map(Path::display)
        .map(|display| display.to_string())
        .unwrap_or_else(|| "unknown".to_owned());

    if !result.enabled {
        return CheckResult::ok(
            "graph_numa_pin",
            format!("Graph NUMA pinning is disabled for {snapshot_path}."),
        );
    }

    if result.succeeded {
        return CheckResult::ok(
            "graph_numa_pin",
            format!("Graph NUMA pinning succeeded for {snapshot_path}."),
        );
    }

    CheckResult {
        name: "graph_numa_pin",
        severity: CheckSeverity::Warning,
        message: format!(
            "Graph NUMA pinning is enabled but degraded for {snapshot_path}; degraded codes: {}.",
            format_static_codes(&result.degraded_codes)
        ),
        error_code: None,
        repair: Some("Inspect `ee status --json` graph.numaPin and graph NUMA env/config."),
        tier: CheckTier::Core,
    }
}

fn graph_numa_pin_config_for_doctor() -> NumaPinConfig {
    NumaPinConfig::from_environment_with_reader(
        |name| match name {
            NUMA_PIN_DISABLE_ENV => read_env_var(EnvVar::GraphNumaPinDisable),
            NUMA_PIN_NODE_ENV => read_env_var(EnvVar::GraphNumaPinNode),
            NUMA_PIN_POPULATE_ENV => read_env_var(EnvVar::GraphNumaPinPopulate),
            _ => None,
        },
        |_name, _raw| {},
    )
}

fn graph_numa_pin_snapshot_path(workspace_path: Option<&Path>) -> PathBuf {
    workspace_path
        .map(|path| path.join(".ee").join("graph"))
        .unwrap_or_else(|| PathBuf::from(".ee").join("graph"))
}

fn check_daemon_socket_reachable() -> CheckResult {
    check_daemon_socket_reachable_at(&crate::daemon::default_daemon_socket_path())
}

#[cfg(unix)]
fn check_daemon_socket_reachable_at(socket_path: &Path) -> CheckResult {
    match std::fs::symlink_metadata(socket_path) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket() {
                return CheckResult {
                    name: "daemon_socket_reachable",
                    severity: CheckSeverity::Warning,
                    message: format!(
                        "Daemon socket path exists but is not a Unix-domain socket at {}.",
                        socket_path.display()
                    ),
                    error_code: None,
                    repair: Some("Inspect `ee daemon status --json` and restart the daemon."),
                    tier: CheckTier::Core,
                };
            }

            match UnixStream::connect(socket_path) {
                Ok(_stream) => CheckResult::ok(
                    "daemon_socket_reachable",
                    format!(
                        "Daemon socket accepts local connections at {}.",
                        socket_path.display()
                    ),
                ),
                Err(error) => CheckResult {
                    name: "daemon_socket_reachable",
                    severity: CheckSeverity::Warning,
                    message: format!(
                        "Daemon socket exists but did not accept a local connection at {}: {error}.",
                        socket_path.display()
                    ),
                    error_code: None,
                    repair: Some("Inspect `ee daemon status --json` and restart the daemon."),
                    tier: CheckTier::Core,
                },
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => CheckResult::ok(
            "daemon_socket_reachable",
            format!(
                "Optional daemon socket is not present at {}; in-process CLI execution remains authoritative.",
                socket_path.display()
            ),
        ),
        Err(error) => CheckResult {
            name: "daemon_socket_reachable",
            severity: CheckSeverity::Warning,
            message: format!(
                "Daemon socket path could not be inspected at {}: {error}.",
                socket_path.display()
            ),
            error_code: None,
            repair: Some(
                "Inspect `ee daemon status --json` and the daemon socket parent directory.",
            ),
            tier: CheckTier::Core,
        },
    }
}

#[cfg(not(unix))]
fn check_daemon_socket_reachable_at(_socket_path: &Path) -> CheckResult {
    CheckResult::ok(
        "daemon_socket_reachable",
        "Unix-domain daemon sockets are not supported on this platform.",
    )
}

fn format_string_codes(codes: &[String]) -> String {
    if codes.is_empty() {
        "none".to_owned()
    } else {
        codes.join(",")
    }
}

fn format_static_codes(codes: &[&'static str]) -> String {
    if codes.is_empty() {
        "none".to_owned()
    } else {
        codes.join(",")
    }
}

fn check_cass() -> CheckResult {
    use crate::models::CapabilityStatus;
    match probe_cass_capability() {
        CapabilityStatus::Ready => {
            CheckResult::ok("cass", "CASS binary discovered and accessible.")
        }
        CapabilityStatus::Degraded => CheckResult::warning(
            "cass",
            "CASS binary found but capabilities are limited.",
            error_codes::CASS_DEGRADED,
        ),
        CapabilityStatus::Pending => CheckResult::warning(
            "cass",
            "CASS binary not found in trusted locations.",
            error_codes::CASS_NOT_FOUND,
        ),
        CapabilityStatus::Unimplemented => CheckResult::warning(
            "cass",
            "CASS integration is unavailable.",
            error_codes::CASS_UNAVAILABLE,
        ),
    }
}

fn check_rch_worker_pressure(report: &RchWorkerPressureReport) -> CheckResult {
    match report.status.as_str() {
        "pressure_clear" => CheckResult::ok(
            "rch_worker_pressure",
            format!(
                "RCH worker pressure is clear; {} of {} worker(s) usable.",
                report.usable_worker_count, report.worker_count
            ),
        ),
        "pressure_unknown" | "not_collected" => CheckResult::ok(
            "rch_worker_pressure",
            "RCH worker pressure telemetry is unavailable; no RCH pressure blocker was inferred.",
        ),
        "healthy_but_pressure_blocked" => CheckResult {
            name: "rch_worker_pressure",
            severity: CheckSeverity::Warning,
            message: format!(
                "RCH reports healthy workers, but pressure blocks admission on {} of {} worker(s).",
                report.blocked_worker_count, report.worker_count
            ),
            error_code: None,
            repair: Some("rch status --workers --jobs --json"),
            tier: CheckTier::Core,
        },
        "pressure_policy_denied" => CheckResult {
            name: "rch_worker_pressure",
            severity: CheckSeverity::Warning,
            message: "RCH worker admission was denied by pressure policy.".to_string(),
            error_code: None,
            repair: Some("rch status --workers --jobs --json"),
            tier: CheckTier::Core,
        },
        "telemetry_stale" => CheckResult {
            name: "rch_worker_pressure",
            severity: CheckSeverity::Warning,
            message: format!(
                "RCH worker pressure telemetry is stale for {} of {} worker(s).",
                report.stale_worker_count, report.worker_count
            ),
            error_code: None,
            repair: Some("rch status --workers --jobs --json"),
            tier: CheckTier::Core,
        },
        "pressure_degraded" => CheckResult {
            name: "rch_worker_pressure",
            severity: CheckSeverity::Warning,
            message: format!(
                "RCH worker pressure is degraded; {} usable, {} blocked, {} stale.",
                report.usable_worker_count, report.blocked_worker_count, report.stale_worker_count
            ),
            error_code: None,
            repair: Some("rch status --workers --jobs --json"),
            tier: CheckTier::Core,
        },
        _ => CheckResult {
            name: "rch_worker_pressure",
            severity: CheckSeverity::Warning,
            message: format!(
                "RCH worker pressure returned unrecognized status '{}'.",
                report.status
            ),
            error_code: None,
            repair: Some("rch status --workers --jobs --json"),
            tier: CheckTier::Core,
        },
    }
}

fn check_rch_verify_ledger(report: &RchVerifyLedgerStatusReport) -> CheckResult {
    match report.status {
        "active_blockers" => CheckResult {
            name: "verification_ledger",
            severity: CheckSeverity::Warning,
            message: format!(
                "RCH verifier ledger has {} active blocker(s); local fallback refused: {}.",
                report.active_blocker_count, report.local_fallback_refused
            ),
            error_code: None,
            repair: Some("ee verify rch blockers --workspace . --json"),
            tier: CheckTier::Core,
        },
        "clear" => CheckResult::ok(
            "verification_ledger",
            "RCH verifier ledger has no active blockers.",
        ),
        "not_initialized" | "not_inspected" => CheckResult::ok(
            "verification_ledger",
            "RCH verifier ledger is not initialized for this workspace.",
        ),
        _ => CheckResult {
            name: "verification_ledger",
            severity: CheckSeverity::Warning,
            message: format!(
                "RCH verifier ledger could not be inspected (status: {}).",
                report.status
            ),
            error_code: None,
            repair: Some("ee doctor --workspace . --json"),
            tier: CheckTier::Core,
        },
    }
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
            version: "0.3.3",
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
        release_pin_decision: "Registry version 0.3.3 is accepted; /dp/asupersync remains the local source reference for API checks.",
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
        diagnostic_command: "ee status --json",
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
        diagnostic_command: "ee diag graph --json",
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
        name: "franken_mermaid",
        kind: "planned_rust_crate",
        owning_surface: "ee-diagram",
        status: "planned_not_linked",
        enabled_by_default: false,
        source: DependencySource {
            kind: "not_linked",
            version: "unresolved",
            path: "/dp/franken_mermaid",
        },
        default_feature_profile: DependencyFeatureProfile {
            default_features: false,
            features: &[],
        },
        optional_feature_profiles: &[DependencyOptionalFeatureProfile {
            name: "franken-mermaid-adapter",
            features: &["diagram-validation"],
            status: "blocked_until_repository_api_and_dependency_audit",
        }],
        blocked_features: &[DependencyBlockedFeature {
            name: "browser-or-network-renderer",
            forbidden_crates: &["tokio", "hyper", "axum", "tower", "reqwest"],
            action: "plain_mermaid_text_remains_the_default_until_adapter_tree_is_clean",
        }],
        forbidden_transitive_dependencies: &[],
        minimum_smoke_test: "Gate 11 Mermaid goldens plus future FrankenMermaid adapter cargo-tree audit",
        degradation_code: "diagram_backend_unavailable",
        status_fields: &["capabilities.output.diagram", "degraded[].code"],
        diagnostic_command: "ee doctor --json",
        release_pin_decision: "Do not link before /dp/franken_mermaid exists, its API is audited, and a forbidden-dependency cargo-tree gate passes.",
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
    use crate::models::install::{
        INSTALL_FRESHNESS_SCHEMA_V1, InstallFreshnessReport, InstallFreshnessVerdict,
        InstallVersionEvidence,
    };
    use crate::models::{
        CurrentBinary, InstallCheckReport, InstallFinding, InstallFindingCode, InstallPathAnalysis,
        InstallPathStatus, InstallPermissionCheck, InstallPermissionStatus, InstallTarget,
        PathBinary, UpdateSourcePosture,
    };

    type TestResult = Result<(), String>;
    const TEST_WORKSPACE_ID: &str = "wsp_01234567890123456789012345";

    fn ensure<T: std::fmt::Debug + PartialEq>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    fn install_report_with_shadowed_path() -> InstallCheckReport {
        InstallCheckReport {
            command: "install check".to_owned(),
            schema: crate::models::INSTALL_CHECK_SCHEMA_V1.to_owned(),
            version: "0.12.0".to_owned(),
            current_binary: CurrentBinary {
                path: Some("/Users/alice/.local/bin/ee".to_owned()),
                version: "0.12.0".to_owned(),
                source: "running_process".to_owned(),
            },
            target: InstallTarget {
                target_triple: "x86_64-pc-windows-msvc".to_owned(),
                supported: true,
                binary_name: "ee".to_owned(),
                executable_name: "ee.exe".to_owned(),
                install_dir: "/Users/alice/.local/bin".to_owned(),
                install_path: "/Users/alice/.local/bin/ee".to_owned(),
            },
            path: InstallPathAnalysis {
                status: InstallPathStatus::Duplicate,
                path_entries: vec![
                    "/opt/old-ee".to_owned(),
                    "/Users/alice/.local/bin".to_owned(),
                ],
                binaries: vec![
                    PathBinary {
                        path: "/opt/old-ee/ee".to_owned(),
                        ordinal: 0,
                        is_current_binary: false,
                        version: Some("0.5.0".to_owned()),
                        version_status: Some("reported".to_owned()),
                    },
                    PathBinary {
                        path: "/Users/alice/.local/bin/ee".to_owned(),
                        ordinal: 1,
                        is_current_binary: true,
                        version: Some("0.12.0".to_owned()),
                        version_status: Some("reported".to_owned()),
                    },
                ],
                first_binary: Some("/opt/old-ee/ee".to_owned()),
                current_binary_on_path: true,
                duplicate_count: 2,
            },
            permissions: InstallPermissionCheck {
                status: InstallPermissionStatus::Writable,
                install_dir: "/Users/alice/.local/bin".to_owned(),
                target_path: "/Users/alice/.local/bin/ee".to_owned(),
                exists: true,
                writable: true,
            },
            update_source: UpdateSourcePosture {
                configured: false,
                offline: true,
                source: None,
                status: "offline_no_manifest".to_owned(),
            },
            freshness: InstallFreshnessReport {
                schema: INSTALL_FRESHNESS_SCHEMA_V1.to_owned(),
                verdict: InstallFreshnessVerdict::ShadowedBinary,
                authoritative: false,
                comparison: "equal".to_owned(),
                source_version: InstallVersionEvidence {
                    version: Some("0.12.0".to_owned()),
                    source: "cargo_toml".to_owned(),
                    status: "ok".to_owned(),
                    path: Some("/repo/Cargo.toml".to_owned()),
                    path_class: Some("host_local_path".to_owned()),
                },
                installed_version: InstallVersionEvidence {
                    version: Some("0.12.0".to_owned()),
                    source: "current_binary".to_owned(),
                    status: "ok".to_owned(),
                    path: Some("/Users/alice/.local/bin/ee".to_owned()),
                    path_class: Some("host_local_path".to_owned()),
                },
                path_status: InstallPathStatus::Duplicate,
                required_surfaces: vec!["install_check".to_owned()],
                missing_required_surfaces: Vec::new(),
                blocking_findings: vec![InstallFindingCode::CurrentBinaryShadowed],
                repair: "Run the first ee binary from the intended install target.".to_owned(),
            },
            findings: vec![
                InstallFinding::warning(
                    InstallFindingCode::DuplicatePathBinary,
                    "2 'ee' binaries were found in PATH",
                    "remove stale PATH entries or move the intended ee earlier",
                ),
                InstallFinding::warning(
                    InstallFindingCode::PathBinaryVersionMismatch,
                    "1 PATH ee binary reports a different version than the running binary (0.12.0): /opt/old-ee/ee=0.5.0",
                    "remove or replace stale PATH binaries",
                ),
                InstallFinding::error(
                    InstallFindingCode::CurrentBinaryShadowed,
                    "the running ee binary (/Users/alice/.local/bin/ee) is shadowed by the first PATH binary (/opt/old-ee/ee)",
                    "fix PATH ordering before trusting shell-invoked ee",
                ),
            ],
        }
    }

    fn clean_install_report() -> InstallCheckReport {
        let mut report = install_report_with_shadowed_path();
        report.path.status = InstallPathStatus::Ok;
        report.path.binaries = vec![PathBinary {
            path: "/Users/alice/.local/bin/ee".to_owned(),
            ordinal: 0,
            is_current_binary: true,
            version: Some("0.12.0".to_owned()),
            version_status: Some("reported".to_owned()),
        }];
        report.path.first_binary = Some("/Users/alice/.local/bin/ee".to_owned());
        report.path.duplicate_count = 1;
        report.freshness.verdict = InstallFreshnessVerdict::Fresh;
        report.freshness.authoritative = true;
        report.freshness.path_status = InstallPathStatus::Ok;
        report.freshness.blocking_findings.clear();
        report.freshness.repair = "Installed ee matches the local source version.".to_owned();
        report.findings = vec![InstallFinding::info(
            InstallFindingCode::NoUpdateSourceConfigured,
            "No update source is configured; install check remains local-only.",
            "Configure an update manifest when release automation is available.",
        )];
        report
    }

    fn mesh_auto_enrollment_problem_probe() -> DoctorMeshAutoEnrollmentProbe {
        DoctorMeshAutoEnrollmentProbe {
            workspace_path: "/tmp/ee-doctor-mesh".to_owned(),
            mesh_enabled: true,
            mesh_enabled_source: "test",
            tailscale: Some(TailscaleLocalReport {
                schema: "ee.tailscale.local.v1",
                installed: false,
                daemon_reachable: false,
                authenticated: false,
                binary_authentic: false,
                binary_version_raw: None,
                binary_absolute_path: None,
                shields_up: Some(true),
                tailnet_id: None,
                tailnet_display_name: None,
                self_node_key: None,
                self_tailscale_ip: None,
                self_magic_dns_name: None,
                self_advertised_tags: Vec::new(),
                peers: Vec::new(),
                version: None,
                probe_method: crate::core::tailscale_probe::TailscaleProbeMethod::Cli,
                probe_elapsed_ms: 10,
                platform: TailscalePlatform::MacosOpen,
                degradations: Vec::new(),
            }),
            hello_responder: Some(HelloResponderStatusReport {
                schema: "ee.mesh.hello_responder.status.v1",
                running: false,
                listen_address: None,
                accepted_requests_1h: 0,
                denied_requests_1h: 0,
                rate_limited_requests_1h: 0,
                last_request_at: None,
                last_restart_at: None,
                crash_count_24h: 3,
                degraded: Vec::new(),
            }),
            audit_chain_intact: Some(false),
            steward_consecutive_failures_24h: Some(2),
            steward_state_file_readable: Some(false),
            discovery_cache_stale_beyond_workspace: Some(true),
            materialized_peer_group_count: 0,
            materialized_peer_count: 0,
            materialized_peer_group_consistent: Some(false),
            mcp_parity_present: Some(false),
        }
    }

    fn mesh_auto_enrollment_disabled_probe() -> DoctorMeshAutoEnrollmentProbe {
        DoctorMeshAutoEnrollmentProbe {
            workspace_path: "/tmp/ee-doctor-mesh".to_owned(),
            mesh_enabled: false,
            mesh_enabled_source: "default",
            tailscale: None,
            hello_responder: None,
            audit_chain_intact: None,
            steward_consecutive_failures_24h: None,
            steward_state_file_readable: None,
            discovery_cache_stale_beyond_workspace: None,
            materialized_peer_group_count: 0,
            materialized_peer_count: 0,
            materialized_peer_group_consistent: None,
            mcp_parity_present: None,
        }
    }

    #[test]
    fn doctor_mesh_auto_enrollment_returns_skipped_when_mesh_disabled() -> TestResult {
        let report =
            DoctorMeshAutoEnrollmentReport::from_probe(&mesh_auto_enrollment_disabled_probe());

        ensure(report.enabled, false, "mesh disabled")?;
        ensure(report.checks.len(), 15, "all readiness checks emitted")?;
        ensure(
            report
                .checks
                .iter()
                .all(|check| check.status == DoctorMeshAutoEnrollmentCheckStatus::Skipped),
            true,
            "all readiness checks are skipped",
        )?;
        ensure(
            report.action_graph.actions.is_empty(),
            true,
            "disabled mesh emits no repair actions",
        )?;
        ensure(report.categorized_summary.skipped, 15, "skipped count")
    }

    #[test]
    fn doctor_mesh_action_graph_schema_reuses_shared_repair_action_graph_v1() -> TestResult {
        let report =
            DoctorMeshAutoEnrollmentReport::from_probe(&mesh_auto_enrollment_problem_probe());

        ensure(
            report.action_graph.schema.as_str(),
            REPAIR_ACTION_GRAPH_SCHEMA_V1,
            "action graph schema",
        )
    }

    #[test]
    fn doctor_mesh_action_graph_orders_dependencies_topologically() -> TestResult {
        let report =
            DoctorMeshAutoEnrollmentReport::from_probe(&mesh_auto_enrollment_problem_probe());
        let order = &report.action_graph.topologically_ordered_execution;
        let position = |id: &str| {
            order
                .iter()
                .position(|candidate| candidate == id)
                .ok_or_else(|| format!("missing action {id}"))
        };

        ensure(
            position("tailscale_install")? < position("tailscale_up")?,
            true,
            "tailscale install precedes tailscale up",
        )?;
        ensure(
            position("tailscale_up")? < position("ee_daemon_start")?,
            true,
            "tailscale up precedes daemon start",
        )?;
        ensure(
            position("ee_daemon_start")? < position("ee_mesh_auto_enroll")?,
            true,
            "daemon start precedes auto-enroll",
        )
    }

    #[test]
    fn doctor_mesh_action_graph_groups_independent_actions_for_parallel_execution() -> TestResult {
        let report =
            DoctorMeshAutoEnrollmentReport::from_probe(&mesh_auto_enrollment_problem_probe());
        let first_group = report
            .action_graph
            .parallelizable_groups
            .first()
            .ok_or_else(|| "expected at least one parallel group".to_owned())?;

        ensure(
            first_group.contains(&"inspect_auto_enrollment_audit".to_owned())
                && first_group.contains(&"inspect_steward_state".to_owned()),
            true,
            "independent inspection actions share first group",
        )?;
        ensure(
            first_group.len() > 1,
            true,
            "first group has parallel actions",
        )
    }

    #[test]
    fn doctor_mesh_action_graph_estimates_action_and_total_duration() -> TestResult {
        let report =
            DoctorMeshAutoEnrollmentReport::from_probe(&mesh_auto_enrollment_problem_probe());
        let sum = report
            .action_graph
            .actions
            .iter()
            .map(|action| u64::from(action.estimated_duration_seconds))
            .sum::<u64>();

        ensure(
            report.action_graph.estimated_total_duration_seconds,
            sum,
            "total duration sums actions",
        )?;
        ensure(sum > 0, true, "duration is non-zero")
    }

    #[test]
    fn doctor_mesh_action_graph_distinguishes_action_kinds() -> TestResult {
        let report =
            DoctorMeshAutoEnrollmentReport::from_probe(&mesh_auto_enrollment_problem_probe());
        let kinds = report
            .action_graph
            .actions
            .iter()
            .map(|action| action.kind.as_str())
            .collect::<std::collections::BTreeSet<_>>();

        ensure(
            kinds.contains("shell_command"),
            true,
            "shell command action",
        )?;
        ensure(
            kinds.contains("ee_subcommand"),
            true,
            "ee subcommand action",
        )?;
        ensure(
            kinds.contains("external_tool"),
            true,
            "external tool action",
        )?;
        ensure(kinds.contains("manual_step"), true, "manual step action")
    }

    #[test]
    fn doctor_mesh_action_graph_escapes_shell_sensitive_workspace_path() -> TestResult {
        let mut probe = mesh_auto_enrollment_problem_probe();
        probe.workspace_path = "/tmp/ee \"quoted\" $HOME".to_owned();
        let report = DoctorMeshAutoEnrollmentReport::from_probe(&probe);
        let expected = "--workspace \"/tmp/ee \\\"quoted\\\" \\$HOME\"";

        for action in &report.action_graph.actions {
            if action.command.contains("--workspace") {
                ensure(
                    action.command.contains(expected),
                    true,
                    &format!("{} command quotes workspace", action.id),
                )?;
            }
            if let Some(reversal) = &action.reversal_command {
                if reversal.contains("--workspace") {
                    ensure(
                        reversal.contains(expected),
                        true,
                        &format!("{} reversal quotes workspace", action.id),
                    )?;
                }
            }
        }
        Ok(())
    }

    #[test]
    fn doctor_mesh_action_graph_marks_reversible_and_destructive_actions() -> TestResult {
        let report =
            DoctorMeshAutoEnrollmentReport::from_probe(&mesh_auto_enrollment_problem_probe());
        let disable = report
            .action_graph
            .actions
            .iter()
            .find(|action| action.id == "ee_mesh_disable")
            .ok_or_else(|| "ee_mesh_disable action missing".to_owned())?;
        let daemon = report
            .action_graph
            .actions
            .iter()
            .find(|action| action.id == "ee_daemon_start")
            .ok_or_else(|| "ee_daemon_start action missing".to_owned())?;

        ensure(disable.reversible, false, "mesh disable is not reversible")?;
        ensure(
            disable.requires_user_confirmation,
            true,
            "mesh disable requires confirmation",
        )?;
        ensure(daemon.reversible, true, "daemon start is reversible")
    }

    #[test]
    fn doctor_mesh_categorized_summary_matches_individual_checks() -> TestResult {
        let report =
            DoctorMeshAutoEnrollmentReport::from_probe(&mesh_auto_enrollment_problem_probe());
        let recomputed = DoctorMeshAutoEnrollmentSummary::from_checks(&report.checks);

        ensure(
            &report.categorized_summary,
            &recomputed,
            "summary matches checks",
        )?;
        ensure(report.categorized_summary.total, 15, "summary total")
    }

    #[test]
    fn doctor_report_gather_returns_checks() -> TestResult {
        let report = DoctorReport::gather_with_workspace(None);

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

        let install_path = report.checks.iter().find(|c| c.name == "ee_install_path");
        ensure(
            install_path.map(|c| c.tier),
            Some(CheckTier::Advisory),
            "ee install PATH check is advisory",
        )?;

        Ok(())
    }

    #[test]
    fn doctor_report_overall_healthy_reflects_all_checks() -> TestResult {
        let report = DoctorReport::gather_with_workspace(None);

        let has_topline_issues = report.checks.iter().any(|c| !c.is_topline_healthy());

        ensure(
            report.overall_healthy,
            !has_topline_issues,
            "overall_healthy matches core check status",
        )
    }

    #[test]
    fn check_result_ok_has_no_error_code() -> TestResult {
        let check = CheckResult::ok("test", "All good");
        ensure(check.error_code.is_none(), true, "ok has no error code")?;
        ensure(check.repair.is_none(), true, "ok has no repair")?;
        ensure(check.tier, CheckTier::Core, "ok defaults to core")
    }

    #[test]
    fn check_result_warning_has_error_code_and_repair() -> TestResult {
        let check = CheckResult::warning("test", "Issue found", error_codes::DATABASE_NOT_FOUND);
        ensure(check.error_code.is_some(), true, "warning has error code")?;
        ensure(check.repair.is_some(), true, "warning has repair from code")
    }

    #[test]
    fn ee_install_path_advisory_names_shadowing_paths_versions_and_repair() -> TestResult {
        let report = install_report_with_shadowed_path();
        let check = ee_install_path_check_from_report(&report);

        ensure(check.name, "ee_install_path", "check name")?;
        ensure(check.tier, CheckTier::Advisory, "check is advisory")?;
        ensure(check.severity, CheckSeverity::Warning, "check severity")?;
        ensure(
            check.is_topline_healthy(),
            true,
            "install PATH warnings do not affect top-line memory health",
        )?;
        ensure(
            check.error_code,
            None,
            "advisory does not invent error code",
        )?;
        ensure(check.repair.is_some(), true, "repair hint is present")?;

        for needle in [
            "duplicate_path_binary",
            "current_binary_shadowed",
            "path_binary_version_mismatch",
            "/opt/old-ee/ee",
            "0.5.0",
            "/Users/alice/.local/bin/ee",
            "0.12.0",
            "local source version 0.12.0",
            "freshness shadowed_binary",
            "no network lookup",
        ] {
            ensure(
                check.message.contains(needle),
                true,
                &format!("message includes {needle}"),
            )?;
        }

        Ok(())
    }

    #[test]
    fn ee_install_path_clean_report_is_advisory_ok() -> TestResult {
        let report = clean_install_report();
        let check = ee_install_path_check_from_report(&report);

        ensure(check.name, "ee_install_path", "check name")?;
        ensure(check.tier, CheckTier::Advisory, "check is advisory")?;
        ensure(check.severity, CheckSeverity::Ok, "clean severity")?;
        ensure(
            check
                .message
                .contains("no shadowed or stale ee binary detected"),
            true,
            "clean message",
        )
    }

    #[test]
    fn posture_ignores_advisory_install_path_warning() -> TestResult {
        let checks = vec![
            CheckResult::ok("runtime", "ok"),
            CheckResult::ok("database", "ok"),
            ee_install_path_check_from_report(&install_report_with_shadowed_path()),
        ];

        ensure(
            Posture::from_checks(&checks, None),
            Posture::Ok,
            "advisory warning ignored by posture",
        )?;
        ensure(
            checks.iter().all(CheckResult::is_topline_healthy),
            true,
            "advisory warning ignored by legacy healthy bool",
        )
    }

    #[test]
    fn rch_worker_pressure_check_warns_on_blocked_workers() -> TestResult {
        let report = RchWorkerPressureReport {
            schema: super::super::swarm_brief::RCH_WORKER_PRESSURE_SCHEMA_V1,
            status: "healthy_but_pressure_blocked".to_string(),
            worker_count: 2,
            usable_worker_count: 0,
            blocked_worker_count: 2,
            stale_worker_count: 0,
            unknown_worker_count: 0,
            workers: Vec::new(),
        };

        let check = check_rch_worker_pressure(&report);

        ensure(check.name, "rch_worker_pressure", "check name")?;
        ensure(check.severity, CheckSeverity::Warning, "check severity")?;
        ensure(
            check.error_code,
            None,
            "check does not invent ee error code",
        )?;
        ensure(
            check.repair,
            Some("rch status --workers --jobs --json"),
            "repair command",
        )
    }

    #[test]
    fn doctor_report_includes_memory_tier_posture_checks() -> TestResult {
        let report = DoctorReport::gather_with_workspace(None);

        ensure(
            report
                .checks
                .iter()
                .any(|check| check.name == "lexical_ram_tier"),
            true,
            "lexical RAM-tier doctor check exists",
        )?;
        ensure(
            report
                .checks
                .iter()
                .any(|check| check.name == "graph_numa_pin"),
            true,
            "graph NUMA pin doctor check exists",
        )?;
        ensure(
            report
                .checks
                .iter()
                .any(|check| check.name == "daemon_socket_reachable"),
            true,
            "daemon socket doctor check exists",
        )
    }

    #[test]
    fn daemon_socket_check_is_ok_when_socket_is_absent() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let socket_path = temp.path().join("missing-daemon.sock");

        let check = check_daemon_socket_reachable_at(&socket_path);

        ensure(check.name, "daemon_socket_reachable", "check name")?;
        ensure(
            check.severity,
            CheckSeverity::Ok,
            "absent optional daemon is ok",
        )?;
        ensure(
            check.message.contains("not present"),
            true,
            "message explains absent optional daemon",
        )
    }

    #[cfg(unix)]
    #[test]
    fn daemon_socket_check_warns_when_path_is_not_socket() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let socket_path = temp.path().join("daemon.sock");
        std::fs::write(&socket_path, b"not a socket").map_err(|error| error.to_string())?;

        let check = check_daemon_socket_reachable_at(&socket_path);

        ensure(check.name, "daemon_socket_reachable", "check name")?;
        ensure(
            check.severity,
            CheckSeverity::Warning,
            "non-socket daemon path warns",
        )?;
        ensure(
            check.message.contains("not a Unix-domain socket"),
            true,
            "message explains non-socket path",
        )?;
        ensure(check.repair.is_some(), true, "warning has repair guidance")
    }

    #[cfg(unix)]
    #[test]
    fn daemon_socket_check_is_ok_when_socket_accepts_connection() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let socket_path = temp.path().join("daemon.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket_path).map_err(|error| {
            format!(
                "bind daemon socket fixture {}: {error}",
                socket_path.display()
            )
        })?;

        let check = check_daemon_socket_reachable_at(&socket_path);

        ensure(check.name, "daemon_socket_reachable", "check name")?;
        ensure(
            check.severity,
            CheckSeverity::Ok,
            "connectable daemon socket is ok",
        )?;
        ensure(
            check.message.contains("accepts local connections"),
            true,
            "message explains reachable daemon socket",
        )
    }

    #[test]
    fn lexical_ram_tier_check_is_ok_when_disabled() -> TestResult {
        let check = check_lexical_ram_tier_with_config(None, LexicalRamTierConfig::disabled());

        ensure(check.name, "lexical_ram_tier", "check name")?;
        ensure(check.severity, CheckSeverity::Ok, "disabled is ok")?;
        ensure(
            check.message.contains("disabled"),
            true,
            "message explains disabled posture",
        )
    }

    #[test]
    fn lexical_ram_tier_check_warns_when_enabled_but_degraded() -> TestResult {
        let check = check_lexical_ram_tier_with_config(
            None,
            LexicalRamTierConfig {
                enabled: true,
                request_hugepages: false,
                populate_on_open: true,
            },
        );

        ensure(check.name, "lexical_ram_tier", "check name")?;
        ensure(
            check.severity,
            CheckSeverity::Warning,
            "enabled scaffold degrades",
        )?;
        ensure(
            check.message.contains("degraded codes:"),
            true,
            "message includes degraded codes",
        )?;
        ensure(
            check.repair.is_some(),
            true,
            "degraded posture has repair guidance",
        )
    }

    #[test]
    fn graph_numa_pin_check_is_ok_when_disabled() -> TestResult {
        let check = check_graph_numa_pin_with_config(None, NumaPinConfig::disabled());

        ensure(check.name, "graph_numa_pin", "check name")?;
        ensure(check.severity, CheckSeverity::Ok, "disabled is ok")?;
        ensure(
            check.message.contains("disabled"),
            true,
            "message explains disabled posture",
        )
    }

    #[test]
    fn graph_numa_pin_check_warns_when_enabled_but_degraded() -> TestResult {
        let check = check_graph_numa_pin_with_config(None, NumaPinConfig::default());

        ensure(check.name, "graph_numa_pin", "check name")?;
        ensure(
            check.severity,
            CheckSeverity::Warning,
            "enabled scaffold degrades",
        )?;
        ensure(
            check.message.contains("degraded codes:"),
            true,
            "message includes degraded codes",
        )?;
        ensure(
            check.repair.is_some(),
            true,
            "degraded posture has repair guidance",
        )
    }

    #[test]
    fn check_severity_strings_are_stable() -> TestResult {
        ensure(CheckSeverity::Ok.as_str(), "ok", "ok")?;
        ensure(CheckSeverity::Warning.as_str(), "warning", "warning")?;
        ensure(CheckSeverity::Error.as_str(), "error", "error")
    }

    #[test]
    fn fix_plan_contains_only_fixable_issues() -> TestResult {
        let report = DoctorReport::gather_with_workspace(None);
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
        let report = DoctorReport::gather_with_workspace(None);
        let plan = report.to_fix_plan();

        for (idx, step) in plan.steps.iter().enumerate() {
            ensure(step.order, idx + 1, "step order is sequential")?;
        }

        Ok(())
    }

    #[test]
    fn fix_plan_counts_match() -> TestResult {
        let report = DoctorReport::gather_with_workspace(None);
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
    fn doctor_report_uses_explicit_workspace_path() -> TestResult {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        std::fs::create_dir(dir.path().join(".ee"))
            .map_err(|error| format!("failed to create .ee dir: {error}"))?;

        let report = DoctorReport::gather_for_workspace(dir.path());
        let workspace = report
            .checks
            .iter()
            .find(|check| check.name == "workspace")
            .ok_or_else(|| "workspace check missing".to_string())?;

        ensure(
            workspace.severity,
            CheckSeverity::Ok,
            "workspace check should inspect selected path",
        )?;
        ensure(
            workspace
                .message
                .contains(&dir.path().display().to_string()),
            true,
            "workspace message includes selected path",
        )
    }

    #[cfg(unix)]
    #[test]
    fn doctor_canonicalizes_symlinked_workspace_before_database_checks() -> TestResult {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let target = root.path().join("real-workspace");
        let alias = root.path().join("alias-workspace");
        let ee_dir = target.join(".ee");
        std::fs::create_dir_all(&ee_dir).map_err(|error| error.to_string())?;
        symlink(&target, &alias).map_err(|error| error.to_string())?;

        let database_path = ee_dir.join("ee.db");
        let connection = crate::db::DbConnection::open_file(&database_path)
            .map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let report = DoctorReport::gather_for_workspace(&alias);
        let database = report
            .checks
            .iter()
            .find(|check| check.name == "database")
            .ok_or_else(|| "database check missing".to_string())?;
        ensure(
            database.severity,
            CheckSeverity::Ok,
            "database check should use canonical path",
        )?;
        ensure(
            database
                .message
                .contains(&database_path.display().to_string()),
            true,
            "database message includes canonical database path",
        )
    }

    #[test]
    fn fix_plan_is_empty_when_all_healthy() -> TestResult {
        let report = DoctorReport {
            version: "0.1.0",
            overall_healthy: true,
            posture: Posture::Ok,
            singleflight_posture: singleflight_posture_report(),
            qos_posture: super::gather_qos_posture(None),
            rch_worker_pressure: RchWorkerPressureReport::pressure_unknown(),
            verification_posture: VerificationPostureReport::not_inspected(),
            verification_ledger: RchVerifyLedgerStatusReport::not_inspected(),
            host_calibration: None,
            flight_recorder: FlightRecorderStatusReport::disabled(PathBuf::from(
                "obs/flight_recorder",
            )),
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
    fn fix_plan_default_guidance_defers_agent_root_inspection() -> TestResult {
        let report = DoctorReport {
            version: "0.1.0",
            overall_healthy: true,
            posture: Posture::Ok,
            singleflight_posture: singleflight_posture_report(),
            qos_posture: super::gather_qos_posture(None),
            rch_worker_pressure: RchWorkerPressureReport::pressure_unknown(),
            verification_posture: VerificationPostureReport::not_inspected(),
            verification_ledger: RchVerifyLedgerStatusReport::not_inspected(),
            host_calibration: None,
            flight_recorder: FlightRecorderStatusReport::disabled(PathBuf::from(
                "obs/flight_recorder",
            )),
            checks: vec![],
        };
        let plan = report.to_fix_plan();

        ensure(
            plan.cass_import_guidance.status,
            CassImportGuidanceStatus::NotInspected,
            "default guidance is deferred",
        )?;
        ensure(
            plan.cass_import_guidance.detected_root_count,
            0,
            "deferred guidance has no roots",
        )?;
        ensure(
            plan.cass_import_guidance
                .suggested_commands
                .contains(&"ee agent status --json".to_string()),
            true,
            "deferred guidance suggests agent status",
        )
    }

    #[test]
    fn fix_plan_agent_inventory_guidance_uses_detected_roots() -> TestResult {
        let inventory = AgentInventoryReport::from_detection(
            crate::core::agent_detect::detect_fixture_agents()
                .map_err(|error| error.to_string())?,
        );
        let report = DoctorReport {
            version: "0.1.0",
            overall_healthy: false,
            posture: Posture::DegradedRecoverable,
            singleflight_posture: singleflight_posture_report(),
            qos_posture: super::gather_qos_posture(None),
            rch_worker_pressure: RchWorkerPressureReport::pressure_unknown(),
            verification_posture: VerificationPostureReport::not_inspected(),
            verification_ledger: RchVerifyLedgerStatusReport::not_inspected(),
            host_calibration: None,
            flight_recorder: FlightRecorderStatusReport::disabled(PathBuf::from(
                "obs/flight_recorder",
            )),
            checks: vec![CheckResult::warning(
                "cass",
                "CASS import dry-run recommended.",
                error_codes::AGENT_SOURCE_NOT_IMPORTED,
            )],
        };
        let plan = report.to_fix_plan_with_agent_inventory(&inventory);

        ensure(
            plan.cass_import_guidance.status,
            CassImportGuidanceStatus::AgentRootsDetected,
            "fixture roots detected",
        )?;
        ensure(
            plan.cass_import_guidance.detected_root_count >= 4,
            true,
            "fixture detected roots counted",
        )?;
        ensure(
            plan.cass_import_guidance
                .roots
                .iter()
                .any(|root| root.connector == "codex"),
            true,
            "codex fixture root present",
        )?;
        ensure(
            plan.cass_import_guidance
                .suggested_commands
                .contains(&"ee import cass --dry-run --json".to_string()),
            true,
            "CASS dry-run command suggested",
        )
    }

    #[test]
    fn integrity_diagnostics_missing_database_degrades_without_creating_file() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join("missing-ee.db");

        let report = IntegrityDiagnosticsReport::gather(&IntegrityDiagnosticsOptions {
            workspace_path: temp.path().to_path_buf(),
            database_path: Some(database_path.clone()),
            workspace_id: "default".to_string(),
            sample_size: 8,
            create_canary: false,
            dry_run: false,
        });

        ensure(
            report.status,
            IntegrityDiagnosticsStatus::Degraded,
            "missing db degrades",
        )?;
        ensure(database_path.exists(), false, "missing db was not created")?;
        ensure(
            report.canary.status,
            IntegrityCanaryStatus::NotRequested,
            "canary not requested",
        )?;
        ensure(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == "integrity_database_missing"),
            true,
            "missing database degradation present",
        )
    }

    #[test]
    fn integrity_diagnostics_canary_dry_run_does_not_write_memory() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join("ee.db");
        let connection = crate::db::DbConnection::open_file(&database_path)
            .map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                TEST_WORKSPACE_ID,
                &crate::db::CreateWorkspaceInput {
                    path: temp.path().to_string_lossy().into_owned(),
                    name: Some("integrity-test".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let report = IntegrityDiagnosticsReport::gather(&IntegrityDiagnosticsOptions {
            workspace_path: temp.path().to_path_buf(),
            database_path: Some(database_path.clone()),
            workspace_id: TEST_WORKSPACE_ID.to_string(),
            sample_size: 8,
            create_canary: true,
            dry_run: true,
        });

        ensure(
            report.canary.status,
            IntegrityCanaryStatus::DryRun,
            "dry-run canary status",
        )?;

        let connection = crate::db::DbConnection::open_file(&database_path)
            .map_err(|error| error.to_string())?;
        ensure(
            connection
                .get_memory(INTEGRITY_CANARY_MEMORY_ID)
                .map_err(|error| error.to_string())?
                .is_none(),
            true,
            "dry run did not write canary",
        )
    }

    #[test]
    fn integrity_diagnostics_create_canary_is_idempotent() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join("ee.db");
        let connection = crate::db::DbConnection::open_file(&database_path)
            .map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                TEST_WORKSPACE_ID,
                &crate::db::CreateWorkspaceInput {
                    path: temp.path().to_string_lossy().into_owned(),
                    name: Some("integrity-test".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let options = IntegrityDiagnosticsOptions {
            workspace_path: temp.path().to_path_buf(),
            database_path: Some(database_path.clone()),
            workspace_id: TEST_WORKSPACE_ID.to_string(),
            sample_size: 8,
            create_canary: true,
            dry_run: false,
        };

        let first = IntegrityDiagnosticsReport::gather(&options);
        ensure(
            first.canary.status,
            IntegrityCanaryStatus::Created,
            "first run creates canary",
        )?;

        let second = IntegrityDiagnosticsReport::gather(&options);
        ensure(
            second.canary.status,
            IntegrityCanaryStatus::AlreadyExists,
            "second run is idempotent",
        )?;

        let connection = crate::db::DbConnection::open_file(&database_path)
            .map_err(|error| error.to_string())?;
        let memory = connection
            .get_memory(INTEGRITY_CANARY_MEMORY_ID)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "canary memory should exist".to_string())?;
        ensure(
            memory.trust_class,
            TrustClass::AgentAssertion.as_str().to_string(),
            "canary trust class",
        )
    }

    #[test]
    fn integrity_diagnostics_unmigrated_database_skips_canary() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join("ee.db");
        let connection = crate::db::DbConnection::open_file(&database_path)
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let report = IntegrityDiagnosticsReport::gather(&IntegrityDiagnosticsOptions {
            workspace_path: temp.path().to_path_buf(),
            database_path: Some(database_path),
            workspace_id: "default".to_string(),
            sample_size: 8,
            create_canary: true,
            dry_run: false,
        });

        ensure(
            report.status,
            IntegrityDiagnosticsStatus::Degraded,
            "unmigrated db degrades",
        )?;
        ensure(
            report.canary.status,
            IntegrityCanaryStatus::Skipped,
            "unmigrated db skips canary",
        )?;
        ensure(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == "integrity_schema_migration_required"),
            true,
            "schema migration degradation present",
        )
    }

    #[test]
    fn integrity_diagnostics_reports_reference_integrity_issues() -> TestResult {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let database_path = temp.path().join("ee.db");
        let connection = crate::db::DbConnection::open_file(&database_path)
            .map_err(|error| error.to_string())?;
        connection.migrate().map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                TEST_WORKSPACE_ID,
                &crate::db::CreateWorkspaceInput {
                    path: temp.path().to_string_lossy().into_owned(),
                    name: Some("integrity-main".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_workspace(
                "wsp_98765432109876543210987654",
                &crate::db::CreateWorkspaceInput {
                    path: temp.path().join("alt").to_string_lossy().into_owned(),
                    name: Some("integrity-alt".to_string()),
                },
            )
            .map_err(|error| error.to_string())?;

        let memory_input = |workspace_id: &str, content: &str| CreateMemoryInput {
            workspace_id: workspace_id.to_string(),
            level: "semantic".to_string(),
            kind: "fact".to_string(),
            content: content.to_string(),
            workflow_id: None,
            confidence: TrustClass::AgentAssertion.initial_confidence(),
            utility: 0.2,
            importance: 0.2,
            provenance_uri: None,
            trust_class: TrustClass::AgentAssertion.as_str().to_string(),
            trust_subclass: None,
            tags: vec![],
            valid_from: None,
            valid_to: None,
        };

        connection
            .insert_memory(
                "mem_00000000000000000000000121",
                &memory_input(TEST_WORKSPACE_ID, "main workspace memory"),
            )
            .map_err(|error| error.to_string())?;
        connection
            .insert_memory(
                "mem_00000000000000000000000122",
                &memory_input(
                    "wsp_98765432109876543210987654",
                    "alternate workspace memory",
                ),
            )
            .map_err(|error| error.to_string())?;

        connection
            .insert_memory_link(
                "link_00000000000000000000000121",
                &crate::db::CreateMemoryLinkInput {
                    src_memory_id: "mem_00000000000000000000000121".to_string(),
                    dst_memory_id: "mem_00000000000000000000000122".to_string(),
                    relation: crate::db::MemoryLinkRelation::Supports,
                    weight: 0.9,
                    confidence: 0.9,
                    directed: true,
                    evidence_count: 1,
                    last_reinforced_at: None,
                    source: crate::db::MemoryLinkSource::Agent,
                    created_by: Some("agent:test".to_string()),
                    metadata_json: None,
                },
            )
            .map_err(|error| error.to_string())?;

        let pack_id = "pack_00000000000000000000000121";
        connection
            .insert_pack_record(
                pack_id,
                &crate::db::CreatePackRecordInput {
                    workspace_id: TEST_WORKSPACE_ID.to_string(),
                    query: "reference integrity test".to_string(),
                    profile: "compact".to_string(),
                    max_tokens: 512,
                    used_tokens: 128,
                    item_count: 2,
                    omitted_count: 0,
                    pack_hash: "blake3:ref-integrity-test".to_string(),
                    degraded_json: None,
                    created_by: Some("agent:test".to_string()),
                },
                &[crate::db::CreatePackItemInput {
                    pack_id: pack_id.to_string(),
                    memory_id: "mem_00000000000000000000000122".to_string(),
                    rank: 1,
                    section: "evidence".to_string(),
                    estimated_tokens: 64,
                    relevance: 0.8,
                    utility: 0.6,
                    why: "cross-workspace item".to_string(),
                    diversity_key: None,
                    provenance_json: r#"{"schema":"ee.pack_item.provenance.v1","entries":[]}"#
                        .to_string(),
                    trust_class: "agent_assertion".to_string(),
                    trust_subclass: None,
                }],
                &[crate::db::CreatePackOmissionInput {
                    pack_id: pack_id.to_string(),
                    memory_id: "mem_00000000000000000000000122".to_string(),
                    estimated_tokens: 64,
                    reason: "token_budget_exceeded".to_string(),
                }],
            )
            .map_err(|error| error.to_string())?;
        connection.close().map_err(|error| error.to_string())?;

        let report = IntegrityDiagnosticsReport::gather(&IntegrityDiagnosticsOptions {
            workspace_path: temp.path().to_path_buf(),
            database_path: Some(database_path),
            workspace_id: TEST_WORKSPACE_ID.to_string(),
            sample_size: 8,
            create_canary: false,
            dry_run: false,
        });

        ensure(
            report.status != IntegrityDiagnosticsStatus::Ok,
            true,
            "reference issues must not report an ok integrity status",
        )?;
        let reference_check = report
            .checks
            .iter()
            .find(|check| check.name == "reference_integrity")
            .ok_or_else(|| "missing reference_integrity check".to_string())?;
        ensure(
            reference_check.severity,
            IntegrityDiagnosticSeverity::Warning,
            "reference integrity check warns when findings exist",
        )?;
        ensure(
            report
                .degraded
                .iter()
                .any(|entry| entry.code == "integrity_reference_issues"),
            true,
            "reference integrity degradation code present",
        )
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
        ensure(report.entries.len(), 10, "matrix row count")?;
        ensure(
            report.summary.total_dependencies,
            10,
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
            8,
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
            "franken_mermaid",
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

    // ========================================================================
    // Bead bd-17c65.5.1 (E1) — Posture aggregation
    // ========================================================================

    #[test]
    fn posture_as_str_is_stable() {
        // These string forms are the JSON wire enum. Do not rename.
        assert_eq!(Posture::Ok.as_str(), "ok");
        assert_eq!(
            Posture::DegradedRecoverable.as_str(),
            "degraded_recoverable"
        );
        assert_eq!(Posture::Blocked.as_str(), "blocked");
    }

    #[test]
    fn posture_ok_when_all_checks_pass() {
        let checks = vec![
            CheckResult::ok("runtime", "ok"),
            CheckResult::ok("workspace", "ok"),
        ];
        assert_eq!(Posture::from_checks(&checks, None), Posture::Ok);
    }

    #[test]
    fn posture_blocked_on_any_error() {
        let checks = vec![
            CheckResult::ok("runtime", "ok"),
            CheckResult::error(
                "database",
                "corrupted",
                crate::models::error_codes::DATABASE_CORRUPTED,
            ),
        ];
        assert_eq!(Posture::from_checks(&checks, None), Posture::Blocked);
    }

    #[test]
    fn posture_degraded_when_warning_present_and_no_errors() {
        let checks = vec![
            CheckResult::ok("runtime", "ok"),
            CheckResult::warning(
                "search_index",
                "stale",
                crate::models::error_codes::INDEX_STALE,
            ),
        ];
        assert_eq!(
            Posture::from_checks(&checks, None),
            Posture::DegradedRecoverable
        );
    }

    #[test]
    fn posture_warning_does_not_downgrade_when_transient_predicate_marks_it() {
        let checks = vec![
            CheckResult::ok("runtime", "ok"),
            CheckResult::warning(
                "search_index",
                "stale",
                crate::models::error_codes::INDEX_STALE,
            ),
        ];
        let transient = |c: &CheckResult| c.name == "search_index";
        assert_eq!(Posture::from_checks(&checks, Some(&transient)), Posture::Ok);
    }

    #[test]
    fn posture_blocked_overrides_warning_aggregation() {
        // Even when the only warning is "transient", an Error elsewhere
        // promotes to Blocked.
        let checks = vec![
            CheckResult::warning(
                "search_index",
                "stale",
                crate::models::error_codes::INDEX_STALE,
            ),
            CheckResult::error(
                "database",
                "corrupted",
                crate::models::error_codes::DATABASE_CORRUPTED,
            ),
        ];
        let transient = |c: &CheckResult| c.name == "search_index";
        assert_eq!(
            Posture::from_checks(&checks, Some(&transient)),
            Posture::Blocked
        );
    }

    #[test]
    fn posture_empty_check_set_is_ok() {
        let checks: Vec<CheckResult> = Vec::new();
        assert_eq!(Posture::from_checks(&checks, None), Posture::Ok);
    }

    #[test]
    fn posture_ignores_advisory_warnings_but_not_core() {
        // ADR 0081 / bd-1et0v.12: advisory subsystem warnings (cass, numa,
        // rch worker pressure, …) must never flip the top-line; a CORE warning
        // still degrades. This is the analyst-finding-#4 regression.
        use crate::models::error_codes::INDEX_STALE;
        let advisory_only = vec![
            CheckResult::ok("runtime", "ok"),
            CheckResult::ok("workspace", "ok"),
            CheckResult::ok("database", "ok"),
            CheckResult::ok("search_index", "ok"),
            CheckResult::warning("cass", "missing binary", INDEX_STALE).advisory(),
            CheckResult::warning("graph_numa_pin", "unsupported platform", INDEX_STALE).advisory(),
            CheckResult::warning("rch_worker_pressure", "fleet pressure", INDEX_STALE).advisory(),
        ];
        assert_eq!(
            Posture::from_checks(&advisory_only, None),
            Posture::Ok,
            "advisory warnings must not degrade the top-line"
        );
        assert!(
            advisory_only.iter().all(CheckResult::is_topline_healthy),
            "advisory warnings must not flip overall_healthy"
        );

        let core_warning = vec![
            CheckResult::ok("runtime", "ok"),
            CheckResult::warning("search_index", "stale", INDEX_STALE),
            CheckResult::warning("cass", "missing binary", INDEX_STALE).advisory(),
        ];
        assert_eq!(
            Posture::from_checks(&core_warning, None),
            Posture::DegradedRecoverable,
            "a CORE warning still degrades even when advisory warnings are present"
        );
    }

    #[test]
    fn gathered_checks_only_core_loop_drives_topline() {
        // ADR 0081 / bd-1et0v.12: only the store/retrieve memory loop is CORE.
        // Every other gathered check must be ADVISORY so optional subsystems
        // never flip the top-line. Partition test — robust to which advisory
        // checks happen to be present in this environment.
        const CORE: &[&str] = &["runtime", "workspace", "database", "search_index"];
        let report = DoctorReport::gather_with_workspace(None);
        for check in &report.checks {
            let expected = if CORE.contains(&check.name) {
                CheckTier::Core
            } else {
                CheckTier::Advisory
            };
            assert_eq!(
                check.tier, expected,
                "check `{}` has the wrong tier (CORE = the memory loop only)",
                check.name
            );
        }
        for core in CORE {
            assert!(
                report.checks.iter().any(|check| &check.name == core),
                "core check `{core}` must always be present"
            );
        }
    }

    #[test]
    fn doctor_report_struct_carries_posture_alongside_overall_healthy() {
        // Regression: the new `posture` field is wired into the report.
        let report = DoctorReport {
            version: "0.1.0",
            overall_healthy: true,
            posture: Posture::Ok,
            singleflight_posture: singleflight_posture_report(),
            qos_posture: super::gather_qos_posture(None),
            rch_worker_pressure: RchWorkerPressureReport::pressure_unknown(),
            verification_posture: VerificationPostureReport::not_inspected(),
            verification_ledger: RchVerifyLedgerStatusReport::not_inspected(),
            host_calibration: None,
            flight_recorder: FlightRecorderStatusReport::disabled(PathBuf::from(
                "obs/flight_recorder",
            )),
            checks: vec![CheckResult::ok("runtime", "ok")],
        };
        assert_eq!(report.posture, Posture::Ok);
        assert!(report.overall_healthy);
    }

    #[test]
    fn embedding_posture_semantic_is_info_advisory_and_topline_healthy() {
        let check = embedding_posture_check_result(
            true,
            "neural_local",
            "potion-multilingual-128M",
            256,
            true,
            &[],
        );
        assert_eq!(check.severity, CheckSeverity::Ok);
        assert_eq!(check.tier, CheckTier::Advisory);
        assert!(check.is_topline_healthy());
        assert!(check.message.contains("ready"));
        assert!(check.message.contains("neural_local"));
        assert!(check.message.contains("potion-multilingual-128M"));
        assert!(check.message.contains("256d"));
        assert!(check.message.contains("ee model status"));
    }

    #[test]
    fn embedding_posture_hash_fallback_is_advisory_warning_but_never_flips_health() {
        let check = embedding_posture_check_result(
            false,
            "deterministic_hash",
            "fnv1a-256",
            256,
            true,
            &[],
        );
        assert_eq!(check.severity, CheckSeverity::Warning);
        assert_eq!(check.tier, CheckTier::Advisory);
        // Advisory tier => never participates in the top-line memory verdict.
        assert!(check.is_topline_healthy());
        assert!(check.message.contains("deterministic-hash fallback"));
        assert!(check.message.contains("embed_model_unavailable"));
        assert!(check.repair.is_some());
    }

    #[test]
    fn embedding_posture_pending_download_is_advisory_warning_not_hash_fallback() {
        let check = embedding_posture_check_result(
            false,
            EMBEDDING_POSTURE_MODE_NEURAL_LOCAL_PENDING,
            "potion-multilingual-128M",
            256,
            true,
            &[],
        );
        assert_eq!(check.severity, CheckSeverity::Warning);
        assert_eq!(check.tier, CheckTier::Advisory);
        assert!(check.is_topline_healthy());
        assert!(check.message.contains("pending first-use download"));
        assert!(check.message.contains("degraded-but-improving"));
        assert!(!check.message.contains("Degraded code: embed_model_unavailable"));
        assert!(check.repair.is_some());
    }

    #[test]
    fn embedding_posture_discloses_the_env_trap() {
        let check = embedding_posture_check_result(
            true,
            "neural_local",
            "potion-multilingual-128M",
            256,
            true,
            &["EMBEDDING_MODEL"],
        );
        assert!(check.message.contains("EMBEDDING_MODEL"));
        assert!(check.message.contains("does not consume"));
        assert!(check.message.contains("bundled local embedder"));
        // Disclosure must not change the advisory/healthy posture.
        assert_eq!(check.tier, CheckTier::Advisory);
        assert!(check.is_topline_healthy());
    }

    #[test]
    fn embedding_env_trap_note_pluralizes_and_is_empty_when_absent() {
        assert!(embedding_env_trap_note(&[], "neural_local").is_none());
        let one = embedding_env_trap_note(&["EMBEDDING_MODEL"], "hash")
            .expect("one present var yields a note");
        assert!(one.contains("consume it"));
        let two = embedding_env_trap_note(&["EMBEDDING_MODEL", "OPENAI_API_KEY"], "hash")
            .expect("two present vars yield a note");
        assert!(two.contains("consume them"));
        assert!(two.contains("OPENAI_API_KEY"));
    }

    #[test]
    fn embedding_posture_unavailable_is_advisory_ok() {
        let check = embedding_posture_unavailable_check(&[], "no workspace path");
        assert_eq!(check.severity, CheckSeverity::Ok);
        assert_eq!(check.tier, CheckTier::Advisory);
        assert!(check.is_topline_healthy());
        assert!(check.message.contains("could not be determined"));
    }
}
