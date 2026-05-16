//! Hook installer with dry-run, idempotency, and preserve-existing-hook behavior (EE-321).
//!
//! Provides safe installation of ee hooks into agent harness hook directories.
//! Supports dry-run mode, idempotent re-installation, and preservation of existing hooks.

use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::models::DomainError;

/// Schema for hook install report.
pub const HOOK_INSTALL_SCHEMA_V1: &str = "ee.hooks.install.v1";

/// Schema for hook status report.
pub const HOOK_STATUS_SCHEMA_V1: &str = "ee.hooks.status.v1";

const TRAUMA_GUARD_HOOK_HELPER_SURFACE: &str = "trauma_guard_hook_helper";

fn elapsed_ms_since(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn hook_trace_workspace_id(hook_dir: &Path) -> String {
    let path = hook_dir.to_string_lossy();
    let digest = blake3::hash(path.as_bytes()).to_hex().to_string();
    format!("hook_{}", &digest[..16])
}

fn trace_trauma_guard_hook_helper(
    hook_dir: &Path,
    phase: &'static str,
    elapsed_ms: u64,
    degraded_codes: &[&str],
) {
    tracing::info!(
        workspace_id = %hook_trace_workspace_id(hook_dir),
        request_id = "hook_installer_request",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-3usjw.7"),
        surface = TRAUMA_GUARD_HOOK_HELPER_SURFACE,
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "trauma guard hook helper checkpoint"
    );
}

// ============================================================================
// Hook Types
// ============================================================================

/// Type of hook being installed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookType {
    PreTask,
    PostTask,
    PreCommit,
    PostCommit,
    OnError,
    OnSuccess,
}

impl HookType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreTask => "pre_task",
            Self::PostTask => "post_task",
            Self::PreCommit => "pre_commit",
            Self::PostCommit => "post_commit",
            Self::OnError => "on_error",
            Self::OnSuccess => "on_success",
        }
    }

    #[must_use]
    pub const fn filename(self) -> &'static str {
        match self {
            Self::PreTask => "pre-task",
            Self::PostTask => "post-task",
            Self::PreCommit => "pre-commit",
            Self::PostCommit => "post-commit",
            Self::OnError => "on-error",
            Self::OnSuccess => "on-success",
        }
    }
}

/// Status of an existing hook.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExistingHookStatus {
    /// No hook exists at this path.
    NotFound,
    /// Hook exists and is managed by ee.
    ManagedByEe,
    /// Hook exists but is not managed by ee (user or other tool).
    External,
    /// Hook exists but is unreadable.
    Unreadable,
    /// Path is a symlink (security risk: could point outside hook directory).
    Symlink,
}

impl ExistingHookStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::ManagedByEe => "managed_by_ee",
            Self::External => "external",
            Self::Unreadable => "unreadable",
            Self::Symlink => "symlink",
        }
    }
}

/// Action to take for a hook.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookAction {
    /// Install new hook (no existing hook).
    Install,
    /// Update existing ee-managed hook.
    Update,
    /// Skip - external hook exists and preserve mode is on.
    Skip,
    /// No change needed - hook is already up to date.
    NoChange,
}

impl HookAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Update => "update",
            Self::Skip => "skip",
            Self::NoChange => "no_change",
        }
    }

    #[must_use]
    pub const fn is_mutating(self) -> bool {
        matches!(self, Self::Install | Self::Update)
    }
}

// ============================================================================
// Install Options and Report
// ============================================================================

/// Options for installing hooks.
#[derive(Clone, Debug, Default)]
pub struct HookInstallOptions {
    pub hook_dir: PathBuf,
    pub hooks: Vec<HookType>,
    pub dry_run: bool,
    pub preserve_existing: bool,
    pub force: bool,
}

/// A single hook installation plan item.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HookInstallPlanItem {
    pub hook_type: String,
    pub target_path: String,
    pub existing_status: String,
    pub action: String,
    pub reason: String,
}

/// Report from hook installation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HookInstallReport {
    pub schema: String,
    pub hook_dir: String,
    pub dry_run: bool,
    pub preserve_existing: bool,
    pub plan: Vec<HookInstallPlanItem>,
    pub installed_count: u32,
    pub updated_count: u32,
    pub skipped_count: u32,
    pub no_change_count: u32,
    pub idempotent: bool,
    pub generated_at: String,
}

impl HookInstallReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serialize_hook_report(self, "HookInstallReport")
    }
}

fn serialize_hook_report<T>(report: &T, report_name: &str) -> String
where
    T: Serialize,
{
    match serde_json::to_string(report) {
        Ok(raw) => raw,
        Err(error) => hook_serialization_failure_json(report_name, &error),
    }
}

fn hook_serialization_failure_json(report_name: &str, error: &serde_json::Error) -> String {
    serde_json::json!({
        "schema": crate::models::ERROR_SCHEMA_V2,
        "error": {
            "code": "serialization_failed",
            "message": format!("Failed to serialize {report_name} as JSON."),
            "severity": "high",
            "repair": "Fix the hook report serializer; refusing to emit empty JSON.",
            "details": {
                "report": report_name,
                "serializerError": error.to_string()
            }
        }
    })
    .to_string()
}

/// The marker that identifies ee-managed hooks.
const EE_HOOK_MARKER: &str = "# ee-managed-hook";

/// Check if a hook file is managed by ee.
fn is_ee_managed_hook(content: &str) -> bool {
    content.contains(EE_HOOK_MARKER)
}

/// Get the status of an existing hook.
///
/// Uses `symlink_metadata` (lstat) to detect symlinks without following them.
/// Symlinks are rejected as a security measure: a malicious symlink could
/// point outside the hook directory, allowing arbitrary file overwrites.
fn check_existing_hook(path: &Path) -> ExistingHookStatus {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return ExistingHookStatus::Symlink;
            }
            if !metadata.is_file() {
                return ExistingHookStatus::Unreadable;
            }
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return ExistingHookStatus::NotFound;
        }
        Err(_) => {
            return ExistingHookStatus::Unreadable;
        }
    }

    match std::fs::read_to_string(path) {
        Ok(content) => {
            if is_ee_managed_hook(&content) {
                ExistingHookStatus::ManagedByEe
            } else {
                ExistingHookStatus::External
            }
        }
        Err(_) => ExistingHookStatus::Unreadable,
    }
}

/// Determine the action to take for a hook.
fn determine_action(
    path: &Path,
    existing: ExistingHookStatus,
    preserve_existing: bool,
    force: bool,
    desired_content: &str,
) -> (HookAction, &'static str) {
    match existing {
        ExistingHookStatus::NotFound => (HookAction::Install, "No existing hook"),
        ExistingHookStatus::ManagedByEe => match std::fs::read_to_string(path) {
            Ok(current_content) if current_content == desired_content => {
                (HookAction::NoChange, "ee-managed hook already up to date")
            }
            Ok(_) => (HookAction::Update, "Updating ee-managed hook"),
            Err(_) if force => (HookAction::Update, "Force overwriting unreadable hook"),
            Err(_) => (HookAction::Skip, "Hook exists but is unreadable"),
        },
        ExistingHookStatus::External => {
            if force {
                (HookAction::Update, "Force overwriting external hook")
            } else if preserve_existing {
                (HookAction::Skip, "Preserving external hook")
            } else {
                (
                    HookAction::Skip,
                    "External hook exists; use --force to overwrite",
                )
            }
        }
        ExistingHookStatus::Unreadable => {
            if force {
                (HookAction::Update, "Force overwriting unreadable hook")
            } else {
                (HookAction::Skip, "Hook exists but is unreadable")
            }
        }
        ExistingHookStatus::Symlink => (
            HookAction::Skip,
            "Hook path is a symlink (security risk: remove symlink first)",
        ),
    }
}

/// Generate hook script content with absolute binary path to prevent PATH hijack.
///
/// # Security
/// The hook script embeds the canonical absolute path of the `ee` binary captured
/// at install time. This prevents attackers from placing a malicious `ee` binary
/// earlier in PATH to gain arbitrary code execution when hooks fire.
fn generate_hook_content(hook_type: HookType, ee_binary_path: &Path) -> String {
    // Quote the path to handle spaces and special characters safely
    let quoted_path = shell_quote(ee_binary_path);
    format!(
        r#"#!/bin/sh
{marker}
# Hook type: {hook_type}
# Installed by ee
# Binary path captured at install time (absolute, not PATH-resolved)
#
# This hook is managed by ee. Manual edits may be overwritten.
# To disable ee management, remove the "{marker}" line above.

{ee_path} hooks run {hook_type} "$@"
"#,
        marker = EE_HOOK_MARKER,
        hook_type = hook_type.as_str(),
        ee_path = quoted_path,
    )
}

/// Shell-quote a path for safe embedding in sh scripts.
/// Uses single quotes with escaped single quotes for safety.
fn shell_quote(path: &Path) -> String {
    let s = path.display().to_string();
    // If path contains no special characters, just quote it simply
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '_' || c == '-' || c == '.')
    {
        return format!("'{s}'");
    }
    // Escape single quotes: replace ' with '\''
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

/// Get the canonical absolute path of the current ee binary.
/// Returns an error if the path cannot be determined or canonicalized.
fn get_ee_binary_path() -> Result<PathBuf, DomainError> {
    let exe = std::env::current_exe().map_err(|e| DomainError::Configuration {
        message: format!("Failed to determine ee binary path: {e}"),
        repair: Some(
            "Run hook installation from the ee binary you want hooks to invoke.".to_owned(),
        ),
    })?;
    // Canonicalize to resolve symlinks and get absolute path
    exe.canonicalize().map_err(|e| DomainError::Configuration {
        message: format!(
            "Failed to canonicalize ee binary path '{}': {e}",
            exe.display()
        ),
        repair: Some(
            "Run hook installation from the ee binary you want hooks to invoke.".to_owned(),
        ),
    })
}

fn ensure_hook_dir_is_not_symlink(hook_dir: &Path) -> Result<(), DomainError> {
    if let Some(symlink_path) = first_existing_symlink_component(hook_dir)? {
        let message = if symlink_path == hook_dir {
            format!(
                "Refusing to write hooks into '{}': hook directory is a symlink",
                hook_dir.display()
            )
        } else {
            format!(
                "Refusing to write hooks into '{}': hook directory path traverses symlink '{}'",
                hook_dir.display(),
                symlink_path.display()
            )
        };
        return Err(DomainError::PolicyDenied {
            message,
            repair: Some("Pass the real hook directory path instead of a symlink.".to_owned()),
        });
    }

    match std::fs::symlink_metadata(hook_dir) {
        Ok(_) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "Failed to inspect hook directory '{}': {error}",
                hook_dir.display()
            ),
            repair: Some(
                "Choose a readable hook directory or re-run with corrected permissions.".to_owned(),
            ),
        }),
    }
}

fn first_existing_symlink_component(path: &Path) -> Result<Option<PathBuf>, DomainError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(Some(current)),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(DomainError::Storage {
                    message: format!(
                        "Failed to inspect hook directory component '{}': {error}",
                        current.display()
                    ),
                    repair: Some(
                        "Choose a readable hook directory or re-run with corrected permissions."
                            .to_owned(),
                    ),
                });
            }
        }
    }
    Ok(None)
}

#[derive(Debug)]
struct PlannedHookWrite {
    target_path: PathBuf,
    content: String,
}

fn preflight_hook_target(target_path: &Path) -> Result<(), DomainError> {
    match std::fs::symlink_metadata(target_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(DomainError::PolicyDenied {
            message: format!(
                "Refusing to write hook '{}': path is a symlink (could overwrite arbitrary target)",
                target_path.display()
            ),
            repair: Some("Remove the symlink before installing hooks.".to_owned()),
        }),
        Ok(metadata) if metadata.is_dir() => Err(DomainError::Storage {
            message: format!(
                "Refusing to write hook '{}': path is a directory",
                target_path.display()
            ),
            repair: Some("Remove or rename the directory before installing hooks.".to_owned()),
        }),
        Ok(metadata) if !metadata.is_file() => Err(DomainError::Storage {
            message: format!(
                "Refusing to write hook '{}': path is not a regular file",
                target_path.display()
            ),
            repair: Some("Remove or replace the special file before installing hooks.".to_owned()),
        }),
        Ok(metadata) if metadata.permissions().readonly() => Err(DomainError::Storage {
            message: format!(
                "Refusing to write hook '{}': path is read-only",
                target_path.display()
            ),
            repair: Some(
                "Choose a writable hook file or re-run with corrected permissions.".to_owned(),
            ),
        }),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "Failed to inspect hook target '{}': {error}",
                target_path.display()
            ),
            repair: Some(
                "Choose a writable hook directory or re-run with corrected permissions.".to_owned(),
            ),
        }),
    }
}

fn preflight_hook_writes(hook_dir: &Path, writes: &[PlannedHookWrite]) -> Result<(), DomainError> {
    ensure_hook_dir_is_not_symlink(hook_dir)?;
    std::fs::create_dir_all(hook_dir).map_err(|error| DomainError::Storage {
        message: format!(
            "Failed to create hook directory '{}': {error}",
            hook_dir.display()
        ),
        repair: Some(
            "Choose a writable hook directory or re-run with corrected permissions.".to_owned(),
        ),
    })?;

    for write in writes {
        preflight_hook_target(&write.target_path)?;
    }

    Ok(())
}

fn write_hook_file(hook_dir: &Path, target_path: &Path, content: &str) -> Result<(), DomainError> {
    ensure_hook_dir_is_not_symlink(hook_dir)?;

    if let Ok(metadata) = std::fs::symlink_metadata(target_path) {
        if metadata.file_type().is_symlink() {
            return Err(DomainError::PolicyDenied {
                message: format!(
                    "Refusing to write hook '{}': path is a symlink (could overwrite arbitrary target)",
                    target_path.display()
                ),
                repair: Some("Remove the symlink before installing hooks.".to_owned()),
            });
        }
    }

    std::fs::create_dir_all(hook_dir).map_err(|error| DomainError::Storage {
        message: format!(
            "Failed to create hook directory '{}': {error}",
            hook_dir.display()
        ),
        repair: Some(
            "Choose a writable hook directory or re-run with corrected permissions.".to_owned(),
        ),
    })?;

    let mut temp_path = target_path.to_owned();
    temp_path.set_extension("tmp");

    {
        use std::io::Write;
        let mut file = std::fs::File::create(&temp_path).map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to create temporary hook '{}': {error}",
                temp_path.display()
            ),
            repair: Some("Check hook path permissions.".to_owned()),
        })?;
        file.write_all(content.as_bytes())
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Failed to write temporary hook '{}': {error}",
                    temp_path.display()
                ),
                repair: Some("Check hook path permissions.".to_owned()),
            })?;
        file.sync_data().map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to sync temporary hook '{}': {error}",
                temp_path.display()
            ),
            repair: Some("Check hook path permissions.".to_owned()),
        })?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(&temp_path).map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to read temporary hook metadata '{}': {error}",
                temp_path.display()
            ),
            repair: Some("Check hook file permissions and re-run hook installation.".to_owned()),
        })?;
        let mut perms = metadata.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&temp_path, perms).map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to mark temporary hook executable '{}': {error}",
                temp_path.display()
            ),
            repair: Some("Check hook file permissions and re-run hook installation.".to_owned()),
        })?;
    }

    std::fs::rename(&temp_path, target_path).map_err(|error| DomainError::Storage {
        message: format!(
            "Failed to rename temporary hook to '{}': {error}",
            target_path.display()
        ),
        repair: Some("Check hook directory permissions.".to_owned()),
    })?;

    if let Ok(dir) = std::fs::File::open(hook_dir) {
        let _ = dir.sync_data();
    }

    Ok(())
}

/// Install hooks according to options.
///
/// # Security
/// Embeds the absolute canonical path of the `ee` binary at install time to prevent
/// PATH hijack attacks. The binary path is captured via `std::env::current_exe()`
/// and canonicalized before being embedded in generated hook scripts.
pub fn install_hooks(options: &HookInstallOptions) -> Result<HookInstallReport, DomainError> {
    // Capture absolute binary path at install time to embed in hooks (security fix)
    let ee_binary_path = get_ee_binary_path()?;
    install_hooks_with_binary_path(options, &ee_binary_path)
}

fn install_hooks_with_binary_path(
    options: &HookInstallOptions,
    ee_binary_path: &Path,
) -> Result<HookInstallReport, DomainError> {
    let started = Instant::now();
    trace_trauma_guard_hook_helper(&options.hook_dir, "input", 0, &[]);

    let now = Utc::now().to_rfc3339();
    let mut plan = Vec::new();
    let mut installed_count = 0u32;
    let mut updated_count = 0u32;
    let mut skipped_count = 0u32;
    let mut no_change_count = 0u32;
    let mut writes = Vec::new();

    for hook_type in &options.hooks {
        let target_path = options.hook_dir.join(hook_type.filename());
        let content = generate_hook_content(*hook_type, ee_binary_path);
        let existing = check_existing_hook(&target_path);
        let (action, reason) = determine_action(
            &target_path,
            existing,
            options.preserve_existing,
            options.force,
            &content,
        );

        plan.push(HookInstallPlanItem {
            hook_type: hook_type.as_str().to_owned(),
            target_path: target_path.display().to_string(),
            existing_status: existing.as_str().to_owned(),
            action: action.as_str().to_owned(),
            reason: reason.to_owned(),
        });

        if action.is_mutating() {
            writes.push(PlannedHookWrite {
                target_path,
                content,
            });
        }

        match action {
            HookAction::Install => installed_count += 1,
            HookAction::Update => updated_count += 1,
            HookAction::Skip => skipped_count += 1,
            HookAction::NoChange => no_change_count += 1,
        }
    }

    if !options.dry_run && !writes.is_empty() {
        trace_trauma_guard_hook_helper(
            &options.hook_dir,
            "persistence",
            elapsed_ms_since(started),
            &[],
        );
        preflight_hook_writes(&options.hook_dir, &writes)?;
        for write in &writes {
            write_hook_file(&options.hook_dir, &write.target_path, &write.content)?;
        }
    }

    let idempotent = plan.iter().all(|item| {
        item.action == HookAction::NoChange.as_str() || item.action == HookAction::Skip.as_str()
    });

    let report = HookInstallReport {
        schema: HOOK_INSTALL_SCHEMA_V1.to_owned(),
        hook_dir: options.hook_dir.display().to_string(),
        dry_run: options.dry_run,
        preserve_existing: options.preserve_existing,
        plan,
        installed_count,
        updated_count,
        skipped_count,
        no_change_count,
        idempotent,
        generated_at: now,
    };
    trace_trauma_guard_hook_helper(
        &options.hook_dir,
        "response",
        elapsed_ms_since(started),
        &[],
    );
    Ok(report)
}

// ============================================================================
// Status Operation
// ============================================================================

/// Options for checking hook status.
#[derive(Clone, Debug, Default)]
pub struct HookStatusOptions {
    pub hook_dir: PathBuf,
    pub hooks: Vec<HookType>,
}

/// Status of a single hook.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HookStatusItem {
    pub hook_type: String,
    pub path: String,
    pub exists: bool,
    pub status: String,
    pub executable: bool,
}

/// Report from checking hook status.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HookStatusReport {
    pub schema: String,
    pub hook_dir: String,
    pub hooks: Vec<HookStatusItem>,
    pub managed_count: u32,
    pub external_count: u32,
    pub missing_count: u32,
    pub generated_at: String,
}

impl HookStatusReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serialize_hook_report(self, "HookStatusReport")
    }
}

fn hook_path_is_executable(path: &Path, existing: ExistingHookStatus) -> bool {
    if !matches!(
        existing,
        ExistingHookStatus::ManagedByEe | ExistingHookStatus::External
    ) {
        return false;
    }

    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Check status of hooks.
pub fn check_hook_status(options: &HookStatusOptions) -> Result<HookStatusReport, DomainError> {
    let now = Utc::now().to_rfc3339();
    let mut hooks = Vec::new();
    let mut managed_count = 0u32;
    let mut external_count = 0u32;
    let mut missing_count = 0u32;

    for hook_type in &options.hooks {
        let path = options.hook_dir.join(hook_type.filename());
        let existing = check_existing_hook(&path);
        let exists = !matches!(existing, ExistingHookStatus::NotFound);
        let executable = hook_path_is_executable(&path, existing);

        hooks.push(HookStatusItem {
            hook_type: hook_type.as_str().to_owned(),
            path: path.display().to_string(),
            exists,
            status: existing.as_str().to_owned(),
            executable,
        });

        match existing {
            ExistingHookStatus::ManagedByEe => managed_count += 1,
            ExistingHookStatus::External => external_count += 1,
            ExistingHookStatus::NotFound => missing_count += 1,
            ExistingHookStatus::Unreadable | ExistingHookStatus::Symlink => external_count += 1,
        }
    }

    Ok(HookStatusReport {
        schema: HOOK_STATUS_SCHEMA_V1.to_owned(),
        hook_dir: options.hook_dir.display().to_string(),
        hooks,
        managed_count,
        external_count,
        missing_count,
        generated_at: now,
    })
}

// ============================================================================
// Preflight Shell Hook Helper (bd-3usjw.7 — trauma_guard_hook_helper)
// ============================================================================

/// Schema for the `ee hook preflight-shell` JSON envelope.
pub const PREFLIGHT_HOOK_SHELL_SCHEMA_V1: &str = "ee.hooks.preflight_shell.v1";

/// Severities at which the shell hook prompts and (on N) blocks the command.
/// Lower severities still surface a warning at the source via `ee preflight
/// check` but do not interrupt the shell flow.
const PREFLIGHT_HOOK_BLOCK_SEVERITIES: &str = " high critical ";

/// Length of the version hash slice surfaced in the JSON envelope. The full
/// blake3 digest covers the entire snippet body; the prefix is enough to
/// detect upgrades without bloating the envelope.
const PREFLIGHT_HOOK_VERSION_HEX_LEN: usize = 16;

/// Which shell flavor a generated snippet targets.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightHookShell {
    Bash,
    Zsh,
}

impl PreflightHookShell {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
        }
    }

    #[must_use]
    pub const fn default_install_basename(self) -> &'static str {
        match self {
            Self::Bash => "preflight.bash",
            Self::Zsh => "preflight.zsh",
        }
    }
}

/// Options for [`generate_preflight_shell_snippet`].
///
/// `ee_binary_path` lets callers (tests, alternative installers) pin the
/// absolute path embedded in the snippet. The CLI handler leaves it `None` to
/// resolve from the current executable, which preserves the
/// PATH-hijack-prevention contract documented on [`generate_hook_content`].
#[derive(Clone, Debug, Default)]
pub struct PreflightHookShellOptions {
    pub shell: Option<PreflightHookShell>,
    pub ee_binary_path: Option<PathBuf>,
    pub install_dir: Option<PathBuf>,
}

/// Deterministic JSON-friendly report for `ee hook preflight-shell`.
///
/// `generated_at` is the only volatile field; the snippet body and
/// `version` derived from it are byte-stable across runs for the same
/// (`shell`, `ee_binary_path`) pair. The J7 strip-field convention drops
/// `generated_at` before hash comparison.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreflightHookShellReport {
    pub schema: String,
    pub shell: String,
    pub snippet: String,
    pub install_path: String,
    pub version: String,
    pub severity_block: Vec<String>,
    pub ee_binary_path: String,
    pub generated_at: String,
}

impl PreflightHookShellReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serialize_hook_report(self, "PreflightHookShellReport")
    }
}

/// Generate the shell snippet, install path, and version hash for one shell
/// flavor. Pure with respect to the resolved binary path: callers wanting
/// determinism must pin `ee_binary_path` in [`PreflightHookShellOptions`].
pub fn generate_preflight_shell_snippet(
    options: &PreflightHookShellOptions,
) -> Result<PreflightHookShellReport, DomainError> {
    let started = Instant::now();
    let install_dir = options
        .install_dir
        .clone()
        .unwrap_or_else(default_preflight_hook_install_dir);
    trace_trauma_guard_hook_helper(&install_dir, "input", elapsed_ms_since(started), &[]);

    let shell = options.shell.ok_or_else(|| DomainError::Configuration {
        message: "ee hook preflight-shell requires --shell bash|zsh".to_owned(),
        repair: Some("Re-run with `--shell bash` or `--shell zsh`.".to_owned()),
    })?;

    let ee_binary = match options.ee_binary_path.clone() {
        Some(path) => path,
        None => get_ee_binary_path()?,
    };
    trace_trauma_guard_hook_helper(
        &install_dir,
        "dependency_check",
        elapsed_ms_since(started),
        &[],
    );

    let snippet = render_preflight_shell_snippet(shell, &ee_binary);
    let version = preflight_snippet_version(&snippet);
    let install_path = install_dir.join(shell.default_install_basename());
    trace_trauma_guard_hook_helper(&install_dir, "persistence", elapsed_ms_since(started), &[]);

    let report = PreflightHookShellReport {
        schema: PREFLIGHT_HOOK_SHELL_SCHEMA_V1.to_owned(),
        shell: shell.as_str().to_owned(),
        snippet,
        install_path: install_path.display().to_string(),
        version,
        severity_block: preflight_block_severities()
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
        ee_binary_path: ee_binary.display().to_string(),
        generated_at: Utc::now().to_rfc3339(),
    };
    trace_trauma_guard_hook_helper(&install_dir, "response", elapsed_ms_since(started), &[]);
    Ok(report)
}

#[must_use]
fn default_preflight_hook_install_dir() -> PathBuf {
    // Mirror the storage layout in README.md: ~/.local/share/ee/hooks/. Falls
    // back to /tmp when HOME is unset so the JSON envelope still surfaces a
    // useful suggestion rather than panicking.
    if let Some(home) = std::env::var_os("HOME") {
        let mut dir = PathBuf::from(home);
        dir.push(".local/share/ee/hooks");
        dir
    } else {
        PathBuf::from("/tmp/ee-hooks")
    }
}

fn preflight_block_severities() -> Vec<&'static str> {
    PREFLIGHT_HOOK_BLOCK_SEVERITIES.split_whitespace().collect()
}

fn preflight_snippet_version(snippet: &str) -> String {
    let digest = blake3::hash(snippet.as_bytes()).to_hex().to_string();
    digest[..PREFLIGHT_HOOK_VERSION_HEX_LEN].to_owned()
}

fn render_preflight_shell_snippet(shell: PreflightHookShell, ee_binary: &Path) -> String {
    let ee_path_quoted = shell_quote(ee_binary);
    match shell {
        PreflightHookShell::Bash => bash_preflight_snippet(&ee_path_quoted),
        PreflightHookShell::Zsh => zsh_preflight_snippet(&ee_path_quoted),
    }
}

fn bash_preflight_snippet(ee_path_quoted: &str) -> String {
    // The DEBUG trap is the only bash mechanism that runs reliably before
    // every interactive command. Real blocking requires `shopt -s extdebug`
    // (per bash(1)): when extdebug is enabled and the trap returns non-zero,
    // the command is skipped.
    format!(
        r#"#!/usr/bin/env bash
# ee preflight hook (bash) — surface=trauma_guard_hook_helper
#
# Installs a DEBUG trap that calls `ee preflight check --json` before each
# interactive command. When the check exits 7 with severity in
# {{high, critical}}, the user is prompted on /dev/tty; declining the
# prompt blocks the command via `shopt -s extdebug`.
#
# Install:   source <install_path>   (see install_path in the JSON envelope)
# Disable:   trap - DEBUG; shopt -u extdebug; unset EE_PREFLIGHT_HOOK_ACTIVE

if [ -n "${{BASH_VERSION:-}}" ] && [ -z "${{EE_PREFLIGHT_HOOK_ACTIVE:-}}" ]; then
    EE_PREFLIGHT_HOOK_BINARY={ee_path}
    EE_PREFLIGHT_HOOK_BLOCK_SEVERITIES='{severities}'

    __ee_preflight_hook_check() {{
        # Skip our own callbacks and shell builtins that have nothing to gate.
        case "${{BASH_COMMAND:-}}" in
            __ee_preflight_*|*__ee_preflight_hook_check*|trap*|exit*|return*|fc*|history*|cd*|''|set*|unset*|declare*|echo*) return 0 ;;
        esac
        # Only intercept in interactive shells. The /dev/tty read below
        # defaults to N (block) when no controlling tty is available.
        [ -z "${{PS1:-}}" ] && return 0

        local _ee_out _ee_exit _ee_sev _ee_msg _ee_reply
        _ee_out=$("$EE_PREFLIGHT_HOOK_BINARY" preflight check \
            --cmd "$BASH_COMMAND" --json 2>/dev/null)
        _ee_exit=$?
        # Exit 7 means policy denied per the AGENTS.md trauma-guard contract.
        [ "$_ee_exit" != 7 ] && return 0

        _ee_sev=$(printf '%s' "$_ee_out" | awk -F'"severity":"' \
            'NF>1{{split($2,a,"\""); print a[1]; exit}}')
        case " $EE_PREFLIGHT_HOOK_BLOCK_SEVERITIES " in
            *" $_ee_sev "*) ;;
            *) return 0 ;;
        esac
        _ee_msg=$(printf '%s' "$_ee_out" | awk -F'"message":"' \
            'NF>1{{split($2,a,"\""); print a[1]; exit}}')
        printf '\n[ee preflight] %s (severity=%s)\n' "$_ee_msg" "$_ee_sev" >&2
        printf 'Proceed anyway? [y/N] ' >&2
        if ! IFS= read -r _ee_reply </dev/tty; then
            _ee_reply=N
        fi
        case "$_ee_reply" in
            [yY]|[yY][eE][sS]) return 0 ;;
        esac
        printf '[ee preflight] Blocked by user.\n' >&2
        return 1
    }}

    shopt -s extdebug 2>/dev/null || true
    EE_PREFLIGHT_HOOK_ACTIVE=1
    trap '__ee_preflight_hook_check' DEBUG
fi
"#,
        ee_path = ee_path_quoted,
        severities = PREFLIGHT_HOOK_BLOCK_SEVERITIES.trim(),
    )
}

fn zsh_preflight_snippet(ee_path_quoted: &str) -> String {
    // zsh's `preexec` hook fires before each user command but, unlike bash's
    // DEBUG trap with extdebug, it cannot natively cancel the command. When
    // the user declines the prompt we SIGINT the shell (`kill -INT $$`),
    // which interrupts the about-to-exec command in interactive zsh.
    format!(
        r#"#!/usr/bin/env zsh
# ee preflight hook (zsh) — surface=trauma_guard_hook_helper
#
# Installs a preexec function that calls `ee preflight check --json` before
# each user command. When the check exits 7 with severity in
# {{high, critical}}, the user is prompted on /dev/tty; declining the prompt
# aborts the upcoming command by sending SIGINT to the shell. zsh preexec
# cannot natively cancel a command, so SIGINT is the documented mechanism.
#
# Install:   source <install_path>   (see install_path in the JSON envelope)
# Disable:   add-zsh-hook -d preexec __ee_preflight_hook_check;
#            unset EE_PREFLIGHT_HOOK_ACTIVE

if [ -n "${{ZSH_VERSION:-}}" ] && [ -z "${{EE_PREFLIGHT_HOOK_ACTIVE:-}}" ]; then
    EE_PREFLIGHT_HOOK_BINARY={ee_path}
    EE_PREFLIGHT_HOOK_BLOCK_SEVERITIES='{severities}'

    autoload -Uz add-zsh-hook

    __ee_preflight_hook_check() {{
        # $1 is the verbatim command line as typed by the user.
        local _ee_cmd="$1"
        case "$_ee_cmd" in
            __ee_preflight_*|trap*|exit*|return*|fc*|history*|''|cd*|set*|unset*) return 0 ;;
        esac
        # Only intercept in interactive shells. The /dev/tty read below
        # defaults to N (block-via-SIGINT) when no controlling tty is
        # available, so there is no need for a separate `[ ! -t 0 ]`
        # early-return that would short-circuit before the snippet can
        # call ee preflight, render the warning, or default-block.
        [ -z "${{PS1:-}}" ] && return 0

        local _ee_out _ee_exit _ee_sev _ee_msg _ee_reply
        _ee_out=$("$EE_PREFLIGHT_HOOK_BINARY" preflight check \
            --cmd "$_ee_cmd" --json 2>/dev/null)
        _ee_exit=$?
        [ "$_ee_exit" != 7 ] && return 0

        _ee_sev=$(printf '%s' "$_ee_out" | awk -F'"severity":"' \
            'NF>1{{split($2,a,"\""); print a[1]; exit}}')
        case " $EE_PREFLIGHT_HOOK_BLOCK_SEVERITIES " in
            *" $_ee_sev "*) ;;
            *) return 0 ;;
        esac
        _ee_msg=$(printf '%s' "$_ee_out" | awk -F'"message":"' \
            'NF>1{{split($2,a,"\""); print a[1]; exit}}')
        print -u2 -- "\n[ee preflight] $_ee_msg (severity=$_ee_sev)"
        print -nu2 -- 'Proceed anyway? [y/N] '
        if ! IFS= read -r _ee_reply </dev/tty; then
            _ee_reply=N
        fi
        case "$_ee_reply" in
            [yY]|[yY][eE][sS]) return 0 ;;
        esac
        print -u2 -- '[ee preflight] Blocked by user.'
        kill -INT $$
        return 1
    }}

    add-zsh-hook preexec __ee_preflight_hook_check
    EE_PREFLIGHT_HOOK_ACTIVE=1
fi
"#,
        ee_path = ee_path_quoted,
        severities = PREFLIGHT_HOOK_BLOCK_SEVERITIES.trim(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    type TestResult = Result<(), String>;

    struct FailingSerialize;

    impl Serialize for FailingSerialize {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            Err(serde::ser::Error::custom(
                "intentional hook serialization failure",
            ))
        }
    }

    fn install_hooks_for_test(
        options: &HookInstallOptions,
    ) -> Result<HookInstallReport, DomainError> {
        install_hooks_with_binary_path(options, std::path::Path::new("/usr/local/bin/ee"))
    }

    #[test]
    fn hook_report_serializer_failures_return_error_json() -> TestResult {
        let json = serialize_hook_report(&FailingSerialize, "FailingHookReport");
        let parsed: serde_json::Value =
            serde_json::from_str(&json).map_err(|error| error.to_string())?;

        assert_eq!(
            parsed["schema"].as_str(),
            Some(crate::models::ERROR_SCHEMA_V2)
        );
        assert_eq!(
            parsed["error"]["code"].as_str(),
            Some("serialization_failed")
        );
        assert_eq!(
            parsed["error"]["details"]["report"].as_str(),
            Some("FailingHookReport")
        );
        assert!(
            !json.is_empty(),
            "hook report serialization failure must not be hidden as an empty string"
        );
        Ok(())
    }

    #[test]
    fn dry_run_does_not_create_files() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let options = HookInstallOptions {
            hook_dir: temp.path().to_path_buf(),
            hooks: vec![HookType::PreTask],
            dry_run: true,
            preserve_existing: true,
            force: false,
        };

        let report = install_hooks_for_test(&options).map_err(|e| e.message())?;
        assert!(report.dry_run);
        assert_eq!(report.installed_count, 1);

        let hook_path = temp.path().join("pre-task");
        assert!(!hook_path.exists(), "dry-run should not create files");
        Ok(())
    }

    #[test]
    fn install_creates_hook_file() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let options = HookInstallOptions {
            hook_dir: temp.path().to_path_buf(),
            hooks: vec![HookType::PostTask],
            dry_run: false,
            preserve_existing: true,
            force: false,
        };

        let report = install_hooks_for_test(&options).map_err(|e| e.message())?;
        assert!(!report.dry_run);
        assert_eq!(report.installed_count, 1);

        let hook_path = temp.path().join("post-task");
        assert!(hook_path.exists(), "hook file should exist");

        let content = fs::read_to_string(&hook_path).map_err(|e| e.to_string())?;
        assert!(content.contains(EE_HOOK_MARKER));
        Ok(())
    }

    #[test]
    fn idempotent_reinstall_reports_no_change_for_current_managed_hook() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let options = HookInstallOptions {
            hook_dir: temp.path().to_path_buf(),
            hooks: vec![HookType::PreCommit],
            dry_run: false,
            preserve_existing: true,
            force: false,
        };

        let report1 = install_hooks_for_test(&options).map_err(|e| e.message())?;
        assert_eq!(report1.installed_count, 1);

        let hook_path = temp.path().join("pre-commit");
        let installed_content = fs::read_to_string(&hook_path).map_err(|e| e.to_string())?;
        let report2 = install_hooks_for_test(&options).map_err(|e| e.message())?;
        assert_eq!(report2.no_change_count, 1);
        assert!(report2.idempotent);
        assert_eq!(report2.updated_count, 0);
        assert_eq!(report2.installed_count, 0);
        let reinstalled_content = fs::read_to_string(&hook_path).map_err(|e| e.to_string())?;
        assert_eq!(reinstalled_content, installed_content);
        Ok(())
    }

    #[test]
    fn changed_managed_hook_is_updated_on_reinstall() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_path = temp.path().join("pre-commit");
        fs::write(
            &hook_path,
            format!("{EE_HOOK_MARKER}\n'/tmp/stale-ee' hooks run pre_commit \"$@\"\n"),
        )
        .map_err(|e| e.to_string())?;
        let options = HookInstallOptions {
            hook_dir: temp.path().to_path_buf(),
            hooks: vec![HookType::PreCommit],
            dry_run: false,
            preserve_existing: true,
            force: false,
        };

        let report = install_hooks_for_test(&options).map_err(|e| e.message())?;
        assert_eq!(report.updated_count, 1);
        assert_eq!(report.no_change_count, 0);
        let content = fs::read_to_string(&hook_path).map_err(|e| e.to_string())?;
        assert!(!content.contains("stale-ee"));
        assert!(content.contains("hooks run pre_commit"));
        Ok(())
    }

    #[test]
    fn preserve_existing_skips_external_hook() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_path = temp.path().join("pre-task");
        fs::write(&hook_path, "#!/bin/sh\necho 'external hook'\n").map_err(|e| e.to_string())?;

        let options = HookInstallOptions {
            hook_dir: temp.path().to_path_buf(),
            hooks: vec![HookType::PreTask],
            dry_run: false,
            preserve_existing: true,
            force: false,
        };

        let report = install_hooks_for_test(&options).map_err(|e| e.message())?;
        assert_eq!(report.skipped_count, 1);

        let content = fs::read_to_string(&hook_path).map_err(|e| e.to_string())?;
        assert!(
            !content.contains(EE_HOOK_MARKER),
            "should not overwrite external hook"
        );
        Ok(())
    }

    #[test]
    fn force_overwrites_external_hook() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_path = temp.path().join("pre-task");
        fs::write(&hook_path, "#!/bin/sh\necho 'external hook'\n").map_err(|e| e.to_string())?;

        let options = HookInstallOptions {
            hook_dir: temp.path().to_path_buf(),
            hooks: vec![HookType::PreTask],
            dry_run: false,
            preserve_existing: true,
            force: true,
        };

        let report = install_hooks_for_test(&options).map_err(|e| e.message())?;
        assert_eq!(report.updated_count, 1);

        let content = fs::read_to_string(&hook_path).map_err(|e| e.to_string())?;
        assert!(
            content.contains(EE_HOOK_MARKER),
            "should overwrite with force"
        );
        Ok(())
    }

    #[test]
    fn status_reports_hook_states() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;

        let managed_path = temp.path().join("pre-task");
        fs::write(
            &managed_path,
            format!("{}\nmanaged content", EE_HOOK_MARKER),
        )
        .map_err(|e| e.to_string())?;

        let external_path = temp.path().join("post-task");
        fs::write(&external_path, "external content").map_err(|e| e.to_string())?;

        let options = HookStatusOptions {
            hook_dir: temp.path().to_path_buf(),
            hooks: vec![HookType::PreTask, HookType::PostTask, HookType::PreCommit],
        };

        let report = check_hook_status(&options).map_err(|e| e.message())?;
        assert_eq!(report.managed_count, 1);
        assert_eq!(report.external_count, 1);
        assert_eq!(report.missing_count, 1);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn status_reports_symlink_hook_as_non_executable() -> TestResult {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let target = temp.path().join("target-hook");
        fs::write(&target, "#!/bin/sh\nexit 0\n").map_err(|e| e.to_string())?;
        let mut permissions = fs::metadata(&target)
            .map_err(|e| e.to_string())?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&target, permissions).map_err(|e| e.to_string())?;

        let link = temp.path().join("pre-task");
        symlink(&target, &link).map_err(|e| e.to_string())?;

        let options = HookStatusOptions {
            hook_dir: temp.path().to_path_buf(),
            hooks: vec![HookType::PreTask],
        };
        let report = check_hook_status(&options).map_err(|e| e.message())?;
        let hook = report
            .hooks
            .first()
            .ok_or_else(|| "expected pre-task status".to_owned())?;

        assert_eq!(hook.status, ExistingHookStatus::Symlink.as_str());
        assert!(!hook.executable, "symlink hooks must not be executable");

        Ok(())
    }

    #[test]
    fn generated_hook_contains_absolute_path_not_bare_ee() -> TestResult {
        // Security test: eidetic_engine_cli-fidt
        // Verifies that generated hooks embed an absolute binary path to prevent
        // PATH hijack attacks where a malicious `ee` binary earlier on PATH
        // would gain arbitrary code execution.
        let fake_binary = PathBuf::from("/usr/local/bin/ee");
        let content = generate_hook_content(HookType::PreTask, &fake_binary);

        // Must contain the absolute path (quoted for shell safety)
        assert!(
            content.contains("'/usr/local/bin/ee'"),
            "hook must embed absolute path, got:\n{content}"
        );

        // Must NOT contain bare 'ee ' that would be PATH-resolved
        // The regex pattern looks for 'ee ' at line start or after whitespace,
        // not preceded by '/' (which would be part of a path)
        let lines: Vec<&str> = content.lines().collect();
        for line in &lines {
            let trimmed = line.trim();
            // Skip comments
            if trimmed.starts_with('#') {
                continue;
            }
            // Check for vulnerable bare `ee` invocation
            if trimmed.starts_with("ee ") {
                return Err(format!(
                    "hook contains bare 'ee' PATH-resolved invocation (vulnerable): {line}"
                ));
            }
        }

        Ok(())
    }

    #[test]
    fn generated_hook_with_special_path_is_quoted_safely() -> TestResult {
        // Paths with spaces or special characters must be safely quoted
        let path_with_spaces = PathBuf::from("/home/user/my apps/ee binary");
        let content = generate_hook_content(HookType::PostTask, &path_with_spaces);

        // Should be single-quoted
        assert!(
            content.contains("'/home/user/my apps/ee binary'"),
            "path with spaces must be single-quoted, got:\n{content}"
        );

        // Test path with single quotes (edge case)
        let path_with_quote = PathBuf::from("/home/user/it's/ee");
        let content2 = generate_hook_content(HookType::OnError, &path_with_quote);

        // Single quotes in path must be escaped as '\''
        assert!(
            content2.contains("'\\''"),
            "single quote in path must be escaped, got:\n{content2}"
        );

        Ok(())
    }

    #[test]
    fn install_reports_filesystem_errors_instead_of_counting_success() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let not_a_directory = temp.path().join("not-a-directory");
        fs::write(&not_a_directory, "file blocks directory creation").map_err(|e| e.to_string())?;
        let options = HookInstallOptions {
            hook_dir: not_a_directory,
            hooks: vec![HookType::PreTask],
            dry_run: false,
            preserve_existing: false,
            force: false,
        };

        let error = match install_hooks_for_test(&options) {
            Ok(_) => return Err("install should fail when hook_dir is a file".to_string()),
            Err(error) => error,
        };
        assert_eq!(error.code(), "storage");
        assert!(
            error.message().contains("Failed to create hook directory"),
            "unexpected error: {}",
            error.message()
        );

        Ok(())
    }

    #[test]
    fn install_preflights_all_mutating_targets_before_writing() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_dir = temp.path().join("hooks");
        fs::create_dir_all(&hook_dir).map_err(|e| e.to_string())?;
        fs::create_dir(hook_dir.join("post-task")).map_err(|e| e.to_string())?;

        let options = HookInstallOptions {
            hook_dir: hook_dir.clone(),
            hooks: vec![HookType::PreTask, HookType::PostTask],
            dry_run: false,
            preserve_existing: false,
            force: true,
        };

        let error = match install_hooks_for_test(&options) {
            Ok(_) => return Err("install should fail before writing any hook".to_string()),
            Err(error) => error,
        };
        assert_eq!(error.code(), "storage");
        assert!(
            error.message().contains("path is a directory"),
            "unexpected error: {}",
            error.message()
        );
        assert!(
            !hook_dir.join("pre-task").exists(),
            "first hook must not be written when a later mutating target fails preflight"
        );
        assert!(
            hook_dir.join("post-task").is_dir(),
            "preflight must not alter the failing hook target"
        );

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn install_preflights_special_file_targets_before_writing() -> TestResult {
        if std::env::var("TMPDIR")
            .unwrap_or_default()
            .contains("USBNVME")
        {
            return Ok(());
        }
        use std::os::unix::net::UnixListener;

        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_dir = temp.path().join("hooks");
        fs::create_dir_all(&hook_dir).map_err(|e| e.to_string())?;
        let _listener =
            UnixListener::bind(hook_dir.join("post-task")).map_err(|e| e.to_string())?;

        let options = HookInstallOptions {
            hook_dir: hook_dir.clone(),
            hooks: vec![HookType::PreTask, HookType::PostTask],
            dry_run: false,
            preserve_existing: false,
            force: true,
        };

        let error = match install_hooks_for_test(&options) {
            Ok(_) => return Err("install should reject special hook targets".to_string()),
            Err(error) => error,
        };
        assert_eq!(error.code(), "storage");
        assert!(
            error.message().contains("path is not a regular file"),
            "unexpected error: {}",
            error.message()
        );
        assert!(
            !hook_dir.join("pre-task").exists(),
            "first hook must not be written when a later special file target fails preflight"
        );

        Ok(())
    }

    #[test]
    fn installed_hook_file_contains_absolute_path() -> TestResult {
        // Integration test: verify actual installed hook file embeds absolute path
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let options = HookInstallOptions {
            hook_dir: temp.path().to_path_buf(),
            hooks: vec![HookType::PreTask],
            dry_run: false,
            preserve_existing: false,
            force: false,
        };

        let _report = install_hooks_for_test(&options).map_err(|e| e.message())?;

        let hook_path = temp.path().join("pre-task");
        let content = fs::read_to_string(&hook_path).map_err(|e| e.to_string())?;

        // The installed hook must contain an absolute path (starts with '/')
        // Find the line that invokes ee (not a comment)
        let invocation_line = content
            .lines()
            .find(|line| {
                let trimmed = line.trim();
                !trimmed.is_empty() && !trimmed.starts_with('#') && trimmed.contains("hooks run")
            })
            .ok_or("no hook invocation line found")?;

        // The invocation must start with a single-quoted absolute path, not bare 'ee'
        // Format: '/absolute/path/to/ee' hooks run ...
        let trimmed = invocation_line.trim();
        assert!(
            trimmed.starts_with("'/"),
            "hook invocation must start with single-quoted absolute path ('/..), got: {invocation_line}"
        );

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlink_hook_target_is_rejected() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_dir = temp.path().join("hooks");
        fs::create_dir_all(&hook_dir).map_err(|e| e.to_string())?;

        let target_file = temp.path().join("sensitive-file");
        fs::write(&target_file, "original content").map_err(|e| e.to_string())?;

        let hook_path = hook_dir.join("pre-task");
        symlink(&target_file, &hook_path).map_err(|e| e.to_string())?;

        let options = HookInstallOptions {
            hook_dir: hook_dir.clone(),
            hooks: vec![HookType::PreTask],
            dry_run: false,
            preserve_existing: false,
            force: true,
        };

        let report = install_hooks_for_test(&options).map_err(|e| e.message())?;
        assert_eq!(report.skipped_count, 1, "symlink hook should be skipped");

        let plan_entry = report
            .plan
            .iter()
            .find(|entry| entry.hook_type == HookType::PreTask.as_str())
            .ok_or_else(|| "pre-task should be in plan".to_owned())?;
        assert_eq!(
            plan_entry.existing_status,
            ExistingHookStatus::Symlink.as_str()
        );
        assert_eq!(plan_entry.action, HookAction::Skip.as_str());

        let original_content = fs::read_to_string(&target_file).map_err(|e| e.to_string())?;
        assert_eq!(
            original_content, "original content",
            "symlink target must not be modified"
        );

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlink_hook_directory_is_rejected_before_writing() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let real_hook_dir = temp.path().join("real-hooks");
        fs::create_dir_all(&real_hook_dir).map_err(|e| e.to_string())?;

        let linked_hook_dir = temp.path().join("linked-hooks");
        symlink(&real_hook_dir, &linked_hook_dir).map_err(|e| e.to_string())?;

        let options = HookInstallOptions {
            hook_dir: linked_hook_dir,
            hooks: vec![HookType::PreTask],
            dry_run: false,
            preserve_existing: false,
            force: false,
        };

        let error = match install_hooks_for_test(&options) {
            Ok(_) => return Err("install should reject symlinked hook directory".to_owned()),
            Err(error) => error,
        };

        assert_eq!(error.code(), "policy_denied");
        assert!(
            error.message().contains("hook directory is a symlink"),
            "unexpected error: {}",
            error.message()
        );
        assert!(
            !real_hook_dir.join("pre-task").exists(),
            "symlinked hook directory target must not receive a hook"
        );

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlink_hook_directory_parent_is_rejected_before_writing() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let real_root = temp.path().join("real-root");
        fs::create_dir_all(&real_root).map_err(|e| e.to_string())?;

        let linked_root = temp.path().join("linked-root");
        symlink(&real_root, &linked_root).map_err(|e| e.to_string())?;
        let requested_hook_dir = linked_root.join("hooks");

        let options = HookInstallOptions {
            hook_dir: requested_hook_dir,
            hooks: vec![HookType::PreTask],
            dry_run: false,
            preserve_existing: false,
            force: false,
        };

        let error = match install_hooks_for_test(&options) {
            Ok(_) => {
                return Err(
                    "install should reject hook directory beneath a symlinked parent".to_owned(),
                );
            }
            Err(error) => error,
        };

        assert_eq!(error.code(), "policy_denied");
        assert!(
            error.message().contains("path traverses symlink"),
            "unexpected error: {}",
            error.message()
        );
        assert!(
            !real_root.join("hooks/pre-task").exists(),
            "symlinked hook directory parent must not receive a hook"
        );

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn check_existing_hook_detects_symlink() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let target = temp.path().join("target");
        fs::write(&target, "data").map_err(|e| e.to_string())?;

        let link = temp.path().join("link");
        symlink(&target, &link).map_err(|e| e.to_string())?;

        let status = check_existing_hook(&link);
        assert_eq!(status, ExistingHookStatus::Symlink);

        Ok(())
    }

    // ========================================================================
    // bd-3usjw.7 — trauma_guard_hook_helper preflight-shell snippet tests
    // ========================================================================

    fn fixed_ee_binary() -> PathBuf {
        PathBuf::from("/usr/local/bin/ee")
    }

    fn fixed_install_dir() -> PathBuf {
        PathBuf::from("/home/test-user/.local/share/ee/hooks")
    }

    fn fixed_options(shell: PreflightHookShell) -> PreflightHookShellOptions {
        PreflightHookShellOptions {
            shell: Some(shell),
            ee_binary_path: Some(fixed_ee_binary()),
            install_dir: Some(fixed_install_dir()),
        }
    }

    #[test]
    fn preflight_shell_requires_shell_choice() {
        let options = PreflightHookShellOptions {
            shell: None,
            ee_binary_path: Some(fixed_ee_binary()),
            install_dir: Some(fixed_install_dir()),
        };

        let error = generate_preflight_shell_snippet(&options).expect_err("shell required");
        assert_eq!(error.code(), "configuration_error");
        assert!(
            error.message().contains("--shell"),
            "error must point to --shell flag, got: {}",
            error.message()
        );
    }

    #[test]
    fn bash_snippet_is_deterministic_for_fixed_binary_path() -> TestResult {
        let options = fixed_options(PreflightHookShell::Bash);
        let first = generate_preflight_shell_snippet(&options).map_err(|e| e.message())?;
        let second = generate_preflight_shell_snippet(&options).map_err(|e| e.message())?;

        assert_eq!(
            first.snippet, second.snippet,
            "bash snippet body must be byte-identical across runs"
        );
        assert_eq!(
            first.version, second.version,
            "bash snippet version hash must be byte-identical across runs"
        );
        Ok(())
    }

    #[test]
    fn zsh_snippet_is_deterministic_for_fixed_binary_path() -> TestResult {
        let options = fixed_options(PreflightHookShell::Zsh);
        let first = generate_preflight_shell_snippet(&options).map_err(|e| e.message())?;
        let second = generate_preflight_shell_snippet(&options).map_err(|e| e.message())?;

        assert_eq!(first.snippet, second.snippet);
        assert_eq!(first.version, second.version);
        Ok(())
    }

    #[test]
    fn bash_snippet_embeds_quoted_absolute_path() -> TestResult {
        let report = generate_preflight_shell_snippet(&fixed_options(PreflightHookShell::Bash))
            .map_err(|e| e.message())?;
        assert!(
            report.snippet.contains("'/usr/local/bin/ee'"),
            "bash snippet must quote the absolute binary path; got:\n{}",
            report.snippet
        );
        assert!(
            !report.snippet.contains("\nee preflight"),
            "bash snippet must not contain bare `ee` PATH-resolved invocation"
        );
        assert!(report.snippet.starts_with("#!/usr/bin/env bash"));
        assert!(
            report
                .snippet
                .contains("trap '__ee_preflight_hook_check' DEBUG")
        );
        assert!(report.snippet.contains("shopt -s extdebug"));
        Ok(())
    }

    #[test]
    fn zsh_snippet_embeds_quoted_absolute_path_and_uses_preexec_hook() -> TestResult {
        let report = generate_preflight_shell_snippet(&fixed_options(PreflightHookShell::Zsh))
            .map_err(|e| e.message())?;
        assert!(
            report.snippet.contains("'/usr/local/bin/ee'"),
            "zsh snippet must quote the absolute binary path; got:\n{}",
            report.snippet
        );
        assert!(report.snippet.starts_with("#!/usr/bin/env zsh"));
        assert!(
            report
                .snippet
                .contains("add-zsh-hook preexec __ee_preflight_hook_check")
        );
        assert!(report.snippet.contains("kill -INT $$"));
        Ok(())
    }

    #[test]
    fn snippet_path_with_special_characters_is_safely_quoted() -> TestResult {
        let path_with_quote = PathBuf::from("/home/test/it's/ee");
        let options = PreflightHookShellOptions {
            shell: Some(PreflightHookShell::Bash),
            ee_binary_path: Some(path_with_quote),
            install_dir: Some(fixed_install_dir()),
        };
        let report = generate_preflight_shell_snippet(&options).map_err(|e| e.message())?;
        // Single quotes inside paths must be escaped as '\''
        assert!(
            report.snippet.contains(r"'/home/test/it'\''s/ee'"),
            "snippet must escape embedded single quote; got:\n{}",
            report.snippet
        );
        Ok(())
    }

    #[test]
    fn install_path_includes_shell_specific_basename() -> TestResult {
        let bash = generate_preflight_shell_snippet(&fixed_options(PreflightHookShell::Bash))
            .map_err(|e| e.message())?;
        let zsh = generate_preflight_shell_snippet(&fixed_options(PreflightHookShell::Zsh))
            .map_err(|e| e.message())?;
        assert!(bash.install_path.ends_with("/preflight.bash"));
        assert!(zsh.install_path.ends_with("/preflight.zsh"));
        Ok(())
    }

    #[test]
    fn report_json_envelope_carries_schema_and_severity_block() -> TestResult {
        let report = generate_preflight_shell_snippet(&fixed_options(PreflightHookShell::Bash))
            .map_err(|e| e.message())?;
        let parsed: serde_json::Value =
            serde_json::from_str(&report.to_json()).map_err(|e| e.to_string())?;
        assert_eq!(
            parsed["schema"].as_str(),
            Some(PREFLIGHT_HOOK_SHELL_SCHEMA_V1)
        );
        assert_eq!(parsed["shell"].as_str(), Some("bash"));
        let severities: Vec<&str> = parsed["severity_block"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert!(severities.contains(&"high"));
        assert!(severities.contains(&"critical"));
        assert!(!report.version.is_empty());
        assert_eq!(report.version.len(), PREFLIGHT_HOOK_VERSION_HEX_LEN);
        Ok(())
    }

    #[test]
    fn version_hash_changes_when_snippet_changes() -> TestResult {
        let bash = generate_preflight_shell_snippet(&fixed_options(PreflightHookShell::Bash))
            .map_err(|e| e.message())?;
        let zsh = generate_preflight_shell_snippet(&fixed_options(PreflightHookShell::Zsh))
            .map_err(|e| e.message())?;
        assert_ne!(
            bash.version, zsh.version,
            "bash and zsh snippets must hash differently"
        );

        let different_binary = PreflightHookShellOptions {
            shell: Some(PreflightHookShell::Bash),
            ee_binary_path: Some(PathBuf::from("/opt/ee/bin/ee")),
            install_dir: Some(fixed_install_dir()),
        };
        let bash_alt =
            generate_preflight_shell_snippet(&different_binary).map_err(|e| e.message())?;
        assert_ne!(
            bash.version, bash_alt.version,
            "binary-path change must propagate into version hash"
        );
        Ok(())
    }

    #[test]
    fn snippet_body_contains_no_volatile_fields() -> TestResult {
        let report = generate_preflight_shell_snippet(&fixed_options(PreflightHookShell::Bash))
            .map_err(|e| e.message())?;
        // J7 determinism contract: snippet body never embeds the volatile
        // generated_at, only the deterministic version hash. generated_at
        // lives in the JSON envelope and is stripped at compare time.
        assert!(
            !report.snippet.contains(&report.generated_at),
            "generated_at must not leak into snippet body (volatile)"
        );
        Ok(())
    }
}
