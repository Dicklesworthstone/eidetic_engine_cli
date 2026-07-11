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
            Self::PeerScopeNotDurable { peer_id } => write!(
                formatter,
                "peer-scoped mesh containment for `{peer_id}` cannot be persisted durably yet; use `ee mesh peer revoke {peer_id}` for durable per-peer containment or `ee mesh disable` (without --peer) for workspace-wide containment"
            ),
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

#[must_use]
pub fn plan_emergency_disable(input: &MeshEmergencyDisableInput) -> MeshEmergencyDisableReport {
    let scope = if input.all_workspaces {
        "all_workspaces"
    } else if input.peer_id.is_some() {
        "peer"
    } else {
        "workspace"
    };
    let peer_capabilities_suspended = input
        .peer_id
        .as_ref()
        .map(|peer_id| MeshPeerSuspension {
            peer_id: peer_id.clone(),
            state: "suspended".to_owned(),
            new_requests_rejected: true,
            capabilities_suspended: vec![
                "body".to_owned(),
                "embedding".to_owned(),
                "graphLink".to_owned(),
                "revisionNotice".to_owned(),
            ],
        })
        .into_iter()
        .collect();
    // Peer scope leaves the workspace mesh posture untouched: the report
    // must not claim workspace-wide effects a peer-scoped disable does not
    // have (bd-3mw86).
    let peer_scope = scope == "peer";
    let next_commands = if let Some(peer_id) = input.peer_id.as_deref() {
        vec![
            format!(
                "ee mesh peer revoke {peer_id} --workspace \"{}\" --json",
                input.workspace_path.display()
            ),
            format!(
                "ee mesh disable --workspace \"{}\" --json",
                input.workspace_path.display()
            ),
            format!(
                "ee mesh status --workspace \"{}\" --json",
                input.workspace_path.display()
            ),
        ]
    } else {
        vec![
            format!(
                "ee mesh status --workspace \"{}\" --json",
                input.workspace_path.display()
            ),
            format!(
                "ee mesh reenable --workspace \"{}\" --confirm-reenable --json",
                input.workspace_path.display()
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
        new_peer_requests_rejected: true,
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
            format!(
                "ee mesh disable --workspace \"{}\" --dry-run --json",
                workspace_path.display()
            ),
            format!(
                "ee mesh reenable --workspace \"{}\" --confirm-reenable --json",
                workspace_path.display()
            ),
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
        plan_emergency_disable, plan_emergency_reenable,
    };
    use crate::config::{ConfigFile, MeshCommandMode};
    use std::fs;
    use std::io;

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

    #[test]
    fn peer_specific_disable_suspends_only_the_named_peer_capabilities() {
        let input = MeshEmergencyDisableInput {
            workspace_path: "/tmp/mesh-peer-suspend".into(),
            all_workspaces: false,
            dry_run: true,
            reason: Some("unexpected body lane".to_owned()),
            peer_id: Some("peer_alpha".to_owned()),
            temporary_for: Some("30m".to_owned()),
            mesh_enabled_before: true,
            command_mode_before: MeshCommandMode::Cache,
        };

        let report = plan_emergency_disable(&input);
        assert_eq!(report.scope, "peer");
        assert!(!report.applied);
        assert_eq!(report.peer_capabilities_suspended.len(), 1);
        let peer = &report.peer_capabilities_suspended[0];
        assert_eq!(peer.peer_id, "peer_alpha");
        assert_eq!(peer.state, "suspended");
        assert!(peer.new_requests_rejected);
        assert!(peer.capabilities_suspended.contains(&"body".to_owned()));
        assert!(
            peer.capabilities_suspended
                .contains(&"embedding".to_owned())
        );
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
        assert!(
            report
                .next_commands
                .iter()
                .any(|command| command.contains("mesh peer revoke peer_alpha")),
            "peer scope must point at the durable per-peer containment path: {:?}",
            report.next_commands
        );
    }

    #[test]
    fn peer_specific_disable_apply_fails_honestly_without_config_mutation_bd_3mw86() {
        let workspace: std::path::PathBuf = "/tmp/mesh-peer-apply-refusal-bd-3mw86".into();
        let input = MeshEmergencyDisableInput {
            workspace_path: workspace.clone(),
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
        assert!(
            matches!(
                &error,
                MeshEmergencyError::PeerScopeNotDurable { peer_id } if peer_id == "peer_alpha"
            ),
            "unexpected error: {error}"
        );
        let message = error.to_string();
        assert!(message.contains("ee mesh peer revoke peer_alpha"));
        assert!(
            !workspace.exists(),
            "peer-scope refusal must not create or mutate workspace config"
        );
    }

    #[test]
    fn peer_and_all_workspaces_scopes_are_rejected_bd_3mw86() {
        let input = MeshEmergencyDisableInput {
            workspace_path: "/tmp/mesh-peer-scope-conflict-bd-3mw86".into(),
            all_workspaces: true,
            dry_run: true,
            reason: None,
            peer_id: Some("peer_alpha".to_owned()),
            temporary_for: None,
            mesh_enabled_before: true,
            command_mode_before: MeshCommandMode::Revisable,
        };

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
    }

    #[test]
    fn all_workspaces_disable_is_supported_without_workspace_file_mutation() {
        let input = MeshEmergencyDisableInput {
            workspace_path: "/tmp/mesh-global-disable".into(),
            all_workspaces: true,
            dry_run: false,
            reason: Some("fleet incident".to_owned()),
            peer_id: None,
            temporary_for: None,
            mesh_enabled_before: true,
            command_mode_before: MeshCommandMode::Revisable,
        };

        let report = apply_emergency_disable(&input).expect("global disable plan");
        assert_eq!(report.scope, "all_workspaces");
        assert!(!report.applied);
        assert!(
            report
                .config_actions
                .iter()
                .all(|action| action.target == "operator_global" && !action.applied)
        );
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
