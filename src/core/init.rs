//! Init command handler (EE-028).
//!
//! Initializes the ee workspace with database and index directories.
//! Supports dry-run mode and idempotent re-runs.

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, Write},
    path::{Path, PathBuf},
};

use super::{
    build_info,
    index::{
        IndexRebuildOptions, IndexRebuildStatus, index_corpus_compatibility_is_current,
        rebuild_index,
    },
    workspace::stable_workspace_id,
};
use crate::db::{CreateWorkspaceInput, DbConnection};
use crate::policy::store_auth::{StoreAuthRoot, workspace_keys_dir};

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

/// Detailed reason for an init action that failed or was refused.
#[derive(Clone, Debug)]
pub struct InitActionError {
    pub action: &'static str,
    pub path: PathBuf,
    pub status: &'static str,
    pub message: String,
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
    pub action_errors: Vec<InitActionError>,
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

        // Tell the agent up front whether semantic retrieval is live; on a
        // clean machine it degrades to lexical-only hash fallback, and the
        // agent should know to enable it rather than silently getting weaker
        // recall. (agent-UX item 6)
        match crate::core::search::semantic_retrieval_unavailable_reason() {
            Some(reason) => {
                output.push_str(&format!(
                    "  Semantic retrieval: OFF — lexical-only ({reason})\n    Enable: {}\n",
                    crate::core::search::SEMANTIC_ENABLE_HINT
                ));
            }
            None => output.push_str("  Semantic retrieval: ready\n"),
        }

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

        if !self.action_errors.is_empty() {
            output.push_str("\nFailure details:\n");
            for error in &self.action_errors {
                output.push_str(&format!(
                    "  {} {} ({}): {}\n",
                    error.action,
                    error.path.display(),
                    error.status,
                    error.message
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
        let action_errors: Vec<serde_json::Value> = self
            .action_errors
            .iter()
            .map(|error| {
                serde_json::json!({
                    "action": error.action,
                    "path": error.path.display().to_string(),
                    "status": error.status,
                    "message": error.message,
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
            "actionErrors": action_errors,
            "dryRun": self.dry_run,
            // agent-UX item 6: onboarding-time semantic posture so harnesses
            // can branch on whether retrieval is full-hybrid or lexical-only.
            "semanticRetrieval": crate::core::search::semantic_retrieval_unavailable_reason()
                .map_or_else(
                    || serde_json::json!({ "enabled": true }),
                    |reason| serde_json::json!({
                        "enabled": false,
                        "reason": reason,
                        "enable": crate::core::search::SEMANTIC_ENABLE_HINT,
                    }),
                ),
        })
    }
}

/// Lexically normalize a workspace path without touching the filesystem.
///
/// `--workspace .` joins to `current_dir()/.` which then renders as
/// `/proj/./.ee`, `/proj/./ee.db`, etc. in init/status output. The dir may
/// not exist yet during init, so we can't `canonicalize()`; instead we strip
/// `CurDir` (`.`) components and collapse `ParentDir` (`..`) against a
/// preceding normal component, preserving root/prefix semantics.
fn normalize_workspace_path(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::{Component, PathBuf};
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir
                if matches!(out.components().next_back(), Some(Component::Normal(_))) =>
            {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

fn path_without_trailing_separators(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        out.push(component.as_os_str());
    }
    if out.as_os_str().is_empty() {
        path.to_path_buf()
    } else {
        out
    }
}

/// Resolve harmless ancestor symlinks before applying no-follow init guards.
///
/// On macOS `/tmp` is normally a symlink to `/private/tmp`; agents naturally
/// create scratch workspaces there. Refusing those paths forces `realpath`
/// busywork even though init can safely operate on the canonical target. A
/// final symlink remains refused unless `--allow-symlink` is explicit.
fn canonicalize_init_workspace_for_no_follow(path: &Path, allow_symlink: bool) -> PathBuf {
    if allow_symlink {
        return path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    }

    let final_component_path = path_without_trailing_separators(path);
    if matches!(
        fs::symlink_metadata(&final_component_path),
        Ok(metadata) if metadata.file_type().is_symlink()
    ) {
        return path.to_path_buf();
    }

    if path.exists() {
        return path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    }

    let mut ancestor = path;
    let mut missing_suffix = Vec::new();
    while let Some(parent) = ancestor.parent() {
        let Some(leaf) = ancestor.file_name() else {
            break;
        };
        missing_suffix.push(leaf.to_os_string());
        ancestor = parent;
        if ancestor.exists() {
            return ancestor
                .canonicalize()
                .map(|mut canonical| {
                    for component in missing_suffix.iter().rev() {
                        canonical.push(component);
                    }
                    canonical
                })
                .unwrap_or_else(|_| path.to_path_buf());
        }
    }

    path.to_path_buf()
}

fn record_failed_init_action(
    actions: &mut Vec<InitAction>,
    action_errors: &mut Vec<InitActionError>,
    action: &'static str,
    path: PathBuf,
    status: &'static str,
    message: impl Into<String>,
) {
    actions.push(InitAction {
        action,
        path: path.clone(),
        status,
    });
    action_errors.push(InitActionError {
        action,
        path,
        status,
        message: message.into(),
    });
}

/// Initialize the ee workspace.
///
/// Creates the .ee directory and database, then publishes a ready search index
/// if one does not exist. Idempotent: returns success if already initialized.
///
/// Modes:
/// - `dry_run`: Report what would be done without creating files
/// - `repair_plan`: Report non-destructive repair actions for existing workspaces
/// - `force`: Force revalidation/recreation of EE-owned artifacts
#[must_use]
pub fn init_workspace(options: &InitOptions) -> InitReport {
    let version = build_info().version;

    let workspace = if options.workspace_path.is_absolute() {
        normalize_workspace_path(&options.workspace_path)
    } else {
        normalize_workspace_path(
            &std::env::current_dir()
                .unwrap_or_default()
                .join(&options.workspace_path),
        )
    };
    let workspace = canonicalize_init_workspace_for_no_follow(&workspace, options.allow_symlink);

    let ee_dir = workspace.join(".ee");
    let database_path = ee_dir.join("ee.db");
    let index_dir = ee_dir.join("index");

    let mut actions = Vec::new();
    let mut action_errors = Vec::new();
    let mut any_created = false;
    let mut any_failed = false;

    if let Some(issue) = init_path_safety_issue(&workspace, options.allow_symlink) {
        record_failed_init_action(
            &mut actions,
            &mut action_errors,
            "check_workspace",
            workspace.clone(),
            issue.status,
            issue.message,
        );
        return InitReport {
            version,
            status: InitStatus::Failed,
            workspace,
            ee_dir,
            database_path,
            index_dir,
            actions,
            action_errors,
            dry_run: options.dry_run,
        };
    }

    // Repair plan mode: report what could be fixed without making changes
    if options.repair_plan {
        let mut repair_actions = Vec::new();

        if let Some(issue) = init_path_safety_issue(&ee_dir, options.allow_symlink) {
            record_failed_init_action(
                &mut repair_actions,
                &mut action_errors,
                "check_directory",
                ee_dir.clone(),
                issue.status,
                issue.message,
            );
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

        if let Some(issue) = init_path_safety_issue(&index_dir, options.allow_symlink) {
            record_failed_init_action(
                &mut repair_actions,
                &mut action_errors,
                "check_directory",
                index_dir.clone(),
                issue.status,
                issue.message,
            );
        } else if !index_corpus_compatibility_is_current(&index_dir) {
            repair_actions.push(InitAction {
                action: "initialize_index",
                path: index_dir.clone(),
                status: "missing",
            });
        } else {
            repair_actions.push(InitAction {
                action: "check_index",
                path: index_dir.clone(),
                status: "compatible",
            });
        }

        if let Some(issue) = init_path_safety_issue(&database_path, options.allow_symlink) {
            record_failed_init_action(
                &mut repair_actions,
                &mut action_errors,
                "check_file",
                database_path.clone(),
                issue.status,
                issue.message,
            );
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
            action_errors,
            dry_run: false,
        };
    }

    if options.dry_run {
        if let Some(issue) = init_path_safety_issue(&ee_dir, options.allow_symlink) {
            record_failed_init_action(
                &mut actions,
                &mut action_errors,
                "check_directory",
                ee_dir.clone(),
                issue.status,
                issue.message,
            );
        } else if !ee_dir.exists() {
            actions.push(InitAction {
                action: "create_directory",
                path: ee_dir.clone(),
                status: "would_create",
            });
        }
        if let Some(issue) = init_path_safety_issue(&index_dir, options.allow_symlink) {
            record_failed_init_action(
                &mut actions,
                &mut action_errors,
                "check_directory",
                index_dir.clone(),
                issue.status,
                issue.message,
            );
        } else if !index_corpus_compatibility_is_current(&index_dir) {
            actions.push(InitAction {
                action: "initialize_index",
                path: index_dir.clone(),
                status: "would_create",
            });
        }
        if let Some(issue) = init_path_safety_issue(&database_path, options.allow_symlink) {
            record_failed_init_action(
                &mut actions,
                &mut action_errors,
                "check_file",
                database_path.clone(),
                issue.status,
                issue.message,
            );
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
            action_errors,
            dry_run: true,
        };
    }

    if let Some(issue) = init_path_safety_issue(&ee_dir, options.allow_symlink) {
        record_failed_init_action(
            &mut actions,
            &mut action_errors,
            "check_directory",
            ee_dir.clone(),
            issue.status,
            issue.message,
        );
        any_failed = true;
    } else if !ee_dir.exists() {
        match fs::create_dir_all(&ee_dir) {
            Ok(()) => {
                if let Some(issue) = init_path_safety_issue(&ee_dir, options.allow_symlink) {
                    record_failed_init_action(
                        &mut actions,
                        &mut action_errors,
                        "create_directory",
                        ee_dir.clone(),
                        issue.status,
                        issue.message,
                    );
                    any_failed = true;
                } else if let Err(error) =
                    harden_init_directory_mode(&ee_dir, options.allow_symlink)
                {
                    record_failed_init_action(
                        &mut actions,
                        &mut action_errors,
                        "create_directory",
                        ee_dir.clone(),
                        "failed",
                        format!("failed to harden directory permissions: {error}"),
                    );
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
            Err(error) => {
                record_failed_init_action(
                    &mut actions,
                    &mut action_errors,
                    "create_directory",
                    ee_dir.clone(),
                    "failed",
                    format!("failed to create directory: {error}"),
                );
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

    let (rebuild_initial_index, index_was_compatible) =
        if let Some(issue) = init_path_safety_issue(&index_dir, options.allow_symlink) {
            record_failed_init_action(
                &mut actions,
                &mut action_errors,
                "check_directory",
                index_dir.clone(),
                issue.status,
                issue.message,
            );
            any_failed = true;
            (false, false)
        } else {
            let index_was_compatible = index_corpus_compatibility_is_current(&index_dir);
            let rebuild_initial_index = options.force || !index_was_compatible;
            if !rebuild_initial_index {
                actions.push(InitAction {
                    action: "check_index",
                    path: index_dir.clone(),
                    status: "compatible",
                });
            }
            (rebuild_initial_index, index_was_compatible)
        };

    if let Some(issue) = init_path_safety_issue(&database_path, options.allow_symlink) {
        record_failed_init_action(
            &mut actions,
            &mut action_errors,
            "check_file",
            database_path.clone(),
            issue.status,
            issue.message,
        );
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
            Err(error) => {
                record_failed_init_action(
                    &mut actions,
                    &mut action_errors,
                    "create_file",
                    database_path.clone(),
                    "failed",
                    error,
                );
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
            Err(error) => record_failed_init_action(
                &mut actions,
                &mut action_errors,
                "check_file",
                database_path.clone(),
                "failed",
                error,
            ),
        }
        if actions
            .last()
            .is_some_and(|action| action.status == "failed")
        {
            any_failed = true;
        }
    }

    if !any_failed && rebuild_initial_index {
        let rebuild = rebuild_index(&IndexRebuildOptions {
            workspace_path: workspace.clone(),
            database_path: Some(database_path.clone()),
            index_dir: Some(index_dir.clone()),
            dry_run: false,
        });
        match rebuild {
            Ok(report)
                if matches!(
                    report.status,
                    IndexRebuildStatus::Success | IndexRebuildStatus::NoDocuments
                ) =>
            {
                if let Err(error) = harden_init_directory_mode(&index_dir, options.allow_symlink) {
                    record_failed_init_action(
                        &mut actions,
                        &mut action_errors,
                        "initialize_index",
                        index_dir.clone(),
                        "failed",
                        format!(
                            "search index was published but its directory permissions could not be hardened: {error}; run `ee index rebuild --workspace .` after correcting the filesystem permissions"
                        ),
                    );
                    any_failed = true;
                } else {
                    actions.push(InitAction {
                        action: "initialize_index",
                        path: index_dir.clone(),
                        status: if index_was_compatible {
                            "revalidated"
                        } else {
                            "ready"
                        },
                    });
                    if !index_was_compatible {
                        any_created = true;
                    }
                }
            }
            Ok(report) => {
                let detail = if report.errors.is_empty() {
                    format!("index bootstrap returned {}", report.status.as_str())
                } else {
                    report.errors.join("; ")
                };
                record_failed_init_action(
                    &mut actions,
                    &mut action_errors,
                    "initialize_index",
                    index_dir.clone(),
                    "failed",
                    format!(
                        "failed to initialize search index: {detail}; run `ee index rebuild --workspace .`"
                    ),
                );
                any_failed = true;
            }
            Err(error) => {
                record_failed_init_action(
                    &mut actions,
                    &mut action_errors,
                    "initialize_index",
                    index_dir.clone(),
                    "failed",
                    format!(
                        "failed to initialize search index: {error}; run `ee index rebuild --workspace .`"
                    ),
                );
                any_failed = true;
            }
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
            Err(error) => {
                record_failed_init_action(
                    &mut actions,
                    &mut action_errors,
                    "create_file",
                    agents_path,
                    "failed",
                    format!("failed to create AGENTS.md boilerplate: {error}"),
                );
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
            Err(error) => {
                record_failed_init_action(
                    &mut actions,
                    &mut action_errors,
                    "create_file",
                    claude_path,
                    "failed",
                    format!("failed to create CLAUDE.md boilerplate: {error}"),
                );
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
        action_errors,
        dry_run: false,
    }
}

fn initialize_database(
    database_path: &PathBuf,
    workspace_path: &Path,
    allow_symlink: bool,
) -> Result<(), String> {
    prepare_init_database_path(database_path, allow_symlink)?;
    let connection = DbConnection::open_file(database_path)
        .map_err(|error| format!("failed to open database: {error}"))?;
    connection
        .migrate()
        .map_err(|error| format!("failed to migrate database: {error}"))?;
    harden_init_database_mode(database_path, allow_symlink)
        .map_err(|error| format!("failed to harden database permissions: {error}"))?;

    // Create workspace row if it doesn't exist (idempotent).
    let workspace_key = workspace_path.to_string_lossy().to_string();
    let existing = connection
        .get_workspace_by_path(&workspace_key)
        .map_err(|error| format!("failed to check workspace: {error}"))?;
    let workspace_id = if let Some(existing) = existing {
        existing.id
    } else {
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
        workspace_id
    };

    super::model::ensure_bundled_embedding_model_registered(&connection, &workspace_id)
        .map_err(|error| format!("failed to register bundled embedding model: {error}"))?;

    // Approval-token issuance is a read-only preview operation and therefore
    // must never create authentication material as a side effect. Establish
    // the hardened store-local root during explicit workspace initialization
    // instead; idempotent init also repairs older initialized workspaces that
    // predate this key.
    StoreAuthRoot::open_or_create(workspace_keys_dir(workspace_path))
        .map_err(|error| format!("failed to initialize store authentication root: {error}"))?;

    Ok(())
}

fn prepare_init_database_path(path: &Path, allow_symlink: bool) -> Result<(), String> {
    ensure_init_path_has_no_symlink_components(path, allow_symlink)?;
    if allow_symlink {
        return Ok(());
    }
    let file = open_init_database_file_no_follow(path)
        .map_err(|error| format!("failed to prepare database file safely: {error}"))?;
    set_init_file_permissions(&file, 0o600)
        .map_err(|error| format!("failed to harden database permissions: {error}"))
}

/// Tighten ee storage directory permissions to owner-only (0700) on Unix.
///
/// `.ee/` and `.ee/index/` hold workspace identity state and the search
/// index; under umask 022 these would be created 0755 (group/world
/// readable). The `audit-security-2-2026-05-20` review found this leaks
/// data-at-rest even before any other agent writes secrets into them, so
/// init now hardens both directory roots up front.
#[cfg(unix)]
fn harden_init_directory_mode(path: &Path, allow_symlink: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if allow_symlink {
        return fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
    let directory = open_init_directory_no_follow(path)?;
    set_init_file_permissions(&directory, 0o700)
}

#[cfg(not(unix))]
fn harden_init_directory_mode(_path: &Path, _allow_symlink: bool) -> io::Result<()> {
    Ok(())
}

/// Tighten the ee SQLite database file permissions to owner-only (0600) on
/// Unix. `.ee/ee.db` stores memories, the audit log, preflight token
/// hashes, and other identity-bearing state; under umask 022 it would be
/// created 0644 (group/world readable). Init now restricts the file before
/// returning a successful workspace setup.
#[cfg(unix)]
fn harden_init_database_mode(path: &Path, allow_symlink: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if allow_symlink {
        return fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    let database = open_init_database_file_no_follow(path)?;
    set_init_file_permissions(&database, 0o600)
}

#[cfg(not(unix))]
fn harden_init_database_mode(_path: &Path, _allow_symlink: bool) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_init_file_permissions(file: &File, mode: u32) -> io::Result<()> {
    // `rustix::fs::Mode::from_raw_mode` takes a `RawMode` that aliases
    // the platform's POSIX `mode_t` and is therefore target-dependent:
    // `u32` on Linux (both the `linux_raw` and libc/glibc backends use
    // `ffi::c_uint`) and `u16` on macOS / BSDs (where `c::mode_t` is
    // narrower). Pinning a fixed integer width here breaks the build on
    // whichever platform doesn't match — `u16` would fail to compile on
    // Linux (`expected u32, found u16` at the call site) and `u32`
    // would fail to compile on macOS. Convert through the
    // platform-aliased `rustix::fs::RawMode` so the same call site
    // works on every supported target: on Linux `try_into` is the
    // identity `u32 → u32`, on macOS it is the range-checked
    // `u32 → u16` (callers always pass values ≤ 0o777, which fit).
    let raw: rustix::fs::RawMode = mode.try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("init file mode {mode:#o} does not fit in the target platform's mode_t"),
        )
    })?;
    rustix::fs::fchmod(file, rustix::fs::Mode::from_raw_mode(raw)).map_err(io::Error::from)
}

#[cfg(not(unix))]
fn set_init_file_permissions(_file: &File, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn open_init_database_file_no_follow(path: &Path) -> io::Result<File> {
    open_init_leaf_no_follow(
        path,
        rustix::fs::OFlags::RDWR | rustix::fs::OFlags::CREATE,
        rustix::fs::Mode::from_raw_mode(0o600),
    )
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn open_init_database_file_no_follow(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn open_init_directory_no_follow(path: &Path) -> io::Result<File> {
    open_init_leaf_no_follow(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
        rustix::fs::Mode::from_raw_mode(0),
    )
}

#[cfg(not(all(unix, not(any(target_os = "espidf", target_os = "horizon")))))]
fn open_init_directory_no_follow(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).open(path)
}

fn open_init_boilerplate_file(path: &Path, allow_symlink: bool) -> io::Result<File> {
    #[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
    if !allow_symlink {
        return open_init_leaf_no_follow(
            path,
            rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::CREATE | rustix::fs::OFlags::EXCL,
            rustix::fs::Mode::from_raw_mode(0o600),
        );
    }

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    options.open(path)
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn open_init_leaf_no_follow(
    path: &Path,
    flags: rustix::fs::OFlags,
    create_mode: rustix::fs::Mode,
) -> io::Result<File> {
    let (parent, leaf) = open_init_parent_directory_no_follow(path)?;
    let fd = rustix::fs::openat(
        &parent,
        leaf.as_os_str(),
        flags | rustix::fs::OFlags::NOFOLLOW,
        create_mode,
    )
    .map_err(io::Error::from)?;
    Ok(File::from(fd))
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn open_init_parent_directory_no_follow(path: &Path) -> io::Result<(File, OsString)> {
    let leaf = path.file_name().map(OsString::from).ok_or_else(|| {
        io::Error::new(
            ErrorKind::InvalidInput,
            format!("init path {} has no final component", path.display()),
        )
    })?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok((open_init_directory_chain_no_follow(parent)?, leaf))
}

#[cfg(all(unix, not(any(target_os = "espidf", target_os = "horizon"))))]
fn open_init_directory_chain_no_follow(path: &Path) -> io::Result<File> {
    use std::path::Component;

    let directory_flags =
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::NOFOLLOW;
    let mut directory = if path.is_absolute() {
        let fd = rustix::fs::openat(
            rustix::fs::CWD,
            Path::new("/"),
            directory_flags,
            rustix::fs::Mode::from_raw_mode(0),
        )
        .map_err(io::Error::from)?;
        File::from(fd)
    } else {
        let fd = rustix::fs::openat(
            rustix::fs::CWD,
            Path::new("."),
            directory_flags,
            rustix::fs::Mode::from_raw_mode(0),
        )
        .map_err(io::Error::from)?;
        File::from(fd)
    };

    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(_) | Component::ParentDir => {
                let fd = rustix::fs::openat(
                    &directory,
                    component,
                    directory_flags,
                    rustix::fs::Mode::from_raw_mode(0),
                )
                .map_err(io::Error::from)?;
                directory = File::from(fd);
            }
            Component::Prefix(_) => {
                return Err(io::Error::new(
                    ErrorKind::InvalidInput,
                    format!("unsupported init path prefix in {}", path.display()),
                ));
            }
        }
    }

    Ok(directory)
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
    let mut file = match open_init_boilerplate_file(path, allow_symlink) {
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

#[derive(Clone, Debug)]
struct InitPathSafetyIssue {
    status: &'static str,
    message: String,
}

fn init_path_safety_issue(path: &Path, allow_symlink: bool) -> Option<InitPathSafetyIssue> {
    if allow_symlink {
        return None;
    }
    match init_path_has_symlink_component(path) {
        Ok(false) => None,
        Ok(true) => Some(InitPathSafetyIssue {
            status: "symlink_refused",
            message: format!(
                "refusing to initialize {} because the path traverses a symlink",
                path.display()
            ),
        }),
        Err(error) => Some(InitPathSafetyIssue {
            status: "inspect_failed",
            message: format!(
                "failed to inspect init path {} for symlink safety: {error}",
                path.display()
            ),
        }),
    }
}

fn init_path_safety_status(path: &Path, allow_symlink: bool) -> Option<&'static str> {
    init_path_safety_issue(path, allow_symlink).map(|issue| issue.status)
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
    super::path_safety::path_has_symlink_component(path)
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
        let index_dir = workspace.join(".ee").join("index");
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
        ensure(index_dir.exists(), true, "index dir should exist")?;
        ensure(database_path.exists(), true, "database file should exist")?;
        let database_action = report
            .actions
            .iter()
            .find(|action| action.action == "create_file" && action.path == database_path)
            .ok_or_else(|| "missing database create action".to_string())?;
        ensure(database_action.status, "created", "database action status")?;
        let index_action = report
            .actions
            .iter()
            .find(|action| action.action == "initialize_index" && action.path == index_dir)
            .ok_or_else(|| "missing search-index initialization action".to_string())?;
        ensure(index_action.status, "ready", "search index action status")?;
        ensure(
            report
                .actions
                .iter()
                .any(|action| action.status == "failed"),
            false,
            "init should not report failed actions",
        )?;

        let index_status =
            crate::core::index::get_index_status(&crate::core::index::IndexStatusOptions {
                workspace_path: workspace.clone(),
                database_path: Some(database_path.clone()),
                index_dir: Some(index_dir),
            })
            .map_err(|error| error.to_string())?;
        ensure(
            index_status.health,
            crate::core::index::IndexHealth::Ready,
            "fresh index health",
        )?;
        ensure(index_status.index_exists, true, "fresh index exists")?;
        ensure(
            index_status.index_document_count,
            Some(0),
            "fresh index document count",
        )?;
        let document_counts = index_status
            .index_document_counts
            .ok_or_else(|| "fresh index is missing per-source document counts".to_string())?;
        ensure(document_counts.total(), 0, "fresh index source counts")?;
        ensure(
            index_status.index_generation,
            index_status.db_generation,
            "fresh index generation matches database",
        )?;
        ensure(
            index_status.actual_corpus_revision.as_deref(),
            Some(index_status.expected_corpus_revision.as_str()),
            "fresh index corpus revision",
        )?;
        ensure(index_status.repair_hint, None, "fresh index repair hint")?;
        ensure(
            index_status.last_check_error,
            None,
            "fresh index check error",
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

    #[cfg(unix)]
    #[test]
    fn init_creates_storage_with_owner_only_permissions() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

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
        ensure(report.status, InitStatus::Created, "init created status")?;

        let ee_mode = fs::metadata(workspace.join(".ee"))
            .map_err(|error| error.to_string())?
            .permissions()
            .mode()
            & 0o777;
        ensure(ee_mode, 0o700, ".ee directory mode must be 0700")?;

        let index_mode = fs::metadata(workspace.join(".ee").join("index"))
            .map_err(|error| error.to_string())?
            .permissions()
            .mode()
            & 0o777;
        ensure(index_mode, 0o700, ".ee/index directory mode must be 0700")?;

        let database_mode = fs::metadata(&database_path)
            .map_err(|error| error.to_string())?
            .permissions()
            .mode()
            & 0o777;
        ensure(database_mode, 0o600, ".ee/ee.db mode must be 0600")
    }

    #[cfg(unix)]
    #[test]
    fn init_directory_hardening_refuses_final_symlink() -> TestResult {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let target_dir = temp_dir.path().join("target-ee");
        let linked_dir = temp_dir.path().join("linked-ee");
        fs::create_dir(&target_dir).map_err(|error| error.to_string())?;
        fs::set_permissions(&target_dir, fs::Permissions::from_mode(0o755))
            .map_err(|error| error.to_string())?;
        symlink(&target_dir, &linked_dir).map_err(|error| error.to_string())?;

        let error = harden_init_directory_mode(&linked_dir, false)
            .expect_err("directory hardening must refuse a final symlink");
        ensure(
            error.kind() != ErrorKind::NotFound,
            true,
            "final symlink refusal error should come from the symlinked path",
        )?;
        let target_mode = fs::metadata(&target_dir)
            .map_err(|error| error.to_string())?
            .permissions()
            .mode()
            & 0o777;
        ensure(
            target_mode,
            0o755,
            "directory hardening must not chmod the symlink target",
        )
    }

    #[cfg(unix)]
    #[test]
    fn init_database_hardening_refuses_final_symlink() -> TestResult {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let target_database = temp_dir.path().join("target.db");
        let linked_database = temp_dir.path().join("ee.db");
        fs::write(&target_database, b"not sqlite").map_err(|error| error.to_string())?;
        fs::set_permissions(&target_database, fs::Permissions::from_mode(0o644))
            .map_err(|error| error.to_string())?;
        symlink(&target_database, &linked_database).map_err(|error| error.to_string())?;

        let error = harden_init_database_mode(&linked_database, false)
            .expect_err("database hardening must refuse a final symlink");
        ensure(
            error.kind() != ErrorKind::NotFound,
            true,
            "final symlink refusal error should come from the symlinked path",
        )?;
        let target_mode = fs::metadata(&target_database)
            .map_err(|error| error.to_string())?
            .permissions()
            .mode()
            & 0o777;
        ensure(
            target_mode,
            0o644,
            "database hardening must not chmod the symlink target",
        )
    }

    #[cfg(unix)]
    #[test]
    fn init_directory_hardening_refuses_parent_symlink_swap() -> TestResult {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let real_workspace = temp_dir.path().join("real-workspace");
        let linked_workspace = temp_dir.path().join("linked-workspace");
        let target_dir = real_workspace.join(".ee");
        fs::create_dir(&real_workspace).map_err(|error| error.to_string())?;
        fs::create_dir(&target_dir).map_err(|error| error.to_string())?;
        fs::set_permissions(&target_dir, fs::Permissions::from_mode(0o755))
            .map_err(|error| error.to_string())?;
        symlink(&real_workspace, &linked_workspace).map_err(|error| error.to_string())?;

        let error = harden_init_directory_mode(&linked_workspace.join(".ee"), false)
            .expect_err("directory hardening must refuse a swapped parent symlink");
        ensure(
            error.kind() != ErrorKind::NotFound,
            true,
            "parent symlink refusal error should come from the symlinked path",
        )?;
        let target_mode = fs::metadata(&target_dir)
            .map_err(|error| error.to_string())?
            .permissions()
            .mode()
            & 0o777;
        ensure(
            target_mode,
            0o755,
            "directory hardening must not chmod through a parent symlink",
        )
    }

    #[cfg(unix)]
    #[test]
    fn init_database_hardening_refuses_parent_symlink_swap() -> TestResult {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let real_workspace = temp_dir.path().join("real-workspace");
        let linked_workspace = temp_dir.path().join("linked-workspace");
        let ee_dir = real_workspace.join(".ee");
        let target_database = ee_dir.join("ee.db");
        fs::create_dir(&real_workspace).map_err(|error| error.to_string())?;
        fs::create_dir(&ee_dir).map_err(|error| error.to_string())?;
        fs::write(&target_database, b"not sqlite").map_err(|error| error.to_string())?;
        fs::set_permissions(&target_database, fs::Permissions::from_mode(0o644))
            .map_err(|error| error.to_string())?;
        symlink(&real_workspace, &linked_workspace).map_err(|error| error.to_string())?;

        let error = harden_init_database_mode(&linked_workspace.join(".ee").join("ee.db"), false)
            .expect_err("database hardening must refuse a swapped parent symlink");
        ensure(
            error.kind() != ErrorKind::NotFound,
            true,
            "parent symlink refusal error should come from the symlinked path",
        )?;
        let target_mode = fs::metadata(&target_database)
            .map_err(|error| error.to_string())?
            .permissions()
            .mode()
            & 0o777;
        ensure(
            target_mode,
            0o644,
            "database hardening must not chmod through a parent symlink",
        )
    }

    #[test]
    fn init_is_idempotent() -> TestResult {
        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let workspace = temp_dir.path().to_path_buf();
        let metadata_path = workspace.join(".ee").join("index").join("meta.json");
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
        let initial_metadata = fs::read(&metadata_path).map_err(|error| error.to_string())?;

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
        ensure(
            second_report
                .actions
                .iter()
                .any(|action| action.action == "initialize_index"),
            false,
            "second run does not rebuild a ready index",
        )?;
        let rerun_metadata = fs::read(&metadata_path).map_err(|error| error.to_string())?;
        ensure(
            rerun_metadata,
            initial_metadata,
            "idempotent init preserves index metadata",
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
            action_errors: vec![],
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
        )?;
        ensure(
            json.get("actionErrors")
                .and_then(|value| value.as_array())
                .map(Vec::is_empty),
            Some(true),
            "actionErrors field",
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
            action_errors: vec![],
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

    #[cfg(unix)]
    #[test]
    fn init_repair_plan_reports_symlink_action_errors() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let workspace = temp_dir.path().join("workspace");
        let outside_metadata = temp_dir.path().join("outside-metadata");
        std::fs::create_dir(&workspace).map_err(|error| error.to_string())?;
        std::fs::create_dir(&outside_metadata).map_err(|error| error.to_string())?;
        symlink(&outside_metadata, workspace.join(".ee")).map_err(|error| error.to_string())?;
        let options = InitOptions {
            workspace_path: workspace,
            dry_run: false,
            repair_plan: true,
            force: false,
            allow_symlink: false,
            skip_boilerplate: true,
        };

        let report = init_workspace(&options);

        ensure(report.status, InitStatus::RepairPlan, "repair plan status")?;
        ensure(
            report
                .actions
                .iter()
                .any(|action| action.status == "symlink_refused"),
            true,
            "repair plan reports symlink refusal action",
        )?;
        ensure(
            report.action_errors.iter().any(|error| {
                error.status == "symlink_refused" && error.message.contains("traverses a symlink")
            }),
            true,
            "repair plan reports symlink refusal details",
        )
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
    fn init_canonicalizes_symlinked_workspace_ancestor() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let real_root = temp_dir.path().join("real-root");
        let linked_root = temp_dir.path().join("linked-root");
        let real_workspace = real_root.join("workspace");
        let linked_workspace = linked_root.join("workspace");
        std::fs::create_dir(&real_root).map_err(|error| error.to_string())?;
        std::fs::create_dir(&real_workspace).map_err(|error| error.to_string())?;
        symlink(&real_root, &linked_root).map_err(|error| error.to_string())?;
        let options = InitOptions {
            workspace_path: linked_workspace,
            dry_run: false,
            repair_plan: false,
            force: false,
            allow_symlink: false,
            skip_boilerplate: true,
        };

        let report = init_workspace(&options);
        let expected_workspace = real_workspace
            .canonicalize()
            .map_err(|error| error.to_string())?;

        ensure(
            report.status,
            InitStatus::Created,
            "ancestor symlink should be canonicalized",
        )?;
        ensure(
            report.workspace,
            expected_workspace,
            "report should use canonical workspace path",
        )?;
        ensure(
            real_workspace.join(".ee").join("ee.db").exists(),
            true,
            "init should write into the canonical workspace",
        )?;
        ensure(
            report.action_errors.is_empty(),
            true,
            "canonicalized ancestor symlink should not produce action errors",
        )
    }

    #[cfg(unix)]
    #[test]
    fn init_canonicalizes_symlinked_workspace_ancestor_with_missing_parents() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let real_root = temp_dir.path().join("real-root");
        let linked_root = temp_dir.path().join("linked-root");
        let real_workspace = real_root.join("missing-parent").join("workspace");
        let linked_workspace = linked_root.join("missing-parent").join("workspace");
        std::fs::create_dir(&real_root).map_err(|error| error.to_string())?;
        symlink(&real_root, &linked_root).map_err(|error| error.to_string())?;
        let options = InitOptions {
            workspace_path: linked_workspace,
            dry_run: false,
            repair_plan: false,
            force: false,
            allow_symlink: false,
            skip_boilerplate: true,
        };

        let report = init_workspace(&options);
        let expected_workspace = real_workspace
            .canonicalize()
            .map_err(|error| error.to_string())?;

        ensure(
            report.status,
            InitStatus::Created,
            "missing parent under ancestor symlink should be canonicalized",
        )?;
        ensure(
            report.workspace,
            expected_workspace,
            "report should use canonical nested workspace path",
        )?;
        ensure(
            real_workspace.join(".ee").join("ee.db").exists(),
            true,
            "init should create nested missing parents under the canonical workspace",
        )?;
        ensure(
            report.action_errors.is_empty(),
            true,
            "canonicalized nested ancestor symlink should not produce action errors",
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
            report.action_errors.len(),
            1,
            "symlink refusal has one action error",
        )?;
        ensure(
            report.action_errors[0]
                .message
                .contains("traverses a symlink"),
            true,
            "symlink refusal explains the reason",
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
    fn init_rejects_symlinked_workspace_with_trailing_separator_by_default() -> TestResult {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let real_workspace = temp_dir.path().join("real-workspace");
        let linked_workspace = temp_dir.path().join("linked-workspace");
        std::fs::create_dir(&real_workspace).map_err(|error| error.to_string())?;
        symlink(&real_workspace, &linked_workspace).map_err(|error| error.to_string())?;
        let options = InitOptions {
            workspace_path: PathBuf::from(format!("{}/", linked_workspace.display())),
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
            report.action_errors.len(),
            1,
            "trailing-separator symlink refusal has one action error",
        )?;
        ensure(
            real_workspace.join(".ee").exists(),
            false,
            "init must not create metadata through a symlinked workspace with trailing separator",
        )?;
        ensure(
            real_workspace.join("AGENTS.md").exists(),
            false,
            "init must not write boilerplate through a symlinked workspace with trailing separator",
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
