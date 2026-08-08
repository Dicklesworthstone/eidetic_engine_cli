//! Emergency mesh disable and containment planning.
//!
//! This module is deliberately local-only: it models the state transitions and
//! filesystem config write needed to shut mesh down without deleting memories,
//! cache rows, audit evidence, or other source-of-truth data.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use toml_edit::{DocumentMut, value};

use crate::config::MeshCommandMode;

pub const MESH_EMERGENCY_DISABLE_SCHEMA_V1: &str = "ee.mesh.emergency_disable.v1";
pub const MESH_EMERGENCY_STATUS_SCHEMA_V1: &str = "ee.mesh.emergency_status.v1";
pub const MESH_EMERGENCY_REENABLE_SCHEMA_V1: &str = "ee.mesh.emergency_reenable.v1";

/// Cap on the byte size of the workspace `.ee/config.toml` file that
/// `write_mesh_config` reads before mutating for a mesh emergency
/// containment event. Workspace config files carry a handful of mesh
/// flags plus a few related sections; 1 MiB is many orders of
/// magnitude above any realistic config while bounding the allocation
/// an accidentally-pointed-at-a-log-file or adversarial path can
/// demand. bd-3gmzf / bd-1icct multi-pass-bug-hunting follow-up
/// (mirrors MESH_CONFIG_MAX_BYTES in 5b725c82 and the 8 MiB
/// AGENT_MAIL_SNAPSHOT_MAX_BYTES precedent in bd-1sdr5).
const MESH_EMERGENCY_CONFIG_MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshEmergencyDisableInput {
    pub workspace_path: PathBuf,
    /// Explicit `--database` override from the invocation, when present.
    /// Carried so refusal messages and `next_commands` reproduce the full
    /// invoking scope instead of pointing repair commands at the default
    /// store (bd-3mw86 review).
    pub database_path: Option<PathBuf>,
    pub all_workspaces: bool,
    pub dry_run: bool,
    pub reason: Option<String>,
    pub peer_id: Option<String>,
    pub temporary_for: Option<String>,
    pub mesh_enabled_before: bool,
    pub command_mode_before: MeshCommandMode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshEmergencyReenableInput {
    pub workspace_path: PathBuf,
    pub dry_run: bool,
    pub explicit: bool,
    pub mesh_enabled_before: bool,
    pub command_mode_before: MeshCommandMode,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshConfigAction {
    pub target: String,
    pub key: String,
    pub before: String,
    pub after: String,
    pub applied: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshPeerSuspension {
    pub peer_id: String,
    pub state: String,
    pub new_requests_rejected: bool,
    pub capabilities_suspended: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshEmergencyDisableReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub dry_run: bool,
    pub scope: String,
    pub workspace_path: String,
    pub reason: Option<String>,
    pub temporary_for: Option<String>,
    pub mesh_enabled_before: bool,
    pub mesh_enabled_after: bool,
    pub command_mode_before: String,
    pub command_mode_after: String,
    pub disable_requested: bool,
    pub listener_stopped: bool,
    pub background_sync_stopped: bool,
    pub queued_exports_cancelled: u32,
    pub new_peer_requests_rejected: bool,
    pub peer_capabilities_suspended: Vec<MeshPeerSuspension>,
    pub local_cache_readable: bool,
    pub source_of_truth_memories_preserved: bool,
    pub audit_state_preserved: bool,
    pub reenable_requires_explicit_command: bool,
    pub applied: bool,
    pub config_actions: Vec<MeshConfigAction>,
    pub next_commands: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshEmergencyStatusReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub workspace_path: String,
    pub mesh_enabled: bool,
    pub command_mode: String,
    pub containment_active: bool,
    pub local_cache_readable: bool,
    pub source_of_truth_memories_preserved: bool,
    pub next_commands: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshEmergencyReenableReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub dry_run: bool,
    pub explicit: bool,
    pub workspace_path: String,
    pub mesh_enabled_before: bool,
    pub mesh_enabled_after: bool,
    pub command_mode_before: String,
    pub command_mode_after: String,
    pub applied: bool,
    pub config_actions: Vec<MeshConfigAction>,
    pub reenable_requires_explicit_command: bool,
}

#[derive(Debug)]
pub enum MeshEmergencyError {
    ReenableRequiresExplicitCommand,
    ReadConfig {
        path: PathBuf,
        source: io::Error,
    },
    ParseConfig {
        path: PathBuf,
        message: String,
    },
    WriteConfig {
        path: PathBuf,
        source: io::Error,
    },
    /// Peer-scoped containment cannot be made durable yet: the mesh peer
    /// state model has no suspended state (`Active | Revoked` only), so a
    /// `--peer` disable must fail closed instead of silently widening to a
    /// workspace-wide `mesh.enabled=false` flip (bd-3mw86).
    PeerScopeNotDurable {
        peer_id: String,
        /// Resolved workspace of the refused invocation, so repair
        /// commands target the same store the operator addressed.
        workspace_path: PathBuf,
        /// Explicit `--database` override of the refused invocation, when
        /// one was passed; `None` means the workspace-default database.
        database_path: Option<PathBuf>,
    },
    /// `--peer` and `--all-workspaces` name contradictory blast radii; the
    /// domain layer rejects the combination even if a caller bypasses the
    /// CLI-level conflict (bd-3mw86).
    PeerScopeConflictsAllWorkspaces {
        peer_id: String,
    },
}

impl std::fmt::Display for MeshEmergencyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReenableRequiresExplicitCommand => write!(
                formatter,
                "mesh re-enable requires an explicit confirmation flag"
            ),
            Self::ReadConfig { path, source } => {
                write!(formatter, "failed to read {}: {source}", path.display())
            }
            Self::ParseConfig { path, message } => {
                write!(formatter, "failed to parse {}: {message}", path.display())
            }
            Self::WriteConfig { path, source } => {
                write!(formatter, "failed to write {}: {source}", path.display())
            }
            Self::PeerScopeNotDurable {
                peer_id,
                workspace_path,
                database_path,
            } => {
                let peer_argument = shell_quote_command_arg(peer_id);
                let workspace_argument =
                    shell_quote_command_arg(&workspace_path.display().to_string());
                let database_argument = database_argument_fragment(database_path.as_deref());
                write!(
                    formatter,
                    "peer-scoped mesh containment for `{peer_id}` cannot be persisted durably yet; use `ee mesh peer revoke {peer_argument} --workspace {workspace_argument}{database_argument} --json` for durable per-peer containment or `ee mesh disable --workspace {workspace_argument}{database_argument} --json` (without --peer) for workspace-wide containment",
                )
            }
            Self::PeerScopeConflictsAllWorkspaces { peer_id } => write!(
                formatter,
                "--peer {peer_id} and --all-workspaces name contradictory containment scopes; pass exactly one"
            ),
        }
    }
}

impl std::error::Error for MeshEmergencyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ReadConfig { source, .. } | Self::WriteConfig { source, .. } => Some(source),
            Self::ReenableRequiresExplicitCommand
            | Self::ParseConfig { .. }
            | Self::PeerScopeNotDurable { .. }
            | Self::PeerScopeConflictsAllWorkspaces { .. } => None,
        }
    }
}

/// Quote one shell argument for redaction-safe recovery and next-command
/// strings. Values containing shell metacharacters stay inside single quotes;
/// embedded single quotes use the standard POSIX close/escape/reopen form.
fn shell_quote_command_arg(value: &str) -> String {
    if value.is_empty() {
        "''".to_owned()
    } else if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':'))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// Format the ` --database <path>` fragment repair/next commands append
/// when the invocation carried an explicit database override; empty when
/// the workspace-default database applies (bd-3mw86 review).
fn database_argument_fragment(database_path: Option<&Path>) -> String {
    database_path.map_or_else(String::new, |path| {
        format!(
            " --database {}",
            shell_quote_command_arg(&path.display().to_string())
        )
    })
}

#[must_use]
pub(crate) fn plan_emergency_disable(
    input: &MeshEmergencyDisableInput,
) -> MeshEmergencyDisableReport {
    let scope = if input.all_workspaces {
        "all_workspaces"
    } else if input.peer_id.is_some() {
        "peer"
    } else {
        "workspace"
    };
    // Peer scope leaves the workspace mesh posture untouched: the report
    // must not claim workspace-wide effects a peer-scoped disable does not
    // have (bd-3mw86).
    let peer_scope = scope == "peer";
    // The peer state model has no durable suspended state (Active | Revoked
    // only), so the preview must not claim a suspension or rejection that
    // apply_emergency_disable will refuse to perform. The entry names the
    // targeted peer with state "unavailable" and suspends nothing; the
    // durable path lives in next_commands (bd-3mw86 review).
    let peer_capabilities_suspended: Vec<MeshPeerSuspension> = if peer_scope {
        input
            .peer_id
            .as_ref()
            .map(|peer_id| MeshPeerSuspension {
                peer_id: peer_id.clone(),
                state: "unavailable".to_owned(),
                new_requests_rejected: false,
                capabilities_suspended: Vec::new(),
            })
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };
    let database_argument = database_argument_fragment(input.database_path.as_deref());
    let workspace_argument = shell_quote_command_arg(&input.workspace_path.display().to_string());
    let next_commands = if let Some(peer_id) = input.peer_id.as_deref() {
        let peer_argument = shell_quote_command_arg(peer_id);
        vec![
            format!(
                "ee mesh peer revoke {peer_argument} --workspace {workspace_argument}{database_argument} --json"
            ),
            format!("ee mesh disable --workspace {workspace_argument}{database_argument} --json"),
            format!("ee mesh status --workspace {workspace_argument}{database_argument} --json"),
        ]
    } else {
        vec![
            format!("ee mesh status --workspace {workspace_argument}{database_argument} --json"),
            format!(
                "ee mesh reenable --workspace {workspace_argument}{database_argument} --confirm-reenable --json"
            ),
        ]
    };
    MeshEmergencyDisableReport {
        schema: MESH_EMERGENCY_DISABLE_SCHEMA_V1,
        command: "mesh disable",
        dry_run: input.dry_run,
        scope: scope.to_owned(),
        workspace_path: input.workspace_path.display().to_string(),
        reason: input.reason.clone(),
        temporary_for: input.temporary_for.clone(),
        mesh_enabled_before: input.mesh_enabled_before,
        mesh_enabled_after: if peer_scope {
            input.mesh_enabled_before
        } else {
            false
        },
        command_mode_before: input.command_mode_before.as_str().to_owned(),
        command_mode_after: if peer_scope {
            input.command_mode_before.as_str().to_owned()
        } else {
            MeshCommandMode::Off.as_str().to_owned()
        },
        disable_requested: true,
        listener_stopped: !peer_scope,
        background_sync_stopped: !peer_scope,
        queued_exports_cancelled: 0,
        // Only a workspace-wide (or all-workspaces) disable actually turns
        // new peer requests away; a peer-scoped invocation has no durable
        // mechanism, so claiming rejection would be a lie (bd-3mw86 review).
        new_peer_requests_rejected: !peer_scope,
        peer_capabilities_suspended,
        local_cache_readable: true,
        source_of_truth_memories_preserved: true,
        audit_state_preserved: true,
        reenable_requires_explicit_command: !peer_scope,
        applied: false,
        config_actions: disable_config_actions(input, false),
        next_commands,
    }
}

pub fn apply_emergency_disable(
    input: &MeshEmergencyDisableInput,
) -> Result<MeshEmergencyDisableReport, MeshEmergencyError> {
    if let Some(peer_id) = input.peer_id.as_deref() {
        // Contradictory scopes fail closed even if a caller bypasses the
        // CLI-level conflicts_with (bd-3mw86).
        if input.all_workspaces {
            return Err(MeshEmergencyError::PeerScopeConflictsAllWorkspaces {
                peer_id: peer_id.to_owned(),
            });
        }
        // Peer-scoped containment has no durable persistence path yet (the
        // peer state model is Active | Revoked, with no suspended state), so
        // a non-dry-run --peer disable fails honestly instead of silently
        // widening to a workspace-wide mesh.enabled=false flip — the
        // bd-3mw86 blast-radius bug. Dry-run planning stays available.
        if !input.dry_run {
            return Err(MeshEmergencyError::PeerScopeNotDurable {
                peer_id: peer_id.to_owned(),
                workspace_path: input.workspace_path.clone(),
                database_path: input.database_path.clone(),
            });
        }
    }
    let mut report = plan_emergency_disable(input);
    if !input.dry_run && !input.all_workspaces {
        write_mesh_config(
            &input.workspace_path,
            false,
            MeshCommandMode::Off,
            MESH_EMERGENCY_DISABLE_SCHEMA_V1,
        )?;
        report.config_actions = disable_config_actions(input, true);
        report.applied = true;
    }
    Ok(report)
}

#[must_use]
pub fn emergency_status_report(
    workspace_path: &Path,
    mesh_enabled: bool,
    command_mode: MeshCommandMode,
) -> MeshEmergencyStatusReport {
    let workspace_argument = shell_quote_command_arg(&workspace_path.display().to_string());
    MeshEmergencyStatusReport {
        schema: MESH_EMERGENCY_STATUS_SCHEMA_V1,
        command: "mesh status",
        workspace_path: workspace_path.display().to_string(),
        mesh_enabled,
        command_mode: command_mode.as_str().to_owned(),
        containment_active: !mesh_enabled || command_mode == MeshCommandMode::Off,
        local_cache_readable: true,
        source_of_truth_memories_preserved: true,
        next_commands: vec![
            format!("ee mesh disable --workspace {workspace_argument} --dry-run --json"),
            format!("ee mesh reenable --workspace {workspace_argument} --confirm-reenable --json"),
        ],
    }
}

#[must_use]
pub fn plan_emergency_reenable(
    input: &MeshEmergencyReenableInput,
) -> Result<MeshEmergencyReenableReport, MeshEmergencyError> {
    if !input.explicit {
        return Err(MeshEmergencyError::ReenableRequiresExplicitCommand);
    }
    Ok(MeshEmergencyReenableReport {
        schema: MESH_EMERGENCY_REENABLE_SCHEMA_V1,
        command: "mesh reenable",
        dry_run: input.dry_run,
        explicit: input.explicit,
        workspace_path: input.workspace_path.display().to_string(),
        mesh_enabled_before: input.mesh_enabled_before,
        mesh_enabled_after: true,
        command_mode_before: input.command_mode_before.as_str().to_owned(),
        command_mode_after: MeshCommandMode::Cache.as_str().to_owned(),
        applied: false,
        config_actions: reenable_config_actions(input, false),
        reenable_requires_explicit_command: true,
    })
}

pub fn apply_emergency_reenable(
    input: &MeshEmergencyReenableInput,
) -> Result<MeshEmergencyReenableReport, MeshEmergencyError> {
    let mut report = plan_emergency_reenable(input)?;
    if !input.dry_run {
        write_mesh_config(
            &input.workspace_path,
            true,
            MeshCommandMode::Cache,
            MESH_EMERGENCY_REENABLE_SCHEMA_V1,
        )?;
        report.config_actions = reenable_config_actions(input, true);
        report.applied = true;
    }
    Ok(report)
}

fn disable_config_actions(
    input: &MeshEmergencyDisableInput,
    applied: bool,
) -> Vec<MeshConfigAction> {
    // Peer scope plans NO workspace config mutation: collapsing a --peer
    // disable into a workspace-wide mesh.enabled flip was the bd-3mw86
    // blast-radius bug, and the config-action report must stay truthful.
    if !input.all_workspaces && input.peer_id.is_some() {
        return Vec::new();
    }
    let target = if input.all_workspaces {
        "operator_global"
    } else {
        "workspace_config"
    };
    vec![
        MeshConfigAction {
            target: target.to_owned(),
            key: "mesh.enabled".to_owned(),
            before: input.mesh_enabled_before.to_string(),
            after: "false".to_owned(),
            applied: applied && !input.all_workspaces,
        },
        MeshConfigAction {
            target: target.to_owned(),
            key: "mesh.command_mode".to_owned(),
            before: input.command_mode_before.as_str().to_owned(),
            after: MeshCommandMode::Off.as_str().to_owned(),
            applied: applied && !input.all_workspaces,
        },
    ]
}

fn reenable_config_actions(
    input: &MeshEmergencyReenableInput,
    applied: bool,
) -> Vec<MeshConfigAction> {
    vec![
        MeshConfigAction {
            target: "workspace_config".to_owned(),
            key: "mesh.enabled".to_owned(),
            before: input.mesh_enabled_before.to_string(),
            after: "true".to_owned(),
            applied,
        },
        MeshConfigAction {
            target: "workspace_config".to_owned(),
            key: "mesh.command_mode".to_owned(),
            before: input.command_mode_before.as_str().to_owned(),
            after: MeshCommandMode::Cache.as_str().to_owned(),
            applied,
        },
    ]
}

fn write_mesh_config(
    workspace_path: &Path,
    enabled: bool,
    command_mode: MeshCommandMode,
    source_schema: &str,
) -> Result<(), MeshEmergencyError> {
    let config_path = workspace_path.join(".ee").join("config.toml");
    ensure_mesh_config_path_has_no_symlink_components(&config_path, "read or write").map_err(
        |source| MeshEmergencyError::ReadConfig {
            path: config_path.clone(),
            source,
        },
    )?;
    let input = match read_mesh_emergency_config_bounded(&config_path) {
        Ok(contents) => contents,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            String::new()
        }
        Err(source) => {
            return Err(MeshEmergencyError::ReadConfig {
                path: config_path,
                source,
            });
        }
    };
    let mut document: DocumentMut =
        input.parse().map_err(
            |source: toml_edit::TomlError| MeshEmergencyError::ParseConfig {
                path: config_path.clone(),
                message: source.to_string(),
            },
        )?;
    document["mesh"]["enabled"] = value(enabled);
    document["mesh"]["command_mode"] = value(command_mode.as_str());
    document["mesh"]["last_containment_schema"] = value(source_schema);
    let planned_toml = document.to_string();
    let parent = config_path
        .parent()
        .ok_or_else(|| MeshEmergencyError::WriteConfig {
            path: config_path.clone(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "config path has no parent"),
        })?;
    fs::create_dir_all(parent).map_err(|source| MeshEmergencyError::WriteConfig {
        path: config_path.clone(),
        source,
    })?;
    let mut temp_path = config_path.clone();
    temp_path.set_extension("mesh-containment.tmp");
    ensure_mesh_config_path_has_no_symlink_components(&temp_path, "write").map_err(|source| {
        MeshEmergencyError::WriteConfig {
            path: temp_path.clone(),
            source,
        }
    })?;
    ensure_mesh_config_path_has_no_symlink_components(&config_path, "publish").map_err(
        |source| MeshEmergencyError::WriteConfig {
            path: config_path.clone(),
            source,
        },
    )?;
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|source| MeshEmergencyError::WriteConfig {
                path: temp_path.clone(),
                source,
            })?;
        file.write_all(planned_toml.as_bytes()).map_err(|source| {
            MeshEmergencyError::WriteConfig {
                path: temp_path.clone(),
                source,
            }
        })?;
        file.sync_data()
            .map_err(|source| MeshEmergencyError::WriteConfig {
                path: temp_path.clone(),
                source,
            })?;
    }
    ensure_mesh_config_path_has_no_symlink_components(&config_path, "publish").map_err(
        |source| MeshEmergencyError::WriteConfig {
            path: config_path.clone(),
            source,
        },
    )?;
    fs::rename(&temp_path, &config_path).map_err(|source| MeshEmergencyError::WriteConfig {
        path: config_path,
        source,
    })
}

fn ensure_mesh_config_path_has_no_symlink_components(
    path: &Path,
    operation: &'static str,
) -> io::Result<()> {
    if let Some(symlink_path) = crate::core::path_safety::first_existing_symlink_component(path)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to {operation} mesh emergency config '{}' through symlinked path component '{}'",
                path.display(),
                symlink_path.display()
            ),
        ));
    }
    Ok(())
}

/// Read `path` into a string while refusing payloads above
/// [`MESH_EMERGENCY_CONFIG_MAX_BYTES`]. Uses `File::open + Read::take
/// (cap+1) + read_to_end`; an over-cap read returns
/// `io::ErrorKind::InvalidData` naming the cap, which the caller folds
/// into `MeshEmergencyError::ReadConfig`. Mirrors the bounded-read
/// pattern in `src/cli/mesh.rs::read_mesh_text_bounded` (bd-3l1cy,
/// 5b725c82). bd-3gmzf.
fn read_mesh_emergency_config_bounded(path: &Path) -> io::Result<String> {
    let read_limit = MESH_EMERGENCY_CONFIG_MAX_BYTES
        .checked_add(1)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "mesh emergency config read cap overflowed usize",
            )
        })?;
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(read_limit as u64).read_to_end(&mut bytes)?;
    if bytes.len() > MESH_EMERGENCY_CONFIG_MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Mesh emergency config '{}' exceeds the {MESH_EMERGENCY_CONFIG_MAX_BYTES}-byte cap; refusing to read",
                path.display()
            ),
        ));
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::{
        MESH_EMERGENCY_CONFIG_MAX_BYTES, MeshEmergencyDisableInput, MeshEmergencyError,
        MeshEmergencyReenableInput, apply_emergency_disable, apply_emergency_reenable,
        emergency_status_report, plan_emergency_disable, plan_emergency_reenable,
        shell_quote_command_arg,
    };
    use crate::config::{ConfigFile, MeshCommandMode};
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};

    type TestResult = Result<(), String>;

    fn temp_workspace() -> Result<tempfile::TempDir, String> {
        tempfile::tempdir().map_err(|error| format!("tempdir: {error}"))
    }

    #[test]
    fn emergency_disable_refuses_oversized_workspace_config() -> TestResult {
        // bd-3gmzf: a workspace .ee/config.toml larger than
        // MESH_EMERGENCY_CONFIG_MAX_BYTES (1 MiB) must be refused
        // with MeshEmergencyError::ReadConfig wrapping an
        // io::ErrorKind::InvalidData before we attempt to parse or
        // mutate it. Mirrors the bd-3l1cy / bd-1sdr5 regression for
        // the same defect class.
        let workspace = temp_workspace()?;
        let ee_dir = workspace.path().join(".ee");
        fs::create_dir_all(&ee_dir).map_err(|error| format!("create .ee: {error}"))?;
        let config_path = ee_dir.join("config.toml");
        fs::write(
            &config_path,
            vec![b'x'; MESH_EMERGENCY_CONFIG_MAX_BYTES + 1],
        )
        .map_err(|error| format!("write oversized config: {error}"))?;

        let input = MeshEmergencyDisableInput {
            workspace_path: workspace.path().to_path_buf(),
            database_path: None,
            all_workspaces: false,
            dry_run: false,
            reason: Some("oversized config probe".to_owned()),
            peer_id: None,
            temporary_for: None,
            mesh_enabled_before: true,
            command_mode_before: MeshCommandMode::Blocking,
        };

        let error = apply_emergency_disable(&input).expect_err("oversized config read must error");
        match error {
            MeshEmergencyError::ReadConfig { source, .. } => {
                assert_eq!(source.kind(), io::ErrorKind::InvalidData);
                let message = format!("{source}");
                assert!(
                    message.contains("exceeds") && message.contains("byte cap"),
                    "expected over-cap diagnostic; got {message}"
                );
            }
            other => {
                panic!("expected MeshEmergencyError::ReadConfig with InvalidData; got {other:?}")
            }
        }
        Ok(())
    }

    #[test]
    fn emergency_disable_overrides_enabled_mesh_without_deleting_local_truth() -> TestResult {
        let workspace = temp_workspace()?;
        let input = MeshEmergencyDisableInput {
            workspace_path: workspace.path().to_path_buf(),
            database_path: None,
            all_workspaces: false,
            dry_run: false,
            reason: Some("unexpected peer".to_owned()),
            peer_id: None,
            temporary_for: None,
            mesh_enabled_before: true,
            command_mode_before: MeshCommandMode::Blocking,
        };

        let report = apply_emergency_disable(&input).map_err(|error| error.to_string())?;
        assert!(report.applied);
        assert!(report.disable_requested);
        assert!(report.listener_stopped);
        assert!(report.background_sync_stopped);
        assert_eq!(report.queued_exports_cancelled, 0);
        assert!(report.local_cache_readable);
        assert!(report.source_of_truth_memories_preserved);
        assert!(report.audit_state_preserved);
        assert!(report.reenable_requires_explicit_command);

        let config_path = workspace.path().join(".ee").join("config.toml");
        let config_text = fs::read_to_string(&config_path)
            .map_err(|error| format!("read {}: {error}", config_path.display()))?;
        let config = ConfigFile::parse(&config_text).map_err(|error| error.to_string())?;
        assert_eq!(config.mesh.enabled, Some(false));
        assert_eq!(config.mesh.command_mode, Some(MeshCommandMode::Off));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn emergency_disable_refuses_symlinked_ee_directory() -> TestResult {
        use std::os::unix::fs::symlink;

        let workspace = temp_workspace()?;
        let redirected_ee = workspace.path().join("redirected-ee");
        fs::create_dir_all(&redirected_ee)
            .map_err(|error| format!("create redirected .ee: {error}"))?;
        symlink(&redirected_ee, workspace.path().join(".ee"))
            .map_err(|error| format!("create .ee symlink: {error}"))?;

        let input = MeshEmergencyDisableInput {
            workspace_path: workspace.path().to_path_buf(),
            database_path: None,
            all_workspaces: false,
            dry_run: false,
            reason: Some("symlinked workspace config probe".to_owned()),
            peer_id: None,
            temporary_for: None,
            mesh_enabled_before: true,
            command_mode_before: MeshCommandMode::Blocking,
        };

        let error =
            apply_emergency_disable(&input).expect_err("symlinked .ee write must be refused");
        match error {
            MeshEmergencyError::ReadConfig { source, .. }
            | MeshEmergencyError::WriteConfig { source, .. } => {
                assert_eq!(source.kind(), io::ErrorKind::InvalidInput);
                let message = source.to_string();
                assert!(
                    message.contains("symlinked path component"),
                    "expected symlink diagnostic; got {message}"
                );
            }
            other => panic!("expected config path safety error; got {other:?}"),
        }
        assert!(
            !redirected_ee.join("config.toml").exists(),
            "containment config must not be written through symlinked .ee"
        );
        Ok(())
    }

    /// Seed a workspace `.ee/config.toml` with distinctive bytes so a test
    /// can prove a refused command left the file byte-identical.
    fn seed_config(workspace: &Path) -> Result<(PathBuf, String), String> {
        let ee_dir = workspace.join(".ee");
        fs::create_dir_all(&ee_dir).map_err(|error| format!("create .ee: {error}"))?;
        let config_path = ee_dir.join("config.toml");
        let seeded = "# operator marker: bd-3mw86 seeded config\n[mesh]\nenabled = true\ncommand_mode = \"cache\"\n";
        fs::write(&config_path, seeded).map_err(|error| format!("seed config: {error}"))?;
        Ok((config_path, seeded.to_owned()))
    }

    #[test]
    fn peer_scope_dry_run_previews_refusal_without_claiming_suspension() -> TestResult {
        // bd-3mw86 review: no durable suspended state exists, so the peer
        // dry-run must not advertise state="suspended", rejected requests,
        // or suspended capabilities that the non-dry-run path refuses to
        // deliver. It previews the refusal and points at the durable path.
        let workspace = temp_workspace()?;
        let (config_path, seeded) = seed_config(workspace.path())?;
        let input = MeshEmergencyDisableInput {
            workspace_path: workspace.path().to_path_buf(),
            database_path: None,
            all_workspaces: false,
            dry_run: true,
            reason: Some("unexpected body lane".to_owned()),
            peer_id: Some("peer_alpha".to_owned()),
            temporary_for: Some("30m".to_owned()),
            mesh_enabled_before: true,
            command_mode_before: MeshCommandMode::Cache,
        };

        // Dry-run routes through apply so domain validation runs on
        // previews too; peer dry-run stays available and writes nothing.
        let report = apply_emergency_disable(&input).map_err(|error| error.to_string())?;
        assert_eq!(report.scope, "peer");
        assert!(!report.applied);
        assert_eq!(report.peer_capabilities_suspended.len(), 1);
        let peer = &report.peer_capabilities_suspended[0];
        assert_eq!(peer.peer_id, "peer_alpha");
        assert_eq!(peer.state, "unavailable");
        assert!(!peer.new_requests_rejected);
        assert!(peer.capabilities_suspended.is_empty());
        assert!(!report.new_peer_requests_rejected);
        // bd-3mw86: peer scope must not claim workspace-wide effects — the
        // workspace mesh posture is untouched by a peer-scoped disable.
        assert_eq!(report.mesh_enabled_after, input.mesh_enabled_before);
        assert_eq!(
            report.command_mode_after,
            input.command_mode_before.as_str()
        );
        assert!(!report.listener_stopped);
        assert!(!report.background_sync_stopped);
        assert!(report.config_actions.is_empty());
        let workspace_display = workspace.path().display().to_string();
        assert!(
            report.next_commands.iter().any(|command| {
                command.contains("mesh peer revoke peer_alpha")
                    && command.contains(&workspace_display)
            }),
            "peer scope must point at the durable per-peer containment path with the invoking workspace: {:?}",
            report.next_commands
        );
        let after = fs::read_to_string(&config_path).map_err(|error| error.to_string())?;
        assert_eq!(
            after, seeded,
            "dry-run must leave the seeded config byte-identical"
        );
        Ok(())
    }

    #[test]
    fn peer_specific_disable_apply_fails_honestly_without_config_mutation_bd_3mw86() -> TestResult {
        let workspace = temp_workspace()?;
        let (config_path, seeded) = seed_config(workspace.path())?;
        let database = workspace.path().join("custom").join("ee.db");
        let input = MeshEmergencyDisableInput {
            workspace_path: workspace.path().to_path_buf(),
            database_path: Some(database.clone()),
            all_workspaces: false,
            dry_run: false,
            reason: Some("isolate one peer".to_owned()),
            peer_id: Some("peer_alpha".to_owned()),
            temporary_for: None,
            mesh_enabled_before: true,
            command_mode_before: MeshCommandMode::Cache,
        };

        let error = apply_emergency_disable(&input)
            .expect_err("non-dry-run peer disable must fail honestly, not widen scope");
        match &error {
            MeshEmergencyError::PeerScopeNotDurable {
                peer_id,
                workspace_path,
                database_path,
            } => {
                assert_eq!(peer_id, "peer_alpha");
                assert_eq!(workspace_path, workspace.path());
                assert_eq!(database_path.as_deref(), Some(database.as_path()));
            }
            other => panic!("expected PeerScopeNotDurable; got {other}"),
        }
        // The refusal message must reproduce the full invoking scope so an
        // operator or agent following it cannot mutate the wrong store.
        let message = error.to_string();
        assert!(message.contains("ee mesh peer revoke peer_alpha"));
        let workspace_argument = shell_quote_command_arg(&workspace.path().display().to_string());
        let database_argument = shell_quote_command_arg(&database.display().to_string());
        assert!(
            message.contains(&format!("--workspace {workspace_argument}")),
            "refusal must carry the resolved workspace: {message}"
        );
        assert!(
            message.contains(&format!("--database {database_argument}")),
            "refusal must carry the explicit database override: {message}"
        );
        let after = fs::read_to_string(&config_path).map_err(|error| error.to_string())?;
        assert_eq!(
            after, seeded,
            "peer-scope refusal must leave the seeded config byte-identical"
        );
        Ok(())
    }

    #[test]
    fn peer_and_all_workspaces_scopes_are_rejected_bd_3mw86() -> TestResult {
        let workspace = temp_workspace()?;
        let (config_path, seeded) = seed_config(workspace.path())?;
        let input = MeshEmergencyDisableInput {
            workspace_path: workspace.path().to_path_buf(),
            database_path: None,
            all_workspaces: true,
            dry_run: true,
            reason: None,
            peer_id: Some("peer_alpha".to_owned()),
            temporary_for: None,
            mesh_enabled_before: true,
            command_mode_before: MeshCommandMode::Revisable,
        };

        // The conflict is rejected even for dry-run previews, and even when
        // a caller bypasses the CLI-level conflicts_with.
        let error = apply_emergency_disable(&input)
            .expect_err("contradictory --peer/--all-workspaces must be rejected");
        assert!(
            matches!(
                &error,
                MeshEmergencyError::PeerScopeConflictsAllWorkspaces { peer_id }
                    if peer_id == "peer_alpha"
            ),
            "unexpected error: {error}"
        );
        let after = fs::read_to_string(&config_path).map_err(|error| error.to_string())?;
        assert_eq!(
            after, seeded,
            "scope conflict must not touch workspace config"
        );
        Ok(())
    }

    #[test]
    fn plan_never_mixes_all_workspaces_scope_with_peer_suspensions() {
        // Defense in depth for callers that reach the planner without
        // apply_emergency_disable's validation: a contradictory input must
        // not produce an all-workspaces report that also advertises a peer
        // suspension entry (bd-3mw86 review).
        let input = MeshEmergencyDisableInput {
            workspace_path: PathBuf::from("unused"),
            database_path: None,
            all_workspaces: true,
            dry_run: true,
            reason: None,
            peer_id: Some("peer_alpha".to_owned()),
            temporary_for: None,
            mesh_enabled_before: true,
            command_mode_before: MeshCommandMode::Revisable,
        };

        let report = plan_emergency_disable(&input);
        assert_eq!(report.scope, "all_workspaces");
        assert!(report.peer_capabilities_suspended.is_empty());
    }

    #[test]
    fn peer_next_commands_carry_explicit_database_override() -> TestResult {
        let workspace = temp_workspace()?;
        let database = workspace.path().join("elsewhere.db");
        let input = MeshEmergencyDisableInput {
            workspace_path: workspace.path().to_path_buf(),
            database_path: Some(database.clone()),
            all_workspaces: false,
            dry_run: true,
            reason: None,
            peer_id: Some("peer_alpha".to_owned()),
            temporary_for: None,
            mesh_enabled_before: true,
            command_mode_before: MeshCommandMode::Cache,
        };

        let report = apply_emergency_disable(&input).map_err(|error| error.to_string())?;
        let database_argument = format!(
            "--database {}",
            shell_quote_command_arg(&database.display().to_string())
        );
        assert!(
            report
                .next_commands
                .iter()
                .all(|command| command.contains(&database_argument)),
            "every next command must reproduce the explicit database override: {:?}",
            report.next_commands
        );
        Ok(())
    }

    #[test]
    fn recovery_commands_shell_quote_adversarial_scope_paths() -> TestResult {
        let workspace_path = PathBuf::from("/tmp/ee $(touch nope) 'workspace'");
        let database_path = PathBuf::from("/tmp/ee `touch nope2` $HOME 'database'.db");
        let input = MeshEmergencyDisableInput {
            workspace_path: workspace_path.clone(),
            database_path: Some(database_path.clone()),
            all_workspaces: false,
            dry_run: true,
            reason: None,
            peer_id: Some("peer_alpha".to_owned()),
            temporary_for: None,
            mesh_enabled_before: true,
            command_mode_before: MeshCommandMode::Cache,
        };

        let report = apply_emergency_disable(&input).map_err(|error| error.to_string())?;
        let workspace_argument = shell_quote_command_arg(&workspace_path.display().to_string());
        let database_argument = shell_quote_command_arg(&database_path.display().to_string());
        for command in &report.next_commands {
            assert!(
                command.contains(&format!("--workspace {workspace_argument}")),
                "workspace path must be one quoted shell argument: {command}"
            );
            assert!(
                command.contains(&format!("--database {database_argument}")),
                "database path must be one quoted shell argument: {command}"
            );
            assert!(
                !command.contains("--workspace \"/tmp/ee $("),
                "command substitutions must never remain active in double quotes: {command}"
            );
        }

        let status = emergency_status_report(&workspace_path, true, MeshCommandMode::Cache);
        assert!(status.next_commands.iter().all(|command| {
            command.contains(&format!("--workspace {workspace_argument}"))
                && !command.contains("--workspace \"/tmp/ee $(")
        }));
        Ok(())
    }

    #[test]
    fn all_workspaces_disable_is_supported_without_workspace_file_mutation() -> TestResult {
        let workspace = temp_workspace()?;
        let input = MeshEmergencyDisableInput {
            workspace_path: workspace.path().to_path_buf(),
            database_path: None,
            all_workspaces: true,
            dry_run: false,
            reason: Some("fleet incident".to_owned()),
            peer_id: None,
            temporary_for: None,
            mesh_enabled_before: true,
            command_mode_before: MeshCommandMode::Revisable,
        };

        let report = apply_emergency_disable(&input).map_err(|error| error.to_string())?;
        assert_eq!(report.scope, "all_workspaces");
        assert!(!report.applied);
        assert!(
            report
                .config_actions
                .iter()
                .all(|action| action.target == "operator_global" && !action.applied)
        );
        assert!(
            !workspace.path().join(".ee").exists(),
            "all-workspaces intent must not mutate the invoking workspace config"
        );
        Ok(())
    }

    #[test]
    fn emergency_reenable_requires_explicit_confirmation() -> TestResult {
        let input = MeshEmergencyReenableInput {
            workspace_path: "/tmp/mesh-reenable".into(),
            dry_run: true,
            explicit: false,
            mesh_enabled_before: false,
            command_mode_before: MeshCommandMode::Off,
        };
        let error = plan_emergency_reenable(&input).expect_err("missing confirmation fails");
        assert_eq!(
            error.to_string(),
            "mesh re-enable requires an explicit confirmation flag"
        );

        let explicit = MeshEmergencyReenableInput {
            explicit: true,
            ..input
        };
        let report = plan_emergency_reenable(&explicit).map_err(|error| error.to_string())?;
        assert_eq!(report.command_mode_after, "cache");
        assert!(report.reenable_requires_explicit_command);
        Ok(())
    }

    #[test]
    fn emergency_reenable_writes_enabled_cache_mode() -> TestResult {
        let workspace = temp_workspace()?;
        let input = MeshEmergencyReenableInput {
            workspace_path: workspace.path().to_path_buf(),
            dry_run: false,
            explicit: true,
            mesh_enabled_before: false,
            command_mode_before: MeshCommandMode::Off,
        };
        let report = apply_emergency_reenable(&input).map_err(|error| error.to_string())?;
        assert!(report.applied);
        let config_path = workspace.path().join(".ee").join("config.toml");
        let config_text = fs::read_to_string(&config_path)
            .map_err(|error| format!("read {}: {error}", config_path.display()))?;
        let config = ConfigFile::parse(&config_text).map_err(|error| error.to_string())?;
        assert_eq!(config.mesh.enabled, Some(true));
        assert_eq!(config.mesh.command_mode, Some(MeshCommandMode::Cache));
        Ok(())
    }
}
