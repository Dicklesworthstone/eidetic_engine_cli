//! Read-only disk-pressure diagnostics for crowded agent workspaces.
//!
//! This module intentionally plans recovery actions without performing them.
//! The command surface is meant to make disk pressure visible to agents while
//! preserving the repository rule that cleanup needs explicit human approval.

use std::cmp::Reverse;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

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

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
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
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildAdmissionDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: String,
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
        let external = path.starts_with(external_root);

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

        if external_available && external_required && !external {
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
            degraded.push(BuildAdmissionDegradation {
                code,
                severity: "warning",
                message: format!(
                    "{label} path `{}` is not under the external build root {}.",
                    path.display(),
                    EXTERNAL_BUILD_ROOT
                ),
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
            admitted: !required || has_required_space,
            external_required: external_available && external_required,
            external,
        });
    }

    let admitted = degraded
        .iter()
        .all(|entry| entry.code != "build_admission_denied");
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
    if !root.is_dir() {
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
    Path::new(path).starts_with(root)
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
}
