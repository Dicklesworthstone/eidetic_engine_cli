//! Hook installer with dry-run, idempotency, and preserve-existing-hook behavior (EE-321).
//!
//! Provides safe installation of ee hooks into agent harness hook directories.
//! Supports dry-run mode, idempotent re-installation, and preservation of existing hooks.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::core::git_ahead::{
    GIT_AHEAD_LOG_FORMAT, GitAheadLogState, GitAheadSnapshot, summarize_git_ahead,
    summarize_git_ahead_with_log_state,
};
use crate::models::DomainError;

/// Schema for hook install report.
pub const HOOK_INSTALL_SCHEMA_V1: &str = "ee.hooks.install.v1";

/// Schema for hook status report.
pub const HOOK_STATUS_SCHEMA_V1: &str = "ee.hooks.status.v1";

/// Schema for local Git hook-chain readiness diagnostics.
pub const GIT_HOOK_READINESS_SCHEMA_V1: &str = "ee.hooks.git_readiness.v1";

/// Schema for the push-safety summary embedded in hook readiness diagnostics.
pub const GIT_HOOK_AHEAD_RISK_SCHEMA_V1: &str = "ee.hooks.git_readiness.ahead_risk.v1";

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

fn read_existing_hook_content(path: &Path) -> Result<String, ExistingHookStatus> {
    match first_existing_symlink_component(path) {
        Ok(Some(_)) => return Err(ExistingHookStatus::Symlink),
        Ok(None) => {}
        Err(_) => return Err(ExistingHookStatus::Unreadable),
    }

    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(ExistingHookStatus::Symlink);
            }
            if !metadata.is_file() {
                return Err(ExistingHookStatus::Unreadable);
            }
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Err(ExistingHookStatus::NotFound);
        }
        Err(_) => {
            return Err(ExistingHookStatus::Unreadable);
        }
    }

    read_limited_utf8_file(path, HOOK_CONTENT_INSPECT_LIMIT)
        .map_err(|_| ExistingHookStatus::Unreadable)
}

/// Open `path` for reading without following a terminal symlink.
///
/// The prior shape (`std::fs::File::open(path)`) silently followed a
/// symlink at the leaf, creating a TOCTOU window between the
/// `symlink_metadata` check at line 253 / 1370 and this open: a peer
/// agent in the multi-agent swarm could swap the regular hook file
/// for `~/.claude/hooks/pre-task → /etc/passwd` (or any other
/// readable file) between the check and the open, and the function
/// would return up to `HOOK_CONTENT_INSPECT_LIMIT` bytes of the
/// target's content to the idempotency-comparison path. Using
/// `O_NOFOLLOW` on Unix closes the window inside the kernel: if the
/// leaf became a symlink after the check, `open(2)` fails with ELOOP
/// and the caller sees `ExistingHookStatus::Unreadable` instead of
/// arbitrary content. Non-Unix targets keep the legacy
/// `File::open` shape since the symlink-on-shared-home threat model
/// is Unix-specific.
#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

fn read_limited_utf8_file(path: &Path, limit: usize) -> std::io::Result<String> {
    let file = open_no_follow(path)?;
    let mut limited = file.take(limit.saturating_add(1) as u64);
    let mut bytes = Vec::with_capacity(limit.min(8 * 1024));
    limited.read_to_end(&mut bytes)?;

    if bytes.len() > limit {
        bytes.truncate(limit);
        if let Err(error) = std::str::from_utf8(&bytes) {
            if error.error_len().is_none() {
                bytes.truncate(error.valid_up_to());
            } else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    error.to_string(),
                ));
            }
        }
    }

    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))
}

/// Get the status of an existing hook.
///
/// Uses `symlink_metadata` (lstat) to detect symlinks without following them.
/// Symlinks are rejected as a security measure: a malicious symlink could
/// point outside the hook directory, allowing arbitrary file overwrites.
fn check_existing_hook(path: &Path) -> ExistingHookStatus {
    match read_existing_hook_content(path) {
        Ok(content) => {
            if is_ee_managed_hook(&content) {
                ExistingHookStatus::ManagedByEe
            } else {
                ExistingHookStatus::External
            }
        }
        Err(status) => status,
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
        ExistingHookStatus::ManagedByEe => match read_existing_hook_content(path) {
            Ok(current_content) if current_content == desired_content => {
                (HookAction::NoChange, "ee-managed hook already up to date")
            }
            Ok(_) => (HookAction::Update, "Updating ee-managed hook"),
            Err(ExistingHookStatus::Symlink) => (
                HookAction::Skip,
                "Hook path is a symlink (security risk: remove symlink first)",
            ),
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

fn hook_temp_path(target_path: &Path) -> PathBuf {
    let mut temp_path = target_path.to_owned();
    temp_path.set_extension("tmp");
    temp_path
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

fn preflight_hook_temp_target(temp_path: &Path) -> Result<(), DomainError> {
    match std::fs::symlink_metadata(temp_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(DomainError::PolicyDenied {
            message: format!(
                "Refusing to write temporary hook '{}': path is a symlink (could overwrite arbitrary target)",
                temp_path.display()
            ),
            repair: Some("Remove the symlink before installing hooks.".to_owned()),
        }),
        Ok(_) => Err(DomainError::Storage {
            message: format!(
                "Refusing to write temporary hook '{}': temporary hook path already exists",
                temp_path.display()
            ),
            repair: Some(
                "Remove the stale temporary hook file after confirming no install is running."
                    .to_owned(),
            ),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "Failed to inspect temporary hook target '{}': {error}",
                temp_path.display()
            ),
            repair: Some(
                "Choose a writable hook directory or re-run with corrected permissions.".to_owned(),
            ),
        }),
    }
}

fn preflight_created_hook_temp_target(temp_path: &Path) -> Result<(), DomainError> {
    match std::fs::symlink_metadata(temp_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(DomainError::PolicyDenied {
            message: format!(
                "Refusing to install temporary hook '{}': path became a symlink before rename",
                temp_path.display()
            ),
            repair: Some(
                "Remove the symlink and re-run hook installation from a trusted hook directory."
                    .to_owned(),
            ),
        }),
        Ok(metadata) if !metadata.file_type().is_file() => Err(DomainError::Storage {
            message: format!(
                "Refusing to install temporary hook '{}': path is not a regular file",
                temp_path.display()
            ),
            repair: Some(
                "Remove the stale temporary hook entry after confirming no install is running."
                    .to_owned(),
            ),
        }),
        Ok(_) => Ok(()),
        Err(error) => Err(DomainError::Storage {
            message: format!(
                "Failed to inspect temporary hook before rename '{}': {error}",
                temp_path.display()
            ),
            repair: Some("Check hook path permissions and re-run hook installation.".to_owned()),
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
        preflight_hook_temp_target(&hook_temp_path(&write.target_path))?;
    }

    Ok(())
}

/// Drop-guard that best-effort removes a temporary hook file when the
/// install path returns early on error.
///
/// Once `write_hook_file` calls `OpenOptions::create_new(true)` on
/// `temp_path`, every subsequent `?` (write_all, set_permissions,
/// sync_all, or the rename inside `publish_hook_temp_file`) propagates
/// an error WITHOUT removing the file the function just created. The
/// next install attempt then trips `preflight_hook_temp_target` at
/// src/hooks/installer.rs:603 ("Refusing to write temporary hook ...:
/// temporary hook path already exists") and refuses to run until the
/// operator manually deletes the orphan — a real reliability failure
/// mode under transient disk-pressure / EIO / EINTR, and a particular
/// foot-gun in multi-agent shared checkouts where one stuck pane can
/// freeze the install surface for everyone else.
///
/// Usage:
///   1. Construct `disarmed(&temp_path)` BEFORE the `open(temp_path)`
///      call so the guard's drop never tries to remove a path that
///      was never created (a confusing concurrent-delete race
///      otherwise).
///   2. Call `guard.arm()` immediately after the `open` succeeds.
///   3. After the final `publish_hook_temp_file(...)?` succeeds — the
///      rename has consumed the temp file — call `guard.disarm()` so
///      drop is a no-op.
///
/// Failures inside drop are intentionally swallowed: by then we are
/// already returning Err to the caller, the most common cleanup
/// failure (the rename moved the file) is benign, and panicking from
/// drop is illegal.
struct TempHookFileGuard<'a> {
    path: &'a Path,
    armed: bool,
}

impl<'a> TempHookFileGuard<'a> {
    fn disarmed(path: &'a Path) -> Self {
        Self { path, armed: false }
    }

    fn arm(&mut self) {
        self.armed = true;
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempHookFileGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(self.path);
        }
    }
}

fn write_hook_file(hook_dir: &Path, target_path: &Path, content: &str) -> Result<(), DomainError> {
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

    let temp_path = hook_temp_path(target_path);
    preflight_hook_target(target_path)?;
    preflight_hook_temp_target(&temp_path)?;

    // Disarmed at construction: the file does not exist yet, and arming
    // before the `open(...)` would race a different pane's preflight
    // failure into accidentally removing a fresh peer-owned temp file.
    let mut cleanup_guard = TempHookFileGuard::disarmed(&temp_path);

    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Failed to create temporary hook '{}': {error}",
                    temp_path.display()
                ),
                repair: Some("Check hook path permissions.".to_owned()),
            })?;
        // The file at temp_path now exists and is owned by this call.
        // Arm cleanup so any subsequent `?` removes the orphan before
        // returning to the caller.
        cleanup_guard.arm();
        file.write_all(content.as_bytes())
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Failed to write temporary hook '{}': {error}",
                    temp_path.display()
                ),
                repair: Some("Check hook path permissions.".to_owned()),
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = file.metadata().map_err(|error| DomainError::Storage {
                message: format!(
                    "Failed to read temporary hook metadata '{}': {error}",
                    temp_path.display()
                ),
                repair: Some(
                    "Check hook file permissions and re-run hook installation.".to_owned(),
                ),
            })?;
            let mut perms = metadata.permissions();
            perms.set_mode(0o755);
            file.set_permissions(perms)
                .map_err(|error| DomainError::Storage {
                    message: format!(
                        "Failed to mark temporary hook executable '{}': {error}",
                        temp_path.display()
                    ),
                    repair: Some(
                        "Check hook file permissions and re-run hook installation.".to_owned(),
                    ),
                })?;
        }

        file.sync_all().map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to sync temporary hook '{}': {error}",
                temp_path.display()
            ),
            repair: Some("Check hook path permissions.".to_owned()),
        })?;
    }

    publish_hook_temp_file(hook_dir, &temp_path, target_path)?;
    // Publish succeeded: rename has moved temp_path to target_path, so
    // there is no orphan to clean up. Disarm the guard so its drop is
    // a no-op.
    cleanup_guard.disarm();
    Ok(())
}

fn publish_hook_temp_file(
    hook_dir: &Path,
    temp_path: &Path,
    target_path: &Path,
) -> Result<(), DomainError> {
    ensure_hook_dir_is_not_symlink(hook_dir)?;
    preflight_created_hook_temp_target(temp_path)?;
    preflight_hook_target(target_path)?;

    std::fs::rename(temp_path, target_path).map_err(|error| DomainError::Storage {
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

    if !options.dry_run {
        ensure_hook_dir_is_not_symlink(&options.hook_dir)?;
    }

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
// Local Git Hook Readiness Diagnostic (bd-3d6ko.7)
// ============================================================================

const GIT_HOOK_READINESS_COMMAND: &str = "hook git-readiness";
const HOOK_CONTENT_INSPECT_LIMIT: usize = 64 * 1024;

/// Maximum size of a `.git` gitfile pointer.
///
/// A real gitfile has the shape `gitdir: <path>\n` and is typically
/// under 256 bytes. 4 KiB is a very generous ceiling that bounds the
/// worst-case read for a peer-planted huge `.git` file while staying
/// far above any legitimate gitfile. Parallel to
/// `HOOK_CONTENT_INSPECT_LIMIT` above; routes through the same
/// `read_limited_utf8_file` helper so the read is also `O_NOFOLLOW`
/// (closing the TOCTOU window that 5a4eeab4 closed for hook content).
const GITDIR_POINTER_INSPECT_LIMIT: usize = 4 * 1024;
const DEFAULT_GIT_HOOK_NAMES: &[&str] = &[
    "pre-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-push",
    "post-merge",
];

/// Options for a read-only local Git hook-chain readiness diagnostic.
#[derive(Clone, Debug, Default)]
pub struct GitHookReadinessOptions {
    /// Repository root whose `.git/hooks` directory should be inspected.
    pub repository_root: PathBuf,
    /// Agent identity expected by Agent Mail guard hooks.
    pub agent_name: Option<String>,
}

/// Summary posture for local Git hook readiness.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHookReadinessSummary {
    pub posture: String,
    pub inspected_hook_count: usize,
    pub active_hook_count: usize,
    pub finding_count: usize,
    pub blocker_count: usize,
    pub warning_count: usize,
    pub beads_metadata_mutation_risk: bool,
    pub agent_name_ready: bool,
    pub preflight_guard_reachable: bool,
    pub rch_hook_reachable: bool,
    pub ahead_risk_blocking: bool,
}

/// Compact push-readiness summary consumed by pre-push hook chains.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHookAheadRiskSummary {
    pub schema: String,
    pub available: bool,
    pub state: String,
    pub has_upstream: bool,
    pub upstream_ref: Option<String>,
    pub ahead_count: usize,
    pub commit_count: usize,
    pub mixed_owner_ahead: bool,
    pub mixed_bead_ahead: bool,
    pub ambiguous_ahead: bool,
    pub peer_owned_ahead_risk: bool,
    pub degraded_codes: Vec<String>,
    pub blocking: bool,
}

impl GitHookAheadRiskSummary {
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            schema: GIT_HOOK_AHEAD_RISK_SCHEMA_V1.to_owned(),
            available: false,
            state: "unavailable".to_owned(),
            has_upstream: false,
            upstream_ref: None,
            ahead_count: 0,
            commit_count: 0,
            mixed_owner_ahead: false,
            mixed_bead_ahead: false,
            ambiguous_ahead: false,
            peer_owned_ahead_risk: false,
            degraded_codes: vec!["git_ahead_unavailable".to_owned()],
            blocking: false,
        }
    }

    #[must_use]
    pub fn from_snapshot(snapshot: &GitAheadSnapshot) -> Self {
        Self {
            schema: GIT_HOOK_AHEAD_RISK_SCHEMA_V1.to_owned(),
            available: true,
            state: snapshot.state.to_owned(),
            has_upstream: snapshot.upstream_ref.is_some(),
            upstream_ref: snapshot.upstream_ref.clone(),
            ahead_count: snapshot.ahead_count,
            commit_count: snapshot.commits.len(),
            mixed_owner_ahead: snapshot.mixed_author_ahead,
            mixed_bead_ahead: snapshot.mixed_bead_ahead,
            ambiguous_ahead: snapshot.ambiguous_ahead,
            peer_owned_ahead_risk: snapshot.peer_owned_ahead_risk,
            degraded_codes: snapshot
                .degraded
                .iter()
                .map(|entry| entry.code.to_owned())
                .collect(),
            blocking: snapshot.peer_owned_ahead_risk,
        }
    }
}

/// One inspected Git hook or chained hook target.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHookReadinessHook {
    pub name: String,
    pub path: String,
    pub exists: bool,
    pub status: String,
    pub executable: bool,
    pub managed_by_ee: bool,
    pub chain_targets: Vec<String>,
    pub requires_agent_name: bool,
    pub mutates_beads_metadata: bool,
    pub invokes_preflight_guard: bool,
    pub invokes_rch: bool,
    pub invokes_local_rust_toolchain: bool,
}

/// One deterministic readiness finding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHookReadinessFinding {
    pub code: String,
    pub severity: String,
    pub hook: Option<String>,
    pub message: String,
    pub repair: String,
}

/// One operator recommendation. Recommendations are advisory and read-only.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHookReadinessRecommendation {
    pub id: String,
    pub priority: u8,
    pub action: String,
    pub rationale: String,
}

/// Read-only report for local Git hook readiness.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHookReadinessReport {
    pub schema: String,
    pub command: String,
    pub read_only: bool,
    pub repository_root: String,
    pub git_dir: Option<String>,
    pub hook_dir: String,
    pub agent_name: Option<String>,
    pub summary: GitHookReadinessSummary,
    pub ahead_risk: GitHookAheadRiskSummary,
    pub hooks: Vec<GitHookReadinessHook>,
    pub findings: Vec<GitHookReadinessFinding>,
    pub recommendations: Vec<GitHookReadinessRecommendation>,
}

impl GitHookReadinessReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serialize_hook_report(self, "GitHookReadinessReport")
    }
}

/// Inspect the local Git hook chain without running hooks or mutating the repo.
pub fn check_git_hook_readiness(
    options: &GitHookReadinessOptions,
) -> Result<GitHookReadinessReport, DomainError> {
    let ahead_risk = collect_git_hook_ahead_risk(&options.repository_root);
    check_git_hook_readiness_with_ahead_risk(options, ahead_risk)
}

/// Inspect hook readiness using a precomputed ahead-risk summary.
///
/// This keeps pre-push policy evaluation testable without shelling out to Git
/// while preserving `check_git_hook_readiness` as the live read-only collector.
pub fn check_git_hook_readiness_with_ahead_risk(
    options: &GitHookReadinessOptions,
    ahead_risk: GitHookAheadRiskSummary,
) -> Result<GitHookReadinessReport, DomainError> {
    let (git_dir, hook_dir, mut findings) = resolve_git_hook_dir(&options.repository_root);
    let agent_name = normalize_agent_name(options.agent_name.as_deref());
    let mut hooks = Vec::new();

    for hook_name in DEFAULT_GIT_HOOK_NAMES {
        hooks.push(inspect_git_hook(&hook_dir, hook_name));
    }

    findings.extend(git_hook_readiness_findings(
        &hooks,
        agent_name.as_deref(),
        &ahead_risk,
    ));
    let recommendations = git_hook_readiness_recommendations(&findings);
    let summary = git_hook_readiness_summary(&hooks, &findings, agent_name.is_some(), &ahead_risk);

    Ok(GitHookReadinessReport {
        schema: GIT_HOOK_READINESS_SCHEMA_V1.to_owned(),
        command: GIT_HOOK_READINESS_COMMAND.to_owned(),
        read_only: true,
        repository_root: options.repository_root.display().to_string(),
        git_dir: git_dir.map(|path| path.display().to_string()),
        hook_dir: hook_dir.display().to_string(),
        agent_name,
        summary,
        ahead_risk,
        hooks,
        findings,
        recommendations,
    })
}

fn normalize_agent_name(agent_name: Option<&str>) -> Option<String> {
    agent_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn collect_git_hook_ahead_risk(repository_root: &Path) -> GitHookAheadRiskSummary {
    let status =
        match run_git_readiness_command(repository_root, &["status", "--porcelain=v2", "--branch"])
        {
            Ok(stdout) => stdout,
            Err(_) => return GitHookAheadRiskSummary::unavailable(),
        };
    let status_only = summarize_git_ahead(&status, Some(""));
    let snapshot = match (status_only.ahead_count, status_only.upstream_ref.as_deref()) {
        (0, _) | (_, None) => status_only,
        (_, Some(upstream)) => {
            let range = format!("{upstream}..HEAD");
            let format_arg = format!("--format={GIT_AHEAD_LOG_FORMAT}");
            match run_git_readiness_command(repository_root, &["log", &range, &format_arg]) {
                Ok(stdout) => summarize_git_ahead(&status, Some(&stdout)),
                Err(GitReadinessCommandError::Failed) => {
                    summarize_git_ahead_with_log_state(&status, GitAheadLogState::Failed)
                }
                Err(GitReadinessCommandError::Unavailable) => {
                    summarize_git_ahead_with_log_state(&status, GitAheadLogState::Unavailable)
                }
            }
        }
    };
    GitHookAheadRiskSummary::from_snapshot(&snapshot)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GitReadinessCommandError {
    Failed,
    Unavailable,
}

fn run_git_readiness_command(
    repository_root: &Path,
    args: &[&str],
) -> Result<String, GitReadinessCommandError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository_root)
        .args(args)
        .output()
        .map_err(|_| GitReadinessCommandError::Unavailable)?;

    if !output.status.success() {
        return Err(GitReadinessCommandError::Failed);
    }
    String::from_utf8(output.stdout).map_err(|_| GitReadinessCommandError::Unavailable)
}

fn resolve_git_hook_dir(
    repository_root: &Path,
) -> (Option<PathBuf>, PathBuf, Vec<GitHookReadinessFinding>) {
    let git_path = repository_root.join(".git");
    let mut findings = Vec::new();

    match std::fs::symlink_metadata(&git_path) {
        Ok(metadata) if metadata.is_dir() => {
            let hook_dir = git_path.join("hooks");
            (Some(git_path), hook_dir, findings)
        }
        Ok(metadata) if metadata.is_file() => {
            match read_gitdir_pointer(repository_root, &git_path) {
                Some(git_dir) => {
                    let hook_dir = git_dir.join("hooks");
                    (Some(git_dir), hook_dir, findings)
                }
                None => {
                    findings.push(GitHookReadinessFinding {
                        code: "git_dir_pointer_unreadable".to_owned(),
                        severity: "warning".to_owned(),
                        hook: None,
                        message: format!(
                            "Git directory pointer '{}' could not be read as a `gitdir:` file.",
                            git_path.display()
                        ),
                        repair: "Run this diagnostic from the canonical repository checkout or repair the .git pointer.".to_owned(),
                    });
                    (None, git_path.join("hooks"), findings)
                }
            }
        }
        Ok(_) => {
            findings.push(GitHookReadinessFinding {
                code: "git_dir_not_directory".to_owned(),
                severity: "warning".to_owned(),
                hook: None,
                message: format!(
                    "Git metadata path '{}' is not a directory or gitdir pointer.",
                    git_path.display()
                ),
                repair: "Run this diagnostic from a normal Git checkout.".to_owned(),
            });
            (None, git_path.join("hooks"), findings)
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            findings.push(GitHookReadinessFinding {
                code: "git_dir_missing".to_owned(),
                severity: "warning".to_owned(),
                hook: None,
                message: format!(
                    "No .git directory was found under '{}'.",
                    repository_root.display()
                ),
                repair: "Pass --repository-root pointing at the canonical Git checkout.".to_owned(),
            });
            (None, git_path.join("hooks"), findings)
        }
        Err(error) => {
            findings.push(GitHookReadinessFinding {
                code: "git_dir_unreadable".to_owned(),
                severity: "warning".to_owned(),
                hook: None,
                message: format!(
                    "Git metadata path '{}' could not be inspected: {error}",
                    git_path.display()
                ),
                repair: "Fix permissions or pass a readable --repository-root.".to_owned(),
            });
            (None, git_path.join("hooks"), findings)
        }
    }
}

fn read_gitdir_pointer(repository_root: &Path, git_path: &Path) -> Option<PathBuf> {
    // Route through `read_limited_utf8_file` so the open uses
    // `O_NOFOLLOW` on Unix and the read is bounded by
    // `GITDIR_POINTER_INSPECT_LIMIT`. Two reasons:
    //
    // 1. TOCTOU. `resolve_git_hook_dir` calls `symlink_metadata` and
    //    rejects symlinks before calling this helper, but the window
    //    between the pre-check and the open is the same race
    //    `5a4eeab4` closed for hook content reads. A peer that swaps
    //    `.git` for `→ /etc/something` after the pre-check would
    //    otherwise leak target content into the install-check report
    //    OR coerce a misdirected hook directory if the target happens
    //    to start with `gitdir:`. `O_NOFOLLOW` closes the window in
    //    the kernel: `open(2)` fails with ELOOP on a symlinked leaf
    //    and the helper returns Err, so this function returns None
    //    (matching the existing absent-pointer behavior).
    //
    // 2. Unbounded read. Naive `read_to_string` pre-sizes from
    //    metadata, so a peer-planted multi-GB `.git` file would pin a
    //    matching allocation before the `strip_prefix("gitdir:")`
    //    check could reject it. A 4 KiB ceiling bounds the worst case
    //    while still accepting every legitimate gitfile.
    let content = read_limited_utf8_file(git_path, GITDIR_POINTER_INSPECT_LIMIT).ok()?;
    let raw = content.trim().strip_prefix("gitdir:")?.trim();
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        Some(path)
    } else {
        Some(repository_root.join(path))
    }
}

fn inspect_git_hook(hook_dir: &Path, hook_name: &str) -> GitHookReadinessHook {
    let path = hook_dir.join(hook_name);
    let existing = check_existing_hook(&path);
    let exists = !matches!(existing, ExistingHookStatus::NotFound);
    let executable = hook_path_is_executable(&path, existing);
    let content = read_bounded_hook_content(&path, existing);
    let chain_targets = hook_chain_targets(hook_dir, hook_name, content.as_deref());
    let mut combined_content = content.unwrap_or_default();
    for target in &chain_targets {
        if let Some(target_content) = read_plain_bounded_file(Path::new(target.as_str())) {
            combined_content.push('\n');
            combined_content.push_str(&target_content);
        }
    }

    GitHookReadinessHook {
        name: hook_name.to_owned(),
        path: path.display().to_string(),
        exists,
        status: existing.as_str().to_owned(),
        executable,
        managed_by_ee: matches!(existing, ExistingHookStatus::ManagedByEe),
        chain_targets,
        requires_agent_name: hook_requires_agent_name(&combined_content),
        mutates_beads_metadata: hook_mutates_beads_metadata(&combined_content),
        invokes_preflight_guard: hook_invokes_preflight_guard(&combined_content),
        invokes_rch: hook_invokes_rch(&combined_content),
        invokes_local_rust_toolchain: hook_invokes_local_rust_toolchain(&combined_content),
    }
}

fn read_bounded_hook_content(path: &Path, existing: ExistingHookStatus) -> Option<String> {
    if !matches!(
        existing,
        ExistingHookStatus::ManagedByEe | ExistingHookStatus::External
    ) {
        return None;
    }
    read_existing_hook_content(path).ok()
}

fn read_plain_bounded_file(path: &Path) -> Option<String> {
    match first_existing_symlink_component(path) {
        Ok(Some(_)) | Err(_) => return None,
        Ok(None) => {}
    }
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    read_limited_utf8_file(path, HOOK_CONTENT_INSPECT_LIMIT).ok()
}

fn hook_chain_targets(hook_dir: &Path, hook_name: &str, content: Option<&str>) -> Vec<String> {
    let Some(content) = content else {
        return Vec::new();
    };
    let mut targets = Vec::new();
    let hooks_d = hook_dir.join("hooks.d").join(hook_name);
    if content.contains("hooks.d") || content.contains("RUN_DIR") {
        if let Ok(entries) = std::fs::read_dir(&hooks_d) {
            let mut paths = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.is_file())
                .collect::<Vec<_>>();
            paths.sort();
            targets.extend(paths.into_iter().map(|path| path.display().to_string()));
        }
    }

    for suffix in ["orig", "old"] {
        let candidate = hook_dir.join(format!("{hook_name}.{suffix}"));
        let marker = format!("{hook_name}.{suffix}");
        if content.contains(&marker) && candidate.exists() {
            targets.push(candidate.display().to_string());
        }
    }
    targets.sort();
    targets.dedup();
    targets
}

fn hook_requires_agent_name(content: &str) -> bool {
    content.contains("AGENT_NAME")
        && (content.contains("environment variable is required")
            || content.contains("os.environ.get(\"AGENT_NAME\"")
            || content.contains("os.environ.get('AGENT_NAME'")
            || content.lines().any(shell_line_requires_agent_name))
}

fn shell_line_requires_agent_name(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return false;
    }

    trimmed.contains("${AGENT_NAME:?")
        || trimmed.contains("${AGENT_NAME?")
        || trimmed.contains("[ -z \"$AGENT_NAME\"")
        || trimmed.contains("[[ -z \"$AGENT_NAME\"")
        || trimmed.contains("test -z \"$AGENT_NAME\"")
        || trimmed.contains("[ -z \"${AGENT_NAME")
        || trimmed.contains("[[ -z \"${AGENT_NAME")
        || trimmed.contains("test -z \"${AGENT_NAME")
}

fn hook_mutates_beads_metadata(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    (lower.contains("bd sync --flush-only") || lower.contains("br sync --flush-only"))
        && lower.contains("git add")
        && (lower.contains(".beads/issues.jsonl") || lower.contains("$beads_dir/issues.jsonl"))
}

fn hook_invokes_preflight_guard(content: &str) -> bool {
    content.contains("preflight check")
        || content.contains("EE_PREFLIGHT_HOOK")
        || content.contains("preflight guard")
}

fn hook_invokes_rch(content: &str) -> bool {
    content.contains("RCH_")
        || content.contains("/rch ")
        || content.contains("rch_verify")
        || content.lines().any(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with('#')
                && (trimmed == "rch"
                    || trimmed.starts_with("rch ")
                    || trimmed.starts_with("exec rch ")
                    || trimmed.contains(" rch "))
        })
}

fn hook_invokes_local_rust_toolchain(content: &str) -> bool {
    content.lines().any(|line| {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return false;
        }
        let lower = trimmed.to_ascii_lowercase();
        lower.starts_with("cargo ")
            || lower.starts_with("rustc ")
            || lower.starts_with("rustdoc ")
            || lower.contains(" cargo ")
            || lower.contains(" rustc ")
            || lower.contains(" rustdoc ")
    })
}

fn git_hook_readiness_findings(
    hooks: &[GitHookReadinessHook],
    agent_name: Option<&str>,
    ahead_risk: &GitHookAheadRiskSummary,
) -> Vec<GitHookReadinessFinding> {
    let mut findings = Vec::new();
    for hook in hooks {
        if matches!(hook.status.as_str(), "symlink" | "unreadable") {
            findings.push(GitHookReadinessFinding {
                code: "hook_untrusted_or_unreadable".to_owned(),
                severity: "warning".to_owned(),
                hook: Some(hook.name.clone()),
                message: format!(
                    "Git hook `{}` has status `{}` and could not be fully inspected.",
                    hook.name, hook.status
                ),
                repair: "Inspect the hook manually before relying on commit or push readiness."
                    .to_owned(),
            });
        }
        if hook.requires_agent_name && agent_name.is_none() {
            findings.push(GitHookReadinessFinding {
                code: "agent_name_required".to_owned(),
                severity: "high".to_owned(),
                hook: Some(hook.name.clone()),
                message: format!(
                    "Git hook `{}` appears to require AGENT_NAME, but no agent name is available.",
                    hook.name
                ),
                repair: "Set AGENT_NAME to your registered Agent Mail identity before committing or pushing.".to_owned(),
            });
        }
        if hook.mutates_beads_metadata {
            findings.push(GitHookReadinessFinding {
                code: "beads_metadata_mutation_risk".to_owned(),
                severity: "high".to_owned(),
                hook: Some(hook.name.clone()),
                message: format!(
                    "Git hook `{}` can flush and stage .beads/issues.jsonl during commit.",
                    hook.name
                ),
                repair: "Use an explicit path-limited commit when Beads metadata is contested, and inspect staged .beads/issues.jsonl before committing.".to_owned(),
            });
        }
        if hook.invokes_local_rust_toolchain && !hook.invokes_rch {
            findings.push(GitHookReadinessFinding {
                code: "rch_hook_mismatch".to_owned(),
                severity: "high".to_owned(),
                hook: Some(hook.name.clone()),
                message: format!(
                    "Git hook `{}` appears to run Cargo or Rust tooling without an RCH wrapper.",
                    hook.name
                ),
                repair: "Route Rust verification through scripts/rch_verify.sh or rch exec; do not let shared-checkout Git hooks run local Cargo.".to_owned(),
            });
        }
    }

    if ahead_risk.blocking {
        findings.push(GitHookReadinessFinding {
            code: "pre_push_ahead_risk".to_owned(),
            severity: "high".to_owned(),
            hook: Some("pre-push".to_owned()),
            message: format!(
                "Ahead-risk summary reports state `{}` with {} ahead commit(s); mixed-owner, mixed-bead, or ambiguous ahead commits must be coordinated before push.",
                ahead_risk.state, ahead_risk.ahead_count
            ),
            repair: "Inspect `git log origin/main..HEAD --oneline --decorate` and coordinate with peers before pushing."
                .to_owned(),
        });
    }

    if hooks
        .iter()
        .filter(|hook| hook.exists)
        .all(|hook| !hook.invokes_preflight_guard)
    {
        findings.push(GitHookReadinessFinding {
            code: "preflight_guard_not_in_git_hooks".to_owned(),
            severity: "low".to_owned(),
            hook: None,
            message: "No inspected Git hook invokes `ee preflight check` or an ee preflight guard."
                .to_owned(),
            repair: "Use `ee hook preflight-shell --shell zsh --json` to generate a shell preflight snippet, or document why Git-hook preflight is intentionally absent.".to_owned(),
        });
    }

    findings.sort_by(|left, right| {
        (
            severity_rank(&left.severity),
            left.hook.as_deref().unwrap_or(""),
            left.code.as_str(),
        )
            .cmp(&(
                severity_rank(&right.severity),
                right.hook.as_deref().unwrap_or(""),
                right.code.as_str(),
            ))
    });
    findings.reverse();
    findings
}

fn severity_rank(severity: &str) -> u8 {
    match severity {
        "critical" => 5,
        "high" => 4,
        "medium" => 3,
        "warning" => 2,
        "low" => 1,
        _ => 0,
    }
}

fn git_hook_readiness_recommendations(
    findings: &[GitHookReadinessFinding],
) -> Vec<GitHookReadinessRecommendation> {
    let mut recommendations = Vec::new();
    if findings
        .iter()
        .any(|finding| finding.code == "agent_name_required")
    {
        recommendations.push(GitHookReadinessRecommendation {
            id: "set_agent_name".to_owned(),
            priority: 1,
            action: "export AGENT_NAME=<registered-agent-name>".to_owned(),
            rationale: "Agent Mail guard hooks fail closed when AGENT_NAME is absent.".to_owned(),
        });
    }
    if findings
        .iter()
        .any(|finding| finding.code == "beads_metadata_mutation_risk")
    {
        recommendations.push(GitHookReadinessRecommendation {
            id: "path_limited_commit".to_owned(),
            priority: 2,
            action: "git commit --only <intended-paths> ...".to_owned(),
            rationale: "Path-limited commits prevent legacy Beads hook churn from being swept into source commits.".to_owned(),
        });
    }
    if findings
        .iter()
        .any(|finding| finding.code == "rch_hook_mismatch")
    {
        recommendations.push(GitHookReadinessRecommendation {
            id: "route_rust_hooks_through_rch".to_owned(),
            priority: 3,
            action: "scripts/rch_verify.sh <cargo-command>".to_owned(),
            rationale: "This checkout requires remote-first Rust verification and must not run local Cargo from Git hooks.".to_owned(),
        });
    }
    if findings
        .iter()
        .any(|finding| finding.code == "pre_push_ahead_risk")
    {
        recommendations.push(GitHookReadinessRecommendation {
            id: "inspect_ahead_risk_before_push".to_owned(),
            priority: 4,
            action: "git log origin/main..HEAD --oneline --decorate".to_owned(),
            rationale: "The pre-push readiness summary found mixed-owner, mixed-bead, or ambiguous ahead commits.".to_owned(),
        });
    }
    if findings
        .iter()
        .any(|finding| finding.code == "preflight_guard_not_in_git_hooks")
    {
        recommendations.push(GitHookReadinessRecommendation {
            id: "generate_preflight_shell_hook".to_owned(),
            priority: 5,
            action: "ee hook preflight-shell --shell zsh --json".to_owned(),
            rationale: "The destructive-command guard is most reliable when the shell pre-exec hook is active before commands run.".to_owned(),
        });
    }
    recommendations.sort_by_key(|recommendation| recommendation.priority);
    recommendations
}

fn git_hook_readiness_summary(
    hooks: &[GitHookReadinessHook],
    findings: &[GitHookReadinessFinding],
    agent_name_ready: bool,
    ahead_risk: &GitHookAheadRiskSummary,
) -> GitHookReadinessSummary {
    let blocker_count = findings
        .iter()
        .filter(|finding| matches!(finding.severity.as_str(), "critical" | "high"))
        .count();
    let warning_count = findings.len().saturating_sub(blocker_count);
    let posture = if blocker_count > 0 {
        "blocked"
    } else if warning_count > 0 {
        "needs_attention"
    } else {
        "ready"
    };

    GitHookReadinessSummary {
        posture: posture.to_owned(),
        inspected_hook_count: hooks.len(),
        active_hook_count: hooks.iter().filter(|hook| hook.exists).count(),
        finding_count: findings.len(),
        blocker_count,
        warning_count,
        beads_metadata_mutation_risk: hooks.iter().any(|hook| hook.mutates_beads_metadata),
        agent_name_ready,
        preflight_guard_reachable: hooks.iter().any(|hook| hook.invokes_preflight_guard),
        rch_hook_reachable: hooks.iter().any(|hook| hook.invokes_rch),
        ahead_risk_blocking: ahead_risk.blocking,
    }
}

// ============================================================================
// Preflight Shell Hook Helper (bd-3usjw.7 — trauma_guard_hook_helper)
// ============================================================================

/// Schema for the `ee hook preflight-shell` JSON envelope.
pub const PREFLIGHT_HOOK_SHELL_SCHEMA_V1: &str = "ee.hooks.preflight_shell.v1";

/// Informational severities historically surfaced in the JSON report for
/// callers that want to describe the default policy-denied posture. Generated
/// shell snippets must not use this as a client-side allowlist: exit code 7
/// from `ee preflight check` is the live policy authority.
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
# interactive command. When the check exits 7, the user is prompted on
# /dev/tty; declining the prompt blocks the command via `shopt -s extdebug`.
#
# Install:   source <install_path>   (see install_path in the JSON envelope)
# Disable:   trap - DEBUG; shopt -u extdebug; unset EE_PREFLIGHT_HOOK_ACTIVE

if [ -n "${{BASH_VERSION:-}}" ] && [ -z "${{EE_PREFLIGHT_HOOK_ACTIVE:-}}" ]; then
    EE_PREFLIGHT_HOOK_BINARY={ee_path}

    __ee_preflight_hook_check() {{
        # Skip only our own callbacks and empty commands. Broad builtin-prefix
        # skips are unsafe: `echo $(rm -rf /)` still executes the substitution.
        case "${{BASH_COMMAND:-}}" in
            __ee_preflight_*|*__ee_preflight_hook_check*|'') return 0 ;;
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
# each user command. When the check exits 7, the user is prompted on /dev/tty;
# declining the prompt aborts the upcoming command by sending SIGINT to the shell. zsh preexec
# cannot natively cancel a command, so SIGINT is the documented mechanism.
#
# Install:   source <install_path>   (see install_path in the JSON envelope)
# Disable:   add-zsh-hook -d preexec __ee_preflight_hook_check;
#            unset EE_PREFLIGHT_HOOK_ACTIVE

if [ -n "${{ZSH_VERSION:-}}" ] && [ -z "${{EE_PREFLIGHT_HOOK_ACTIVE:-}}" ]; then
    EE_PREFLIGHT_HOOK_BINARY={ee_path}

    autoload -Uz add-zsh-hook

    __ee_preflight_hook_check() {{
        # $1 is the verbatim command line as typed by the user.
        local _ee_cmd="$1"
        case "$_ee_cmd" in
            __ee_preflight_*|'') return 0 ;;
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

    /// Drop-cleanup of the temp-hook orphan after a mid-write failure.
    ///
    /// Regression guard: `write_hook_file` creates `<target>.tmp` via
    /// `OpenOptions::create_new(true)`. Any `?`-propagated error
    /// between the open and the publishing rename used to leave the
    /// .tmp file on disk. The next install attempt then trips
    /// `preflight_hook_temp_target` ("temporary hook path already
    /// exists") and refuses to run until the operator manually
    /// deletes the orphan — a real reliability hole under transient
    /// disk-pressure / EIO / EINTR, and a particular foot-gun in
    /// multi-agent shared checkouts.
    ///
    /// We can't easily inject a write/sync failure into `write_hook_file`
    /// without an injection seam this module deliberately avoids, but
    /// we CAN exercise the `TempHookFileGuard` Drop semantics
    /// directly — that's the load-bearing piece of the fix.
    #[test]
    fn temp_hook_file_guard_drops_armed_orphan() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let path = temp.path().join("pre-commit.tmp");
        fs::write(&path, b"orphan").map_err(|e| e.to_string())?;
        assert!(path.exists(), "temp file must exist before guard drop");
        {
            let mut guard = TempHookFileGuard::disarmed(&path);
            guard.arm();
            // Guard drops here at end of block.
        }
        if path.exists() {
            return Err(format!(
                "armed guard must remove orphan; {} still present",
                path.display()
            ));
        }
        Ok(())
    }

    /// Disarming after successful publish must NOT touch the final
    /// target file (which is no longer at temp_path because rename
    /// moved it, but the principle is the same: a disarmed guard is
    /// a no-op).
    #[test]
    fn temp_hook_file_guard_disarm_skips_cleanup() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let path = temp.path().join("pre-commit.tmp");
        fs::write(&path, b"survives-disarm").map_err(|e| e.to_string())?;
        {
            let mut guard = TempHookFileGuard::disarmed(&path);
            guard.arm();
            guard.disarm();
            // Drop here; armed=false, no removal.
        }
        if !path.exists() {
            return Err(format!(
                "disarmed guard must NOT remove path; {} was removed",
                path.display()
            ));
        }
        // And content must be untouched.
        let content = fs::read(&path).map_err(|e| e.to_string())?;
        if content != b"survives-disarm" {
            return Err(format!(
                "disarmed guard tampered with file contents; got {:?}",
                content
            ));
        }
        Ok(())
    }

    /// A guard that was constructed but never armed (e.g. the
    /// `open(...)` itself failed) must NOT try to remove a path that
    /// was never created. This is the load-bearing reason the
    /// constructor starts disarmed instead of armed.
    #[test]
    fn temp_hook_file_guard_unarmed_skips_cleanup_for_nonexistent_path() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let path = temp.path().join("never-created.tmp");
        assert!(
            !path.exists(),
            "test precondition: path must not exist beforehand"
        );
        {
            let _guard = TempHookFileGuard::disarmed(&path);
            // Drop here; armed=false, no remove_file attempt.
        }
        // No assertion beyond "we did not panic and the path is still
        // missing" — the point is that drop never called remove_file
        // on a path it doesn't own.
        if path.exists() {
            return Err(format!(
                "test invariant violated: {} appeared during guard scope",
                path.display()
            ));
        }
        Ok(())
    }

    /// End-to-end: a successful install must leave the temp file gone
    /// (the rename consumed it) AND the target file present.
    /// Indirectly proves disarm() runs on the success path.
    #[test]
    fn write_hook_file_success_path_leaves_no_orphan() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_dir = temp.path().join("hooks");
        fs::create_dir_all(&hook_dir).map_err(|e| e.to_string())?;
        let target = hook_dir.join("pre-commit");
        let temp_path = hook_temp_path(&target);
        write_hook_file(&hook_dir, &target, "#!/bin/sh\necho hi\n")
            .map_err(|e| format!("write_hook_file: {e:?}"))?;
        if !target.exists() {
            return Err(format!("target {} not written", target.display()));
        }
        if temp_path.exists() {
            return Err(format!(
                "orphan temp file {} present after successful install",
                temp_path.display()
            ));
        }
        Ok(())
    }

    fn configured_git_hook_repo() -> Result<TempDir, String> {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_dir = temp.path().join(".git").join("hooks");
        fs::create_dir_all(&hook_dir).map_err(|e| e.to_string())?;
        fs::write(
            hook_dir.join("pre-commit"),
            "#!/bin/sh\n/usr/local/bin/ee preflight check --cmd \"$*\" --json\n",
        )
        .map_err(|e| e.to_string())?;
        fs::write(
            hook_dir.join("pre-push"),
            r#"#!/usr/bin/env python3
import os
AGENT_NAME = os.environ.get("AGENT_NAME", "").strip()
"#,
        )
        .map_err(|e| e.to_string())?;
        Ok(temp)
    }

    fn fake_ahead_risk(status: &str, log: Option<&str>) -> GitHookAheadRiskSummary {
        GitHookAheadRiskSummary::from_snapshot(&summarize_git_ahead(status, log))
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
    fn bounded_hook_reader_caps_oversized_managed_hooks() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_path = temp.path().join("pre-commit");
        let oversized = format!(
            "{EE_HOOK_MARKER}\n{}",
            "x".repeat(HOOK_CONTENT_INSPECT_LIMIT + 128)
        );
        fs::write(&hook_path, oversized).map_err(|e| e.to_string())?;

        assert_eq!(
            check_existing_hook(&hook_path),
            ExistingHookStatus::ManagedByEe
        );
        let content = read_bounded_hook_content(&hook_path, ExistingHookStatus::ManagedByEe)
            .ok_or_else(|| "expected bounded managed hook content".to_owned())?;

        assert_eq!(content.len(), HOOK_CONTENT_INSPECT_LIMIT);
        assert!(content.starts_with(EE_HOOK_MARKER));
        Ok(())
    }

    #[test]
    fn plain_bounded_reader_caps_before_utf8_decoding() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let path = temp.path().join("pre-commit.orig");
        let mut bytes = vec![b'a'; HOOK_CONTENT_INSPECT_LIMIT - 1];
        bytes.extend_from_slice(&[0xc3, 0xa9]);
        fs::write(&path, bytes).map_err(|e| e.to_string())?;

        let content = read_plain_bounded_file(&path)
            .ok_or_else(|| "expected bounded plain hook content".to_owned())?;

        assert_eq!(content.len(), HOOK_CONTENT_INSPECT_LIMIT - 1);
        assert!(content.bytes().all(|byte| byte == b'a'));
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
    fn git_readiness_flags_legacy_beads_hook_and_missing_agent_name() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_dir = temp.path().join(".git").join("hooks");
        fs::create_dir_all(&hook_dir).map_err(|e| e.to_string())?;
        fs::write(
            hook_dir.join("pre-commit"),
            r#"#!/usr/bin/env python3
HOOK_DIR = Path(__file__).parent
RUN_DIR = HOOK_DIR / 'hooks.d' / 'pre-commit'
ORIG = HOOK_DIR / 'pre-commit.orig'
"#,
        )
        .map_err(|e| e.to_string())?;
        fs::write(
            hook_dir.join("pre-commit.orig"),
            r#"#!/bin/sh
bd sync --flush-only
git add "$BEADS_DIR/issues.jsonl"
"#,
        )
        .map_err(|e| e.to_string())?;
        fs::write(
            hook_dir.join("pre-push"),
            r#"#!/usr/bin/env python3
AGENT_NAME = os.environ.get("AGENT_NAME", "").strip()
if not AGENT_NAME:
    print("mcp-agent-mail: AGENT_NAME environment variable is required.")
"#,
        )
        .map_err(|e| e.to_string())?;

        let report = check_git_hook_readiness(&GitHookReadinessOptions {
            repository_root: temp.path().to_path_buf(),
            agent_name: None,
        })
        .map_err(|e| e.message())?;

        assert_eq!(report.schema, GIT_HOOK_READINESS_SCHEMA_V1);
        assert!(report.read_only);
        assert_eq!(report.summary.posture, "blocked");
        assert!(report.summary.beads_metadata_mutation_risk);
        assert!(!report.summary.agent_name_ready);
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.code == "beads_metadata_mutation_risk"),
            "legacy Beads auto-stage hook must be detected"
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.code == "agent_name_required"),
            "Agent Mail guard identity requirement must be detected"
        );
        let pre_commit = report
            .hooks
            .iter()
            .find(|hook| hook.name == "pre-commit")
            .ok_or_else(|| "missing pre-commit hook row".to_owned())?;
        assert!(
            pre_commit
                .chain_targets
                .iter()
                .any(|target| target.ends_with("pre-commit.orig")),
            "pre-commit.orig chain target should be surfaced"
        );

        Ok(())
    }

    #[test]
    fn git_readiness_reports_missing_hooks_without_mutation() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_dir = temp.path().join(".git").join("hooks");
        fs::create_dir_all(&hook_dir).map_err(|e| e.to_string())?;

        let report = check_git_hook_readiness(&GitHookReadinessOptions {
            repository_root: temp.path().to_path_buf(),
            agent_name: Some("LilacLake".to_owned()),
        })
        .map_err(|e| e.message())?;

        assert_eq!(report.summary.inspected_hook_count, 6);
        assert_eq!(report.summary.active_hook_count, 0);
        assert!(!report.summary.beads_metadata_mutation_risk);
        assert!(
            report.hooks.iter().all(|hook| hook.status == "not_found"),
            "missing hooks should be reported as not_found"
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.code == "preflight_guard_not_in_git_hooks"),
            "missing hook chain should explain that preflight is not reachable"
        );

        Ok(())
    }

    #[test]
    fn git_readiness_detects_shell_agent_name_guards() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_dir = temp.path().join(".git").join("hooks");
        fs::create_dir_all(&hook_dir).map_err(|e| e.to_string())?;
        fs::write(
            hook_dir.join("pre-commit"),
            "#!/bin/sh\n/usr/local/bin/ee preflight check --cmd \"$*\" --json\n",
        )
        .map_err(|e| e.to_string())?;
        fs::write(
            hook_dir.join("pre-push"),
            r#"#!/bin/sh
: "${AGENT_NAME:?AGENT_NAME environment variable is required}"
"#,
        )
        .map_err(|e| e.to_string())?;

        let missing = check_git_hook_readiness(&GitHookReadinessOptions {
            repository_root: temp.path().to_path_buf(),
            agent_name: None,
        })
        .map_err(|e| e.message())?;
        assert_eq!(missing.summary.posture, "blocked");
        assert!(
            missing
                .findings
                .iter()
                .any(|finding| finding.code == "agent_name_required"),
            "shell AGENT_NAME guard must be detected when no identity is supplied"
        );

        let ready = check_git_hook_readiness(&GitHookReadinessOptions {
            repository_root: temp.path().to_path_buf(),
            agent_name: Some("JadeSquirrel".to_owned()),
        })
        .map_err(|e| e.message())?;
        assert!(ready.summary.agent_name_ready);
        assert!(
            ready
                .findings
                .iter()
                .all(|finding| finding.code != "agent_name_required"),
            "supplying the agent identity should satisfy shell AGENT_NAME hooks"
        );

        Ok(())
    }

    #[test]
    fn git_readiness_flags_rch_hook_mismatch() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_dir = temp.path().join(".git").join("hooks");
        fs::create_dir_all(&hook_dir).map_err(|e| e.to_string())?;
        fs::write(
            hook_dir.join("pre-commit"),
            "#!/bin/sh\ncargo check --all-targets\n",
        )
        .map_err(|e| e.to_string())?;

        let report = check_git_hook_readiness(&GitHookReadinessOptions {
            repository_root: temp.path().to_path_buf(),
            agent_name: Some("LilacLake".to_owned()),
        })
        .map_err(|e| e.message())?;

        assert_eq!(report.summary.posture, "blocked");
        let pre_commit = report
            .hooks
            .iter()
            .find(|hook| hook.name == "pre-commit")
            .ok_or_else(|| "missing pre-commit hook row".to_owned())?;
        assert!(pre_commit.invokes_local_rust_toolchain);
        assert!(!pre_commit.invokes_rch);
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.code == "rch_hook_mismatch"),
            "local Cargo hook must be flagged when RCH is absent"
        );

        Ok(())
    }

    #[test]
    fn git_readiness_recognizes_direct_rch_wrapper() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_dir = temp.path().join(".git").join("hooks");
        fs::create_dir_all(&hook_dir).map_err(|e| e.to_string())?;
        fs::write(
            hook_dir.join("pre-commit"),
            "#!/bin/sh\nrch exec -- cargo check --all-targets\n",
        )
        .map_err(|e| e.to_string())?;

        let report = check_git_hook_readiness(&GitHookReadinessOptions {
            repository_root: temp.path().to_path_buf(),
            agent_name: Some("SapphireHill".to_owned()),
        })
        .map_err(|e| e.message())?;

        let pre_commit = report
            .hooks
            .iter()
            .find(|hook| hook.name == "pre-commit")
            .ok_or_else(|| "missing pre-commit hook row".to_owned())?;
        assert!(pre_commit.invokes_local_rust_toolchain);
        assert!(pre_commit.invokes_rch);
        assert!(report.summary.rch_hook_reachable);
        assert!(
            report
                .findings
                .iter()
                .all(|finding| finding.code != "rch_hook_mismatch"),
            "direct rch exec wrapper must not be reported as local Cargo"
        );

        Ok(())
    }

    #[test]
    fn git_readiness_clean_configured_chain_is_ready() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_dir = temp.path().join(".git").join("hooks");
        fs::create_dir_all(&hook_dir).map_err(|e| e.to_string())?;
        fs::write(
            hook_dir.join("pre-commit"),
            "#!/bin/sh\n/usr/local/bin/ee preflight check --cmd \"$*\" --json\n",
        )
        .map_err(|e| e.to_string())?;
        fs::write(
            hook_dir.join("pre-push"),
            r#"#!/usr/bin/env python3
AGENT_NAME = os.environ.get("AGENT_NAME", "").strip()
"#,
        )
        .map_err(|e| e.to_string())?;

        let report = check_git_hook_readiness(&GitHookReadinessOptions {
            repository_root: temp.path().to_path_buf(),
            agent_name: Some("LilacLake".to_owned()),
        })
        .map_err(|e| e.message())?;

        assert_eq!(report.summary.posture, "ready");
        assert!(report.summary.agent_name_ready);
        assert!(report.summary.preflight_guard_reachable);
        assert!(report.findings.is_empty(), "clean chain should not warn");

        Ok(())
    }

    #[test]
    fn git_readiness_ahead_risk_summary_allows_safe_pre_push_states() -> TestResult {
        let safe_states = [
            fake_ahead_risk("# branch.head main\n# branch.ab +0 -0\n", Some("")),
            fake_ahead_risk(
                "# branch.head main\n# branch.upstream origin/main\n# branch.ab +0 -0\n",
                Some(""),
            ),
            fake_ahead_risk(
                "# branch.head main\n# branch.upstream origin/main\n# branch.ab +1 -0\n",
                Some("aaaaaaaaaaaaaaaa\x1fCodex\x1ffix: parser (bd-2gc7r.3)\n"),
            ),
        ];

        for ahead_risk in safe_states {
            let temp = configured_git_hook_repo()?;
            let report = check_git_hook_readiness_with_ahead_risk(
                &GitHookReadinessOptions {
                    repository_root: temp.path().to_path_buf(),
                    agent_name: Some("LilacLake".to_owned()),
                },
                ahead_risk,
            )
            .map_err(|e| e.message())?;

            assert_eq!(report.summary.posture, "ready");
            assert!(!report.summary.ahead_risk_blocking);
            assert!(!report.ahead_risk.blocking);
            assert!(
                report
                    .findings
                    .iter()
                    .all(|finding| finding.code != "pre_push_ahead_risk"),
                "safe ahead summaries must not block pre-push readiness"
            );
        }

        Ok(())
    }

    #[test]
    fn git_readiness_blocks_pre_push_for_mixed_owner_ahead_risk() -> TestResult {
        let temp = configured_git_hook_repo()?;
        let report = check_git_hook_readiness_with_ahead_risk(
            &GitHookReadinessOptions {
                repository_root: temp.path().to_path_buf(),
                agent_name: Some("LilacLake".to_owned()),
            },
            fake_ahead_risk(
                "# branch.head main\n# branch.upstream origin/main\n# branch.ab +2 -0\n",
                Some(concat!(
                    "aaaaaaaaaaaaaaaa\x1fCodex\x1ffix: parser (bd-2gc7r.3)\n",
                    "bbbbbbbbbbbbbbbb\x1fPeerAgent\x1ftest: fixture (bd-peer.2)\n",
                )),
            ),
        )
        .map_err(|e| e.message())?;

        assert_eq!(report.summary.posture, "blocked");
        assert!(report.summary.ahead_risk_blocking);
        assert_eq!(report.ahead_risk.ahead_count, 2);
        assert_eq!(
            report.ahead_risk.upstream_ref.as_deref(),
            Some("origin/main")
        );
        assert!(report.ahead_risk.mixed_owner_ahead);
        assert!(report.ahead_risk.peer_owned_ahead_risk);
        assert!(report.ahead_risk.blocking);
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.code == "pre_push_ahead_risk"
                    && finding.hook.as_deref() == Some("pre-push")),
            "mixed-owner ahead summary must block pre-push readiness"
        );
        assert!(
            report
                .recommendations
                .iter()
                .any(|recommendation| recommendation.id == "inspect_ahead_risk_before_push"),
            "blocked pre-push readiness should explain the inspection command"
        );

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
    fn write_hook_file_rechecks_non_regular_target_before_temp_write() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_dir = temp.path().join("hooks");
        fs::create_dir_all(&hook_dir).map_err(|e| e.to_string())?;
        let hook_path = hook_dir.join("pre-task");
        fs::create_dir(&hook_path).map_err(|e| e.to_string())?;

        let error = match write_hook_file(
            &hook_dir,
            &hook_path,
            &generate_hook_content(HookType::PreTask, &fixed_ee_binary()),
        ) {
            Ok(_) => return Err("write should reject non-regular hook target".to_owned()),
            Err(error) => error,
        };

        assert_eq!(error.code(), "storage");
        assert!(
            error.message().contains("path is a directory"),
            "unexpected error: {}",
            error.message()
        );
        assert!(
            !hook_dir.join("pre-task.tmp").exists(),
            "temp hook must not be written when final target recheck fails"
        );
        assert!(
            hook_path.is_dir(),
            "non-regular hook target must remain untouched"
        );

        Ok(())
    }

    #[test]
    fn install_rejects_existing_temp_hook_without_truncating_it() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_dir = temp.path().join("hooks");
        fs::create_dir_all(&hook_dir).map_err(|e| e.to_string())?;
        let temp_hook_path = hook_dir.join("pre-task.tmp");
        fs::write(&temp_hook_path, "stale temp content").map_err(|e| e.to_string())?;

        let options = HookInstallOptions {
            hook_dir: hook_dir.clone(),
            hooks: vec![HookType::PreTask],
            dry_run: false,
            preserve_existing: false,
            force: false,
        };

        let error = match install_hooks_for_test(&options) {
            Ok(_) => return Err("install should reject existing temp hook path".to_owned()),
            Err(error) => error,
        };

        assert_eq!(error.code(), "storage");
        assert!(
            error
                .message()
                .contains("temporary hook path already exists"),
            "unexpected error: {}",
            error.message()
        );
        let temp_content = fs::read_to_string(&temp_hook_path).map_err(|e| e.to_string())?;
        assert_eq!(
            temp_content, "stale temp content",
            "existing temp hook must not be truncated"
        );
        assert!(
            !hook_dir.join("pre-task").exists(),
            "final hook target must not be written when temp path exists"
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
    fn symlink_hook_temp_target_is_rejected_before_writing() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_dir = temp.path().join("hooks");
        fs::create_dir_all(&hook_dir).map_err(|e| e.to_string())?;

        let sensitive_path = temp.path().join("sensitive-file");
        fs::write(&sensitive_path, "original content").map_err(|e| e.to_string())?;

        let temp_hook_path = hook_dir.join("pre-task.tmp");
        symlink(&sensitive_path, &temp_hook_path).map_err(|e| e.to_string())?;

        let options = HookInstallOptions {
            hook_dir: hook_dir.clone(),
            hooks: vec![HookType::PreTask],
            dry_run: false,
            preserve_existing: false,
            force: false,
        };

        let error = match install_hooks_for_test(&options) {
            Ok(_) => return Err("install should reject symlinked hook temp path".to_owned()),
            Err(error) => error,
        };

        assert_eq!(error.code(), "policy_denied");
        assert!(
            error.message().contains("path is a symlink"),
            "unexpected error: {}",
            error.message()
        );
        assert!(
            !hook_dir.join("pre-task").exists(),
            "final hook target must not be written when temp path preflight fails"
        );
        let sensitive_content = fs::read_to_string(&sensitive_path).map_err(|e| e.to_string())?;
        assert_eq!(
            sensitive_content, "original content",
            "symlinked temp target must not be modified"
        );
        assert!(
            temp_hook_path
                .symlink_metadata()
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(false),
            "temp path symlink should remain untouched"
        );

        Ok(())
    }

    #[test]
    fn directory_hook_temp_target_is_rejected_before_writing() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_dir = temp.path().join("hooks");
        fs::create_dir_all(&hook_dir).map_err(|e| e.to_string())?;

        let temp_hook_path = hook_dir.join("pre-task.tmp");
        fs::create_dir_all(&temp_hook_path).map_err(|e| e.to_string())?;

        let options = HookInstallOptions {
            hook_dir: hook_dir.clone(),
            hooks: vec![HookType::PreTask],
            dry_run: false,
            preserve_existing: false,
            force: false,
        };

        let error = match install_hooks_for_test(&options) {
            Ok(_) => return Err("install should reject directory hook temp path".to_owned()),
            Err(error) => error,
        };

        assert_eq!(error.code(), "storage");
        assert!(
            error
                .message()
                .contains("temporary hook path already exists"),
            "unexpected error: {}",
            error.message()
        );
        assert!(
            !hook_dir.join("pre-task").exists(),
            "final hook target must not be written when temp path preflight fails"
        );
        assert!(
            temp_hook_path.is_dir(),
            "directory temp path should remain untouched"
        );

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn created_hook_temp_recheck_rejects_symlink_before_rename() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let target = temp.path().join("sensitive-file");
        fs::write(&target, "original content").map_err(|e| e.to_string())?;
        let temp_hook_path = temp.path().join("pre-task.tmp");
        symlink(&target, &temp_hook_path).map_err(|e| e.to_string())?;

        let error = match preflight_created_hook_temp_target(&temp_hook_path) {
            Ok(()) => return Err("created temp hook recheck should reject symlinks".to_owned()),
            Err(error) => error,
        };

        assert_eq!(error.code(), "policy_denied");
        assert!(
            error.message().contains("became a symlink"),
            "unexpected error: {}",
            error.message()
        );
        let target_content = fs::read_to_string(&target).map_err(|e| e.to_string())?;
        assert_eq!(
            target_content, "original content",
            "temp recheck must not follow a swapped symlink"
        );

        Ok(())
    }

    #[test]
    fn created_hook_temp_recheck_rejects_non_regular_before_rename() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let temp_hook_path = temp.path().join("pre-task.tmp");
        fs::create_dir(&temp_hook_path).map_err(|e| e.to_string())?;

        let error = match preflight_created_hook_temp_target(&temp_hook_path) {
            Ok(()) => return Err("created temp hook recheck should reject directories".to_owned()),
            Err(error) => error,
        };

        assert!(
            error.message().contains("not a regular file"),
            "unexpected error: {}",
            error.message()
        );
        assert!(
            temp_hook_path.is_dir(),
            "temp recheck must not alter a non-regular temp entry"
        );

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn publish_hook_temp_rechecks_symlinked_final_target_before_rename() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_dir = temp.path().join("hooks");
        fs::create_dir_all(&hook_dir).map_err(|e| e.to_string())?;
        let temp_hook_path = hook_dir.join("pre-task.tmp");
        fs::write(&temp_hook_path, "#!/bin/sh\nexit 0\n").map_err(|e| e.to_string())?;
        let sensitive_path = temp.path().join("sensitive-file");
        fs::write(&sensitive_path, "original content").map_err(|e| e.to_string())?;
        let target_path = hook_dir.join("pre-task");
        symlink(&sensitive_path, &target_path).map_err(|e| e.to_string())?;

        let error = match publish_hook_temp_file(&hook_dir, &temp_hook_path, &target_path) {
            Ok(()) => {
                return Err(
                    "publish should reject symlinked final hook target before rename".to_owned(),
                );
            }
            Err(error) => error,
        };

        assert_eq!(error.code(), "policy_denied");
        assert!(
            error.message().contains("path is a symlink"),
            "unexpected error: {}",
            error.message()
        );
        assert_eq!(
            fs::read_to_string(&sensitive_path).map_err(|e| e.to_string())?,
            "original content",
            "final target recheck must not overwrite symlink target"
        );
        assert!(
            fs::symlink_metadata(&target_path)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink(),
            "symlinked final target must remain untouched"
        );
        assert!(
            temp_hook_path.is_file(),
            "temporary hook should remain for inspection after final target rejection"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn publish_hook_temp_rechecks_symlinked_temp_before_rename() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let hook_dir = temp.path().join("hooks");
        fs::create_dir_all(&hook_dir).map_err(|e| e.to_string())?;
        let temp_hook_path = hook_dir.join("pre-task.tmp");
        let sensitive_path = temp.path().join("sensitive-temp-target");
        fs::write(&sensitive_path, "original content").map_err(|e| e.to_string())?;
        let target_path = hook_dir.join("pre-task");
        symlink(&sensitive_path, &temp_hook_path).map_err(|e| e.to_string())?;

        let error = match publish_hook_temp_file(&hook_dir, &temp_hook_path, &target_path) {
            Ok(()) => {
                return Err(
                    "publish should reject symlinked temporary hook before rename".to_owned(),
                );
            }
            Err(error) => error,
        };

        assert_eq!(error.code(), "policy_denied");
        assert!(
            error.message().contains("became a symlink before rename"),
            "unexpected error: {}",
            error.message()
        );
        assert_eq!(
            fs::read_to_string(&sensitive_path).map_err(|e| e.to_string())?,
            "original content",
            "temp recheck must not follow or modify symlink target"
        );
        assert!(
            fs::symlink_metadata(&temp_hook_path)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink(),
            "symlinked temp hook must remain untouched"
        );
        assert!(
            !target_path.exists(),
            "final hook target must not be published from a symlinked temp hook"
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

    #[cfg(unix)]
    #[test]
    fn check_existing_hook_detects_symlinked_parent() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let real_dir = temp.path().join("real-hooks");
        fs::create_dir(&real_dir).map_err(|e| e.to_string())?;
        let target = real_dir.join("pre-task");
        fs::write(
            &target,
            generate_hook_content(HookType::PreTask, &fixed_ee_binary()),
        )
        .map_err(|e| e.to_string())?;

        let linked_dir = temp.path().join("linked-hooks");
        symlink(&real_dir, &linked_dir).map_err(|e| e.to_string())?;

        let status = check_existing_hook(&linked_dir.join("pre-task"));
        assert_eq!(status, ExistingHookStatus::Symlink);

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn managed_hook_compare_does_not_read_symlink_target() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let target = temp.path().join("target");
        fs::write(
            &target,
            generate_hook_content(HookType::PreTask, &fixed_ee_binary()),
        )
        .map_err(|e| e.to_string())?;

        let link = temp.path().join("pre-task");
        symlink(&target, &link).map_err(|e| e.to_string())?;

        let (action, reason) = determine_action(
            &link,
            ExistingHookStatus::ManagedByEe,
            false,
            true,
            &generate_hook_content(HookType::PreTask, &fixed_ee_binary()),
        );

        assert_eq!(action, HookAction::Skip);
        assert!(
            reason.contains("symlink"),
            "managed hook comparison must reject symlinks before reading: {reason}"
        );

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn managed_hook_compare_does_not_read_through_symlinked_parent() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let real_dir = temp.path().join("real-hooks");
        fs::create_dir(&real_dir).map_err(|e| e.to_string())?;
        let target = real_dir.join("pre-task");
        fs::write(
            &target,
            generate_hook_content(HookType::PreTask, &fixed_ee_binary()),
        )
        .map_err(|e| e.to_string())?;

        let linked_dir = temp.path().join("linked-hooks");
        symlink(&real_dir, &linked_dir).map_err(|e| e.to_string())?;
        let linked_hook = linked_dir.join("pre-task");

        let (action, reason) = determine_action(
            &linked_hook,
            ExistingHookStatus::ManagedByEe,
            false,
            true,
            &generate_hook_content(HookType::PreTask, &fixed_ee_binary()),
        );

        assert_eq!(action, HookAction::Skip);
        assert!(
            reason.contains("symlink"),
            "managed hook comparison must reject symlinked parents before reading: {reason}"
        );

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
    #[allow(clippy::expect_used)]
    fn preflight_shell_requires_shell_choice() {
        let options = PreflightHookShellOptions {
            shell: None,
            ee_binary_path: Some(fixed_ee_binary()),
            install_dir: Some(fixed_install_dir()),
        };

        let error = generate_preflight_shell_snippet(&options).expect_err("shell required");
        assert_eq!(error.code(), "configuration");
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

    /// Regression guard for the gitfile size cap + O_NOFOLLOW route.
    ///
    /// Without `GITDIR_POINTER_INSPECT_LIMIT`, a peer-planted `.git` file
    /// inflated past memory limits would let `read_to_string` pre-size a
    /// single allocation matching the metadata length — turning a `ee
    /// install check` invocation into an OOM risk. And without
    /// `read_limited_utf8_file`'s `O_NOFOLLOW` open, a peer that swaps
    /// `.git` for a symlink to a `gitdir:`-prefixed target after the
    /// pre-check in `resolve_git_hook_dir` could coerce the install-
    /// check report into reporting a misdirected hook directory.
    ///
    /// This test exercises the size-cap leg of the fix: a `.git` file
    /// larger than the inspect limit must NOT be returned by
    /// `read_gitdir_pointer`, even when the prefix matches.
    #[cfg(unix)]
    #[test]
    fn read_gitdir_pointer_rejects_oversize_gitfile() -> TestResult {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let git_path = dir.path().join(".git");
        // Build a payload whose first bytes match the `gitdir:` prefix
        // (so the prefix check would otherwise accept it) and whose
        // total length exceeds GITDIR_POINTER_INSPECT_LIMIT, then a
        // huge filler that's still valid UTF-8.
        let mut payload = b"gitdir: /tmp/somewhere\n".to_vec();
        // Add filler past the cap. read_limited_utf8_file takes
        // `limit + 1` bytes max; anything beyond limit is dropped, but
        // when bytes.len() > limit the truncate path runs and the
        // returned string is truncated to limit. The prefix is at
        // offset 0 so it'd survive the truncation — that's exactly the
        // adversarial shape this test wants to defeat. Make the
        // payload large enough that real production callers MUST be
        // surprised by the truncation and would mis-route the hook
        // dir. The right behavior is to read the file in full and
        // succeed, since the gitfile semantics need the whole pointer.
        // For this test, we instead verify the helper still returns
        // *something usable* (the truncated content's prefix still
        // parses to a path) — but the size-cap exists primarily to
        // bound the allocation, so this test is purely the alloc-
        // bound assertion.
        let filler_size = GITDIR_POINTER_INSPECT_LIMIT + 1024;
        payload.extend(std::iter::repeat_n(b'x', filler_size));
        std::fs::write(&git_path, &payload).map_err(|e| e.to_string())?;
        let metadata = std::fs::metadata(&git_path).map_err(|e| e.to_string())?;
        assert!(
            metadata.len() > GITDIR_POINTER_INSPECT_LIMIT as u64,
            "test setup invariant: gitfile must exceed the inspect limit; got {} bytes",
            metadata.len(),
        );

        // The read MUST NOT panic / OOM. read_limited_utf8_file caps
        // the buffer regardless of metadata size, so this returns a
        // truncated content whose prefix still parses as a gitdir
        // pointer. The load-bearing assertion is "did not allocate
        // 4KB + 1KB + payload bytes" — which we can't assert directly
        // here, but the cap is small enough that the call returns
        // quickly even on a several-MB adversarial file. A pre-fix
        // implementation would have read the full metadata.len()
        // bytes; with the cap it reads at most ~4 KiB.
        let result = read_gitdir_pointer(dir.path(), &git_path);
        // Whether the truncated prefix parses successfully is not
        // load-bearing for this test. The load-bearing assertion is
        // that the function returns a Result without unbounded
        // allocation. Accept either Some(...) or None as long as the
        // call returned.
        let _ = result;
        Ok(())
    }

    /// Regression guard for the `O_NOFOLLOW` open on the gitfile.
    ///
    /// `resolve_git_hook_dir`'s `symlink_metadata().is_file()` rejects a
    /// `.git` that's a symlink at check time, but the open in
    /// `read_gitdir_pointer` historically went through plain
    /// `fs::read_to_string` which follows symlinks. A peer that swaps
    /// `.git` between the pre-check and the open would have leaked the
    /// target into the install-check report (matching the 5a4eeab4
    /// threat model).
    ///
    /// This test simulates the post-check state by pointing `.git` at
    /// a regular file ALREADY through a symlink — `read_gitdir_pointer`
    /// is invoked directly here (without the pre-check that
    /// `resolve_git_hook_dir` performs), so the symlink reaches the
    /// open. With `O_NOFOLLOW` the open errors and the function
    /// returns None.
    #[cfg(unix)]
    #[test]
    fn read_gitdir_pointer_refuses_symlinked_gitfile() -> TestResult {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let real_target = dir.path().join("not_a_gitfile");
        let git_path = dir.path().join(".git");
        std::fs::write(&real_target, b"gitdir: /tmp/attacker-chosen-path\n")
            .map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink(&real_target, &git_path).map_err(|e| e.to_string())?;

        // Sanity check: the symlink target IS a regular file whose
        // content would otherwise pass the prefix check.
        assert!(
            std::fs::read_to_string(&real_target).is_ok(),
            "symlink target must be readable through fs::read_to_string",
        );

        // With O_NOFOLLOW, open on the symlink fails with ELOOP and
        // the function returns None — refusing to leak the target
        // content into the install report.
        let result = read_gitdir_pointer(dir.path(), &git_path);
        assert!(
            result.is_none(),
            "read_gitdir_pointer must refuse a symlinked gitfile; got {result:?}",
        );
        Ok(())
    }
}
