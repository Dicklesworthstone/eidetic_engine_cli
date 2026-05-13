//! Read-only disk-pressure diagnostics for crowded agent workspaces.
//!
//! This module intentionally plans recovery actions without performing them.
//! The command surface is meant to make disk pressure visible to agents while
//! preserving the repository rule that cleanup needs explicit human approval.

use std::cmp::Reverse;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

pub const DISK_PRESSURE_DIAGNOSTICS_SCHEMA_V1: &str = "ee.disk_pressure.diagnostics.v1";
pub const EXTERNAL_BUILD_ROOT: &str = "/Volumes/USBNVME16TB/temp_agent_space";

const GIB: u64 = 1024 * 1024 * 1024;
const WARNING_AVAILABLE_BYTES: u64 = 20 * GIB;
const DEGRADED_AVAILABLE_BYTES: u64 = 5 * GIB;
const BLOCKED_AVAILABLE_BYTES: u64 = GIB;
const WARNING_PERCENT_USED: f64 = 85.0;
const DEGRADED_PERCENT_USED: f64 = 95.0;
const BLOCKED_PERCENT_USED: f64 = 99.0;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiskPressureOptions {
    pub workspace: PathBuf,
    pub workspace_source: &'static str,
    pub top_limit: usize,
    pub consumer_depth: usize,
    pub consumer_entry_limit: usize,
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

#[must_use]
pub fn gather_disk_pressure_report(options: &DiskPressureOptions) -> DiskPressureReport {
    let workspace = normalize_path(&options.workspace);
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

    let posture = roots
        .iter()
        .map(|root| root.posture)
        .max()
        .unwrap_or(DiskPressurePosture::Ok);
    let guidance = build_guidance(&roots, external_root);
    let recovery_actions = build_recovery_actions(posture, &guidance);

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
    fn percent_used_rounds_to_two_decimals() -> TestResult {
        let value = percent_used(3, 2);
        if (value - 66.67).abs() < 0.001 {
            Ok(())
        } else {
            Err(format!("expected 66.67, got {value}"))
        }
    }
}
