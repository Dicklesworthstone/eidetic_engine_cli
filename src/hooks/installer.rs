//! Hook installer with dry-run, idempotency, and preserve-existing-hook behavior (EE-321).
//!
//! Provides safe installation of ee hooks into agent harness hook directories.
//! Supports dry-run mode, idempotent re-installation, and preservation of existing hooks.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::models::DomainError;

/// Schema for hook install report.
pub const HOOK_INSTALL_SCHEMA_V1: &str = "ee.hooks.install.v1";

/// Schema for hook status report.
pub const HOOK_STATUS_SCHEMA_V1: &str = "ee.hooks.status.v1";

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
}

impl ExistingHookStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::ManagedByEe => "managed_by_ee",
            Self::External => "external",
            Self::Unreadable => "unreadable",
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
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// The marker that identifies ee-managed hooks.
const EE_HOOK_MARKER: &str = "# ee-managed-hook";

/// Check if a hook file is managed by ee.
fn is_ee_managed_hook(content: &str) -> bool {
    content.contains(EE_HOOK_MARKER)
}

/// Get the status of an existing hook.
fn check_existing_hook(path: &Path) -> ExistingHookStatus {
    if !path.exists() {
        return ExistingHookStatus::NotFound;
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
    existing: ExistingHookStatus,
    preserve_existing: bool,
    force: bool,
) -> (HookAction, &'static str) {
    match existing {
        ExistingHookStatus::NotFound => (HookAction::Install, "No existing hook"),
        ExistingHookStatus::ManagedByEe => (HookAction::Update, "Updating ee-managed hook"),
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
# Installed by ee at: {timestamp}
# Binary path captured at install time (absolute, not PATH-resolved)
#
# This hook is managed by ee. Manual edits may be overwritten.
# To disable ee management, remove the "{marker}" line above.

{ee_path} hooks run {hook_type} "$@"
"#,
        marker = EE_HOOK_MARKER,
        hook_type = hook_type.as_str(),
        timestamp = Utc::now().to_rfc3339(),
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
    let exe = std::env::current_exe().map_err(|e| {
        DomainError::internal(format!("Failed to determine ee binary path: {e}"))
    })?;
    // Canonicalize to resolve symlinks and get absolute path
    exe.canonicalize().map_err(|e| {
        DomainError::internal(format!(
            "Failed to canonicalize ee binary path '{}': {e}",
            exe.display()
        ))
    })
}

/// Install hooks according to options.
///
/// # Security
/// Embeds the absolute canonical path of the `ee` binary at install time to prevent
/// PATH hijack attacks. The binary path is captured via `std::env::current_exe()`
/// and canonicalized before being embedded in generated hook scripts.
pub fn install_hooks(options: &HookInstallOptions) -> Result<HookInstallReport, DomainError> {
    let now = Utc::now().to_rfc3339();
    let mut plan = Vec::new();
    let mut installed_count = 0u32;
    let mut updated_count = 0u32;
    let mut skipped_count = 0u32;
    let mut no_change_count = 0u32;

    // Capture absolute binary path at install time to embed in hooks (security fix)
    let ee_binary_path = get_ee_binary_path()?;

    for hook_type in &options.hooks {
        let target_path = options.hook_dir.join(hook_type.filename());
        let existing = check_existing_hook(&target_path);
        let (action, reason) = determine_action(existing, options.preserve_existing, options.force);

        plan.push(HookInstallPlanItem {
            hook_type: hook_type.as_str().to_owned(),
            target_path: target_path.display().to_string(),
            existing_status: existing.as_str().to_owned(),
            action: action.as_str().to_owned(),
            reason: reason.to_owned(),
        });

        if !options.dry_run && action.is_mutating() {
            let content = generate_hook_content(*hook_type, &ee_binary_path);
            std::fs::create_dir_all(&options.hook_dir).ok();
            std::fs::write(&target_path, content).ok();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(metadata) = std::fs::metadata(&target_path) {
                    let mut perms = metadata.permissions();
                    perms.set_mode(0o755);
                    std::fs::set_permissions(&target_path, perms).ok();
                }
            }
        }

        match action {
            HookAction::Install => installed_count += 1,
            HookAction::Update => updated_count += 1,
            HookAction::Skip => skipped_count += 1,
            HookAction::NoChange => no_change_count += 1,
        }
    }

    let idempotent = plan.iter().all(|item| {
        item.action == HookAction::NoChange.as_str() || item.action == HookAction::Skip.as_str()
    });

    Ok(HookInstallReport {
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
    })
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
        serde_json::to_string(self).unwrap_or_default()
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
        let executable = path.exists() && {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::metadata(&path)
                    .map(|m| m.permissions().mode() & 0o111 != 0)
                    .unwrap_or(false)
            }
            #[cfg(not(unix))]
            {
                true
            }
        };

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
            ExistingHookStatus::Unreadable => external_count += 1,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    type TestResult = Result<(), String>;

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

        let report = install_hooks(&options).map_err(|e| e.message())?;
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

        let report = install_hooks(&options).map_err(|e| e.message())?;
        assert!(!report.dry_run);
        assert_eq!(report.installed_count, 1);

        let hook_path = temp.path().join("post-task");
        assert!(hook_path.exists(), "hook file should exist");

        let content = fs::read_to_string(&hook_path).map_err(|e| e.to_string())?;
        assert!(content.contains(EE_HOOK_MARKER));
        Ok(())
    }

    #[test]
    fn idempotent_reinstall_updates_managed_hook() -> TestResult {
        let temp = TempDir::new().map_err(|e| e.to_string())?;
        let options = HookInstallOptions {
            hook_dir: temp.path().to_path_buf(),
            hooks: vec![HookType::PreCommit],
            dry_run: false,
            preserve_existing: true,
            force: false,
        };

        let report1 = install_hooks(&options).map_err(|e| e.message())?;
        assert_eq!(report1.installed_count, 1);

        let report2 = install_hooks(&options).map_err(|e| e.message())?;
        assert_eq!(report2.updated_count, 1);
        assert_eq!(report2.installed_count, 0);
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

        let report = install_hooks(&options).map_err(|e| e.message())?;
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

        let report = install_hooks(&options).map_err(|e| e.message())?;
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

        let _report = install_hooks(&options).map_err(|e| e.message())?;

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

        // The invocation must start with a quoted absolute path, not bare 'ee'
        assert!(
            invocation_line.trim().starts_with("'"),
            "hook invocation must start with quoted path, got: {invocation_line}"
        );
        assert!(
            invocation_line.contains("/'"),
            "hook must contain absolute path (with /), got: {invocation_line}"
        );

        Ok(())
    }
}
