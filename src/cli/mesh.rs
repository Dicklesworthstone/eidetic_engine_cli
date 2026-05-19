use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{ArgAction, Parser, Subcommand};
use serde::Serialize;
use serde_json::json;

use crate::config::{EnvVar, MeshCommandMode, read_env_var, workspace_config};
use crate::db::{
    DbConnection, InsertMeshImportLedgerEventInput, UpsertMeshPeerCursorInput, UpsertMeshPeerInput,
};
use crate::mesh::foreground_cli::{
    MESH_CLI_EXPORT_SCHEMA_V1, MESH_CLI_IMPORT_SCHEMA_V1, MESH_CLI_SYNC_SCHEMA_V1,
    MESH_EXPORT_ARTIFACT_SCHEMA_V1, MeshCliDegradation, MeshCliExportReport, MeshCliImportReport,
    MeshCliPeersReport, MeshCliStatusReport, MeshCliSyncReport, MeshExportArtifact,
    MeshForegroundSnapshot, MeshStorageCounts, foreground_degradations,
};
use crate::models::{DomainError, ProcessExitCode};
use crate::output;

use super::{Cli, write_domain_error, write_stdout};

const MESH_CLI_INIT_SCHEMA_V1: &str = "ee.mesh.cli.init.v1";

/// Subcommands for foreground `ee mesh` operations.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum MeshCommand {
    /// Inspect foreground mesh readiness without starting a daemon.
    Init(MeshInitArgs),
    /// List configured peers and anti-entropy cursors from local storage.
    Peers(MeshPeersArgs),
    /// Report local mesh posture, cache counts, and repair commands.
    Status(MeshStatusArgs),
    /// Export redaction-safe foreground mesh rows to a JSON artifact.
    Export(MeshExportArgs),
    /// Import a foreground mesh JSON artifact from a local file.
    Import(MeshImportArgs),
    /// Run one foreground sync cycle without background daemon mode.
    Sync(MeshSyncArgs),
}

/// Arguments for `ee mesh init`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshInitArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee mesh peers`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshPeersArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee mesh status`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshStatusArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee mesh export`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshExportArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,

    /// Write the mesh export artifact to this local file.
    #[arg(long = "out", value_name = "PATH")]
    pub out: Option<PathBuf>,
}

/// Arguments for `ee mesh import`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshImportArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,

    /// Local foreground mesh export artifact.
    #[arg(long = "file", value_name = "PATH")]
    pub file: PathBuf,

    /// Parse and report the artifact without writing rows.
    #[arg(long = "dry-run", action = ArgAction::SetTrue)]
    pub dry_run: bool,
}

/// Arguments for `ee mesh sync`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshSyncArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,

    /// Run exactly one foreground cycle and exit.
    #[arg(long = "once", action = ArgAction::SetTrue, required = true)]
    pub once: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct MeshCliInitReport {
    schema: &'static str,
    command: &'static str,
    workspace_id: String,
    workspace_path: String,
    database_path: String,
    initialized: bool,
    mesh_enabled: bool,
    mode: String,
    background_process_required: bool,
    network_required: bool,
    status: String,
    next_commands: Vec<String>,
    degraded: Vec<MeshCliDegradation>,
}

pub fn handle_mesh<W, E>(
    cli: &Cli,
    command: &MeshCommand,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    match command {
        MeshCommand::Init(args) => handle_mesh_init(cli, args, stdout, stderr),
        MeshCommand::Peers(args) => handle_mesh_peers(cli, args, stdout, stderr),
        MeshCommand::Status(args) => handle_mesh_status(cli, args, stdout, stderr),
        MeshCommand::Export(args) => handle_mesh_export(cli, args, stdout, stderr),
        MeshCommand::Import(args) => handle_mesh_import(cli, args, stdout, stderr),
        MeshCommand::Sync(args) => handle_mesh_sync(cli, args, stdout, stderr),
    }
}

fn handle_mesh_init<W, E>(
    cli: &Cli,
    args: &MeshInitArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let snapshot = match build_snapshot(cli, args.database.as_deref()) {
        Ok(snapshot) => snapshot,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let report = MeshCliInitReport {
        schema: MESH_CLI_INIT_SCHEMA_V1,
        command: "mesh init",
        workspace_id: snapshot.workspace_id.clone(),
        workspace_path: snapshot.workspace_path.clone(),
        database_path: snapshot.database_path.clone(),
        initialized: snapshot.initialized,
        mesh_enabled: snapshot.mesh_enabled,
        mode: snapshot.mode.clone(),
        background_process_required: false,
        network_required: false,
        status: if snapshot.initialized {
            "ready_for_foreground_operations".to_owned()
        } else {
            "requires_workspace_init".to_owned()
        },
        next_commands: vec![
            format!(
                "ee mesh status --workspace \"{}\" --json",
                snapshot.workspace_path
            ),
            format!(
                "ee mesh export --workspace \"{}\" --out mesh-export.json --json",
                snapshot.workspace_path
            ),
            format!(
                "ee mesh import --workspace \"{}\" --file mesh-export.json --json",
                snapshot.workspace_path
            ),
        ],
        degraded: snapshot.degraded.clone(),
    };
    write_mesh_report(cli, &report, &render_mesh_init_human(&report), stdout)
}

fn handle_mesh_status<W, E>(
    cli: &Cli,
    args: &MeshStatusArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let snapshot = match build_snapshot(cli, args.database.as_deref()) {
        Ok(snapshot) => snapshot,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let report = snapshot.status_report();
    write_mesh_report(cli, &report, &render_mesh_status_human(&report), stdout)
}

fn handle_mesh_peers<W, E>(
    cli: &Cli,
    args: &MeshPeersArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let snapshot = match build_snapshot(cli, args.database.as_deref()) {
        Ok(snapshot) => snapshot,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let report = snapshot.peers_report();
    write_mesh_report(cli, &report, &render_mesh_peers_human(&report), stdout)
}

fn handle_mesh_export<W, E>(
    cli: &Cli,
    args: &MeshExportArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let snapshot = match build_snapshot(cli, args.database.as_deref()) {
        Ok(snapshot) => snapshot,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let artifact = snapshot.export_artifact();
    if let Some(output_path) = &args.out {
        let artifact_json = match serde_json::to_string_pretty(&artifact) {
            Ok(value) => value,
            Err(error) => {
                let domain_error = DomainError::Usage {
                    message: format!("Failed to serialize mesh export artifact: {error}"),
                    repair: Some(
                        "Retry the command or report the serialization failure.".to_owned(),
                    ),
                };
                return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
            }
        };
        if let Err(error) = fs::write(output_path, artifact_json + "\n") {
            let domain_error = DomainError::Storage {
                message: format!(
                    "Failed to write mesh export artifact to {}: {error}",
                    output_path.display()
                ),
                repair: Some("Choose a writable --out path and retry.".to_owned()),
            };
            return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
        }
    }
    let report = MeshCliExportReport {
        schema: MESH_CLI_EXPORT_SCHEMA_V1,
        command: "mesh export",
        artifact_schema: MESH_EXPORT_ARTIFACT_SCHEMA_V1,
        output_path: args.out.as_ref().map(|path| path.display().to_string()),
        peer_count: artifact.peers.len(),
        cursor_count: artifact.cursors.len(),
        event_count: artifact.events.len(),
        artifact: Some(artifact),
        degraded: snapshot.degraded.clone(),
    };
    write_mesh_report(cli, &report, &render_mesh_export_human(&report), stdout)
}

fn handle_mesh_import<W, E>(
    cli: &Cli,
    args: &MeshImportArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let artifact = match read_mesh_export_artifact(&args.file) {
        Ok(artifact) => artifact,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let snapshot = match build_snapshot(cli, args.database.as_deref()) {
        Ok(snapshot) => snapshot,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let (imported_peer_count, imported_cursor_count, imported_event_count) = if args.dry_run {
        (0, 0, 0)
    } else {
        match import_mesh_artifact(args.database.as_deref(), cli, &artifact) {
            Ok(counts) => counts,
            Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
        }
    };
    let report = MeshCliImportReport {
        schema: MESH_CLI_IMPORT_SCHEMA_V1,
        command: "mesh import",
        source_path: args.file.display().to_string(),
        dry_run: args.dry_run,
        peer_count: artifact.peers.len(),
        cursor_count: artifact.cursors.len(),
        event_count: artifact.events.len(),
        imported_peer_count,
        imported_cursor_count,
        imported_event_count,
        degraded: snapshot.degraded.clone(),
    };
    write_mesh_report(cli, &report, &render_mesh_import_human(&report), stdout)
}

fn handle_mesh_sync<W, E>(
    cli: &Cli,
    args: &MeshSyncArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let snapshot = match build_snapshot(cli, args.database.as_deref()) {
        Ok(snapshot) => snapshot,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let mut degraded = snapshot.degraded.clone();
    degraded.push(MeshCliDegradation::sync_once_network_deferred());
    let report = MeshCliSyncReport {
        schema: MESH_CLI_SYNC_SCHEMA_V1,
        command: "mesh sync",
        once: args.once,
        mode: snapshot.mode.clone(),
        contacted_peers: false,
        export_command: format!(
            "ee mesh export --workspace \"{}\" --out mesh-export.json --json",
            snapshot.workspace_path
        ),
        import_command: format!(
            "ee mesh import --workspace \"{}\" --file mesh-export.json --json",
            snapshot.workspace_path
        ),
        degraded,
    };
    write_mesh_report(cli, &report, &render_mesh_sync_human(&report), stdout)
}

fn build_snapshot(
    cli: &Cli,
    database_override: Option<&Path>,
) -> Result<MeshForegroundSnapshot, DomainError> {
    let workspace_path = cli.resolve_workspace();
    let database_path = database_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    let initialized = database_path.is_file();
    let (mesh_enabled, mode) = mesh_config_for_workspace(&workspace_path)?;
    let canonical_workspace = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.clone());

    let (workspace_id, storage, peers, cursors, events) = if initialized {
        let connection = open_mesh_connection(&database_path)?;
        let workspace_id = resolve_mesh_workspace_id(&connection, &workspace_path)?;
        let storage = connection
            .mesh_storage_status(&workspace_id)
            .map_err(|error| storage_error("Failed to inspect mesh storage status", error))?;
        let peers = connection
            .list_mesh_peers(&workspace_id)
            .map_err(|error| storage_error("Failed to list mesh peers", error))?
            .iter()
            .map(Into::into)
            .collect();
        let cursors = connection
            .list_mesh_peer_cursors(&workspace_id)
            .map_err(|error| storage_error("Failed to list mesh peer cursors", error))?
            .iter()
            .map(Into::into)
            .collect();
        let events = connection
            .list_mesh_import_ledger_events_for_workspace(&workspace_id)
            .map_err(|error| storage_error("Failed to list mesh import ledger events", error))?
            .iter()
            .map(Into::into)
            .collect();
        (
            workspace_id,
            MeshStorageCounts::from(&storage),
            peers,
            cursors,
            events,
        )
    } else {
        (
            super::stable_cli_workspace_id(&canonical_workspace),
            MeshStorageCounts::default(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    };

    let workspace_path_string = workspace_path.display().to_string();
    Ok(MeshForegroundSnapshot {
        workspace_id,
        workspace_path: workspace_path_string.clone(),
        database_path: database_path.display().to_string(),
        initialized,
        mesh_enabled,
        mode: mode.as_str().to_owned(),
        storage,
        peers,
        cursors,
        events,
        degraded: foreground_degradations(&workspace_path_string, initialized, mesh_enabled),
    })
}

fn mesh_config_for_workspace(
    workspace_path: &Path,
) -> Result<(bool, MeshCommandMode), DomainError> {
    let project_config = workspace_config(workspace_path);
    let configured_enabled = project_config
        .as_ref()
        .and_then(|config| config.mesh.enabled)
        .unwrap_or(false);
    let configured_mode = project_config
        .as_ref()
        .and_then(|config| config.mesh.command_mode)
        .unwrap_or_default();
    let mesh_enabled = read_env_var(EnvVar::MeshEnabled)
        .map(|value| parse_env_bool(EnvVar::MeshEnabled, &value))
        .transpose()?
        .unwrap_or(configured_enabled);
    let mode = read_env_var(EnvVar::MeshMode)
        .map(|value| parse_env_mesh_mode(EnvVar::MeshMode, &value))
        .transpose()?
        .unwrap_or(configured_mode);
    Ok((mesh_enabled, mode))
}

fn parse_env_bool(variable: EnvVar, value: &str) -> Result<bool, DomainError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(DomainError::Configuration {
            message: format!("{} has invalid boolean value {:?}", variable.name(), value),
            repair: Some("Use true/false, yes/no, on/off, or 1/0.".to_owned()),
        }),
    }
}

fn parse_env_mesh_mode(variable: EnvVar, value: &str) -> Result<MeshCommandMode, DomainError> {
    value
        .trim()
        .parse::<MeshCommandMode>()
        .map_err(|_| DomainError::Configuration {
            message: format!("{} has invalid mesh mode {:?}", variable.name(), value),
            repair: Some("Use off, cache, revisable, or blocking.".to_owned()),
        })
}

fn read_mesh_export_artifact(path: &Path) -> Result<MeshExportArtifact, DomainError> {
    let contents = fs::read_to_string(path).map_err(|error| DomainError::Storage {
        message: format!(
            "Failed to read mesh import artifact {}: {error}",
            path.display()
        ),
        repair: Some(
            "Pass a readable artifact path with `ee mesh import --file <path>`.".to_owned(),
        ),
    })?;
    let artifact = serde_json::from_str::<MeshExportArtifact>(&contents).map_err(|error| {
        DomainError::Usage {
            message: format!(
                "Failed to parse mesh import artifact {}: {error}",
                path.display()
            ),
            repair: Some(
                "Use a JSON artifact produced by `ee mesh export --out <path>`.".to_owned(),
            ),
        }
    })?;
    if artifact.schema != MESH_EXPORT_ARTIFACT_SCHEMA_V1 {
        return Err(DomainError::Usage {
            message: format!(
                "Unsupported mesh import artifact schema {:?}",
                artifact.schema
            ),
            repair: Some(format!(
                "Expected schema {MESH_EXPORT_ARTIFACT_SCHEMA_V1}; re-export with this ee binary."
            )),
        });
    }
    Ok(artifact)
}

fn import_mesh_artifact(
    database_override: Option<&Path>,
    cli: &Cli,
    artifact: &MeshExportArtifact,
) -> Result<(usize, usize, usize), DomainError> {
    let workspace_path = cli.resolve_workspace();
    let database_path = database_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    if !database_path.is_file() {
        return Err(DomainError::Storage {
            message: format!(
                "Cannot import mesh artifact because {} does not exist",
                database_path.display()
            ),
            repair: Some(format!(
                "Run `ee init --workspace \"{}\" --json` first.",
                workspace_path.display()
            )),
        });
    }
    let connection = open_mesh_connection(&database_path)?;
    let workspace_id = resolve_mesh_workspace_id(&connection, &workspace_path)?;
    for peer in &artifact.peers {
        connection
            .upsert_mesh_peer(&UpsertMeshPeerInput {
                workspace_id: workspace_id.clone(),
                peer_id: peer.peer_id.clone(),
                origin_node_id: peer.origin_node_id.clone(),
                display_name: peer.display_name.clone(),
                policy_summary_json: peer.policy_summary_json.clone(),
                enabled: peer.enabled,
                last_seen_at: Some(peer.last_seen_at.clone()),
            })
            .map_err(|error| storage_error("Failed to import mesh peer", error))?;
    }
    for cursor in &artifact.cursors {
        connection
            .upsert_mesh_peer_cursor(&UpsertMeshPeerCursorInput {
                workspace_id: workspace_id.clone(),
                peer_id: cursor.peer_id.clone(),
                origin_node_id: cursor.origin_node_id.clone(),
                origin_workspace_id: cursor.origin_workspace_id.clone(),
                last_seq: cursor.last_seq,
                tip_event_hash: cursor.tip_event_hash.clone(),
                tip_audit_hash: cursor.tip_audit_hash.clone(),
                status: cursor.status.clone(),
                updated_at: Some(cursor.updated_at.clone()),
            })
            .map_err(|error| storage_error("Failed to import mesh peer cursor", error))?;
    }
    for event in &artifact.events {
        connection
            .insert_mesh_import_ledger_event(&InsertMeshImportLedgerEventInput {
                workspace_id: workspace_id.clone(),
                event_id: event.event_id.clone(),
                origin_node_id: event.origin_node_id.clone(),
                origin_workspace_id: event.origin_workspace_id.clone(),
                producer_peer_id: event.producer_peer_id.clone(),
                seq: event.seq,
                prev_event_hash: event.prev_event_hash.clone(),
                event_hash: event.event_hash.clone(),
                event_kind: event.event_kind.clone(),
                logical_memory_id: event.logical_memory_id.clone(),
                content_hash: event.content_hash.clone(),
                material_lane: event.material_lane.clone(),
                redaction_class: event.redaction_class.clone(),
                trust_lane: event.trust_lane.clone(),
                import_decision: event.import_decision.clone(),
                local_memory_id: event.local_memory_id.clone(),
                body_cache_key: event.body_cache_key.clone(),
                policy_failure_surface_json: event.policy_failure_surface_json.clone(),
                policy_decision_json: event.policy_decision_json.clone(),
                event_json: event.event_json.clone(),
                imported_at: Some(event.imported_at.clone()),
            })
            .map_err(|error| storage_error("Failed to import mesh event", error))?;
    }
    Ok((
        artifact.peers.len(),
        artifact.cursors.len(),
        artifact.events.len(),
    ))
}

fn open_mesh_connection(path: &Path) -> Result<DbConnection, DomainError> {
    DbConnection::open_file(path)
        .map_err(|error| storage_error("Failed to open foreground mesh database", error))
}

fn resolve_mesh_workspace_id(
    connection: &DbConnection,
    workspace_path: &Path,
) -> Result<String, DomainError> {
    let primary = workspace_path.to_string_lossy().into_owned();
    if let Some(workspace) = connection
        .get_workspace_by_path(&primary)
        .map_err(|error| DomainError::Storage {
            message: format!("Failed to query workspace: {error}"),
            repair: Some("ee doctor".to_owned()),
        })?
    {
        return Ok(workspace.id);
    }

    let canonical = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let canonical_str = canonical.to_string_lossy().into_owned();
    if canonical_str != primary
        && let Some(workspace) =
            connection
                .get_workspace_by_path(&canonical_str)
                .map_err(|error| DomainError::Storage {
                    message: format!("Failed to query workspace: {error}"),
                    repair: Some("ee doctor".to_owned()),
                })?
    {
        return Ok(workspace.id);
    }

    Ok(super::stable_cli_workspace_id(&canonical))
}

fn storage_error(context: &str, error: crate::db::DbError) -> DomainError {
    DomainError::Storage {
        message: format!("{context}: {error}"),
        repair: Some("Run `ee doctor --json` and verify the workspace database.".to_owned()),
    }
}

fn write_mesh_report<W, T>(
    cli: &Cli,
    report: &T,
    human_output: &str,
    stdout: &mut W,
) -> ProcessExitCode
where
    W: Write,
    T: Serialize,
{
    match cli.renderer() {
        output::Renderer::Human | output::Renderer::Markdown => write_stdout(stdout, human_output),
        output::Renderer::Toon => {
            let data = serde_json::to_value(report).expect("mesh CLI report must serialize");
            write_stdout(
                stdout,
                &(output::render_toon_from_json(&data.to_string()) + "\n"),
            )
        }
        output::Renderer::Json
        | output::Renderer::Jsonl
        | output::Renderer::Compact
        | output::Renderer::Hook => {
            let json = json!({
                "schema": crate::models::RESPONSE_SCHEMA_V1,
                "success": true,
                "data": report,
            });
            write_stdout(stdout, &(json.to_string() + "\n"))
        }
    }
}

fn render_mesh_init_human(report: &MeshCliInitReport) -> String {
    let mut output = format!(
        "Mesh foreground init: {status}\n  Workspace: {workspace}\n  Database: {database}\n  Mesh: {mesh} ({mode})\n  Background process required: no\n  Network required: no\n",
        status = report.status,
        workspace = report.workspace_path,
        database = report.database_path,
        mesh = if report.mesh_enabled {
            "enabled"
        } else {
            "disabled"
        },
        mode = report.mode,
    );
    append_degradations(&mut output, &report.degraded);
    output
}

fn render_mesh_status_human(report: &MeshCliStatusReport) -> String {
    let mut output = format!(
        "Mesh status: {posture}\n  Workspace: {workspace}\n  Database: {database}\n  Mesh: {mesh} ({mode})\n  Peers: {peers}\n  Cursors: {cursors}\n  Imported events: {events}\n  Policy decisions: {decisions}\n  Policy failures: {failures}\n  Cached bodies: {bodies}\n",
        posture = report.posture,
        workspace = report.workspace_path,
        database = report.database_path,
        mesh = if report.mesh_enabled {
            "enabled"
        } else {
            "disabled"
        },
        mode = report.mode,
        peers = report.storage.peer_count,
        cursors = report.storage.cursor_count,
        events = report.storage.imported_event_count,
        decisions = report.storage.policy_decision_event_count,
        failures = report.storage.policy_failure_event_count,
        bodies = report.storage.cached_body_count,
    );
    if !report.repair_commands.is_empty() {
        output.push_str("  Repair commands:\n");
        for command in &report.repair_commands {
            output.push_str(&format!("    - {command}\n"));
        }
    }
    append_degradations(&mut output, &report.degraded);
    output
}

fn render_mesh_peers_human(report: &MeshCliPeersReport) -> String {
    let mut output = format!(
        "Mesh peers\n  Workspace ID: {}\n  Peers: {}\n  Cursors: {}\n",
        report.workspace_id, report.peer_count, report.cursor_count,
    );
    for peer in &report.peers {
        output.push_str(&format!(
            "    - {} ({}) enabled={}\n",
            peer.peer_id, peer.origin_node_id, peer.enabled
        ));
    }
    append_degradations(&mut output, &report.degraded);
    output
}

fn render_mesh_export_human(report: &MeshCliExportReport) -> String {
    let target = report.output_path.as_deref().unwrap_or("stdout envelope");
    let mut output = format!(
        "Mesh export\n  Target: {target}\n  Peers: {peers}\n  Cursors: {cursors}\n  Events: {events}\n",
        peers = report.peer_count,
        cursors = report.cursor_count,
        events = report.event_count,
    );
    append_degradations(&mut output, &report.degraded);
    output
}

fn render_mesh_import_human(report: &MeshCliImportReport) -> String {
    let mut output = format!(
        "Mesh import\n  Source: {source}\n  Dry run: {dry_run}\n  Artifact rows: {peers} peers, {cursors} cursors, {events} events\n  Imported: {imported_peers} peers, {imported_cursors} cursors, {imported_events} events\n",
        source = report.source_path,
        dry_run = if report.dry_run { "yes" } else { "no" },
        peers = report.peer_count,
        cursors = report.cursor_count,
        events = report.event_count,
        imported_peers = report.imported_peer_count,
        imported_cursors = report.imported_cursor_count,
        imported_events = report.imported_event_count,
    );
    append_degradations(&mut output, &report.degraded);
    output
}

fn render_mesh_sync_human(report: &MeshCliSyncReport) -> String {
    let mut output = format!(
        "Mesh sync --once\n  Mode: {}\n  Contacted peers: no\n  Export fallback: {}\n  Import fallback: {}\n",
        report.mode, report.export_command, report.import_command,
    );
    append_degradations(&mut output, &report.degraded);
    output
}

fn append_degradations(output: &mut String, degraded: &[MeshCliDegradation]) {
    if degraded.is_empty() {
        return;
    }
    output.push_str("  Degraded:\n");
    for item in degraded {
        output.push_str(&format!(
            "    - {} [{}]: {} Repair: {}\n",
            item.code, item.severity, item.message, item.repair
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_bool_accepts_documented_forms() {
        assert!(parse_env_bool(EnvVar::MeshEnabled, "true").unwrap());
        assert!(parse_env_bool(EnvVar::MeshEnabled, "1").unwrap());
        assert!(!parse_env_bool(EnvVar::MeshEnabled, "off").unwrap());
        assert!(parse_env_bool(EnvVar::MeshEnabled, "maybe").is_err());
    }

    #[test]
    fn env_mesh_mode_accepts_registered_modes() {
        assert_eq!(
            parse_env_mesh_mode(EnvVar::MeshMode, "cache").unwrap(),
            MeshCommandMode::Cache
        );
        assert!(parse_env_mesh_mode(EnvVar::MeshMode, "online").is_err());
    }
}
