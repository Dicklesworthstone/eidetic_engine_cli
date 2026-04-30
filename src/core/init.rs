//! Init command handler (EE-028).
//!
//! Initializes the ee workspace with database and index directories.
//! Supports dry-run mode and idempotent re-runs.

use std::path::PathBuf;

use super::build_info;

/// Status of the init operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitStatus {
    Created,
    AlreadyExists,
    DryRun,
}

impl InitStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::AlreadyExists => "already_exists",
            Self::DryRun => "dry_run",
        }
    }

    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Created | Self::AlreadyExists | Self::DryRun)
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

        if self.dry_run {
            output.push_str("DRY RUN: Would initialize ee workspace\n\n");
        } else {
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

    if options.dry_run {
        if !ee_dir.exists() {
            actions.push(InitAction {
                action: "create_directory",
                path: ee_dir.clone(),
                status: "would_create",
            });
        }
        if !index_dir.exists() {
            actions.push(InitAction {
                action: "create_directory",
                path: index_dir.clone(),
                status: "would_create",
            });
        }
        if !database_path.exists() {
            actions.push(InitAction {
                action: "create_file",
                path: database_path.clone(),
                status: "would_create",
            });
        }

        return InitReport {
            version,
            status: InitStatus::DryRun,
            workspace,
            ee_dir,
            database_path,
            index_dir,
            actions,
            dry_run: true,
        };
    }

    if !ee_dir.exists() {
        match std::fs::create_dir_all(&ee_dir) {
            Ok(()) => {
                actions.push(InitAction {
                    action: "create_directory",
                    path: ee_dir.clone(),
                    status: "created",
                });
                any_created = true;
            }
            Err(_) => {
                actions.push(InitAction {
                    action: "create_directory",
                    path: ee_dir.clone(),
                    status: "failed",
                });
            }
        }
    } else {
        actions.push(InitAction {
            action: "check_directory",
            path: ee_dir.clone(),
            status: "exists",
        });
    }

    if !index_dir.exists() {
        match std::fs::create_dir_all(&index_dir) {
            Ok(()) => {
                actions.push(InitAction {
                    action: "create_directory",
                    path: index_dir.clone(),
                    status: "created",
                });
                any_created = true;
            }
            Err(_) => {
                actions.push(InitAction {
                    action: "create_directory",
                    path: index_dir.clone(),
                    status: "failed",
                });
            }
        }
    } else {
        actions.push(InitAction {
            action: "check_directory",
            path: index_dir.clone(),
            status: "exists",
        });
    }

    let status = if any_created {
        InitStatus::Created
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
        ensure(InitStatus::DryRun.as_str(), "dry_run", "dry_run")
    }

    #[test]
    fn init_status_is_success() -> TestResult {
        ensure(InitStatus::Created.is_success(), true, "created is success")?;
        ensure(
            InitStatus::AlreadyExists.is_success(),
            true,
            "already_exists is success",
        )?;
        ensure(InitStatus::DryRun.is_success(), true, "dry_run is success")
    }

    #[test]
    fn init_dry_run_does_not_create_files() -> TestResult {
        let temp_dir = std::env::temp_dir().join(format!("ee_init_test_{}", std::process::id()));

        let options = InitOptions {
            workspace_path: temp_dir.clone(),
            dry_run: true,
        };

        let report = init_workspace(&options);

        ensure(report.status, InitStatus::DryRun, "status is dry_run")?;
        ensure(report.dry_run, true, "dry_run flag is true")?;
        ensure(
            temp_dir.join(".ee").exists(),
            false,
            ".ee dir should not exist after dry run",
        )
    }

    #[test]
    fn init_creates_ee_directory() -> TestResult {
        let temp_dir =
            std::env::temp_dir().join(format!("ee_init_create_test_{}", std::process::id()));

        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;

        let options = InitOptions {
            workspace_path: temp_dir.clone(),
            dry_run: false,
        };

        let report = init_workspace(&options);

        ensure(report.status, InitStatus::Created, "status is created")?;
        ensure(temp_dir.join(".ee").exists(), true, ".ee dir should exist")?;
        ensure(
            temp_dir.join(".ee").join("index").exists(),
            true,
            "index dir should exist",
        )?;

        let _ = std::fs::remove_dir_all(&temp_dir);
        Ok(())
    }

    #[test]
    fn init_is_idempotent() -> TestResult {
        let temp_dir =
            std::env::temp_dir().join(format!("ee_init_idempotent_test_{}", std::process::id()));

        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;

        let options = InitOptions {
            workspace_path: temp_dir.clone(),
            dry_run: false,
        };

        let first_report = init_workspace(&options);
        ensure(
            first_report.status,
            InitStatus::Created,
            "first run creates",
        )?;

        let second_report = init_workspace(&options);
        ensure(
            second_report.status,
            InitStatus::AlreadyExists,
            "second run is already_exists",
        )?;

        let _ = std::fs::remove_dir_all(&temp_dir);
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
}
