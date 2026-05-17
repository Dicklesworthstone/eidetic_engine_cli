//! Init command handler (EE-028).
//!
//! Initializes the ee workspace with database and index directories.
//! Supports dry-run mode and idempotent re-runs.

use std::{
    fs::{self, OpenOptions},
    io::{self, ErrorKind, Write},
    path::{Path, PathBuf},
};

use super::{build_info, workspace::stable_workspace_id};
use crate::db::{CreateWorkspaceInput, DbConnection};

/// Status of the init operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitStatus {
    Created,
    AlreadyExists,
    DryRun,
    RepairPlan,
    Revalidated,
    Failed,
}

impl InitStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::AlreadyExists => "already_exists",
            Self::DryRun => "dry_run",
            Self::RepairPlan => "repair_plan",
            Self::Revalidated => "revalidated",
            Self::Failed => "failed",
        }
    }

    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(
            self,
            Self::Created
                | Self::AlreadyExists
                | Self::DryRun
                | Self::RepairPlan
                | Self::Revalidated
        )
    }
}

/// A single action taken or planned during init.
#[derive(Clone, Debug)]
pub struct InitAction {
    pub action: &'static str,
    pub path: PathBuf,
    pub status: &'static str,
}

/// Options for the init command.
#[derive(Clone, Debug)]
pub struct InitOptions {
    pub workspace_path: PathBuf,
    pub dry_run: bool,
    /// Report non-destructive repair actions without applying them.
    pub repair_plan: bool,
    /// Force revalidation/recreation of EE-owned artifacts.
    pub force: bool,
    /// Allow workspace paths that traverse symlinks.
    pub allow_symlink: bool,
    /// Skip generating AGENTS.md and CLAUDE.md boilerplate files.
    pub skip_boilerplate: bool,
}

/// Report returned by the init command.
#[derive(Clone, Debug)]
pub struct InitReport {
    pub version: &'static str,
    pub status: InitStatus,
    pub workspace: PathBuf,
    pub ee_dir: PathBuf,
    pub database_path: PathBuf,
    pub index_dir: PathBuf,
    pub actions: Vec<InitAction>,
    pub dry_run: bool,
}

impl InitReport {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = String::new();

        match self.status {
            InitStatus::Created => {
                output.push_str("Initialized ee workspace\n\n");
            }
            InitStatus::AlreadyExists => {
                output.push_str("ee workspace already initialized\n\n");
            }
            InitStatus::DryRun => {
                output.push_str("DRY RUN: Would initialize ee workspace\n\n");
            }
            InitStatus::RepairPlan => {
                output.push_str("REPAIR PLAN: Proposed actions for ee workspace\n\n");
            }
            InitStatus::Revalidated => {
                output.push_str("Revalidated ee workspace\n\n");
            }
            InitStatus::Failed => {
                output.push_str("Failed to initialize ee workspace\n\n");
            }
        }

        output.push_str(&format!("  Workspace: {}\n", self.workspace.display()));
        output.push_str(&format!("  ee directory: {}\n", self.ee_dir.display()));
        output.push_str(&format!("  Database: {}\n", self.database_path.display()));
        output.push_str(&format!("  Index: {}\n", self.index_dir.display()));

        if !self.actions.is_empty() {
            output.push_str("\nActions:\n");
            for action in &self.actions {
                output.push_str(&format!(
                    "  {} {} ({})\n",
                    action.action,
                    action.path.display(),
                    action.status
                ));
            }
        }

        output
    }

    #[must_use]
    pub fn toon_output(&self) -> String {
        format!(
            "INIT|{}|{}|{}",
            self.status.as_str(),
            self.workspace.display(),
            self.actions.len()
        )
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        let actions: Vec<serde_json::Value> = self
            .actions
            .iter()
            .map(|a| {
                serde_json::json!({
                    "action": a.action,
                    "path": a.path.display().to_string(),
                    "status": a.status,
                })
            })
            .collect();

        serde_json::json!({
            "command": "init",
            "version": self.version,
            "status": self.status.as_str(),
            "workspace": self.workspace.display().to_string(),
            "eeDir": self.ee_dir.display().to_string(),
            "databasePath": self.database_path.display().to_string(),
            "indexDir": self.index_dir.display().to_string(),
            "actions": actions,
            "dryRun": self.dry_run,
        })
    }
}

/// Initialize the ee workspace.
///
/// Creates the .ee directory, database file placeholder, and index directory
/// if they don't exist. Idempotent: returns success if already initialized.
///
/// Modes:
/// - `dry_run`: Report what would be done without creating files
/// - `repair_plan`: Report non-destructive repair actions for existing workspaces
/// - `force`: Force revalidation/recreation of EE-owned artifacts
#[must_use]
pub fn init_workspace(options: &InitOptions) -> InitReport {
    let version = build_info().version;

    let workspace = if options.workspace_path.is_absolute() {
        options.workspace_path.clone()
    } else {
        std::env::current_dir()
            .unwrap_or_default()
            .join(&options.workspace_path)
    };

    let ee_dir = workspace.join(".ee");
    let database_path = ee_dir.join("ee.db");
    let index_dir = ee_dir.join("index");

    let mut actions = Vec::new();
    let mut any_created = false;
    let mut any_failed = false;

    if let Some(status) = init_path_safety_status(&workspace, options.allow_symlink) {
        actions.push(InitAction {
            action: "check_workspace",
            path: workspace.clone(),
            status,
        });
        return InitReport {
            version,
            status: InitStatus::Failed,
            workspace,
            ee_dir,
            database_path,
            index_dir,
            actions,
            dry_run: options.dry_run,
        };
    }

    // Repair plan mode: report what could be fixed without making changes
    if options.repair_plan {
        let mut repair_actions = Vec::new();

        if let Some(status) = init_path_safety_status(&ee_dir, options.allow_symlink) {
            repair_actions.push(InitAction {
                action: "check_directory",
                path: ee_dir.clone(),
                status,
            });
        } else if !ee_dir.exists() {
            repair_actions.push(InitAction {
                action: "create_directory",
                path: ee_dir.clone(),
                status: "missing",
            });
        } else {
            repair_actions.push(InitAction {
                action: "check_directory",
                path: ee_dir.clone(),
                status: "ok",
            });
        }

        if let Some(status) = init_path_safety_status(&index_dir, options.allow_symlink) {
            repair_actions.push(InitAction {
                action: "check_directory",
                path: index_dir.clone(),
                status,
            });
        } else if !index_dir.exists() {
            repair_actions.push(InitAction {
                action: "create_directory",
                path: index_dir.clone(),
                status: "missing",
            });
        } else {
            repair_actions.push(InitAction {
                action: "check_directory",
                path: index_dir.clone(),
                status: "ok",
            });
        }

        if let Some(status) = init_path_safety_status(&database_path, options.allow_symlink) {
            repair_actions.push(InitAction {
                action: "check_file",
                path: database_path.clone(),
                status,
            });
        } else if !database_path.exists() {
            repair_actions.push(InitAction {
                action: "create_file",
                path: database_path.clone(),
                status: "missing",
            });
        } else {
            repair_actions.push(InitAction {
                action: "check_file",
                path: database_path.clone(),
                status: "ok",
            });
        }

        return InitReport {
            version,
            status: InitStatus::RepairPlan,
            workspace,
            ee_dir,
            database_path,
            index_dir,
            actions: repair_actions,
            dry_run: false,
        };
    }

    if options.dry_run {
        if let Some(status) = init_path_safety_status(&ee_dir, options.allow_symlink) {
            actions.push(InitAction {
                action: "check_directory",
                path: ee_dir.clone(),
                status,
            });
        } else if !ee_dir.exists() {
            actions.push(InitAction {
                action: "create_directory",
                path: ee_dir.clone(),
                status: "would_create",
            });
        }
        if let Some(status) = init_path_safety_status(&index_dir, options.allow_symlink) {
            actions.push(InitAction {
                action: "check_directory",
                path: index_dir.clone(),
                status,
            });
        } else if !index_dir.exists() {
            actions.push(InitAction {
                action: "create_directory",
                path: index_dir.clone(),
                status: "would_create",
            });
        }
        if let Some(status) = init_path_safety_status(&database_path, options.allow_symlink) {
            actions.push(InitAction {
                action: "check_file",
                path: database_path.clone(),
                status,
            });
        } else if !database_path.exists() {
            actions.push(InitAction {
                action: "create_file",
                path: database_path.clone(),
                status: "would_create",
            });
        }

        return InitReport {
            version,
            status: if actions
                .iter()
                .any(|action| is_init_failure_status(action.status))
            {
                InitStatus::Failed
            } else {
                InitStatus::DryRun
            },
            workspace,
            ee_dir,
            database_path,
            index_dir,
            actions,
            dry_run: true,
        };
    }

    if let Some(status) = init_path_safety_status(&ee_dir, options.allow_symlink) {
        actions.push(InitAction {
            action: "check_directory",
            path: ee_dir.clone(),
            status,
        });
        any_failed = true;
    } else if !ee_dir.exists() {
        match fs::create_dir_all(&ee_dir) {
            Ok(()) => {
                if let Some(status) = init_path_safety_status(&ee_dir, options.allow_symlink) {
                    actions.push(InitAction {
                        action: "create_directory",
                        path: ee_dir.clone(),
                        status,
                    });
                    any_failed = true;
                } else {
                    actions.push(InitAction {
                        action: "create_directory",
                        path: ee_dir.clone(),
                        status: "created",
                    });
                    any_created = true;
                }
            }
            Err(_) => {
                actions.push(InitAction {
                    action: "create_directory",
                    path: ee_dir.clone(),
                    status: "failed",
                });
                any_failed = true;
            }
        }
    } else {
        actions.push(InitAction {
            action: "check_directory",
            path: ee_dir.clone(),
            status: "exists",
        });
    }

    if let Some(status) = init_path_safety_status(&index_dir, options.allow_symlink) {
        actions.push(InitAction {
            action: "check_directory",
            path: index_dir.clone(),
            status,
        });
        any_failed = true;
    } else if !index_dir.exists() {
        match fs::create_dir_all(&index_dir) {
            Ok(()) => {
                if let Some(status) = init_path_safety_status(&index_dir, options.allow_symlink) {
                    actions.push(InitAction {
                        action: "create_directory",
                        path: index_dir.clone(),
                        status,
                    });
                    any_failed = true;
                } else {
                    actions.push(InitAction {
                        action: "create_directory",
                        path: index_dir.clone(),
                        status: "created",
                    });
                    any_created = true;
                }
            }
            Err(_) => {
                actions.push(InitAction {
                    action: "create_directory",
                    path: index_dir.clone(),
                    status: "failed",
                });
                any_failed = true;
            }
        }
    } else {
        actions.push(InitAction {
            action: "check_directory",
            path: index_dir.clone(),
            status: "exists",
        });
    }

    if let Some(status) = init_path_safety_status(&database_path, options.allow_symlink) {
        actions.push(InitAction {
            action: "check_file",
            path: database_path.clone(),
            status,
        });
        any_failed = true;
    } else if !database_path.exists() {
        match initialize_database(&database_path, &workspace, options.allow_symlink) {
            Ok(()) => {
                actions.push(InitAction {
                    action: "create_file",
                    path: database_path.clone(),
                    status: "created",
                });
                any_created = true;
            }
            Err(_) => {
                actions.push(InitAction {
                    action: "create_file",
                    path: database_path.clone(),
                    status: "failed",
                });
                any_failed = true;
            }
        }
    } else {
        match initialize_database(&database_path, &workspace, options.allow_symlink) {
            Ok(()) => actions.push(InitAction {
                action: "check_file",
                path: database_path.clone(),
                status: "exists",
            }),
            Err(_) => actions.push(InitAction {
                action: "check_file",
                path: database_path.clone(),
                status: "failed",
            }),
        }
        if actions
            .last()
            .is_some_and(|action| action.status == "failed")
        {
            any_failed = true;
        }
    }

    if !any_failed && !options.skip_boilerplate && !options.repair_plan && !options.dry_run {
        let agents_path = workspace.join("AGENTS.md");
        match create_boilerplate_file(&agents_path, AGENTS_MD_BOILERPLATE, options.allow_symlink) {
            Ok(BoilerplateCreateStatus::Created) => {
                actions.push(InitAction {
                    action: "create_file",
                    path: agents_path,
                    status: "created",
                });
                any_created = true;
            }
            Ok(BoilerplateCreateStatus::Exists) => {
                actions.push(InitAction {
                    action: "check_file",
                    path: agents_path,
                    status: "exists",
                });
            }
            Err(_) => {
                actions.push(InitAction {
                    action: "create_file",
                    path: agents_path,
                    status: "failed",
                });
                any_failed = true;
            }
        }

        let claude_path = workspace.join("CLAUDE.md");
        match create_boilerplate_file(&claude_path, CLAUDE_MD_BOILERPLATE, options.allow_symlink) {
            Ok(BoilerplateCreateStatus::Created) => {
                actions.push(InitAction {
                    action: "create_file",
                    path: claude_path,
                    status: "created",
                });
                any_created = true;
            }
            Ok(BoilerplateCreateStatus::Exists) => {
                actions.push(InitAction {
                    action: "check_file",
                    path: claude_path,
                    status: "exists",
                });
            }
            Err(_) => {
                actions.push(InitAction {
                    action: "create_file",
                    path: claude_path,
                    status: "failed",
                });
                any_failed = true;
            }
        }
    }

    let status = if any_failed {
        InitStatus::Failed
    } else if any_created {
        InitStatus::Created
    } else if options.force {
        InitStatus::Revalidated
    } else {
        InitStatus::AlreadyExists
    };

    InitReport {
        version,
        status,
        workspace,
        ee_dir,
        database_path,
        index_dir,
        actions,
        dry_run: false,
    }
}

fn initialize_database(
    database_path: &PathBuf,
    workspace_path: &Path,
    allow_symlink: bool,
) -> Result<(), String> {
    ensure_init_path_has_no_symlink_components(database_path, allow_symlink)?;
    let connection = DbConnection::open_file(database_path)
        .map_err(|error| format!("failed to open database: {error}"))?;
    connection
        .migrate()
        .map_err(|error| format!("failed to migrate database: {error}"))?;

    // Create workspace row if it doesn't exist (idempotent).
    let workspace_key = workspace_path.to_string_lossy().to_string();
    let existing = connection
        .get_workspace_by_path(&workspace_key)
        .map_err(|error| format!("failed to check workspace: {error}"))?;
    if existing.is_none() {
        let workspace_id = stable_workspace_id(workspace_path);
        connection
            .insert_workspace(
                &workspace_id,
                &CreateWorkspaceInput {
                    path: workspace_key,
                    name: None,
                },
            )
            .map_err(|error| format!("failed to create workspace row: {error}"))?;
    }

    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoilerplateCreateStatus {
    Created,
    Exists,
}

fn create_boilerplate_file(
    path: &Path,
    contents: &str,
    allow_symlink: bool,
) -> Result<BoilerplateCreateStatus, std::io::Error> {
    ensure_boilerplate_path_has_no_symlink_components(path, allow_symlink)?;
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            ensure_existing_boilerplate_path_is_file(path, allow_symlink)?;
            return Ok(BoilerplateCreateStatus::Exists);
        }
        Err(error) => return Err(error),
    };

    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    Ok(BoilerplateCreateStatus::Created)
}

fn ensure_existing_boilerplate_path_is_file(
    path: &Path,
    allow_symlink: bool,
) -> Result<(), io::Error> {
    let metadata = if allow_symlink {
        fs::metadata(path)
    } else {
        fs::symlink_metadata(path)
    }?;
    if metadata.file_type().is_file() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "refusing to treat existing init boilerplate path {} as a file because it is not a regular file",
            path.display()
        ),
    ))
}

fn init_path_safety_status(path: &Path, allow_symlink: bool) -> Option<&'static str> {
    if allow_symlink {
        return None;
    }
    match init_path_has_symlink_component(path) {
        Ok(false) => None,
        Ok(true) => Some("symlink_refused"),
        Err(_) => Some("inspect_failed"),
    }
}

fn ensure_init_path_has_no_symlink_components(
    path: &Path,
    allow_symlink: bool,
) -> Result<(), String> {
    if let Some(status) = init_path_safety_status(path, allow_symlink) {
        return Err(format!("refusing init path {}: {status}", path.display()));
    }
    Ok(())
}

fn ensure_boilerplate_path_has_no_symlink_components(
    path: &Path,
    allow_symlink: bool,
) -> Result<(), io::Error> {
    if let Some(status) = init_path_safety_status(path, allow_symlink) {
        return Err(io::Error::other(format!(
            "refusing boilerplate path {}: {status}",
            path.display()
        )));
    }
    Ok(())
}

fn init_path_has_symlink_component(path: &Path) -> io::Result<bool> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(true),
            Ok(_) => {}
            Err(error)
                if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
            {
                return Ok(false);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

fn is_init_failure_status(status: &str) -> bool {
    matches!(status, "failed" | "inspect_failed" | "symlink_refused")
}

const AGENTS_MD_BOILERPLATE: &str = r#"# AGENTS.md

Instructions for coding agents working in this workspace.

- Read this file before making changes.
- Preserve user work and avoid destructive filesystem or git commands unless explicitly authorized.
- Run the project's formatting, linting, and test commands before committing changes.
"#;

const CLAUDE_MD_BOILERPLATE: &str = r#"# CLAUDE.md

Agent notes for Claude-compatible tools.

See AGENTS.md for the canonical workspace instructions.
"#;

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
    fn init_status_strings_are_stable() -> TestResult {
        ensure(InitStatus::Created.as_str(), "created", "created")?;
        ensure(
            InitStatus::AlreadyExists.as_str(),
            "already_exists",
            "already_exists",
        )?;
        ensure(InitStatus::DryRun.as_str(), "dry_run", "dry_run")?;
        ensure(
            InitStatus::RepairPlan.as_str(),
            "repair_plan",
            "repair_plan",
        )?;
        ensure(
            InitStatus::Revalidated.as_str(),
            "revalidated",
            "revalidated",
        )?;
        ensure(InitStatus::Failed.as_str(), "failed", "failed")
    }

    #[test]
    fn init_status_is_success() -> TestResult {
        ensure(InitStatus::Created.is_success(), true, "created is success")?;
        ensure(
            InitStatus::AlreadyExists.is_success(),
            true,
            "already_exists is success",
        )?;
        ensure(InitStatus::DryRun.is_success(), true, "dry_run is success")?;
        ensure(
            InitStatus::RepairPlan.is_success(),
            true,
            "repair_plan is success",
        )?;
        ensure(
            InitStatus::Revalidated.is_success(),
            true,
            "revalidated is success",
        )?;
        ensure(
            InitStatus::Failed.is_success(),
            false,
            "failed is not success",
        )
    }

    #[test]
    fn init_dry_run_does_not_create_files() -> TestResult {
        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let workspace = temp_dir.path().to_path_buf();
        let options = InitOptions {
            workspace_path: workspace.clone(),
            dry_run: true,
            repair_plan: false,
            force: false,
            allow_symlink: false,
            skip_boilerplate: false,
        };

        let report = init_workspace(&options);

        ensure(report.status, InitStatus::DryRun, "status is dry_run")?;
        ensure(report.dry_run, true, "dry_run flag is true")?;
        ensure(
            workspace.join(".ee").exists(),
            false,
            ".ee dir should not exist after dry run",
        )
    }

    #[test]
    fn init_creates_ee_directory() -> TestResult {
        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let workspace = temp_dir.path().to_path_buf();
        let database_path = workspace.join(".ee").join("ee.db");
        let options = InitOptions {
            workspace_path: workspace.clone(),
            dry_run: false,
            repair_plan: false,
            force: false,
            allow_symlink: false,
            skip_boilerplate: true,
        };

        let report = init_workspace(&options);

        ensure(report.status, InitStatus::Created, "status is created")?;
        ensure(workspace.join(".ee").exists(), true, ".ee dir should exist")?;
        ensure(
            workspace.join(".ee").join("index").exists(),
            true,
            "index dir should exist",
        )?;
        ensure(database_path.exists(), true, "database file should exist")?;
        let database_action = report
            .actions
            .iter()
            .find(|action| action.action == "create_file" && action.path == database_path)
            .ok_or_else(|| "missing database create action".to_string())?;
        ensure(database_action.status, "created", "database action status")?;
        ensure(
            report
                .actions
                .iter()
                .any(|action| action.status == "failed"),
            false,
            "init should not report failed actions",
        )?;

        let connection =
            DbConnection::open_file(&database_path).map_err(|error| error.to_string())?;
        let workspace_key = workspace.to_string_lossy().to_string();
        let stored = connection
            .get_workspace_by_path(&workspace_key)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing workspace row".to_string())?;
        ensure(stored.id.starts_with("wsp_"), true, "workspace id prefix")?;
        ensure(stored.id.len(), 30, "workspace id length")?;

        Ok(())
    }

    #[test]
    fn init_is_idempotent() -> TestResult {
        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let workspace = temp_dir.path().to_path_buf();
        let options = InitOptions {
            workspace_path: workspace,
            dry_run: false,
            repair_plan: false,
            force: false,
            allow_symlink: false,
            skip_boilerplate: true,
        };

        let first_report = init_workspace(&options);
        ensure(
            first_report.status,
            InitStatus::Created,
            "first run creates",
        )?;
        ensure(
            first_report
                .actions
                .iter()
                .any(|action| action.status == "failed"),
            false,
            "first run has no failed actions",
        )?;

        let second_report = init_workspace(&options);
        ensure(
            second_report.status,
            InitStatus::AlreadyExists,
            "second run is already_exists",
        )?;
        ensure(
            second_report
                .actions
                .iter()
                .any(|action| action.status == "failed"),
            false,
            "second run has no failed actions",
        )?;

        Ok(())
    }

    #[test]
    fn init_report_json_has_required_fields() -> TestResult {
        let report = InitReport {
            version: "0.1.0",
            status: InitStatus::Created,
            workspace: PathBuf::from("/test/workspace"),
            ee_dir: PathBuf::from("/test/workspace/.ee"),
            database_path: PathBuf::from("/test/workspace/.ee/ee.db"),
            index_dir: PathBuf::from("/test/workspace/.ee/index"),
            actions: vec![],
            dry_run: false,
        };

        let json = report.data_json();

        ensure(
            json.get("command").and_then(|v| v.as_str()),
            Some("init"),
            "command field",
        )?;
        ensure(
            json.get("status").and_then(|v| v.as_str()),
            Some("created"),
            "status field",
        )?;
        ensure(
            json.get("dryRun").and_then(|v| v.as_bool()),
            Some(false),
            "dryRun field",
        )
    }

    #[test]
    fn init_report_toon_output_has_pipe_format() -> TestResult {
        let report = InitReport {
            version: "0.1.0",
            status: InitStatus::Created,
            workspace: PathBuf::from("/test/workspace"),
            ee_dir: PathBuf::from("/test/workspace/.ee"),
            database_path: PathBuf::from("/test/workspace/.ee/ee.db"),
            index_dir: PathBuf::from("/test/workspace/.ee/index"),
            actions: vec![],
            dry_run: false,
        };

        let toon = report.toon_output();

        ensure(toon.starts_with("INIT|"), true, "toon starts with INIT|")?;
        ensure(toon.contains("created"), true, "toon contains status")
    }

    #[test]
    fn init_repair_plan_mode() -> TestResult {
        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let workspace = temp_dir.path().to_path_buf();
        let options = InitOptions {
            workspace_path: workspace.clone(),
            dry_run: false,
            repair_plan: true,
            force: false,
            allow_symlink: false,
            skip_boilerplate: true,
        };

        let report = init_workspace(&options);

        ensure(
            report.status,
            InitStatus::RepairPlan,
            "status is repair_plan",
        )?;
        ensure(
            workspace.join(".ee").exists(),
            false,
            ".ee dir should not exist after repair_plan",
        )?;
        ensure(
            !report.actions.is_empty(),
            true,
            "repair_plan should have actions",
        )?;

        Ok(())
    }

    #[test]
    fn init_force_revalidates_existing() -> TestResult {
        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let workspace = temp_dir.path().to_path_buf();

        // First init to create workspace
        let options = InitOptions {
            workspace_path: workspace.clone(),
            dry_run: false,
            repair_plan: false,
            force: false,
            allow_symlink: false,
            skip_boilerplate: true,
        };
        let _ = init_workspace(&options);

        // Second init with force
        let force_options = InitOptions {
            workspace_path: workspace,
            dry_run: false,
            repair_plan: false,
            force: true,
            allow_symlink: false,
            skip_boilerplate: true,
        };
        let report = init_workspace(&force_options);

        ensure(
            report.status,
            InitStatus::Revalidated,
            "force on existing workspace returns revalidated",
        )?;

        Ok(())
    }

    #[test]
    fn init_creates_agent_boilerplate_by_default() -> TestResult {
        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let workspace = temp_dir.path().to_path_buf();
        let options = InitOptions {
            workspace_path: workspace.clone(),
            dry_run: false,
            repair_plan: false,
            force: false,
            allow_symlink: false,
            skip_boilerplate: false,
        };

        let report = init_workspace(&options);

        ensure(report.status, InitStatus::Created, "status is created")?;
        ensure(
            workspace.join("AGENTS.md").exists(),
            true,
            "AGENTS.md boilerplate should exist",
        )?;
        ensure(
            workspace.join("CLAUDE.md").exists(),
            true,
            "CLAUDE.md boilerplate should exist",
        )
    }

    #[test]
    fn boilerplate_creation_preserves_existing_file() -> TestResult {
        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let path = temp_dir.path().join("AGENTS.md");

        let first = create_boilerplate_file(&path, "first\n", false).map_err(|e| e.to_string())?;
        let second =
            create_boilerplate_file(&path, "second\n", false).map_err(|e| e.to_string())?;
        let contents = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;

        ensure(
            first,
            BoilerplateCreateStatus::Created,
            "first call creates",
        )?;
        ensure(
            second,
            BoilerplateCreateStatus::Exists,
            "second call observes existing file",
        )?;
        ensure(
            contents,
            "first\n".to_string(),
            "existing file contents are preserved",
        )
    }

    #[test]
    fn boilerplate_creation_rejects_existing_directory() -> TestResult {
        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let path = temp_dir.path().join("AGENTS.md");
        std::fs::create_dir(&path).map_err(|e| e.to_string())?;

        let error = create_boilerplate_file(&path, "contents\n", false)
            .expect_err("directory boilerplate path should reject");

        ensure(
            error.kind(),
            ErrorKind::InvalidInput,
            "non-regular boilerplate error kind",
        )?;
        ensure(
            error.to_string().contains("not a regular file"),
            true,
            "non-regular boilerplate error message",
        )?;
        ensure(path.is_dir(), true, "directory path remains untouched")
    }

    #[test]
    fn boilerplate_creation_allows_one_concurrent_creator() -> TestResult {
        use std::sync::{Arc, Barrier};

        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let path = Arc::new(temp_dir.path().join("AGENTS.md"));
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();

        for contents in ["first\n", "second\n"] {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                create_boilerplate_file(&path, contents, false)
            }));
        }

        let mut statuses = Vec::new();
        for handle in handles {
            let status = handle
                .join()
                .map_err(|_| "boilerplate writer thread panicked".to_string())?
                .map_err(|e| e.to_string())?;
            statuses.push(status);
        }

        let created_count = statuses
            .iter()
            .filter(|status| **status == BoilerplateCreateStatus::Created)
            .count();
        let exists_count = statuses
            .iter()
            .filter(|status| **status == BoilerplateCreateStatus::Exists)
            .count();
        let contents = std::fs::read_to_string(path.as_ref()).map_err(|e| e.to_string())?;

        ensure(created_count, 1, "exactly one writer creates the file")?;
        ensure(exists_count, 1, "exactly one writer observes existing file")?;
        ensure(
            contents == "first\n" || contents == "second\n",
            true,
            "file contains one complete winning write",
        )
    }

    #[test]
    fn init_skip_boilerplate_omits_agent_files() -> TestResult {
        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let workspace = temp_dir.path().to_path_buf();
        let options = InitOptions {
            workspace_path: workspace.clone(),
            dry_run: false,
            repair_plan: false,
            force: false,
            allow_symlink: false,
            skip_boilerplate: true,
        };

        let report = init_workspace(&options);

        ensure(report.status, InitStatus::Created, "status is created")?;
        ensure(
            workspace.join("AGENTS.md").exists(),
            false,
            "AGENTS.md boilerplate should be skipped",
        )?;
        ensure(
            workspace.join("CLAUDE.md").exists(),
            false,
            "CLAUDE.md boilerplate should be skipped",
        )
    }

    #[cfg(unix)]
    #[test]
    fn init_rejects_symlinked_workspace_by_default() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let real_workspace = temp_dir.path().join("real-workspace");
        let linked_workspace = temp_dir.path().join("linked-workspace");
        std::fs::create_dir(&real_workspace).map_err(|error| error.to_string())?;
        symlink(&real_workspace, &linked_workspace).map_err(|error| error.to_string())?;
        let options = InitOptions {
            workspace_path: linked_workspace,
            dry_run: false,
            repair_plan: false,
            force: false,
            allow_symlink: false,
            skip_boilerplate: true,
        };

        let report = init_workspace(&options);

        ensure(report.status, InitStatus::Failed, "symlink status")?;
        ensure(report.actions.len(), 1, "only workspace check is reported")?;
        ensure(
            report.actions[0].status,
            "symlink_refused",
            "workspace check status",
        )?;
        ensure(
            real_workspace.join(".ee").exists(),
            false,
            "init must not create metadata through a symlinked workspace",
        )?;
        ensure(
            real_workspace.join("AGENTS.md").exists(),
            false,
            "init must not write boilerplate through a symlinked workspace",
        )
    }

    #[cfg(unix)]
    #[test]
    fn init_allow_symlink_permits_symlinked_workspace() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let real_workspace = temp_dir.path().join("real-workspace");
        let linked_workspace = temp_dir.path().join("linked-workspace");
        std::fs::create_dir(&real_workspace).map_err(|error| error.to_string())?;
        symlink(&real_workspace, &linked_workspace).map_err(|error| error.to_string())?;
        let options = InitOptions {
            workspace_path: linked_workspace,
            dry_run: false,
            repair_plan: false,
            force: false,
            allow_symlink: true,
            skip_boilerplate: true,
        };

        let report = init_workspace(&options);

        ensure(report.status, InitStatus::Created, "allowed symlink status")?;
        ensure(
            real_workspace.join(".ee").join("ee.db").exists(),
            true,
            "allow-symlink should keep the explicit opt-in behavior",
        )
    }

    #[cfg(unix)]
    #[test]
    fn init_rejects_symlinked_metadata_directory() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let workspace = temp_dir.path().join("workspace");
        let outside_metadata = temp_dir.path().join("outside-metadata");
        std::fs::create_dir(&workspace).map_err(|error| error.to_string())?;
        std::fs::create_dir(&outside_metadata).map_err(|error| error.to_string())?;
        symlink(&outside_metadata, workspace.join(".ee")).map_err(|error| error.to_string())?;
        let options = InitOptions {
            workspace_path: workspace.clone(),
            dry_run: false,
            repair_plan: false,
            force: false,
            allow_symlink: false,
            skip_boilerplate: false,
        };

        let report = init_workspace(&options);

        ensure(report.status, InitStatus::Failed, "symlinked .ee status")?;
        ensure(
            report
                .actions
                .iter()
                .any(|action| action.status == "symlink_refused"),
            true,
            "symlinked .ee should be reported",
        )?;
        ensure(
            outside_metadata.join("ee.db").exists(),
            false,
            "init must not create database through a symlinked .ee directory",
        )?;
        ensure(
            outside_metadata.join("index").exists(),
            false,
            "init must not create index through a symlinked .ee directory",
        )?;
        ensure(
            workspace.join("AGENTS.md").exists(),
            false,
            "failed init must not leave partial boilerplate files",
        )
    }
}
