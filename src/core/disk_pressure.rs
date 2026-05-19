//! Read-only disk-pressure diagnostics for crowded agent workspaces.
//!
//! This module intentionally plans recovery actions without performing them.
//! The command surface is meant to make disk pressure visible to agents while
//! preserving the repository rule that cleanup needs explicit human approval.

use std::cmp::Reverse;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde::ser::{SerializeStruct, Serializer};

use crate::core::degraded_aggregation::{DegradationAggregationInput, aggregate_degraded_entries};

pub const ARTIFACT_RETENTION_DIAGNOSTICS_SCHEMA_V1: &str = "ee.artifact_retention.diagnostics.v1";
pub const BUILD_ADMISSION_DIAGNOSTICS_SCHEMA_V1: &str = "ee.build_admission.diagnostics.v1";
pub const DISK_PRESSURE_DIAGNOSTICS_SCHEMA_V1: &str = "ee.disk_pressure.diagnostics.v1";
pub const EXTERNAL_BUILD_ROOT: &str = "/Volumes/USBNVME16TB/temp_agent_space";

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;
const WARNING_AVAILABLE_BYTES: u64 = 20 * GIB;
const DEGRADED_AVAILABLE_BYTES: u64 = 5 * GIB;
const BLOCKED_AVAILABLE_BYTES: u64 = GIB;
const WARNING_PERCENT_USED: f64 = 85.0;
const DEGRADED_PERCENT_USED: f64 = 95.0;
const BLOCKED_PERCENT_USED: f64 = 99.0;
const SECONDS_PER_DAY: u64 = 86_400;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiskPressureOptions {
    pub workspace: PathBuf,
    pub workspace_source: &'static str,
    pub top_limit: usize,
    pub consumer_depth: usize,
    pub consumer_entry_limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactRetentionOptions {
    pub workspace: PathBuf,
    pub workspace_source: &'static str,
    pub top_limit: usize,
    pub consumer_depth: usize,
    pub consumer_entry_limit: usize,
    pub now_unix_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildAdmissionOptions {
    pub workspace: PathBuf,
    pub workspace_source: &'static str,
    pub min_free_bytes: u64,
    pub artifact_destinations: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskPressurePosture {
    Ok,
    Warning,
    DegradedRecoverable,
    Blocked,
}

impl DiskPressurePosture {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::DegradedRecoverable => "degraded_recoverable",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskPressureReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub side_effect_free: bool,
    pub mutation_policy: &'static str,
    pub workspace: DiskPressureWorkspace,
    pub thresholds: DiskPressureThresholds,
    pub posture: DiskPressurePosture,
    pub roots: Vec<DiskPressureRoot>,
    pub top_consumers: Vec<DiskPressureTopConsumer>,
    pub recovery_actions: Vec<DiskPressureRecoveryAction>,
    pub guidance: Vec<DiskPressureGuidance>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskPressureWorkspace {
    pub path: String,
    pub source: &'static str,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskPressureThresholds {
    pub warning_percent_used: f64,
    pub degraded_percent_used: f64,
    pub blocked_percent_used: f64,
    pub warning_available_bytes: u64,
    pub degraded_available_bytes: u64,
    pub blocked_available_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskPressureRoot {
    pub label: &'static str,
    pub role: &'static str,
    pub path: String,
    pub exists: bool,
    pub nearest_existing_ancestor: Option<String>,
    pub capacity: Option<DiskPressureCapacity>,
    pub posture: DiskPressurePosture,
    pub top_consumers: Vec<DiskPressureTopConsumer>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskPressureCapacity {
    pub bytes_total: u64,
    pub bytes_available: u64,
    pub bytes_used: u64,
    pub percent_used: f64,
    pub source: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskPressureTopConsumer {
    pub root_label: &'static str,
    pub path: String,
    pub kind: &'static str,
    pub bytes: u64,
    pub measurement: &'static str,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskPressureRecoveryAction {
    pub priority: u8,
    pub kind: &'static str,
    pub target: &'static str,
    pub reason: String,
    pub suggestion: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskPressureGuidance {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: String,
}

#[derive(Clone, Debug)]
pub struct BuildAdmissionReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub side_effect_free: bool,
    pub mutation_policy: &'static str,
    pub workspace: DiskPressureWorkspace,
    pub external_build_root: String,
    pub min_free_bytes: u64,
    pub admitted: bool,
    pub checks: Vec<BuildAdmissionCheck>,
    pub degraded: Vec<BuildAdmissionDegradation>,
    pub recovery_actions: Vec<DiskPressureRecoveryAction>,
}

impl Serialize for BuildAdmissionReport {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let degraded = build_admission_degraded_data_json(&self.degraded);
        let mut state = serializer.serialize_struct("BuildAdmissionReport", 11)?;
        state.serialize_field("schema", &self.schema)?;
        state.serialize_field("command", &self.command)?;
        state.serialize_field("sideEffectFree", &self.side_effect_free)?;
        state.serialize_field("mutationPolicy", &self.mutation_policy)?;
        state.serialize_field("workspace", &self.workspace)?;
        state.serialize_field("externalBuildRoot", &self.external_build_root)?;
        state.serialize_field("minFreeBytes", &self.min_free_bytes)?;
        state.serialize_field("admitted", &self.admitted)?;
        state.serialize_field("checks", &self.checks)?;
        state.serialize_field("degraded", &degraded)?;
        state.serialize_field("recoveryActions", &self.recovery_actions)?;
        state.end()
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildAdmissionCheck {
    pub label: &'static str,
    pub role: &'static str,
    pub path: String,
    pub required: bool,
    pub exists: bool,
    pub nearest_existing_ancestor: Option<String>,
    pub bytes_available: Option<u64>,
    pub min_free_bytes: u64,
    pub admitted: bool,
    pub external_required: bool,
    pub external: bool,
    pub symlink_component: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildAdmissionDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: String,
}

fn build_admission_degraded_data_json(
    degraded: &[BuildAdmissionDegradation],
) -> Vec<serde_json::Value> {
    aggregate_degraded_entries(degraded.iter().map(|entry| {
        DegradationAggregationInput::new(
            "build_admission",
            entry.code,
            entry.severity,
            entry.message.clone(),
            entry.repair.clone(),
        )
    }))
    .into_iter()
    .map(|entry| {
        serde_json::json!({
            "code": entry.code,
            "severity": entry.severity,
            "message": entry.message,
            "repair": entry.repair,
            "sources": entry.sources,
        })
    })
    .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRetentionActionKind {
    Keep,
    MovePreserve,
    CompressPreserve,
    EligibleForHumanCleanup,
}

impl ArtifactRetentionActionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::MovePreserve => "move_preserve",
            Self::CompressPreserve => "compress_preserve",
            Self::EligibleForHumanCleanup => "eligible_for_human_cleanup",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRetentionReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub side_effect_free: bool,
    pub mutation_policy: &'static str,
    pub workspace: DiskPressureWorkspace,
    pub summary: ArtifactRetentionSummary,
    pub roots: Vec<ArtifactRetentionRoot>,
    pub actions: Vec<ArtifactRetentionAction>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRetentionSummary {
    pub root_count: usize,
    pub existing_roots: usize,
    pub total_bytes: u64,
    pub over_budget_roots: usize,
    pub expired_roots: usize,
    pub retained_for_closeout_roots: usize,
    pub j1_log_configured: bool,
    pub retention_manifest_configured: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRetentionRoot {
    pub label: &'static str,
    pub category: &'static str,
    pub path: String,
    pub path_source: &'static str,
    pub exists: bool,
    pub kind: &'static str,
    pub bytes: u64,
    pub artifact_count: u64,
    pub measurement: &'static str,
    pub truncated: bool,
    pub retention_reason: &'static str,
    pub default_ttl_days: Option<u64>,
    pub bead_closeout_required: bool,
    pub budget: ArtifactRetentionBudget,
    pub latest_modified_unix_seconds: Option<u64>,
    pub age_days: Option<u64>,
    pub contains_retention_manifest: bool,
    pub posture: &'static str,
    pub recommended_action: ArtifactRetentionActionKind,
    pub top_consumers: Vec<DiskPressureTopConsumer>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRetentionBudget {
    pub warning_bytes: u64,
    pub degraded_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRetentionAction {
    pub priority: u8,
    pub kind: ArtifactRetentionActionKind,
    pub target: &'static str,
    pub path: String,
    pub reason: String,
    pub suggestion: String,
    pub destructive: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RootSpec {
    label: &'static str,
    role: &'static str,
    required: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FsCapacity {
    total_bytes: u64,
    available_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ArtifactRetentionRootSpec {
    label: &'static str,
    category: &'static str,
    path: PathBuf,
    path_source: &'static str,
    retention_reason: &'static str,
    default_ttl_days: Option<u64>,
    bead_closeout_required: bool,
    warning_bytes: u64,
    degraded_bytes: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ArtifactRetentionScan {
    bytes: u64,
    artifact_count: u64,
    latest_modified_unix_seconds: Option<u64>,
    contains_retention_manifest: bool,
    truncated: bool,
}

#[must_use]
pub fn gather_disk_pressure_report(options: &DiskPressureOptions) -> DiskPressureReport {
    let workspace = normalize_path(&options.workspace);
    tracing::info!(
        event = "disk_pressure_scan_started",
        workspace = %path_to_string(&workspace),
        workspace_source = options.workspace_source,
        top_limit = options.top_limit,
        consumer_depth = options.consumer_depth,
        consumer_entry_limit = options.consumer_entry_limit,
        "disk pressure diagnostic scan started"
    );
    let specs = root_specs(&workspace);
    let external_root = Path::new(EXTERNAL_BUILD_ROOT);

    let mut roots = Vec::with_capacity(specs.len());
    let mut global_consumers = Vec::new();

    for (spec, path) in specs {
        let root = inspect_root(
            spec,
            &path,
            options.top_limit.max(1),
            options.consumer_depth,
            options.consumer_entry_limit.max(1),
        );
        global_consumers.extend(root.top_consumers.clone());
        roots.push(root);
    }

    global_consumers.sort_by_key(|consumer| Reverse(consumer.bytes));
    global_consumers.truncate(options.top_limit.max(1));
    for consumer in &global_consumers {
        tracing::info!(
            event = "disk_pressure_top_consumer",
            root_label = consumer.root_label,
            path = %consumer.path,
            kind = consumer.kind,
            bytes = consumer.bytes,
            measurement = consumer.measurement,
            truncated = consumer.truncated,
            "disk pressure top consumer measured"
        );
    }

    let posture = roots
        .iter()
        .map(|root| root.posture)
        .max()
        .unwrap_or(DiskPressurePosture::Ok);
    let guidance = build_guidance(&roots, external_root);
    let recovery_actions = build_recovery_actions(posture, &guidance);
    tracing::info!(
        event = "disk_pressure_repair_plan_emitted",
        posture = posture.as_str(),
        guidance_count = guidance.len(),
        recovery_action_count = recovery_actions.len(),
        side_effect_free = true,
        "disk pressure repair plan emitted"
    );

    DiskPressureReport {
        schema: DISK_PRESSURE_DIAGNOSTICS_SCHEMA_V1,
        command: "diag disk-pressure",
        side_effect_free: true,
        mutation_policy: "read_only_report_no_files_modified",
        workspace: DiskPressureWorkspace {
            path: path_to_string(&workspace),
            source: options.workspace_source,
        },
        thresholds: DiskPressureThresholds {
            warning_percent_used: WARNING_PERCENT_USED,
            degraded_percent_used: DEGRADED_PERCENT_USED,
            blocked_percent_used: BLOCKED_PERCENT_USED,
            warning_available_bytes: WARNING_AVAILABLE_BYTES,
            degraded_available_bytes: DEGRADED_AVAILABLE_BYTES,
            blocked_available_bytes: BLOCKED_AVAILABLE_BYTES,
        },
        posture,
        roots,
        top_consumers: global_consumers,
        recovery_actions,
        guidance,
    }
}

#[must_use]
pub fn gather_artifact_retention_report(
    options: &ArtifactRetentionOptions,
) -> ArtifactRetentionReport {
    let workspace = normalize_path(&options.workspace);
    let specs = artifact_retention_specs(&workspace);
    let j1_log_configured = env::var_os("EE_TEST_LOG_PATH").is_some();
    let retention_manifest_configured = env::var_os("EPIC_RETENTION_MANIFEST").is_some()
        || env::var_os("EE_E2E_RETENTION_MANIFEST").is_some();

    let mut roots = Vec::with_capacity(specs.len());
    let mut actions = Vec::with_capacity(specs.len());

    for spec in specs {
        let root = inspect_artifact_retention_root(
            &spec,
            options.top_limit.max(1),
            options.consumer_depth,
            options.consumer_entry_limit.max(1),
            options.now_unix_seconds,
        );
        actions.push(artifact_retention_action(actions.len(), &root));
        roots.push(root);
    }

    let summary = ArtifactRetentionSummary {
        root_count: roots.len(),
        existing_roots: roots.iter().filter(|root| root.exists).count(),
        total_bytes: roots.iter().map(|root| root.bytes).sum(),
        over_budget_roots: roots
            .iter()
            .filter(|root| {
                matches!(
                    root.recommended_action,
                    ArtifactRetentionActionKind::MovePreserve
                        | ArtifactRetentionActionKind::CompressPreserve
                )
            })
            .count(),
        expired_roots: roots
            .iter()
            .filter(|root| {
                root.recommended_action == ArtifactRetentionActionKind::EligibleForHumanCleanup
            })
            .count(),
        retained_for_closeout_roots: roots
            .iter()
            .filter(|root| root.bead_closeout_required && root.exists)
            .count(),
        j1_log_configured,
        retention_manifest_configured,
    };

    ArtifactRetentionReport {
        schema: ARTIFACT_RETENTION_DIAGNOSTICS_SCHEMA_V1,
        command: "diag artifacts",
        side_effect_free: true,
        mutation_policy: "read_only_report_no_files_modified_no_cleanup",
        workspace: DiskPressureWorkspace {
            path: path_to_string(&workspace),
            source: options.workspace_source,
        },
        summary,
        roots,
        actions,
    }
}

#[must_use]
pub fn gather_build_admission_report(options: &BuildAdmissionOptions) -> BuildAdmissionReport {
    let workspace = normalize_path(&options.workspace);
    let external_root = Path::new(EXTERNAL_BUILD_ROOT);
    let external_available = external_root.exists();
    let tmpdir = env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    let cargo_target = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target"));

    let mut check_specs = vec![
        (
            "workspace",
            "workspace_root",
            workspace.clone(),
            true,
            false,
        ),
        (
            "cargo_target",
            "cargo_build_target",
            cargo_target,
            true,
            true,
        ),
        ("tmpdir", "temporary_directory", tmpdir, true, true),
    ];
    for destination in &options.artifact_destinations {
        check_specs.push((
            "artifact_destination",
            "artifact_sync_down_destination",
            normalize_path(destination),
            false,
            true,
        ));
    }

    let mut checks = Vec::with_capacity(check_specs.len());
    let mut degraded = Vec::new();

    for (label, role, path, required, external_required) in check_specs {
        let exists = path.exists();
        let nearest = nearest_existing_path(&path);
        let capacity = nearest
            .as_ref()
            .and_then(|ancestor| statvfs_capacity(ancestor));
        let bytes_available = capacity.as_ref().map(|capacity| capacity.available_bytes);
        let has_required_space = bytes_available
            .map(|available| available >= options.min_free_bytes)
            .unwrap_or(false);
        let (symlink_component, symlink_inspection_failed) =
            match first_existing_symlink_component(&path) {
                Ok(component) => (component, false),
                Err(_) => (None, true),
            };
        let external_required_active = external_available && external_required;
        let untrusted_external_path =
            external_required_active && (symlink_component.is_some() || symlink_inspection_failed);
        let external = path.starts_with(external_root) && !untrusted_external_path;

        if required && !has_required_space {
            degraded.push(BuildAdmissionDegradation {
                code: "build_admission_denied",
                severity: "medium",
                message: format!(
                    "required build path `{label}` is below the free-space admission threshold."
                ),
                repair: "Move build outputs and temporary scratch to the external drive, reduce artifact sync-down, or ask the human before cleanup.".to_owned(),
            });
        }

        if required && untrusted_external_path {
            let reason = symlink_component.as_ref().map_or_else(
                || "could not be inspected for symlink components".to_owned(),
                |component| format!("traverses symlink component `{}`", component.display()),
            );
            degraded.push(BuildAdmissionDegradation {
                code: "build_admission_denied",
                severity: "medium",
                message: format!(
                    "required build path `{label}` {reason} before external-drive placement can be trusted."
                ),
                repair: "Use a real, inspectable directory under the external build root before starting heavy work.".to_owned(),
            });
        }

        if external_required_active && !external {
            let (code, repair) = if label == "artifact_destination" {
                (
                    "artifact_destination_not_external",
                    "Point artifact sync-down destinations at the external drive or pass an explicit external --artifact-destination.",
                )
            } else if label == "cargo_target" {
                (
                    "cargo_target_not_external",
                    "Set CARGO_TARGET_DIR under /Volumes/USBNVME16TB/temp_agent_space before starting heavy Cargo work.",
                )
            } else {
                (
                    "tmpdir_not_external",
                    "Set TMPDIR under /Volumes/USBNVME16TB/temp_agent_space before starting heavy Cargo work.",
                )
            };
            let message = if let Some(component) = symlink_component.as_ref() {
                format!(
                    "{label} path `{}` traverses symlink component `{}`; external build-root placement cannot be trusted.",
                    path.display(),
                    component.display()
                )
            } else if symlink_inspection_failed {
                format!(
                    "{label} path `{}` could not be inspected for symlink components; external build-root placement cannot be trusted.",
                    path.display()
                )
            } else {
                format!(
                    "{label} path `{}` is not under the external build root {}.",
                    path.display(),
                    EXTERNAL_BUILD_ROOT
                )
            };
            degraded.push(BuildAdmissionDegradation {
                code,
                severity: "warning",
                message,
                repair: repair.to_owned(),
            });
        }

        checks.push(BuildAdmissionCheck {
            label,
            role,
            path: path_to_string(&path),
            required,
            exists,
            nearest_existing_ancestor: nearest.as_ref().map(|path| path_to_string(path)),
            bytes_available,
            min_free_bytes: options.min_free_bytes,
            admitted: !required || (has_required_space && !untrusted_external_path),
            external_required: external_required_active,
            external,
            symlink_component: symlink_component
                .as_ref()
                .map(|component| path_to_string(component)),
        });
    }

    let admitted = degraded
        .iter()
        .all(|entry| entry.code != "build_admission_denied");
    let recovery_actions = build_admission_recovery_actions(&degraded, admitted);

    BuildAdmissionReport {
        schema: BUILD_ADMISSION_DIAGNOSTICS_SCHEMA_V1,
        command: "diag build-admission",
        side_effect_free: true,
        mutation_policy: "read_only_report_no_files_modified",
        workspace: DiskPressureWorkspace {
            path: path_to_string(&workspace),
            source: options.workspace_source,
        },
        external_build_root: EXTERNAL_BUILD_ROOT.to_owned(),
        min_free_bytes: options.min_free_bytes,
        admitted,
        checks,
        degraded,
        recovery_actions,
    }
}

fn build_admission_recovery_actions(
    degraded: &[BuildAdmissionDegradation],
    admitted: bool,
) -> Vec<DiskPressureRecoveryAction> {
    let mut recovery_actions = Vec::new();
    if degraded
        .iter()
        .any(|entry| entry.code == "cargo_target_not_external")
    {
        recovery_actions.push(DiskPressureRecoveryAction {
            priority: 1,
            kind: "move_preserve",
            target: "cargo_target",
            reason: "Cargo build output is not using the external build root.".to_owned(),
            suggestion: format!("Set CARGO_TARGET_DIR under {EXTERNAL_BUILD_ROOT}."),
        });
    }
    if degraded
        .iter()
        .any(|entry| entry.code == "tmpdir_not_external")
    {
        recovery_actions.push(DiskPressureRecoveryAction {
            priority: 2,
            kind: "move_preserve",
            target: "tmpdir",
            reason: "Temporary build scratch is not using the external build root.".to_owned(),
            suggestion: format!("Set TMPDIR under {EXTERNAL_BUILD_ROOT}/tmp."),
        });
    }
    if degraded
        .iter()
        .any(|entry| entry.code == "artifact_destination_not_external")
    {
        recovery_actions.push(DiskPressureRecoveryAction {
            priority: 3,
            kind: "move_preserve",
            target: "artifact_destination",
            reason: "Artifact sync-down destination is not on the external drive.".to_owned(),
            suggestion: "Use an artifact sync-down destination under the external build root."
                .to_owned(),
        });
    }
    if !admitted {
        recovery_actions.push(DiskPressureRecoveryAction {
            priority: 4,
            kind: "ask_human",
            target: "build_admission",
            reason: "One or more required build paths are below the admission threshold."
                .to_owned(),
            suggestion: "Ask the human before any cleanup; this diagnostic does not delete files."
                .to_owned(),
        });
    }

    recovery_actions
}

#[must_use]
pub fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn root_specs(workspace: &Path) -> Vec<(RootSpec, PathBuf)> {
    let home = env::var_os("HOME").map(PathBuf::from);
    let tmpdir = env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    let cargo_target = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target"));

    let mut specs = vec![
        (
            RootSpec {
                label: "workspace",
                role: "workspace_root",
                required: true,
            },
            workspace.to_path_buf(),
        ),
        (
            RootSpec {
                label: "workspace_state",
                role: "workspace_ee_dir",
                required: false,
            },
            workspace.join(".ee"),
        ),
        (
            RootSpec {
                label: "cargo_target",
                role: "cargo_build_target",
                required: true,
            },
            cargo_target,
        ),
        (
            RootSpec {
                label: "tmpdir",
                role: "temporary_directory",
                required: true,
            },
            tmpdir.clone(),
        ),
        (
            RootSpec {
                label: "e2e_audit_artifacts",
                role: "e2e_artifact_root",
                required: false,
            },
            workspace.join("tests/audit_artifacts"),
        ),
        (
            RootSpec {
                label: "e2e_tmp_swarm_scale",
                role: "e2e_artifact_root",
                required: false,
            },
            tmpdir.join("ee-swarm-scale-events"),
        ),
        (
            RootSpec {
                label: "e2e_tmp_swarm_fixture",
                role: "e2e_artifact_root",
                required: false,
            },
            tmpdir.join("ee-swarm-fixture-events"),
        ),
    ];

    if let Some(home) = home {
        specs.extend([
            (
                RootSpec {
                    label: "ee_data",
                    role: "ee_user_data_dir",
                    required: false,
                },
                home.join(".local/share/ee"),
            ),
            (
                RootSpec {
                    label: "agent_mail_archive",
                    role: "agent_mail_archive_root",
                    required: false,
                },
                home.join(".local/share/mcp_agent_mail"),
            ),
            (
                RootSpec {
                    label: "codex_home",
                    role: "codex_home",
                    required: false,
                },
                home.join(".codex"),
            ),
            (
                RootSpec {
                    label: "codex_sessions",
                    role: "codex_session_root",
                    required: false,
                },
                home.join(".codex/sessions"),
            ),
            (
                RootSpec {
                    label: "codex_logs",
                    role: "codex_log_root",
                    required: false,
                },
                home.join(".codex/log"),
            ),
        ]);
    }

    specs
}

fn artifact_retention_specs(workspace: &Path) -> Vec<ArtifactRetentionRootSpec> {
    let tmpdir = env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    let cargo_target = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target"));

    let mut specs = vec![
        ArtifactRetentionRootSpec {
            label: "tests_audit_artifacts",
            category: "verification_audit_artifacts",
            path: workspace.join("tests/audit_artifacts"),
            path_source: "workspace/tests/audit_artifacts",
            retention_reason: "install, release, and verification audit JSON evidence",
            default_ttl_days: None,
            bead_closeout_required: true,
            warning_bytes: 256 * MIB,
            degraded_bytes: GIB,
        },
        ArtifactRetentionRootSpec {
            label: "cargo_target_e2e",
            category: "e2e_retained_workspaces",
            path: cargo_target.join("ee-e2e"),
            path_source: "CARGO_TARGET_DIR/ee-e2e",
            retention_reason: "E2E command dossiers and retained workspaces",
            default_ttl_days: Some(14),
            bead_closeout_required: false,
            warning_bytes: 2 * GIB,
            degraded_bytes: 10 * GIB,
        },
        ArtifactRetentionRootSpec {
            label: "workspace_target_e2e",
            category: "e2e_retained_workspaces",
            path: workspace.join("target/ee-e2e"),
            path_source: "workspace/target/ee-e2e",
            retention_reason: "legacy or fallback E2E artifact root",
            default_ttl_days: Some(14),
            bead_closeout_required: false,
            warning_bytes: 2 * GIB,
            degraded_bytes: 10 * GIB,
        },
        ArtifactRetentionRootSpec {
            label: "golden_artifacts",
            category: "golden_artifacts",
            path: cargo_target.join("ee-golden-artifacts"),
            path_source: "CARGO_TARGET_DIR/ee-golden-artifacts",
            retention_reason: "generated golden comparison artifacts",
            default_ttl_days: Some(30),
            bead_closeout_required: true,
            warning_bytes: GIB,
            degraded_bytes: 5 * GIB,
        },
        ArtifactRetentionRootSpec {
            label: "bench_artifacts",
            category: "benchmark_artifacts",
            path: cargo_target.join("ee-bench"),
            path_source: "CARGO_TARGET_DIR/ee-bench",
            retention_reason: "benchmark and performance regression JSON artifacts",
            default_ttl_days: Some(30),
            bead_closeout_required: true,
            warning_bytes: GIB,
            degraded_bytes: 5 * GIB,
        },
        ArtifactRetentionRootSpec {
            label: "tmp_e2e_workspaces",
            category: "e2e_retained_workspaces",
            path: tmpdir.clone(),
            path_source: "TMPDIR",
            retention_reason: "temporary E2E workspaces and J1 stdout/stderr side artifacts",
            default_ttl_days: Some(7),
            bead_closeout_required: false,
            warning_bytes: 5 * GIB,
            degraded_bytes: 20 * GIB,
        },
        ArtifactRetentionRootSpec {
            label: "support_bundles",
            category: "support_bundles",
            path: workspace.join(".ee/support-bundles"),
            path_source: "workspace/.ee/support-bundles",
            retention_reason: "redacted diagnostic support bundles",
            default_ttl_days: Some(30),
            bead_closeout_required: true,
            warning_bytes: GIB,
            degraded_bytes: 5 * GIB,
        },
    ];

    if let Some(path) = env::var_os("EE_TEST_LOG_PATH").map(PathBuf::from) {
        specs.push(ArtifactRetentionRootSpec {
            label: "j1_current_log",
            category: "j1_jsonl_log",
            path,
            path_source: "EE_TEST_LOG_PATH",
            retention_reason: "current structured J1 JSONL event log",
            default_ttl_days: Some(30),
            bead_closeout_required: true,
            warning_bytes: 128 * MIB,
            degraded_bytes: 512 * MIB,
        });
    }

    if let Some(path) = env::var_os("EPIC_RETENTION_MANIFEST")
        .or_else(|| env::var_os("EE_E2E_RETENTION_MANIFEST"))
        .map(PathBuf::from)
    {
        specs.push(ArtifactRetentionRootSpec {
            label: "current_retention_manifest",
            category: "retention_manifest",
            path,
            path_source: "EPIC_RETENTION_MANIFEST|EE_E2E_RETENTION_MANIFEST",
            retention_reason: "current per-run retained-artifact manifest",
            default_ttl_days: Some(30),
            bead_closeout_required: true,
            warning_bytes: 16 * MIB,
            degraded_bytes: 128 * MIB,
        });
    }

    specs
}

fn inspect_artifact_retention_root(
    spec: &ArtifactRetentionRootSpec,
    top_limit: usize,
    consumer_depth: usize,
    consumer_entry_limit: usize,
    now_unix_seconds: u64,
) -> ArtifactRetentionRoot {
    let exists = spec.path.exists();
    let kind = file_kind(&spec.path);
    let scan = if exists {
        scan_artifact_retention_path(&spec.path, consumer_depth, consumer_entry_limit)
    } else {
        ArtifactRetentionScan::default()
    };
    let age_days = scan
        .latest_modified_unix_seconds
        .map(|modified| now_unix_seconds.saturating_sub(modified) / SECONDS_PER_DAY);
    let recommended_action = classify_artifact_retention_action(exists, &scan, spec, age_days);
    let posture = artifact_retention_posture(
        exists,
        scan.bytes,
        spec.warning_bytes,
        spec.degraded_bytes,
        spec.default_ttl_days,
        age_days,
        spec.bead_closeout_required,
    );

    ArtifactRetentionRoot {
        label: spec.label,
        category: spec.category,
        path: path_to_string(&spec.path),
        path_source: spec.path_source,
        exists,
        kind,
        bytes: scan.bytes,
        artifact_count: scan.artifact_count,
        measurement: "bounded_recursive_metadata",
        truncated: scan.truncated,
        retention_reason: spec.retention_reason,
        default_ttl_days: spec.default_ttl_days,
        bead_closeout_required: spec.bead_closeout_required,
        budget: ArtifactRetentionBudget {
            warning_bytes: spec.warning_bytes,
            degraded_bytes: spec.degraded_bytes,
        },
        latest_modified_unix_seconds: scan.latest_modified_unix_seconds,
        age_days,
        contains_retention_manifest: scan.contains_retention_manifest,
        posture,
        recommended_action,
        top_consumers: top_consumers(
            spec.label,
            &spec.path,
            top_limit,
            consumer_depth,
            consumer_entry_limit,
        ),
    }
}

fn classify_artifact_retention_action(
    exists: bool,
    scan: &ArtifactRetentionScan,
    spec: &ArtifactRetentionRootSpec,
    age_days: Option<u64>,
) -> ArtifactRetentionActionKind {
    if !exists || scan.bytes == 0 || scan.artifact_count == 0 || spec.bead_closeout_required {
        return ArtifactRetentionActionKind::Keep;
    }
    if spec
        .default_ttl_days
        .zip(age_days)
        .is_some_and(|(ttl, age)| age >= ttl)
    {
        return ArtifactRetentionActionKind::EligibleForHumanCleanup;
    }
    if scan.bytes >= spec.degraded_bytes {
        ArtifactRetentionActionKind::MovePreserve
    } else if scan.bytes >= spec.warning_bytes {
        ArtifactRetentionActionKind::CompressPreserve
    } else {
        ArtifactRetentionActionKind::Keep
    }
}

fn artifact_retention_posture(
    exists: bool,
    bytes: u64,
    warning_bytes: u64,
    degraded_bytes: u64,
    ttl_days: Option<u64>,
    age_days: Option<u64>,
    bead_closeout_required: bool,
) -> &'static str {
    if !exists {
        "missing"
    } else if bead_closeout_required {
        "retained_for_closeout"
    } else if ttl_days.zip(age_days).is_some_and(|(ttl, age)| age >= ttl) {
        "expired"
    } else if bytes >= degraded_bytes {
        "over_budget"
    } else if bytes >= warning_bytes {
        "watch"
    } else {
        "ok"
    }
}

fn artifact_retention_action(
    index: usize,
    root: &ArtifactRetentionRoot,
) -> ArtifactRetentionAction {
    let reason = match root.recommended_action {
        ArtifactRetentionActionKind::Keep if root.bead_closeout_required => {
            "Artifact root is retained as closeout evidence.".to_owned()
        }
        ArtifactRetentionActionKind::Keep if !root.exists => {
            "Artifact root is not present.".to_owned()
        }
        ArtifactRetentionActionKind::Keep => "Artifact root is within retention budget.".to_owned(),
        ArtifactRetentionActionKind::CompressPreserve => {
            "Artifact root is above the warning budget.".to_owned()
        }
        ArtifactRetentionActionKind::MovePreserve => {
            "Artifact root is above the degraded budget.".to_owned()
        }
        ArtifactRetentionActionKind::EligibleForHumanCleanup => {
            "Artifact root is past its retention TTL and is not required for bead closeout."
                .to_owned()
        }
    };
    let suggestion = match root.recommended_action {
        ArtifactRetentionActionKind::Keep => {
            "Keep artifacts in place; no cleanup action is recommended.".to_owned()
        }
        ArtifactRetentionActionKind::CompressPreserve => {
            "Compress this root into a manifest-tracked archive before considering cleanup."
                .to_owned()
        }
        ArtifactRetentionActionKind::MovePreserve => format!(
            "Use `ee artifact relocate --from {} --to {}/artifact-retention --manifest <path> --apply --json` to preserve-copy before any cleanup review.",
            root.path, EXTERNAL_BUILD_ROOT
        ),
        ArtifactRetentionActionKind::EligibleForHumanCleanup => {
            "Ask the human to approve any exact cleanup command; this diagnostic never deletes."
                .to_owned()
        }
    };

    ArtifactRetentionAction {
        priority: u8::try_from(index.saturating_add(1)).unwrap_or(u8::MAX),
        kind: root.recommended_action,
        target: root.label,
        path: root.path.clone(),
        reason,
        suggestion,
        destructive: false,
    }
}

fn inspect_root(
    spec: RootSpec,
    path: &Path,
    top_limit: usize,
    consumer_depth: usize,
    consumer_entry_limit: usize,
) -> DiskPressureRoot {
    let exists = path.exists();
    let capacity = nearest_existing_path(path).and_then(|ancestor| {
        statvfs_capacity(&ancestor).map(|capacity| {
            let bytes_used = capacity
                .total_bytes
                .saturating_sub(capacity.available_bytes);
            let percent_used = percent_used(capacity.total_bytes, bytes_used);
            (
                ancestor,
                DiskPressureCapacity {
                    bytes_total: capacity.total_bytes,
                    bytes_available: capacity.available_bytes,
                    bytes_used,
                    percent_used,
                    source: "statvfs",
                },
            )
        })
    });

    let (nearest_existing_ancestor, capacity) = match capacity {
        Some((ancestor, capacity)) => (Some(path_to_string(&ancestor)), Some(capacity)),
        None => (None, None),
    };

    let posture = capacity
        .as_ref()
        .map_or(DiskPressurePosture::Warning, |capacity| {
            classify_capacity(capacity.bytes_available, capacity.percent_used)
        });
    let mut notes = Vec::new();
    if !exists && spec.required {
        notes.push("required root does not exist; inspecting nearest existing ancestor".to_owned());
    } else if !exists {
        notes.push(
            "optional root was not present; capacity is inherited from nearest ancestor".to_owned(),
        );
    }
    if capacity.is_none() {
        notes.push("capacity unavailable from statvfs".to_owned());
    }

    DiskPressureRoot {
        label: spec.label,
        role: spec.role,
        path: path_to_string(path),
        exists,
        nearest_existing_ancestor,
        capacity,
        posture,
        top_consumers: top_consumers(
            spec.label,
            path,
            top_limit,
            consumer_depth,
            consumer_entry_limit,
        ),
        notes,
    }
}

fn classify_capacity(bytes_available: u64, percent_used: f64) -> DiskPressurePosture {
    if bytes_available < BLOCKED_AVAILABLE_BYTES || percent_used >= BLOCKED_PERCENT_USED {
        DiskPressurePosture::Blocked
    } else if bytes_available < DEGRADED_AVAILABLE_BYTES || percent_used >= DEGRADED_PERCENT_USED {
        DiskPressurePosture::DegradedRecoverable
    } else if bytes_available < WARNING_AVAILABLE_BYTES || percent_used >= WARNING_PERCENT_USED {
        DiskPressurePosture::Warning
    } else {
        DiskPressurePosture::Ok
    }
}

fn build_guidance(roots: &[DiskPressureRoot], external_root: &Path) -> Vec<DiskPressureGuidance> {
    let mut guidance = Vec::new();
    let external_available = external_root.exists();

    for root in roots {
        if root.posture >= DiskPressurePosture::DegradedRecoverable {
            guidance.push(DiskPressureGuidance {
                code: "disk_pressure_high",
                severity: if root.posture == DiskPressurePosture::Blocked {
                    "high"
                } else {
                    "medium"
                },
                message: format!(
                    "{} has disk posture {}.",
                    root.label,
                    root.posture.as_str()
                ),
                repair: "Use the recoveryActions plan; do not delete artifacts without explicit approval.".to_owned(),
            });
        }
    }

    let cargo_target = roots.iter().find(|root| root.label == "cargo_target");
    if let Some(root) = cargo_target {
        if external_available && !path_string_is_within(&root.path, external_root) {
            guidance.push(DiskPressureGuidance {
                code: "cargo_target_not_external",
                severity: "warning",
                message: format!(
                    "CARGO_TARGET_DIR is not under {}.",
                    EXTERNAL_BUILD_ROOT
                ),
                repair: format!(
                    "Open a shell with CARGO_TARGET_DIR={}/cargo-target or another external-drive target.",
                    EXTERNAL_BUILD_ROOT
                ),
            });
        }
    }

    let tmpdir = roots.iter().find(|root| root.label == "tmpdir");
    if let Some(root) = tmpdir {
        if external_available && !path_string_is_within(&root.path, external_root) {
            guidance.push(DiskPressureGuidance {
                code: "tmpdir_not_external",
                severity: "warning",
                message: format!("TMPDIR is not under {}.", EXTERNAL_BUILD_ROOT),
                repair: format!(
                    "Open a shell with TMPDIR={}/tmp or another external-drive temporary directory.",
                    EXTERNAL_BUILD_ROOT
                ),
            });
        }
    }

    guidance
}

fn build_recovery_actions(
    posture: DiskPressurePosture,
    guidance: &[DiskPressureGuidance],
) -> Vec<DiskPressureRecoveryAction> {
    let mut actions = Vec::new();

    if guidance
        .iter()
        .any(|entry| entry.code == "cargo_target_not_external")
    {
        actions.push(DiskPressureRecoveryAction {
            priority: 1,
            kind: "move_preserve",
            target: "cargo_target",
            reason: "Build artifacts are not using the external build drive.".to_owned(),
            suggestion: format!(
                "Preserve existing artifacts, then configure CARGO_TARGET_DIR under {} for future builds.",
                EXTERNAL_BUILD_ROOT
            ),
        });
    }

    if guidance
        .iter()
        .any(|entry| entry.code == "tmpdir_not_external")
    {
        actions.push(DiskPressureRecoveryAction {
            priority: 2,
            kind: "move_preserve",
            target: "tmpdir",
            reason: "Temporary build scratch is not using the external drive.".to_owned(),
            suggestion: format!(
                "Preserve current scratch if needed, then configure TMPDIR under {} for future builds.",
                EXTERNAL_BUILD_ROOT
            ),
        });
    }

    if posture >= DiskPressurePosture::Warning {
        actions.push(DiskPressureRecoveryAction {
            priority: 3,
            kind: "rotate_with_manifest",
            target: "e2e_artifacts",
            reason: "E2E and verification artifacts can grow under repeated agent runs.".to_owned(),
            suggestion: "Rotate old artifacts into a manifest-tracked archive before removing anything from active paths.".to_owned(),
        });
        actions.push(DiskPressureRecoveryAction {
            priority: 4,
            kind: "compress_preserve",
            target: "logs_and_archives",
            reason: "Logs, Agent Mail archives, and Codex session history are append-heavy.".to_owned(),
            suggestion: "Compress cold artifacts into a preservation archive; keep manifests and indexes readable.".to_owned(),
        });
    }

    if posture == DiskPressurePosture::Blocked {
        actions.push(DiskPressureRecoveryAction {
            priority: 5,
            kind: "ask_human",
            target: "disk_pressure",
            reason: "Disk posture is blocked and cleanup is irreversible without review.".to_owned(),
            suggestion: "Ask the human to approve a specific cleanup or relocation plan before any deletion.".to_owned(),
        });
    }

    if actions.is_empty() {
        actions.push(DiskPressureRecoveryAction {
            priority: 1,
            kind: "noop",
            target: "disk_pressure",
            reason: "No warning or degraded disk-pressure signal was detected.".to_owned(),
            suggestion:
                "Keep using external build and temp directories for cargo and verification runs."
                    .to_owned(),
        });
    }

    actions
}

fn top_consumers(
    root_label: &'static str,
    root: &Path,
    top_limit: usize,
    max_depth: usize,
    entry_limit: usize,
) -> Vec<DiskPressureTopConsumer> {
    if !path_is_directory_no_follow(root) {
        return Vec::new();
    }
    let mut entries = Vec::new();
    let Ok(children) = fs::read_dir(root) else {
        return Vec::new();
    };
    for child in children.take(entry_limit).flatten() {
        let path = child.path();
        let (bytes, truncated) = bounded_size(&path, max_depth, entry_limit);
        let kind = file_kind(&path);
        entries.push(DiskPressureTopConsumer {
            root_label,
            path: path_to_string(&path),
            kind,
            bytes,
            measurement: "bounded_recursive_metadata",
            truncated,
        });
    }
    entries.sort_by_key(|entry| Reverse(entry.bytes));
    entries.truncate(top_limit);
    entries
}

fn path_is_directory_no_follow(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_dir())
        .unwrap_or(false)
}

fn scan_artifact_retention_path(
    path: &Path,
    max_depth: usize,
    entry_limit: usize,
) -> ArtifactRetentionScan {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return ArtifactRetentionScan::default();
    };

    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());
    let mut scan = ArtifactRetentionScan {
        bytes: metadata.len(),
        artifact_count: 1,
        latest_modified_unix_seconds: modified,
        contains_retention_manifest: path
            .file_name()
            .is_some_and(|name| name == "e2e_retention_manifest.json"),
        truncated: false,
    };

    if !metadata.is_dir() || max_depth == 0 {
        return scan;
    }

    let Ok(children) = fs::read_dir(path) else {
        return scan;
    };
    for (index, child) in children.flatten().enumerate() {
        if index >= entry_limit {
            scan.truncated = true;
            break;
        }
        let child_scan =
            scan_artifact_retention_path(&child.path(), max_depth.saturating_sub(1), entry_limit);
        scan.bytes = scan.bytes.saturating_add(child_scan.bytes);
        scan.artifact_count = scan
            .artifact_count
            .saturating_add(child_scan.artifact_count);
        scan.latest_modified_unix_seconds = match (
            scan.latest_modified_unix_seconds,
            child_scan.latest_modified_unix_seconds,
        ) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (None, Some(right)) => Some(right),
            (Some(left), None) => Some(left),
            (None, None) => None,
        };
        scan.contains_retention_manifest |= child_scan.contains_retention_manifest;
        scan.truncated |= child_scan.truncated;
    }

    scan
}

fn bounded_size(path: &Path, max_depth: usize, entry_limit: usize) -> (u64, bool) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return (0, false);
    };
    if !metadata.is_dir() || max_depth == 0 {
        return (metadata.len(), false);
    }

    let mut bytes = metadata.len();
    let mut truncated = false;
    let Ok(children) = fs::read_dir(path) else {
        return (bytes, false);
    };
    for (index, child) in children.flatten().enumerate() {
        if index >= entry_limit {
            truncated = true;
            break;
        }
        let (child_bytes, child_truncated) =
            bounded_size(&child.path(), max_depth.saturating_sub(1), entry_limit);
        bytes = bytes.saturating_add(child_bytes);
        truncated |= child_truncated;
    }
    (bytes, truncated)
}

fn file_kind(path: &Path) -> &'static str {
    match fs::symlink_metadata(path).map(|metadata| metadata.file_type()) {
        Ok(file_type) if file_type.is_dir() => "directory",
        Ok(file_type) if file_type.is_file() => "file",
        Ok(file_type) if file_type.is_symlink() => "symlink",
        Ok(_) => "other",
        Err(_) => "unknown",
    }
}

fn nearest_existing_path(path: &Path) -> Option<PathBuf> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if candidate.exists() {
            return Some(candidate.to_path_buf());
        }
        current = candidate.parent();
    }
    None
}

#[cfg(unix)]
fn statvfs_capacity(path: &Path) -> Option<FsCapacity> {
    let stat = rustix::fs::statvfs(path).ok()?;
    let block_size = if stat.f_frsize == 0 {
        stat.f_bsize
    } else {
        stat.f_frsize
    };
    Some(FsCapacity {
        total_bytes: stat.f_blocks.saturating_mul(block_size),
        available_bytes: stat.f_bavail.saturating_mul(block_size),
    })
}

#[cfg(not(unix))]
fn statvfs_capacity(path: &Path) -> Option<FsCapacity> {
    let _ = fs::metadata(path).ok()?;
    // Rust std has no portable filesystem-capacity API; callers already
    // surface capacity absence through the existing degraded path.
    None
}

fn percent_used(total: u64, used: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        let percent = (used as f64 / total as f64) * 100.0;
        (percent * 100.0).round() / 100.0
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn path_string_is_within(path: &str, root: &Path) -> bool {
    trusted_external_path(Path::new(path), root)
}

fn trusted_external_path(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
        && first_existing_symlink_component(path)
            .map(|component| component.is_none())
            .unwrap_or(false)
}

fn first_existing_symlink_component(path: &Path) -> io::Result<Option<PathBuf>> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(Some(current)),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

// -----------------------------------------------------------------------------
// bd-1zb7k.11.7 — Agent-harness runaway-log classifier (preservation-oriented)
//
// `ee diag disk-pressure --json` already detects `codex_log_root` as a path,
// but does not classify individual oversized harness log files separately,
// emit per-file repair plans, or distinguish actively-open logs (where naive
// truncation would corrupt the writer's append offset) from closed oversized
// logs (where rotation is safe with manifest evidence).
//
// This sub-module adds a *pure* per-entry classifier with no I/O and no
// mutation. The walker that gathers `AgentHarnessLogEntry` inputs from
// `~/.codex/log/*` and feeds them into the classifier is a follow-up slice
// of bd-1zb7k.11.7 (the bead remains open until that lands).
//
// The classifier never returns a repair kind that deletes or truncates files
// without explicit human approval: every recommended action is either
// `preserve_tail_copy`, `rotate_with_manifest`, `move_preserve`, `ask_human`,
// or `noop`, matching the AGENTS.md RULE 1 / RULE 2 / "no-file-deletion"
// policy.
// -----------------------------------------------------------------------------

/// Stable schema tag for the per-entry classification result. Mirrors the
/// `ee.disk_pressure.*` family used elsewhere in this module.
pub const AGENT_HARNESS_LOG_CLASSIFIER_SCHEMA_V1: &str =
    "ee.disk_pressure.agent_harness_log_classifier.v1";

/// Activity state observed for a harness log file. Detected before
/// classification by the gatherer (lsof / /proc inspection); the classifier
/// itself never opens the file.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentHarnessLogActivity {
    /// One or more open file descriptors point at this log.
    ActiveOpen,
    /// No open handles are observable.
    Closed,
    /// The gatherer could not probe open-handle status (e.g., `lsof` is
    /// unavailable on this host).
    OpenHandleProbeUnavailable,
}

impl AgentHarnessLogActivity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ActiveOpen => "active_open",
            Self::Closed => "closed",
            Self::OpenHandleProbeUnavailable => "open_handle_probe_unavailable",
        }
    }
}

/// Repair action kinds the classifier may recommend. Each kind preserves the
/// AGENTS.md no-deletion policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentHarnessLogRepairKind {
    /// Copy the trailing `tail_byte_target` bytes of an active log to a
    /// dated preservation path so an operator can approve subsequent
    /// rotation without losing recent context.
    PreserveTailCopy,
    /// Move the closed oversized log into a manifest-tracked archive
    /// directory before any deletion is considered.
    RotateWithManifest,
    /// Move the log onto a different filesystem (e.g., external NVMe)
    /// while preserving the original path via a symlink.
    MovePreserve,
    /// Stop and ask a human to approve a specific cleanup or relocation
    /// plan. Always emitted when an active log is on the workspace
    /// filesystem under blocked posture.
    AskHuman,
    /// No action recommended.
    Noop,
}

impl AgentHarnessLogRepairKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreserveTailCopy => "preserve_tail_copy",
            Self::RotateWithManifest => "rotate_with_manifest",
            Self::MovePreserve => "move_preserve",
            Self::AskHuman => "ask_human",
            Self::Noop => "noop",
        }
    }
}

/// One agent-harness log file as observed by the gatherer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHarnessLogEntry {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub filesystem_label: String,
    pub on_workspace_filesystem: bool,
    pub mtime_unix_seconds: Option<u64>,
    pub activity: AgentHarnessLogActivity,
    pub owning_process_summary: Option<String>,
    pub tail_byte_target: u64,
}

/// Classification result, ready to drop into the existing
/// `DiskPressureRecoveryAction` lane or rendered standalone.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHarnessLogClassification {
    pub schema: &'static str,
    pub entry: AgentHarnessLogEntry,
    pub role: &'static str,
    pub repair_kind: AgentHarnessLogRepairKind,
    pub reason: String,
    pub suggestion: String,
    pub mutation_policy: &'static str,
    pub side_effect_free: bool,
}

/// Threshold above which a harness log is considered oversized for the
/// purpose of P7 classification. Matches the existing disk-pressure
/// `WARNING_AVAILABLE_BYTES` family (1 GiB).
pub const AGENT_HARNESS_LOG_WARNING_BYTES: u64 = GIB;

/// Threshold above which a harness log triggers a stronger action even
/// when activity is unknown.
pub const AGENT_HARNESS_LOG_DEGRADED_BYTES: u64 = 10 * GIB;

/// Default tail size to preserve before any rotation is recommended.
/// 16 MiB is roughly the last few minutes of any reasonable agent log.
pub const AGENT_HARNESS_LOG_DEFAULT_TAIL_BYTES: u64 = 16 * MIB;

/// Pure classifier: given an [`AgentHarnessLogEntry`], decide on the
/// preservation-oriented repair action. The function performs no I/O,
/// makes no syscalls, and never returns a delete/truncate action.
#[must_use]
pub fn classify_agent_harness_log(entry: &AgentHarnessLogEntry) -> AgentHarnessLogClassification {
    let (repair_kind, reason, suggestion) = recommend_repair(entry);
    AgentHarnessLogClassification {
        schema: AGENT_HARNESS_LOG_CLASSIFIER_SCHEMA_V1,
        entry: entry.clone(),
        role: "agent_harness_log",
        repair_kind,
        reason,
        suggestion,
        mutation_policy: "preservation_only",
        side_effect_free: true,
    }
}

fn recommend_repair(entry: &AgentHarnessLogEntry) -> (AgentHarnessLogRepairKind, String, String) {
    let oversized_warning = entry.size_bytes >= AGENT_HARNESS_LOG_WARNING_BYTES;
    let oversized_degraded = entry.size_bytes >= AGENT_HARNESS_LOG_DEGRADED_BYTES;

    if !oversized_warning {
        return (
            AgentHarnessLogRepairKind::Noop,
            format!(
                "Harness log {} is {} bytes, under the {} byte warning threshold.",
                entry.path.display(),
                entry.size_bytes,
                AGENT_HARNESS_LOG_WARNING_BYTES
            ),
            "No action recommended; keep using the external build/scratch drive.".to_owned(),
        );
    }

    match entry.activity {
        AgentHarnessLogActivity::ActiveOpen => {
            if oversized_degraded && entry.on_workspace_filesystem {
                (
                    AgentHarnessLogRepairKind::AskHuman,
                    format!(
                        "Active harness log {} is {} bytes on the workspace filesystem; \
truncation would corrupt the running agent and deletion is not allowed.",
                        entry.path.display(),
                        entry.size_bytes
                    ),
                    format!(
                        "Stop and request explicit human approval before any cleanup. \
Pre-stage a {}-byte tail copy under a dated preservation path first.",
                        entry.tail_byte_target
                    ),
                )
            } else {
                (
                    AgentHarnessLogRepairKind::PreserveTailCopy,
                    format!(
                        "Active harness log {} is {} bytes; the writer's append offset \
must be preserved.",
                        entry.path.display(),
                        entry.size_bytes
                    ),
                    format!(
                        "Copy the trailing {} bytes to a dated preservation path. \
Do not truncate or rename the active file without explicit human approval.",
                        entry.tail_byte_target
                    ),
                )
            }
        }
        AgentHarnessLogActivity::Closed => {
            if entry.on_workspace_filesystem && oversized_degraded {
                (
                    AgentHarnessLogRepairKind::MovePreserve,
                    format!(
                        "Closed harness log {} is {} bytes on the workspace filesystem.",
                        entry.path.display(),
                        entry.size_bytes
                    ),
                    "Move the log onto external scratch and symlink the original path; \
no deletion until manifest is recorded."
                        .to_owned(),
                )
            } else {
                (
                    AgentHarnessLogRepairKind::RotateWithManifest,
                    format!(
                        "Closed harness log {} is {} bytes; rotation is safe.",
                        entry.path.display(),
                        entry.size_bytes
                    ),
                    "Rotate into a manifest-tracked archive before any removal is considered."
                        .to_owned(),
                )
            }
        }
        AgentHarnessLogActivity::OpenHandleProbeUnavailable => (
            AgentHarnessLogRepairKind::AskHuman,
            format!(
                "Harness log {} is {} bytes but open-handle probe is unavailable; \
cannot safely decide between active-log preservation and closed-log rotation.",
                entry.path.display(),
                entry.size_bytes
            ),
            format!(
                "Install or grant `lsof` access, then re-run `ee diag disk-pressure --json`. \
Until then, only preserve a {}-byte tail copy; never truncate.",
                entry.tail_byte_target
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), String>;

    fn ensure<T>(actual: T, expected: T, label: &str) -> TestResult
    where
        T: Eq + std::fmt::Debug,
    {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{label}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn capacity_classification_thresholds_are_ordered() -> TestResult {
        ensure(
            classify_capacity(64 * GIB, 40.0),
            DiskPressurePosture::Ok,
            "ok",
        )?;
        ensure(
            classify_capacity(19 * GIB, 40.0),
            DiskPressurePosture::Warning,
            "available warning",
        )?;
        ensure(
            classify_capacity(4 * GIB, 40.0),
            DiskPressurePosture::DegradedRecoverable,
            "available degraded",
        )?;
        ensure(
            classify_capacity(GIB - 1, 40.0),
            DiskPressurePosture::Blocked,
            "available blocked",
        )?;
        ensure(
            classify_capacity(64 * GIB, 95.0),
            DiskPressurePosture::DegradedRecoverable,
            "percent degraded",
        )?;
        ensure(
            classify_capacity(64 * GIB, 99.0),
            DiskPressurePosture::Blocked,
            "percent blocked",
        )
    }

    #[test]
    fn recovery_action_kinds_are_non_destructive() -> TestResult {
        let guidance = vec![
            DiskPressureGuidance {
                code: "cargo_target_not_external",
                severity: "warning",
                message: "cargo".to_owned(),
                repair: "repair".to_owned(),
            },
            DiskPressureGuidance {
                code: "tmpdir_not_external",
                severity: "warning",
                message: "tmp".to_owned(),
                repair: "repair".to_owned(),
            },
        ];
        let actions = build_recovery_actions(DiskPressurePosture::Blocked, &guidance);
        let allowed = [
            "move_preserve",
            "compress_preserve",
            "rotate_with_manifest",
            "ask_human",
            "noop",
        ];
        if actions.iter().all(|action| allowed.contains(&action.kind)) {
            Ok(())
        } else {
            Err(format!("unexpected action kinds: {actions:?}"))
        }
    }

    #[test]
    fn build_admission_recovery_actions_cover_path_gaps_and_denial() -> TestResult {
        let degraded = vec![
            BuildAdmissionDegradation {
                code: "cargo_target_not_external",
                severity: "warning",
                message: "cargo target outside external root".to_owned(),
                repair: "repair".to_owned(),
            },
            BuildAdmissionDegradation {
                code: "tmpdir_not_external",
                severity: "warning",
                message: "tmpdir outside external root".to_owned(),
                repair: "repair".to_owned(),
            },
            BuildAdmissionDegradation {
                code: "artifact_destination_not_external",
                severity: "warning",
                message: "artifact destination outside external root".to_owned(),
                repair: "repair".to_owned(),
            },
            BuildAdmissionDegradation {
                code: "build_admission_denied",
                severity: "medium",
                message: "required path below threshold".to_owned(),
                repair: "repair".to_owned(),
            },
        ];

        let actions = build_admission_recovery_actions(&degraded, false);
        let targets: Vec<_> = actions.iter().map(|action| action.target).collect();
        let kinds: Vec<_> = actions.iter().map(|action| action.kind).collect();

        ensure(
            targets,
            vec![
                "cargo_target",
                "tmpdir",
                "artifact_destination",
                "build_admission",
            ],
            "action targets",
        )?;
        ensure(
            kinds,
            vec![
                "move_preserve",
                "move_preserve",
                "move_preserve",
                "ask_human",
            ],
            "action kinds",
        )
    }

    #[test]
    fn build_admission_report_serializes_aggregated_degraded_entries() -> TestResult {
        let report = BuildAdmissionReport {
            schema: BUILD_ADMISSION_DIAGNOSTICS_SCHEMA_V1,
            command: "diag build-admission",
            side_effect_free: true,
            mutation_policy: "read_only_report_no_files_modified",
            workspace: DiskPressureWorkspace {
                path: ".".to_owned(),
                source: "test",
            },
            external_build_root: EXTERNAL_BUILD_ROOT.to_owned(),
            min_free_bytes: 1024,
            admitted: false,
            checks: Vec::new(),
            degraded: vec![
                BuildAdmissionDegradation {
                    code: "build_admission_denied",
                    severity: "warning",
                    message: "required path is close to the threshold".to_owned(),
                    repair: "move build output to the external root".to_owned(),
                },
                BuildAdmissionDegradation {
                    code: "build_admission_denied",
                    severity: "medium",
                    message: "required path is below the admission threshold".to_owned(),
                    repair: "free space before running a build".to_owned(),
                },
            ],
            recovery_actions: Vec::new(),
        };

        let value = serde_json::to_value(&report).map_err(|error| error.to_string())?;
        let degraded = value
            .get("degraded")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| format!("missing degraded array in {value}"))?;

        ensure(degraded.len(), 1usize, "aggregated degraded count")?;
        ensure(
            degraded[0].get("code"),
            Some(&serde_json::json!("build_admission_denied")),
            "aggregated code",
        )?;
        ensure(
            degraded[0].get("severity"),
            Some(&serde_json::json!("medium")),
            "severity escalated",
        )?;
        ensure(
            degraded[0].get("repair"),
            Some(&serde_json::json!("free space before running a build")),
            "highest severity repair selected",
        )?;
        ensure(
            degraded[0].get("sources"),
            Some(&serde_json::json!(["build_admission"])),
            "source label",
        )
    }

    #[test]
    fn build_admission_recovery_actions_omit_denial_when_admitted() -> TestResult {
        let degraded = vec![BuildAdmissionDegradation {
            code: "cargo_target_not_external",
            severity: "warning",
            message: "cargo target outside external root".to_owned(),
            repair: "repair".to_owned(),
        }];

        let actions = build_admission_recovery_actions(&degraded, true);
        ensure(actions.len(), 1usize, "action count")?;
        ensure(actions[0].target, "cargo_target", "cargo target action")?;
        ensure(actions[0].kind, "move_preserve", "cargo target kind")
    }

    #[test]
    fn external_path_classification_uses_external_build_root_prefix() -> TestResult {
        let external_root = Path::new(EXTERNAL_BUILD_ROOT);
        ensure(
            path_string_is_within(
                "/Volumes/USBNVME16TB/temp_agent_space/cargo-target/debug",
                external_root,
            ),
            true,
            "external target path",
        )?;
        ensure(
            path_string_is_within("/tmp/ee-local-target", external_root),
            false,
            "local target path",
        )?;
        ensure(
            path_string_is_within("./target/debug", external_root),
            false,
            "relative target path",
        )
    }

    #[cfg(unix)]
    #[test]
    fn external_path_classification_rejects_symlink_components() -> TestResult {
        let temp_dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let external_root = temp_dir.path().join("external");
        let outside_root = temp_dir.path().join("outside");
        std::fs::create_dir_all(&external_root).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&outside_root).map_err(|error| error.to_string())?;
        let linked_target = external_root.join("cargo-target");
        std::os::unix::fs::symlink(&outside_root, &linked_target)
            .map_err(|error| error.to_string())?;

        ensure(
            trusted_external_path(&external_root.join("real-target"), &external_root),
            true,
            "ordinary external child path",
        )?;
        ensure(
            trusted_external_path(&linked_target, &external_root),
            false,
            "symlinked external child path",
        )
    }

    #[cfg(unix)]
    #[test]
    fn top_consumers_skips_symlinked_scan_root_before_read_dir() -> TestResult {
        let temp_dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside_root = temp_dir.path().join("outside-root");
        let linked_root = temp_dir.path().join("linked-root");
        std::fs::create_dir_all(&outside_root).map_err(|error| error.to_string())?;
        std::fs::write(outside_root.join("outside.bin"), vec![7_u8; 128])
            .map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&outside_root, &linked_root)
            .map_err(|error| error.to_string())?;

        let consumers = top_consumers("fixture", &linked_root, 5, 2, 10);

        ensure(
            consumers.is_empty(),
            true,
            "symlinked scan root must not expose target consumers",
        )
    }

    #[test]
    fn ok_posture_emits_noop_action() -> TestResult {
        let actions = build_recovery_actions(DiskPressurePosture::Ok, &[]);
        ensure(actions.len(), 1usize, "action count")?;
        ensure(actions[0].kind, "noop", "noop kind")
    }

    #[test]
    fn artifact_retention_actions_are_preservation_only() -> TestResult {
        let normal_spec = ArtifactRetentionRootSpec {
            label: "fixture",
            category: "fixture",
            path: PathBuf::from("/tmp/ee-artifact-retention-fixture"),
            path_source: "fixture",
            retention_reason: "fixture",
            default_ttl_days: None,
            bead_closeout_required: false,
            warning_bytes: 10,
            degraded_bytes: 20,
        };
        let ttl_spec = ArtifactRetentionRootSpec {
            default_ttl_days: Some(7),
            ..normal_spec.clone()
        };
        let closeout_spec = ArtifactRetentionRootSpec {
            bead_closeout_required: true,
            ..normal_spec.clone()
        };
        let empty_scan = ArtifactRetentionScan::default();
        let scan8 = ArtifactRetentionScan {
            bytes: 8,
            artifact_count: 1,
            ..ArtifactRetentionScan::default()
        };
        let scan12 = ArtifactRetentionScan {
            bytes: 12,
            artifact_count: 1,
            ..ArtifactRetentionScan::default()
        };
        let scan24 = ArtifactRetentionScan {
            bytes: 24,
            artifact_count: 1,
            ..ArtifactRetentionScan::default()
        };
        let actions = [
            classify_artifact_retention_action(false, &empty_scan, &normal_spec, None),
            classify_artifact_retention_action(true, &scan8, &normal_spec, None),
            classify_artifact_retention_action(true, &scan12, &normal_spec, None),
            classify_artifact_retention_action(true, &scan24, &normal_spec, None),
            classify_artifact_retention_action(true, &scan8, &ttl_spec, Some(8)),
            classify_artifact_retention_action(true, &scan24, &closeout_spec, Some(8)),
        ];
        let expected = [
            ArtifactRetentionActionKind::Keep,
            ArtifactRetentionActionKind::Keep,
            ArtifactRetentionActionKind::CompressPreserve,
            ArtifactRetentionActionKind::MovePreserve,
            ArtifactRetentionActionKind::EligibleForHumanCleanup,
            ArtifactRetentionActionKind::Keep,
        ];
        ensure(actions, expected, "artifact retention actions")
    }

    #[test]
    fn artifact_retention_postures_explain_budget_and_ttl() -> TestResult {
        ensure(
            artifact_retention_posture(false, 0, 10, 20, None, None, false),
            "missing",
            "missing",
        )?;
        ensure(
            artifact_retention_posture(true, 1, 10, 20, None, None, true),
            "retained_for_closeout",
            "closeout",
        )?;
        ensure(
            artifact_retention_posture(true, 1, 10, 20, Some(7), Some(8), false),
            "expired",
            "expired",
        )?;
        ensure(
            artifact_retention_posture(true, 24, 10, 20, None, None, false),
            "over_budget",
            "over budget",
        )?;
        ensure(
            artifact_retention_posture(true, 12, 10, 20, None, None, false),
            "watch",
            "watch",
        )
    }

    #[test]
    fn percent_used_rounds_to_two_decimals() -> TestResult {
        let value = percent_used(3, 2);
        if (value - 66.67).abs() < 0.001 {
            Ok(())
        } else {
            Err(format!("expected 66.67, got {value}"))
        }
    }

    // -------------------------------------------------------------------------
    // bd-1zb7k.11.7 — AgentHarnessLog classifier tests (5 cases per spec)
    // -------------------------------------------------------------------------

    fn harness_log_entry(
        size_bytes: u64,
        on_workspace_filesystem: bool,
        activity: AgentHarnessLogActivity,
    ) -> AgentHarnessLogEntry {
        AgentHarnessLogEntry {
            path: PathBuf::from("/home/agent/.codex/log/2026-05-19T04-00.log"),
            size_bytes,
            filesystem_label: "apfs".to_owned(),
            on_workspace_filesystem,
            mtime_unix_seconds: Some(1_779_167_000),
            activity,
            owning_process_summary: Some("codex pid=42 user=jemanuel".to_owned()),
            tail_byte_target: AGENT_HARNESS_LOG_DEFAULT_TAIL_BYTES,
        }
    }

    #[test]
    fn classify_active_open_oversized_log_recommends_preserve_tail_copy() {
        let entry = harness_log_entry(2 * GIB, false, AgentHarnessLogActivity::ActiveOpen);
        let classification = classify_agent_harness_log(&entry);
        assert_eq!(
            classification.repair_kind,
            AgentHarnessLogRepairKind::PreserveTailCopy
        );
        assert_eq!(classification.role, "agent_harness_log");
        assert_eq!(classification.mutation_policy, "preservation_only");
        assert!(classification.side_effect_free);
        assert_eq!(
            classification.schema,
            AGENT_HARNESS_LOG_CLASSIFIER_SCHEMA_V1
        );
        assert!(
            classification.suggestion.contains("16777216")
                || classification
                    .suggestion
                    .contains(&AGENT_HARNESS_LOG_DEFAULT_TAIL_BYTES.to_string()),
            "suggestion should mention the tail byte target: {}",
            classification.suggestion
        );
    }

    #[test]
    fn classify_active_open_blocked_workspace_log_escalates_to_ask_human() {
        let entry = harness_log_entry(20 * GIB, true, AgentHarnessLogActivity::ActiveOpen);
        let classification = classify_agent_harness_log(&entry);
        assert_eq!(
            classification.repair_kind,
            AgentHarnessLogRepairKind::AskHuman
        );
        assert!(
            classification.reason.contains("workspace filesystem"),
            "reason should explain workspace-filesystem escalation: {}",
            classification.reason
        );
    }

    #[test]
    fn classify_closed_oversized_log_recommends_rotate_with_manifest() {
        let entry = harness_log_entry(2 * GIB, false, AgentHarnessLogActivity::Closed);
        let classification = classify_agent_harness_log(&entry);
        assert_eq!(
            classification.repair_kind,
            AgentHarnessLogRepairKind::RotateWithManifest
        );
    }

    #[test]
    fn classify_closed_log_on_workspace_filesystem_recommends_move_preserve() {
        let entry = harness_log_entry(20 * GIB, true, AgentHarnessLogActivity::Closed);
        let classification = classify_agent_harness_log(&entry);
        assert_eq!(
            classification.repair_kind,
            AgentHarnessLogRepairKind::MovePreserve
        );
    }

    #[test]
    fn classify_open_handle_probe_unavailable_escalates_to_ask_human() {
        let entry = harness_log_entry(
            5 * GIB,
            false,
            AgentHarnessLogActivity::OpenHandleProbeUnavailable,
        );
        let classification = classify_agent_harness_log(&entry);
        assert_eq!(
            classification.repair_kind,
            AgentHarnessLogRepairKind::AskHuman
        );
        assert!(
            classification.suggestion.contains("lsof"),
            "suggestion should mention lsof availability: {}",
            classification.suggestion
        );
    }

    #[test]
    fn classify_log_under_warning_threshold_is_noop() {
        // Tiny log (10 MiB) should produce noop even with ActiveOpen activity.
        let entry = harness_log_entry(10 * MIB, false, AgentHarnessLogActivity::ActiveOpen);
        let classification = classify_agent_harness_log(&entry);
        assert_eq!(classification.repair_kind, AgentHarnessLogRepairKind::Noop);
    }

    #[test]
    fn agent_harness_log_repair_kind_strings_are_snake_case_and_stable() {
        // Pin the strings the schema and human renderer rely on.
        assert_eq!(
            AgentHarnessLogRepairKind::PreserveTailCopy.as_str(),
            "preserve_tail_copy"
        );
        assert_eq!(
            AgentHarnessLogRepairKind::RotateWithManifest.as_str(),
            "rotate_with_manifest"
        );
        assert_eq!(
            AgentHarnessLogRepairKind::MovePreserve.as_str(),
            "move_preserve"
        );
        assert_eq!(AgentHarnessLogRepairKind::AskHuman.as_str(), "ask_human");
        assert_eq!(AgentHarnessLogRepairKind::Noop.as_str(), "noop");
    }

    #[test]
    fn agent_harness_log_activity_strings_are_snake_case_and_stable() {
        assert_eq!(AgentHarnessLogActivity::ActiveOpen.as_str(), "active_open");
        assert_eq!(AgentHarnessLogActivity::Closed.as_str(), "closed");
        assert_eq!(
            AgentHarnessLogActivity::OpenHandleProbeUnavailable.as_str(),
            "open_handle_probe_unavailable"
        );
    }

    #[test]
    fn classification_serializes_to_camel_case_with_no_secrets() {
        let entry = harness_log_entry(2 * GIB, false, AgentHarnessLogActivity::ActiveOpen);
        let classification = classify_agent_harness_log(&entry);
        let json = serde_json::to_value(&classification).expect("serializes");
        let object = json.as_object().expect("object");
        for required in [
            "schema",
            "entry",
            "role",
            "repairKind",
            "reason",
            "suggestion",
            "mutationPolicy",
            "sideEffectFree",
        ] {
            assert!(
                object.contains_key(required),
                "missing camelCase field `{required}` in {json}"
            );
        }
        // Defense-in-depth: no log content (only the trailing tail target
        // value) is included in the classification; assert the suggestion
        // text never echoes the owning process's full command-line.
        let suggestion = object
            .get("suggestion")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            !suggestion.contains("pid="),
            "classification suggestion must not leak owning-process detail: {suggestion}"
        );
    }
}
