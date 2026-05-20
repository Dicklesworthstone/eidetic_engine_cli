use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::json;

use crate::config::{EnvVar, MeshCommandMode, read_env_var, workspace_config};
use crate::db::{
    DbConnection, InsertMeshImportLedgerEventInput, UpsertMeshPeerCursorInput, UpsertMeshPeerInput,
};
use crate::mesh::audit::{
    MeshAuditDetails, MeshAuditEventInput, MeshAuditEventKind, MeshAuditLedgerError,
    append_mesh_audit_event, compute_mesh_audit_event,
};
use crate::mesh::foreground_cli::{
    MESH_CLI_EXPORT_SCHEMA_V1, MESH_CLI_IMPORT_SCHEMA_V1, MESH_CLI_SYNC_SCHEMA_V1,
    MESH_EXPORT_ARTIFACT_SCHEMA_V1, MeshCliDegradation, MeshCliExportReport, MeshCliImportReport,
    MeshCliPeersReport, MeshCliStatusReport, MeshCliSyncReport, MeshExportArtifact,
    MeshForegroundSnapshot, MeshStorageCounts, foreground_degradations,
};
use crate::mesh::peer::{
    MeshPeerCapabilityProfile, MeshPeerCommandReport, MeshPeerEndpoint, MeshPeerEnrollInput,
    MeshPeerHandshake, MeshPeerRecord, MeshPeerRotateInput, build_peer_origin_node_id, enroll_peer,
    list_peers, revoke_peer, rotate_peer_key, show_peer, unknown_peer_attempt_report,
};
use crate::models::{DomainError, ProcessExitCode};
use crate::output;
use crate::policy::{MESH_SECRET_EXPORT_DENIED_CODE, MeshExportSecretScanReport};

use super::{Cli, write_domain_error, write_stdout};

const MESH_CLI_INIT_SCHEMA_V1: &str = "ee.mesh.cli.init.v1";

/// Subcommands for foreground `ee mesh` operations.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum MeshCommand {
    /// Inspect foreground mesh readiness without starting a daemon.
    Init(MeshInitArgs),
    /// List configured peers and anti-entropy cursors from local storage.
    Peers(MeshPeersArgs),
    /// Deliberately enroll, inspect, rotate, or revoke one app-level mesh peer.
    Peer(MeshPeerArgs),
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

/// Arguments for `ee mesh peer`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshPeerArgs {
    #[command(subcommand)]
    pub command: MeshPeerCommand,
}

/// Subcommands for `ee mesh peer`.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum MeshPeerCommand {
    /// Enroll a peer after an explicit capability handshake and human consent.
    Add(MeshPeerAddArgs),
    /// List enrolled peers with their app-level capability profiles.
    List(MeshPeerListArgs),
    /// Show one enrolled peer.
    Show(MeshPeerShowArgs),
    /// Rotate one enrolled peer's public key fingerprint.
    Rotate(MeshPeerRotateArgs),
    /// Revoke one enrolled peer and deny all capability lanes.
    Revoke(MeshPeerRevokeArgs),
    /// Classify a network-reachable node that has not been explicitly enrolled.
    UnknownAttempt(MeshPeerUnknownAttemptArgs),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum MeshPeerCapabilityProfileArg {
    MetadataOnly,
    BodyAllowed,
    EmbeddingsDenied,
    FullyDenied,
}

impl From<MeshPeerCapabilityProfileArg> for MeshPeerCapabilityProfile {
    fn from(value: MeshPeerCapabilityProfileArg) -> Self {
        match value {
            MeshPeerCapabilityProfileArg::MetadataOnly => Self::MetadataOnly,
            MeshPeerCapabilityProfileArg::BodyAllowed => Self::BodyAllowed,
            MeshPeerCapabilityProfileArg::EmbeddingsDenied => Self::EmbeddingsDenied,
            MeshPeerCapabilityProfileArg::FullyDenied => Self::FullyDenied,
        }
    }
}

/// Arguments for `ee mesh peer add`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshPeerAddArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,

    /// Local display alias for the peer.
    #[arg(long, value_name = "ALIAS")]
    pub alias: String,

    /// Tailscale node key or configured endpoint identity from the responder.
    #[arg(long = "tailscale-node-key", value_name = "NODE_KEY")]
    pub tailscale_node_key: String,

    /// Endpoint address associated with the peer.
    #[arg(long, value_name = "ENDPOINT")]
    pub endpoint: String,

    /// Tailnet identifier associated with the peer.
    #[arg(long = "tailnet-id", value_name = "TAILNET_ID")]
    pub tailnet_id: String,

    /// Human-readable tailnet name.
    #[arg(long = "tailnet-name", value_name = "NAME")]
    pub tailnet_display_name: Option<String>,

    /// MagicDNS name associated with the peer.
    #[arg(long = "magic-dns-name", value_name = "NAME")]
    pub magic_dns_name: Option<String>,

    /// Requested sharing capability profile.
    #[arg(long = "profile", value_enum, default_value = "metadata-only")]
    pub profile: MeshPeerCapabilityProfileArg,

    /// Public key fingerprint to associate with this peer.
    #[arg(long = "public-key-fingerprint", value_name = "FINGERPRINT")]
    pub public_key_fingerprint: String,

    /// Capability advertised by the responder handshake. Repeat for multiple lanes.
    #[arg(long = "responder-capability", value_name = "CAPABILITY", action = ArgAction::Append)]
    pub responder_capabilities: Vec<String>,

    /// Override the responder node key used in the handshake.
    #[arg(long = "handshake-node-key", value_name = "NODE_KEY")]
    pub handshake_node_key: Option<String>,

    /// Stable handshake request ID.
    #[arg(long = "handshake-request-id", default_value = "hello_req_cli")]
    pub handshake_request_id: String,

    /// Protocol version reported by both sides in the synthetic handshake.
    #[arg(long = "protocol-version", default_value = "1.0")]
    pub protocol_version: String,

    /// Force a denied handshake for dry-run and denial UX testing.
    #[arg(long = "deny-handshake", action = ArgAction::SetTrue)]
    pub deny_handshake: bool,

    /// RFC3339 timestamp for deterministic enrollment output.
    #[arg(long, value_name = "RFC3339")]
    pub now: Option<String>,

    /// Required explicit human consent flag.
    #[arg(long = "yes", action = ArgAction::SetTrue)]
    pub explicit_human_consent: bool,
}

/// Arguments for `ee mesh peer list`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshPeerListArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee mesh peer show`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshPeerShowArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,

    /// Peer ID to inspect.
    #[arg(value_name = "PEER_ID")]
    pub peer_id: String,
}

/// Arguments for `ee mesh peer rotate`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshPeerRotateArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,

    /// Peer ID to rotate.
    #[arg(value_name = "PEER_ID")]
    pub peer_id: String,

    /// Replacement public key fingerprint.
    #[arg(long = "public-key-fingerprint", value_name = "FINGERPRINT")]
    pub public_key_fingerprint: String,

    /// RFC3339 rotation timestamp.
    #[arg(long = "rotated-at", value_name = "RFC3339")]
    pub rotated_at: Option<String>,

    /// Operator-visible rotation reason.
    #[arg(long, default_value = "operator requested rotation")]
    pub reason: String,
}

/// Arguments for `ee mesh peer revoke`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshPeerRevokeArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,

    /// Peer ID to revoke.
    #[arg(value_name = "PEER_ID")]
    pub peer_id: String,

    /// RFC3339 revocation timestamp.
    #[arg(long = "revoked-at", value_name = "RFC3339")]
    pub revoked_at: Option<String>,
}

/// Arguments for `ee mesh peer unknown-attempt`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshPeerUnknownAttemptArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,

    /// Tailscale node key or configured endpoint identity that contacted us.
    #[arg(long = "tailscale-node-key", value_name = "NODE_KEY")]
    pub tailscale_node_key: String,
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
        MeshCommand::Peer(args) => handle_mesh_peer(cli, args, stdout, stderr),
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

fn handle_mesh_peer<W, E>(
    cli: &Cli,
    args: &MeshPeerArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    match &args.command {
        MeshPeerCommand::Add(args) => handle_mesh_peer_add(cli, args, stdout, stderr),
        MeshPeerCommand::List(args) => handle_mesh_peer_list(cli, args, stdout, stderr),
        MeshPeerCommand::Show(args) => handle_mesh_peer_show(cli, args, stdout, stderr),
        MeshPeerCommand::Rotate(args) => handle_mesh_peer_rotate(cli, args, stdout, stderr),
        MeshPeerCommand::Revoke(args) => handle_mesh_peer_revoke(cli, args, stdout, stderr),
        MeshPeerCommand::UnknownAttempt(args) => {
            handle_mesh_peer_unknown_attempt(cli, args, stdout, stderr)
        }
    }
}

fn handle_mesh_peer_add<W, E>(
    cli: &Cli,
    args: &MeshPeerAddArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (snapshot, connection) = match open_mesh_peer_store(cli, args.database.as_deref()) {
        Ok(store) => store,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let now = args
        .now
        .clone()
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let handshake_node_key = args
        .handshake_node_key
        .clone()
        .unwrap_or_else(|| args.tailscale_node_key.clone());
    let handshake = if args.deny_handshake {
        MeshPeerHandshake::denied(args.handshake_request_id.clone(), handshake_node_key)
    } else {
        MeshPeerHandshake::granted(
            args.handshake_request_id.clone(),
            args.protocol_version.clone(),
            handshake_node_key,
            args.responder_capabilities.clone(),
        )
    };
    let report = enroll_peer(MeshPeerEnrollInput {
        workspace_id: snapshot.workspace_id.clone(),
        alias: args.alias.clone(),
        endpoint: MeshPeerEndpoint {
            tailscale_node_key: args.tailscale_node_key.clone(),
            tailnet_id: args.tailnet_id.clone(),
            tailnet_display_name: args.tailnet_display_name.clone(),
            endpoint: args.endpoint.clone(),
            magic_dns_name: args.magic_dns_name.clone(),
        },
        capability_profile: args.profile.into(),
        handshake,
        public_key_fingerprint: args.public_key_fingerprint.clone(),
        now,
        explicit_human_consent: args.explicit_human_consent,
    });
    if let Some(peer) = report.peer.as_ref()
        && let Err(error) = persist_mesh_peer_record(&connection, &snapshot.workspace_id, peer)
    {
        return write_domain_error(&error, cli.wants_json(), stdout, stderr);
    }
    write_mesh_report(
        cli,
        &report,
        &render_mesh_peer_command_human(&report),
        stdout,
    )
}

fn handle_mesh_peer_list<W, E>(
    cli: &Cli,
    args: &MeshPeerListArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (snapshot, connection) = match open_mesh_peer_store(cli, args.database.as_deref()) {
        Ok(store) => store,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let peers = match list_enrolled_peer_records(&connection, &snapshot.workspace_id) {
        Ok(peers) => peers,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let report = list_peers(&peers);
    write_mesh_report(
        cli,
        &report,
        &render_mesh_peer_command_human(&report),
        stdout,
    )
}

fn handle_mesh_peer_show<W, E>(
    cli: &Cli,
    args: &MeshPeerShowArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (snapshot, connection) = match open_mesh_peer_store(cli, args.database.as_deref()) {
        Ok(store) => store,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let peer = match get_enrolled_peer_record(&connection, &snapshot.workspace_id, &args.peer_id) {
        Ok(peer) => peer,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let report = show_peer(&peer);
    write_mesh_report(
        cli,
        &report,
        &render_mesh_peer_command_human(&report),
        stdout,
    )
}

fn handle_mesh_peer_rotate<W, E>(
    cli: &Cli,
    args: &MeshPeerRotateArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (snapshot, connection) = match open_mesh_peer_store(cli, args.database.as_deref()) {
        Ok(store) => store,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let peer = match get_enrolled_peer_record(&connection, &snapshot.workspace_id, &args.peer_id) {
        Ok(peer) => peer,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let report = rotate_peer_key(
        &peer,
        MeshPeerRotateInput {
            new_public_key_fingerprint: args.public_key_fingerprint.clone(),
            rotated_at: args
                .rotated_at
                .clone()
                .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
            reason: args.reason.clone(),
        },
    );
    if let Some(peer) = report.peer.as_ref()
        && let Err(error) = persist_mesh_peer_record(&connection, &snapshot.workspace_id, peer)
    {
        return write_domain_error(&error, cli.wants_json(), stdout, stderr);
    }
    write_mesh_report(
        cli,
        &report,
        &render_mesh_peer_command_human(&report),
        stdout,
    )
}

fn handle_mesh_peer_revoke<W, E>(
    cli: &Cli,
    args: &MeshPeerRevokeArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (snapshot, connection) = match open_mesh_peer_store(cli, args.database.as_deref()) {
        Ok(store) => store,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let peer = match get_enrolled_peer_record(&connection, &snapshot.workspace_id, &args.peer_id) {
        Ok(peer) => peer,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let report = revoke_peer(
        &peer,
        args.revoked_at
            .clone()
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
    );
    if let Some(peer) = report.peer.as_ref()
        && let Err(error) = persist_mesh_peer_record(&connection, &snapshot.workspace_id, peer)
    {
        return write_domain_error(&error, cli.wants_json(), stdout, stderr);
    }
    write_mesh_report(
        cli,
        &report,
        &render_mesh_peer_command_human(&report),
        stdout,
    )
}

fn handle_mesh_peer_unknown_attempt<W, E>(
    cli: &Cli,
    args: &MeshPeerUnknownAttemptArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let (snapshot, connection) = match open_mesh_peer_store(cli, args.database.as_deref()) {
        Ok(store) => store,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let peers = match list_enrolled_peer_records(&connection, &snapshot.workspace_id) {
        Ok(peers) => peers,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let report =
        unknown_peer_attempt_report(&peers, &snapshot.workspace_id, &args.tailscale_node_key);
    write_mesh_report(
        cli,
        &report,
        &render_mesh_peer_command_human(&report),
        stdout,
    )
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
    let checked_export = match snapshot.checked_export_artifact() {
        Ok(checked_export) => checked_export,
        Err(secret_scan) => {
            let audit_id = match record_mesh_export_secret_scan_audit(
                cli,
                args.database.as_deref(),
                &snapshot,
                &secret_scan,
            ) {
                Ok(audit_id) => audit_id,
                Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
            };
            let domain_error = mesh_secret_export_denied_error(&secret_scan, audit_id.as_deref());
            return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
        }
    };
    let artifact = checked_export.artifact;
    let secret_scan = checked_export.secret_scan;
    let audit_id = match record_mesh_export_secret_scan_audit(
        cli,
        args.database.as_deref(),
        &snapshot,
        &secret_scan,
    ) {
        Ok(audit_id) => audit_id,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
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
        audit_id,
        secret_scan,
        artifact: Some(artifact),
        degraded: snapshot.degraded.clone(),
    };
    write_mesh_report(cli, &report, &render_mesh_export_human(&report), stdout)
}

fn mesh_secret_export_denied_error(
    secret_scan: &MeshExportSecretScanReport,
    audit_id: Option<&str>,
) -> DomainError {
    let details_json = serde_json::json!({
        "code": MESH_SECRET_EXPORT_DENIED_CODE,
        "auditId": audit_id,
        "secretScan": secret_scan,
    })
    .to_string();
    let classes = if secret_scan.denied_secret_classes.is_empty() {
        "unknown".to_owned()
    } else {
        secret_scan.denied_secret_classes.join(", ")
    };
    DomainError::PolicyDeniedWithDetails {
        message: format!(
            "mesh export denied because pre-export secret scanning found policy-denied material: {classes}"
        ),
        repair: Some(
            "Redact or remove the flagged body, tag, evidence, artifact path, embedding surrogate, or policy JSON before retrying export."
                .to_owned(),
        ),
        details_json,
    }
}

fn record_mesh_export_secret_scan_audit(
    cli: &Cli,
    database_override: Option<&Path>,
    snapshot: &MeshForegroundSnapshot,
    secret_scan: &MeshExportSecretScanReport,
) -> Result<Option<String>, DomainError> {
    if !snapshot.initialized {
        return Ok(None);
    }

    let workspace_path = cli.resolve_workspace();
    let database_path = database_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    let connection = open_mesh_connection(&database_path)?;
    let scan_hash = mesh_export_secret_scan_hash(secret_scan)?;
    let mut details = MeshAuditDetails::default();
    details
        .insert_reference("scan_status", &secret_scan.status)
        .map_err(mesh_audit_domain_error)?;
    details
        .insert_reference("policy_action", &secret_scan.policy_action)
        .map_err(mesh_audit_domain_error)?;
    details
        .insert_bool("export_allowed", !secret_scan.denied())
        .map_err(mesh_audit_domain_error)?;
    details
        .insert_count(
            "scanned_field_count",
            u64::from(secret_scan.scanned_field_count),
        )
        .map_err(mesh_audit_domain_error)?;
    details
        .insert_count("secret_finding_count", u64::from(secret_scan.finding_count))
        .map_err(mesh_audit_domain_error)?;
    details
        .insert_digest("secret_scan_hash", &scan_hash)
        .map_err(mesh_audit_domain_error)?;
    if secret_scan.denied() {
        details
            .insert_redacted_text(
                "denied_classes",
                "class_digest",
                &secret_scan.denied_secret_classes.join(","),
            )
            .map_err(mesh_audit_domain_error)?;
    }

    let event = compute_mesh_audit_event(&MeshAuditEventInput {
        workspace_id: snapshot.workspace_id.clone(),
        event_kind: MeshAuditEventKind::Export,
        peer_id: None,
        origin_workspace_id: None,
        target_workspace_id: None,
        workspace_scope: Some("foreground_export".to_owned()),
        policy_decision_id: Some("preexport_scan".to_owned()),
        local_row_refs: Vec::new(),
        cached_body_refs: Vec::new(),
        details,
        previous_event_hash: None,
    })
    .map_err(mesh_audit_domain_error)?;
    let audit_id = append_mesh_audit_event(&connection, &event, Some("ee mesh export"))
        .map_err(mesh_audit_domain_error)?;
    Ok(Some(audit_id))
}

fn mesh_export_secret_scan_hash(
    secret_scan: &MeshExportSecretScanReport,
) -> Result<String, DomainError> {
    let bytes = serde_json::to_vec(secret_scan).map_err(|error| DomainError::Usage {
        message: format!("Failed to serialize mesh export secret scan report: {error}"),
        repair: Some("Retry the command or report the serialization failure.".to_owned()),
    })?;
    Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
}

fn mesh_audit_domain_error(error: MeshAuditLedgerError) -> DomainError {
    DomainError::Storage {
        message: format!("mesh export secret-scan audit failed: {error}"),
        repair: Some(
            "Inspect `ee audit verify --json` and retry mesh export after the audit ledger is healthy."
                .to_owned(),
        ),
    }
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

fn open_mesh_peer_store(
    cli: &Cli,
    database_override: Option<&Path>,
) -> Result<(MeshForegroundSnapshot, DbConnection), DomainError> {
    let snapshot = build_snapshot(cli, database_override)?;
    if !snapshot.initialized {
        return Err(DomainError::Storage {
            message: format!(
                "Cannot use mesh peer enrollment commands because {} does not exist",
                snapshot.database_path
            ),
            repair: Some(format!(
                "Run `ee init --workspace \"{}\" --json` first.",
                snapshot.workspace_path
            )),
        });
    }
    let connection = open_mesh_connection(Path::new(&snapshot.database_path))?;
    Ok((snapshot, connection))
}

fn persist_mesh_peer_record(
    connection: &DbConnection,
    workspace_id: &str,
    peer: &MeshPeerRecord,
) -> Result<(), DomainError> {
    let policy_summary_json = serde_json::to_string(peer).map_err(|error| DomainError::Usage {
        message: format!("Failed to serialize mesh peer record: {error}"),
        repair: Some("Retry the command or report the serialization failure.".to_owned()),
    })?;
    let last_seen_at = peer
        .revoked_at
        .clone()
        .or_else(|| peer.key.rotated_at.clone())
        .unwrap_or_else(|| peer.enrolled_at.clone());
    connection
        .upsert_mesh_peer(&UpsertMeshPeerInput {
            workspace_id: workspace_id.to_owned(),
            peer_id: peer.peer_id.clone(),
            origin_node_id: build_peer_origin_node_id(&peer.endpoint.tailscale_node_key),
            display_name: Some(peer.alias.clone()),
            policy_summary_json: Some(policy_summary_json),
            enabled: peer.state == crate::mesh::peer::MeshPeerState::Active,
            last_seen_at: Some(last_seen_at),
        })
        .map_err(|error| storage_error("Failed to persist mesh peer enrollment", error))?;
    Ok(())
}

fn list_enrolled_peer_records(
    connection: &DbConnection,
    workspace_id: &str,
) -> Result<Vec<MeshPeerRecord>, DomainError> {
    let stored = connection
        .list_mesh_peers(workspace_id)
        .map_err(|error| storage_error("Failed to list mesh peer enrollments", error))?;
    let mut peers = Vec::new();
    for row in stored {
        if let Some(peer) = enrolled_peer_record_from_policy_summary(
            row.policy_summary_json.as_deref(),
            row.peer_id.as_str(),
        )? {
            peers.push(peer);
        }
    }
    peers.sort_by(|left, right| left.peer_id.cmp(&right.peer_id));
    Ok(peers)
}

fn get_enrolled_peer_record(
    connection: &DbConnection,
    workspace_id: &str,
    peer_id: &str,
) -> Result<MeshPeerRecord, DomainError> {
    let row = connection
        .get_mesh_peer(workspace_id, peer_id)
        .map_err(|error| storage_error("Failed to load mesh peer enrollment", error))?
        .ok_or_else(|| unknown_mesh_peer_error(peer_id))?;
    enrolled_peer_record_from_policy_summary(row.policy_summary_json.as_deref(), peer_id)?
        .ok_or_else(|| unknown_mesh_peer_error(peer_id))
}

fn enrolled_peer_record_from_policy_summary(
    policy_summary_json: Option<&str>,
    peer_id: &str,
) -> Result<Option<MeshPeerRecord>, DomainError> {
    let Some(policy_summary_json) = policy_summary_json else {
        return Ok(None);
    };
    let value: serde_json::Value =
        serde_json::from_str(policy_summary_json).map_err(|error| DomainError::Usage {
            message: format!("Stored mesh peer {peer_id} has invalid policy summary JSON: {error}"),
            repair: Some(
                "Re-run `ee mesh peer add ... --yes --json` for this peer or revoke the stale row."
                    .to_owned(),
            ),
        })?;
    if value.get("schema").and_then(serde_json::Value::as_str)
        != Some(crate::mesh::peer::MESH_PEER_RECORD_SCHEMA_V1)
    {
        return Ok(None);
    }
    serde_json::from_value::<MeshPeerRecord>(value)
        .map(Some)
        .map_err(|error| DomainError::Usage {
            message: format!("Stored mesh peer {peer_id} record is malformed: {error}"),
            repair: Some(
                "Re-run `ee mesh peer add ... --yes --json` for this peer or revoke the stale row."
                    .to_owned(),
            ),
        })
}

fn unknown_mesh_peer_error(peer_id: &str) -> DomainError {
    DomainError::Usage {
        message: format!("No enrolled mesh peer found for {peer_id}"),
        repair: Some("Run `ee mesh peer list --json` to inspect enrolled peers.".to_owned()),
    }
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
    output.push_str(&format!(
        "  Selective sync: {profiles} profiles, {subscriptions} subscriptions, default {default_profile}\n",
        profiles = report.selective_sync.profile_count,
        subscriptions = report.selective_sync.subscription_count,
        default_profile = report.selective_sync.default_profile_id,
    ));
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

fn render_mesh_peer_command_human(report: &MeshPeerCommandReport) -> String {
    let mut output = format!(
        "Mesh peer: {command}\n  Success: {success}\n  Message: {message}\n",
        command = report.command,
        success = if report.success { "yes" } else { "no" },
        message = report.message,
    );
    if let Some(code) = report.denied_code {
        output.push_str(&format!("  Denied code: {code}\n"));
    }
    if let Some(peer) = &report.peer {
        output.push_str(&format!(
            "  Peer: {} alias={} state={} profile={}\n  Endpoint: {} ({})\n  Key generation: {}\n",
            peer.peer_id,
            peer.alias,
            peer.state.as_str(),
            peer.capabilities.profile.as_str(),
            peer.endpoint.tailscale_node_key,
            peer.endpoint.endpoint,
            peer.key.generation,
        ));
    }
    if !report.peers.is_empty() {
        output.push_str("  Peers:\n");
        for peer in &report.peers {
            output.push_str(&format!(
                "    - {} alias={} state={} profile={}\n",
                peer.peer_id,
                peer.alias,
                peer.state.as_str(),
                peer.capabilities.profile.as_str()
            ));
        }
    }
    if !report.next_commands.is_empty() {
        output.push_str("  Next commands:\n");
        for command in &report.next_commands {
            output.push_str(&format!("    - {command}\n"));
        }
    }
    output
}

fn render_mesh_export_human(report: &MeshCliExportReport) -> String {
    let target = report.output_path.as_deref().unwrap_or("stdout envelope");
    let mut output = format!(
        "Mesh export\n  Target: {target}\n  Peers: {peers}\n  Cursors: {cursors}\n  Events: {events}\n  Secret scan: {scan_status} ({scanned_fields} fields)\n",
        peers = report.peer_count,
        cursors = report.cursor_count,
        events = report.event_count,
        scan_status = report.secret_scan.status,
        scanned_fields = report.secret_scan.scanned_field_count,
    );
    if let Some(audit_id) = &report.audit_id {
        output.push_str(&format!("  Audit: {audit_id}\n"));
    }
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
