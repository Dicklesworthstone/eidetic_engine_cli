use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
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
    SearchIndexJobType, StoredMeshPeerCursor, UpsertMeshPeerCursorInput, UpsertMeshPeerInput,
    audit_actions, generate_audit_id,
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
use crate::mesh::emergency_disable::{
    MeshEmergencyDisableInput, MeshEmergencyDisableReport, MeshEmergencyReenableInput,
    MeshEmergencyReenableReport, apply_emergency_disable, apply_emergency_reenable,
    plan_emergency_disable, plan_emergency_reenable,
};
use crate::mesh::foreground_cli::{
    MESH_CLI_EXPORT_SCHEMA_V1, MESH_CLI_IMPORT_SCHEMA_V1, MESH_CLI_SYNC_SCHEMA_V1,
    MESH_EXPORT_ARTIFACT_SCHEMA_V1, MESH_SYNC_ONCE_NETWORK_DEFERRED_CODE, MeshCliDegradation,
    MeshCliExportReport, MeshCliImportReport, MeshCliPeersReport, MeshCliStatusReport,
    MeshCliSyncReport, MeshExportArtifact, MeshForegroundSnapshot, MeshStorageCounts,
    MeshSyncSupervisorOptions, MeshSyncSupervisorReport, apply_outbound_export_policy,
    foreground_degradations, run_mesh_sync_supervisor_supervised,
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
use crate::policy::{
    MESH_SECRET_EXPORT_DENIED_CODE, MeshExportSecretScanReport, OsSecretFindingRandom,
    decorate_export_secret_findings,
};

use super::{Cli, write_domain_error, write_stdout};

const MESH_CLI_INIT_SCHEMA_V1: &str = "ee.mesh.cli.init.v1";
const MESH_GRANT_SCHEMA_V1: &str = "ee.mesh.grant.v1";
const MESH_REVOKE_LANE_SCHEMA_V1: &str = "ee.mesh.revoke_lane.v1";
const MESH_GRANT_COMMAND: &str = "ee mesh grant";
const MESH_REVOKE_LANE_COMMAND: &str = "ee mesh revoke-lane";
const MESH_GRANT_RESIDUAL: &str = "A later lane revocation stops future serving but cannot erase bytes the peer already cached or copied.";
const MESH_REVOKE_LANE_RESIDUAL: &str =
    "Revocation stops future serving but cannot erase bytes the peer already cached or copied.";
const APPROVAL_TOKEN_EXTERNAL_RECORDER_RESIDUAL: &str = "An opted-in approval bearer may be captured by a third-party stdout or session recorder until it expires.";
const DISCOVERY_POLICY_CONFIG_FILE: &str = "discovery_policy.toml";
const AUTO_ENROLL_OVERRIDES_FILE: &str = "auto_enroll_overrides.toml";

/// Cap on the byte size of operator-supplied mesh discovery-policy and
/// auto-enroll-override TOML config files before refusing to read them.
///
/// These config files carry a handful of policy modes plus a bounded set
/// of node-key allow/deny lists; in operational use they sit in the
/// hundreds-of-bytes-to-low-kilobytes range. 1 MiB is several orders of
/// magnitude above any realistic config while bounding the allocation an
/// accidentally-aimed-at-a-log-file or adversarial path can demand.
/// bd-3l1cy / bd-1icct multi-pass-bug-hunting follow-ups (mirrors the
/// 8 MiB AGENT_MAIL_SNAPSHOT_MAX_BYTES posture in bd-1sdr5).
const MESH_CONFIG_MAX_BYTES: usize = 1024 * 1024;

/// Cap on the byte size of an operator-supplied mesh import artifact
/// (`ee mesh import --file <path>`) before refusing to read it.
///
/// Mesh export artifacts carry peers, cursors, and bounded per-event JSON
/// rows for a single workspace. 8 MiB matches the swarm-brief
/// `AGENT_MAIL_SNAPSHOT_MAX_BYTES` precedent (bd-1sdr5) and is sized for
/// operational workspace exports while bounding adversarial allocation;
/// operators with larger payloads can chunk imports. bd-3l1cy /
/// bd-1icct multi-pass-bug-hunting follow-up.
const MESH_IMPORT_ARTIFACT_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Human lane-grant confirmation accepts only `y` or `yes`; cap the entire
/// line so a piped stream without a newline cannot grow memory until EOF.
const LANE_GRANT_CONFIRMATION_MAX_BYTES: usize = 16;

/// Read `path` into a string while refusing payloads above `max_bytes`.
///
/// Opens the file and consumes at most `max_bytes + 1` bytes via
/// `Read::take`; if the read fills the +1 sentinel slot, the file is
/// over-cap and we return `io::ErrorKind::InvalidData` naming the cap
/// and the offending path. Mirrors the bounded-read pattern in
/// `src/core/swarm_brief.rs::read_agent_mail_snapshot_file` (bd-1sdr5,
/// commit bc642162). `kind_label` flows into the error message so the
/// caller's `DomainError::Storage` adapter can surface a precise
/// diagnostic. bd-3l1cy.
fn read_mesh_text_bounded(
    path: &Path,
    max_bytes: usize,
    kind_label: &str,
) -> std::io::Result<String> {
    let read_limit = max_bytes.checked_add(1).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Mesh config read cap overflowed usize",
        )
    })?;
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(read_limit as u64).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Mesh {kind_label} '{}' exceeds the {max_bytes}-byte cap; refusing to read",
                path.display()
            ),
        ));
    }
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

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
    /// Immediately contain mesh activity without deleting local truth.
    Disable(MeshDisableArgs),
    /// Re-enable mesh after explicit containment review.
    Reenable(MeshReenableArgs),
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
    /// Grant one lane after an authenticated, revision-pinned preview.
    Grant(MeshGrantArgs),
    /// Revoke one lane and invalidate every preview from the prior generation.
    RevokeLane(MeshRevokeLaneArgs),
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
    /// Opaque enrolled peer ID the preview targets.
    #[arg(value_name = "PEER_ID")]
    pub peer_id: String,

    /// Lane to preview granting on this peer.
    #[arg(long, value_name = "LANE")]
    pub lane: MeshPreviewGrantLane,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,

    /// Maximum preview rows. Internally clamped to LANE_GRANT_PREVIEW_MAX_LIMIT.
    #[arg(long, default_value_t = 25)]
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

    /// Issue a marked-sensitive 15-minute approval bearer in JSON mode.
    #[arg(long = "issue-approval-token", action = ArgAction::SetTrue)]
    pub issue_approval_token: bool,
}

/// Arguments for `ee mesh grant`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshGrantArgs {
    /// Opaque enrolled peer ID whose effective policy will be widened.
    #[arg(value_name = "PEER_ID")]
    pub peer_id: String,

    /// Lane to allow for this exact peer.
    #[arg(long, value_name = "LANE")]
    pub lane: MeshPreviewGrantLane,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,

    /// Read the approval bearer from bounded standard input (JSON mode only).
    #[arg(long = "preview-token-stdin", action = ArgAction::SetTrue)]
    pub preview_token_stdin: bool,
}

/// Arguments for `ee mesh revoke-lane`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshRevokeLaneArgs {
    /// Opaque enrolled peer ID whose effective policy will be narrowed.
    #[arg(value_name = "PEER_ID")]
    pub peer_id: String,

    /// Lane to deny for this exact peer.
    #[arg(long, value_name = "LANE")]
    pub lane: MeshPreviewGrantLane,

    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MeshLaneGrantPreviewReport {
    command: &'static str,
    workspace_id: String,
    preview: crate::mesh::lane_grant_preview::LaneGrantPreview,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MeshLaneMutationReport {
    schema: &'static str,
    command: &'static str,
    workspace_id: String,
    target: crate::mesh::lane_grant_preview::GrantTargetSnapshot,
    lane: String,
    previous_grant_generation: u64,
    new_grant_generation: u64,
    decision: &'static str,
    audit_id: String,
    remote_erasure_guaranteed: bool,
    residual: &'static str,
}

#[derive(Debug)]
struct PreparedLaneGrantPreview {
    preview: crate::mesh::lane_grant_preview::LaneGrantPreview,
    target_adapter: crate::db::MeshLaneGrantTargetAdapter,
    current_state: Option<crate::db::StoredMeshLaneGrantState>,
}

#[derive(Debug)]
enum LaneGrantEffectError {
    Approval(crate::mesh::lane_grant::ApprovalTokenError),
    Domain(DomainError),
}

impl std::fmt::Display for LaneGrantEffectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Approval(error) => write!(formatter, "{error}"),
            Self::Domain(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for LaneGrantEffectError {}

/// Arguments for `ee mesh disable`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshDisableArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,

    /// Preview containment without writing workspace config.
    #[arg(long, action = ArgAction::SetTrue)]
    pub dry_run: bool,

    /// Record all-workspaces containment intent without mutating every workspace config.
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "peer")]
    pub all_workspaces: bool,

    /// Narrow containment to one mesh peer.
    #[arg(long, value_name = "PEER_ID")]
    pub peer: Option<String>,

    /// Optional temporary containment duration for operator/audit context.
    #[arg(long = "temporary-for", value_name = "DURATION")]
    pub temporary_for: Option<String>,

    /// Operator-visible incident reason.
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,
}

/// Arguments for `ee mesh reenable`.
#[derive(Clone, Debug, Eq, Parser, PartialEq)]
pub struct MeshReenableArgs {
    /// Database path. Defaults to <workspace>/.ee/ee.db.
    #[arg(long, value_name = "PATH")]
    pub database: Option<PathBuf>,

    /// Preview re-enable actions without writing workspace config.
    #[arg(long, action = ArgAction::SetTrue)]
    pub dry_run: bool,

    /// Required confirmation that containment review is complete.
    #[arg(long = "confirm-reenable", action = ArgAction::SetTrue)]
    pub confirm_reenable: bool,
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

    /// Target peer id whose outbound policy (`[[mesh.peer_policies]]`) governs
    /// what may be exported. When set, each event is filtered per record and
    /// lane: records the peer may not receive are dropped, and bodies are
    /// stripped when the body lane is denied. Omit for an unfiltered self-export.
    #[arg(long = "peer", value_name = "PEER_ID")]
    pub peer: Option<String>,
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
        MeshCommand::Disable(args) => handle_mesh_disable(cli, args, stdout, stderr),
        MeshCommand::Reenable(args) => handle_mesh_reenable(cli, args, stdout, stderr),
        MeshCommand::AutoEnroll(args) => handle_mesh_auto_enroll(cli, args, stdout, stderr),
        MeshCommand::DiscoveryPolicy(args) => {
            handle_mesh_discovery_policy(cli, args, stdout, stderr)
        }
        MeshCommand::HelloResponder(args) => handle_mesh_hello_responder(cli, args, stdout, stderr),
        MeshCommand::Export(args) => handle_mesh_export(cli, args, stdout, stderr),
        MeshCommand::Import(args) => handle_mesh_import(cli, args, stdout, stderr),
        MeshCommand::Sync(args) => handle_mesh_sync(cli, args, stdout, stderr),
        MeshCommand::PreviewGrant(args) => handle_mesh_preview_grant(cli, args, stdout, stderr),
        MeshCommand::Grant(args) => handle_mesh_grant(cli, args, stdout, stderr),
        MeshCommand::RevokeLane(args) => handle_mesh_revoke_lane(cli, args, stdout, stderr),
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
    use crate::mesh::lane_grant_preview::{
        ApprovalTokenProjection, LANE_GRANT_PREVIEW_DEFAULT_LIMIT, LANE_GRANT_PREVIEW_MAX_LIMIT,
        SampleStrategy,
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
    if args.issue_approval_token && !cli.wants_json() {
        let domain_error = DomainError::Usage {
            message: "--issue-approval-token is available only with --json".to_owned(),
            repair: Some(
                "Use ordinary human preview output without a bearer, or add --json for an explicit robot bearer projection."
                    .to_owned(),
            ),
        };
        return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
    }

    let sample_strategy = match args.sample_strategy {
        MeshPreviewGrantSampleStrategy::Random => SampleStrategy::Random,
        MeshPreviewGrantSampleStrategy::HighestTrust => SampleStrategy::HighestTrust,
        MeshPreviewGrantSampleStrategy::MostRecent => SampleStrategy::MostRecent,
    };
    if args.issue_approval_token
        && (args.limit != LANE_GRANT_PREVIEW_DEFAULT_LIMIT
            || sample_strategy != SampleStrategy::Random
            || args.seed != 0)
    {
        let domain_error = DomainError::Usage {
            message: "Approval-token issuance requires the canonical preview sample parameters"
                .to_owned(),
            repair: Some(format!(
                "Re-run with --limit {LANE_GRANT_PREVIEW_DEFAULT_LIMIT} --sample-strategy random --seed 0 --issue-approval-token --json."
            )),
        };
        return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
    }

    let snapshot = match build_snapshot(cli, args.database.as_deref()) {
        Ok(snapshot) => snapshot,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    if !snapshot.initialized {
        let error = lane_grant_uninitialized_error(&snapshot);
        return write_domain_error(&error, cli.wants_json(), stdout, stderr);
    }
    let database_path = Path::new(&snapshot.database_path);
    let connection = match open_mesh_connection(database_path) {
        Ok(connection) => connection,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let (config, config_bytes) = match load_mesh_config_for_approval(cli, args.database.as_deref())
    {
        Ok(config) => config,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let lane = preview_lane(args.lane);
    let mut prepared = match prepare_lane_grant_preview(
        &connection,
        &snapshot.workspace_id,
        &args.peer_id,
        lane,
        sample_strategy,
        args.limit,
        args.seed,
        &config,
        &config_bytes,
    ) {
        Ok(prepared) => prepared,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };

    if args.issue_approval_token {
        let canonical_snapshot = match prepared.preview.canonical_approval_snapshot_bytes() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let domain_error = lane_grant_serialization_error(error);
                return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
            }
        };
        let root = match open_lane_grant_auth_root(&snapshot.workspace_path) {
            Ok(root) => root,
            Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
        };
        let issued = match crate::mesh::lane_grant::issue(
            &root,
            &snapshot.workspace_id,
            MESH_GRANT_SCHEMA_V1,
            &canonical_snapshot,
            chrono::Utc::now().timestamp(),
        ) {
            Ok(issued) => issued,
            Err(error) => {
                let domain_error = approval_token_domain_error(&error, &args.peer_id, lane);
                return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
            }
        };
        let Some(expires_at) =
            chrono::DateTime::<chrono::Utc>::from_timestamp(issued.expires_at_unix_seconds(), 0)
        else {
            let domain_error = DomainError::Storage {
                message: "Issued mesh approval token had an invalid expiry timestamp".to_owned(),
                repair: Some("Run `ee doctor --json` and retry the preview.".to_owned()),
            };
            return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
        };
        prepared.preview.approval_token = Some(ApprovalTokenProjection {
            schema: crate::mesh::lane_grant::APPROVAL_TOKEN_SCHEMA_V1.to_owned(),
            sensitive: true,
            bearer: issued.token().expose_bearer(),
            expires_at: expires_at.to_rfc3339(),
            external_recorder_residual: APPROVAL_TOKEN_EXTERNAL_RECORDER_RESIDUAL.to_owned(),
        });
    }

    let report = MeshLaneGrantPreviewReport {
        command: "mesh preview-grant",
        workspace_id: snapshot.workspace_id,
        preview: prepared.preview,
    };
    let human = render_lane_grant_preview_human(&report.preview);
    let degraded = lane_grant_preview_degraded(&report.preview);
    write_mesh_report_with_degraded(cli, &report, &human, &degraded, stdout)
}

fn handle_mesh_grant<W, E>(
    cli: &Cli,
    args: &MeshGrantArgs,
    stdout: &mut W,
    stderr: &mut E,
) -> ProcessExitCode
where
    W: Write,
    E: Write,
{
    use crate::mesh::lane_grant::{issue, read_bounded_token, verify_authentic_token};
    use crate::mesh::lane_grant_preview::{LANE_GRANT_PREVIEW_DEFAULT_LIMIT, SampleStrategy};

    if cli.wants_json() != args.preview_token_stdin {
        let error = DomainError::Usage {
            message: if cli.wants_json() {
                "JSON grant requires --preview-token-stdin".to_owned()
            } else {
                "--preview-token-stdin is available only with --json".to_owned()
            },
            repair: Some(if cli.wants_json() {
                "Pipe only the bearer from an explicit preview into `ee mesh grant <peer> --lane <lane> --preview-token-stdin --json`."
                    .to_owned()
            } else {
                "Run the human command without --preview-token-stdin and confirm its inline canonical preview."
                    .to_owned()
            }),
        };
        return write_domain_error(&error, cli.wants_json(), stdout, stderr);
    }

    let snapshot = match build_snapshot(cli, args.database.as_deref()) {
        Ok(snapshot) => snapshot,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    if !snapshot.initialized {
        let error = lane_grant_uninitialized_error(&snapshot);
        return write_domain_error(&error, cli.wants_json(), stdout, stderr);
    }
    let connection = match open_mesh_connection(Path::new(&snapshot.database_path)) {
        Ok(connection) => connection,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let root = match open_lane_grant_auth_root(&snapshot.workspace_path) {
        Ok(root) => root,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let lane = preview_lane(args.lane);

    let (authenticated, target_adapter, current_state) = if cli.wants_json() {
        let token = match read_bounded_token(&mut std::io::stdin().lock()) {
            Ok(token) => token,
            Err(error) => {
                let domain_error = approval_token_domain_error(&error, &args.peer_id, lane);
                return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
            }
        };
        let authenticated = match verify_authentic_token(
            &root,
            &snapshot.workspace_id,
            MESH_GRANT_SCHEMA_V1,
            &token,
            chrono::Utc::now().timestamp(),
        ) {
            Ok(authenticated) => authenticated,
            Err(error) => {
                let domain_error = approval_token_domain_error(&error, &args.peer_id, lane);
                return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
            }
        };
        let (target, state) = match load_lane_grant_target_state(
            &connection,
            &snapshot.workspace_id,
            &args.peer_id,
        ) {
            Ok(target) => target,
            Err(LaneGrantTargetStateError::Missing | LaneGrantTargetStateError::Disabled) => {
                let error = approval_token_domain_error(
                    &crate::mesh::lane_grant::ApprovalTokenError::Stale,
                    &args.peer_id,
                    lane,
                );
                return write_domain_error(&error, cli.wants_json(), stdout, stderr);
            }
            Err(LaneGrantTargetStateError::Domain(error)) => {
                return write_domain_error(&error, cli.wants_json(), stdout, stderr);
            }
        };
        (authenticated, target, state)
    } else {
        let (config, config_bytes) =
            match load_mesh_config_for_approval(cli, args.database.as_deref()) {
                Ok(config) => config,
                Err(error) => {
                    return write_domain_error(&error, cli.wants_json(), stdout, stderr);
                }
            };
        let prepared = match prepare_lane_grant_preview(
            &connection,
            &snapshot.workspace_id,
            &args.peer_id,
            lane,
            SampleStrategy::Random,
            LANE_GRANT_PREVIEW_DEFAULT_LIMIT,
            0,
            &config,
            &config_bytes,
        ) {
            Ok(prepared) => prepared,
            Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
        };
        let human = render_lane_grant_preview_human(&prepared.preview);
        let write_exit = write_stdout(stdout, &human);
        if write_exit != ProcessExitCode::Success {
            return write_exit;
        }
        let _ = write!(
            stderr,
            "Grant lane '{}' to peer {}? [y/N] ",
            lane.as_str(),
            args.peer_id
        );
        let _ = stderr.flush();
        let confirmed = match read_lane_grant_confirmation(std::io::stdin().lock()) {
            Ok(confirmed) => confirmed,
            Err(error) => {
                let error = DomainError::Usage {
                    message: format!(
                        "Failed to read bounded lane-grant confirmation from stdin: {error}"
                    ),
                    repair: Some("Retry interactively and enter only `y` or `yes`.".to_owned()),
                };
                return write_domain_error(&error, cli.wants_json(), stdout, stderr);
            }
        };
        if !confirmed {
            return write_stdout(
                stdout,
                "Lane grant cancelled; no state or audit row was written.\n",
            );
        }
        let canonical_snapshot = match prepared.preview.canonical_approval_snapshot_bytes() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let domain_error = lane_grant_serialization_error(error);
                return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
            }
        };
        let issuance_now = chrono::Utc::now().timestamp();
        let issued = match issue(
            &root,
            &snapshot.workspace_id,
            MESH_GRANT_SCHEMA_V1,
            &canonical_snapshot,
            issuance_now,
        ) {
            Ok(issued) => issued,
            Err(error) => {
                let domain_error = approval_token_domain_error(&error, &args.peer_id, lane);
                return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
            }
        };
        let authenticated = match verify_authentic_token(
            &root,
            &snapshot.workspace_id,
            MESH_GRANT_SCHEMA_V1,
            issued.token(),
            issuance_now,
        ) {
            Ok(authenticated) => authenticated,
            Err(error) => {
                let domain_error = approval_token_domain_error(&error, &args.peer_id, lane);
                return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
            }
        };
        (
            authenticated,
            prepared.target_adapter,
            prepared.current_state,
        )
    };

    // Capture the digest proposed for durable storage immediately before the
    // writer transaction. The verification closure rereads and proves these
    // are the same exact bytes; the effect closure rereads once more before
    // audit/commit. Runtime policy use remains the final fail-closed fence for
    // a filesystem replacement after that last read.
    let (_, mutation_config_bytes) =
        match load_mesh_config_for_approval(cli, args.database.as_deref()) {
            Ok(config) => config,
            Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
        };
    let approval_config_digest =
        crate::mesh::lane_grant::approval_config_digest(&mutation_config_bytes);
    let previous_generation = current_state
        .as_ref()
        .map_or(0, |state| state.grant_generation);
    let mutation = crate::db::MeshLaneGrantMutationInput {
        workspace_id: snapshot.workspace_id.clone(),
        peer_id: args.peer_id.clone(),
        target_adapter: target_adapter.clone(),
        material_lane: config_mesh_lane(lane),
        expected_generation: previous_generation,
        approval_config_digest: Some(approval_config_digest.clone()),
        updated_at: Some(chrono::Utc::now().to_rfc3339()),
    };
    // Hold the store-auth root's shared lock from the final snapshot check
    // through the grant, audit, and database commit. A concurrent rotation
    // therefore occurs wholly before this phase (and makes the authenticated
    // bearer invalid) or wholly after the atomic mutation.
    let locked_root = match open_lane_grant_auth_guard(&snapshot.workspace_path) {
        Ok(root) => root,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let transaction = connection.apply_mesh_lane_grant_transaction(
        &mutation,
        || {
            let (config, config_bytes) =
                load_mesh_config_for_approval(cli, args.database.as_deref())
                    .map_err(LaneGrantEffectError::Domain)?;
            if crate::mesh::lane_grant::approval_config_digest(&config_bytes)
                != approval_config_digest
            {
                return Err(LaneGrantEffectError::Approval(
                    crate::mesh::lane_grant::ApprovalTokenError::Stale,
                ));
            }
            let (transaction_target, transaction_state) =
                load_lane_grant_target_state(&connection, &snapshot.workspace_id, &args.peer_id)
                    .map_err(|error| match error {
                        LaneGrantTargetStateError::Missing
                        | LaneGrantTargetStateError::Disabled => LaneGrantEffectError::Approval(
                            crate::mesh::lane_grant::ApprovalTokenError::Stale,
                        ),
                        LaneGrantTargetStateError::Domain(error) => {
                            LaneGrantEffectError::Domain(error)
                        }
                    })?;
            let prepared = prepare_lane_grant_preview_for_state(
                &connection,
                &snapshot.workspace_id,
                &args.peer_id,
                lane,
                SampleStrategy::Random,
                LANE_GRANT_PREVIEW_DEFAULT_LIMIT,
                0,
                &config,
                &config_bytes,
                transaction_target,
                transaction_state,
            )
            .map_err(LaneGrantEffectError::Domain)?;
            let canonical_snapshot = prepared
                .preview
                .canonical_approval_snapshot_bytes()
                .map_err(lane_grant_serialization_error)
                .map_err(LaneGrantEffectError::Domain)?;
            crate::mesh::lane_grant::compare_snapshot(
                &locked_root,
                &authenticated,
                &canonical_snapshot,
                chrono::Utc::now().timestamp(),
            )
            .map_err(LaneGrantEffectError::Approval)
        },
        |next_state, verified| {
            let (_, final_config_bytes) =
                load_mesh_config_for_approval(cli, args.database.as_deref())
                    .map_err(LaneGrantEffectError::Domain)?;
            if crate::mesh::lane_grant::approval_config_digest(&final_config_bytes)
                != approval_config_digest
            {
                return Err(LaneGrantEffectError::Approval(
                    crate::mesh::lane_grant::ApprovalTokenError::Stale,
                ));
            }
            let approval_audit_id = verified.audit_id().to_opaque_string();
            append_lane_mutation_audit(
                &connection,
                &snapshot.workspace_id,
                &args.peer_id,
                lane,
                previous_generation,
                next_state.grant_generation,
                "allow",
                MeshAuditEventKind::LaneGrant,
                Some(&approval_audit_id),
                Some(&approval_config_digest),
            )
            .map_err(LaneGrantEffectError::Domain)?;
            Ok(approval_audit_id)
        },
    );

    let (next_state, _, audit_id) = match transaction {
        Ok(result) => result,
        Err(crate::db::MeshLaneGrantAtomicError::Mutation(error)) => {
            let domain_error = grant_mutation_domain_error(error, &args.peer_id, lane);
            return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
        }
        Err(crate::db::MeshLaneGrantAtomicError::Verification(LaneGrantEffectError::Approval(
            error,
        ))) => {
            let domain_error = approval_token_domain_error(&error, &args.peer_id, lane);
            return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
        }
        Err(crate::db::MeshLaneGrantAtomicError::Verification(LaneGrantEffectError::Domain(
            error,
        ))) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
        Err(crate::db::MeshLaneGrantAtomicError::Effect(LaneGrantEffectError::Approval(error))) => {
            let domain_error = approval_token_domain_error(&error, &args.peer_id, lane);
            return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
        }
        Err(crate::db::MeshLaneGrantAtomicError::Effect(LaneGrantEffectError::Domain(error))) => {
            return write_domain_error(&error, cli.wants_json(), stdout, stderr);
        }
    };

    let report = MeshLaneMutationReport {
        schema: MESH_GRANT_SCHEMA_V1,
        command: MESH_GRANT_COMMAND,
        workspace_id: snapshot.workspace_id,
        target: public_lane_grant_target(&args.peer_id),
        lane: lane.as_str().to_owned(),
        previous_grant_generation: previous_generation,
        new_grant_generation: next_state.grant_generation,
        decision: "allow",
        audit_id,
        remote_erasure_guaranteed: false,
        residual: MESH_GRANT_RESIDUAL,
    };
    let human = render_lane_mutation_human(&report);
    write_mesh_report(cli, &report, &human, stdout)
}

fn handle_mesh_revoke_lane<W, E>(
    cli: &Cli,
    args: &MeshRevokeLaneArgs,
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
    if !snapshot.initialized {
        let error = lane_grant_uninitialized_error(&snapshot);
        return write_domain_error(&error, cli.wants_json(), stdout, stderr);
    }
    let connection = match open_mesh_connection(Path::new(&snapshot.database_path)) {
        Ok(connection) => connection,
        Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
    };
    let lane = preview_lane(args.lane);
    let (target_adapter, state) =
        match load_lane_grant_target_state(&connection, &snapshot.workspace_id, &args.peer_id) {
            Ok(target) => target,
            Err(error) => {
                let error = error.into_domain(&args.peer_id);
                return write_domain_error(&error, cli.wants_json(), stdout, stderr);
            }
        };
    let previous_generation = state.as_ref().map_or(0, |state| state.grant_generation);
    let mutation = crate::db::MeshLaneGrantMutationInput {
        workspace_id: snapshot.workspace_id.clone(),
        peer_id: args.peer_id.clone(),
        target_adapter,
        material_lane: config_mesh_lane(lane),
        expected_generation: previous_generation,
        approval_config_digest: None,
        updated_at: Some(chrono::Utc::now().to_rfc3339()),
    };
    let transaction = connection.revoke_mesh_lane_transaction(
        &mutation,
        || Ok::<(), std::convert::Infallible>(()),
        |next_state, _| {
            append_lane_mutation_audit(
                &connection,
                &snapshot.workspace_id,
                &args.peer_id,
                lane,
                previous_generation,
                next_state.grant_generation,
                "deny",
                MeshAuditEventKind::LaneRevoke,
                None,
                None,
            )
        },
    );
    let (next_state, (), audit_id) = match transaction {
        Ok(result) => result,
        Err(crate::db::MeshLaneGrantAtomicError::Mutation(error)) => {
            let domain_error = revoke_mutation_domain_error(error, &args.peer_id, lane);
            return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
        }
        Err(crate::db::MeshLaneGrantAtomicError::Verification(never)) => match never {},
        Err(crate::db::MeshLaneGrantAtomicError::Effect(error)) => {
            return write_domain_error(&error, cli.wants_json(), stdout, stderr);
        }
    };
    let report = MeshLaneMutationReport {
        schema: MESH_REVOKE_LANE_SCHEMA_V1,
        command: MESH_REVOKE_LANE_COMMAND,
        workspace_id: snapshot.workspace_id,
        target: public_lane_grant_target(&args.peer_id),
        lane: lane.as_str().to_owned(),
        previous_grant_generation: previous_generation,
        new_grant_generation: next_state.grant_generation,
        decision: "deny",
        audit_id,
        remote_erasure_guaranteed: false,
        residual: MESH_REVOKE_LANE_RESIDUAL,
    };
    let human = render_lane_mutation_human(&report);
    write_mesh_report(cli, &report, &human, stdout)
}

fn lane_grant_preview_proposed_policy(
    mut policy: crate::mesh::auto_enrollment_safety::IntendedLanePolicy,
    lane: crate::mesh::lane_grant_preview::Lane,
) -> crate::mesh::auto_enrollment_safety::IntendedLanePolicy {
    use crate::mesh::auto_enrollment_safety::LaneDecision;
    use crate::mesh::lane_grant_preview::Lane;
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

fn lane_grant_uninitialized_error(snapshot: &MeshForegroundSnapshot) -> DomainError {
    DomainError::Storage {
        message: format!(
            "Cannot manage mesh lane grants because {} does not exist",
            snapshot.database_path
        ),
        repair: Some(format!(
            "Run `ee init --workspace \"{}\" --json` first.",
            snapshot.workspace_path
        )),
    }
}

fn open_lane_grant_auth_root(
    workspace_path: &str,
) -> Result<crate::policy::store_auth::StoreAuthRoot, DomainError> {
    let keys_dir = crate::policy::store_auth::workspace_keys_dir(Path::new(workspace_path));
    crate::policy::store_auth::StoreAuthRoot::open(keys_dir).map_err(|error| {
        approval_token_domain_error(
            &crate::mesh::lane_grant::ApprovalTokenError::StoreAuth(error),
            "peer_unavailable",
            crate::mesh::lane_grant_preview::Lane::Metadata,
        )
    })
}

fn open_lane_grant_auth_guard(
    workspace_path: &str,
) -> Result<crate::policy::store_auth::StoreAuthReadGuard, DomainError> {
    let keys_dir = crate::policy::store_auth::workspace_keys_dir(Path::new(workspace_path));
    crate::policy::store_auth::StoreAuthRoot::open_read_locked(keys_dir).map_err(|error| {
        approval_token_domain_error(
            &crate::mesh::lane_grant::ApprovalTokenError::StoreAuth(error),
            "peer_unavailable",
            crate::mesh::lane_grant_preview::Lane::Metadata,
        )
    })
}

enum LaneGrantTargetStateError {
    Missing,
    Disabled,
    Domain(DomainError),
}

impl LaneGrantTargetStateError {
    fn into_domain(self, peer_id: &str) -> DomainError {
        match self {
            Self::Missing => unknown_mesh_peer_error(peer_id),
            Self::Disabled => DomainError::PolicyDenied {
                message: format!("Mesh peer {peer_id} is disabled; lane policy cannot be widened"),
                repair: Some(
                    "Re-enroll or explicitly re-enable the peer, then obtain a fresh preview."
                        .to_owned(),
                ),
            },
            Self::Domain(error) => error,
        }
    }
}

fn load_lane_grant_target_state(
    connection: &DbConnection,
    workspace_id: &str,
    peer_id: &str,
) -> Result<
    (
        crate::db::MeshLaneGrantTargetAdapter,
        Option<crate::db::StoredMeshLaneGrantState>,
    ),
    LaneGrantTargetStateError,
> {
    let peer = connection
        .get_mesh_peer(workspace_id, peer_id)
        .map_err(|error| {
            LaneGrantTargetStateError::Domain(storage_error(
                "Failed to load lane-grant peer",
                error,
            ))
        })?
        .ok_or(LaneGrantTargetStateError::Missing)?;
    if !peer.enabled {
        return Err(LaneGrantTargetStateError::Disabled);
    }
    let target_adapter =
        crate::db::MeshLaneGrantTargetAdapter::new(peer.peer_id, peer.origin_node_id);
    target_adapter.canonical_json().map_err(|error| {
        LaneGrantTargetStateError::Domain(DomainError::Storage {
            message: format!("Stored mesh peer cannot form a lane-grant target: {error}"),
            repair: Some("Run `ee doctor --json` and repair the peer record.".to_owned()),
        })
    })?;
    let state = connection
        .get_mesh_lane_grant_state(workspace_id, peer_id)
        .map_err(|error| {
            LaneGrantTargetStateError::Domain(storage_error(
                "Failed to load lane-grant state",
                error,
            ))
        })?;
    Ok((target_adapter, state))
}

#[allow(clippy::too_many_arguments)]
fn prepare_lane_grant_preview(
    connection: &DbConnection,
    workspace_id: &str,
    peer_id: &str,
    lane: crate::mesh::lane_grant_preview::Lane,
    sample_strategy: crate::mesh::lane_grant_preview::SampleStrategy,
    limit: usize,
    seed: u64,
    config: &crate::config::ConfigFile,
    config_bytes: &[u8],
) -> Result<PreparedLaneGrantPreview, DomainError> {
    let (target_adapter, current_state) =
        load_lane_grant_target_state(connection, workspace_id, peer_id)
            .map_err(|error| error.into_domain(peer_id))?;
    prepare_lane_grant_preview_for_state(
        connection,
        workspace_id,
        peer_id,
        lane,
        sample_strategy,
        limit,
        seed,
        config,
        config_bytes,
        target_adapter,
        current_state,
    )
}

#[allow(clippy::too_many_arguments)]
fn prepare_lane_grant_preview_for_state(
    connection: &DbConnection,
    workspace_id: &str,
    peer_id: &str,
    lane: crate::mesh::lane_grant_preview::Lane,
    sample_strategy: crate::mesh::lane_grant_preview::SampleStrategy,
    limit: usize,
    seed: u64,
    config: &crate::config::ConfigFile,
    config_bytes: &[u8],
    target_adapter: crate::db::MeshLaneGrantTargetAdapter,
    current_state: Option<crate::db::StoredMeshLaneGrantState>,
) -> Result<PreparedLaneGrantPreview, DomainError> {
    use crate::mesh::lane_grant_preview::{
        LaneGrantApprovalContext, LaneGrantPreviewInput, MemoryView,
        compute_lane_grant_preview_with_context,
    };

    let target_adapter_json =
        target_adapter
            .canonical_json()
            .map_err(|error| DomainError::Storage {
                message: format!("Stored mesh peer cannot form a lane-grant target: {error}"),
                repair: Some("Run `ee doctor --json` and repair the peer record.".to_owned()),
            })?;
    let grant_generation = current_state
        .as_ref()
        .map_or(0, |state| state.grant_generation);
    let current_peer_policy = effective_lane_grant_policy(
        config,
        config_bytes,
        workspace_id,
        peer_id,
        current_state.as_ref(),
    )?;
    let current_policy = intended_lane_policy_from_peer_policy(&current_peer_policy);
    let proposed_policy = lane_grant_preview_proposed_policy(current_policy, lane);
    let proposed_generation =
        grant_generation
            .checked_add(1)
            .ok_or_else(|| DomainError::Storage {
                message: format!(
                    "Mesh lane grant generation {grant_generation} cannot be advanced"
                ),
                repair: Some(
                    "Run `ee doctor --json`; do not bypass the generation guard.".to_owned(),
                ),
            })?;
    let current_policy_generation = lane_grant_policy_generation(
        config_bytes,
        &target_adapter_json,
        grant_generation,
        &current_policy,
    );
    let proposed_policy_generation = lane_grant_policy_generation(
        config_bytes,
        &target_adapter_json,
        proposed_generation,
        &proposed_policy,
    );
    let candidate_revision_generation = connection
        .get_workspace_generation(workspace_id)
        .map_err(|error| {
            storage_error(
                "Failed to load the lane-preview candidate revision generation",
                error,
            )
        })?
        .unwrap_or(0);
    let (memories, redaction_rules) =
        lane_grant_preview_memories(connection, workspace_id, &current_peer_policy, lane)?;
    let memory_views = memories
        .iter()
        .map(LaneGrantPreviewMemory::as_view)
        .collect::<Vec<MemoryView<'_>>>();
    let bindings = config
        .mesh
        .peer_group_bindings
        .as_deref()
        .unwrap_or_default();
    let preview = compute_lane_grant_preview_with_context(
        &LaneGrantPreviewInput {
            peer_node_key: peer_id,
            peer_in_group: peer_is_in_lane_group(workspace_id, peer_id, bindings),
            lane,
            workspace_id,
            current_policy,
            proposed_policy,
            memories: &memory_views,
            sample_strategy,
            limit,
            redaction_rules: &redaction_rules,
            sample_random_seed: seed,
        },
        &LaneGrantApprovalContext {
            target_peer_id: peer_id,
            grant_generation,
            candidate_revision_generation,
            current_policy_generation: &current_policy_generation,
            proposed_policy_generation: &proposed_policy_generation,
        },
    );
    Ok(PreparedLaneGrantPreview {
        preview,
        target_adapter,
        current_state,
    })
}

fn public_lane_grant_target(peer_id: &str) -> crate::mesh::lane_grant_preview::GrantTargetSnapshot {
    crate::mesh::lane_grant_preview::GrantTargetSnapshot {
        adapter_version: crate::mesh::lane_grant_preview::LANE_GRANT_TARGET_ADAPTER_VERSION
            .to_owned(),
        peer_id: peer_id.to_owned(),
    }
}

fn render_lane_grant_preview_human(
    preview: &crate::mesh::lane_grant_preview::LaneGrantPreview,
) -> String {
    let mut output = format!(
        "Mesh lane grant preview\n  peer: {}\n  lane: {}\n  generation: {}\n  current: {}\n  proposed: {}\n  affected memories: {}\n  redacted from exposure: {}\n",
        preview.target.peer_id,
        preview.lane,
        preview.grant_generation,
        preview.current_policy.decision,
        preview.proposed_policy.decision,
        preview.affected_memory_count,
        preview.redacted_from_exposure_count,
    );
    if !preview.preview_sample.is_empty() {
        output.push_str("  sample:\n");
        for row in &preview.preview_sample {
            output.push_str(&format!(
                "    - {} [{} / {}]: {}\n",
                row.memory_id, row.trust_class, row.kind, row.content_preview
            ));
        }
    }
    if !preview.cautions.is_empty() {
        output.push_str("  cautions:\n");
        for caution in &preview.cautions {
            output.push_str(&format!(
                "    - {} ({}): {}\n",
                caution.kind, caution.severity, caution.message
            ));
        }
    }
    output
}

fn render_lane_mutation_human(report: &MeshLaneMutationReport) -> String {
    format!(
        "{}: peer={} lane={} decision={} generation {} -> {}\n  audit: {}\n  remote erasure guaranteed: no\n  {}\n",
        report.command,
        report.target.peer_id,
        report.lane,
        report.decision,
        report.previous_grant_generation,
        report.new_grant_generation,
        report.audit_id,
        report.residual,
    )
}

fn lane_grant_serialization_error(error: serde_json::Error) -> DomainError {
    DomainError::Storage {
        message: format!("Failed to serialize the canonical lane-grant preview: {error}"),
        repair: Some("Retry the preview and report the serialization failure.".to_owned()),
    }
}

fn lane_cli_value(lane: crate::mesh::lane_grant_preview::Lane) -> &'static str {
    use crate::mesh::lane_grant_preview::Lane;
    match lane {
        Lane::Metadata => "metadata",
        Lane::Body => "body",
        Lane::Embedding => "embedding",
        Lane::GraphLink => "graph-link",
        Lane::CurationSignal => "curation-signal",
        Lane::RevisionNotice => "revision-notice",
    }
}

fn read_lane_grant_confirmation<R: std::io::BufRead>(reader: R) -> std::io::Result<bool> {
    let mut limited = reader.take((LANE_GRANT_CONFIRMATION_MAX_BYTES + 1) as u64);
    let mut answer = Vec::with_capacity(LANE_GRANT_CONFIRMATION_MAX_BYTES + 1);
    std::io::BufRead::read_until(&mut limited, b'\n', &mut answer)?;
    if answer.len() > LANE_GRANT_CONFIRMATION_MAX_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("confirmation exceeds {LANE_GRANT_CONFIRMATION_MAX_BYTES}-byte limit"),
        ));
    }
    let answer = std::str::from_utf8(&answer).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("confirmation is not UTF-8: {error}"),
        )
    })?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn approval_token_domain_error(
    error: &crate::mesh::lane_grant::ApprovalTokenError,
    peer_id: &str,
    lane: crate::mesh::lane_grant_preview::Lane,
) -> DomainError {
    use crate::mesh::lane_grant::ApprovalTokenError;
    if let ApprovalTokenError::StoreAuth(store_error) = error {
        return DomainError::UsageCodeWithDetails {
            code: crate::policy::store_auth::MESH_STORE_AUTHENTICATION_UNAVAILABLE_CODE,
            message: store_error.message(),
            repair: Some(store_error.repair()),
            details_json: "{}".to_owned(),
        };
    }
    let peer_id = super::shell_quote_cli_arg(peer_id);
    let command = format!(
        "ee mesh preview-grant {peer_id} --lane {} --issue-approval-token --json",
        lane_cli_value(lane)
    );
    DomainError::UsageCodeWithDetails {
        code: error.code(),
        message: error.message(),
        repair: Some(error.repair()),
        details_json: json!({
            "recovery": [{
                "priority": 0,
                "kind": "command",
                "command": command,
                "rationale": "Rebuild the read-only approval snapshot and explicitly issue a fresh short-lived bearer.",
                "riskClass": "read_only_probe",
                "requiresHumanApproval": false,
                "mutatesExternalState": false,
                "mutatesTrackerState": false,
                "privacyClass": "sensitive_bearer_issuance",
            }],
        })
        .to_string(),
    }
}

fn grant_mutation_domain_error(
    error: crate::db::MeshLaneGrantMutationError,
    peer_id: &str,
    lane: crate::mesh::lane_grant_preview::Lane,
) -> DomainError {
    use crate::db::MeshLaneGrantMutationError;
    match error {
        MeshLaneGrantMutationError::GenerationConflict { .. }
        | MeshLaneGrantMutationError::TargetMismatch { .. }
        | MeshLaneGrantMutationError::PeerNotFound { .. }
        | MeshLaneGrantMutationError::PeerDisabled { .. } => approval_token_domain_error(
            &crate::mesh::lane_grant::ApprovalTokenError::Stale,
            peer_id,
            lane,
        ),
        MeshLaneGrantMutationError::Database(error) => {
            storage_error("Failed to commit mesh lane grant", error)
        }
        MeshLaneGrantMutationError::InvalidTargetAdapter { message } => DomainError::Storage {
            message: format!("Invalid persisted lane-grant target: {message}"),
            repair: Some("Run `ee doctor --json` and repair the peer record.".to_owned()),
        },
        MeshLaneGrantMutationError::InvalidApprovalConfigDigest => DomainError::Storage {
            message: "Mesh lane grant lost its verified config binding before commit".to_owned(),
            repair: Some("Obtain a fresh read-only preview and retry the grant.".to_owned()),
        },
        MeshLaneGrantMutationError::GenerationExhausted { current } => DomainError::Storage {
            message: format!("Mesh lane grant generation {current} cannot be advanced"),
            repair: Some("Run `ee doctor --json`; do not bypass the generation guard.".to_owned()),
        },
    }
}

fn revoke_mutation_domain_error(
    error: crate::db::MeshLaneGrantMutationError,
    peer_id: &str,
    lane: crate::mesh::lane_grant_preview::Lane,
) -> DomainError {
    use crate::db::MeshLaneGrantMutationError;
    match error {
        MeshLaneGrantMutationError::Database(error) => {
            storage_error("Failed to revoke mesh lane", error)
        }
        MeshLaneGrantMutationError::GenerationConflict { expected, actual } => DomainError::Usage {
            message: format!(
                "Mesh lane generation changed while revoking {} for {peer_id}: expected {expected}, actual {actual}",
                lane.as_str()
            ),
            repair: Some(
                "Retry `ee mesh revoke-lane`; it will bind the new generation.".to_owned(),
            ),
        },
        other => DomainError::PolicyDenied {
            message: format!(
                "Cannot revoke mesh lane {} for {peer_id}: {other}",
                lane.as_str()
            ),
            repair: Some("Inspect `ee mesh peer show <peer> --json` and retry.".to_owned()),
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn append_lane_mutation_audit(
    connection: &DbConnection,
    workspace_id: &str,
    peer_id: &str,
    lane: crate::mesh::lane_grant_preview::Lane,
    previous_generation: u64,
    new_generation: u64,
    decision: &str,
    event_kind: MeshAuditEventKind,
    approval_audit_id: Option<&str>,
    approval_config_digest: Option<&str>,
) -> Result<String, DomainError> {
    let mut details = MeshAuditDetails::default();
    details
        .insert_reference("lane", lane.as_str())
        .map_err(lane_mutation_audit_error)?;
    details
        .insert_reference("decision", decision)
        .map_err(lane_mutation_audit_error)?;
    details
        .insert_count("previous_grant_generation", previous_generation)
        .map_err(lane_mutation_audit_error)?;
    details
        .insert_count("new_grant_generation", new_generation)
        .map_err(lane_mutation_audit_error)?;
    details
        .insert_bool("remote_erasure_guaranteed", false)
        .map_err(lane_mutation_audit_error)?;
    if let Some(approval_audit_id) = approval_audit_id {
        details
            .insert_reference("approval_audit_id", approval_audit_id)
            .map_err(lane_mutation_audit_error)?;
    }
    if let Some(approval_config_digest) = approval_config_digest {
        details
            .insert_digest("approval_config_digest", approval_config_digest)
            .map_err(lane_mutation_audit_error)?;
    }
    let event = compute_mesh_audit_event(&MeshAuditEventInput {
        workspace_id: workspace_id.to_owned(),
        event_kind,
        peer_id: Some(peer_id.to_owned()),
        origin_workspace_id: None,
        target_workspace_id: None,
        workspace_scope: Some("exact_peer_lane".to_owned()),
        policy_decision_id: approval_audit_id.map(str::to_owned),
        local_row_refs: Vec::new(),
        cached_body_refs: Vec::new(),
        details,
        previous_event_hash: None,
    })
    .map_err(lane_mutation_audit_error)?;
    append_mesh_audit_event(connection, &event, Some(event_kind.audit_action()))
        .map_err(lane_mutation_audit_error)
}

fn lane_mutation_audit_error(error: MeshAuditLedgerError) -> DomainError {
    DomainError::Storage {
        message: format!("Mesh lane mutation audit failed: {error}"),
        repair: Some(
            "Run `ee audit verify --json`; the lane mutation was rolled back with its audit."
                .to_owned(),
        ),
    }
}

#[derive(Clone, Debug)]
struct LaneGrantPreviewMemory {
    memory_id: String,
    level: String,
    kind: String,
    content: String,
    tags: Vec<String>,
    trust_class: crate::models::TrustClass,
    redacted_fields: Vec<String>,
    created_at_secs: i64,
    is_tombstoned: bool,
    blocked_by_redaction_class: bool,
}

impl LaneGrantPreviewMemory {
    fn as_view(&self) -> crate::mesh::lane_grant_preview::MemoryView<'_> {
        crate::mesh::lane_grant_preview::MemoryView {
            memory_id: &self.memory_id,
            level: &self.level,
            kind: &self.kind,
            content: &self.content,
            tags: &self.tags,
            trust_class: self.trust_class,
            redacted_fields: &self.redacted_fields,
            created_at_secs: self.created_at_secs,
            is_tombstoned: self.is_tombstoned,
            blocked_by_redaction_class: self.blocked_by_redaction_class,
        }
    }
}

fn lane_grant_preview_memories(
    connection: &DbConnection,
    workspace_id: &str,
    policy: &crate::mesh::policy::MeshPeerPolicy,
    lane: crate::mesh::lane_grant_preview::Lane,
) -> Result<(Vec<LaneGrantPreviewMemory>, Vec<String>), DomainError> {
    let memories = connection
        .list_current_memories_including_tombstoned(workspace_id)
        .map_err(|error| storage_error("Failed to list lane-preview memories", error))?;
    let ids = memories
        .iter()
        .map(|memory| memory.id.as_str())
        .collect::<Vec<_>>();
    let tags_by_memory = connection
        .get_memory_tags_batch(&ids)
        .map_err(|error| storage_error("Failed to list lane-preview memory tags", error))?;
    let blocked_by_redaction_class = lane_grant_redaction_denied(policy, lane);
    let mut rules = BTreeSet::new();
    let mut prepared = Vec::with_capacity(memories.len());
    for memory in memories {
        let trust_class = memory
            .trust_class
            .parse()
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Memory {} has an invalid trust class during lane preview: {error}",
                    memory.id
                ),
                repair: Some("Run `ee doctor --json` before granting a mesh lane.".to_owned()),
            })?;
        let redaction = crate::policy::redact_secret_like_content(&memory.content);
        let mut redacted_fields = redaction
            .redacted_reasons
            .iter()
            .map(|reason| {
                rules.insert((*reason).to_owned());
                format!("content:{reason}")
            })
            .collect::<Vec<_>>();
        let mut tags = tags_by_memory
            .get(&memory.id)
            .into_iter()
            .flatten()
            .map(|tag| {
                let report = crate::policy::redact_secret_like_content(tag);
                for reason in &report.redacted_reasons {
                    rules.insert((*reason).to_owned());
                    redacted_fields.push(format!("tag:{reason}"));
                }
                report.content
            })
            .collect::<Vec<_>>();
        tags.sort();
        tags.dedup();
        redacted_fields.sort();
        redacted_fields.dedup();
        let created_at_secs = chrono::DateTime::parse_from_rfc3339(&memory.created_at)
            .map_err(|error| DomainError::Storage {
                message: format!(
                    "Memory {} has an invalid created_at timestamp during lane preview: {error}",
                    memory.id
                ),
                repair: Some("Run `ee doctor --json` before granting a mesh lane.".to_owned()),
            })?
            .timestamp();
        prepared.push(LaneGrantPreviewMemory {
            memory_id: memory.id,
            level: memory.level,
            kind: memory.kind,
            content: redaction.content,
            tags,
            trust_class,
            redacted_fields,
            created_at_secs,
            is_tombstoned: memory.tombstoned_at.is_some(),
            blocked_by_redaction_class,
        });
    }
    Ok((prepared, rules.into_iter().collect()))
}

fn lane_grant_redaction_denied(
    policy: &crate::mesh::policy::MeshPeerPolicy,
    lane: crate::mesh::lane_grant_preview::Lane,
) -> bool {
    use crate::mesh::lane_grant_preview::Lane;
    use crate::mesh::policy::MeshRedactionDecision;
    match lane {
        Lane::Metadata => policy.redaction.metadata == MeshRedactionDecision::Deny,
        Lane::Body => policy.redaction.body == MeshRedactionDecision::Deny,
        Lane::Embedding => policy.redaction.embedding == MeshRedactionDecision::Deny,
        Lane::GraphLink | Lane::CurationSignal | Lane::RevisionNotice => false,
    }
}

fn intended_lane_policy_from_peer_policy(
    policy: &crate::mesh::policy::MeshPeerPolicy,
) -> crate::mesh::auto_enrollment_safety::IntendedLanePolicy {
    use crate::config::MeshLane;
    use crate::mesh::auto_enrollment_safety::IntendedLanePolicy;
    IntendedLanePolicy {
        metadata: preview_lane_decision(policy.allowed_lanes.decision(MeshLane::Metadata)),
        body: preview_lane_decision(policy.allowed_lanes.decision(MeshLane::Body)),
        embedding: preview_lane_decision(policy.allowed_lanes.decision(MeshLane::Embedding)),
        graph_link: preview_lane_decision(policy.allowed_lanes.decision(MeshLane::GraphLink)),
        revision_notice: preview_lane_decision(
            policy.allowed_lanes.decision(MeshLane::RevisionNotice),
        ),
        curation_signal: preview_lane_decision(
            policy.allowed_lanes.decision(MeshLane::CurationSignal),
        ),
    }
}

fn preview_lane_decision(
    decision: crate::config::MeshLaneDecision,
) -> crate::mesh::auto_enrollment_safety::LaneDecision {
    use crate::config::MeshLaneDecision;
    use crate::mesh::auto_enrollment_safety::LaneDecision;
    match decision {
        MeshLaneDecision::Allow => LaneDecision::Allow,
        MeshLaneDecision::Quarantine => LaneDecision::Quarantine,
        MeshLaneDecision::Deny => LaneDecision::Deny,
    }
}

fn config_mesh_lane(lane: crate::mesh::lane_grant_preview::Lane) -> crate::config::MeshLane {
    use crate::config::MeshLane;
    use crate::mesh::lane_grant_preview::Lane;
    match lane {
        Lane::Metadata => MeshLane::Metadata,
        Lane::Body => MeshLane::Body,
        Lane::Embedding => MeshLane::Embedding,
        Lane::GraphLink => MeshLane::GraphLink,
        Lane::CurationSignal => MeshLane::CurationSignal,
        Lane::RevisionNotice => MeshLane::RevisionNotice,
    }
}

fn preview_lane(args: MeshPreviewGrantLane) -> crate::mesh::lane_grant_preview::Lane {
    use crate::mesh::lane_grant_preview::Lane;
    match args {
        MeshPreviewGrantLane::Metadata => Lane::Metadata,
        MeshPreviewGrantLane::Body => Lane::Body,
        MeshPreviewGrantLane::Embedding => Lane::Embedding,
        MeshPreviewGrantLane::GraphLink => Lane::GraphLink,
        MeshPreviewGrantLane::CurationSignal => Lane::CurationSignal,
        MeshPreviewGrantLane::RevisionNotice => Lane::RevisionNotice,
    }
}

fn effective_lane_grant_policy(
    config: &crate::config::ConfigFile,
    config_bytes: &[u8],
    workspace_id: &str,
    peer_id: &str,
    state: Option<&crate::db::StoredMeshLaneGrantState>,
) -> Result<crate::mesh::policy::MeshPeerPolicy, DomainError> {
    use crate::core::memory_scope::MeshOutboundPolicyDecisionInput;
    use crate::mesh::policy::MeshPeerPolicyRegistry;
    let registry = MeshPeerPolicyRegistry::from_config(config);
    let input = MeshOutboundPolicyDecisionInput {
        local_workspace_id: workspace_id,
        origin_workspace_id: workspace_id,
        target_peer_id: peer_id,
        material_lane: crate::config::MeshLane::Metadata,
        payload_is_redacted: true,
    };
    let mut policy = registry
        .select_outbound_policy(&input)
        .map_err(|error| DomainError::Configuration {
            message: format!(
                "Cannot build lane approval for peer {peer_id}: {error}"
            ),
            repair: Some(
                "Configure exactly one mesh.peer_policies entry for this workspace, peer, and local origin, then re-run the preview."
                    .to_owned(),
            ),
        })?
        .clone();
    if let Some(state) = state.filter(|state| state.target_matches_current_peer) {
        let config_digest = crate::mesh::lane_grant::approval_config_digest(config_bytes);
        apply_lane_grant_state_to_policy(&mut policy, state, &config_digest);
    }
    Ok(policy)
}

fn apply_lane_grant_state_to_policy(
    policy: &mut crate::mesh::policy::MeshPeerPolicy,
    state: &crate::db::StoredMeshLaneGrantState,
    current_config_digest: &str,
) {
    use crate::config::MeshLane;
    if let Some(decision) =
        state.effective_override_for(MeshLane::Metadata, Some(current_config_digest))
    {
        policy.allowed_lanes.metadata = Some(decision);
    }
    if let Some(decision) =
        state.effective_override_for(MeshLane::Body, Some(current_config_digest))
    {
        policy.allowed_lanes.body = Some(decision);
    }
    if let Some(decision) =
        state.effective_override_for(MeshLane::Embedding, Some(current_config_digest))
    {
        policy.allowed_lanes.embedding = Some(decision);
    }
    if let Some(decision) =
        state.effective_override_for(MeshLane::GraphLink, Some(current_config_digest))
    {
        policy.allowed_lanes.graph_link = Some(decision);
    }
    if let Some(decision) =
        state.effective_override_for(MeshLane::RevisionNotice, Some(current_config_digest))
    {
        policy.allowed_lanes.revision_notice = Some(decision);
    }
    if let Some(decision) =
        state.effective_override_for(MeshLane::CurationSignal, Some(current_config_digest))
    {
        policy.allowed_lanes.curation_signal = Some(decision);
    }
}

fn lane_grant_policy_generation(
    config_bytes: &[u8],
    target_adapter_json: &str,
    generation: u64,
    policy: &crate::mesh::auto_enrollment_safety::IntendedLanePolicy,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.mesh.lane_policy_generation.v1\0");
    hasher.update(&(config_bytes.len() as u64).to_be_bytes());
    hasher.update(config_bytes);
    hasher.update(&(target_adapter_json.len() as u64).to_be_bytes());
    hasher.update(target_adapter_json.as_bytes());
    hasher.update(&generation.to_be_bytes());
    for decision in [
        policy.metadata,
        policy.body,
        policy.embedding,
        policy.graph_link,
        policy.revision_notice,
        policy.curation_signal,
    ] {
        hasher.update(decision.as_str().as_bytes());
        hasher.update(&[0]);
    }
    let digest = hasher.finalize().to_hex();
    format!("mesh_policy_{}", &digest[..24])
}

fn peer_is_in_lane_group(
    workspace_id: &str,
    peer_id: &str,
    bindings: &[crate::config::MeshPeerGroupBinding],
) -> bool {
    bindings.iter().any(|binding| {
        binding.workspace_id.as_deref() == Some(workspace_id)
            && binding
                .peer_ids
                .as_ref()
                .is_some_and(|peers| peers.iter().any(|peer| peer == peer_id))
    })
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
        let report = snapshot.status_report_with_autodiscovery(&autodiscovery);
        return write_mesh_status_json_with_autodiscovery(stdout, &report, &autodiscovery);
    }
    write_mesh_report(cli, &report, &render_mesh_status_human(&report), stdout)
}

fn handle_mesh_disable<W, E>(
    cli: &Cli,
    args: &MeshDisableArgs,
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
    let input = MeshEmergencyDisableInput {
        workspace_path: cli.resolve_workspace(),
        all_workspaces: args.all_workspaces,
        dry_run: args.dry_run,
        reason: args.reason.clone(),
        peer_id: args.peer.clone(),
        temporary_for: args.temporary_for.clone(),
        mesh_enabled_before: snapshot.mesh_enabled,
        command_mode_before: snapshot_mesh_command_mode(&snapshot),
    };
    let report = if args.dry_run {
        plan_emergency_disable(&input)
    } else {
        match apply_emergency_disable(&input) {
            Ok(report) => report,
            Err(error) => {
                return write_domain_error(
                    &mesh_emergency_domain_error(error),
                    cli.wants_json(),
                    stdout,
                    stderr,
                );
            }
        }
    };
    write_mesh_report(cli, &report, &render_mesh_disable_human(&report), stdout)
}

fn handle_mesh_reenable<W, E>(
    cli: &Cli,
    args: &MeshReenableArgs,
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
    let input = MeshEmergencyReenableInput {
        workspace_path: cli.resolve_workspace(),
        dry_run: args.dry_run,
        explicit: args.confirm_reenable,
        mesh_enabled_before: snapshot.mesh_enabled,
        command_mode_before: snapshot_mesh_command_mode(&snapshot),
    };
    let report = if args.dry_run {
        match plan_emergency_reenable(&input) {
            Ok(report) => report,
            Err(error) => {
                return write_domain_error(
                    &mesh_emergency_domain_error(error),
                    cli.wants_json(),
                    stdout,
                    stderr,
                );
            }
        }
    } else {
        match apply_emergency_reenable(&input) {
            Ok(report) => report,
            Err(error) => {
                return write_domain_error(
                    &mesh_emergency_domain_error(error),
                    cli.wants_json(),
                    stdout,
                    stderr,
                );
            }
        }
    };
    write_mesh_report(cli, &report, &render_mesh_reenable_human(&report), stdout)
}

fn snapshot_mesh_command_mode(snapshot: &MeshForegroundSnapshot) -> MeshCommandMode {
    snapshot.mode.parse().unwrap_or(MeshCommandMode::Off)
}

fn mesh_emergency_domain_error(
    error: crate::mesh::emergency_disable::MeshEmergencyError,
) -> DomainError {
    let message = error.to_string();
    match error {
        crate::mesh::emergency_disable::MeshEmergencyError::ReenableRequiresExplicitCommand => {
            DomainError::Usage {
                message,
                repair: Some(
                    "Re-run with `ee mesh reenable --confirm-reenable --json` after containment review."
                        .to_owned(),
                ),
            }
        }
        crate::mesh::emergency_disable::MeshEmergencyError::ReadConfig { .. }
        | crate::mesh::emergency_disable::MeshEmergencyError::ParseConfig { .. }
        | crate::mesh::emergency_disable::MeshEmergencyError::WriteConfig { .. } => {
            DomainError::Configuration {
                message,
                repair: Some(
                    "Inspect the workspace .ee/config.toml permissions and retry the mesh containment command."
                        .to_owned(),
                ),
            }
        }
        crate::mesh::emergency_disable::MeshEmergencyError::PeerScopeNotDurable { peer_id } => {
            DomainError::Usage {
                message,
                repair: Some(format!(
                    "Use `ee mesh peer revoke {peer_id} --json` for durable per-peer containment, or `ee mesh disable --json` (without --peer) for workspace-wide containment."
                )),
            }
        }
        crate::mesh::emergency_disable::MeshEmergencyError::PeerScopeConflictsAllWorkspaces {
            ..
        } => DomainError::Usage {
            message,
            repair: Some(
                "Pass exactly one containment scope: `--peer <id>` or `--all-workspaces`."
                    .to_owned(),
            ),
        },
    }
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
    let policy_state = load_discovery_policy_state(&workspace_path, None, None).ok();
    let discovery_mode = policy_state.as_ref().map_or_else(
        || DiscoveryMode::from_env_discovery(|_| {}),
        |state| state.discovery_mode,
    );
    let lists = policy_state
        .map(|state| state.lists)
        .unwrap_or_else(|| load_workspace_lists(&workspace_path).unwrap_or_default());
    let mut config = TailscaleAutodiscoveryConfig::new(
        snapshot.mesh_enabled,
        &snapshot.workspace_id,
        discovery_mode,
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
        "schema": crate::models::RESPONSE_SCHEMA_V2,
        "success": true,
        "data": data,
        "degraded": [],
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
        tailnet_peers: auto_enrollment_candidates_from_local(
            local.as_ref(),
            &snapshot.workspace_id,
        ),
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
            discovery.self_node_key.as_deref(),
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
                connection
                    .upsert_mesh_peer_with_grant_invalidation_in_current_transaction(upsert)?;
            }
            for revocation in &revocations {
                connection
                    .upsert_mesh_peer_with_grant_invalidation_in_current_transaction(revocation)?;
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
        Err(mut secret_scan) => {
            // Decorate the denial report's findings with fresh opaque IDs from
            // the OS CSPRNG at this effectful boundary. A randomness failure is
            // an ee.error.v2, never a hash-shaped or fallback identifier.
            if let Err(error) =
                decorate_export_secret_findings(&mut secret_scan, &mut OsSecretFindingRandom)
            {
                let domain_error = mesh_secret_finding_random_error(&error);
                return write_domain_error(&domain_error, cli.wants_json(), stdout, stderr);
            }
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
    let mut artifact = checked_export.artifact;
    // When a target peer is named, its outbound policy governs what may leave:
    // drop records the peer may not receive and strip denied bodies. This is
    // the production entry point that makes [[mesh.peer_policies]] load-bearing.
    if let Some(peer_id) = args.peer.as_deref() {
        let registry = match load_mesh_peer_policy_registry(cli, args.database.as_deref()) {
            Ok(registry) => registry,
            Err(error) => return write_domain_error(&error, cli.wants_json(), stdout, stderr),
        };
        let filtered = apply_outbound_export_policy(
            std::mem::take(&mut artifact.events),
            &registry,
            &snapshot.workspace_id,
            peer_id,
        );
        artifact.events = filtered.events;
    }
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

/// Build the peer-policy registry from the effective workspace config. A
/// missing or unparseable `config.toml` yields an empty registry, so a named
/// peer with no configured policy fails closed (every record is denied) rather
/// than leaking an unfiltered export.
pub(crate) fn load_mesh_peer_policy_registry(
    cli: &Cli,
    database_override: Option<&Path>,
) -> Result<crate::mesh::policy::MeshPeerPolicyRegistry, DomainError> {
    let workspace_path = cli.resolve_workspace();
    let database_path = database_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_path.join(".ee").join("ee.db"));
    let config_path = database_override
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| workspace_path.join(".ee"))
        .join("config.toml");
    let (config, config_bytes) = load_mesh_policy_config_snapshot_fail_closed(&config_path);
    let registry = mesh_peer_policy_registry_for_config_snapshot(&config, config_bytes.as_deref());
    if !database_path.is_file() {
        return Ok(registry);
    }
    let connection = open_mesh_connection(&database_path)?;
    let workspace_id = resolve_mesh_workspace_id(&connection, &workspace_path)?;
    let states = connection
        .list_mesh_lane_grant_states(&workspace_id)
        .map_err(|error| storage_error("Failed to load mesh lane-grant policy state", error))?;
    Ok(registry.with_lane_grant_states(states))
}

fn load_mesh_policy_config_snapshot_fail_closed(
    config_path: &Path,
) -> (crate::config::ConfigFile, Option<Vec<u8>>) {
    if !config_path.exists() {
        return (crate::config::ConfigFile::default(), Some(Vec::new()));
    }
    if !config_path.is_file() {
        return (crate::config::ConfigFile::default(), None);
    }
    let Ok(contents) = read_mesh_text_bounded(config_path, MESH_CONFIG_MAX_BYTES, "mesh config")
    else {
        return (crate::config::ConfigFile::default(), None);
    };
    let Ok(config) = crate::config::ConfigFile::parse(&contents) else {
        return (crate::config::ConfigFile::default(), None);
    };
    (config, Some(contents.into_bytes()))
}

fn mesh_peer_policy_registry_for_config_snapshot(
    config: &crate::config::ConfigFile,
    config_bytes: Option<&[u8]>,
) -> crate::mesh::policy::MeshPeerPolicyRegistry {
    config_bytes.map_or_else(
        || crate::mesh::policy::MeshPeerPolicyRegistry::from_config(config),
        |bytes| crate::mesh::policy::MeshPeerPolicyRegistry::from_config_snapshot(config, bytes),
    )
}

fn mesh_workspace_config_path(cli: &Cli, database_override: Option<&Path>) -> PathBuf {
    database_override
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| cli.resolve_workspace().join(".ee"))
        .join("config.toml")
}

/// Load the exact policy bytes authenticated by an approval snapshot. Unlike
/// ordinary fail-closed export/import lookup, a consent preview may not turn a
/// malformed or unreadable config into an apparently valid empty policy: that
/// would let a later parse repair silently change what the operator approved.
fn load_mesh_config_for_approval(
    cli: &Cli,
    database_override: Option<&Path>,
) -> Result<(crate::config::ConfigFile, Vec<u8>), DomainError> {
    let config_path = mesh_workspace_config_path(cli, database_override);
    if !config_path.exists() {
        return Ok((crate::config::ConfigFile::default(), Vec::new()));
    }
    if !config_path.is_file() {
        return Err(DomainError::Configuration {
            message: format!(
                "Mesh approval config path {} is not a regular file",
                config_path.display()
            ),
            repair: Some(
                "Restore .ee/config.toml as a readable regular file, then re-run the preview."
                    .to_owned(),
            ),
        });
    }
    let contents = read_mesh_text_bounded(&config_path, MESH_CONFIG_MAX_BYTES, "approval config")
        .map_err(|error| DomainError::Configuration {
        message: format!(
            "Failed to read mesh approval config {}: {error}",
            config_path.display()
        ),
        repair: Some(
            "Fix .ee/config.toml ownership, permissions, or size, then re-run the preview."
                .to_owned(),
        ),
    })?;
    let config = crate::config::ConfigFile::parse(&contents).map_err(|error| {
        DomainError::Configuration {
            message: format!(
                "Failed to parse mesh approval config {}: {error}",
                config_path.display()
            ),
            repair: Some(
                "Fix the reported .ee/config.toml field and obtain a fresh preview.".to_owned(),
            ),
        }
    })?;
    Ok((config, contents.into_bytes()))
}

fn mesh_secret_finding_random_error(
    error: &crate::policy::SecretFindingRandomError,
) -> DomainError {
    DomainError::Storage {
        message: format!(
            "mesh export could not decorate secret findings with secure identifiers: {}",
            error.message
        ),
        repair: Some("Retry on a host with a healthy OS CSPRNG.".to_owned()),
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
    let degraded = mesh_sync_cli_degradations(
        &snapshot.degraded,
        &supervisor.degraded,
        supervisor.contacted_peers,
    );
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

fn mesh_sync_cli_degradations(
    snapshot_degraded: &[MeshCliDegradation],
    supervisor_degraded: &[MeshCliDegradation],
    contacted_peers: bool,
) -> Vec<MeshCliDegradation> {
    let mut degraded = snapshot_degraded.to_vec();
    degraded.extend_from_slice(supervisor_degraded);
    if !contacted_peers
        && !degraded
            .iter()
            .any(|item| item.code == MESH_SYNC_ONCE_NETWORK_DEFERRED_CODE)
    {
        degraded.push(MeshCliDegradation::sync_once_network_deferred());
    }
    degraded
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
    let body = match read_mesh_text_bounded(&path, MESH_CONFIG_MAX_BYTES, "discovery policy config")
    {
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
        read_mesh_text_bounded(path, MESH_CONFIG_MAX_BYTES, "discovery policy config")
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
    if let Some(symlink_path) = crate::core::path_safety::first_existing_symlink_component(path)
        .map_err(|error| DomainError::Storage {
            message: format!(
                "Failed to inspect mesh discovery policy path {}: {error}",
                path.display()
            ),
            repair: Some("Check workspace .ee directory permissions.".to_owned()),
        })?
    {
        return Err(DomainError::PolicyDenied {
            message: format!(
                "Refusing to write mesh discovery policy through symlink component {}",
                symlink_path.display()
            ),
            repair: Some(
                "Replace the symlink with a regular directory or file owned by this workspace."
                    .to_owned(),
            ),
        });
    }
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
                tailnet_display_name: record.endpoint.tailnet_display_name,
                materialized_on_node_key: record.materialized_on_node_key,
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
                tailnet_display_name: None,
                materialized_on_node_key: None,
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
            hostname: auto_enrollment_candidate_hostname(
                peer.hostname.as_deref(),
                peer.magic_dns_name.as_deref(),
                &peer.tailscale_ip,
                &peer.node_key,
            ),
            ee_protocol_version: peer.ee_protocol_version.clone(),
            discovery_policy_decision: peer.discovery_policy_decision.clone(),
        })
        .collect()
}

fn auto_enrollment_candidate_hostname(
    hostname: Option<&str>,
    magic_dns_name: Option<&str>,
    tailscale_ip: &str,
    node_key: &str,
) -> String {
    hostname
        .and_then(trimmed_non_empty)
        .or_else(|| magic_dns_name.and_then(trimmed_magic_dns_name))
        .or_else(|| trimmed_non_empty(tailscale_ip))
        .unwrap_or_else(|| node_key.to_owned())
}

fn trimmed_non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn trimmed_magic_dns_name(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_end_matches('.');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn auto_enrollment_candidates_from_local(
    local: Option<&TailscaleLocalReport>,
    workspace_id: &str,
) -> Vec<AutoEnrollmentCandidate> {
    local
        .into_iter()
        .flat_map(|report| report.peers.iter())
        .filter_map(|peer| {
            if peer.online == Some(false) {
                return None;
            }
            let tailscale_ip = peer.tailscale_ips.first()?.clone();
            let capability = peer.ee_capability.as_ref().filter(|capability| {
                capability.respond
                    && capability.looks_like_ee()
                    && capability
                        .workspace_ids
                        .iter()
                        .any(|peer_workspace_id| peer_workspace_id == workspace_id)
            })?;
            let hostname = auto_enrollment_candidate_hostname(
                peer.hostname.as_deref(),
                peer.magic_dns_name.as_deref(),
                &tailscale_ip,
                &peer.node_key,
            );
            Some(AutoEnrollmentCandidate {
                node_key: peer.node_key.clone(),
                tailscale_ip,
                magic_dns_name: peer.magic_dns_name.clone(),
                hostname,
                ee_protocol_version: capability.ee_protocol_version.clone(),
                discovery_policy_decision: "force_include_override".to_owned(),
            })
        })
        .collect()
}

fn auto_enrollment_peer_upserts(
    workspace_id: &str,
    tailnet_id: &str,
    tailnet_display_name: Option<&str>,
    self_node_key: Option<&str>,
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
        peer.materialized_on_node_key = self_node_key.map(str::to_owned);
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
        read_mesh_text_bounded(&path, MESH_CONFIG_MAX_BYTES, "auto-enroll override config")
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
    let peer =
        serde_json::from_value::<MeshPeerRecord>(value).map_err(|error| DomainError::Usage {
            message: format!("Stored mesh peer {peer_id} record is malformed: {error}"),
            repair: Some(
                "Re-run `ee mesh peer add ... --yes --json` for this peer or revoke the stale row."
                    .to_owned(),
            ),
        })?;
    if peer.peer_id != peer_id {
        return Err(DomainError::Usage {
            message: format!(
                "Stored mesh peer row {peer_id} contains policy summary for peer {}",
                peer.peer_id
            ),
            repair: Some(
                "Re-run `ee mesh peer add ... --yes --json` for this peer or revoke the stale row."
                    .to_owned(),
            ),
        });
    }
    Ok(Some(peer))
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
    let contents = read_mesh_text_bounded(path, MESH_IMPORT_ARTIFACT_MAX_BYTES, "import artifact")
        .map_err(|error| DomainError::Storage {
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
    // Membership bindings, peer policy, and the approval config digest must
    // come from one byte snapshot; loading them separately permits an atomic
    // config replacement to combine authority from two different files.
    let config_path = mesh_workspace_config_path(cli, database_override);
    let (config, config_bytes) = load_mesh_policy_config_snapshot_fail_closed(&config_path);
    let bindings = config.mesh.peer_group_bindings.clone().unwrap_or_default();
    let states = connection
        .list_mesh_lane_grant_states(&workspace_id)
        .map_err(|error| storage_error("Failed to load mesh lane-grant policy state", error))?;
    let registry = mesh_peer_policy_registry_for_config_snapshot(&config, config_bytes.as_deref())
        .with_lane_grant_states(states);
    import_mesh_artifact_into_connection(&connection, &workspace_id, artifact, &bindings, &registry)
}

fn import_mesh_artifact_into_connection(
    connection: &DbConnection,
    workspace_id: &str,
    artifact: &MeshExportArtifact,
    bindings: &[crate::config::MeshPeerGroupBinding],
    registry: &crate::mesh::policy::MeshPeerPolicyRegistry,
) -> Result<(usize, usize, usize), DomainError> {
    let mut imported_peer_count = 0;
    for peer in &artifact.peers {
        // Enrollment identity and enabled state are local authority. A replay
        // artifact may introduce a missing peer, but it must never rotate,
        // re-enable, or roll back a peer already enrolled in this store.
        let inserted = connection
            .insert_mesh_peer_if_absent(&UpsertMeshPeerInput {
                workspace_id: workspace_id.to_owned(),
                peer_id: peer.peer_id.clone(),
                origin_node_id: peer.origin_node_id.clone(),
                display_name: peer.display_name.clone(),
                policy_summary_json: peer.policy_summary_json.clone(),
                enabled: peer.enabled,
                last_seen_at: Some(peer.last_seen_at.clone()),
            })
            .map_err(|error| storage_error("Failed to import mesh peer", error))?;
        if inserted {
            imported_peer_count += 1;
        }
    }

    let mut imported_cursor_count = 0;
    for cursor in &artifact.cursors {
        let enrolled_peer = connection
            .get_mesh_peer(workspace_id, &cursor.peer_id)
            .map_err(|error| {
                storage_error("Failed to verify mesh cursor producer identity", error)
            })?;
        if !enrolled_peer
            .as_ref()
            .is_some_and(|peer| peer.enabled && peer.origin_node_id == cursor.origin_node_id)
        {
            continue;
        }
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

    // Refresh after admitting any previously unknown peer so the import
    // decision sees the current locally authoritative enrollment and grant.
    let refreshed_states = connection
        .list_mesh_lane_grant_states(workspace_id)
        .map_err(|error| storage_error("Failed to refresh mesh lane-grant policy state", error))?;
    let refreshed_registry = registry.clone().with_lane_grant_states(refreshed_states);

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
        let decision = if let Some(producer_peer_id) = event.producer_peer_id.as_deref() {
            let enrolled_peer = connection
                .get_mesh_peer(workspace_id, producer_peer_id)
                .map_err(|error| {
                    storage_error("Failed to verify mesh event producer identity", error)
                })?;
            match enrolled_peer {
                Some(peer) if peer.enabled && peer.origin_node_id == event.origin_node_id => {
                    decide_import_event(workspace_id, event, bindings, &refreshed_registry)
                }
                _ => denied_import_event(event, "peer_identity_mismatch"),
            }
        } else {
            decide_import_event(workspace_id, event, bindings, &refreshed_registry)
        };
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
                import_decision: decision.import_decision.clone(),
                local_memory_id: event.local_memory_id.clone(),
                body_cache_key: event.body_cache_key.clone(),
                policy_failure_surface_json: decision.policy_failure_surface_json.clone(),
                policy_decision_json: decision.policy_decision_json.clone(),
                event_json: event.event_json.clone(),
                imported_at: Some(event.imported_at.clone()),
            })
            .map_err(|error| storage_error("Failed to import mesh event", error))?;
        if changed {
            // Deny/quarantine/reject still records the ledger row (the honest
            // import record) but performs no local-truth side effects: only an
            // admitted event enqueues an index job / memory-side upsert.
            if decision.admits {
                enqueue_mesh_import_index_job(connection, workspace_id, event)?;
            }
            imported_event_count += 1;
        }
    }
    Ok((
        imported_peer_count,
        imported_cursor_count,
        imported_event_count,
    ))
}

/// The effective inbound decision for one replayed mesh event, ready to write
/// into the import ledger. `admits` gates every local-truth side effect
/// (search/index enqueue and any memory-side upsert).
struct ImportEventDecision {
    import_decision: String,
    policy_decision_json: Option<String>,
    policy_failure_surface_json: Option<String>,
    admits: bool,
}

fn denied_import_event(
    event: &crate::mesh::foreground_cli::MeshEventRow,
    reason: &str,
) -> ImportEventDecision {
    ImportEventDecision {
        import_decision: "deny".to_owned(),
        policy_decision_json: Some(
            json!({
                "schema": "ee.mesh.policy_decision.v1",
                "direction": "inbound",
                "action": "deny",
                "reason": reason,
                "materialLane": event.material_lane,
            })
            .to_string(),
        ),
        policy_failure_surface_json: Some(
            json!({
                "schema": "ee.mesh.policy_failure_surface.v1",
                "code": "mesh_peer_policy_denied",
                "reason": reason,
            })
            .to_string(),
        ),
        admits: false,
    }
}

/// Peer-supplied events may not claim trust classes above the `agent_validated`
/// ceiling; a claim of operator, CASS, or legacy authority is rejected so a
/// peer cannot launder elevated trust into the local store (ADR 0086 TC-D3).
const MESH_IMPORT_REJECTED_TRUST_CLAIMS: [&str; 3] =
    ["human_explicit", "cass_evidence", "legacy_import"];

/// Compose the two-layer inbound authority for one replayed event: the
/// peer-group membership gate (`[[mesh.peer_group_bindings]]` via
/// `decide_mesh_import`) followed by the authoritative peer policy
/// (`[[mesh.peer_policies]]` via `decide_inbound`), under an outer trust-claim
/// ceiling. Only a bound peer-group member whose policy admits the lane admits
/// to local truth; everything else records a ledger denial/rejection and
/// admits nothing. The recorded decision is always computed locally — the
/// transported `import_decision`/`policy_decision_json` on the event are never
/// trusted (ADR 0086 TC-D3, plan P0.2/T1.3).
fn decide_import_event(
    workspace_id: &str,
    event: &crate::mesh::foreground_cli::MeshEventRow,
    bindings: &[crate::config::MeshPeerGroupBinding],
    registry: &crate::mesh::policy::MeshPeerPolicyRegistry,
) -> ImportEventDecision {
    use crate::core::memory_scope::{
        MeshEventValidity, MeshImportDecisionInput, MeshPeerPolicyDecisionInput,
        decide_mesh_import_with_lane_override, parse_mesh_lane,
    };

    // Outer ceiling: reject an over-claimed trust lane before any policy work.
    if MESH_IMPORT_REJECTED_TRUST_CLAIMS.contains(&event.trust_lane.as_str()) {
        return ImportEventDecision {
            import_decision: "reject".to_owned(),
            policy_decision_json: Some(
                json!({
                    "schema": "ee.mesh.policy_decision.v1",
                    "direction": "inbound",
                    "action": "reject",
                    "reason": "peer_trust_claim_exceeds_ceiling",
                    "claimedTrustLane": event.trust_lane,
                })
                .to_string(),
            ),
            policy_failure_surface_json: Some(
                json!({
                    "schema": "ee.mesh.policy_failure_surface.v1",
                    "code": "mesh_peer_policy_rejected",
                    "reason": "peer_trust_claim_exceeds_ceiling",
                })
                .to_string(),
            ),
            admits: false,
        };
    }

    // Fail closed on an unparseable lane (mirrors the outbound export filter).
    let Some(lane) = parse_mesh_lane(&event.material_lane) else {
        return denied_import_event(event, "unparseable_material_lane");
    };

    let origin = event.origin_workspace_id.as_str();
    let producer = event.producer_peer_id.as_deref().unwrap_or("");

    // Layer 1 — peer-group membership gate.
    let membership_input = MeshImportDecisionInput {
        local_workspace_id: workspace_id,
        origin_workspace_id: origin,
        producer_peer_id: producer,
        material_lane: lane,
        event_validity: MeshEventValidity::Valid,
    };
    let membership_override = registry.inbound_membership_override(&membership_input);
    let membership = decide_mesh_import_with_lane_override(
        &membership_input,
        bindings,
        membership_override.as_ref(),
    );
    if !membership.permits_local_truth_side_effects() {
        return ImportEventDecision {
            import_decision: membership.workspace_scope_decision.as_str().to_owned(),
            policy_decision_json: Some(
                json!({
                    "schema": "ee.mesh.policy_decision.v1",
                    "direction": "inbound",
                    "layer": "peer_group_membership",
                    "action": membership.workspace_scope_decision.as_str(),
                    "reason": membership.reason,
                    "membership": membership.to_log_fields(),
                })
                .to_string(),
            ),
            policy_failure_surface_json: Some(
                json!({
                    "schema": "ee.mesh.policy_failure_surface.v1",
                    "code": "mesh_peer_policy_denied",
                    "reason": membership.reason,
                })
                .to_string(),
            ),
            admits: false,
        };
    }

    // Layer 2 — authoritative peer policy (lane / redaction / trust cap).
    let policy = registry.decide_inbound(&MeshPeerPolicyDecisionInput {
        local_workspace_id: workspace_id,
        origin_workspace_id: origin,
        producer_peer_id: producer,
        material_lane: lane,
        event_validity: MeshEventValidity::Valid,
        requested_body_bytes: None,
        body_fetch_consent: false,
    });
    ImportEventDecision {
        import_decision: policy.import.workspace_scope_decision.as_str().to_owned(),
        policy_decision_json: Some(policy.to_json().to_string()),
        policy_failure_surface_json: policy
            .failure_surface()
            .map(|surface| surface.to_json().to_string()),
        admits: policy.import.permits_local_truth_side_effects(),
    }
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
    write_mesh_report_with_degraded(cli, report, human_output, &[], stdout)
}

fn write_mesh_report_with_degraded<W, T>(
    cli: &Cli,
    report: &T,
    human_output: &str,
    degraded: &[serde_json::Value],
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
                "schema": crate::models::RESPONSE_SCHEMA_V2,
                "success": true,
                "data": report,
                "degraded": degraded,
            });
            write_stdout(stdout, &(json.to_string() + "\n"))
        }
    }
}

fn lane_grant_preview_degraded(
    preview: &crate::mesh::lane_grant_preview::LaneGrantPreview,
) -> Vec<serde_json::Value> {
    use crate::mesh::lane_grant_preview::{
        LANE_GRANT_PREVIEW_LANE_ALREADY_GRANTED_CODE, LANE_GRANT_PREVIEW_PEER_NOT_IN_GROUP_CODE,
        caution_kinds,
    };

    preview
        .cautions
        .iter()
        .filter_map(|caution| match caution.kind.as_str() {
            caution_kinds::PEER_NOT_IN_GROUP => Some(json!({
                "code": LANE_GRANT_PREVIEW_PEER_NOT_IN_GROUP_CODE,
                "severity": "info",
                "message": caution.message,
                "repair": format!(
                    "Add peer '{}' to `peer_ids` in the matching `[[mesh.peer_group_bindings]]` entry for workspace '{}' in `.ee/config.toml`, then re-run `ee mesh preview-grant {} --lane {} --json`.",
                    preview.target.peer_id,
                    preview.workspace_id,
                    super::shell_quote_cli_arg(&preview.target.peer_id),
                    lane_cli_value_from_wire(&preview.lane),
                ),
            })),
            caution_kinds::LANE_ALREADY_GRANTED => Some(json!({
                "code": LANE_GRANT_PREVIEW_LANE_ALREADY_GRANTED_CODE,
                "severity": "info",
                "message": caution.message,
                "repair": format!(
                    "Review current exposure with `ee mesh preview-grant {} --lane {} --json`; use `ee mesh revoke-lane` if the lane should be closed.",
                    super::shell_quote_cli_arg(&preview.target.peer_id),
                    lane_cli_value_from_wire(&preview.lane),
                ),
            })),
            _ => None,
        })
        .collect()
}

fn lane_cli_value_from_wire(lane: &str) -> &str {
    match lane {
        "graph_link" => "graph-link",
        "curation_signal" => "curation-signal",
        "revision_notice" => "revision-notice",
        other => other,
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

fn render_mesh_disable_human(report: &MeshEmergencyDisableReport) -> String {
    let mut output = format!(
        "Mesh disable: {scope}\n  Workspace: {workspace}\n  Mesh: {before} -> {after}\n  Mode: {mode_before} -> {mode_after}\n  Applied: {applied}\n  Listener stopped: {listener}\n  Background sync stopped: {sync}\n  Queued exports cancelled: {queued}\n  Local cache readable: {cache}\n  Source-of-truth memories preserved: {truth}\n",
        scope = report.scope,
        workspace = report.workspace_path,
        before = report.mesh_enabled_before,
        after = report.mesh_enabled_after,
        mode_before = report.command_mode_before,
        mode_after = report.command_mode_after,
        applied = report.applied,
        listener = report.listener_stopped,
        sync = report.background_sync_stopped,
        queued = report.queued_exports_cancelled,
        cache = report.local_cache_readable,
        truth = report.source_of_truth_memories_preserved,
    );
    if let Some(reason) = &report.reason {
        output.push_str(&format!("  Reason: {reason}\n"));
    }
    if !report.peer_capabilities_suspended.is_empty() {
        output.push_str("  Peer suspensions:\n");
        for peer in &report.peer_capabilities_suspended {
            output.push_str(&format!(
                "    - {} capabilities={}\n",
                peer.peer_id,
                peer.capabilities_suspended.join(",")
            ));
        }
    }
    output
}

fn render_mesh_reenable_human(report: &MeshEmergencyReenableReport) -> String {
    format!(
        "Mesh reenable\n  Workspace: {workspace}\n  Mesh: {before} -> {after}\n  Mode: {mode_before} -> {mode_after}\n  Applied: {applied}\n  Explicit confirmation: {explicit}\n",
        workspace = report.workspace_path,
        before = report.mesh_enabled_before,
        after = report.mesh_enabled_after,
        mode_before = report.command_mode_before,
        mode_after = report.command_mode_after,
        applied = report.applied,
        explicit = report.explicit,
    )
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
    use crate::core::tailscale_probe::{
        TailscalePeerEeCapability, TailscalePeerReport, TailscaleProbeMethod,
    };
    use crate::mesh::tailscale_autodiscovery::TailscaleAutodiscoveryPeer;

    #[test]
    fn approval_token_recovery_shell_quotes_untrusted_peer_id() {
        let peer_id = "peer'; touch /tmp/should-not-run; echo '";
        let error = approval_token_domain_error(
            &crate::mesh::lane_grant::ApprovalTokenError::Invalid,
            peer_id,
            crate::mesh::lane_grant_preview::Lane::GraphLink,
        );
        let details_json = match error {
            DomainError::UsageCodeWithDetails { details_json, .. } => details_json,
            other => panic!("expected structured usage error, got {other:?}"),
        };
        let details: serde_json::Value =
            serde_json::from_str(&details_json).expect("structured recovery JSON");
        let command = details
            .pointer("/recovery/0/command")
            .and_then(serde_json::Value::as_str)
            .expect("recovery command");
        let quoted_peer = super::super::shell_quote_cli_arg(peer_id);
        assert_eq!(
            command,
            format!(
                "ee mesh preview-grant {quoted_peer} --lane graph-link --issue-approval-token --json"
            )
        );
    }

    #[test]
    fn lane_grant_human_confirmation_is_bounded() {
        assert!(read_lane_grant_confirmation(std::io::Cursor::new(b"yes\nignored")).unwrap());
        assert!(!read_lane_grant_confirmation(std::io::Cursor::new(b"no\n")).unwrap());

        let oversized = vec![b'y'; LANE_GRANT_CONFIRMATION_MAX_BYTES + 1];
        let error = read_lane_grant_confirmation(std::io::Cursor::new(oversized))
            .expect_err("oversized confirmation must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("byte limit"));
    }

    #[test]
    fn read_mesh_text_bounded_refuses_oversized_payload() {
        // bd-3l1cy: a mesh discovery-policy / auto-enroll / import-artifact
        // path larger than the per-context cap must be refused with
        // io::ErrorKind::InvalidData before we materialize it. Mirrors the
        // swarm-brief bd-1sdr5 regression for the same defect class.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("oversized.toml");
        let cap: usize = 1024;
        std::fs::write(&path, vec![b'x'; cap + 1]).expect("write oversized");

        let error = read_mesh_text_bounded(&path, cap, "discovery policy config")
            .expect_err("oversized read must error");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        let message = format!("{error}");
        assert!(
            message.contains("exceeds") && message.contains("byte cap"),
            "expected over-cap diagnostic; got {message}"
        );
        assert!(
            message.contains("discovery policy config"),
            "expected kind label in diagnostic; got {message}"
        );
    }

    #[test]
    fn read_mesh_text_bounded_passes_payload_at_cap() {
        // bd-3l1cy: at-the-cap reads must succeed; only over-cap reads
        // are refused. Otherwise legitimate configs that happen to size
        // exactly to the cap would be wrongly rejected.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("at_cap.toml");
        let cap: usize = 64;
        std::fs::write(&path, vec![b'y'; cap]).expect("write at-cap");

        let body = read_mesh_text_bounded(&path, cap, "auto-enroll override config")
            .expect("at-cap read must succeed");
        assert_eq!(body.len(), cap);
    }

    #[test]
    fn read_mesh_text_bounded_propagates_not_found() {
        // bd-3l1cy: the load_workspace_policy_modes adapter discriminates
        // on ErrorKind::NotFound to return the default empty policy
        // without surfacing a DomainError::Storage. The bounded-read
        // helper must preserve that distinction.
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("does_not_exist.toml");
        let error =
            read_mesh_text_bounded(&missing, MESH_CONFIG_MAX_BYTES, "discovery policy config")
                .expect_err("missing path must error");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn ensure_writable_regular_file_rejects_symlinked_parent_component() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tempdir");
        let real_ee = tmp.path().join("real-ee");
        std::fs::create_dir_all(&real_ee).expect("create real .ee");
        let linked_ee = tmp.path().join(".ee");
        symlink(&real_ee, &linked_ee).expect("create .ee symlink");

        let error = ensure_writable_regular_file(&linked_ee.join("discovery_policy.toml"))
            .expect_err("symlinked .ee parent must be refused");
        match error {
            DomainError::PolicyDenied { message, .. } => {
                assert!(
                    message.contains("symlink component"),
                    "expected symlink component diagnostic; got {message}"
                );
            }
            other => panic!("expected PolicyDenied for symlinked parent; got {other:?}"),
        }
        assert!(
            !real_ee.join("discovery_policy.toml").exists(),
            "policy writes must not be redirected through symlinked .ee"
        );
    }

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
    fn mesh_sync_cli_degradations_do_not_add_deferred_after_peer_contact() {
        let degraded = mesh_sync_cli_degradations(&[], &[], true);

        assert!(
            degraded
                .iter()
                .all(|item| item.code != MESH_SYNC_ONCE_NETWORK_DEFERRED_CODE),
            "contacted supervisor reports must not be overwritten with deferred fallback"
        );
    }

    #[test]
    fn mesh_sync_cli_degradations_add_deferred_when_no_peer_contact_happened() {
        let degraded = mesh_sync_cli_degradations(&[], &[], false);

        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, MESH_SYNC_ONCE_NETWORK_DEFERRED_CODE);
    }

    #[test]
    fn mesh_sync_cli_degradations_do_not_duplicate_supervisor_deferred_code() {
        let supervisor_degraded = vec![MeshCliDegradation::sync_once_network_deferred()];
        let degraded = mesh_sync_cli_degradations(&[], &supervisor_degraded, false);

        assert_eq!(degraded.len(), 1);
        assert_eq!(degraded[0].code, MESH_SYNC_ONCE_NETWORK_DEFERRED_CODE);
    }

    #[test]
    fn autodiscovery_uses_persisted_workspace_discovery_policy() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace_path = tmp.path();
        let node_key = "nodekey:00000000000000000000000000000000000000000000000000000000000000aa";
        write_workspace_policy_modes(
            &discovery_policy_config_path(workspace_path),
            DiscoveryMode::Allowlist,
            DiscoveryMode::ServiceTag,
        )
        .expect("write discovery policy config");
        write_node_key_list(
            &workspace_path.join(".ee").join(DISCOVERY_ALLOWLIST_FILE),
            &std::collections::BTreeSet::from([node_key.to_owned()]),
        )
        .expect("write discovery allowlist");

        let cli = Cli::try_parse_from([
            "ee",
            "--workspace",
            workspace_path.to_str().expect("utf8 workspace path"),
            "--json",
        ])
        .expect("parse cli");
        let snapshot = MeshForegroundSnapshot {
            workspace_id: "wsp_test_workspace".to_owned(),
            workspace_path: workspace_path.display().to_string(),
            database_path: workspace_path
                .join(".ee")
                .join("ee.db")
                .display()
                .to_string(),
            initialized: true,
            mesh_enabled: true,
            mode: "cache".to_owned(),
            storage: MeshStorageCounts::default(),
            peers: Vec::new(),
            cursors: Vec::new(),
            events: Vec::new(),
            degraded: Vec::new(),
        };
        let local = TailscaleLocalReport {
            schema: crate::core::tailscale_probe::TAILSCALE_LOCAL_SCHEMA_V1,
            installed: true,
            daemon_reachable: true,
            authenticated: true,
            binary_authentic: true,
            binary_version_raw: None,
            binary_absolute_path: None,
            shields_up: Some(false),
            tailnet_id: Some("tailnet-alpha".to_owned()),
            tailnet_display_name: Some("alpha.example".to_owned()),
            self_node_key: Some(
                "nodekey:0000000000000000000000000000000000000000000000000000000000000001"
                    .to_owned(),
            ),
            self_tailscale_ip: Some("100.64.0.1".to_owned()),
            self_magic_dns_name: Some("self.tailnet.test.".to_owned()),
            self_advertised_tags: Vec::new(),
            peers: vec![TailscalePeerReport {
                node_key: node_key.to_owned(),
                tailscale_ips: vec!["100.64.0.2".to_owned()],
                magic_dns_name: Some("allowed.tailnet.test.".to_owned()),
                hostname: Some("allowed".to_owned()),
                advertised_tags: Vec::new(),
                online: Some(true),
                ee_capability: Some(TailscalePeerEeCapability {
                    ee_version: "0.2.0".to_owned(),
                    ee_protocol_version: "1.0".to_owned(),
                    workspace_ids: vec!["wsp_test_workspace".to_owned()],
                    respond: true,
                    latency_ms: 1,
                }),
            }],
            version: Some("1.66.0".to_owned()),
            probe_method: TailscaleProbeMethod::Cli,
            probe_elapsed_ms: 10,
            platform: TailscalePlatform::Linux,
            degradations: Vec::new(),
        };

        let report = build_tailscale_autodiscovery_report_from_local(&cli, &snapshot, Some(&local));

        assert_eq!(report.probed_peer_count, 1);
        assert_eq!(report.eligible_peer_count, 1);
        assert_eq!(report.ee_capable_peers[0].node_key, node_key);
        assert_eq!(
            report.ee_capable_peers[0].discovery_policy_decision,
            "allowlisted"
        );
        assert!(
            report.skipped_peers.is_empty(),
            "{:?}",
            report.skipped_peers
        );
    }

    #[test]
    fn mesh_json_envelopes_include_clean_degraded_array() {
        let cli = Cli::try_parse_from(["ee", "--json"]).expect("parse json cli");
        let mut stdout = Vec::new();
        let report = json!({
            "schema": "ee.mesh.test.v1",
            "command": "mesh test"
        });

        let exit = write_mesh_report(&cli, &report, "", &mut stdout);

        assert_eq!(exit, ProcessExitCode::Success);
        let envelope: serde_json::Value =
            serde_json::from_slice(&stdout).expect("mesh report json envelope");
        assert_eq!(envelope["schema"], crate::models::RESPONSE_SCHEMA_V2);
        assert_eq!(envelope["success"], true);
        assert_eq!(envelope["degraded"], serde_json::json!([]));

        let snapshot = mesh_snapshot_with_peers(Vec::new());
        let status = snapshot.status_report();
        let autodiscovery = TailscaleAutodiscoveryReport {
            schema: crate::mesh::tailscale_autodiscovery::TAILSCALE_AUTODISCOVERY_SCHEMA_V1,
            tailnet_id: None,
            tailnet_display_name: None,
            self_node_key: None,
            probed_peer_count: 0,
            eligible_peer_count: 0,
            ee_capable_peers: Vec::new(),
            skipped_peers: Vec::new(),
            degraded: Vec::new(),
        };
        stdout.clear();

        let exit = write_mesh_status_json_with_autodiscovery(&mut stdout, &status, &autodiscovery);

        assert_eq!(exit, ProcessExitCode::Success);
        let envelope: serde_json::Value =
            serde_json::from_slice(&stdout).expect("mesh status json envelope");
        assert_eq!(envelope["schema"], crate::models::RESPONSE_SCHEMA_V2);
        assert_eq!(envelope["success"], true);
        assert_eq!(envelope["degraded"], serde_json::json!([]));
        assert_eq!(
            envelope["data"]["autoEnrollment"]["discovery"]["schema"],
            crate::mesh::tailscale_autodiscovery::TAILSCALE_AUTODISCOVERY_SCHEMA_V1
        );
    }

    #[test]
    fn lane_grant_preview_cautions_project_to_catalogued_degraded_entries() {
        use crate::mesh::auto_enrollment_safety::{IntendedLanePolicy, LaneDecision};
        use crate::mesh::lane_grant_preview::{
            LANE_GRANT_PREVIEW_LANE_ALREADY_GRANTED_CODE,
            LANE_GRANT_PREVIEW_PEER_NOT_IN_GROUP_CODE, Lane, LaneGrantPreviewInput, SampleStrategy,
            compute_lane_grant_preview,
        };

        let mut already_allowed = IntendedLanePolicy::conservative_default();
        already_allowed.body = LaneDecision::Allow;
        let redaction_rules = Vec::new();
        let memories = Vec::new();
        let preview = compute_lane_grant_preview(&LaneGrantPreviewInput {
            peer_node_key: "peer-not-in-group",
            peer_in_group: false,
            lane: Lane::Body,
            workspace_id: "wsp-test",
            current_policy: already_allowed,
            proposed_policy: already_allowed,
            memories: &memories,
            sample_strategy: SampleStrategy::Random,
            limit: 25,
            redaction_rules: &redaction_rules,
            sample_random_seed: 0,
        });

        let degraded = lane_grant_preview_degraded(&preview);
        assert_eq!(degraded.len(), 2);
        assert_eq!(
            degraded[0]["code"],
            LANE_GRANT_PREVIEW_PEER_NOT_IN_GROUP_CODE
        );
        assert_eq!(
            degraded[1]["code"],
            LANE_GRANT_PREVIEW_LANE_ALREADY_GRANTED_CODE
        );
        assert!(degraded.iter().all(|entry| entry["severity"] == "info"));
        assert!(degraded.iter().all(|entry| {
            entry["repair"]
                .as_str()
                .is_some_and(|repair| repair.contains("ee mesh"))
        }));
        assert!(
            degraded[0]["message"]
                .as_str()
                .is_some_and(|message| message.contains("peer-group bindings"))
        );
        assert!(
            degraded[0]["repair"]
                .as_str()
                .is_some_and(|repair| repair.contains("[[mesh.peer_group_bindings]]"))
        );
        assert!(
            degraded[1]["message"]
                .as_str()
                .is_some_and(|message| message.contains("currently exposed"))
        );
    }

    #[test]
    fn auto_enrollment_peer_upserts_persist_materialized_node_key_binding() {
        let candidates = vec![AutoEnrollmentCandidate {
            node_key: "nodekey:alpha".to_owned(),
            tailscale_ip: "100.64.0.2".to_owned(),
            magic_dns_name: Some("alpha.tailnet.test.".to_owned()),
            hostname: "alpha".to_owned(),
            ee_protocol_version: "1.0".to_owned(),
            discovery_policy_decision: "service_tag_match".to_owned(),
        }];

        let upserts = auto_enrollment_peer_upserts(
            "wsp_test_workspace",
            "tailnet-alpha",
            Some("alpha.example"),
            Some("nodekey:self"),
            "2026-05-20T00:00:00Z",
            &candidates,
        )
        .expect("auto-enrollment peer upsert should build");
        let policy_summary_json = upserts[0]
            .policy_summary_json
            .as_deref()
            .expect("auto-enrollment should persist peer record JSON");
        let value: serde_json::Value =
            serde_json::from_str(policy_summary_json).expect("peer record JSON should parse");

        assert_eq!(
            value["schema"],
            crate::mesh::peer::MESH_PEER_RECORD_SCHEMA_V1
        );
        assert_eq!(value["materializedOnNodeKey"], "nodekey:self");
        assert_eq!(value["endpoint"]["tailnetId"], "tailnet-alpha");
        assert_eq!(value["trustEstablishedBy"], "tailscale_auto_enrollment");
    }

    #[test]
    fn auto_enrollment_discovery_candidate_hostname_uses_magic_dns_before_node_key() {
        let report = TailscaleAutodiscoveryReport {
            schema: crate::mesh::tailscale_autodiscovery::TAILSCALE_AUTODISCOVERY_SCHEMA_V1,
            tailnet_id: Some("tailnet-alpha".to_owned()),
            tailnet_display_name: Some("alpha.example".to_owned()),
            self_node_key: Some("nodekey:self".to_owned()),
            probed_peer_count: 1,
            eligible_peer_count: 1,
            ee_capable_peers: vec![TailscaleAutodiscoveryPeer {
                node_key: "nodekey:alpha".to_owned(),
                tailscale_ip: "100.64.0.2".to_owned(),
                magic_dns_name: Some("alpha.tailnet.test.".to_owned()),
                hostname: None,
                ee_protocol_version: "1.0".to_owned(),
                workspace_match_set: vec!["workspace-alpha".to_owned()],
                last_probed_at: "2026-05-20T00:00:00Z".to_owned(),
                latency_ms: 5,
                discovery_policy_decision: "service_tag_match".to_owned(),
            }],
            skipped_peers: Vec::new(),
            degraded: Vec::new(),
        };

        let candidates = auto_enrollment_candidates_from_discovery(&report);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].hostname, "alpha.tailnet.test");
    }

    #[test]
    fn auto_enrollment_local_candidate_hostname_uses_tailscale_ip_before_node_key() {
        let report = TailscaleLocalReport {
            schema: crate::core::tailscale_probe::TAILSCALE_LOCAL_SCHEMA_V1,
            installed: true,
            daemon_reachable: true,
            authenticated: true,
            binary_authentic: true,
            binary_version_raw: None,
            binary_absolute_path: None,
            shields_up: Some(false),
            tailnet_id: Some("tailnet-alpha".to_owned()),
            tailnet_display_name: Some("alpha.example".to_owned()),
            self_node_key: Some("nodekey:self".to_owned()),
            self_tailscale_ip: Some("100.64.0.1".to_owned()),
            self_magic_dns_name: Some("self.tailnet.test.".to_owned()),
            self_advertised_tags: Vec::new(),
            peers: vec![TailscalePeerReport {
                node_key: "nodekey:ee".to_owned(),
                tailscale_ips: vec!["100.64.0.4".to_owned()],
                magic_dns_name: None,
                hostname: Some("   ".to_owned()),
                advertised_tags: Vec::new(),
                online: Some(true),
                ee_capability: Some(TailscalePeerEeCapability {
                    ee_version: "0.2.0".to_owned(),
                    ee_protocol_version: "1.0".to_owned(),
                    workspace_ids: vec!["workspace-alpha".to_owned()],
                    respond: true,
                    latency_ms: 1,
                }),
            }],
            version: Some("1.66.0".to_owned()),
            probe_method: TailscaleProbeMethod::Cli,
            probe_elapsed_ms: 10,
            platform: TailscalePlatform::Linux,
            degradations: Vec::new(),
        };

        let candidates = auto_enrollment_candidates_from_local(Some(&report), "workspace-alpha");

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].hostname, "100.64.0.4");
    }

    #[test]
    fn auto_enrollment_local_include_candidates_require_valid_ee_capability() {
        let report = TailscaleLocalReport {
            schema: crate::core::tailscale_probe::TAILSCALE_LOCAL_SCHEMA_V1,
            installed: true,
            daemon_reachable: true,
            authenticated: true,
            binary_authentic: true,
            binary_version_raw: None,
            binary_absolute_path: None,
            shields_up: Some(false),
            tailnet_id: Some("tailnet-alpha".to_owned()),
            tailnet_display_name: Some("alpha.example".to_owned()),
            self_node_key: Some("nodekey:self".to_owned()),
            self_tailscale_ip: Some("100.64.0.1".to_owned()),
            self_magic_dns_name: Some("self.tailnet.test.".to_owned()),
            self_advertised_tags: Vec::new(),
            peers: vec![
                TailscalePeerReport {
                    node_key: "nodekey:plain".to_owned(),
                    tailscale_ips: vec!["100.64.0.2".to_owned()],
                    magic_dns_name: Some("plain.tailnet.test.".to_owned()),
                    hostname: Some("plain".to_owned()),
                    advertised_tags: Vec::new(),
                    online: Some(true),
                    ee_capability: None,
                },
                TailscalePeerReport {
                    node_key: "nodekey:malformed".to_owned(),
                    tailscale_ips: vec!["100.64.0.3".to_owned()],
                    magic_dns_name: Some("malformed.tailnet.test.".to_owned()),
                    hostname: Some("malformed".to_owned()),
                    advertised_tags: Vec::new(),
                    online: Some(true),
                    ee_capability: Some(TailscalePeerEeCapability {
                        ee_version: "0.0.0".to_owned(),
                        ee_protocol_version: "1.0".to_owned(),
                        workspace_ids: vec!["workspace-alpha".to_owned()],
                        respond: true,
                        latency_ms: 1,
                    }),
                },
                TailscalePeerReport {
                    node_key: "nodekey:ee".to_owned(),
                    tailscale_ips: vec!["100.64.0.4".to_owned()],
                    magic_dns_name: Some("ee.tailnet.test.".to_owned()),
                    hostname: Some("ee".to_owned()),
                    advertised_tags: Vec::new(),
                    online: Some(true),
                    ee_capability: Some(TailscalePeerEeCapability {
                        ee_version: "0.2.0".to_owned(),
                        ee_protocol_version: "1.0".to_owned(),
                        workspace_ids: vec!["workspace-alpha".to_owned()],
                        respond: true,
                        latency_ms: 1,
                    }),
                },
                TailscalePeerReport {
                    node_key: "nodekey:offline-ee".to_owned(),
                    tailscale_ips: vec!["100.64.0.7".to_owned()],
                    magic_dns_name: Some("offline-ee.tailnet.test.".to_owned()),
                    hostname: Some("offline-ee".to_owned()),
                    advertised_tags: Vec::new(),
                    online: Some(false),
                    ee_capability: Some(TailscalePeerEeCapability {
                        ee_version: "0.2.0".to_owned(),
                        ee_protocol_version: "1.0".to_owned(),
                        workspace_ids: vec!["workspace-alpha".to_owned()],
                        respond: true,
                        latency_ms: 1,
                    }),
                },
                TailscalePeerReport {
                    node_key: "nodekey:declined".to_owned(),
                    tailscale_ips: vec!["100.64.0.5".to_owned()],
                    magic_dns_name: Some("declined.tailnet.test.".to_owned()),
                    hostname: Some("declined".to_owned()),
                    advertised_tags: Vec::new(),
                    online: Some(true),
                    ee_capability: Some(TailscalePeerEeCapability {
                        ee_version: "0.2.0".to_owned(),
                        ee_protocol_version: "1.0".to_owned(),
                        workspace_ids: vec!["workspace-alpha".to_owned()],
                        respond: false,
                        latency_ms: 1,
                    }),
                },
                TailscalePeerReport {
                    node_key: "nodekey:other-workspace".to_owned(),
                    tailscale_ips: vec!["100.64.0.6".to_owned()],
                    magic_dns_name: Some("other-workspace.tailnet.test.".to_owned()),
                    hostname: Some("other-workspace".to_owned()),
                    advertised_tags: Vec::new(),
                    online: Some(true),
                    ee_capability: Some(TailscalePeerEeCapability {
                        ee_version: "0.2.0".to_owned(),
                        ee_protocol_version: "1.0".to_owned(),
                        workspace_ids: vec!["workspace-beta".to_owned()],
                        respond: true,
                        latency_ms: 1,
                    }),
                },
            ],
            version: Some("1.66.0".to_owned()),
            probe_method: TailscaleProbeMethod::Cli,
            probe_elapsed_ms: 10,
            platform: TailscalePlatform::Linux,
            degradations: Vec::new(),
        };

        let candidates = auto_enrollment_candidates_from_local(Some(&report), "workspace-alpha");

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].node_key, "nodekey:ee");
        assert_eq!(candidates[0].ee_protocol_version, "1.0");
        assert_eq!(
            candidates[0].discovery_policy_decision,
            "force_include_override"
        );
    }

    #[test]
    fn enrolled_peer_record_rejects_policy_summary_peer_id_mismatch() {
        let record = enroll_peer(MeshPeerEnrollInput {
            workspace_id: "wsp_test_workspace".to_owned(),
            alias: "alpha".to_owned(),
            endpoint: MeshPeerEndpoint {
                tailscale_node_key: "nodekey:alpha".to_owned(),
                tailnet_id: "tailnet-alpha".to_owned(),
                tailnet_display_name: Some("alpha.example".to_owned()),
                endpoint: "100.64.0.2".to_owned(),
                magic_dns_name: Some("alpha.tailnet.test.".to_owned()),
            },
            capability_profile: MeshPeerCapabilityProfile::MetadataOnly,
            handshake: MeshPeerHandshake::granted(
                "hello_req_alpha",
                "1.0",
                "nodekey:alpha",
                vec!["mesh:metadata".to_owned()],
            ),
            public_key_fingerprint: "blake3:pubkey-alpha".to_owned(),
            now: "2026-05-20T00:00:00Z".to_owned(),
            explicit_human_consent: true,
        })
        .peer
        .expect("peer record");
        let policy_summary_json =
            serde_json::to_string(&record).expect("peer record should serialize");

        let error = enrolled_peer_record_from_policy_summary(
            Some(&policy_summary_json),
            "peer_different_row",
        )
        .expect_err("policy summary peer id must match row peer id");

        assert!(
            error.message().contains("contains policy summary for peer"),
            "unexpected error: {}",
            error.message()
        );
        assert!(
            error
                .repair()
                .is_some_and(|repair| repair.contains("ee mesh peer add")),
            "repair should direct operator to refresh or revoke the stale row"
        );
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

    /// Config that admits `peer_mesh_replay_counts` (origin
    /// `wsp_remote…0001`) into `wsp_meshreplay…0001` on the metadata lane
    /// through BOTH layers: a peer-group binding (membership) and a peer
    /// policy (lane/redaction/trust). Body/embedding stay denied.
    fn admitting_replay_config() -> crate::config::ConfigFile {
        crate::config::ConfigFile::parse(
            r#"
[[mesh.peer_group_bindings]]
workspace_id = "wsp_meshreplay0000000000000001"
peer_group_id = "pg_replay_counts"
peer_ids = ["peer_mesh_replay_counts"]
origin_workspace_ids = ["wsp_remote00000000000000000001"]
default_action = "deny"

[mesh.peer_group_bindings.lanes]
metadata = "allow"
body = "deny"
embedding = "deny"
graph_link = "allow"
revision_notice = "allow"
curation_signal = "allow"

[[mesh.peer_policies]]
policy_id = "pol_replay_counts"
workspace_id = "wsp_meshreplay0000000000000001"
peer_id = "peer_mesh_replay_counts"
origin_workspace_ids = ["wsp_remote00000000000000000001"]
trust_lane = "peerAgent"
import_trust_class = "agent_validated"
default_action = "deny"

[mesh.peer_policies.allowed_lanes]
metadata = "allow"
body = "deny"
embedding = "deny"
graph_link = "allow"
revision_notice = "allow"
curation_signal = "allow"

[mesh.peer_policies.redaction]
metadata = "share"
preview = "redact"
body = "deny"
embedding = "deny"

[mesh.peer_policies.body_fetch]
allowed = false
requires_consent = true
max_bytes = 0
"#,
        )
        .expect("admitting replay config parses")
    }

    fn admitting_replay_authority() -> (
        Vec<crate::config::MeshPeerGroupBinding>,
        crate::mesh::policy::MeshPeerPolicyRegistry,
    ) {
        let config = admitting_replay_config();
        let bindings = config.mesh.peer_group_bindings.clone().unwrap_or_default();
        let registry = crate::mesh::policy::MeshPeerPolicyRegistry::from_config(&config);
        (bindings, registry)
    }

    fn replay_event(
        material_lane: &str,
        trust_lane: &str,
    ) -> crate::mesh::foreground_cli::MeshEventRow {
        crate::mesh::foreground_cli::MeshEventRow {
            event_id: "mesh_evt_decide_00000000000000000001".to_string(),
            origin_node_id: "node_mesh_replay_counts".to_string(),
            origin_workspace_id: "wsp_remote00000000000000000001".to_string(),
            producer_peer_id: Some("peer_mesh_replay_counts".to_string()),
            seq: 1,
            prev_event_hash: None,
            event_hash: hash_for_test('c'),
            event_kind: "create".to_string(),
            logical_memory_id: "mem_mesh_decide".to_string(),
            content_hash: hash_for_test('d'),
            material_lane: material_lane.to_string(),
            redaction_class: "metadataOnly".to_string(),
            trust_lane: trust_lane.to_string(),
            import_decision: "allow".to_string(),
            local_memory_id: None,
            body_cache_key: None,
            policy_failure_surface_json: None,
            policy_decision_json: None,
            event_json: r#"{"schema":"ee.mesh.event.v1","eventKind":"create"}"#.to_string(),
            policy_attestation: None,
            imported_at: "2026-05-21T19:50:02Z".to_string(),
        }
    }

    fn failure_code(decision: &ImportEventDecision) -> Option<String> {
        let raw = decision.policy_failure_surface_json.as_ref()?;
        serde_json::from_str::<serde_json::Value>(raw)
            .ok()?
            .get("code")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    }

    const REPLAY_WORKSPACE_ID: &str = "wsp_meshreplay0000000000000001";

    #[test]
    fn import_event_decision_admits_configured_member_metadata() {
        let (bindings, registry) = admitting_replay_authority();
        let event = replay_event("metadata", "peerAgent");
        let decision = decide_import_event(REPLAY_WORKSPACE_ID, &event, &bindings, &registry);
        assert!(decision.admits, "configured member metadata must admit");
        assert_eq!(decision.import_decision, "allow");
        assert!(decision.policy_failure_surface_json.is_none());
    }

    #[test]
    fn import_event_decision_denies_body_lane_under_metadata_only_policy() {
        let (bindings, registry) = admitting_replay_authority();
        let event = replay_event("body", "peerAgent");
        let decision = decide_import_event(REPLAY_WORKSPACE_ID, &event, &bindings, &registry);
        assert!(!decision.admits, "body lane is denied, admits nothing");
        assert_eq!(decision.import_decision, "deny");
        assert_eq!(
            failure_code(&decision).as_deref(),
            Some("mesh_peer_policy_denied")
        );
    }

    #[test]
    fn import_event_decision_denies_non_member_peer() {
        // No bindings => membership gate fails closed regardless of policy.
        let (_bindings, registry) = admitting_replay_authority();
        let event = replay_event("metadata", "peerAgent");
        let decision = decide_import_event(REPLAY_WORKSPACE_ID, &event, &[], &registry);
        assert!(!decision.admits, "non-member must admit nothing");
        assert_eq!(decision.import_decision, "deny");
    }

    #[test]
    fn import_event_decision_rejects_over_claimed_trust() {
        let (bindings, registry) = admitting_replay_authority();
        for claim in ["human_explicit", "cass_evidence", "legacy_import"] {
            let event = replay_event("metadata", claim);
            let decision = decide_import_event(REPLAY_WORKSPACE_ID, &event, &bindings, &registry);
            assert!(
                !decision.admits,
                "over-claimed trust ({claim}) must not admit"
            );
            assert_eq!(decision.import_decision, "reject", "claim {claim}");
            assert_eq!(
                failure_code(&decision).as_deref(),
                Some("mesh_peer_policy_rejected"),
                "claim {claim}"
            );
        }
    }

    #[test]
    fn import_event_decision_fails_closed_on_unparseable_lane() {
        let (bindings, registry) = admitting_replay_authority();
        let event = replay_event("not_a_real_lane", "peerAgent");
        let decision = decide_import_event(REPLAY_WORKSPACE_ID, &event, &bindings, &registry);
        assert!(!decision.admits, "unparseable lane fails closed");
        assert_eq!(decision.import_decision, "deny");
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
        let config = admitting_replay_config();
        let bindings = config.mesh.peer_group_bindings.clone().unwrap_or_default();
        let registry = crate::mesh::policy::MeshPeerPolicyRegistry::from_config(&config);
        let first = import_mesh_artifact_into_connection(
            &connection,
            workspace_id,
            &artifact,
            &bindings,
            &registry,
        )
        .expect("first import");
        assert_eq!(first, (1, 1, 1));

        let duplicate = import_mesh_artifact_into_connection(
            &connection,
            workspace_id,
            &artifact,
            &bindings,
            &registry,
        )
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

    #[test]
    fn mesh_import_peer_rotation_cannot_reuse_a_stale_lane_grant() {
        let connection = DbConnection::open_memory().expect("open memory db");
        connection.migrate().expect("migrate db");
        let workspace_id = REPLAY_WORKSPACE_ID;
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-mesh-replay-rotated-grant".to_string(),
                    name: Some("mesh replay rotated grant".to_string()),
                },
            )
            .expect("insert workspace");

        let peer_id = "peer_mesh_replay_counts";
        let original_node_id = "node_mesh_replay_original";
        connection
            .upsert_mesh_peer(&UpsertMeshPeerInput {
                workspace_id: workspace_id.to_owned(),
                peer_id: peer_id.to_owned(),
                origin_node_id: original_node_id.to_owned(),
                display_name: Some("replay-counts".to_owned()),
                policy_summary_json: None,
                enabled: true,
                last_seen_at: Some("2026-05-21T19:49:00Z".to_owned()),
            })
            .expect("insert original peer target");
        connection
            .apply_mesh_lane_grant_with_effect(
                &crate::db::MeshLaneGrantMutationInput {
                    workspace_id: workspace_id.to_owned(),
                    peer_id: peer_id.to_owned(),
                    target_adapter: crate::db::MeshLaneGrantTargetAdapter::new(
                        peer_id,
                        original_node_id,
                    ),
                    material_lane: crate::config::MeshLane::GraphLink,
                    expected_generation: 0,
                    approval_config_digest: Some(crate::mesh::lane_grant::approval_config_digest(
                        b"replay test config",
                    )),
                    updated_at: Some("2026-05-21T19:49:01Z".to_owned()),
                },
                |_| Ok::<(), std::convert::Infallible>(()),
            )
            .expect("grant graph lane to original target");

        let rotated_node_id = "node_mesh_replay_rotated";
        connection
            .upsert_mesh_peer(&UpsertMeshPeerInput {
                workspace_id: workspace_id.to_owned(),
                peer_id: peer_id.to_owned(),
                origin_node_id: rotated_node_id.to_owned(),
                display_name: Some("replay-counts-rotated".to_owned()),
                policy_summary_json: None,
                enabled: true,
                last_seen_at: Some("2026-05-21T19:49:02Z".to_owned()),
            })
            .expect("rotate the locally authoritative peer target");

        let mut config = admitting_replay_config();
        config
            .mesh
            .peer_group_bindings
            .as_mut()
            .expect("peer-group binding")
            .first_mut()
            .expect("peer-group binding row")
            .lanes
            .graph_link = Some(crate::config::MeshLaneDecision::Deny);
        config
            .mesh
            .peer_policies
            .as_mut()
            .expect("peer policy")
            .first_mut()
            .expect("peer policy row")
            .allowed_lanes
            .graph_link = Some(crate::config::MeshLaneDecision::Deny);
        let bindings = config.mesh.peer_group_bindings.clone().unwrap_or_default();
        let grant_states = connection
            .list_mesh_lane_grant_states(workspace_id)
            .expect("load post-rotation grant state");
        assert!(
            grant_states[0].target_matches_current_peer,
            "local rotation must rebind the cleared generation fence to the new target"
        );
        assert_eq!(grant_states[0].grant_generation, 2);
        assert_eq!(grant_states[0].graph_link_override, None);
        let registry = crate::mesh::policy::MeshPeerPolicyRegistry::from_config(&config)
            .with_lane_grant_states(grant_states);

        let mut artifact = mesh_export_artifact_for_import_counts();
        artifact.peers[0].origin_node_id = original_node_id.to_owned();
        artifact.cursors[0].origin_node_id = original_node_id.to_owned();
        artifact.events[0].origin_node_id = original_node_id.to_owned();
        artifact.events[0].material_lane = "graph_link".to_owned();

        let imported = import_mesh_artifact_into_connection(
            &connection,
            workspace_id,
            &artifact,
            &bindings,
            &registry,
        )
        .expect("import rotating peer and event");
        assert_eq!(
            imported,
            (0, 0, 1),
            "the artifact may record its denied event but cannot roll back peer or cursor identity"
        );

        let peer = connection
            .get_mesh_peer(workspace_id, peer_id)
            .expect("reload enrolled peer")
            .expect("peer remains enrolled");
        assert_eq!(peer.origin_node_id, rotated_node_id);
        assert!(peer.enabled);

        let refreshed = connection
            .get_mesh_lane_grant_state(workspace_id, peer_id)
            .expect("reload grant state")
            .expect("grant state remains durable");
        assert!(
            refreshed.target_matches_current_peer,
            "artifact replay must not replace the locally rebound target"
        );
        assert_eq!(refreshed.grant_generation, 2);
        assert_eq!(refreshed.graph_link_override, None);
        let events = connection
            .list_mesh_import_ledger_events_for_workspace(workspace_id)
            .expect("list imported events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].import_decision, "deny");
        assert!(
            events[0]
                .policy_decision_json
                .as_deref()
                .is_some_and(|decision| decision.contains("peer_identity_mismatch")),
            "the ledger must explain the local enrollment identity mismatch"
        );
        assert!(
            connection
                .list_search_index_jobs(workspace_id, None)
                .expect("list index jobs")
                .is_empty(),
            "a rotated target's stale grant must admit no local-truth side effects"
        );
    }

    #[test]
    fn mesh_import_records_denial_and_admits_nothing_for_non_member() {
        let connection = DbConnection::open_memory().expect("open memory db");
        connection.migrate().expect("migrate db");
        let workspace_id = "wsp_meshreplay0000000000000001";
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-mesh-replay-deny".to_string(),
                    name: Some("mesh replay deny".to_string()),
                },
            )
            .expect("insert workspace");

        // No peer-group bindings and an empty policy registry => the producer
        // is not a bound member, so the membership gate denies before policy.
        let artifact = mesh_export_artifact_for_import_counts();
        let empty_registry = crate::mesh::policy::MeshPeerPolicyRegistry::from_config(
            &crate::config::ConfigFile::default(),
        );
        let counts = import_mesh_artifact_into_connection(
            &connection,
            workspace_id,
            &artifact,
            &[],
            &empty_registry,
        )
        .expect("import");
        assert_eq!(
            counts,
            (1, 1, 1),
            "peer/cursor/event ledger rows are still recorded honestly"
        );

        let events = connection
            .list_mesh_import_ledger_events_for_workspace(workspace_id)
            .expect("list events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].import_decision, "deny");
        assert!(
            events[0]
                .policy_failure_surface_json
                .as_deref()
                .is_some_and(|json| json.contains("mesh_peer_policy_denied")),
            "denial must record the mesh_peer_policy_denied failure surface: {:?}",
            events[0].policy_failure_surface_json
        );

        let index_jobs = connection
            .list_search_index_jobs(workspace_id, None)
            .expect("list index jobs");
        assert!(
            index_jobs.is_empty(),
            "a denied event admits nothing: no import index job may be enqueued"
        );
    }

    #[test]
    fn mesh_import_denies_body_lane_under_metadata_only_authority() {
        let connection = DbConnection::open_memory().expect("open memory db");
        connection.migrate().expect("migrate db");
        let workspace_id = "wsp_meshreplay0000000000000001";
        connection
            .insert_workspace(
                workspace_id,
                &crate::db::CreateWorkspaceInput {
                    path: "/tmp/ee-mesh-replay-body".to_string(),
                    name: Some("mesh replay body".to_string()),
                },
            )
            .expect("insert workspace");

        // The configured member is admitted for metadata but the body lane is
        // denied by both layers; the event records a denial and admits nothing.
        let mut artifact = mesh_export_artifact_for_import_counts();
        artifact.events[0].material_lane = "body".to_string();
        let config = admitting_replay_config();
        let bindings = config.mesh.peer_group_bindings.clone().unwrap_or_default();
        let registry = crate::mesh::policy::MeshPeerPolicyRegistry::from_config(&config);
        let counts = import_mesh_artifact_into_connection(
            &connection,
            workspace_id,
            &artifact,
            &bindings,
            &registry,
        )
        .expect("import");
        assert_eq!(counts, (1, 1, 1));

        let events = connection
            .list_mesh_import_ledger_events_for_workspace(workspace_id)
            .expect("list events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].material_lane, "body");
        assert_eq!(events[0].import_decision, "deny");

        let index_jobs = connection
            .list_search_index_jobs(workspace_id, None)
            .expect("list index jobs");
        assert!(
            index_jobs.is_empty(),
            "body lane is denied; nothing is admitted to local truth"
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
                logical_memory_id: "mem_mesh_replay_counts".to_string(),
                content_hash: hash_for_test('d'),
                material_lane: "metadata".to_string(),
                redaction_class: "metadataOnly".to_string(),
                trust_lane: "peerAgent".to_string(),
                import_decision: "allow".to_string(),
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
