use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use asupersync::{Cx, Outcome};
use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::json;
use toml_edit::{Array, DocumentMut, Item, Value, value};

use crate::config::{EnvVar, MeshCommandMode, read_env_var, workspace_config};
use crate::core::tailscale_probe::{
    SystemTailscaleCliProbeRunner, SystemTailscaleSocketProbeRunner, TailscaleCliProbeConfig,
    TailscaleLocalReport, TailscalePlatform, TailscaleSocketProbeConfig,
    probe_tailscale_local_with_runners, tailscale_probe_timeout_ms_from_env_value,
};
use crate::db::{
    CreateAuditInput, CreateSearchIndexJobInput, DbConnection, InsertMeshImportLedgerEventInput,
    SearchIndexJobType, StoredMeshPeer, StoredMeshPeerCursor, UpsertMeshPeerCursorInput,
    UpsertMeshPeerInput, audit_actions, generate_audit_id,
};
use crate::mesh::audit::{
    MeshAuditDetails, MeshAuditEventInput, MeshAuditEventKind, MeshAuditLedgerError,
    append_mesh_audit_event, compute_mesh_audit_event,
};
use crate::mesh::auto_enrollment::{
    AutoEnrollmentCandidate, AutoEnrollmentInput, AutoEnrollmentOptions, AutoEnrollmentResult,
    AutoEnrollmentSyncOnceMode, ExistingAutoEnrollmentPeer, plan_auto_enrollment,
};
use crate::mesh::auto_enrollment_safety::{
    emit_safety_snapshot_audit, update_materialization_outcome,
};
use crate::mesh::discovery_policy::{
    DISCOVERY_ALLOWLIST_FILE, DISCOVERY_DENYLIST_FILE, DISCOVERY_POLICY_SCHEMA_V1,
    DiscoveryConsent, DiscoveryDecision, DiscoveryDecisionInput, DiscoveryMode,
    EE_MESH_SERVICE_TAG, RESPOND_ALLOWLIST_FILE, RespondDecisionInput, WorkspaceLists,
    decide_discovery, decide_respond, evaluate_policy_degradations, load_node_key_list,
    load_workspace_lists, validate_node_key,
};
use crate::mesh::foreground_cli::{
    MESH_CLI_EXPORT_SCHEMA_V1, MESH_CLI_IMPORT_SCHEMA_V1, MESH_CLI_SYNC_SCHEMA_V1,
    MESH_EXPORT_ARTIFACT_SCHEMA_V1, MESH_SYNC_ONCE_NETWORK_DEFERRED_CODE, MeshCliDegradation,
    MeshCliExportReport, MeshCliImportReport, MeshCliPeersReport, MeshCliStatusReport,
    MeshCliSyncReport, MeshExportArtifact, MeshForegroundSnapshot, MeshStorageCounts,
    MeshSyncSupervisorOptions, MeshSyncSupervisorReport, foreground_degradations,
    run_mesh_sync_supervisor_supervised,
};
use crate::mesh::hello_responder::HelloResponderStatusReport;
use crate::mesh::peer::{
    MeshPeerCapabilityProfile, MeshPeerCommandReport, MeshPeerEndpoint, MeshPeerEnrollInput,
    MeshPeerHandshake, MeshPeerRecord, MeshPeerRotateInput, build_peer_origin_node_id, enroll_peer,
    list_peers, revoke_peer, rotate_peer_key, show_peer, unknown_peer_attempt_report,
};
use crate::mesh::tailscale_autodiscovery::{
    TailscaleAutodiscoveryConfig, TailscaleAutodiscoveryReport,
    TailscaleStatusCapabilityHelloProbe, autodiscover_tailscale_peers,
    tailscale_discovery_budget_ms_from_env_value, tailscale_peer_probe_timeout_ms_from_env_value,
};
use crate::models::{DomainError, ProcessExitCode};
use crate::output;
use crate::policy::{MESH_SECRET_EXPORT_DENIED_CODE, MeshExportSecretScanReport};

use super::{Cli, write_domain_error, write_stdout};

const MESH_CLI_INIT_SCHEMA_V1: &str = "ee.mesh.cli.init.v1";
const DISCOVERY_POLICY_CONFIG_FILE: &str = "discovery_policy.toml";
const AUTO_ENROLL_OVERRIDES_FILE: &str = "auto_enroll_overrides.toml";

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
    /// Materialize zero-touch Tailscale mesh peers from fresh autodiscovery.
    AutoEnroll(MeshAutoEnrollArgs),
    /// Inspect or update Tailscale peer discovery policy.
    DiscoveryPolicy(MeshDiscoveryPolicyArgs),
    /// Inspect the local mesh hello responder lifecycle job.
    HelloResponder(MeshHelloResponderArgs),
    /// Export redaction-safe foreground mesh rows to a JSON artifact.
    Export(MeshExportArgs),
    /// Import a foreground mesh JSON artifact from a local file.
    Import(MeshImportArgs),
    /// Run one foreground sync cycle without background daemon mode.
    Sync(MeshSyncArgs),
    /// Preview the effect of granting one lane to a peer without mutating policy.
    PreviewGrant(MeshPreviewGrantArgs),
}

/// Arguments for `ee mesh discovery-policy`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshDiscoveryPolicyArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,

    /// Include a deterministic caller/responder decision preview.
    #[arg(long, action = ArgAction::SetTrue)]
    pub explain: bool,

    #[command(subcommand)]
    pub command: Option<MeshDiscoveryPolicyCommand>,
}

/// Subcommands for `ee mesh discovery-policy`.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum MeshDiscoveryPolicyCommand {
    /// Persist workspace discovery/respond modes.
    Set(MeshDiscoveryPolicySetArgs),
    /// Add a node key to both discovery and responder allowlists.
    Allow(MeshDiscoveryPolicyNodeArgs),
    /// Add a node key to the discovery denylist.
    Deny(MeshDiscoveryPolicyNodeArgs),
}

/// Arguments for `ee mesh discovery-policy set`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshDiscoveryPolicySetArgs {
    /// Workspace discovery mode.
    #[arg(long = "discovery-mode", value_enum)]
    pub discovery_mode: Option<MeshDiscoveryPolicyModeArg>,

    /// Workspace responder mode.
    #[arg(long = "respond-mode", value_enum)]
    pub respond_mode: Option<MeshDiscoveryPolicyModeArg>,
}

/// Arguments for `ee mesh discovery-policy allow|deny`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshDiscoveryPolicyNodeArgs {
    /// Tailscale node key formatted as nodekey: plus 64 lowercase hex characters.
    #[arg(value_name = "NODE_KEY")]
    pub node_key: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum MeshDiscoveryPolicyModeArg {
    ServiceTag,
    AutoAdmit,
    Allowlist,
}

impl From<MeshDiscoveryPolicyModeArg> for DiscoveryMode {
    fn from(value: MeshDiscoveryPolicyModeArg) -> Self {
        match value {
            MeshDiscoveryPolicyModeArg::ServiceTag => Self::ServiceTag,
            MeshDiscoveryPolicyModeArg::AutoAdmit => Self::AutoAdmit,
            MeshDiscoveryPolicyModeArg::Allowlist => Self::Allowlist,
        }
    }
}

/// Arguments for `ee mesh hello-responder`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshHelloResponderArgs {
    #[command(subcommand)]
    pub command: MeshHelloResponderCommand,
}

/// Subcommands for `ee mesh hello-responder`.
#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum MeshHelloResponderCommand {
    /// Report whether the local hello responder lifecycle job is running.
    Status(MeshHelloResponderStatusArgs),
}

/// Arguments for `ee mesh hello-responder status`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshHelloResponderStatusArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

/// Arguments for `ee mesh preview-grant`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshPreviewGrantArgs {
    /// Peer node key the preview targets.
    #[arg(value_name = "PEER_NODE_KEY")]
    pub peer_node_key: String,

    /// Lane to preview granting on this peer.
    #[arg(long, value_name = "LANE")]
    pub lane: MeshPreviewGrantLane,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,

    /// Maximum preview rows. Internally clamped to LANE_GRANT_PREVIEW_MAX_LIMIT.
    #[arg(long, default_value_t = 50)]
    pub limit: usize,

    /// How the preview samples memories from the candidate set.
    #[arg(
        long = "sample-strategy",
        value_name = "STRATEGY",
        default_value = "random"
    )]
    pub sample_strategy: MeshPreviewGrantSampleStrategy,

    /// Seed for the random sample strategy. Pinned for deterministic replay.
    #[arg(long, default_value_t = 0)]
    pub seed: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum MeshPreviewGrantLane {
    Metadata,
    Body,
    Embedding,
    GraphLink,
    CurationSignal,
    RevisionNotice,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum MeshPreviewGrantSampleStrategy {
    Random,
    HighestTrust,
    MostRecent,
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

/// Arguments for `ee mesh auto-enroll`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshAutoEnrollArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,

    /// Emit the intended config and audit row without writing peer rows.
    #[arg(long, action = ArgAction::SetTrue)]
    pub dry_run: bool,

    /// Force-include a Tailscale node key for this and later reconciliations.
    #[arg(long = "include", value_name = "NODE_KEY", action = ArgAction::Append)]
    pub include: Vec<String>,

    /// Force-exclude a Tailscale node key and append it to the denylist.
    #[arg(long = "exclude", value_name = "NODE_KEY", action = ArgAction::Append)]
    pub exclude: Vec<String>,

    /// Print the per-peer decision tree without durable peer writes.
    #[arg(long, action = ArgAction::SetTrue)]
    pub explain: bool,

    /// Explain one skipped node key.
    #[arg(long = "explain-skip", value_name = "NODE_KEY", action = ArgAction::Append)]
    pub explain_skip: Vec<String>,

    /// Explicitly migrate existing manual mesh rows into the auto-managed lifecycle.
    #[arg(long = "replace-manual-with-auto", action = ArgAction::SetTrue)]
    pub replace_manual_with_auto: bool,
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

    /// Desired background sync cadence in milliseconds, reported for daemon hot-mode handoff.
    #[arg(long = "cadence-ms", default_value_t = 0)]
    pub cadence_ms: u64,

    /// Maximum peers the supervisor may schedule concurrently.
    #[arg(long = "peer-concurrency", default_value_t = 1)]
    pub peer_concurrency: u32,

    /// Maximum body bytes the supervisor may fetch during this foreground sync cycle.
    #[arg(long = "body-fetch-budget-bytes", default_value_t = 65_536)]
    pub body_fetch_budget_bytes: u64,

    /// Maximum stale-read window tolerated for peer summaries.
    #[arg(long = "stale-read-window-ms", default_value_t = 5_000)]
    pub stale_read_window_ms: u64,

    /// Wall-clock budget for the supervised foreground sync cycle.
    #[arg(long = "time-budget-ms", default_value_t = 5_000)]
    pub time_budget_ms: u64,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct MeshDiscoveryPolicyReport {
    schema: &'static str,
    command: &'static str,
    workspace_id: String,
    workspace_path: String,
    discovery_mode: String,
    respond_mode: String,
    allowlisted_node_keys: Vec<String>,
    respond_allowlisted_node_keys: Vec<String>,
    denied_node_keys: Vec<String>,
    degraded: Vec<MeshCliDegradation>,
    effective_decision_preview: Vec<MeshDiscoveryPolicyDecisionPreview>,
    mutation: Option<MeshDiscoveryPolicyMutation>,
    audit_id: Option<String>,
    next_commands: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct MeshDiscoveryPolicyDecisionPreview {
    direction: &'static str,
    node_key: String,
    advertised_tags: Vec<String>,
    decision: String,
    reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct MeshDiscoveryPolicyMutation {
    operation: &'static str,
    changed: bool,
    node_key_hash: Option<String>,
    config_path: Option<String>,
    list_path: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct WorkspacePolicyModes {
    discovery_mode: Option<DiscoveryMode>,
    respond_mode: Option<DiscoveryMode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DiscoveryPolicyState {
    discovery_mode: DiscoveryMode,
    respond_mode: DiscoveryMode,
    lists: WorkspaceLists,
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
        MeshCommand::AutoEnroll(args) => handle_mesh_auto_enroll(cli, args, stdout, stderr),
        MeshCommand::DiscoveryPolicy(args) => {
            handle_mesh_discovery_policy(cli, args, stdout, stderr)
        }
        MeshCommand::HelloResponder(args) => handle_mesh_hello_responder(cli, args, stdout, stderr),
        MeshCommand::Export(args) => handle_mesh_export(cli, args, stdout, stderr),
        MeshCommand::Import(args) => handle_mesh_import(cli, args, stdout, stderr),
        MeshCommand::Sync(args) => handle_mesh_sync(cli, args, stdout, stderr),
        MeshCommand::PreviewGrant(args) => handle_mesh_preview_grant(cli, args, stdout, stderr),
    }
}

fn handle_mesh_preview_grant<W, E>(
    cli: &Cli,
    args: &MeshPreviewGrantArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    use crate::mesh::auto_enrollment_safety::IntendedLanePolicy;
    use crate::mesh::lane_grant_preview::{
        LANE_GRANT_PREVIEW_MAX_LIMIT, Lane, LaneGrantPreviewInput, SampleStrategy,
        compute_lane_grant_preview,
    };

    if args.limit > LANE_GRANT_PREVIEW_MAX_LIMIT {
        let domain_error = DomainError::Configuration {
            message: format!(
                "--limit {} exceeds LANE_GRANT_PREVIEW_MAX_LIMIT={LANE_GRANT_PREVIEW_MAX_LIMIT}",
                args.limit
            ),
            repair: Some(format!(
                "Re-run with --limit <= {LANE_GRANT_PREVIEW_MAX_LIMIT}"
            )),
        };
        return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
    }

    let snapshot = match build_snapshot(cli, args.database.as_deref()) {
        Ok(snapshot) => snapshot,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };

    let lane = match args.lane {
        MeshPreviewGrantLane::Metadata => Lane::Metadata,
        MeshPreviewGrantLane::Body => Lane::Body,
        MeshPreviewGrantLane::Embedding => Lane::Embedding,
        MeshPreviewGrantLane::GraphLink => Lane::GraphLink,
        MeshPreviewGrantLane::CurationSignal => Lane::CurationSignal,
        MeshPreviewGrantLane::RevisionNotice => Lane::RevisionNotice,
    };
    let sample_strategy = match args.sample_strategy {
        MeshPreviewGrantSampleStrategy::Random => SampleStrategy::Random,
        MeshPreviewGrantSampleStrategy::HighestTrust => SampleStrategy::HighestTrust,
        MeshPreviewGrantSampleStrategy::MostRecent => SampleStrategy::MostRecent,
    };

    let redaction_rules: Vec<String> = Vec::new();
    // DB-backed resolver is owned by a follow-up slice; for now the
    // preview runs against an empty candidate set and surfaces the
    // structural envelope including peer_not_in_group and
    // lane_already_granted cautions that derive from peer policy state.
    let memories: Vec<crate::mesh::lane_grant_preview::MemoryView<'_>> = Vec::new();
    let preview = compute_lane_grant_preview(&LaneGrantPreviewInput {
        peer_node_key: args.peer_node_key.as_str(),
        peer_in_group: false,
        lane,
        workspace_id: snapshot.workspace_id.as_str(),
        current_policy: IntendedLanePolicy::conservative_default(),
        proposed_policy: lane_grant_preview_proposed_policy(lane),
        memories: &memories,
        sample_strategy,
        limit: args.limit,
        redaction_rules: redaction_rules.as_slice(),
        sample_random_seed: args.seed,
    });

    let preview_value = serde_json::to_value(&preview).unwrap_or_else(|_| serde_json::json!({}));
    let response = serde_json::json!({
        "schema": "ee.response.v2",
        "success": true,
        "data": {
            "command": "mesh preview-grant",
            "workspaceId": snapshot.workspace_id,
            "preview": preview_value,
        },
        "degraded": [],
    });

    let _ = write_stdout(
        stdout,
        &(serde_json::to_string(&response).unwrap_or_default() + "\n"),
    );
    ProcessExitCode::Success
}

fn lane_grant_preview_proposed_policy(
    lane: crate::mesh::lane_grant_preview::Lane,
) -> crate::mesh::auto_enrollment_safety::IntendedLanePolicy {
    use crate::mesh::auto_enrollment_safety::{IntendedLanePolicy, LaneDecision};
    use crate::mesh::lane_grant_preview::Lane;
    let mut policy = IntendedLanePolicy::conservative_default();
    match lane {
        Lane::Metadata => policy.metadata = LaneDecision::Allow,
        Lane::Body => policy.body = LaneDecision::Allow,
        Lane::Embedding => policy.embedding = LaneDecision::Allow,
        Lane::GraphLink => policy.graph_link = LaneDecision::Allow,
        Lane::CurationSignal => policy.curation_signal = LaneDecision::Allow,
        Lane::RevisionNotice => policy.revision_notice = LaneDecision::Allow,
    }
    policy
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
    if cli.wants_json() {
        let autodiscovery = build_tailscale_autodiscovery_report(cli, &snapshot);
        return write_mesh_status_json_with_autodiscovery(stdout, &report, &autodiscovery);
    }
    write_mesh_report(cli, &report, &render_mesh_status_human(&report), stdout)
}

fn build_tailscale_autodiscovery_report(
    cli: &Cli,
    snapshot: &MeshForegroundSnapshot,
) -> TailscaleAutodiscoveryReport {
    let local = gather_mesh_status_tailscale_local_report(snapshot.mesh_enabled);
    build_tailscale_autodiscovery_report_from_local(cli, snapshot, local.as_ref())
}

fn build_tailscale_autodiscovery_report_from_local(
    cli: &Cli,
    snapshot: &MeshForegroundSnapshot,
    local: Option<&TailscaleLocalReport>,
) -> TailscaleAutodiscoveryReport {
    let workspace_path = cli.resolve_workspace();
    let lists = load_workspace_lists(&workspace_path).unwrap_or_default();
    let mut config = TailscaleAutodiscoveryConfig::new(
        snapshot.mesh_enabled,
        &snapshot.workspace_id,
        DiscoveryMode::from_env_discovery(|_| {}),
        &lists.allowlist,
        &lists.denylist,
    );
    config.peer_probe_timeout_ms = tailscale_peer_probe_timeout_ms_from_env_value(
        read_env_var(EnvVar::TailscalePeerProbeTimeoutMs).as_deref(),
    );
    config.total_budget_ms = tailscale_discovery_budget_ms_from_env_value(
        read_env_var(EnvVar::TailscaleDiscoveryBudgetMs).as_deref(),
    );
    let mut probe = TailscaleStatusCapabilityHelloProbe;
    autodiscover_tailscale_peers(local, &config, &mut probe)
}

fn gather_mesh_status_tailscale_local_report(mesh_enabled: bool) -> Option<TailscaleLocalReport> {
    if !mesh_enabled {
        return None;
    }
    let timeout_ms = tailscale_probe_timeout_ms_from_env_value(
        read_env_var(EnvVar::TailscaleProbeTimeoutMs).as_deref(),
    );
    let mut cli_config = TailscaleCliProbeConfig::mesh_enabled();
    cli_config.timeout_ms = timeout_ms;
    cli_config.binary_override = read_env_var(EnvVar::TailscaleBinaryOverride).map(PathBuf::from);
    cli_config.platform_hint = current_tailscale_platform();

    let mut socket_config = TailscaleSocketProbeConfig::mesh_enabled();
    socket_config.timeout_ms = timeout_ms;
    socket_config.platform_hint = current_tailscale_platform();
    if let Some(path) = read_env_var(EnvVar::TailscaleProbeSocketOverride)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    {
        socket_config.socket_candidates = vec![PathBuf::from(path)];
    }

    let mut socket_runner = SystemTailscaleSocketProbeRunner;
    let mut cli_runner = SystemTailscaleCliProbeRunner;
    Some(probe_tailscale_local_with_runners(
        &socket_config,
        &cli_config,
        &mut socket_runner,
        &mut cli_runner,
    ))
}

fn current_tailscale_platform() -> TailscalePlatform {
    if cfg!(target_os = "linux") {
        TailscalePlatform::Linux
    } else if cfg!(target_os = "macos") {
        TailscalePlatform::MacosOpen
    } else if cfg!(target_os = "windows") {
        TailscalePlatform::Windows
    } else {
        TailscalePlatform::Other
    }
}

fn write_mesh_status_json_with_autodiscovery<W: Write>(
    stdout: &mut W,
    report: &MeshCliStatusReport,
    autodiscovery: &TailscaleAutodiscoveryReport,
) -> ProcessExitCode {
    let mut data = serde_json::to_value(report).unwrap_or(serde_json::Value::Null);
    if let Some(discovery_slot) = data.pointer_mut("/autoEnrollment/discovery") {
        *discovery_slot = serde_json::to_value(autodiscovery).unwrap_or(serde_json::Value::Null);
    }
    let json = json!({
        "schema": crate::models::RESPONSE_SCHEMA_V1,
        "success": true,
        "data": data,
    });
    write_stdout(stdout, &(json.to_string() + "\n"))
}

fn handle_mesh_auto_enroll<W, E>(
    cli: &Cli,
    args: &MeshAutoEnrollArgs,
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
    let workspace_path = cli.resolve_workspace();
    let local = gather_mesh_status_tailscale_local_report(snapshot.mesh_enabled);
    let discovery = build_tailscale_autodiscovery_report_from_local(cli, &snapshot, local.as_ref());
    let existing_peers = match auto_enrollment_existing_peers(&snapshot) {
        Ok(peers) => peers,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let now = chrono::Utc::now().to_rfc3339();
    let mut report = plan_auto_enrollment(AutoEnrollmentInput {
        workspace_id: snapshot.workspace_id.clone(),
        workspace_path: snapshot.workspace_path.clone(),
        now: now.clone(),
        fresh_probe_invocations: u32::from(local.is_some()),
        tailnet_id: discovery.tailnet_id.clone(),
        tailnet_display_name: discovery.tailnet_display_name.clone(),
        self_node_key: discovery.self_node_key.clone(),
        discovered_peers: auto_enrollment_candidates_from_discovery(&discovery),
        tailnet_peers: auto_enrollment_candidates_from_local(local.as_ref()),
        existing_peers,
        options: AutoEnrollmentOptions {
            dry_run: args.dry_run,
            explain: args.explain,
            replace_manual_with_auto: args.replace_manual_with_auto,
            include_overrides: args.include.clone(),
            exclude_overrides: args.exclude.clone(),
            explain_skip: args.explain_skip.clone(),
            sync_once: AutoEnrollmentSyncOnceMode::DeferredToCaller,
            ..AutoEnrollmentOptions::default()
        },
    });

    let audit_id = match emit_safety_snapshot_audit(
        &connection,
        &report.safety_summary,
        Some("ee mesh auto-enroll"),
        report.peer_group_id.as_deref(),
    ) {
        Ok(audit_id) => audit_id,
        Err(error) => {
            return write_domain_error(
                &auto_enrollment_audit_domain_error(error),
                cli.wants_json(),
                stdout,
                stderr,
            );
        }
    };
    report.attach_audit_row_id(audit_id.clone());

    if report.materialization.writes_peer_rows {
        if let Err(error) = persist_auto_enroll_overrides(
            &workspace_path,
            &report.materialization.append_denylist_node_keys,
            &args.include,
            &args.exclude,
        ) {
            return write_domain_error(&error, cli.wants_json(), stdout, stderr);
        }
        let upserts = match auto_enrollment_peer_upserts(
            &snapshot.workspace_id,
            discovery.tailnet_id.as_deref().unwrap_or("tailnet_unknown"),
            discovery.tailnet_display_name.as_deref(),
            &now,
            &report.materialization.peers_to_upsert,
        ) {
            Ok(upserts) => upserts,
            Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
        };
        let revocations = match auto_enrollment_peer_revocations(
            &snapshot.workspace_id,
            &snapshot,
            &report.materialization.peers_to_revoke,
            &now,
        ) {
            Ok(revocations) => revocations,
            Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
        };
        if let Err(error) = connection.with_transaction(|| {
            for upsert in &upserts {
                connection.upsert_mesh_peer(upsert)?;
            }
            for revocation in &revocations {
                connection.upsert_mesh_peer(revocation)?;
            }
            Ok(())
        }) {
            return write_domain_error(
                &storage_error("Failed to materialize mesh auto-enrollment peers", error),
                cli.wants_json(),
                stdout,
                stderr,
            );
        }
    }

    if let Err(error) = update_materialization_outcome(
        &connection,
        &audit_id,
        &snapshot.workspace_id,
        report.materialization.outcome_to_record,
        Some("ee mesh auto-enroll"),
        report.peer_group_id.as_deref(),
    ) {
        return write_domain_error(
            &auto_enrollment_audit_domain_error(error),
            cli.wants_json(),
            stdout,
            stderr,
        );
    }

    if report.materialization.sync_once_after_materialization {
        let sync_options = MeshSyncSupervisorOptions::default();
        match run_mesh_sync_supervisor(&snapshot, &sync_options) {
            Ok(sync_report) => {
                if sync_report.degraded.is_empty() {
                    report.record_sync_once_success(sync_report.contacted_peers);
                } else {
                    report.record_sync_once_failure(
                        sync_report
                            .degraded
                            .iter()
                            .map(|item| item.code)
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                }
            }
            Err(message) => report.record_sync_once_failure(message),
        }
    }

    write_mesh_report(
        cli,
        &report,
        &render_mesh_auto_enroll_human(&report),
        stdout,
    )
}

fn handle_mesh_discovery_policy<W, E>(
    cli: &Cli,
    args: &MeshDiscoveryPolicyArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    let workspace_path = cli.resolve_workspace();
    let snapshot = match build_snapshot(cli, args.database.as_deref()) {
        Ok(snapshot) => snapshot,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };

    let mutation = match &args.command {
        None => Ok(None),
        Some(MeshDiscoveryPolicyCommand::Set(set_args)) => {
            apply_mesh_discovery_policy_set(cli, args.database.as_deref(), set_args)
        }
        Some(MeshDiscoveryPolicyCommand::Allow(node_args)) => {
            apply_mesh_discovery_policy_allow(cli, args.database.as_deref(), &node_args.node_key)
        }
        Some(MeshDiscoveryPolicyCommand::Deny(node_args)) => {
            apply_mesh_discovery_policy_deny(cli, args.database.as_deref(), &node_args.node_key)
        }
    };
    let (mutation, audit_id) = match mutation {
        Ok(value) => value.map_or((None, None), |(mutation, audit_id)| {
            (Some(mutation), Some(audit_id))
        }),
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };

    let state = match load_discovery_policy_state(&workspace_path, None, None) {
        Ok(state) => state,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let report = build_discovery_policy_report(&snapshot, &state, args.explain, mutation, audit_id);
    write_mesh_report(
        cli,
        &report,
        &render_mesh_discovery_policy_human(&report),
        stdout,
    )
}

fn apply_mesh_discovery_policy_set(
    cli: &Cli,
    database_override: Option<&Path>,
    args: &MeshDiscoveryPolicySetArgs,
) -> Result<Option<(MeshDiscoveryPolicyMutation, String)>, DomainError> {
    let (snapshot, connection) = open_mesh_policy_store(cli, database_override)?;
    let workspace_path = cli.resolve_workspace();
    let config_modes = load_workspace_policy_modes(&workspace_path)?;
    let discovery_mode = args
        .discovery_mode
        .map(DiscoveryMode::from)
        .or(config_modes.discovery_mode)
        .unwrap_or_default();
    let respond_mode = args
        .respond_mode
        .map(DiscoveryMode::from)
        .or(config_modes.respond_mode)
        .unwrap_or_default();
    let config_path = discovery_policy_config_path(&workspace_path);
    write_workspace_policy_modes(&config_path, discovery_mode, respond_mode)?;
    let mutation = MeshDiscoveryPolicyMutation {
        operation: "set",
        changed: true,
        node_key_hash: None,
        config_path: Some(config_path.display().to_string()),
        list_path: None,
    };
    let audit_id = record_discovery_policy_changed_audit(
        &connection,
        &snapshot.workspace_id,
        "set",
        &mutation,
        discovery_mode,
        respond_mode,
    )?;
    Ok(Some((mutation, audit_id)))
}

fn apply_mesh_discovery_policy_allow(
    cli: &Cli,
    database_override: Option<&Path>,
    node_key: &str,
) -> Result<Option<(MeshDiscoveryPolicyMutation, String)>, DomainError> {
    validate_node_key_for_cli(node_key)?;
    let (snapshot, connection) = open_mesh_policy_store(cli, database_override)?;
    let workspace_path = cli.resolve_workspace();
    let ee_dir = workspace_path.join(".ee");
    let discovery_path = ee_dir.join(DISCOVERY_ALLOWLIST_FILE);
    let respond_path = ee_dir.join(RESPOND_ALLOWLIST_FILE);
    let mut discovery_allowlist = load_node_key_list_for_cli(&discovery_path)?;
    let mut respond_allowlist = load_node_key_list_for_cli(&respond_path)?;
    let changed = discovery_allowlist.insert(node_key.to_owned())
        | respond_allowlist.insert(node_key.to_owned());
    write_node_key_list(&discovery_path, &discovery_allowlist)?;
    write_node_key_list(&respond_path, &respond_allowlist)?;
    let state = load_discovery_policy_state(&workspace_path, None, None)?;
    let mutation = MeshDiscoveryPolicyMutation {
        operation: "allow",
        changed,
        node_key_hash: Some(redacted_node_key_hash(node_key)),
        config_path: None,
        list_path: Some(format!(
            "{},{}",
            discovery_path.display(),
            respond_path.display()
        )),
    };
    let audit_id = record_discovery_policy_changed_audit(
        &connection,
        &snapshot.workspace_id,
        "allow",
        &mutation,
        state.discovery_mode,
        state.respond_mode,
    )?;
    Ok(Some((mutation, audit_id)))
}

fn apply_mesh_discovery_policy_deny(
    cli: &Cli,
    database_override: Option<&Path>,
    node_key: &str,
) -> Result<Option<(MeshDiscoveryPolicyMutation, String)>, DomainError> {
    validate_node_key_for_cli(node_key)?;
    let (snapshot, connection) = open_mesh_policy_store(cli, database_override)?;
    let workspace_path = cli.resolve_workspace();
    let deny_path = workspace_path.join(".ee").join(DISCOVERY_DENYLIST_FILE);
    let mut denylist = load_node_key_list_for_cli(&deny_path)?;
    let changed = denylist.insert(node_key.to_owned());
    write_node_key_list(&deny_path, &denylist)?;
    let state = load_discovery_policy_state(&workspace_path, None, None)?;
    let mutation = MeshDiscoveryPolicyMutation {
        operation: "deny",
        changed,
        node_key_hash: Some(redacted_node_key_hash(node_key)),
        config_path: None,
        list_path: Some(deny_path.display().to_string()),
    };
    let audit_id = record_discovery_policy_changed_audit(
        &connection,
        &snapshot.workspace_id,
        "deny",
        &mutation,
        state.discovery_mode,
        state.respond_mode,
    )?;
    Ok(Some((mutation, audit_id)))
}

fn handle_mesh_hello_responder<W, E>(
    cli: &Cli,
    args: &MeshHelloResponderArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    match &args.command {
        MeshHelloResponderCommand::Status(args) => {
            handle_mesh_hello_responder_status(cli, args, stdout, stderr)
        }
    }
}

fn handle_mesh_hello_responder_status<W, E>(
    cli: &Cli,
    args: &MeshHelloResponderStatusArgs,
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
    let report = match HelloResponderStatusReport::from_environment(snapshot.mesh_enabled) {
        Ok(report) => report,
        Err(error) => {
            let domain_error = DomainError::Configuration {
                message: error.to_string(),
                repair: Some(
                    "Use EE_MESH_HELLO_PORT=41888 or EE_MESH_HELLO_RESPONDER_DISABLED=true."
                        .to_owned(),
                ),
            };
            return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
        }
    };
    write_mesh_report(
        cli,
        &report,
        &render_mesh_hello_responder_status_human(&report),
        stdout,
    )
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
    let supervisor_options = MeshSyncSupervisorOptions {
        cadence_ms: args.cadence_ms,
        tick_limit: 1,
        peer_concurrency: args.peer_concurrency,
        body_fetch_budget_bytes: args.body_fetch_budget_bytes,
        stale_read_window_ms: args.stale_read_window_ms,
        time_budget_ms: args.time_budget_ms,
    };
    let supervisor =
        run_mesh_sync_supervisor(&snapshot, &supervisor_options).unwrap_or_else(|message| {
            MeshSyncSupervisorReport::runtime_error(&snapshot, &supervisor_options, &message)
        });
    let mut degraded = snapshot.degraded.clone();
    degraded.extend(supervisor.degraded.clone());
    if !degraded
        .iter()
        .any(|item| item.code == MESH_SYNC_ONCE_NETWORK_DEFERRED_CODE)
    {
        degraded.push(MeshCliDegradation::sync_once_network_deferred());
    }
    let report = MeshCliSyncReport {
        schema: MESH_CLI_SYNC_SCHEMA_V1,
        command: "mesh sync",
        once: args.once,
        mode: snapshot.mode.clone(),
        contacted_peers: supervisor.contacted_peers,
        supervisor,
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

fn run_mesh_sync_supervisor(
    snapshot: &MeshForegroundSnapshot,
    options: &MeshSyncSupervisorOptions,
) -> Result<MeshSyncSupervisorReport, String> {
    let runtime = crate::core::build_cli_runtime()
        .map_err(|error| format!("Failed to build Asupersync mesh supervisor runtime: {error}"))?;
    let task_snapshot = snapshot.clone();
    let task_options = options.clone();
    let join = runtime
        .handle()
        .try_spawn(async move {
            let Some(cx) = Cx::current() else {
                return Outcome::Err(
                    "Asupersync mesh supervisor task started without an ambient Cx".to_owned(),
                );
            };
            run_mesh_sync_supervisor_supervised(&cx, &task_snapshot, &task_options).await
        })
        .map_err(|error| format!("Failed to spawn Asupersync mesh supervisor: {error}"))?;
    match runtime.block_on(join) {
        Outcome::Ok(report) => Ok(report),
        Outcome::Err(message) => Err(message),
        Outcome::Cancelled(reason) => Err(format!(
            "Mesh sync supervisor cancelled: {}",
            crate::core::outcome::cancel_message(&reason)
        )),
        Outcome::Panicked(payload) => Err(format!("Mesh sync supervisor panicked: {payload}")),
    }
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

fn open_mesh_policy_store(
    cli: &Cli,
    database_override: Option<&Path>,
) -> Result<(MeshForegroundSnapshot, DbConnection), DomainError> {
    let snapshot = build_snapshot(cli, database_override)?;
    if !snapshot.initialized {
        return Err(DomainError::Storage {
            message: format!(
                "Cannot mutate mesh discovery policy because {} does not exist",
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

fn load_discovery_policy_state(
    workspace_path: &Path,
    discovery_override: Option<DiscoveryMode>,
    respond_override: Option<DiscoveryMode>,
) -> Result<DiscoveryPolicyState, DomainError> {
    let config_modes = load_workspace_policy_modes(workspace_path)?;
    let discovery_mode = discovery_override
        .or(optional_env_discovery_mode(EnvVar::TailscaleDiscoveryMode)?)
        .or(config_modes.discovery_mode)
        .unwrap_or_default();
    let respond_mode = respond_override
        .or(optional_env_discovery_mode(EnvVar::TailscaleRespondMode)?)
        .or(config_modes.respond_mode)
        .unwrap_or_default();
    let lists = load_workspace_lists(workspace_path).map_err(discovery_list_domain_error)?;
    let self_advertised_tags = Vec::new();
    let degraded = evaluate_policy_degradations(
        discovery_mode,
        respond_mode,
        &self_advertised_tags,
        &lists.allowlist,
    )
    .into_iter()
    .map(|item| MeshCliDegradation {
        code: item.code,
        severity: item.severity,
        message: item.message,
        repair: item.repair.to_owned(),
    })
    .collect();
    Ok(DiscoveryPolicyState {
        discovery_mode,
        respond_mode,
        lists,
        degraded,
    })
}

fn optional_env_discovery_mode(variable: EnvVar) -> Result<Option<DiscoveryMode>, DomainError> {
    let Some(raw) = read_env_var(variable) else {
        return Ok(None);
    };
    raw.trim()
        .parse::<DiscoveryMode>()
        .map(Some)
        .map_err(|error| DomainError::Configuration {
            message: format!(
                "{} has invalid discovery policy mode: {error}",
                variable.name()
            ),
            repair: Some("Use service_tag, auto_admit, or allowlist.".to_owned()),
        })
}

fn load_workspace_policy_modes(workspace_path: &Path) -> Result<WorkspacePolicyModes, DomainError> {
    let path = discovery_policy_config_path(workspace_path);
    let body = match fs::read_to_string(&path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(WorkspacePolicyModes::default());
        }
        Err(error) => {
            return Err(DomainError::Storage {
                message: format!(
                    "Failed to read mesh discovery policy config {}: {error}",
                    path.display()
                ),
                repair: Some(
                    "Check workspace .ee permissions or re-run `ee mesh discovery-policy set`."
                        .to_owned(),
                ),
            });
        }
    };
    let document = body
        .parse::<DocumentMut>()
        .map_err(|error| DomainError::Configuration {
            message: format!(
                "Failed to parse mesh discovery policy config {}: {error}",
                path.display()
            ),
            repair: Some("Re-run `ee mesh discovery-policy set` to rewrite the config.".to_owned()),
        })?;
    Ok(WorkspacePolicyModes {
        discovery_mode: optional_policy_mode_from_document(&document, "discovery_mode")?,
        respond_mode: optional_policy_mode_from_document(&document, "respond_mode")?,
    })
}

fn optional_policy_mode_from_document(
    document: &DocumentMut,
    key: &'static str,
) -> Result<Option<DiscoveryMode>, DomainError> {
    let Some(item) = document.get(key) else {
        return Ok(None);
    };
    let Some(raw) = item.as_str() else {
        return Err(DomainError::Configuration {
            message: format!("mesh discovery policy `{key}` must be a string"),
            repair: Some("Use service_tag, auto_admit, or allowlist.".to_owned()),
        });
    };
    raw.parse::<DiscoveryMode>()
        .map(Some)
        .map_err(|error| DomainError::Configuration {
            message: format!("mesh discovery policy `{key}` is invalid: {error}"),
            repair: Some("Use service_tag, auto_admit, or allowlist.".to_owned()),
        })
}

fn discovery_policy_config_path(workspace_path: &Path) -> PathBuf {
    workspace_path
        .join(".ee")
        .join(DISCOVERY_POLICY_CONFIG_FILE)
}

fn write_workspace_policy_modes(
    path: &Path,
    discovery_mode: DiscoveryMode,
    respond_mode: DiscoveryMode,
) -> Result<(), DomainError> {
    ensure_writable_regular_file(path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
            message: format!("Failed to create mesh discovery policy directory: {error}"),
            repair: Some("Check workspace .ee directory permissions.".to_owned()),
        })?;
    }
    let mut document = if path.is_file() {
        fs::read_to_string(path)
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Failed to read mesh discovery policy config {}: {error}",
                    path.display()
                ),
                repair: Some("Check workspace .ee directory permissions.".to_owned()),
            })?
            .parse::<DocumentMut>()
            .map_err(|error| DomainError::Configuration {
                message: format!(
                    "Failed to parse mesh discovery policy config {}: {error}",
                    path.display()
                ),
                repair: Some(
                    "Fix or remove the invalid discovery policy config before retrying.".to_owned(),
                ),
            })?
    } else {
        DocumentMut::new()
    };
    document["discovery_mode"] = value(discovery_mode.as_str());
    document["respond_mode"] = value(respond_mode.as_str());
    fs::write(path, document.to_string()).map_err(|error| DomainError::Storage {
        message: format!(
            "Failed to write mesh discovery policy config {}: {error}",
            path.display()
        ),
        repair: Some("Check workspace .ee directory permissions and retry.".to_owned()),
    })
}

fn load_node_key_list_for_cli(
    path: &Path,
) -> Result<std::collections::BTreeSet<String>, DomainError> {
    load_node_key_list(path).map_err(discovery_list_domain_error)
}

fn write_node_key_list(
    path: &Path,
    node_keys: &std::collections::BTreeSet<String>,
) -> Result<(), DomainError> {
    ensure_writable_regular_file(path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
            message: format!("Failed to create mesh discovery list directory: {error}"),
            repair: Some("Check workspace .ee directory permissions.".to_owned()),
        })?;
    }
    let mut body = String::from("node_keys = [\n");
    for node_key in node_keys {
        body.push_str(&format!("  \"{node_key}\",\n"));
    }
    body.push_str("]\n");
    fs::write(path, body).map_err(|error| DomainError::Storage {
        message: format!(
            "Failed to write mesh discovery list {}: {error}",
            path.display()
        ),
        repair: Some("Check workspace .ee directory permissions and retry.".to_owned()),
    })
}

fn ensure_writable_regular_file(path: &Path) -> Result<(), DomainError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(metadata) if metadata.file_type().is_symlink() => Err(DomainError::PolicyDenied {
            message: format!(
                "Refusing to write mesh discovery policy through symlink {}",
                path.display()
            ),
            repair: Some(
                "Replace the symlink with a regular file owned by this workspace.".to_owned(),
            ),
        }),
        Ok(_) => Err(DomainError::Storage {
            message: format!(
                "Refusing to write mesh discovery policy because {} is not a regular file",
                path.display()
            ),
            repair: Some("Replace the path with a regular TOML file.".to_owned()),
        }),
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
                "Failed to inspect mesh discovery policy path {}: {error}",
                path.display()
            ),
            repair: Some("Check workspace .ee directory permissions.".to_owned()),
        }),
    }
}

fn validate_node_key_for_cli(node_key: &str) -> Result<(), DomainError> {
    validate_node_key(node_key).map_err(discovery_list_domain_error)
}

fn discovery_list_domain_error(error: crate::mesh::discovery_policy::LoadListError) -> DomainError {
    DomainError::Configuration {
        message: error.to_string(),
        repair: Some(
            "Use nodekey: plus 64 lowercase hex characters in discovery policy lists.".to_owned(),
        ),
    }
}

fn redacted_node_key_hash(node_key: &str) -> String {
    format!("blake3:{}", blake3::hash(node_key.as_bytes()).to_hex())
}

fn auto_enrollment_existing_peers(
    snapshot: &MeshForegroundSnapshot,
) -> Result<Vec<ExistingAutoEnrollmentPeer>, DomainError> {
    let mut peers = Vec::new();
    for row in &snapshot.peers {
        let record = enrolled_peer_record_from_policy_summary(
            row.policy_summary_json.as_deref(),
            &row.peer_id,
        )?;
        if let Some(record) = record {
            peers.push(ExistingAutoEnrollmentPeer {
                peer_id: row.peer_id.clone(),
                node_key: record.endpoint.tailscale_node_key,
                tailnet_id: Some(record.endpoint.tailnet_id),
                hostname: record.alias,
                tailscale_ip: record.endpoint.endpoint,
                magic_dns_name: record.endpoint.magic_dns_name,
                ee_protocol_version: record.handshake.responder_protocol_version,
                enrollment_source: record.trust_established_by,
                enabled: row.enabled,
            });
        } else {
            peers.push(ExistingAutoEnrollmentPeer {
                peer_id: row.peer_id.clone(),
                node_key: row.origin_node_id.clone(),
                tailnet_id: None,
                hostname: row
                    .display_name
                    .clone()
                    .unwrap_or_else(|| row.origin_node_id.clone()),
                tailscale_ip: row.origin_node_id.clone(),
                magic_dns_name: None,
                ee_protocol_version: "unknown".to_owned(),
                enrollment_source: "manual".to_owned(),
                enabled: row.enabled,
            });
        }
    }
    Ok(peers)
}

fn auto_enrollment_candidates_from_discovery(
    report: &TailscaleAutodiscoveryReport,
) -> Vec<AutoEnrollmentCandidate> {
    report
        .ee_capable_peers
        .iter()
        .map(|peer| AutoEnrollmentCandidate {
            node_key: peer.node_key.clone(),
            tailscale_ip: peer.tailscale_ip.clone(),
            magic_dns_name: peer.magic_dns_name.clone(),
            hostname: peer
                .hostname
                .clone()
                .unwrap_or_else(|| peer.node_key.clone()),
            ee_protocol_version: peer.ee_protocol_version.clone(),
            discovery_policy_decision: peer.discovery_policy_decision.clone(),
        })
        .collect()
}

fn auto_enrollment_candidates_from_local(
    local: Option<&TailscaleLocalReport>,
) -> Vec<AutoEnrollmentCandidate> {
    local
        .into_iter()
        .flat_map(|report| report.peers.iter())
        .filter_map(|peer| {
            let tailscale_ip = peer.tailscale_ips.first()?.clone();
            let capability = peer.ee_capability.as_ref();
            Some(AutoEnrollmentCandidate {
                node_key: peer.node_key.clone(),
                tailscale_ip,
                magic_dns_name: peer.magic_dns_name.clone(),
                hostname: peer
                    .hostname
                    .clone()
                    .unwrap_or_else(|| peer.node_key.clone()),
                ee_protocol_version: capability
                    .map(|capability| capability.ee_protocol_version.clone())
                    .unwrap_or_else(|| "1.0".to_owned()),
                discovery_policy_decision: "force_include_override".to_owned(),
            })
        })
        .collect()
}

fn auto_enrollment_peer_upserts(
    workspace_id: &str,
    tailnet_id: &str,
    tailnet_display_name: Option<&str>,
    now: &str,
    candidates: &[AutoEnrollmentCandidate],
) -> Result<Vec<UpsertMeshPeerInput>, DomainError> {
    let mut upserts = Vec::new();
    for candidate in candidates {
        let report = enroll_peer(MeshPeerEnrollInput {
            workspace_id: workspace_id.to_owned(),
            alias: candidate.hostname.clone(),
            endpoint: MeshPeerEndpoint {
                tailscale_node_key: candidate.node_key.clone(),
                tailnet_id: tailnet_id.to_owned(),
                tailnet_display_name: tailnet_display_name.map(str::to_owned),
                endpoint: candidate.tailscale_ip.clone(),
                magic_dns_name: candidate.magic_dns_name.clone(),
            },
            capability_profile: MeshPeerCapabilityProfile::MetadataOnly,
            handshake: MeshPeerHandshake::granted(
                format!("auto_enroll:{}", candidate.node_key),
                candidate.ee_protocol_version.clone(),
                candidate.node_key.clone(),
                vec!["metadata".to_owned(), "revisionNotice".to_owned()],
            ),
            public_key_fingerprint: auto_enrollment_public_key_fingerprint(&candidate.node_key),
            now: now.to_owned(),
            explicit_human_consent: true,
        });
        let peer_message = report.message.clone();
        let mut peer = report.peer.ok_or_else(|| DomainError::PolicyDenied {
            message: format!(
                "Auto-enrollment could not compose peer enrollment for {}: {}",
                candidate.node_key, peer_message
            ),
            repair: Some(
                "Inspect the candidate hello response and retry auto-enrollment.".to_owned(),
            ),
        })?;
        peer.trust_established_by = "tailscale_auto_enrollment".to_owned();
        let policy_summary_json =
            serde_json::to_string(&peer).map_err(|error| DomainError::Usage {
                message: format!("Failed to serialize auto-enrolled mesh peer: {error}"),
                repair: Some("Retry the command or report the serialization failure.".to_owned()),
            })?;
        upserts.push(UpsertMeshPeerInput {
            workspace_id: workspace_id.to_owned(),
            peer_id: peer.peer_id,
            origin_node_id: build_peer_origin_node_id(&candidate.node_key),
            display_name: Some(candidate.hostname.clone()),
            policy_summary_json: Some(policy_summary_json),
            enabled: true,
            last_seen_at: Some(now.to_owned()),
        });
    }
    Ok(upserts)
}

fn auto_enrollment_peer_revocations(
    workspace_id: &str,
    snapshot: &MeshForegroundSnapshot,
    node_keys: &[String],
    now: &str,
) -> Result<Vec<UpsertMeshPeerInput>, DomainError> {
    let node_keys: BTreeSet<&str> = node_keys.iter().map(String::as_str).collect();
    if node_keys.is_empty() {
        return Ok(Vec::new());
    }

    let mut revocations = Vec::new();
    for row in &snapshot.peers {
        if !row.enabled {
            continue;
        }
        let node_key = auto_enrollment_node_key_for_row(row)?;
        if !node_keys.contains(node_key.as_str()) {
            continue;
        }
        revocations.push(UpsertMeshPeerInput {
            workspace_id: workspace_id.to_owned(),
            peer_id: row.peer_id.clone(),
            origin_node_id: row.origin_node_id.clone(),
            display_name: row.display_name.clone(),
            policy_summary_json: row.policy_summary_json.clone(),
            enabled: false,
            last_seen_at: Some(now.to_owned()),
        });
    }
    Ok(revocations)
}

fn auto_enrollment_node_key_for_row(
    row: &crate::mesh::foreground_cli::MeshPeerRow,
) -> Result<String, DomainError> {
    enrolled_peer_record_from_policy_summary(row.policy_summary_json.as_deref(), &row.peer_id).map(
        |record| {
            record.map_or_else(
                || row.origin_node_id.clone(),
                |peer| peer.endpoint.tailscale_node_key,
            )
        },
    )
}

fn auto_enrollment_public_key_fingerprint(node_key: &str) -> String {
    format!("auto:{}", blake3::hash(node_key.as_bytes()).to_hex())
}

fn persist_auto_enroll_overrides(
    workspace_path: &Path,
    denylist_node_keys: &[String],
    include_node_keys: &[String],
    exclude_node_keys: &[String],
) -> Result<(), DomainError> {
    let valid_includes = valid_node_key_set(include_node_keys);
    let valid_excludes = valid_node_key_set(exclude_node_keys);
    if !valid_includes.is_empty() || !valid_excludes.is_empty() {
        write_auto_enroll_overrides_file(workspace_path, &valid_includes, &valid_excludes)?;
    }
    let valid_denylist = valid_node_key_set(denylist_node_keys);
    if valid_denylist.is_empty() {
        return Ok(());
    }
    let deny_path = workspace_path.join(".ee").join(DISCOVERY_DENYLIST_FILE);
    let mut denylist = load_node_key_list_for_cli(&deny_path)?;
    denylist.extend(valid_denylist);
    write_node_key_list(&deny_path, &denylist)
}

fn valid_node_key_set(node_keys: &[String]) -> std::collections::BTreeSet<String> {
    node_keys
        .iter()
        .map(|value| value.trim())
        .filter(|value| validate_node_key(value).is_ok())
        .map(str::to_owned)
        .collect()
}

fn write_auto_enroll_overrides_file(
    workspace_path: &Path,
    include_node_keys: &std::collections::BTreeSet<String>,
    exclude_node_keys: &std::collections::BTreeSet<String>,
) -> Result<(), DomainError> {
    let path = workspace_path.join(".ee").join(AUTO_ENROLL_OVERRIDES_FILE);
    ensure_writable_regular_file(&path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| DomainError::Storage {
            message: format!("Failed to create mesh auto-enroll override directory: {error}"),
            repair: Some("Check workspace .ee directory permissions.".to_owned()),
        })?;
    }
    let mut document = if path.is_file() {
        fs::read_to_string(&path)
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to read {}: {error}", path.display()),
                repair: Some("Check workspace .ee directory permissions.".to_owned()),
            })?
            .parse::<DocumentMut>()
            .map_err(|error| DomainError::Configuration {
                message: format!("Failed to parse {}: {error}", path.display()),
                repair: Some("Fix the invalid auto-enroll override TOML and retry.".to_owned()),
            })?
    } else {
        DocumentMut::new()
    };
    let mut includes = node_key_set_from_document(&document, "include_node_keys");
    includes.extend(include_node_keys.iter().cloned());
    let mut excludes = node_key_set_from_document(&document, "exclude_node_keys");
    excludes.extend(exclude_node_keys.iter().cloned());
    set_document_node_key_array(&mut document, "include_node_keys", &includes);
    set_document_node_key_array(&mut document, "exclude_node_keys", &excludes);
    fs::write(&path, document.to_string()).map_err(|error| DomainError::Storage {
        message: format!("Failed to write {}: {error}", path.display()),
        repair: Some("Check workspace .ee directory permissions and retry.".to_owned()),
    })
}

fn node_key_set_from_document(
    document: &DocumentMut,
    key: &'static str,
) -> std::collections::BTreeSet<String> {
    document
        .get(key)
        .and_then(Item::as_array)
        .into_iter()
        .flat_map(|array| array.iter())
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn set_document_node_key_array(
    document: &mut DocumentMut,
    key: &'static str,
    node_keys: &std::collections::BTreeSet<String>,
) {
    let mut array = Array::default();
    for node_key in node_keys {
        array.push(node_key.as_str());
    }
    document[key] = Item::Value(Value::Array(array));
}

fn auto_enrollment_audit_domain_error(
    error: crate::mesh::auto_enrollment_safety::SafetySnapshotError,
) -> DomainError {
    DomainError::Storage {
        message: format!("auto_enrollment_audit_failed: {error}"),
        repair: Some(
            "Inspect the audit chain with `ee audit verify --json` before retrying auto-enrollment."
                .to_owned(),
        ),
    }
}

fn record_discovery_policy_changed_audit(
    connection: &DbConnection,
    workspace_id: &str,
    operation: &'static str,
    mutation: &MeshDiscoveryPolicyMutation,
    discovery_mode: DiscoveryMode,
    respond_mode: DiscoveryMode,
) -> Result<String, DomainError> {
    let details = json!({
        "schema": "ee.mesh.discovery_policy_changed.v1",
        "operation": operation,
        "changed": mutation.changed,
        "discoveryMode": discovery_mode.as_str(),
        "respondMode": respond_mode.as_str(),
        "nodeKeyHash": mutation.node_key_hash,
        "configPath": mutation.config_path,
        "listPath": mutation.list_path,
    });
    let details_json = serde_json::to_string(&details).map_err(|error| DomainError::Usage {
        message: format!("Failed to serialize mesh discovery policy audit details: {error}"),
        repair: Some("Retry the command or report the serialization failure.".to_owned()),
    })?;
    let audit_id = generate_audit_id();
    connection
        .insert_audit(
            &audit_id,
            &CreateAuditInput {
                workspace_id: Some(workspace_id.to_owned()),
                actor: Some("ee mesh discovery-policy".to_owned()),
                action: audit_actions::MESH_DISCOVERY_POLICY_CHANGED.to_owned(),
                target_type: Some("mesh".to_owned()),
                target_id: Some("discovery_policy".to_owned()),
                details: Some(details_json),
            },
        )
        .map_err(|error| storage_error("Failed to write mesh discovery policy audit", error))?;
    Ok(audit_id)
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
    import_mesh_artifact_into_connection(&connection, &workspace_id, artifact)
}

fn import_mesh_artifact_into_connection(
    connection: &DbConnection,
    workspace_id: &str,
    artifact: &MeshExportArtifact,
) -> Result<(usize, usize, usize), DomainError> {
    let mut imported_peer_count = 0;
    for peer in &artifact.peers {
        let existing = connection
            .get_mesh_peer(workspace_id, &peer.peer_id)
            .map_err(|error| storage_error("Failed to inspect existing mesh peer", error))?;
        let changed = existing
            .as_ref()
            .is_none_or(|stored| !mesh_peer_matches_row(stored, workspace_id, peer));
        connection
            .upsert_mesh_peer(&UpsertMeshPeerInput {
                workspace_id: workspace_id.to_owned(),
                peer_id: peer.peer_id.clone(),
                origin_node_id: peer.origin_node_id.clone(),
                display_name: peer.display_name.clone(),
                policy_summary_json: peer.policy_summary_json.clone(),
                enabled: peer.enabled,
                last_seen_at: Some(peer.last_seen_at.clone()),
            })
            .map_err(|error| storage_error("Failed to import mesh peer", error))?;
        if changed {
            imported_peer_count += 1;
        }
    }

    let mut imported_cursor_count = 0;
    for cursor in &artifact.cursors {
        let existing = connection
            .get_mesh_peer_cursor(workspace_id, &cursor.peer_id, &cursor.origin_workspace_id)
            .map_err(|error| storage_error("Failed to inspect existing mesh peer cursor", error))?;
        let changed = existing
            .as_ref()
            .is_none_or(|stored| !mesh_cursor_matches_row(stored, workspace_id, cursor));
        connection
            .upsert_mesh_peer_cursor(&UpsertMeshPeerCursorInput {
                workspace_id: workspace_id.to_owned(),
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
        if changed {
            imported_cursor_count += 1;
        }
    }

    let mut imported_event_count = 0;
    for event in &artifact.events {
        let existing = connection
            .get_mesh_import_ledger_event(
                workspace_id,
                &event.origin_node_id,
                &event.origin_workspace_id,
                event.seq,
            )
            .map_err(|error| storage_error("Failed to inspect existing mesh event", error))?;
        let changed = existing.is_none();
        connection
            .insert_mesh_import_ledger_event(&InsertMeshImportLedgerEventInput {
                workspace_id: workspace_id.to_owned(),
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
        if changed {
            enqueue_mesh_import_index_job(connection, workspace_id, event)?;
            imported_event_count += 1;
        }
    }
    Ok((
        imported_peer_count,
        imported_cursor_count,
        imported_event_count,
    ))
}

fn enqueue_mesh_import_index_job(
    connection: &DbConnection,
    workspace_id: &str,
    event: &crate::mesh::foreground_cli::MeshEventRow,
) -> Result<(), DomainError> {
    let index_job_id = stable_mesh_import_index_job_id(workspace_id, event);
    if connection
        .get_search_index_job(&index_job_id)
        .map_err(|error| storage_error("Failed to inspect mesh import index job", error))?
        .is_some()
    {
        return Ok(());
    }

    connection
        .insert_search_index_job(
            &index_job_id,
            &CreateSearchIndexJobInput {
                workspace_id: workspace_id.to_owned(),
                job_type: SearchIndexJobType::SingleDocument,
                document_source: Some("import".to_owned()),
                document_id: Some(event.event_id.clone()),
                documents_total: 1,
            },
        )
        .map_err(|error| storage_error("Failed to enqueue mesh import index job", error))
}

fn stable_mesh_import_index_job_id(
    workspace_id: &str,
    event: &crate::mesh::foreground_cli::MeshEventRow,
) -> String {
    let hash = blake3::hash(
        format!(
            "mesh-import-index-job:{workspace_id}:{}:{}:{}:{}",
            event.origin_node_id, event.origin_workspace_id, event.seq, event.event_hash
        )
        .as_bytes(),
    )
    .to_hex();
    format!("sidx_{}", &hash[..26])
}

fn mesh_peer_matches_row(
    stored: &StoredMeshPeer,
    workspace_id: &str,
    row: &crate::mesh::foreground_cli::MeshPeerRow,
) -> bool {
    stored.workspace_id == workspace_id
        && stored.peer_id == row.peer_id
        && stored.origin_node_id == row.origin_node_id
        && stored.display_name == row.display_name
        && stored.policy_summary_json == row.policy_summary_json
        && stored.enabled == row.enabled
        && stored.last_seen_at == row.last_seen_at
}

fn mesh_cursor_matches_row(
    stored: &StoredMeshPeerCursor,
    workspace_id: &str,
    row: &crate::mesh::foreground_cli::MeshCursorRow,
) -> bool {
    stored.workspace_id == workspace_id
        && stored.peer_id == row.peer_id
        && stored.origin_node_id == row.origin_node_id
        && stored.origin_workspace_id == row.origin_workspace_id
        && stored.last_seq == row.last_seq
        && stored.tip_event_hash == row.tip_event_hash
        && stored.tip_audit_hash == row.tip_audit_hash
        && stored.status == row.status
        && stored.updated_at == row.updated_at
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
            let data = match serde_json::to_value(report) {
                Ok(data) => data,
                Err(error) => {
                    let domain_error = DomainError::Storage {
                        message: format!("Failed to serialize mesh CLI report: {error}"),
                        repair: Some(
                            "Retry with --format json and report the serialization failure."
                                .to_owned(),
                        ),
                    };
                    let rendered =
                        output::render_toon_from_json(&output::error_response_json(&domain_error));
                    let write_exit = write_stdout(stdout, &(rendered + "\n"));
                    return if write_exit == ProcessExitCode::Success {
                        domain_error.exit_code()
                    } else {
                        write_exit
                    };
                }
            };
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

fn build_discovery_policy_report(
    snapshot: &MeshForegroundSnapshot,
    state: &DiscoveryPolicyState,
    explain: bool,
    mutation: Option<MeshDiscoveryPolicyMutation>,
    audit_id: Option<String>,
) -> MeshDiscoveryPolicyReport {
    MeshDiscoveryPolicyReport {
        schema: DISCOVERY_POLICY_SCHEMA_V1,
        command: "mesh discovery-policy",
        workspace_id: snapshot.workspace_id.clone(),
        workspace_path: snapshot.workspace_path.clone(),
        discovery_mode: state.discovery_mode.as_str().to_owned(),
        respond_mode: state.respond_mode.as_str().to_owned(),
        allowlisted_node_keys: state.lists.allowlist.iter().cloned().collect(),
        respond_allowlisted_node_keys: state.lists.respond_allowlist.iter().cloned().collect(),
        denied_node_keys: state.lists.denylist.iter().cloned().collect(),
        degraded: state.degraded.clone(),
        effective_decision_preview: if explain {
            discovery_policy_decision_preview(state)
        } else {
            Vec::new()
        },
        mutation,
        audit_id,
        next_commands: vec![
            "ee mesh discovery-policy --explain --json".to_owned(),
            "ee mesh discovery-policy set --discovery-mode allowlist --respond-mode allowlist --json"
                .to_owned(),
            "ee mesh discovery-policy allow <node-key> --json".to_owned(),
            "ee mesh discovery-policy deny <node-key> --json".to_owned(),
        ],
    }
}

fn discovery_policy_decision_preview(
    state: &DiscoveryPolicyState,
) -> Vec<MeshDiscoveryPolicyDecisionPreview> {
    const TAGGED_NODE: &str =
        "nodekey:00000000000000000000000000000000000000000000000000000000000000aa";
    const UNTAGGED_NODE: &str =
        "nodekey:00000000000000000000000000000000000000000000000000000000000000bb";
    const SELF_NODE: &str =
        "nodekey:00000000000000000000000000000000000000000000000000000000000000ff";

    let tagged = vec![EE_MESH_SERVICE_TAG.to_owned()];
    let no_tags = Vec::new();
    let mut out = Vec::new();
    for (node_key, advertised_tags) in [(TAGGED_NODE, &tagged), (UNTAGGED_NODE, &no_tags)] {
        let (decision, reason) = decide_discovery(&DiscoveryDecisionInput {
            mode: state.discovery_mode,
            peer_node_key: node_key,
            peer_advertised_tags: advertised_tags,
            self_node_key: SELF_NODE,
            allowlist: &state.lists.allowlist,
            denylist: &state.lists.denylist,
        });
        out.push(MeshDiscoveryPolicyDecisionPreview {
            direction: "discovery",
            node_key: node_key.to_owned(),
            advertised_tags: advertised_tags.clone(),
            decision: match decision {
                DiscoveryDecision::Probe => "probe",
                DiscoveryDecision::Skip => "skip",
            }
            .to_owned(),
            reason: reason.as_str().to_owned(),
        });
    }

    let (consent, reason) = decide_respond(&RespondDecisionInput {
        mode: state.respond_mode,
        requester_node_key: TAGGED_NODE,
        requester_advertised_tags: &tagged,
        self_advertised_tags: &no_tags,
        respond_allowlist: &state.lists.respond_allowlist,
        denylist: &state.lists.denylist,
    });
    out.push(MeshDiscoveryPolicyDecisionPreview {
        direction: "respond",
        node_key: TAGGED_NODE.to_owned(),
        advertised_tags: tagged,
        decision: match consent {
            DiscoveryConsent::Granted => "granted",
            DiscoveryConsent::Denied => "denied",
        }
        .to_owned(),
        reason: reason.as_str().to_owned(),
    });
    out
}

fn render_mesh_discovery_policy_human(report: &MeshDiscoveryPolicyReport) -> String {
    let mut output = format!(
        "Mesh discovery policy\n  Workspace: {}\n  discoveryMode: {}\n  respondMode: {}\n  Allowlist: {} discovery, {} responder\n  Denylist: {}\n",
        report.workspace_path,
        report.discovery_mode,
        report.respond_mode,
        report.allowlisted_node_keys.len(),
        report.respond_allowlisted_node_keys.len(),
        report.denied_node_keys.len(),
    );
    if let Some(mutation) = &report.mutation {
        output.push_str(&format!(
            "  Mutation: {} changed={}\n",
            mutation.operation, mutation.changed
        ));
    }
    if let Some(audit_id) = &report.audit_id {
        output.push_str(&format!("  Audit: {audit_id}\n"));
    }
    if !report.effective_decision_preview.is_empty() {
        output.push_str("  Decision preview:\n");
        for preview in &report.effective_decision_preview {
            output.push_str(&format!(
                "    - {} {} => {} ({})\n",
                preview.direction, preview.node_key, preview.decision, preview.reason
            ));
        }
    }
    append_degradations(&mut output, &report.degraded);
    output
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

fn render_mesh_auto_enroll_human(report: &AutoEnrollmentResult) -> String {
    let mut output = format!(
        "Mesh auto-enroll: {outcome}\n  Peer group: {peer_group}\n  Peer set hash: {peer_set_hash}\n  Audit: {audit}\n  Peers selected: {peer_count}\n  Lane policy: metadata={metadata} body={body} embedding={embedding} graphLink={graph_link} revisionNotice={revision_notice} curationSignal={curation_signal}\n  Sync-once: attempted={sync_attempted} success={sync_success}\n",
        outcome = report.outcome,
        peer_group = report.peer_group_id.as_deref().unwrap_or("none"),
        peer_set_hash = report.peer_set_hash.as_deref().unwrap_or("none"),
        audit = report.audit_row_id.as_deref().unwrap_or("not written"),
        peer_count = report.enrollment_outcomes.len(),
        metadata = report.lane_policy.metadata,
        body = report.lane_policy.body,
        embedding = report.lane_policy.embedding,
        graph_link = report.lane_policy.graph_link,
        revision_notice = report.lane_policy.revision_notice,
        curation_signal = report.lane_policy.curation_signal,
        sync_attempted = report.sync_once_result.attempted,
        sync_success = report.sync_once_result.success,
    );
    if !report.overrides_applied.is_empty() {
        output.push_str("  Overrides:\n");
        for item in &report.overrides_applied {
            output.push_str(&format!(
                "    - {} {} applied={}: {}\n",
                item.kind, item.node_key, item.applied, item.reason
            ));
        }
    }
    if !report.explanation.is_empty() {
        output.push_str("  Explanation:\n");
        for item in &report.explanation {
            output.push_str(&format!(
                "    - {} => {} ({})\n",
                item.node_key, item.decision, item.reason
            ));
        }
    }
    if !report.degraded.is_empty() {
        output.push_str("  Degraded:\n");
        for item in &report.degraded {
            output.push_str(&format!(
                "    - {} [{}]: {} Repair: {}\n",
                item.code, item.severity, item.message, item.repair
            ));
        }
    }
    output
}

fn render_mesh_hello_responder_status_human(report: &HelloResponderStatusReport) -> String {
    let mut output = format!(
        "Mesh hello responder: {state}\n  Listen address: {listen}\n  Requests accepted 1h: {accepted}\n  Requests denied 1h: {denied}\n  Requests rate-limited 1h: {rate_limited}\n  Crash count 24h: {crashes}\n",
        state = if report.running {
            "running"
        } else {
            "not running"
        },
        listen = report.listen_address.as_deref().unwrap_or("not bound"),
        accepted = report.accepted_requests_1h,
        denied = report.denied_requests_1h,
        rate_limited = report.rate_limited_requests_1h,
        crashes = report.crash_count_24h,
    );
    if let Some(last_request_at) = &report.last_request_at {
        output.push_str(&format!("  Last request: {last_request_at}\n"));
    }
    if let Some(last_restart_at) = &report.last_restart_at {
        output.push_str(&format!("  Last restart: {last_restart_at}\n"));
    }
    if !report.degraded.is_empty() {
        output.push_str("  Degraded:\n");
        for item in &report.degraded {
            output.push_str(&format!(
                "    - {} [{}]: {} Repair: {}\n",
                item.code, item.severity, item.message, item.repair
            ));
        }
    }
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
        "Mesh sync --once\n  Mode: {}\n  Supervisor: {} ({})\n  Peer slots: {}/{}\n  Body budget: {} bytes\n  Stale-read window: {} ms\n  Contacted peers: {}\n  Export fallback: {}\n  Import fallback: {}\n",
        report.mode,
        report.supervisor.supervisor,
        report.supervisor.health,
        report.supervisor.active_peer_count,
        report.supervisor.config.peer_concurrency,
        report.supervisor.config.body_fetch_budget_bytes,
        report.supervisor.config.stale_read_window_ms,
        if report.contacted_peers { "yes" } else { "no" },
        report.export_command,
        report.import_command,
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

    #[test]
    fn auto_enrollment_peer_revocations_disable_matching_enabled_rows() {
        let snapshot = mesh_snapshot_with_peers(vec![
            mesh_peer_row(
                "peer_alpha",
                "nodekey:alpha",
                true,
                Some("{\"schema\":\"other\"}"),
            ),
            mesh_peer_row("peer_bravo", "nodekey:bravo", true, None),
            mesh_peer_row("peer_charlie", "nodekey:charlie", false, None),
        ]);

        let revocations = auto_enrollment_peer_revocations(
            "wsp_test_workspace",
            &snapshot,
            &[
                "nodekey:alpha".to_owned(),
                "nodekey:charlie".to_owned(),
                "nodekey:missing".to_owned(),
            ],
            "2026-05-20T00:00:00Z",
        )
        .unwrap();

        assert_eq!(revocations.len(), 1);
        assert_eq!(revocations[0].peer_id, "peer_alpha");
        assert_eq!(revocations[0].origin_node_id, "nodekey:alpha");
        assert_eq!(
            revocations[0].policy_summary_json,
            Some("{\"schema\":\"other\"}".to_owned())
        );
        assert!(!revocations[0].enabled);
    }

    #[test]
    fn mesh_import_counts_only_effective_replay_changes() {
        let connection = DbConnection::open_memory().expect("open memory db");
        connection.migrate().expect("migrate db");
        let workspace_id = "wsp_meshreplay0000000000000001";
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-mesh-replay-counts".to_string(),
                    name: Some("mesh replay counts".to_string()),
                },
            )
            .expect("insert workspace");

        let artifact = mesh_export_artifact_for_import_counts();
        let first = import_mesh_artifact_into_connection(&connection, workspace_id, &artifact)
            .expect("first import");
        assert_eq!(first, (1, 1, 1));

        let duplicate = import_mesh_artifact_into_connection(&connection, workspace_id, &artifact)
            .expect("duplicate import");
        assert_eq!(
            duplicate,
            (0, 0, 0),
            "duplicate replay should be idempotent and report no effective changes"
        );

        let events = connection
            .list_mesh_import_ledger_events_for_workspace(workspace_id)
            .expect("list imported events");
        assert_eq!(events.len(), 1);

        let index_jobs = connection
            .list_search_index_jobs(workspace_id, None)
            .expect("list index jobs");
        assert_eq!(
            index_jobs.len(),
            1,
            "duplicate replay should not enqueue duplicate import index jobs"
        );
        assert_eq!(index_jobs[0].document_source.as_deref(), Some("import"));
        assert_eq!(
            index_jobs[0].document_id.as_deref(),
            Some(artifact.events[0].event_id.as_str())
        );
    }

    fn mesh_export_artifact_for_import_counts() -> MeshExportArtifact {
        MeshExportArtifact {
            schema: MESH_EXPORT_ARTIFACT_SCHEMA_V1.to_string(),
            workspace_id: "wsp_remote00000000000000000001".to_string(),
            source: "ee mesh export".to_string(),
            policy_attestation: None,
            storage: MeshStorageCounts {
                peer_count: 1,
                cursor_count: 1,
                imported_event_count: 1,
                policy_decision_event_count: 0,
                policy_failure_event_count: 0,
                mapped_memory_count: 0,
                cached_body_count: 0,
            },
            peers: vec![crate::mesh::foreground_cli::MeshPeerRow {
                peer_id: "peer_mesh_replay_counts".to_string(),
                origin_node_id: "node_mesh_replay_counts".to_string(),
                display_name: Some("replay-counts".to_string()),
                enabled: true,
                last_seen_at: "2026-05-21T19:50:00Z".to_string(),
                policy_summary_json: Some(r#"{"schema":"ee.mesh.peer_policy.v1"}"#.to_string()),
            }],
            cursors: vec![crate::mesh::foreground_cli::MeshCursorRow {
                peer_id: "peer_mesh_replay_counts".to_string(),
                origin_node_id: "node_mesh_replay_counts".to_string(),
                origin_workspace_id: "wsp_remote00000000000000000001".to_string(),
                last_seq: 1,
                tip_event_hash: Some(hash_for_test('a')),
                tip_audit_hash: Some(hash_for_test('b')),
                status: "current".to_string(),
                updated_at: "2026-05-21T19:50:01Z".to_string(),
            }],
            events: vec![crate::mesh::foreground_cli::MeshEventRow {
                event_id: "mesh_evt_replay_counts_0000000000000001".to_string(),
                origin_node_id: "node_mesh_replay_counts".to_string(),
                origin_workspace_id: "wsp_remote00000000000000000001".to_string(),
                producer_peer_id: Some("peer_mesh_replay_counts".to_string()),
                seq: 1,
                prev_event_hash: None,
                event_hash: hash_for_test('c'),
                event_kind: "create".to_string(),
                logical_memory_id: "mesh_mem_replay_counts".to_string(),
                content_hash: hash_for_test('d'),
                material_lane: "metadata".to_string(),
                redaction_class: "share".to_string(),
                trust_lane: "peerAgent".to_string(),
                import_decision: "accepted".to_string(),
                local_memory_id: None,
                body_cache_key: None,
                policy_failure_surface_json: None,
                policy_decision_json: None,
                event_json: r#"{"schema":"ee.mesh.event.v1","eventKind":"create"}"#.to_string(),
                policy_attestation: None,
                imported_at: "2026-05-21T19:50:02Z".to_string(),
            }],
        }
    }

    fn hash_for_test(character: char) -> String {
        format!("blake3:{}", character.to_string().repeat(64))
    }

    fn mesh_snapshot_with_peers(
        peers: Vec<crate::mesh::foreground_cli::MeshPeerRow>,
    ) -> MeshForegroundSnapshot {
        MeshForegroundSnapshot {
            workspace_id: "wsp_test_workspace".to_owned(),
            workspace_path: "/tmp/workspace".to_owned(),
            database_path: "/tmp/workspace/.ee/ee.db".to_owned(),
            initialized: true,
            mesh_enabled: true,
            mode: "cache".to_owned(),
            storage: MeshStorageCounts::default(),
            peers,
            cursors: Vec::new(),
            events: Vec::new(),
            degraded: Vec::new(),
        }
    }

    fn mesh_peer_row(
        peer_id: &str,
        origin_node_id: &str,
        enabled: bool,
        policy_summary_json: Option<&str>,
    ) -> crate::mesh::foreground_cli::MeshPeerRow {
        crate::mesh::foreground_cli::MeshPeerRow {
            peer_id: peer_id.to_owned(),
            origin_node_id: origin_node_id.to_owned(),
            display_name: Some(peer_id.to_owned()),
            enabled,
            last_seen_at: "2026-05-20T00:00:00Z".to_owned(),
            policy_summary_json: policy_summary_json.map(str::to_owned),
        }
    }
}
