//! Foreground `ee mesh` CLI support.
//!
//! These types deliberately model local, daemon-free operations only. They are
//! safe to use with mesh disabled, with no Tailscale installation, and against a
//! workspace that has no mesh rows yet.

use std::collections::BTreeSet;
use std::net::{SocketAddr, UdpSocket};
use std::path::Path;
use std::time::Duration;

use asupersync::runtime::yield_now::yield_now;
use asupersync::time::sleep as asupersync_sleep;
use asupersync::{CancelReason, Cx, Outcome};
use serde::{Deserialize, Serialize};

use crate::config::{
    EnvVar, MeshCommandMode, MeshLane, MeshLaneDecision, parse_env_bool_flag, read_env_var,
    workspace_config,
};
use crate::core::memory_scope::{
    MeshEventValidity, MeshImportDecisionInput, MeshImportDecisionKind,
    MeshOutboundPolicyDecisionInput, decide_mesh_import, parse_mesh_lane,
};
use crate::core::tailscale_probe::TailscaleLocalReport;
use crate::db::{
    DbConnection, InsertMeshImportLedgerEventInput, MeshStorageStatus, StoredMeshImportLedgerEvent,
    StoredMeshPeer, StoredMeshPeerCursor, UpsertMeshPeerCursorInput,
};
use crate::mesh::anti_entropy_protocol::{
    MeshAntiEntropyRetryPolicy, MeshRoundPeerOutcome, MeshSyncSummaryInput, build_sync_summary,
};
use crate::mesh::bootstrap_envelope::{
    BODY_FETCH_REQUEST_SCHEMA_V1, BodyFetchRequest, BodyFetchResponse, SyncRoundRequest,
    SyncRoundResponse, SyncRoundTip, exchange_bootstrap_hello, exchange_live_mesh_round,
    parse_live_peer_endpoint, parse_sync_round_response,
};
use crate::mesh::hello::{build_request, serialize_within_budget};
use crate::mesh::hello_responder::configured_hello_port;
use crate::mesh::identity_change_guard::{
    AUTO_ENROLLMENT_NODE_KEY_CHANGED_CODE, AUTO_ENROLLMENT_TAILNET_CHANGED_CODE, BoundIdentity,
    CurrentIdentity, IdentityGuardVerdict, evaluate_identity_guard,
};
use crate::mesh::idp::IdentityAttestFrameV1;
use crate::mesh::key_store::{MeshKeyStore, PairKeyClass};
use crate::mesh::peer::{MESH_PEER_RECORD_SCHEMA_V1, MeshPeerRecord};
use crate::mesh::policy::MeshPeerPolicyRegistry;
use crate::mesh::repair_action_graph::{
    ActionKind, ExecutionContext, ExpectedOutcome, Priority, REPAIR_ACTION_GRAPH_SCHEMA_V1,
    RepairAction, RepairActionGraph, build_repair_action_graph,
};
use crate::mesh::responder_broker::{InboundLocalApi, TailscaleLocalApi};
use crate::mesh::sync::{SelectiveSyncConfig, SelectiveSyncStatusSummary};
use crate::mesh::tailscale_autodiscovery::{
    TAILSCALE_AUTODISCOVERY_SCHEMA_V1, TAILSCALE_PEER_LIST_UNAVAILABLE_CODE,
    TailscaleAutodiscoveryDegradation, TailscaleAutodiscoveryReport,
};
use crate::mesh::transport_session::{
    FrameCapability, HandshakeObservations, InitiatorSessionConfig, SessionBinding,
    SessionCapabilities, SessionChannelLimits, SessionMessage, connect_authenticated_session,
};
use crate::policy::{
    MeshExportPolicyAttestation, MeshExportSecretScanReport, MeshExportSecretScanSubject,
    scan_mesh_export_subjects,
};

pub const MESH_CLI_STATUS_SCHEMA_V1: &str = "ee.mesh.cli.status.v1";
pub const MESH_CLI_PEERS_SCHEMA_V1: &str = "ee.mesh.cli.peers.v1";
pub const MESH_CLI_EXPORT_SCHEMA_V1: &str = "ee.mesh.cli.export.v1";
pub const MESH_CLI_IMPORT_SCHEMA_V2: &str = "ee.mesh.cli.import.v2";
pub const MESH_CLI_SYNC_SCHEMA_V1: &str = "ee.mesh.cli.sync.v1";
pub const MESH_EXPORT_ARTIFACT_SCHEMA_V1: &str = "ee.mesh.foreground_export.v1";
pub const MESH_AUTO_STATUS_SCHEMA_V2: &str = "ee.mesh.auto_status.v2";
pub const MESH_IMPORT_LEDGER_SCHEMA_V1: &str = "ee.mesh.import_ledger.v1";

pub const MESH_WORKSPACE_UNINITIALIZED_CODE: &str = "mesh_workspace_uninitialized";
pub const MESH_DISABLED_POSTURE_CODE: &str = "mesh_disabled";
pub const MESH_SYNC_ONCE_NETWORK_DEFERRED_CODE: &str = "mesh_sync_once_network_deferred";
pub const MESH_SYNC_SUPERVISOR_BUDGET_EXHAUSTED_CODE: &str =
    "mesh_sync_supervisor_budget_exhausted";
pub const MESH_SYNC_SUPERVISOR_BACKPRESSURE_CODE: &str = "mesh_sync_supervisor_backpressure";
pub const MESH_SYNC_SUPERVISOR_RUNTIME_ERROR_CODE: &str = "mesh_sync_supervisor_runtime_error";
pub const MESH_IMPORT_PEER_NOT_CONSENTED_CODE: &str = "mesh_import_peer_not_consented";
pub const MESH_IMPORT_CURSOR_UNVERIFIED_CODE: &str = "mesh_import_cursor_unverified";

const MESH_SYNC_SUPERVISOR_SCHEMA_V1: &str = "ee.mesh.sync_supervisor.v1";
const MAX_MESH_SYNC_SUPERVISOR_TICKS: u32 = 64;
const MAX_MESH_SYNC_SUPERVISOR_CADENCE_MS: u64 = 300_000;
const MESH_SYNC_SUPERVISOR_SLEEP_SLICE_MS: u64 = 250;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshCliDegradation {
    pub code: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub repair: String,
}

impl MeshCliDegradation {
    #[must_use]
    pub fn workspace_uninitialized(workspace_path: &str) -> Self {
        let workspace_arg = mesh_shell_quote_arg(workspace_path);
        Self {
            code: MESH_WORKSPACE_UNINITIALIZED_CODE,
            severity: "warning",
            message: format!(
                "Mesh foreground storage was not inspected because {workspace_path}/.ee/ee.db does not exist."
            ),
            repair: format!("Run `ee init --workspace {workspace_arg} --json`."),
        }
    }

    #[must_use]
    pub fn mesh_disabled() -> Self {
        Self {
            code: MESH_DISABLED_POSTURE_CODE,
            severity: "info",
            message: "Optional mesh memory is disabled; foreground commands remain local-only."
                .to_owned(),
            repair: "Set EE_MESH_ENABLED=1 or configure [mesh].enabled only when you want mesh surfaces."
                .to_owned(),
        }
    }

    #[must_use]
    pub fn sync_once_network_deferred() -> Self {
        Self {
            code: MESH_SYNC_ONCE_NETWORK_DEFERRED_CODE,
            severity: "info",
            message: "Foreground sync --once did not contact peers because no usable foreground peer transport path was available."
                .to_owned(),
            repair: "Inspect enrolled peers with `ee mesh peers --json`, then use `ee mesh export` and `ee mesh import --file mesh-export.json` for a foreground peer transfer, or configure a peer transport before retrying sync. Use `ee export` or `ee backup` for local backups."
                .to_owned(),
        }
    }

    /// Emitted when ACTIVE peers are enrolled — an anti-entropy round was
    /// warranted — but no transport could carry it (bd-tc-epic-qzk7o.2.5
    /// item e; the real peer transport lands with tc-T2.4).
    #[must_use]
    pub fn anti_entropy_transport_unavailable(active_peer_count: usize) -> Self {
        Self {
            code: crate::mesh::anti_entropy_protocol::degraded_codes::TRANSPORT_UNAVAILABLE,
            severity: "low",
            message: format!(
                "An anti-entropy round was warranted ({active_peer_count} active peer(s) enrolled) but no peer transport path is available; no round ran."
            ),
            repair: "Use `ee mesh export` and `ee mesh import --file mesh-export.json` for a foreground transfer until a real peer transport is configured; `ee mesh peers --json` lists the peers awaiting sync."
                .to_owned(),
        }
    }

    #[must_use]
    pub fn sync_supervisor_budget_exhausted(resource: &str, limit: impl std::fmt::Display) -> Self {
        Self {
            code: MESH_SYNC_SUPERVISOR_BUDGET_EXHAUSTED_CODE,
            severity: "warning",
            message: format!(
                "Mesh sync supervisor exhausted the {resource} budget before peer contact."
            ),
            repair: format!(
                "Raise the {resource} budget above {limit} or reduce mesh peer fan-out for this sync run."
            ),
        }
    }

    #[must_use]
    pub fn sync_supervisor_backpressure(active_peers: usize, peer_concurrency: u32) -> Self {
        Self {
            code: MESH_SYNC_SUPERVISOR_BACKPRESSURE_CODE,
            severity: "info",
            message: format!(
                "Mesh sync supervisor limited {active_peers} active peers to {peer_concurrency} concurrent peer slots."
            ),
            repair: "Increase --peer-concurrency only if the current host has enough network and body-fetch budget."
                .to_owned(),
        }
    }

    #[must_use]
    pub fn sync_supervisor_runtime_error(message: &str) -> Self {
        Self {
            code: MESH_SYNC_SUPERVISOR_RUNTIME_ERROR_CODE,
            severity: "warning",
            message: format!("Mesh sync supervisor did not start: {message}"),
            repair: "Retry after the Asupersync runtime is healthy; foreground export/import remains available."
                .to_owned(),
        }
    }

    #[must_use]
    pub fn import_peer_not_consented(rejected_peer_count: usize) -> Self {
        Self {
            code: MESH_IMPORT_PEER_NOT_CONSENTED_CODE,
            severity: "warning",
            message: format!(
                "Mesh import rejected {rejected_peer_count} peer row(s) without an exact enabled local enrollment."
            ),
            repair: "Enroll the peer explicitly with `ee mesh peer add`, then obtain a fresh artifact whose peer identity matches the local enrollment."
                .to_owned(),
        }
    }

    #[must_use]
    pub fn import_cursor_unverified(rejected_cursor_count: usize) -> Self {
        Self {
            code: MESH_IMPORT_CURSOR_UNVERIFIED_CODE,
            severity: "warning",
            message: format!(
                "Mesh import rejected {rejected_cursor_count} cursor row(s) that were not backed by locally durable contiguous accepted replay."
            ),
            repair: "Obtain a fresh artifact containing the complete contiguous event range; use the explicit mesh cursor repair workflow for intentional regressions."
                .to_owned(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshStorageCounts {
    pub peer_count: u32,
    pub cursor_count: u32,
    pub imported_event_count: u32,
    pub policy_decision_event_count: u32,
    pub policy_failure_event_count: u32,
    pub mapped_memory_count: u32,
    pub cached_body_count: u32,
}

impl From<&MeshStorageStatus> for MeshStorageCounts {
    fn from(status: &MeshStorageStatus) -> Self {
        Self {
            peer_count: status.peer_count,
            cursor_count: status.cursor_count,
            imported_event_count: status.imported_event_count,
            policy_decision_event_count: status.policy_decision_event_count,
            policy_failure_event_count: status.policy_failure_event_count,
            mapped_memory_count: status.mapped_memory_count,
            cached_body_count: status.cached_body_count,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshPeerRow {
    pub peer_id: String,
    pub origin_node_id: String,
    pub display_name: Option<String>,
    pub enabled: bool,
    pub last_seen_at: String,
    pub policy_summary_json: Option<String>,
}

impl From<&StoredMeshPeer> for MeshPeerRow {
    fn from(peer: &StoredMeshPeer) -> Self {
        Self {
            peer_id: peer.peer_id.clone(),
            origin_node_id: peer.origin_node_id.clone(),
            display_name: peer.display_name.clone(),
            enabled: peer.enabled,
            last_seen_at: peer.last_seen_at.clone(),
            policy_summary_json: peer.policy_summary_json.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshCursorRow {
    pub peer_id: String,
    pub origin_node_id: String,
    pub origin_workspace_id: String,
    pub last_seq: u64,
    pub tip_event_hash: Option<String>,
    pub tip_audit_hash: Option<String>,
    pub status: String,
    pub updated_at: String,
}

impl From<&StoredMeshPeerCursor> for MeshCursorRow {
    fn from(cursor: &StoredMeshPeerCursor) -> Self {
        Self {
            peer_id: cursor.peer_id.clone(),
            origin_node_id: cursor.origin_node_id.clone(),
            origin_workspace_id: cursor.origin_workspace_id.clone(),
            last_seq: cursor.last_seq,
            tip_event_hash: cursor.tip_event_hash.clone(),
            tip_audit_hash: cursor.tip_audit_hash.clone(),
            status: cursor.status.clone(),
            updated_at: cursor.updated_at.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshEventRow {
    pub event_id: String,
    pub origin_node_id: String,
    pub origin_workspace_id: String,
    pub producer_peer_id: Option<String>,
    pub seq: u64,
    pub prev_event_hash: Option<String>,
    pub event_hash: String,
    pub event_kind: String,
    pub logical_memory_id: String,
    pub content_hash: String,
    pub material_lane: String,
    pub redaction_class: String,
    pub trust_lane: String,
    pub import_decision: String,
    pub local_memory_id: Option<String>,
    pub body_cache_key: Option<String>,
    pub policy_failure_surface_json: Option<String>,
    pub policy_decision_json: Option<String>,
    pub event_json: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_attestation: Option<MeshExportPolicyAttestation>,
    pub imported_at: String,
}

/// Stable failure classes for the checked-in `ee.mesh.event.v1` contract.
///
/// The variants deliberately carry no peer-controlled strings. Callers may
/// safely surface the class in policy/audit output without reflecting hostile
/// event bytes or identifiers back to an operator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshEventContractError {
    InvalidJson,
    InvalidSchema,
    UnsupportedRequiredFeature,
    EventHashMismatch,
    EventIdMismatch,
    OuterProjectionMismatch,
}

impl MeshEventContractError {
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::InvalidJson => "invalid_event_json",
            Self::InvalidSchema => "invalid_event_schema",
            Self::UnsupportedRequiredFeature => "unsupported_required_feature",
            Self::EventHashMismatch => "event_hash_mismatch",
            Self::EventIdMismatch => "event_id_mismatch",
            Self::OuterProjectionMismatch => "outer_event_projection_mismatch",
        }
    }
}

impl std::fmt::Display for MeshEventContractError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.reason())
    }
}

impl std::error::Error for MeshEventContractError {}

/// A schema-checked event projection whose `event_json` is canonical JSON.
///
/// `requires_unredacted_body_lane` marks schema-permitted, caller-controlled
/// fields that can carry arbitrary bytes (`trustClaim` values and
/// `bodyRef.uri`). A metadata-lane policy alone is insufficient for those
/// fields. Transfer callers must additionally require an unredacted body-lane
/// grant; the production outbound filter below does so fail-closed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalMeshEventProjection {
    pub event: MeshEventRow,
    pub requires_unredacted_body_lane: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CanonicalMeshEventV1 {
    schema: String,
    event_id: String,
    origin_node_id: String,
    origin_workspace_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    producer_peer_id: Option<String>,
    seq: u64,
    prev_event_hash: Option<String>,
    event_hash: String,
    event_kind: String,
    logical_memory_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    supersedes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tombstones: Option<Vec<String>>,
    content_hash: String,
    body_ref: Option<CanonicalMeshBodyRefV1>,
    material_lane: String,
    redaction_class: String,
    trust_lane: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    trust_claim: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    valid_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    valid_until: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_audit_hash: Option<String>,
    required_features: Vec<String>,
    produced_at: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CanonicalMeshBodyRefV1 {
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size_bytes: Option<u64>,
}

const MESH_EVENT_REQUIRED_FIELDS: [&str; 16] = [
    "schema",
    "eventId",
    "originNodeId",
    "originWorkspaceId",
    "seq",
    "prevEventHash",
    "eventHash",
    "eventKind",
    "logicalMemoryId",
    "contentHash",
    "bodyRef",
    "materialLane",
    "redactionClass",
    "trustLane",
    "requiredFeatures",
    "producedAt",
];

const MESH_EVENT_OPTIONAL_NON_NULL_FIELDS: [&str; 7] = [
    "producerPeerId",
    "supersedes",
    "tombstones",
    "trustClaim",
    "validFrom",
    "validUntil",
    "sourceAuditHash",
];

/// Parse, validate, hash-check, and canonicalize a file-replay mesh event.
///
/// The outer row is a database/export projection, not a second source of
/// truth. Every field duplicated by `ee.mesh.event.v1` must exactly match the
/// canonical inner event or the row is rejected. The returned row contains a
/// freshly serialized, recursively key-sorted `event_json`; unchecked input
/// bytes are never relayed by callers that use this projection.
///
/// Destination-local outer state is deliberately cleared. In particular, an
/// importing peer cannot choose a local memory/cache reference, replay a prior
/// policy verdict or attestation, or backdate the local import ledger. The
/// empty `imported_at` is an explicit "local assignment required" sentinel:
/// persistence callers must replace it with a locally generated timestamp (or
/// pass `None` to the database API so the writer generates one).
pub fn project_canonical_mesh_event(
    event: &MeshEventRow,
) -> Result<CanonicalMeshEventProjection, MeshEventContractError> {
    let raw: serde_json::Value =
        serde_json::from_str(&event.event_json).map_err(|_| MeshEventContractError::InvalidJson)?;
    let object = raw
        .as_object()
        .ok_or(MeshEventContractError::InvalidSchema)?;
    if MESH_EVENT_REQUIRED_FIELDS
        .iter()
        .any(|field| !object.contains_key(*field))
        || MESH_EVENT_OPTIONAL_NON_NULL_FIELDS
            .iter()
            .any(|field| object.get(*field).is_some_and(serde_json::Value::is_null))
        || object
            .get("bodyRef")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|body_ref| {
                ["previewHash", "uri", "sizeBytes"]
                    .iter()
                    .any(|field| body_ref.get(*field).is_some_and(serde_json::Value::is_null))
            })
    {
        return Err(MeshEventContractError::InvalidSchema);
    }

    let parsed = serde_json::from_value::<CanonicalMeshEventV1>(raw)
        .map_err(|_| MeshEventContractError::InvalidSchema)?;
    validate_canonical_mesh_event_schema(&parsed)?;

    let parsed_value =
        serde_json::to_value(&parsed).map_err(|_| MeshEventContractError::InvalidSchema)?;
    let expected_event_hash = canonical_mesh_event_hash(&parsed_value)?;
    if parsed.event_hash != expected_event_hash {
        return Err(MeshEventContractError::EventHashMismatch);
    }
    let expected_event_id = mesh_event_id_from_hash(&expected_event_hash)
        .ok_or(MeshEventContractError::InvalidSchema)?;
    if parsed.event_id != expected_event_id {
        return Err(MeshEventContractError::EventIdMismatch);
    }

    if parsed.event_id != event.event_id
        || parsed.origin_node_id != event.origin_node_id
        || parsed.origin_workspace_id != event.origin_workspace_id
        || parsed.producer_peer_id != event.producer_peer_id
        || parsed.seq != event.seq
        || parsed.prev_event_hash != event.prev_event_hash
        || parsed.event_hash != event.event_hash
        || parsed.event_kind != event.event_kind
        || parsed.logical_memory_id != event.logical_memory_id
        || parsed.content_hash != event.content_hash
        || parsed.material_lane != event.material_lane
        || parsed.redaction_class != event.redaction_class
        || parsed.trust_lane != event.trust_lane
    {
        return Err(MeshEventContractError::OuterProjectionMismatch);
    }

    let requires_unredacted_body_lane = parsed.trust_claim.is_some()
        || parsed
            .body_ref
            .as_ref()
            .and_then(|body_ref| body_ref.uri.as_ref())
            .is_some();
    let canonical_value = canonicalize_mesh_event_json(&parsed_value);
    let canonical_event_json = serde_json::to_string(&canonical_value)
        .map_err(|_| MeshEventContractError::InvalidSchema)?;
    let mut projected = event.clone();
    projected.event_json = canonical_event_json;
    projected.import_decision.clear();
    projected.local_memory_id = None;
    projected.body_cache_key = None;
    projected.policy_failure_surface_json = None;
    projected.policy_decision_json = None;
    projected.policy_attestation = None;
    projected.imported_at.clear();

    Ok(CanonicalMeshEventProjection {
        event: projected,
        requires_unredacted_body_lane,
    })
}

fn validate_canonical_mesh_event_schema(
    event: &CanonicalMeshEventV1,
) -> Result<(), MeshEventContractError> {
    let valid = event.schema == crate::models::MESH_EVENT_SCHEMA_V1
        && mesh_event_identifier_is_valid(&event.event_id, "mesh_evt_", 64, 64, true)
        && mesh_event_identifier_is_valid(&event.origin_node_id, "node_", 6, 128, false)
        && mesh_event_identifier_is_valid(&event.origin_workspace_id, "wsp_", 6, 128, false)
        && event
            .producer_peer_id
            .as_ref()
            .is_none_or(|peer_id| mesh_event_identifier_is_valid(peer_id, "peer_", 6, 128, false))
        && event.seq >= 1
        && event
            .prev_event_hash
            .as_ref()
            .is_none_or(|hash| mesh_event_hash_is_valid(hash))
        && mesh_event_hash_is_valid(&event.event_hash)
        && matches!(
            event.event_kind.as_str(),
            "create"
                | "revise"
                | "tombstone"
                | "shareWithdraw"
                | "trust"
                | "validity"
                | "bodyAvailable"
        )
        && mesh_event_identifier_is_valid(&event.logical_memory_id, "mem_", 6, 128, false)
        && event
            .supersedes
            .as_ref()
            .is_none_or(|ids| mesh_event_memory_id_set_is_valid(ids))
        && event
            .tombstones
            .as_ref()
            .is_none_or(|ids| mesh_event_memory_id_set_is_valid(ids))
        && mesh_event_hash_is_valid(&event.content_hash)
        && event
            .body_ref
            .as_ref()
            .is_none_or(mesh_event_body_ref_is_valid)
        && matches!(
            event.material_lane.as_str(),
            "metadata" | "body" | "embedding" | "graphLink" | "revisionNotice" | "curationSignal"
        )
        && matches!(
            event.redaction_class.as_str(),
            "metadataOnly" | "preview" | "body" | "embedding" | "secretDenied"
        )
        && matches!(
            event.trust_lane.as_str(),
            "localHuman" | "peerHumanViaPeer" | "peerAgent" | "peerDerived" | "untrusted"
        )
        && event.trust_claim.as_ref().is_none_or(|claim| {
            claim.values().all(|value| {
                matches!(
                    value,
                    serde_json::Value::Null
                        | serde_json::Value::Bool(_)
                        | serde_json::Value::Number(_)
                        | serde_json::Value::String(_)
                )
            })
        })
        && event
            .valid_from
            .as_deref()
            .is_none_or(mesh_event_rfc3339_is_valid)
        && event
            .valid_until
            .as_deref()
            .is_none_or(mesh_event_rfc3339_is_valid)
        && event
            .source_audit_hash
            .as_deref()
            .is_none_or(mesh_event_hash_is_valid)
        && mesh_event_required_features_are_well_formed(&event.required_features)
        && mesh_event_rfc3339_is_valid(&event.produced_at);
    if !valid {
        return Err(MeshEventContractError::InvalidSchema);
    }
    if event
        .required_features
        .iter()
        .any(|feature| feature != "mesh.event.v1")
    {
        return Err(MeshEventContractError::UnsupportedRequiredFeature);
    }
    Ok(())
}

fn mesh_event_identifier_is_valid(
    value: &str,
    prefix: &str,
    minimum_suffix_length: usize,
    maximum_suffix_length: usize,
    lowercase_hex_only: bool,
) -> bool {
    let Some(suffix) = value.strip_prefix(prefix) else {
        return false;
    };
    (minimum_suffix_length..=maximum_suffix_length).contains(&suffix.len())
        && suffix.bytes().all(|byte| {
            if lowercase_hex_only {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            } else {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-')
            }
        })
}

fn mesh_event_hash_is_valid(value: &str) -> bool {
    mesh_event_identifier_is_valid(value, "blake3:", 64, 64, true)
}

fn mesh_event_memory_id_set_is_valid(values: &[String]) -> bool {
    let mut unique = BTreeSet::new();
    values.iter().all(|value| {
        mesh_event_identifier_is_valid(value, "mem_", 6, 128, false)
            && unique.insert(value.as_str())
    })
}

fn mesh_event_body_ref_is_valid(body_ref: &CanonicalMeshBodyRefV1) -> bool {
    // Deserializing into `u64` already enforces the schema's integer and
    // non-negative constraints for `sizeBytes`; no additional upper bound is
    // published by v1.
    let _ = body_ref.size_bytes;
    matches!(
        body_ref.kind.as_str(),
        "none" | "inlinePreview" | "contentAddressed" | "remoteAvailable"
    ) && body_ref
        .preview_hash
        .as_deref()
        .is_none_or(mesh_event_hash_is_valid)
        && body_ref.uri.as_ref().is_none_or(|uri| {
            let character_count = uri.chars().count();
            (1..=512).contains(&character_count)
        })
}

fn mesh_event_required_features_are_well_formed(features: &[String]) -> bool {
    let mut unique = BTreeSet::new();
    features.len() <= 32
        && features.iter().all(|feature| {
            let Some(suffix) = feature.strip_prefix("mesh.") else {
                return false;
            };
            !suffix.is_empty()
                && feature.len() <= 64
                && suffix.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'.' | b'-')
                })
                && unique.insert(feature.as_str())
        })
}

fn mesh_event_rfc3339_is_valid(value: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(value).is_ok()
}

fn canonical_mesh_event_hash(event: &serde_json::Value) -> Result<String, MeshEventContractError> {
    let mut hashable = event.clone();
    let object = hashable
        .as_object_mut()
        .ok_or(MeshEventContractError::InvalidSchema)?;
    object.remove("eventHash");
    object.remove("eventId");
    let canonical = canonicalize_mesh_event_json(&hashable);
    let bytes =
        serde_json::to_vec(&canonical).map_err(|_| MeshEventContractError::InvalidSchema)?;
    Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
}

fn mesh_event_id_from_hash(event_hash: &str) -> Option<String> {
    event_hash
        .strip_prefix("blake3:")
        .map(|digest| format!("mesh_evt_{digest}"))
}

fn canonicalize_mesh_event_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(canonicalize_mesh_event_json).collect())
        }
        serde_json::Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut canonical = serde_json::Map::with_capacity(values.len());
            for key in keys {
                canonical.insert(key.clone(), canonicalize_mesh_event_json(&values[key]));
            }
            serde_json::Value::Object(canonical)
        }
        _ => value.clone(),
    }
}

/// Per-event verdict from applying a target peer's outbound policy to an
/// export artifact. Included in the artifact so an operator can audit exactly
/// which records were dropped or body-stripped and why.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshOutboundExportEventDecision {
    pub event_id: String,
    pub material_lane: String,
    pub origin_workspace_id: String,
    /// One of `allow`, `deny`, `reject`, `quarantine`.
    pub action: String,
    pub reason: String,
    pub event_dropped: bool,
    pub body_stripped: bool,
}

/// Result of applying outbound policy to export events: the retained (and where
/// necessary body-stripped) events plus the per-event decision ledger.
pub struct MeshOutboundExportFilter {
    pub events: Vec<MeshEventRow>,
    pub decisions: Vec<MeshOutboundExportEventDecision>,
}

/// Apply the target peer's metadata policy to the non-event portions of a
/// mesh artifact. Peer rows contain display/policy metadata and cursor rows
/// contain topology state; neither projection has passed a redaction
/// transform, so a `redact` requirement fails closed just like `deny`.
pub fn apply_outbound_artifact_metadata_policy(
    artifact: &mut MeshExportArtifact,
    registry: &MeshPeerPolicyRegistry,
    local_workspace_id: &str,
    target_peer_id: &str,
) {
    let peer_metadata_allowed = registry
        .decide_outbound(&MeshOutboundPolicyDecisionInput {
            local_workspace_id,
            target_peer_id,
            origin_workspace_id: local_workspace_id,
            material_lane: MeshLane::Metadata,
            payload_is_redacted: false,
        })
        .permits_payload_export();
    if !peer_metadata_allowed {
        artifact.peers.clear();
    }

    artifact.cursors.retain(|cursor| {
        registry
            .decide_outbound(&MeshOutboundPolicyDecisionInput {
                local_workspace_id,
                target_peer_id,
                origin_workspace_id: &cursor.origin_workspace_id,
                material_lane: MeshLane::Metadata,
                payload_is_redacted: false,
            })
            .permits_payload_export()
    });
}

fn mesh_export_redaction_is_redacted(redaction_class: &str) -> bool {
    matches!(redaction_class, "metadataOnly" | "preview" | "secretDenied")
}

/// Apply `target_peer_id`'s outbound policy to export events, per record and
/// lane. This is the first production caller of the outbound policy engine, so
/// `[[mesh.peer_policies]]` becomes load-bearing here:
///
/// - An event whose own material lane is not permitted for export to that peer
///   is dropped (its material never leaves).
/// - Destination-local outer references and verdicts are stripped by the
///   canonical projector regardless of lane policy. They are never transport
///   material; a stripped local body-cache key is reflected in the decision.
/// - An event whose lane string is unparseable is dropped fail-closed.
///
/// Missing or ambiguous policy for an event denies it (the registry's outbound
/// lookup is fail-closed), so an unknown peer strips everything rather than
/// leaking.
#[must_use]
pub fn apply_outbound_export_policy(
    events: Vec<MeshEventRow>,
    registry: &MeshPeerPolicyRegistry,
    local_workspace_id: &str,
    target_peer_id: &str,
) -> MeshOutboundExportFilter {
    let mut kept = Vec::new();
    let mut decisions = Vec::new();

    for candidate in events {
        let had_destination_local_body_reference = candidate.body_cache_key.is_some();
        let projection = match project_canonical_mesh_event(&candidate) {
            Ok(projection) => projection,
            Err(error) => {
                decisions.push(MeshOutboundExportEventDecision {
                    event_id: safe_rejected_mesh_event_id(&candidate.event_id),
                    material_lane: safe_rejected_mesh_material_lane(&candidate.material_lane),
                    origin_workspace_id: safe_rejected_mesh_workspace_id(
                        &candidate.origin_workspace_id,
                    ),
                    action: "reject".to_owned(),
                    reason: error.reason().to_owned(),
                    event_dropped: true,
                    body_stripped: false,
                });
                continue;
            }
        };
        let requires_unredacted_body_lane = projection.requires_unredacted_body_lane;
        let event = projection.event;
        let Some(lane) = parse_mesh_lane(&event.material_lane) else {
            decisions.push(MeshOutboundExportEventDecision {
                event_id: event.event_id.clone(),
                material_lane: event.material_lane.clone(),
                origin_workspace_id: event.origin_workspace_id.clone(),
                action: "reject".to_owned(),
                reason: "unparseable_material_lane".to_owned(),
                event_dropped: true,
                body_stripped: false,
            });
            continue;
        };

        let payload_is_redacted = mesh_export_redaction_is_redacted(&event.redaction_class);
        let decision = registry.decide_outbound(&MeshOutboundPolicyDecisionInput {
            local_workspace_id,
            target_peer_id,
            origin_workspace_id: &event.origin_workspace_id,
            material_lane: lane,
            payload_is_redacted,
        });

        if !decision.permits_payload_export() {
            decisions.push(MeshOutboundExportEventDecision {
                event_id: event.event_id.clone(),
                material_lane: event.material_lane.clone(),
                origin_workspace_id: event.origin_workspace_id.clone(),
                action: decision.action.as_str().to_owned(),
                reason: decision.reason.to_owned(),
                event_dropped: true,
                body_stripped: false,
            });
            continue;
        }

        // `trustClaim` and `bodyRef.uri` are schema-valid but can carry
        // arbitrary bytes. They may not tunnel through a metadata-only or
        // redact-only grant. Because these fields are hash-bound, stripping
        // them would create a different event; drop the whole event unless an
        // explicit unredacted body-lane grant permits the material.
        if requires_unredacted_body_lane {
            let body_material_decision =
                registry.decide_outbound(&MeshOutboundPolicyDecisionInput {
                    local_workspace_id,
                    target_peer_id,
                    origin_workspace_id: &event.origin_workspace_id,
                    material_lane: MeshLane::Body,
                    payload_is_redacted: false,
                });
            if !body_material_decision.permits_payload_export() {
                decisions.push(MeshOutboundExportEventDecision {
                    event_id: event.event_id.clone(),
                    material_lane: event.material_lane.clone(),
                    origin_workspace_id: event.origin_workspace_id.clone(),
                    action: body_material_decision.action.as_str().to_owned(),
                    reason: "event_metadata_requires_unredacted_body_lane".to_owned(),
                    event_dropped: true,
                    body_stripped: false,
                });
                continue;
            }
        }

        // `body_cache_key` is a destination-local lookup key, not transport
        // material. The canonical projector has already removed it under every
        // policy; retain only the safe boolean explaining that sanitization.
        let body_stripped = had_destination_local_body_reference;

        decisions.push(MeshOutboundExportEventDecision {
            event_id: event.event_id.clone(),
            material_lane: event.material_lane.clone(),
            origin_workspace_id: event.origin_workspace_id.clone(),
            action: decision.action.as_str().to_owned(),
            reason: decision.reason.to_owned(),
            event_dropped: false,
            body_stripped,
        });
        kept.push(event);
    }

    MeshOutboundExportFilter {
        events: kept,
        decisions,
    }
}

fn safe_rejected_mesh_event_id(value: &str) -> String {
    if mesh_event_identifier_is_valid(value, "mesh_evt_", 64, 64, true) {
        value.to_owned()
    } else {
        "mesh_evt_invalid_contract".to_owned()
    }
}

fn safe_rejected_mesh_material_lane(value: &str) -> String {
    if matches!(
        value,
        "metadata" | "body" | "embedding" | "graphLink" | "revisionNotice" | "curationSignal"
    ) {
        value.to_owned()
    } else {
        "invalid".to_owned()
    }
}

fn safe_rejected_mesh_workspace_id(value: &str) -> String {
    if mesh_event_identifier_is_valid(value, "wsp_", 6, 128, false) {
        value.to_owned()
    } else {
        "wsp_invalid_contract".to_owned()
    }
}

impl From<&StoredMeshImportLedgerEvent> for MeshEventRow {
    fn from(event: &StoredMeshImportLedgerEvent) -> Self {
        Self {
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
            policy_attestation: None,
            imported_at: event.imported_at.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshCliStatusReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub initialized: bool,
    pub mesh_enabled: bool,
    pub mode: String,
    pub posture: String,
    pub storage: MeshStorageCounts,
    pub selective_sync: SelectiveSyncStatusSummary,
    pub auto_enrollment: MeshAutoEnrollmentStatus,
    pub repair_commands: Vec<String>,
    pub degraded: Vec<MeshCliDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAutoEnrollmentStatus {
    pub schema: &'static str,
    pub enabled: bool,
    pub read_only: bool,
    pub tailscale: MeshAutoTailscaleStatus,
    pub hello_responder: MeshAutoHelloResponderStatus,
    pub discovery: TailscaleAutodiscoveryReport,
    pub discovery_cache: MeshAutoDiscoveryCacheStatus,
    pub materialized: Option<MeshAutoMaterializedStatus>,
    pub peer_state_breakdown: MeshAutoPeerStateBreakdown,
    pub drift: MeshAutoDriftStatus,
    pub steward_posture: MeshAutoStewardPosture,
    pub degraded: Vec<MeshCliDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAutoTailscaleStatus {
    pub schema: &'static str,
    pub status: String,
    pub authenticated: Option<bool>,
    pub shields_up: Option<bool>,
    pub binary_authentic: Option<bool>,
    pub tailnet_display_name: Option<String>,
    pub peer_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAutoHelloResponderStatus {
    pub schema: &'static str,
    pub status: String,
    pub running: Option<bool>,
    pub listen_addr: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAutoDiscoveryCacheStatus {
    pub schema: &'static str,
    pub status: String,
    pub ttl_seconds: u32,
    pub hit: Option<bool>,
    pub refreshed_at: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAutoMaterializedStatus {
    pub peer_group_id: String,
    pub peer_set_hash: String,
    pub peer_count: u32,
    pub lane_policy: MeshAutoLanePolicy,
    pub bound_tailnet_id: Option<String>,
    pub materialized_on_node_key: Option<String>,
    pub last_materialized_at: Option<String>,
    pub enrollment_source: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAutoLanePolicy {
    pub metadata: &'static str,
    pub body: &'static str,
    pub embedding: &'static str,
    pub graph_link: &'static str,
    pub revision_notice: &'static str,
    pub curation_signal: &'static str,
}

impl MeshAutoLanePolicy {
    const fn deny_all() -> Self {
        Self {
            metadata: "deny",
            body: "deny",
            embedding: "deny",
            graph_link: "deny",
            revision_notice: "deny",
            curation_signal: "deny",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAutoPeerStateBreakdown {
    pub liveness_status: &'static str,
    pub active: Option<u32>,
    pub soft_stale: Option<u32>,
    pub hard_stale: Option<u32>,
    pub denylisted: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAutoDriftStatus {
    pub new_peers_available: Vec<String>,
    pub new_peer_count: u32,
    pub disabled_peers_in_config: Vec<String>,
    pub transient_unreachable: Vec<String>,
    pub tailnet_changed: bool,
    pub node_key_changed: bool,
    pub manual_conflict_present: bool,
    pub drift_severity: &'static str,
    pub action_graph: RepairActionGraph,
    pub next_action_hint: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAutoStewardPosture {
    pub schema: &'static str,
    pub status: String,
    pub enabled: bool,
    pub last_reconciliation_at: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshCliPeersReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub workspace_id: String,
    pub peer_count: usize,
    pub cursor_count: usize,
    pub peers: Vec<MeshPeerRow>,
    pub cursors: Vec<MeshCursorRow>,
    pub degraded: Vec<MeshCliDegradation>,
}

/// Redaction-safe inspection projection for one locally recorded import.
///
/// The raw `event_json` and destination-local body-cache key intentionally do
/// not cross this surface. Operators get the chain, idempotency, provenance,
/// and receiver-local policy verdict needed to diagnose admission without
/// accidentally printing transported body material.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshImportLedgerEntry {
    pub event_id: String,
    pub origin_node_id: String,
    pub origin_workspace_id: String,
    pub producer_peer_id: Option<String>,
    pub seq: u64,
    pub prev_event_hash: Option<String>,
    pub event_hash: String,
    pub event_kind: String,
    pub logical_memory_id: String,
    pub content_hash: String,
    pub material_lane: String,
    pub redaction_class: String,
    pub trust_lane: String,
    pub import_decision: String,
    pub local_memory_id: Option<String>,
    pub policy_failure_surface: Option<serde_json::Value>,
    pub policy_decision: Option<serde_json::Value>,
    pub imported_at: String,
}

impl TryFrom<&MeshEventRow> for MeshImportLedgerEntry {
    type Error = serde_json::Error;

    fn try_from(event: &MeshEventRow) -> Result<Self, Self::Error> {
        Ok(Self {
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
            policy_failure_surface: event
                .policy_failure_surface_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            policy_decision: event
                .policy_decision_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            imported_at: event.imported_at.clone(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshImportLedgerReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub workspace_id: String,
    pub event_count: usize,
    pub events: Vec<MeshImportLedgerEntry>,
    pub degraded: Vec<MeshCliDegradation>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshExportArtifact {
    pub schema: String,
    pub workspace_id: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_attestation: Option<MeshExportPolicyAttestation>,
    pub storage: MeshStorageCounts,
    pub peers: Vec<MeshPeerRow>,
    pub cursors: Vec<MeshCursorRow>,
    pub events: Vec<MeshEventRow>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshCliExportReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub artifact_schema: &'static str,
    pub output_path: Option<String>,
    pub peer_count: usize,
    pub cursor_count: usize,
    pub event_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_id: Option<String>,
    pub secret_scan: MeshExportSecretScanReport,
    pub artifact: Option<MeshExportArtifact>,
    pub degraded: Vec<MeshCliDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshCliImportReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub source_path: String,
    pub dry_run: bool,
    pub peer_count: usize,
    pub cursor_count: usize,
    pub event_count: usize,
    pub imported_peer_count: usize,
    pub imported_cursor_count: usize,
    pub imported_event_count: usize,
    pub rejected_peer_count: usize,
    pub rejected_cursor_count: usize,
    pub degraded: Vec<MeshCliDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshCliSyncReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub once: bool,
    pub mode: String,
    pub contacted_peers: bool,
    pub supervisor: MeshSyncSupervisorReport,
    pub export_command: String,
    pub import_command: String,
    pub degraded: Vec<MeshCliDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshSyncSupervisorOptions {
    pub cadence_ms: u64,
    pub tick_limit: u32,
    pub peer_concurrency: u32,
    pub body_fetch_budget_bytes: u64,
    pub stale_read_window_ms: u64,
    pub time_budget_ms: u64,
}

impl Default for MeshSyncSupervisorOptions {
    fn default() -> Self {
        Self {
            cadence_ms: 0,
            tick_limit: 1,
            peer_concurrency: 1,
            body_fetch_budget_bytes: 64 * 1024,
            stale_read_window_ms: 5_000,
            time_budget_ms: FOREGROUND_SYNC_TIME_BUDGET_MS,
        }
    }
}

impl MeshSyncSupervisorOptions {
    #[must_use]
    pub fn config(&self) -> MeshSyncSupervisorConfig {
        MeshSyncSupervisorConfig {
            cadence_ms: self.cadence_ms,
            tick_limit: self.tick_limit,
            peer_concurrency: self.peer_concurrency,
            body_fetch_budget_bytes: self.body_fetch_budget_bytes,
            stale_read_window_ms: self.stale_read_window_ms,
            time_budget_ms: self.time_budget_ms,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshSyncSupervisorConfig {
    pub cadence_ms: u64,
    pub tick_limit: u32,
    pub peer_concurrency: u32,
    pub body_fetch_budget_bytes: u64,
    pub stale_read_window_ms: u64,
    pub time_budget_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshSyncBudgetStatus {
    pub time_budget_ms: u64,
    pub body_fetch_budget_bytes: u64,
    pub exhausted: bool,
    pub exhausted_resource: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshSyncBackpressureStatus {
    pub active_peer_count: usize,
    pub peer_concurrency: u32,
    pub queued_peer_count: usize,
    pub backpressured: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshSyncSupervisorTickReport {
    pub tick: u32,
    pub health: String,
    pub contacted_peers: bool,
    pub anti_entropy_summary_count: usize,
    pub replay_path: String,
    pub imported_event_count: u32,
    pub degraded: Vec<MeshCliDegradation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshSyncSupervisorReport {
    pub schema: &'static str,
    pub supervisor: &'static str,
    pub health: String,
    pub mode: String,
    pub daemonized: bool,
    pub config: MeshSyncSupervisorConfig,
    pub peer_count: usize,
    pub active_peer_count: usize,
    pub contacted_peers: bool,
    pub local_commands_blocked: bool,
    pub budget: MeshSyncBudgetStatus,
    pub backpressure: MeshSyncBackpressureStatus,
    pub ticks: Vec<MeshSyncSupervisorTickReport>,
    pub degraded: Vec<MeshCliDegradation>,
}

impl MeshSyncSupervisorReport {
    #[must_use]
    pub fn runtime_error(
        snapshot: &MeshForegroundSnapshot,
        options: &MeshSyncSupervisorOptions,
        message: &str,
    ) -> Self {
        let degraded = vec![MeshCliDegradation::sync_supervisor_runtime_error(message)];
        Self {
            schema: MESH_SYNC_SUPERVISOR_SCHEMA_V1,
            supervisor: "asupersync_foreground",
            health: "runtime_error".to_owned(),
            mode: snapshot.mode.clone(),
            daemonized: false,
            config: options.config(),
            peer_count: snapshot.peers.len(),
            active_peer_count: active_peer_count(snapshot),
            contacted_peers: false,
            local_commands_blocked: false,
            budget: MeshSyncBudgetStatus {
                time_budget_ms: options.time_budget_ms,
                body_fetch_budget_bytes: options.body_fetch_budget_bytes,
                exhausted: false,
                exhausted_resource: None,
            },
            backpressure: backpressure_status(snapshot, options),
            ticks: Vec::new(),
            degraded,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MeshForegroundSyncRequest<'a> {
    pub snapshot: &'a MeshForegroundSnapshot,
    pub options: &'a MeshSyncSupervisorOptions,
    pub peer: &'a MeshPeerRow,
    pub peer_record: &'a MeshPeerRecord,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MeshForegroundSyncPeerOutcome {
    pub contacted: bool,
    pub events_accepted: u64,
    pub events_duplicate: u64,
    pub events_forked: u64,
    pub ranges_requested: u64,
    pub ranges_fulfilled: u64,
    pub imported_event_count: u32,
}

pub trait MeshForegroundSyncTransport {
    fn contact_peer(
        &mut self,
        request: MeshForegroundSyncRequest<'_>,
    ) -> MeshForegroundSyncPeerOutcome;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopMeshForegroundSyncTransport;

impl MeshForegroundSyncTransport for NoopMeshForegroundSyncTransport {
    fn contact_peer(
        &mut self,
        _request: MeshForegroundSyncRequest<'_>,
    ) -> MeshForegroundSyncPeerOutcome {
        MeshForegroundSyncPeerOutcome::default()
    }
}

/// Real TCP contact for `ee mesh sync`. A successful bootstrap hello counts
/// as contact; event-range anti-entropy is a later T2.4 increment on this
/// same transport.
#[derive(Clone, Debug)]
pub struct TcpMeshForegroundSyncTransport {
    pub committed_port: u16,
    pub requester_node_key: String,
    pub requester_workspace_ids: Vec<String>,
    pub timeout: Duration,
}

/// Per-peer unsigned hello+sync timeout. Live Windows Tailscale soak
/// needed 4–8s after hello-responder restart; 750ms deferred the round.
pub const FOREGROUND_TCP_SYNC_TIMEOUT: Duration = Duration::from_secs(15);

/// Wall-clock budget for one Tailscale unsigned round plus slack.
pub const FOREGROUND_SYNC_TIME_BUDGET_MS: u64 = 20_000;

impl TcpMeshForegroundSyncTransport {
    #[must_use]
    pub fn from_snapshot(snapshot: &MeshForegroundSnapshot) -> Self {
        Self {
            committed_port: configured_hello_port(),
            requester_node_key: snapshot.workspace_id.clone(),
            requester_workspace_ids: vec![snapshot.workspace_id.clone()],
            timeout: FOREGROUND_TCP_SYNC_TIMEOUT,
        }
    }
}

impl MeshForegroundSyncTransport for TcpMeshForegroundSyncTransport {
    fn contact_peer(
        &mut self,
        request: MeshForegroundSyncRequest<'_>,
    ) -> MeshForegroundSyncPeerOutcome {
        let Some(address) =
            parse_live_peer_endpoint(&request.peer_record.endpoint.endpoint, self.committed_port)
        else {
            return MeshForegroundSyncPeerOutcome::default();
        };
        let hello = build_request(
            format!("sync:{}", request.peer.peer_id),
            self.requester_node_key.clone(),
            env!("CARGO_PKG_VERSION"),
            self.requester_workspace_ids.clone(),
            vec!["sync".to_owned()],
            Vec::new(),
        );
        let Ok(payload_bytes) = serialize_within_budget(&hello) else {
            return MeshForegroundSyncPeerOutcome::default();
        };
        let Ok(payload) = serde_json::from_slice(&payload_bytes) else {
            return MeshForegroundSyncPeerOutcome::default();
        };
        let sync_request =
            local_sync_round_request(request.snapshot, request.peer, request.peer_record);
        match exchange_live_mesh_round(address, self.timeout, payload, &sync_request) {
            Ok((_, sync)) => {
                let received = u32::try_from(sync.events.len()).unwrap_or(u32::MAX);
                let accepted = persist_sync_round_events(
                    request.snapshot,
                    &request.peer.peer_id,
                    &sync.events,
                );
                if accepted > 0 && accepted == received {
                    persist_sync_round_cursor(request.snapshot, &request.peer.peer_id, &sync);
                }
                MeshForegroundSyncPeerOutcome {
                    contacted: true,
                    events_accepted: u64::from(accepted),
                    ranges_requested: 1,
                    ranges_fulfilled: u64::from(!sync.events.is_empty()),
                    imported_event_count: received,
                    ..MeshForegroundSyncPeerOutcome::default()
                }
            }
            Err(_) => match exchange_bootstrap_hello(address, self.timeout, {
                serde_json::from_slice(&payload_bytes).unwrap_or_default()
            }) {
                Ok(_) => MeshForegroundSyncPeerOutcome {
                    contacted: true,
                    ..MeshForegroundSyncPeerOutcome::default()
                },
                Err(_) => MeshForegroundSyncPeerOutcome::default(),
            },
        }
    }
}

/// Authenticated EventFetch initiator used after a pair key is already
/// installed. Unsigned hello+sync remains the daemonless first-contact path.
pub async fn contact_authenticated_mesh_peer(
    cx: &Cx,
    address: std::net::SocketAddr,
    config: InitiatorSessionConfig,
    request: &SyncRoundRequest,
) -> Result<SyncRoundResponse, String> {
    let mut session = connect_authenticated_session(cx, address, config)
        .await
        .map_err(|error| error.to_string())?;
    let correlation_id = "sync-round-1".to_owned();
    let payload = serde_json::to_value(request).map_err(|error| error.to_string())?;
    session
        .send_request(
            cx,
            SessionMessage {
                correlation_id: correlation_id.clone(),
                capability: FrameCapability::EventFetch,
                requested_budget_ms: 10_000,
                payload,
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    let reply = session
        .receive_response(cx, &correlation_id)
        .await
        .map_err(|error| error.to_string())?;
    session.close();
    let bytes = serde_json::to_vec(&reply.payload).map_err(|error| error.to_string())?;
    parse_sync_round_response(&bytes)
        .ok_or_else(|| "authenticated peer did not return ee.mesh.sync_round.v1".to_owned())
}

/// Fetch one published body over an authenticated session.
pub async fn contact_authenticated_body_fetch(
    cx: &Cx,
    address: std::net::SocketAddr,
    config: InitiatorSessionConfig,
    body_cache_key: &str,
) -> Result<BodyFetchResponse, String> {
    let mut session = connect_authenticated_session(cx, address, config)
        .await
        .map_err(|error| error.to_string())?;
    let correlation_id = "body-fetch-1".to_owned();
    let payload = serde_json::to_value(&BodyFetchRequest {
        schema: BODY_FETCH_REQUEST_SCHEMA_V1.to_owned(),
        body_cache_key: body_cache_key.to_owned(),
    })
    .map_err(|error| error.to_string())?;
    session
        .send_request(
            cx,
            SessionMessage {
                correlation_id: correlation_id.clone(),
                capability: FrameCapability::BodyFetch,
                requested_budget_ms: 10_000,
                payload,
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    let reply = session
        .receive_response(cx, &correlation_id)
        .await
        .map_err(|error| error.to_string())?;
    session.close();
    serde_json::from_value(reply.payload)
        .map_err(|error| format!("authenticated peer did not return body fetch: {error}"))
}

/// Apply a token-free identity-attest frame over an authenticated session.
pub async fn contact_authenticated_identity_attest(
    cx: &Cx,
    address: std::net::SocketAddr,
    config: InitiatorSessionConfig,
    frame: &IdentityAttestFrameV1,
) -> Result<IdentityAttestFrameV1, String> {
    let mut session = connect_authenticated_session(cx, address, config)
        .await
        .map_err(|error| error.to_string())?;
    let correlation_id = "identity-attest-1".to_owned();
    let payload = serde_json::to_value(frame).map_err(|error| error.to_string())?;
    session
        .send_request(
            cx,
            SessionMessage {
                correlation_id: correlation_id.clone(),
                capability: FrameCapability::Extension("identity_attest".to_owned()),
                requested_budget_ms: 10_000,
                payload,
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    let reply = session
        .receive_response(cx, &correlation_id)
        .await
        .map_err(|error| error.to_string())?;
    session.close();
    serde_json::from_value(reply.payload)
        .map_err(|error| format!("authenticated peer did not return identity attest: {error}"))
}

fn local_sync_round_request(
    snapshot: &MeshForegroundSnapshot,
    peer: &MeshPeerRow,
    peer_record: &MeshPeerRecord,
) -> SyncRoundRequest {
    let mut tips = snapshot
        .cursors
        .iter()
        .filter(|cursor| cursor.peer_id == peer.peer_id)
        .map(|cursor| SyncRoundTip {
            origin_node_id: cursor.origin_node_id.clone(),
            origin_workspace_id: cursor.origin_workspace_id.clone(),
            last_seq: cursor.last_seq,
            tip_event_hash: cursor.tip_event_hash.clone(),
        })
        .collect::<Vec<_>>();
    let range_start = tips
        .iter()
        .map(|tip| tip.last_seq.saturating_add(1))
        .min()
        .unwrap_or(0);
    if tips.is_empty() {
        tips.push(SyncRoundTip {
            origin_node_id: peer.origin_node_id.clone(),
            origin_workspace_id: peer_record.workspace_id.clone(),
            last_seq: 0,
            tip_event_hash: None,
        });
    }
    SyncRoundRequest::new(tips, range_start, 512)
}

fn persist_sync_round_cursor(
    snapshot: &MeshForegroundSnapshot,
    peer_id: &str,
    sync: &SyncRoundResponse,
) {
    let Some(tip) = sync.tips.first().cloned().or_else(|| {
        sync.events.last().map(|event| SyncRoundTip {
            origin_node_id: event.origin_node_id.clone(),
            origin_workspace_id: event.origin_workspace_id.clone(),
            last_seq: event.seq,
            tip_event_hash: Some(event.event_hash.clone()),
        })
    }) else {
        return;
    };
    let Ok(connection) = DbConnection::open_file(&snapshot.database_path) else {
        return;
    };
    let _ = connection.upsert_mesh_peer_cursor(&UpsertMeshPeerCursorInput {
        workspace_id: snapshot.workspace_id.clone(),
        peer_id: peer_id.to_owned(),
        origin_node_id: tip.origin_node_id,
        origin_workspace_id: tip.origin_workspace_id,
        last_seq: tip.last_seq,
        tip_event_hash: tip.tip_event_hash,
        tip_audit_hash: None,
        status: "current".to_owned(),
        updated_at: None,
    });
}

fn ledger_event_id(raw_event_id: &str, event_hash: &str) -> String {
    if raw_event_id.starts_with("mesh_evt_") && raw_event_id.len() > 9 {
        return raw_event_id.to_owned();
    }
    let suffix = event_hash.trim_start_matches("blake3:");
    let compact = suffix.chars().take(24).collect::<String>();
    format!("mesh_evt_{compact}")
}

fn ledger_memory_id(event_hash: &str) -> String {
    let suffix = event_hash.trim_start_matches("blake3:");
    let compact = suffix.chars().take(24).collect::<String>();
    format!("mem_{compact}")
}

fn persist_sync_round_events(
    snapshot: &MeshForegroundSnapshot,
    producer_peer_id: &str,
    events: &[crate::mesh::bootstrap_envelope::SyncRoundEvent],
) -> u32 {
    if snapshot.database_path.is_empty() || events.is_empty() {
        return 0;
    }
    let Ok(connection) = DbConnection::open_file(&snapshot.database_path) else {
        return 0;
    };
    if matches!(
        crate::mesh::team::any_local_team_paused(&connection),
        Ok(true)
    ) {
        return 0;
    }
    let bindings = workspace_config(std::path::Path::new(&snapshot.workspace_path))
        .and_then(|config| config.mesh.peer_group_bindings)
        .unwrap_or_default();
    let mut imported = 0_u32;
    for event in events {
        let parsed = serde_json::from_str::<serde_json::Value>(&event.payload_json).ok();
        let event_validity = if parsed.is_some() {
            MeshEventValidity::Valid
        } else {
            MeshEventValidity::Malformed
        };
        if matches!(
            crate::mesh::team::origin_node_is_active_member(&connection, &event.origin_node_id),
            Ok(Some(false))
        ) {
            continue;
        }
        let inbound = serde_json::from_str::<crate::mesh::origin_stream::InboundOriginEvent>(
            &event.payload_json,
        )
        .ok();
        if let Some(inbound) = inbound.as_ref() {
            let own_origin = connection
                .list_all_team_members()
                .ok()
                .and_then(|members| {
                    members
                        .into_iter()
                        .find(|member| member.is_self)
                        .map(|member| member.origin_node_id)
                })
                .unwrap_or_default();
            let verifier = crate::mesh::team::TeamMemberKeyVerifier {
                connection: &connection,
            };
            match crate::mesh::origin_stream::ingest_origin_event(
                &connection,
                &verifier,
                &own_origin,
                &BTreeSet::new(),
                inbound,
                chrono::Utc::now().to_rfc3339().as_str(),
            ) {
                Ok(crate::mesh::origin_stream::IngestDisposition::Applied) => {}
                Ok(_) | Err(_) => continue,
            }
        }
        let decision = if bindings.is_empty() {
            None
        } else {
            Some(decide_mesh_import(
                &MeshImportDecisionInput {
                    local_workspace_id: snapshot.workspace_id.as_str(),
                    origin_workspace_id: event.origin_workspace_id.as_str(),
                    producer_peer_id,
                    material_lane: MeshLane::Metadata,
                    event_validity,
                },
                &bindings,
            ))
        };
        let (import_decision, may_append) = match decision.as_ref() {
            None if event_validity == MeshEventValidity::Valid => ("allow", true),
            None => ("reject", false),
            Some(decision) => (
                decision.workspace_scope_decision.as_str(),
                decision.workspace_scope_decision == MeshImportDecisionKind::Allow,
            ),
        };
        let raw_event_id = parsed
            .as_ref()
            .and_then(|value| value.get("eventId").and_then(serde_json::Value::as_str))
            .unwrap_or(event.event_hash.as_str());
        let event_id = ledger_event_id(raw_event_id, &event.event_hash);
        let prev_event_hash = parsed.as_ref().and_then(|value| {
            value
                .get("prevEventHash")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        });
        // Inbound material must not enter mesh_origin_events. That table is
        // origin-owned only; echoing a peer's chain would re-serve it as ours.
        if let Some(inbound) = inbound.as_ref()
            && may_append
        {
            let _ = crate::mesh::team::project_inbound_team_memory(
                &connection,
                snapshot.workspace_id.as_str(),
                inbound,
            );
        }
        if connection
            .insert_mesh_import_ledger_event(&InsertMeshImportLedgerEventInput {
                workspace_id: snapshot.workspace_id.clone(),
                event_id,
                origin_node_id: event.origin_node_id.clone(),
                origin_workspace_id: event.origin_workspace_id.clone(),
                producer_peer_id: Some(producer_peer_id.to_owned()),
                seq: event.seq.max(1),
                prev_event_hash,
                event_hash: event.event_hash.clone(),
                event_kind: "create".to_owned(),
                logical_memory_id: ledger_memory_id(&event.event_hash),
                content_hash: event.event_hash.clone(),
                material_lane: "metadata".to_owned(),
                redaction_class: "metadataOnly".to_owned(),
                trust_lane: "peerAgent".to_owned(),
                import_decision: import_decision.to_owned(),
                local_memory_id: None,
                body_cache_key: inbound.as_ref().and_then(|event| {
                    serde_json::from_value::<crate::mesh::origin_stream::MemoryEventPayload>(
                        event.payload.clone(),
                    )
                    .ok()
                    .filter(|payload| {
                        matches!(
                            payload.body_representation.as_deref(),
                            Some("exact" | "already_redacted")
                        ) && payload.logical_memory_id.starts_with("mem_")
                    })
                    .map(|payload| {
                        crate::mesh::team::team_body_cache_key(&payload.logical_memory_id)
                    })
                }),
                policy_failure_surface_json: None,
                policy_decision_json: None,
                event_json: event.payload_json.clone(),
                imported_at: None,
            })
            .is_ok()
            && may_append
        {
            imported = imported.saturating_add(1);
        }
    }
    imported
}

pub async fn run_mesh_sync_supervisor_supervised(
    cx: &Cx,
    snapshot: &MeshForegroundSnapshot,
    options: &MeshSyncSupervisorOptions,
) -> Outcome<MeshSyncSupervisorReport, String> {
    let mut transport = TcpMeshForegroundSyncTransport::from_snapshot(snapshot);
    run_mesh_sync_supervisor_supervised_with_transport(cx, snapshot, options, &mut transport).await
}

pub async fn run_mesh_sync_supervisor_supervised_with_transport(
    cx: &Cx,
    snapshot: &MeshForegroundSnapshot,
    options: &MeshSyncSupervisorOptions,
    transport: &mut impl MeshForegroundSyncTransport,
) -> Outcome<MeshSyncSupervisorReport, String> {
    if let Err(message) = validate_mesh_sync_supervisor_options(options) {
        return Outcome::Err(message);
    }
    if let Some(cancelled) = mesh_sync_checkpoint(cx) {
        return cancelled;
    }

    let active_peer_count = active_peer_count(snapshot);
    let peer_count = snapshot.peers.len();
    let backpressure = backpressure_status(snapshot, options);
    let budget = budget_status(active_peer_count, options);
    let mut degraded = mesh_sync_supervisor_degradations(&budget, &backpressure);
    let tick_capacity = match usize::try_from(options.tick_limit) {
        Ok(capacity) => capacity,
        Err(_) => {
            return Outcome::Err(
                "Mesh sync supervisor tick limit does not fit this platform".to_owned(),
            );
        }
    };
    let mut ticks = Vec::with_capacity(tick_capacity);
    let mut contacted_peers = false;

    for tick in 1..=options.tick_limit {
        if let Some(cancelled) = mesh_sync_checkpoint(cx) {
            return cancelled;
        }
        let round = run_mesh_sync_transport_round(snapshot, options, &budget, transport);
        contacted_peers |= round.contacted_peers;
        ticks.push(MeshSyncSupervisorTickReport {
            tick,
            health: supervisor_health(&budget, &backpressure, contacted_peers).to_owned(),
            contacted_peers: round.contacted_peers,
            anti_entropy_summary_count: round.anti_entropy_summary_count,
            replay_path: "mesh_import_replay".to_owned(),
            imported_event_count: round.imported_event_count,
            degraded: degraded.clone(),
        });

        if tick < options.tick_limit {
            if let Some(cancelled) =
                sleep_mesh_sync_supervisor_interval(cx, options.cadence_ms).await
            {
                return cancelled;
            }
        }
    }

    if snapshot.mesh_enabled && !contacted_peers {
        degraded.push(MeshCliDegradation::sync_once_network_deferred());
        // Active peers were enrolled, so a round was actually warranted:
        // report the transport gap at the anti-entropy level too, not just
        // the CLI deferral.
        if active_peer_count > 0 {
            degraded.push(MeshCliDegradation::anti_entropy_transport_unavailable(
                active_peer_count,
            ));
        }
    }

    Outcome::Ok(MeshSyncSupervisorReport {
        schema: MESH_SYNC_SUPERVISOR_SCHEMA_V1,
        supervisor: "asupersync_foreground",
        health: supervisor_health(&budget, &backpressure, contacted_peers).to_owned(),
        mode: snapshot.mode.clone(),
        daemonized: false,
        config: options.config(),
        peer_count,
        active_peer_count,
        contacted_peers,
        local_commands_blocked: false,
        budget,
        backpressure,
        ticks,
        degraded,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MeshForegroundSyncRound {
    contacted_peers: bool,
    anti_entropy_summary_count: usize,
    imported_event_count: u32,
}

impl MeshForegroundSyncRound {
    fn deferred(snapshot: &MeshForegroundSnapshot) -> Self {
        Self {
            contacted_peers: false,
            anti_entropy_summary_count: snapshot.cursors.len(),
            imported_event_count: snapshot.storage.imported_event_count,
        }
    }
}

fn apply_authenticated_sync_round(
    mut report: MeshSyncSupervisorReport,
    round: MeshForegroundSyncRound,
) -> MeshSyncSupervisorReport {
    report.contacted_peers = true;
    report.health = supervisor_health(&report.budget, &report.backpressure, true).to_owned();
    report.degraded.retain(|item| {
        item.code != MESH_SYNC_ONCE_NETWORK_DEFERRED_CODE
            && item.code
                != crate::mesh::anti_entropy_protocol::degraded_codes::TRANSPORT_UNAVAILABLE
    });
    if let Some(tick) = report.ticks.last_mut() {
        tick.contacted_peers = true;
        tick.health = report.health.clone();
        tick.imported_event_count = round.imported_event_count;
        tick.anti_entropy_summary_count = round.anti_entropy_summary_count;
        tick.degraded.retain(|item| {
            item.code != MESH_SYNC_ONCE_NETWORK_DEFERRED_CODE
                && item.code
                    != crate::mesh::anti_entropy_protocol::degraded_codes::TRANSPORT_UNAVAILABLE
        });
    }
    report
}

/// Build an authenticated initiator session from the local pair key plus
/// live LocalAPI identities. Team-join placeholders stay in the peer record
/// until WhoIs observes the real Tailscale IDs; the durable responder
/// handshake requires those observed IDs, not `tailnet-team-join`.
async fn live_team_initiator_config(
    cx: &Cx,
    snapshot: &MeshForegroundSnapshot,
    peer_id: &str,
    peer_origin_node_id: &str,
    peer_record: &MeshPeerRecord,
    address: SocketAddr,
) -> Option<InitiatorSessionConfig> {
    if snapshot.workspace_path.is_empty() || snapshot.database_path.is_empty() {
        return None;
    }
    let workspace = Path::new(&snapshot.workspace_path).canonicalize().ok()?;
    let database = Path::new(&snapshot.database_path).canonicalize().ok()?;
    let connection = DbConnection::open_file(&database).ok()?;
    let teams = crate::mesh::team::load_local_teams(&connection).ok()?;
    let team = teams.into_iter().next()?;
    let self_node = connection
        .list_all_team_members()
        .ok()?
        .into_iter()
        .find(|member| member.is_self)?
        .origin_node_id;
    if self_node.is_empty() || self_node == peer_origin_node_id {
        return None;
    }
    let store = MeshKeyStore::open_existing(&workspace).ok().flatten()?;
    let pair = store
        .load_pair_key(peer_id, PairKeyClass::Current)
        .ok()
        .flatten()?;
    let registrations = crate::mesh::responder_broker::plan_team_responder_registrations(
        &connection,
        &snapshot.workspace_id,
        &workspace,
        &database,
        configured_hello_port(),
    );
    let local_api = InboundLocalApi::prefer(&connection, &registrations, None)?;
    let local_status = local_api.local_status(cx).await.ok()?;
    let who_is = local_api.who_is(cx, address).await.ok()?;
    let local_address = ephemeral_source_for(address)?;
    Some(InitiatorSessionConfig {
        local_address,
        binding: SessionBinding {
            team_id: team.team_id,
            tailnet_id: local_status.identity.tailnet_id,
            initiator_node_id: self_node,
            responder_node_id: peer_origin_node_id.to_owned(),
            initiator_workspace_id: snapshot.workspace_id.clone(),
            responder_workspace_id: peer_record.origin_workspace_id.clone(),
            initiator_stable_id: local_status.identity.stable_id,
            responder_stable_id: who_is.stable_id,
            session_id: "replaced-by-connect".to_owned(),
        },
        pair_key: crate::mesh::key_store::SecretBytes::new(*pair.key.as_bytes()),
        pair_key_generation: pair.generation.get(),
        observations: HandshakeObservations {
            initiator_node_pubkey: local_status.identity.current_node_pubkey,
            responder_node_pubkey: who_is.current_node_pubkey,
        },
        capabilities: SessionCapabilities::base(),
        limits: SessionChannelLimits {
            connect_timeout: Duration::from_secs(8),
            io_timeout: Duration::from_secs(8),
            ..SessionChannelLimits::default()
        },
    })
}

async fn try_authenticated_team_sync_round(
    cx: &Cx,
    snapshot: &MeshForegroundSnapshot,
    options: &MeshSyncSupervisorOptions,
) -> Option<MeshForegroundSyncRound> {
    if !snapshot.mesh_enabled || snapshot.database_path.is_empty() {
        return None;
    }
    let peer_limit = usize::try_from(options.peer_concurrency).unwrap_or(usize::MAX);
    if peer_limit == 0 {
        return None;
    }
    let mut eligible_peers = snapshot
        .peers
        .iter()
        .filter_map(|peer| {
            if !peer.enabled {
                return None;
            }
            let peer_record = foreground_sync_peer_record(peer)?;
            if !foreground_sync_peer_allowed(peer, &peer_record) {
                return None;
            }
            Some((peer, peer_record))
        })
        .collect::<Vec<_>>();
    eligible_peers.sort_by(|left, right| left.0.peer_id.cmp(&right.0.peer_id));

    let mut peer_outcomes = Vec::new();
    let mut imported_event_count = snapshot.storage.imported_event_count;
    let mut contacted = false;
    for (peer, peer_record) in eligible_peers.into_iter().take(peer_limit) {
        let Some(address) =
            parse_live_peer_endpoint(&peer_record.endpoint.endpoint, configured_hello_port())
        else {
            continue;
        };
        let Some(config) = live_team_initiator_config(
            cx,
            snapshot,
            &peer.peer_id,
            &peer.origin_node_id,
            &peer_record,
            address,
        )
        .await
        else {
            continue;
        };
        let sync_request = local_sync_round_request(snapshot, peer, &peer_record);
        let Ok(sync) = contact_authenticated_mesh_peer(cx, address, config, &sync_request).await
        else {
            continue;
        };
        contacted = true;
        let received = u32::try_from(sync.events.len()).unwrap_or(u32::MAX);
        let accepted = persist_sync_round_events(snapshot, &peer.peer_id, &sync.events);
        if accepted > 0 && accepted == received {
            persist_sync_round_cursor(snapshot, &peer.peer_id, &sync);
        }
        imported_event_count = imported_event_count.saturating_add(received);
        let mut peer_outcome = MeshRoundPeerOutcome::new(&peer.peer_id);
        peer_outcome.events_accepted = u64::from(accepted);
        peer_outcome.ranges_requested = 1;
        peer_outcome.ranges_fulfilled = u64::from(!sync.events.is_empty());
        peer_outcomes.push(peer_outcome);
    }
    if !contacted {
        return None;
    }
    let summary = build_sync_summary(MeshSyncSummaryInput {
        last_round_completed_at: None,
        origins_tracked: snapshot.cursors.len(),
        peer_outcomes,
        retry_policy: MeshAntiEntropyRetryPolicy::default(),
        current_attempts: 0,
        next_retry_after: None,
        blocked_ranges: Vec::new(),
        degraded: Vec::new(),
    });
    Some(MeshForegroundSyncRound {
        contacted_peers: true,
        anti_entropy_summary_count: summary.peer_count.max(1),
        imported_event_count,
    })
}

fn run_mesh_sync_transport_round(
    snapshot: &MeshForegroundSnapshot,
    options: &MeshSyncSupervisorOptions,
    budget: &MeshSyncBudgetStatus,
    transport: &mut impl MeshForegroundSyncTransport,
) -> MeshForegroundSyncRound {
    if !snapshot.mesh_enabled || budget.exhausted {
        return MeshForegroundSyncRound::deferred(snapshot);
    }

    let peer_limit = usize::try_from(options.peer_concurrency).unwrap_or(usize::MAX);
    if peer_limit == 0 {
        return MeshForegroundSyncRound::deferred(snapshot);
    }

    let mut eligible_peers = snapshot
        .peers
        .iter()
        .filter_map(|peer| {
            if !peer.enabled {
                return None;
            }
            let peer_record = foreground_sync_peer_record(peer)?;
            if !foreground_sync_peer_allowed(peer, &peer_record) {
                return None;
            }
            Some((peer, peer_record))
        })
        .collect::<Vec<_>>();
    eligible_peers.sort_by(|left, right| left.0.peer_id.cmp(&right.0.peer_id));

    let mut peer_outcomes = Vec::new();
    let mut imported_event_count = snapshot.storage.imported_event_count;
    for (peer, peer_record) in eligible_peers.into_iter().take(peer_limit) {
        let outcome = transport.contact_peer(MeshForegroundSyncRequest {
            snapshot,
            options,
            peer,
            peer_record: &peer_record,
        });
        if !outcome.contacted {
            continue;
        }
        imported_event_count = imported_event_count.saturating_add(outcome.imported_event_count);
        let mut peer_outcome = MeshRoundPeerOutcome::new(&peer.peer_id);
        peer_outcome.events_accepted = outcome.events_accepted;
        peer_outcome.events_duplicate = outcome.events_duplicate;
        peer_outcome.events_forked = outcome.events_forked;
        peer_outcome.ranges_requested = outcome.ranges_requested;
        peer_outcome.ranges_fulfilled = outcome.ranges_fulfilled;
        peer_outcomes.push(peer_outcome);
    }

    let summary = build_sync_summary(MeshSyncSummaryInput {
        last_round_completed_at: None,
        origins_tracked: snapshot.cursors.len(),
        peer_outcomes,
        retry_policy: MeshAntiEntropyRetryPolicy::default(),
        current_attempts: 0,
        next_retry_after: None,
        blocked_ranges: Vec::new(),
        degraded: Vec::new(),
    });

    let contacted_peers = summary.peer_count > 0;
    MeshForegroundSyncRound {
        contacted_peers,
        anti_entropy_summary_count: if contacted_peers {
            summary.peer_count
        } else {
            snapshot.cursors.len()
        },
        imported_event_count,
    }
}

fn foreground_sync_peer_record(peer: &MeshPeerRow) -> Option<MeshPeerRecord> {
    let policy_summary_json = peer.policy_summary_json.as_deref()?;
    let value = serde_json::from_str::<serde_json::Value>(policy_summary_json).ok()?;
    if value.get("schema").and_then(serde_json::Value::as_str) != Some(MESH_PEER_RECORD_SCHEMA_V1) {
        return None;
    }
    serde_json::from_value(value).ok()
}

fn foreground_sync_peer_allowed(peer: &MeshPeerRow, record: &MeshPeerRecord) -> bool {
    peer.peer_id == record.peer_id
        && record.is_trusted()
        && !record.endpoint.endpoint.trim().is_empty()
        && record.capabilities.may_send.metadata
        && record.capabilities.may_receive.metadata
}

fn validate_mesh_sync_supervisor_options(
    options: &MeshSyncSupervisorOptions,
) -> Result<(), String> {
    if options.tick_limit == 0 {
        return Err("Mesh sync supervisor tick limit must be at least one".to_owned());
    }
    if options.tick_limit > MAX_MESH_SYNC_SUPERVISOR_TICKS {
        return Err(format!(
            "Mesh sync supervisor tick limit must be no greater than {MAX_MESH_SYNC_SUPERVISOR_TICKS}"
        ));
    }
    if options.cadence_ms > MAX_MESH_SYNC_SUPERVISOR_CADENCE_MS {
        return Err(format!(
            "Mesh sync supervisor cadence must be no greater than {MAX_MESH_SYNC_SUPERVISOR_CADENCE_MS} ms"
        ));
    }
    Ok(())
}

fn mesh_sync_checkpoint<T>(cx: &Cx) -> Option<Outcome<T, String>> {
    if cx.checkpoint().is_ok() {
        return None;
    }
    Some(Outcome::Cancelled(
        cx.cancel_reason()
            .unwrap_or_else(CancelReason::parent_cancelled),
    ))
}

async fn sleep_mesh_sync_supervisor_interval(
    cx: &Cx,
    cadence_ms: u64,
) -> Option<Outcome<MeshSyncSupervisorReport, String>> {
    if let Some(cancelled) = mesh_sync_checkpoint(cx) {
        return Some(cancelled);
    }
    if cadence_ms == 0 {
        yield_now().await;
        return mesh_sync_checkpoint(cx);
    }

    let mut remaining_ms = cadence_ms;
    while remaining_ms > 0 {
        if let Some(cancelled) = mesh_sync_checkpoint(cx) {
            return Some(cancelled);
        }
        let slice_ms = remaining_ms.min(MESH_SYNC_SUPERVISOR_SLEEP_SLICE_MS);
        asupersync_sleep(cx.now(), Duration::from_millis(slice_ms)).await;
        remaining_ms = remaining_ms.saturating_sub(slice_ms);
    }
    mesh_sync_checkpoint(cx)
}

fn active_peer_count(snapshot: &MeshForegroundSnapshot) -> usize {
    snapshot.peers.iter().filter(|peer| peer.enabled).count()
}

fn backpressure_status(
    snapshot: &MeshForegroundSnapshot,
    options: &MeshSyncSupervisorOptions,
) -> MeshSyncBackpressureStatus {
    let active_peer_count = active_peer_count(snapshot);
    let concurrency = usize::try_from(options.peer_concurrency).unwrap_or(usize::MAX);
    let queued_peer_count = active_peer_count.saturating_sub(concurrency);
    MeshSyncBackpressureStatus {
        active_peer_count,
        peer_concurrency: options.peer_concurrency,
        queued_peer_count,
        backpressured: queued_peer_count > 0,
    }
}

fn budget_status(
    active_peer_count: usize,
    options: &MeshSyncSupervisorOptions,
) -> MeshSyncBudgetStatus {
    let exhausted_resource = if options.time_budget_ms == 0 {
        Some("time".to_owned())
    } else if active_peer_count > 0 && options.peer_concurrency == 0 {
        Some("peerConcurrency".to_owned())
    } else if active_peer_count > 0 && options.body_fetch_budget_bytes == 0 {
        Some("bodyFetch".to_owned())
    } else {
        None
    };
    MeshSyncBudgetStatus {
        time_budget_ms: options.time_budget_ms,
        body_fetch_budget_bytes: options.body_fetch_budget_bytes,
        exhausted: exhausted_resource.is_some(),
        exhausted_resource,
    }
}

fn mesh_sync_supervisor_degradations(
    budget: &MeshSyncBudgetStatus,
    backpressure: &MeshSyncBackpressureStatus,
) -> Vec<MeshCliDegradation> {
    let mut degraded = Vec::new();
    if let Some(resource) = budget.exhausted_resource.as_deref() {
        degraded.push(MeshCliDegradation::sync_supervisor_budget_exhausted(
            resource,
            match resource {
                "time" => budget.time_budget_ms,
                "bodyFetch" => budget.body_fetch_budget_bytes,
                _ => u64::from(backpressure.peer_concurrency),
            },
        ));
    }
    if backpressure.backpressured {
        degraded.push(MeshCliDegradation::sync_supervisor_backpressure(
            backpressure.active_peer_count,
            backpressure.peer_concurrency,
        ));
    }
    degraded
}

fn supervisor_health(
    budget: &MeshSyncBudgetStatus,
    backpressure: &MeshSyncBackpressureStatus,
    contacted_peers: bool,
) -> &'static str {
    if budget.exhausted {
        "budget_exhausted"
    } else if backpressure.backpressured {
        "backpressured"
    } else if contacted_peers {
        "synced"
    } else {
        "deferred"
    }
}

#[derive(Clone, Debug)]
pub struct MeshForegroundSnapshot {
    pub workspace_id: String,
    pub workspace_path: String,
    pub database_path: String,
    pub initialized: bool,
    pub mesh_enabled: bool,
    pub mode: String,
    pub storage: MeshStorageCounts,
    pub peers: Vec<MeshPeerRow>,
    pub cursors: Vec<MeshCursorRow>,
    pub events: Vec<MeshEventRow>,
    pub degraded: Vec<MeshCliDegradation>,
}

impl MeshForegroundSnapshot {
    /// Build a daemon/CLI-free snapshot from a workspace and database path.
    pub fn from_paths(workspace_path: &Path, database_path: &Path) -> Result<Self, String> {
        let (mesh_enabled, mode) = mesh_enabled_and_mode(workspace_path);
        let workspace_path_string = workspace_path.display().to_string();
        if !database_path.is_file() {
            return Ok(Self {
                workspace_id: String::new(),
                workspace_path: workspace_path_string,
                database_path: database_path.display().to_string(),
                initialized: false,
                mesh_enabled,
                mode,
                storage: MeshStorageCounts::default(),
                peers: Vec::new(),
                cursors: Vec::new(),
                events: Vec::new(),
                degraded: Vec::new(),
            });
        }
        let connection = DbConnection::open_file(database_path)
            .map_err(|error| format!("open mesh store: {error}"))?;
        let (mesh_enabled, mode) = mesh_enabled_for_store(workspace_path, &connection);
        let workspace_id = resolve_store_workspace_id(&connection, workspace_path)?;
        let storage = connection
            .mesh_storage_status(&workspace_id)
            .map_err(|error| format!("mesh storage status: {error}"))?;
        let peers = connection
            .list_mesh_peers(&workspace_id)
            .map_err(|error| format!("list mesh peers: {error}"))?
            .iter()
            .map(Into::into)
            .collect();
        let cursors = connection
            .list_mesh_peer_cursors(&workspace_id)
            .map_err(|error| format!("list mesh cursors: {error}"))?
            .iter()
            .map(Into::into)
            .collect();
        let events = connection
            .list_mesh_import_ledger_events_for_workspace(&workspace_id)
            .map_err(|error| format!("list mesh import ledger: {error}"))?
            .iter()
            .map(Into::into)
            .collect();
        Ok(Self {
            workspace_id,
            workspace_path: workspace_path_string,
            database_path: database_path.display().to_string(),
            initialized: true,
            mesh_enabled,
            mode,
            storage: MeshStorageCounts::from(&storage),
            peers,
            cursors,
            events,
            degraded: Vec::new(),
        })
    }
}

fn mesh_explicitly_disabled(workspace_path: &Path) -> bool {
    if let Some(value) = read_env_var(EnvVar::MeshEnabled) {
        return matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        );
    }
    workspace_config(workspace_path).and_then(|config| config.mesh.enabled) == Some(false)
}

/// Team create/join is the inbound opt-in. Explicit `EE_MESH_ENABLED=0` or
/// `mesh.enabled = false` still wins.
#[must_use]
pub fn mesh_enabled_for_store(workspace_path: &Path, connection: &DbConnection) -> (bool, String) {
    let (configured_on, mode) = mesh_enabled_and_mode(workspace_path);
    if mesh_explicitly_disabled(workspace_path) {
        return (false, mode);
    }
    if configured_on {
        return (true, mode);
    }
    let has_team = crate::mesh::team::load_local_teams(connection)
        .ok()
        .is_some_and(|teams| !teams.is_empty());
    (has_team, mode)
}

fn mesh_enabled_and_mode(workspace_path: &Path) -> (bool, String) {
    let configured = workspace_config(workspace_path);
    let enabled = read_env_var(EnvVar::MeshEnabled)
        .as_deref()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or_else(|| {
            configured
                .as_ref()
                .and_then(|config| config.mesh.enabled)
                .unwrap_or(false)
        });
    let mode = read_env_var(EnvVar::MeshMode)
        .as_deref()
        .and_then(|value| value.trim().parse::<MeshCommandMode>().ok())
        .or_else(|| configured.and_then(|config| config.mesh.command_mode))
        .unwrap_or_default()
        .as_str()
        .to_owned();
    (enabled, mode)
}

/// Resolve the store's workspace id. Team members win over Windows path
/// spelling (`C:\` vs `\\?\C:\`) so mesh peers stay visible to CLI commands.
pub fn resolve_store_workspace_id(
    connection: &DbConnection,
    workspace_path: &Path,
) -> Result<String, String> {
    let row_id = resolve_workspace_row_id(connection, workspace_path)?;
    // Team members may record a path-hashed id that never landed in
    // `workspaces`. `mesh_peers.workspace_id` FKs that table, so an orphan
    // member id cannot host enrollments. Prefer the member id only when
    // the matching store row exists.
    if let Ok(members) = connection.list_all_team_members()
        && let Some(member) = members
            .iter()
            .find(|member| member.is_self)
            .or_else(|| members.first())
        && !member.workspace_id.is_empty()
        && connection
            .get_workspace(&member.workspace_id)
            .ok()
            .flatten()
            .is_some()
    {
        return Ok(member.workspace_id.clone());
    }
    Ok(row_id)
}

fn resolve_workspace_row_id(
    connection: &DbConnection,
    workspace_path: &Path,
) -> Result<String, String> {
    let primary = workspace_path.to_string_lossy().into_owned();
    if let Some(workspace) = connection
        .get_workspace_by_path(&primary)
        .map_err(|error| format!("query workspace: {error}"))?
    {
        return Ok(workspace.id);
    }
    let canonical = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let mut candidates = vec![canonical.to_string_lossy().into_owned()];
    if let Some(rest) = candidates[0].strip_prefix(r"\\?\UNC\") {
        candidates.push(format!(r"\\{rest}"));
    } else if let Some(rest) = candidates[0].strip_prefix(r"\\?\") {
        candidates.push(rest.to_owned());
    }
    for candidate in candidates {
        if candidate != primary
            && let Some(workspace) = connection
                .get_workspace_by_path(&candidate)
                .map_err(|error| format!("query workspace: {error}"))?
        {
            return Ok(workspace.id);
        }
    }
    let workspaces = connection
        .list_workspaces()
        .map_err(|error| format!("list workspaces: {error}"))?;
    if let [workspace] = workspaces.as_slice() {
        return Ok(workspace.id.clone());
    }
    Err(format!("workspace row missing for {primary}"))
}

/// One bounded sync tick for the team steward / daemon. Same supervisor as
/// `ee mesh sync --once`; no CLI stdout.
pub fn run_mesh_sync_once_from_paths(
    workspace_path: &Path,
    database_path: &Path,
) -> Result<MeshSyncSupervisorReport, String> {
    let snapshot = MeshForegroundSnapshot::from_paths(workspace_path, database_path)?;
    let options = MeshSyncSupervisorOptions::default();
    let runtime =
        crate::core::build_cli_runtime().map_err(|error| format!("mesh sync runtime: {error}"))?;
    let join = runtime
        .handle()
        .try_spawn(async move {
            let Some(cx) = Cx::current() else {
                return Outcome::Err("mesh sync started without an ambient Cx".to_owned());
            };
            run_mesh_sync_supervisor_supervised(&cx, &snapshot, &options).await
        })
        .map_err(|error| format!("spawn mesh sync: {error}"))?;
    match runtime.block_on(join) {
        Outcome::Ok(report) => Ok(complete_team_sync_after_unsigned(
            workspace_path,
            database_path,
            report,
        )),
        Outcome::Err(message) => Err(message),
        Outcome::Cancelled(reason) => Err(format!(
            "mesh sync cancelled: {}",
            crate::core::outcome::cancel_message(&reason)
        )),
        Outcome::Panicked(payload) => Err(format!("mesh sync panicked: {payload}")),
    }
}

/// Same-thread EventFetch + BodyFetch after the Send supervisor returns.
/// LocalAPI and pair-key sessions are `!Send`; `block_on` here does not
/// require Send, unlike `try_spawn` of the unsigned supervisor.
#[must_use]
pub fn complete_team_sync_after_unsigned(
    workspace_path: &Path,
    database_path: &Path,
    report: MeshSyncSupervisorReport,
) -> MeshSyncSupervisorReport {
    let report = if report.contacted_peers {
        report
    } else if let Ok(snapshot) = MeshForegroundSnapshot::from_paths(workspace_path, database_path) {
        finish_authenticated_team_sync(snapshot, report)
    } else {
        report
    };
    let _ = fetch_pending_team_bodies_from_paths(workspace_path, database_path);
    report
}

fn finish_authenticated_team_sync(
    snapshot: MeshForegroundSnapshot,
    report: MeshSyncSupervisorReport,
) -> MeshSyncSupervisorReport {
    if !snapshot.mesh_enabled {
        return report;
    }
    let Ok(runtime) = crate::core::build_cli_runtime() else {
        return report;
    };
    let options = MeshSyncSupervisorOptions::default();
    let auth = runtime.block_on(async move {
        let Some(cx) = Cx::current() else {
            return None;
        };
        try_authenticated_team_sync_round(&cx, &snapshot, &options).await
    });
    match auth {
        Some(round) => apply_authenticated_sync_round(report, round),
        None => report,
    }
}

/// Start the inbound team responder in a current-thread owner if enrolled
/// peers exist. Returns false when mesh is off, disabled, or no route exists.
#[must_use]
pub fn spawn_team_responder_owner_if_needed(workspace_path: &Path, database_path: &Path) -> bool {
    if read_env_var(EnvVar::MeshHelloResponderDisabled)
        .as_deref()
        .and_then(parse_env_bool_flag)
        .unwrap_or(false)
    {
        return false;
    }
    let Ok(workspace_path) = workspace_path.canonicalize() else {
        return false;
    };
    let Ok(database_path) = database_path.canonicalize() else {
        return false;
    };
    if !database_path.is_file() {
        return false;
    }
    let Ok(snapshot) = MeshForegroundSnapshot::from_paths(&workspace_path, &database_path) else {
        return false;
    };
    if !snapshot.initialized || !snapshot.mesh_enabled {
        return false;
    }
    let Ok(connection) = DbConnection::open_file(&database_path) else {
        return false;
    };
    let registrations = crate::mesh::responder_broker::plan_team_responder_registrations(
        &connection,
        &snapshot.workspace_id,
        &workspace_path,
        &database_path,
        configured_hello_port(),
    );
    if registrations.is_empty() {
        return false;
    }
    let Some(local_api) =
        crate::mesh::responder_broker::InboundLocalApi::prefer(&connection, &registrations, None)
    else {
        return false;
    };
    std::thread::Builder::new()
        .name("ee-team-responder".to_owned())
        .spawn(move || {
            let _ = crate::core::run_cli_with_cx(
                Duration::from_secs(365 * 24 * 60 * 60),
                |cx| async move {
                    let mut owner =
                        crate::mesh::responder_broker::ResponderBrokerOwner::start_durable(
                            &cx,
                            local_api,
                            registrations,
                            crate::mesh::responder_broker::PreAuthAdmissionLimits::default(),
                            Duration::from_millis(2_000),
                        )
                        .await?;
                    owner.serve_until_cancelled(&cx).await
                },
            );
        })
        .is_ok()
}

/// Team-join enrolls `body_allowed` on the peer record. EventFetch is
/// ungranted; BodyFetch uses that capability instead of waiting for a
/// second durable lane-grant row.
fn team_join_body_fetch_allowed(record: &MeshPeerRecord) -> bool {
    crate::mesh::team::team_join_allows_ungranted_route(record)
        && record.capabilities.may_receive.body
}

/// Grant-gated BodyFetch after the Send supervisor returns. Pair-key
/// sessions are `!Send`; `block_on` on this thread does not require Send.
pub fn fetch_pending_team_bodies_from_paths(workspace_path: &Path, database_path: &Path) -> usize {
    let Ok(snapshot) = MeshForegroundSnapshot::from_paths(workspace_path, database_path) else {
        return 0;
    };
    if !snapshot.initialized || snapshot.database_path.is_empty() {
        return 0;
    }
    let Ok(runtime) = crate::core::build_cli_runtime() else {
        return 0;
    };
    runtime.block_on(async move {
        let Some(cx) = Cx::current() else {
            return 0_usize;
        };
        fetch_pending_team_bodies_after_sync(&cx, &snapshot).await
    })
}

async fn fetch_pending_team_bodies_after_sync(cx: &Cx, snapshot: &MeshForegroundSnapshot) -> usize {
    let Ok(connection) = DbConnection::open_file(&snapshot.database_path) else {
        return 0;
    };
    let Ok(keys) =
        crate::mesh::team::pending_team_body_fetch_keys(&connection, &snapshot.workspace_id)
    else {
        return 0;
    };
    if keys.is_empty() {
        return 0;
    }
    let Ok(teams) = crate::mesh::team::load_local_teams(&connection) else {
        return 0;
    };
    let Some(team) = teams.into_iter().next() else {
        return 0;
    };
    let workspace_path = Path::new(&snapshot.workspace_path);
    let Ok(peers) = connection.list_mesh_peers(&snapshot.workspace_id) else {
        return 0;
    };
    let mut applied = 0_usize;
    for key in keys {
        for peer in &peers {
            let Some(record) = peer
                .policy_summary_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<MeshPeerRecord>(json).ok())
            else {
                continue;
            };
            if !peer.enabled
                || !(crate::mesh::team::body_lane_allows_fetch(
                    &connection,
                    &snapshot.workspace_id,
                    &peer.peer_id,
                ) || team_join_body_fetch_allowed(&record))
            {
                continue;
            }
            let Some(binding) = crate::mesh::team::plan_team_body_fetch_binding(
                &snapshot.workspace_id,
                &team.origin_node_id,
                &team.team_id,
                &peer.origin_node_id,
                &record.origin_workspace_id,
                &record.endpoint.tailnet_id,
            ) else {
                continue;
            };
            let Some(address) =
                parse_live_peer_endpoint(&record.endpoint.endpoint, configured_hello_port())
            else {
                continue;
            };
            let Some(config) = live_team_initiator_config(
                cx,
                snapshot,
                &peer.peer_id,
                &peer.origin_node_id,
                &record,
                address,
            )
            .await
            else {
                continue;
            };
            if config.binding.team_id != binding.team_id
                || config.binding.initiator_node_id != binding.initiator_node_id
                || config.binding.responder_node_id != binding.responder_node_id
            {
                continue;
            }
            let Ok(fetched) = contact_authenticated_body_fetch(cx, address, config, &key).await
            else {
                continue;
            };
            if crate::mesh::team::apply_fetched_team_body(
                &connection,
                &snapshot.workspace_id,
                workspace_path,
                &fetched,
            )
            .is_ok_and(|published| published.cache_status == "available")
            {
                applied = applied.saturating_add(1);
                break;
            }
        }
    }
    applied
}

/// Concrete same-family source for an authenticated connect. Loopback stays
/// on loopback. Routed remotes use a UDP connect to pick the local IP, then
/// bind TCP with an ephemeral port.
#[must_use]
pub fn ephemeral_source_for(remote: SocketAddr) -> Option<SocketAddr> {
    if remote.ip().is_unspecified() {
        return None;
    }
    if remote.ip().is_loopback() {
        return if remote.is_ipv4() {
            "127.0.0.1:0".parse().ok()
        } else {
            "[::1]:0".parse().ok()
        };
    }
    let bind = if remote.is_ipv4() {
        SocketAddr::from(([0, 0, 0, 0], 0))
    } else {
        SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 0], 0))
    };
    let socket = UdpSocket::bind(bind).ok()?;
    socket.connect(remote).ok()?;
    let local = socket.local_addr().ok()?;
    if local.ip().is_unspecified() || local.is_ipv4() != remote.is_ipv4() {
        return None;
    }
    Some(SocketAddr::new(local.ip(), 0))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshCheckedExportArtifact {
    pub artifact: MeshExportArtifact,
    pub secret_scan: MeshExportSecretScanReport,
}

#[derive(Clone, Debug)]
struct MeshAutoStatusSignals {
    tailscale_authenticated: Option<bool>,
    tailscale_shields_up: Option<bool>,
    tailscale_binary_authentic: Option<bool>,
    tailnet_display_name: Option<String>,
    tailscale_peer_count: u32,
    tailscale_authenticated_for_24h: bool,
    hello_responder_running: Option<bool>,
    discovered_peer_count: u32,
    discovery: Option<TailscaleAutodiscoveryReport>,
    lane_policy: MeshAutoLanePolicy,
    new_peers_available: Vec<String>,
    transient_unreachable: Vec<String>,
    denylisted_peer_count: u32,
    tailscale_degraded: Vec<MeshCliDegradation>,
    tailnet_changed: bool,
    node_key_changed: bool,
    manual_conflict_present: bool,
}

impl Default for MeshAutoStatusSignals {
    fn default() -> Self {
        Self {
            tailscale_authenticated: None,
            tailscale_shields_up: None,
            tailscale_binary_authentic: None,
            tailnet_display_name: None,
            tailscale_peer_count: 0,
            tailscale_authenticated_for_24h: false,
            hello_responder_running: None,
            discovered_peer_count: 0,
            discovery: None,
            lane_policy: MeshAutoLanePolicy::deny_all(),
            new_peers_available: Vec::new(),
            transient_unreachable: Vec::new(),
            denylisted_peer_count: 0,
            tailscale_degraded: Vec::new(),
            tailnet_changed: false,
            node_key_changed: false,
            manual_conflict_present: false,
        }
    }
}

impl MeshForegroundSnapshot {
    #[must_use]
    pub fn status_report(&self) -> MeshCliStatusReport {
        let posture = if !self.initialized {
            "uninitialized"
        } else if !self.mesh_enabled {
            "disabled_local_only"
        } else if self.storage.imported_event_count > 0 || self.storage.peer_count > 0 {
            "enabled_with_local_cache"
        } else {
            "enabled_empty"
        };

        MeshCliStatusReport {
            schema: MESH_CLI_STATUS_SCHEMA_V1,
            command: "mesh status",
            workspace_id: self.workspace_id.clone(),
            workspace_path: self.workspace_path.clone(),
            database_path: self.database_path.clone(),
            initialized: self.initialized,
            mesh_enabled: self.mesh_enabled,
            mode: self.mode.clone(),
            posture: posture.to_owned(),
            storage: self.storage.clone(),
            selective_sync: SelectiveSyncConfig::safe_starter_config().summary(),
            auto_enrollment: auto_enrollment_status_for_snapshot(
                self,
                MeshAutoStatusSignals::default(),
            ),
            repair_commands: vec![
                format!(
                    "ee init --workspace {} --json",
                    mesh_shell_quote_arg(&self.workspace_path)
                ),
                format!(
                    "ee mesh export --workspace {} --peer <peer-id> --out mesh-export.json --json",
                    mesh_shell_quote_arg(&self.workspace_path)
                ),
                format!(
                    "ee mesh import --workspace {} --file mesh-export.json --json",
                    mesh_shell_quote_arg(&self.workspace_path)
                ),
            ],
            degraded: self.degraded.clone(),
        }
    }

    #[must_use]
    pub fn status_report_with_autodiscovery(
        &self,
        autodiscovery: &TailscaleAutodiscoveryReport,
        local: Option<&TailscaleLocalReport>,
        policy_registry: &MeshPeerPolicyRegistry,
        discovery_denylist: &BTreeSet<String>,
    ) -> MeshCliStatusReport {
        let mut report = self.status_report();
        report.auto_enrollment = auto_enrollment_status_for_snapshot(
            self,
            auto_status_signals_from_autodiscovery(
                self,
                autodiscovery,
                local,
                policy_registry,
                discovery_denylist,
            ),
        );
        report
    }

    #[must_use]
    pub fn peers_report(&self) -> MeshCliPeersReport {
        MeshCliPeersReport {
            schema: MESH_CLI_PEERS_SCHEMA_V1,
            command: "mesh peers",
            workspace_id: self.workspace_id.clone(),
            peer_count: self.peers.len(),
            cursor_count: self.cursors.len(),
            peers: self.peers.clone(),
            cursors: self.cursors.clone(),
            degraded: self.degraded.clone(),
        }
    }

    /// Build the stable, redaction-safe import-ledger inspection contract.
    pub fn import_ledger_report(&self) -> Result<MeshImportLedgerReport, serde_json::Error> {
        let events = self
            .events
            .iter()
            .map(MeshImportLedgerEntry::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(MeshImportLedgerReport {
            schema: MESH_IMPORT_LEDGER_SCHEMA_V1,
            command: "mesh ledger",
            workspace_id: self.workspace_id.clone(),
            event_count: events.len(),
            events,
            degraded: self.degraded.clone(),
        })
    }

    #[must_use]
    pub fn export_artifact(&self) -> MeshExportArtifact {
        MeshExportArtifact {
            schema: MESH_EXPORT_ARTIFACT_SCHEMA_V1.to_owned(),
            workspace_id: self.workspace_id.clone(),
            source: "ee mesh export".to_owned(),
            policy_attestation: None,
            storage: self.storage.clone(),
            peers: self.peers.clone(),
            cursors: self.cursors.clone(),
            events: self.events.clone(),
        }
    }

    pub fn checked_export_artifact(
        &self,
    ) -> Result<MeshCheckedExportArtifact, MeshExportSecretScanReport> {
        let mut artifact = self.export_artifact();
        let subjects = mesh_export_secret_subjects(&artifact);
        let secret_scan = scan_mesh_export_subjects(&subjects);
        if secret_scan.denied() {
            return Err(secret_scan);
        }

        artifact.policy_attestation = Some(secret_scan.allowed_attestation());
        for event in &mut artifact.events {
            event.policy_attestation = Some(MeshExportPolicyAttestation::allowed(
                mesh_event_secret_subjects(event).len() as u32,
            ));
        }
        tracing::info!(
            event = "attestation_recorded",
            surface = "mesh export",
            scanned_field_count = secret_scan.scanned_field_count,
            event_count = artifact.events.len()
        );
        Ok(MeshCheckedExportArtifact {
            artifact,
            secret_scan,
        })
    }
}

fn auto_enrollment_status_for_snapshot(
    snapshot: &MeshForegroundSnapshot,
    signals: MeshAutoStatusSignals,
) -> MeshAutoEnrollmentStatus {
    let disabled_peers_in_config = disabled_mesh_peer_ids(snapshot);
    let new_peer_count = u32::try_from(signals.new_peers_available.len()).unwrap_or(u32::MAX);
    let drift_severity = auto_drift_severity(snapshot, &signals, new_peer_count);
    let action_graph = auto_status_action_graph(snapshot, &signals);
    let next_action_hint = auto_status_next_action_hint(snapshot, &signals, new_peer_count);
    let mut degraded = snapshot.degraded.clone();
    degraded.extend(auto_status_degradations(snapshot, &signals));
    degraded.extend(signals.tailscale_degraded.clone());

    MeshAutoEnrollmentStatus {
        schema: MESH_AUTO_STATUS_SCHEMA_V2,
        enabled: snapshot.mesh_enabled,
        read_only: true,
        tailscale: MeshAutoTailscaleStatus {
            schema: "ee.tailscale.local.v1",
            status: match signals.tailscale_authenticated {
                Some(true) => "authenticated",
                Some(false) => "not_authenticated",
                None => "not_probed",
            }
            .to_owned(),
            authenticated: signals.tailscale_authenticated,
            shields_up: signals.tailscale_shields_up,
            binary_authentic: signals.tailscale_binary_authentic,
            tailnet_display_name: signals.tailnet_display_name.clone(),
            peer_count: signals.tailscale_peer_count,
        },
        hello_responder: MeshAutoHelloResponderStatus {
            schema: "ee.mesh.hello_responder.status.v1",
            status: match signals.hello_responder_running {
                Some(true) => "running",
                Some(false) => "not_running",
                None => "not_probed",
            }
            .to_owned(),
            running: signals.hello_responder_running,
            listen_addr: None,
        },
        discovery: signals
            .discovery
            .clone()
            .unwrap_or_else(|| auto_status_discovery_report(&signals)),
        discovery_cache: MeshAutoDiscoveryCacheStatus {
            schema: "ee.mesh.discovery_cache.status.v1",
            status: "not_probed_in_this_mode".to_owned(),
            ttl_seconds: 30,
            hit: None,
            refreshed_at: None,
        },
        materialized: auto_materialized_status(snapshot, &signals.lane_policy),
        peer_state_breakdown: MeshAutoPeerStateBreakdown {
            liveness_status: "not_probed_in_this_mode",
            active: None,
            soft_stale: None,
            hard_stale: None,
            denylisted: signals.denylisted_peer_count,
        },
        drift: MeshAutoDriftStatus {
            new_peers_available: signals.new_peers_available.clone(),
            new_peer_count,
            disabled_peers_in_config,
            transient_unreachable: signals.transient_unreachable.clone(),
            tailnet_changed: signals.tailnet_changed,
            node_key_changed: signals.node_key_changed,
            manual_conflict_present: signals.manual_conflict_present,
            drift_severity,
            action_graph,
            next_action_hint,
        },
        steward_posture: MeshAutoStewardPosture {
            schema: "ee.mesh.steward_posture.v1",
            status: "not_probed_in_this_mode".to_owned(),
            enabled: false,
            last_reconciliation_at: None,
        },
        degraded,
    }
}

fn auto_status_discovery_report(signals: &MeshAutoStatusSignals) -> TailscaleAutodiscoveryReport {
    TailscaleAutodiscoveryReport {
        schema: TAILSCALE_AUTODISCOVERY_SCHEMA_V1,
        tailnet_id: None,
        tailnet_display_name: None,
        self_node_key: None,
        probed_peer_count: 0,
        eligible_peer_count: 0,
        ee_capable_peers: Vec::new(),
        skipped_peers: Vec::new(),
        degraded: vec![TailscaleAutodiscoveryDegradation::new(
            TAILSCALE_PEER_LIST_UNAVAILABLE_CODE,
            "warning",
            if signals.tailscale_authenticated.is_none() {
                "Tailscale peer list was unavailable because the local probe did not run."
            } else {
                "Tailscale peer autodiscovery was skipped from the read-only status path."
            },
            "Enable mesh probing with EE_MESH_ENABLED=1 and re-run `ee mesh status --json`.",
        )],
    }
}

fn auto_status_signals_from_autodiscovery(
    snapshot: &MeshForegroundSnapshot,
    autodiscovery: &TailscaleAutodiscoveryReport,
    local: Option<&TailscaleLocalReport>,
    policy_registry: &MeshPeerPolicyRegistry,
    discovery_denylist: &BTreeSet<String>,
) -> MeshAutoStatusSignals {
    let identity_guard = auto_status_identity_guard(snapshot, autodiscovery);
    let materialized_node_keys = materialized_mesh_node_keys(snapshot);
    let denylisted_peer_count = u32::try_from(
        discovery_denylist
            .intersection(&materialized_node_keys)
            .count(),
    )
    .unwrap_or(u32::MAX);
    let mut new_peers_available = autodiscovery
        .ee_capable_peers
        .iter()
        .filter(|peer| !materialized_node_keys.contains(&peer.node_key))
        .map(|peer| peer.node_key.clone())
        .collect::<Vec<_>>();
    new_peers_available.sort();
    new_peers_available.dedup();
    let mut transient_unreachable = autodiscovery
        .skipped_peers
        .iter()
        .filter(|peer| peer.reason == "probe_timeout")
        .map(|peer| peer.node_key.clone())
        .collect::<Vec<_>>();
    transient_unreachable.sort();
    transient_unreachable.dedup();
    MeshAutoStatusSignals {
        tailscale_authenticated: local.map(|report| report.authenticated),
        tailscale_shields_up: local.and_then(|report| report.shields_up),
        tailscale_binary_authentic: local.map(|report| report.binary_authentic),
        tailnet_display_name: local.and_then(|report| report.tailnet_display_name.clone()),
        tailscale_peer_count: local.map_or(0, |report| {
            u32::try_from(report.peers.len()).unwrap_or(u32::MAX)
        }),
        discovered_peer_count: autodiscovery.eligible_peer_count,
        discovery: Some(autodiscovery.clone()),
        lane_policy: effective_auto_lane_policy(snapshot, policy_registry),
        new_peers_available,
        transient_unreachable,
        denylisted_peer_count,
        tailscale_degraded: local.map_or_else(Vec::new, |report| {
            report
                .degradations
                .iter()
                .map(|item| MeshCliDegradation {
                    code: item.code,
                    severity: item.severity,
                    message: item.message.clone(),
                    repair: item.repair.to_owned(),
                })
                .collect()
        }),
        tailnet_changed: auto_status_tailnet_changed(snapshot, autodiscovery)
            || matches!(identity_guard, IdentityGuardVerdict::TailnetChanged { .. }),
        node_key_changed: matches!(identity_guard, IdentityGuardVerdict::NodeKeyChanged { .. }),
        ..MeshAutoStatusSignals::default()
    }
}

fn auto_status_tailnet_changed(
    snapshot: &MeshForegroundSnapshot,
    autodiscovery: &TailscaleAutodiscoveryReport,
) -> bool {
    let Some(current_tailnet_id) = autodiscovery.tailnet_id.as_deref() else {
        return false;
    };
    auto_materialized_peer_record(snapshot)
        .as_ref()
        .is_some_and(|record| record.endpoint.tailnet_id != current_tailnet_id)
}

fn auto_status_identity_guard(
    snapshot: &MeshForegroundSnapshot,
    autodiscovery: &TailscaleAutodiscoveryReport,
) -> IdentityGuardVerdict {
    let (Some(tailnet_id), Some(self_node_key)) = (
        autodiscovery.tailnet_id.as_deref(),
        autodiscovery.self_node_key.as_deref(),
    ) else {
        return IdentityGuardVerdict::NoBoundIdentity;
    };
    let current = CurrentIdentity {
        tailnet_id: tailnet_id.to_owned(),
        tailnet_display_name: autodiscovery.tailnet_display_name.clone(),
        self_node_key: self_node_key.to_owned(),
    };
    let bound = auto_materialized_peer_record(snapshot).and_then(|record| {
        Some(BoundIdentity {
            tailnet_id: record.endpoint.tailnet_id,
            tailnet_display_name: record.endpoint.tailnet_display_name,
            materialized_on_node_key: record.materialized_on_node_key?,
        })
    });
    evaluate_identity_guard(bound.as_ref(), &current)
}

fn auto_materialized_status(
    snapshot: &MeshForegroundSnapshot,
    lane_policy: &MeshAutoLanePolicy,
) -> Option<MeshAutoMaterializedStatus> {
    if snapshot.storage.peer_count == 0 && snapshot.peers.is_empty() {
        return None;
    }

    let materialized_record = auto_materialized_peer_record(snapshot);
    let peer_set_hash = mesh_auto_peer_set_hash(&snapshot.peers);
    let digest = peer_set_hash
        .strip_prefix("blake3:")
        .unwrap_or(peer_set_hash.as_str());
    let peer_group_suffix: String = digest.chars().take(16).collect();
    Some(MeshAutoMaterializedStatus {
        peer_group_id: format!("pg_{peer_group_suffix}"),
        peer_set_hash,
        peer_count: snapshot.storage.peer_count,
        lane_policy: lane_policy.clone(),
        bound_tailnet_id: materialized_record
            .as_ref()
            .map(|record| record.endpoint.tailnet_id.clone()),
        materialized_on_node_key: materialized_record
            .as_ref()
            .and_then(|record| record.materialized_on_node_key.clone()),
        last_materialized_at: latest_peer_seen_at(snapshot),
        enrollment_source: materialized_record
            .as_ref()
            .map(|record| match record.trust_established_by.as_str() {
                "tailscale_auto_enrollment" => "auto".to_owned(),
                "auto_replaced_manual" => "auto_replaced_manual".to_owned(),
                _ => "manual".to_owned(),
            })
            .unwrap_or_else(|| "manual".to_owned()),
    })
}

fn effective_auto_lane_policy(
    snapshot: &MeshForegroundSnapshot,
    registry: &MeshPeerPolicyRegistry,
) -> MeshAutoLanePolicy {
    // The v2 status contract has one lane decision for the whole peer set.
    // Report the broadest effective exposure across enabled peers so one
    // restrictive peer cannot hide that another peer may receive the lane.
    MeshAutoLanePolicy {
        metadata: effective_auto_lane_decision(snapshot, registry, MeshLane::Metadata).as_str(),
        body: effective_auto_lane_decision(snapshot, registry, MeshLane::Body).as_str(),
        embedding: effective_auto_lane_decision(snapshot, registry, MeshLane::Embedding).as_str(),
        graph_link: effective_auto_lane_decision(snapshot, registry, MeshLane::GraphLink).as_str(),
        revision_notice: effective_auto_lane_decision(snapshot, registry, MeshLane::RevisionNotice)
            .as_str(),
        curation_signal: effective_auto_lane_decision(snapshot, registry, MeshLane::CurationSignal)
            .as_str(),
    }
}

fn effective_auto_lane_decision(
    snapshot: &MeshForegroundSnapshot,
    registry: &MeshPeerPolicyRegistry,
    lane: MeshLane,
) -> MeshLaneDecision {
    let mut enabled_peers = snapshot.peers.iter().filter(|peer| peer.enabled).peekable();
    if enabled_peers.peek().is_none() {
        return MeshLaneDecision::Deny;
    }

    enabled_peers.fold(MeshLaneDecision::Deny, |group_decision, peer| {
        let matching_policies = registry
            .policies()
            .iter()
            .filter(|policy| {
                policy.workspace_id == snapshot.workspace_id
                    && policy.peer_id == peer.peer_id
                    && policy
                        .origin_workspace_ids
                        .iter()
                        .any(|origin| origin == &snapshot.workspace_id)
            })
            .collect::<Vec<_>>();
        let [policy] = matching_policies.as_slice() else {
            return group_decision;
        };
        let durable_override =
            registry.lane_override_for(&snapshot.workspace_id, &peer.peer_id, lane);
        let peer_decision = durable_override.unwrap_or_else(|| policy.allowed_lanes.decision(lane));
        broadest_mesh_lane_decision(group_decision, peer_decision)
    })
}

const fn broadest_mesh_lane_decision(
    left: MeshLaneDecision,
    right: MeshLaneDecision,
) -> MeshLaneDecision {
    match (left, right) {
        (MeshLaneDecision::Allow, _) | (_, MeshLaneDecision::Allow) => MeshLaneDecision::Allow,
        (MeshLaneDecision::Quarantine, _) | (_, MeshLaneDecision::Quarantine) => {
            MeshLaneDecision::Quarantine
        }
        (MeshLaneDecision::Deny, MeshLaneDecision::Deny) => MeshLaneDecision::Deny,
    }
}

fn materialized_mesh_node_keys(snapshot: &MeshForegroundSnapshot) -> BTreeSet<String> {
    snapshot
        .peers
        .iter()
        .flat_map(|peer| {
            let mut keys = vec![peer.peer_id.clone(), peer.origin_node_id.clone()];
            if let Some(record) = foreground_sync_peer_record(peer) {
                keys.push(record.endpoint.tailscale_node_key);
            }
            keys
        })
        .collect()
}

fn auto_materialized_peer_record(snapshot: &MeshForegroundSnapshot) -> Option<MeshPeerRecord> {
    snapshot
        .peers
        .iter()
        .filter(|peer| peer.enabled)
        .filter_map(foreground_sync_peer_record)
        .find(|record| {
            matches!(
                record.trust_established_by.as_str(),
                "tailscale_auto_enrollment" | "auto_replaced_manual"
            )
        })
}

fn mesh_auto_peer_set_hash(peers: &[MeshPeerRow]) -> String {
    let mut rows: Vec<String> = peers
        .iter()
        .map(|peer| {
            format!(
                "{}\t{}\t{}",
                peer.peer_id, peer.origin_node_id, peer.enabled
            )
        })
        .collect();
    rows.sort();

    let mut hasher = blake3::Hasher::new();
    for row in rows {
        hasher.update(row.as_bytes());
        hasher.update(b"\n");
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn latest_peer_seen_at(snapshot: &MeshForegroundSnapshot) -> Option<String> {
    snapshot
        .peers
        .iter()
        .map(|peer| peer.last_seen_at.as_str())
        .max()
        .map(str::to_owned)
}

fn disabled_mesh_peer_ids(snapshot: &MeshForegroundSnapshot) -> Vec<String> {
    let mut ids: Vec<String> = snapshot
        .peers
        .iter()
        .filter(|peer| !peer.enabled)
        .map(|peer| peer.peer_id.clone())
        .collect();
    ids.sort();
    ids
}

fn auto_drift_severity(
    snapshot: &MeshForegroundSnapshot,
    signals: &MeshAutoStatusSignals,
    new_peer_count: u32,
) -> &'static str {
    if signals.tailnet_changed
        || signals.node_key_changed
        || signals.manual_conflict_present
        || signals.hello_responder_running == Some(false)
    {
        return "medium";
    }

    if new_peer_count > 2 {
        return "warning";
    }
    if new_peer_count > 0 {
        return "info";
    }
    if snapshot.initialized && snapshot.mesh_enabled && snapshot.storage.peer_count == 0 {
        return "info";
    }
    "none"
}

fn auto_status_action_graph(
    snapshot: &MeshForegroundSnapshot,
    signals: &MeshAutoStatusSignals,
) -> RepairActionGraph {
    let mut actions = Vec::new();

    if !snapshot.initialized || !snapshot.mesh_enabled {
        return build_auto_repair_action_graph(actions);
    }

    if signals.tailscale_authenticated == Some(false) {
        actions.push(tailscale_up_action());
    }

    if signals.tailnet_changed || signals.node_key_changed || signals.manual_conflict_present {
        actions.push(mesh_disable_action(
            &snapshot.workspace_path,
            signals.node_key_changed,
        ));
        actions.push(mesh_auto_enroll_action(
            &snapshot.workspace_path,
            vec!["ee_mesh_disable".to_owned()],
        ));
        return build_auto_repair_action_graph(actions);
    }

    if snapshot.storage.peer_count == 0 || signals.hello_responder_running == Some(false) {
        let daemon_prerequisites = if signals.tailscale_authenticated == Some(false) {
            vec!["tailscale_up".to_owned()]
        } else {
            Vec::new()
        };
        actions.push(ee_daemon_start_action(
            &snapshot.workspace_path,
            daemon_prerequisites,
        ));
        actions.push(mesh_auto_enroll_action(
            &snapshot.workspace_path,
            vec!["ee_daemon_start".to_owned()],
        ));
    }

    build_auto_repair_action_graph(actions)
}

fn auto_status_next_action_hint(
    snapshot: &MeshForegroundSnapshot,
    signals: &MeshAutoStatusSignals,
    new_peer_count: u32,
) -> String {
    let workspace_arg = mesh_shell_quote_arg(&snapshot.workspace_path);
    if !snapshot.initialized {
        return format!(
            "Run `ee init --workspace {workspace_arg} --json` before mesh auto-enrollment can inspect local state."
        );
    }
    if !snapshot.mesh_enabled {
        return "Mesh is disabled; set EE_MESH_ENABLED=1 or configure [mesh].enabled when this workspace should join a mesh."
            .to_owned();
    }
    if signals.tailscale_authenticated == Some(false) {
        return "Run `tailscale up` to authenticate, then re-run `ee mesh status`.".to_owned();
    }
    if signals.node_key_changed {
        return format!(
            "Auto-enrollment is blocked because nodeKeyChanged=true. Run `ee mesh disable --workspace {workspace_arg} --reason \"restored from different machine\"`, then re-run auto-enroll."
        );
    }
    if signals.tailnet_changed {
        return format!(
            "Auto-enrollment is blocked because tailnetChanged=true. Run `ee mesh disable --workspace {workspace_arg}`, then re-run auto-enroll."
        );
    }
    if signals.manual_conflict_present {
        return "Manual mesh configuration conflicts with auto-enrollment; resolve the manual peer-group before auto-enroll rewrites it."
            .to_owned();
    }
    if signals.hello_responder_running == Some(false) {
        return "Auto-enrolled but peers cannot reach this workspace. Run `ee daemon --foreground` to enable inbound discovery."
            .to_owned();
    }
    if snapshot.storage.peer_count == 0
        && signals.discovered_peer_count == 0
        && signals.tailscale_authenticated == Some(true)
        && signals.tailscale_authenticated_for_24h
    {
        return "You are the first ee instance on this tailnet. Other machines will appear automatically when they run ee with mesh enabled. Status: waiting for first peer."
            .to_owned();
    }
    if snapshot.storage.peer_count == 0 && signals.discovered_peer_count == 0 {
        return "Tailnet status was not refreshed by this read-only command. Run `ee mesh status --refresh --json` to recheck discovery, or run ee on a second machine in this tailnet."
            .to_owned();
    }
    if snapshot.storage.peer_count == 0 && signals.discovered_peer_count > 0 {
        return format!(
            "{} peers discovered. Run `ee mesh auto-enroll --workspace {workspace_arg}` to enroll them.",
            signals.discovered_peer_count
        );
    }
    if new_peer_count > 0 {
        return format!(
            "{new_peer_count} new peers are available. Run `ee mesh auto-enroll --workspace {workspace_arg}` to reconcile the peer set."
        );
    }
    "Auto-enrollment materialized state matches the local mesh cache; no drift was detected by this read-only status view."
        .to_owned()
}

fn auto_status_degradations(
    snapshot: &MeshForegroundSnapshot,
    signals: &MeshAutoStatusSignals,
) -> Vec<MeshCliDegradation> {
    let mut degraded = Vec::new();
    let workspace_arg = mesh_shell_quote_arg(&snapshot.workspace_path);
    if signals.tailnet_changed {
        degraded.push(MeshCliDegradation {
            code: AUTO_ENROLLMENT_TAILNET_CHANGED_CODE,
            severity: "medium",
            message: "Auto-enrollment materialized state belongs to a different tailnet."
                .to_owned(),
            repair: format!(
                "Run `ee mesh disable --workspace {workspace_arg}` and then `ee mesh auto-enroll --workspace {workspace_arg}`."
            ),
        });
    }
    if signals.node_key_changed {
        degraded.push(MeshCliDegradation {
            code: AUTO_ENROLLMENT_NODE_KEY_CHANGED_CODE,
            severity: "medium",
            message:
                    "Auto-enrollment materialized state was created on a different Tailscale node key."
                    .to_owned(),
            repair: format!(
                "Run `ee mesh disable --workspace {workspace_arg} --reason \"restored from different machine\"` and then `ee mesh auto-enroll --workspace {workspace_arg}`."
            ),
        });
    }
    if signals.manual_conflict_present {
        degraded.push(MeshCliDegradation {
            code: "auto_enrollment_manual_conflict",
            severity: "medium",
            message: "Manual peer configuration conflicts with the auto-enrollment peer group."
                .to_owned(),
            repair: "Resolve the manual peer group before re-running `ee mesh auto-enroll`."
                .to_owned(),
        });
    }
    degraded
}

fn mesh_shell_quote_arg(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for ch in value.chars() {
        if matches!(ch, '"' | '$' | '`' | '\\') {
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    quoted
}

fn build_auto_repair_action_graph(actions: Vec<RepairAction>) -> RepairActionGraph {
    build_repair_action_graph(actions).unwrap_or_else(|error| {
        tracing::warn!(
            error = %error,
            "mesh auto-enrollment status built an invalid repair action graph"
        );
        RepairActionGraph {
            schema: REPAIR_ACTION_GRAPH_SCHEMA_V1.to_owned(),
            actions: Vec::new(),
            topologically_ordered_execution: Vec::new(),
            parallelizable_groups: Vec::new(),
            estimated_total_duration_seconds: 0,
        }
    })
}

fn tailscale_up_action() -> RepairAction {
    RepairAction {
        id: "tailscale_up".to_owned(),
        kind: ActionKind::ShellCommand,
        command: "tailscale up".to_owned(),
        human_readable: "Authenticate this host with Tailscale.".to_owned(),
        prerequisites: Vec::new(),
        expected_outcome: ExpectedOutcome {
            resolves_checks: vec!["tailscale_authenticated".to_owned()],
            preconditions_for_next_actions: Vec::new(),
        },
        priority: Priority::Critical,
        estimated_duration_seconds: 60,
        reversible: false,
        reversal_command: None,
        requires_user_confirmation: true,
        execution_context: ExecutionContext::UserShell,
    }
}

fn ee_daemon_start_action(workspace_path: &str, prerequisites: Vec<String>) -> RepairAction {
    let workspace_arg = mesh_shell_quote_arg(workspace_path);
    RepairAction {
        id: "ee_daemon_start".to_owned(),
        kind: ActionKind::EeSubcommand,
        command: format!("ee daemon --foreground --workspace {workspace_arg}"),
        human_readable: "Start the foreground ee daemon so peers can discover this workspace."
            .to_owned(),
        prerequisites,
        expected_outcome: ExpectedOutcome {
            resolves_checks: vec!["hello_responder_running".to_owned()],
            preconditions_for_next_actions: Vec::new(),
        },
        priority: Priority::High,
        estimated_duration_seconds: 10,
        reversible: true,
        reversal_command: Some("Ctrl-C the foreground daemon".to_owned()),
        requires_user_confirmation: false,
        execution_context: ExecutionContext::EeSubcommand,
    }
}

fn mesh_auto_enroll_action(workspace_path: &str, prerequisites: Vec<String>) -> RepairAction {
    let workspace_arg = mesh_shell_quote_arg(workspace_path);
    RepairAction {
        id: "ee_mesh_auto_enroll".to_owned(),
        kind: ActionKind::EeSubcommand,
        command: format!("ee mesh auto-enroll --workspace {workspace_arg}"),
        human_readable: "Materialize the discovered ee-capable peers into the mesh peer set."
            .to_owned(),
        prerequisites,
        expected_outcome: ExpectedOutcome {
            resolves_checks: vec!["mesh_auto_enrollment_materialized".to_owned()],
            preconditions_for_next_actions: Vec::new(),
        },
        priority: Priority::Medium,
        estimated_duration_seconds: 5,
        reversible: true,
        reversal_command: Some(format!(
            "ee mesh disable --workspace {workspace_arg} --reason \"revert auto-enrollment\""
        )),
        requires_user_confirmation: false,
        execution_context: ExecutionContext::EeSubcommand,
    }
}

fn mesh_disable_action(workspace_path: &str, node_key_changed: bool) -> RepairAction {
    let reason = if node_key_changed {
        "restored from different machine"
    } else {
        "tailnet changed"
    };
    let workspace_arg = mesh_shell_quote_arg(workspace_path);
    let reason_arg = mesh_shell_quote_arg(reason);
    RepairAction {
        id: "ee_mesh_disable".to_owned(),
        kind: ActionKind::EeSubcommand,
        command: format!("ee mesh disable --workspace {workspace_arg} --reason {reason_arg}"),
        human_readable: "Disable the stale materialized peer group before auto-enrolling again."
            .to_owned(),
        prerequisites: Vec::new(),
        expected_outcome: ExpectedOutcome {
            resolves_checks: vec!["mesh_auto_enrollment_stale_binding_cleared".to_owned()],
            preconditions_for_next_actions: Vec::new(),
        },
        priority: Priority::High,
        estimated_duration_seconds: 5,
        reversible: false,
        reversal_command: None,
        requires_user_confirmation: true,
        execution_context: ExecutionContext::EeSubcommand,
    }
}

fn mesh_export_secret_subjects(artifact: &MeshExportArtifact) -> Vec<MeshExportSecretScanSubject> {
    let mut subjects = Vec::new();

    for peer in &artifact.peers {
        push_subject(
            &mut subjects,
            "peer",
            &peer.peer_id,
            "peerId",
            &peer.peer_id,
        );
        push_subject(
            &mut subjects,
            "peer",
            &peer.peer_id,
            "originNodeId",
            &peer.origin_node_id,
        );
        push_optional_subject(
            &mut subjects,
            "peer",
            &peer.peer_id,
            "displayName",
            peer.display_name.as_deref(),
        );
        push_optional_json_subjects(
            &mut subjects,
            "peer",
            &peer.peer_id,
            "policySummaryJson",
            peer.policy_summary_json.as_deref(),
        );
    }

    for cursor in &artifact.cursors {
        push_subject(
            &mut subjects,
            "cursor",
            &cursor.peer_id,
            "peerId",
            &cursor.peer_id,
        );
        push_subject(
            &mut subjects,
            "cursor",
            &cursor.peer_id,
            "originNodeId",
            &cursor.origin_node_id,
        );
        push_subject(
            &mut subjects,
            "cursor",
            &cursor.peer_id,
            "originWorkspaceId",
            &cursor.origin_workspace_id,
        );
        push_optional_subject(
            &mut subjects,
            "cursor",
            &cursor.peer_id,
            "tipEventHash",
            cursor.tip_event_hash.as_deref(),
        );
        push_optional_subject(
            &mut subjects,
            "cursor",
            &cursor.peer_id,
            "tipAuditHash",
            cursor.tip_audit_hash.as_deref(),
        );
    }

    for event in &artifact.events {
        subjects.extend(mesh_event_secret_subjects(event));
    }

    subjects
}

fn mesh_event_secret_subjects(event: &MeshEventRow) -> Vec<MeshExportSecretScanSubject> {
    let mut subjects = Vec::new();
    push_subject(
        &mut subjects,
        "event",
        &event.event_id,
        "eventId",
        &event.event_id,
    );
    push_subject(
        &mut subjects,
        "event",
        &event.event_id,
        "originNodeId",
        &event.origin_node_id,
    );
    push_subject(
        &mut subjects,
        "event",
        &event.event_id,
        "originWorkspaceId",
        &event.origin_workspace_id,
    );
    push_optional_subject(
        &mut subjects,
        "event",
        &event.event_id,
        "producerPeerId",
        event.producer_peer_id.as_deref(),
    );
    push_optional_subject(
        &mut subjects,
        "event",
        &event.event_id,
        "prevEventHash",
        event.prev_event_hash.as_deref(),
    );
    push_subject(
        &mut subjects,
        "event",
        &event.event_id,
        "eventHash",
        &event.event_hash,
    );
    push_subject(
        &mut subjects,
        "event",
        &event.event_id,
        "eventKind",
        &event.event_kind,
    );
    push_subject(
        &mut subjects,
        "event",
        &event.event_id,
        "logicalMemoryId",
        &event.logical_memory_id,
    );
    push_subject(
        &mut subjects,
        "event",
        &event.event_id,
        "contentHash",
        &event.content_hash,
    );
    push_subject(
        &mut subjects,
        "event",
        &event.event_id,
        "materialLane",
        &event.material_lane,
    );
    push_subject(
        &mut subjects,
        "event",
        &event.event_id,
        "redactionClass",
        &event.redaction_class,
    );
    push_subject(
        &mut subjects,
        "event",
        &event.event_id,
        "trustLane",
        &event.trust_lane,
    );
    push_subject(
        &mut subjects,
        "event",
        &event.event_id,
        "importDecision",
        &event.import_decision,
    );
    push_optional_subject(
        &mut subjects,
        "event",
        &event.event_id,
        "localMemoryId",
        event.local_memory_id.as_deref(),
    );
    push_optional_subject(
        &mut subjects,
        "event",
        &event.event_id,
        "bodyCacheKey",
        event.body_cache_key.as_deref(),
    );
    push_optional_json_subjects(
        &mut subjects,
        "event",
        &event.event_id,
        "policyFailureSurfaceJson",
        event.policy_failure_surface_json.as_deref(),
    );
    push_optional_json_subjects(
        &mut subjects,
        "event",
        &event.event_id,
        "policyDecisionJson",
        event.policy_decision_json.as_deref(),
    );
    push_json_subjects(
        &mut subjects,
        "event",
        &event.event_id,
        "eventJson",
        &event.event_json,
    );
    subjects
}

fn push_subject(
    subjects: &mut Vec<MeshExportSecretScanSubject>,
    source_surface: &str,
    source_id: &str,
    field: &str,
    value: &str,
) {
    subjects.push(MeshExportSecretScanSubject::new(
        source_surface,
        source_id,
        field,
        value,
    ));
}

fn push_optional_subject(
    subjects: &mut Vec<MeshExportSecretScanSubject>,
    source_surface: &str,
    source_id: &str,
    field: &str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        push_subject(subjects, source_surface, source_id, field, value);
    }
}

fn push_optional_json_subjects(
    subjects: &mut Vec<MeshExportSecretScanSubject>,
    source_surface: &str,
    source_id: &str,
    field: &str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        push_json_subjects(subjects, source_surface, source_id, field, value);
    }
}

fn push_json_subjects(
    subjects: &mut Vec<MeshExportSecretScanSubject>,
    source_surface: &str,
    source_id: &str,
    field: &str,
    value: &str,
) {
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(value) {
        push_json_value_subjects(subjects, source_surface, source_id, field, &parsed);
    } else {
        push_subject(subjects, source_surface, source_id, field, value);
    }
}

fn push_json_value_subjects(
    subjects: &mut Vec<MeshExportSecretScanSubject>,
    source_surface: &str,
    source_id: &str,
    field_prefix: &str,
    value: &serde_json::Value,
) {
    match value {
        serde_json::Value::String(text) => {
            push_subject(subjects, source_surface, source_id, field_prefix, text);
        }
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                push_json_value_subjects(
                    subjects,
                    source_surface,
                    source_id,
                    &format!("{field_prefix}[{index}]"),
                    item,
                );
            }
        }
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                push_json_value_subjects(
                    subjects,
                    source_surface,
                    source_id,
                    &format!("{field_prefix}.{key}"),
                    item,
                );
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

#[must_use]
pub fn foreground_degradations(
    workspace_path: &str,
    initialized: bool,
    mesh_enabled: bool,
) -> Vec<MeshCliDegradation> {
    let mut degraded = Vec::new();
    if !initialized {
        degraded.push(MeshCliDegradation::workspace_uninitialized(workspace_path));
    }
    if !mesh_enabled {
        degraded.push(MeshCliDegradation::mesh_disabled());
    }
    degraded
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        AUTO_ENROLLMENT_NODE_KEY_CHANGED_CODE, AUTO_ENROLLMENT_TAILNET_CHANGED_CODE,
        MESH_AUTO_STATUS_SCHEMA_V2, MESH_EXPORT_ARTIFACT_SCHEMA_V1, MESH_IMPORT_LEDGER_SCHEMA_V1,
        MESH_SYNC_ONCE_NETWORK_DEFERRED_CODE, MESH_SYNC_SUPERVISOR_BACKPRESSURE_CODE,
        MESH_SYNC_SUPERVISOR_BUDGET_EXHAUSTED_CODE, MESH_WORKSPACE_UNINITIALIZED_CODE,
        MeshAutoStatusSignals, MeshCliDegradation, MeshCursorRow, MeshEventContractError,
        MeshEventRow, MeshExportArtifact, MeshExportPolicyAttestation, MeshForegroundSnapshot,
        MeshForegroundSyncPeerOutcome, MeshForegroundSyncRequest, MeshForegroundSyncTransport,
        MeshPeerRow, MeshStorageCounts, MeshSyncSupervisorOptions, REPAIR_ACTION_GRAPH_SCHEMA_V1,
        TcpMeshForegroundSyncTransport, apply_outbound_artifact_metadata_policy,
        apply_outbound_export_policy, auto_enrollment_status_for_snapshot,
        canonical_mesh_event_hash, canonicalize_mesh_event_json, ephemeral_source_for,
        fetch_pending_team_bodies_from_paths, local_sync_round_request, mesh_event_id_from_hash,
        persist_sync_round_events, project_canonical_mesh_event, resolve_store_workspace_id,
        run_mesh_sync_once_from_paths, run_mesh_sync_supervisor_supervised,
        run_mesh_sync_supervisor_supervised_with_transport, spawn_team_responder_owner_if_needed,
        try_authenticated_team_sync_round,
    };
    use crate::config::ConfigFile;
    use crate::core::tailscale_probe::TailscaleLocalReport;
    use crate::db::{CreateWorkspaceInput, DbConnection};
    use crate::mesh::peer::{
        MESH_PEER_RECORD_SCHEMA_V1, MeshPeerCapabilities, MeshPeerCapabilityProfile,
        MeshPeerEndpoint, MeshPeerHandshake, MeshPeerKey, MeshPeerRecord, MeshPeerState,
    };
    use crate::mesh::policy::MeshPeerPolicyRegistry;

    #[test]
    fn spawn_team_responder_owner_skips_missing_store() {
        assert!(!spawn_team_responder_owner_if_needed(
            std::path::Path::new("/tmp/ee-missing-team-responder"),
            std::path::Path::new("/tmp/ee-missing-team-responder/ee.db"),
        ));
    }

    #[test]
    fn resolve_store_workspace_id_uses_store_row_when_team_member_id_is_orphan() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("ee.db");
        let connection = DbConnection::open_file(&database).expect("open");
        connection.migrate().expect("migrate");
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &CreateWorkspaceInput {
                    path: format!(r"\\?\{}", dir.path().display()),
                    name: Some("store-row".to_owned()),
                },
            )
            .expect("workspace");
        connection
            .insert_team_member(&crate::db::InsertTeamMemberInput {
                member_id: "mbr_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                team_id: "team_orphanfixture0000000001".to_owned(),
                workspace_id: "wsp_persistfixture000000000002".to_owned(),
                display_name: "origin".to_owned(),
                state: "active".to_owned(),
                is_self: true,
                origin_node_id: "node_orphanfixture0000000001".to_owned(),
                bound_via: "team_genesis".to_owned(),
                joined_at: "2026-08-17T00:00:00Z".to_owned(),
            })
            .expect("member");
        let resolved = resolve_store_workspace_id(&connection, dir.path()).expect("resolve");
        assert_eq!(resolved, "wsp_persistfixture000000000001");
    }

    #[test]
    fn resolve_store_workspace_id_prefers_team_member_id_when_that_row_exists() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("ee.db");
        let connection = DbConnection::open_file(&database).expect("open");
        connection.migrate().expect("migrate");
        connection
            .insert_workspace(
                "wsp_persistfixture000000000003",
                &CreateWorkspaceInput {
                    path: dir.path().display().to_string(),
                    name: Some("team-row".to_owned()),
                },
            )
            .expect("workspace");
        connection
            .insert_team_member(&crate::db::InsertTeamMemberInput {
                member_id: "mbr_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
                team_id: "team_teamfixture000000000001".to_owned(),
                workspace_id: "wsp_persistfixture000000000003".to_owned(),
                display_name: "origin".to_owned(),
                state: "active".to_owned(),
                is_self: true,
                origin_node_id: "node_teamfixture000000000001".to_owned(),
                bound_via: "team_genesis".to_owned(),
                joined_at: "2026-08-17T00:00:00Z".to_owned(),
            })
            .expect("member");
        let resolved = resolve_store_workspace_id(&connection, dir.path()).expect("resolve");
        assert_eq!(resolved, "wsp_persistfixture000000000003");
    }

    #[test]
    fn snapshot_from_paths_loads_peers_and_sync_once_stays_deferred_when_mesh_off() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("ee.db");
        let connection = DbConnection::open_file(&database).expect("open");
        connection.migrate().expect("migrate");
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &CreateWorkspaceInput {
                    path: dir.path().display().to_string(),
                    name: Some("steward-sync".to_owned()),
                },
            )
            .expect("workspace");
        connection
            .upsert_mesh_peer(&crate::db::UpsertMeshPeerInput {
                workspace_id: "wsp_persistfixture000000000001".to_owned(),
                peer_id: "peer_stewardsync0000000001".to_owned(),
                origin_node_id: "node_stewardsync0000000001".to_owned(),
                display_name: Some("peer".to_owned()),
                policy_summary_json: None,
                enabled: true,
                last_seen_at: Some("2026-08-14T00:00:00Z".to_owned()),
            })
            .expect("peer");
        let snapshot = MeshForegroundSnapshot::from_paths(dir.path(), &database).expect("snapshot");
        assert!(snapshot.initialized);
        assert_eq!(snapshot.peers.len(), 1);
        assert!(!snapshot.mesh_enabled);
        let report = run_mesh_sync_once_from_paths(dir.path(), &database).expect("sync");
        assert!(!report.contacted_peers);
        assert_eq!(
            fetch_pending_team_bodies_from_paths(dir.path(), &database),
            0
        );
    }

    #[test]
    fn local_team_enables_mesh_unless_explicitly_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("ee.db");
        let connection = DbConnection::open_file(&database).expect("open");
        connection.migrate().expect("migrate");
        connection
            .insert_workspace(
                "wsp_persistfixture000000000001",
                &CreateWorkspaceInput {
                    path: dir.path().display().to_string(),
                    name: Some("team-mesh-on".to_owned()),
                },
            )
            .expect("workspace");
        crate::mesh::team::create_local_team(
            &connection,
            "wsp_persistfixture000000000001",
            "Analysts",
            "2026-08-14T00:00:00Z",
        )
        .expect("create");
        let snapshot = MeshForegroundSnapshot::from_paths(dir.path(), &database).expect("snapshot");
        assert!(snapshot.initialized);
        assert!(
            snapshot.mesh_enabled,
            "a local team is the mesh opt-in when mesh.enabled is unset"
        );
    }

    #[test]
    fn ephemeral_source_for_loopback_and_routed_remotes() {
        let loop_v4 =
            ephemeral_source_for("127.0.0.1:41641".parse().expect("v4 loop")).expect("loop v4");
        assert!(loop_v4.ip().is_loopback());
        assert!(loop_v4.is_ipv4());
        assert_eq!(loop_v4.port(), 0);
        let loop_v6 =
            ephemeral_source_for("[::1]:41641".parse().expect("v6 loop")).expect("loop v6");
        assert!(loop_v6.ip().is_loopback());
        assert!(loop_v6.is_ipv6());
        assert_eq!(loop_v6.port(), 0);
        assert!(ephemeral_source_for("0.0.0.0:41641".parse().expect("unspec")).is_none());
        if let Some(routed) = ephemeral_source_for("192.0.2.1:41641".parse().expect("test-net")) {
            assert!(routed.is_ipv4());
            assert!(!routed.ip().is_unspecified());
            assert_eq!(routed.port(), 0);
        }
    }

    /// A `[[mesh.peer_policies]]` entry that allows metadata for `peer_target`
    /// but denies the body lane — the canonical body:deny export policy.
    fn body_deny_registry() -> MeshPeerPolicyRegistry {
        let config = ConfigFile::parse(
            r#"
[[mesh.peer_policies]]
policy_id = "pol_export"
workspace_id = "wsp_local"
peer_id = "peer_target"
origin_workspace_ids = ["wsp_origin"]
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
        .expect("body-deny policy config should parse");
        MeshPeerPolicyRegistry::from_config(&config)
    }

    /// A production outbound policy that permits the canonical material lanes
    /// only after the event projection is already redacted.
    fn redaction_required_registry() -> MeshPeerPolicyRegistry {
        let config = ConfigFile::parse(
            r#"
[[mesh.peer_policies]]
policy_id = "pol_export_redaction_required"
workspace_id = "wsp_local"
peer_id = "peer_target"
origin_workspace_ids = ["wsp_origin"]
trust_lane = "peerAgent"
import_trust_class = "agent_validated"
default_action = "deny"

[mesh.peer_policies.allowed_lanes]
metadata = "allow"
body = "allow"
embedding = "allow"
graph_link = "allow"
revision_notice = "allow"
curation_signal = "allow"

[mesh.peer_policies.redaction]
metadata = "redact"
preview = "redact"
body = "redact"
embedding = "redact"

[mesh.peer_policies.body_fetch]
allowed = true
requires_consent = false
max_bytes = 1048576
"#,
        )
        .expect("redaction-required policy config should parse");
        MeshPeerPolicyRegistry::from_config(&config)
    }

    fn body_allow_registry() -> MeshPeerPolicyRegistry {
        let config = ConfigFile::parse(
            r#"
[[mesh.peer_policies]]
policy_id = "pol_export_body_allow"
workspace_id = "wsp_local"
peer_id = "peer_target"
origin_workspace_ids = ["wsp_origin"]
trust_lane = "peerAgent"
import_trust_class = "agent_validated"
default_action = "deny"

[mesh.peer_policies.allowed_lanes]
metadata = "allow"
body = "allow"
embedding = "deny"
graph_link = "allow"
revision_notice = "allow"
curation_signal = "allow"

[mesh.peer_policies.redaction]
metadata = "share"
preview = "share"
body = "share"
embedding = "deny"

[mesh.peer_policies.body_fetch]
allowed = true
requires_consent = false
max_bytes = 1048576
"#,
        )
        .expect("body-allow policy config should parse");
        MeshPeerPolicyRegistry::from_config(&config)
    }

    fn export_event(label: &str, lane: &str, body_key: Option<&str>) -> MeshEventRow {
        let redaction_class = match lane {
            "body" => "body",
            "embedding" => "embedding",
            _ => "metadataOnly",
        };
        export_event_with_redaction(label, lane, redaction_class, body_key)
    }

    fn export_event_with_redaction(
        label: &str,
        lane: &str,
        redaction_class: &str,
        body_key: Option<&str>,
    ) -> MeshEventRow {
        let logical_memory_id = format!("mem_fixture_{label}");
        let content_hash = format!("blake3:{}", blake3::hash(label.as_bytes()).to_hex());
        let mut inner = serde_json::json!({
            "schema": crate::models::MESH_EVENT_SCHEMA_V1,
            "eventId": format!("mesh_evt_{}", "0".repeat(64)),
            "originNodeId": "node_origin",
            "originWorkspaceId": "wsp_origin",
            "seq": 1,
            "prevEventHash": null,
            "eventHash": format!("blake3:{}", "0".repeat(64)),
            "eventKind": "create",
            "logicalMemoryId": logical_memory_id.clone(),
            "contentHash": content_hash.clone(),
            "bodyRef": null,
            "materialLane": lane,
            "redactionClass": redaction_class,
            "trustLane": "peerAgent",
            "requiredFeatures": ["mesh.event.v1"],
            "producedAt": "2026-07-31T00:00:00Z",
        });
        let event_hash = canonical_mesh_event_hash(&inner).expect("hash fixture event");
        let event_id = mesh_event_id_from_hash(&event_hash).expect("derive fixture event id");
        let object = inner.as_object_mut().expect("fixture event object");
        object.insert(
            "eventHash".to_owned(),
            serde_json::json!(event_hash.clone()),
        );
        object.insert("eventId".to_owned(), serde_json::json!(event_id.clone()));

        MeshEventRow {
            event_id,
            origin_node_id: "node_origin".to_owned(),
            origin_workspace_id: "wsp_origin".to_owned(),
            producer_peer_id: None,
            seq: 1,
            prev_event_hash: None,
            event_hash,
            event_kind: "create".to_owned(),
            logical_memory_id,
            content_hash,
            material_lane: lane.to_owned(),
            redaction_class: redaction_class.to_owned(),
            trust_lane: "peerAgent".to_owned(),
            import_decision: "allow".to_owned(),
            local_memory_id: Some(label.to_owned()),
            body_cache_key: body_key.map(str::to_owned),
            policy_failure_surface_json: None,
            policy_decision_json: None,
            event_json: serde_json::to_string(&inner).expect("serialize fixture event"),
            policy_attestation: None,
            imported_at: "2026-07-31T00:00:00Z".to_owned(),
        }
    }

    fn reseal_event_json(
        event: &mut MeshEventRow,
        mutate: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
    ) {
        let mut inner: serde_json::Value =
            serde_json::from_str(&event.event_json).expect("parse fixture event");
        let object = inner.as_object_mut().expect("fixture event object");
        mutate(object);
        let event_hash = canonical_mesh_event_hash(&inner).expect("rehash fixture event");
        let event_id = mesh_event_id_from_hash(&event_hash).expect("rederive fixture event id");
        let object = inner.as_object_mut().expect("fixture event object");
        object.insert(
            "eventHash".to_owned(),
            serde_json::json!(event_hash.clone()),
        );
        object.insert("eventId".to_owned(), serde_json::json!(event_id.clone()));
        event.event_hash = event_hash;
        event.event_id = event_id;
        event.event_json = serde_json::to_string(&inner).expect("serialize resealed fixture event");
    }

    #[test]
    fn body_deny_policy_drops_body_events_and_strips_body_refs() {
        let meta_no_body = export_event("meta_no_body", "metadata", None);
        let meta_no_body_id = meta_no_body.event_id.clone();
        let meta_with_body = export_event("meta_with_body", "metadata", Some("bodykey-1"));
        let meta_with_body_id = meta_with_body.event_id.clone();
        let pure_body = export_event("pure_body", "body", Some("bodykey-2"));
        let pure_body_id = pure_body.event_id.clone();
        let events = vec![meta_no_body, meta_with_body, pure_body];
        let registry = body_deny_registry();
        let filtered = apply_outbound_export_policy(events, &registry, "wsp_local", "peer_target");

        // The body-lane event is dropped; both metadata events are kept.
        let kept_ids: Vec<&str> = filtered
            .events
            .iter()
            .map(|event| event.event_id.as_str())
            .collect();
        assert_eq!(
            kept_ids,
            vec![meta_no_body_id.as_str(), meta_with_body_id.as_str()]
        );

        // The metadata event that referenced a body has it stripped.
        let with_body = filtered
            .events
            .iter()
            .find(|event| event.event_id == meta_with_body_id)
            .expect("kept metadata event");
        assert!(
            with_body.body_cache_key.is_none(),
            "body ref must be stripped"
        );

        // The decision ledger records the drop and the strip.
        let dropped = filtered
            .decisions
            .iter()
            .find(|decision| decision.event_id == pure_body_id)
            .expect("pure_body decision");
        assert!(dropped.event_dropped);
        let stripped = filtered
            .decisions
            .iter()
            .find(|decision| decision.event_id == meta_with_body_id)
            .expect("meta_with_body decision");
        assert!(stripped.body_stripped && !stripped.event_dropped);
    }

    #[test]
    fn unknown_peer_denies_everything_fail_closed() {
        let events = vec![export_event("meta", "metadata", None)];
        let registry = body_deny_registry();
        // No policy matches peer_other -> outbound lookup is fail-closed.
        let filtered = apply_outbound_export_policy(events, &registry, "wsp_local", "peer_other");
        assert!(filtered.events.is_empty());
        assert!(filtered.decisions[0].event_dropped);
    }

    #[test]
    fn non_event_artifact_metadata_is_policy_gated_and_unredacted() {
        let build_artifact = || MeshExportArtifact {
            schema: MESH_EXPORT_ARTIFACT_SCHEMA_V1.to_owned(),
            workspace_id: "wsp_local".to_owned(),
            source: "test".to_owned(),
            policy_attestation: None,
            storage: MeshStorageCounts::default(),
            peers: vec![MeshPeerRow {
                peer_id: "peer_other".to_owned(),
                origin_node_id: "node_other".to_owned(),
                display_name: Some("private display metadata".to_owned()),
                enabled: true,
                last_seen_at: "2026-08-04T00:00:00Z".to_owned(),
                policy_summary_json: Some(r#"{"scope":"private"}"#.to_owned()),
            }],
            cursors: vec![MeshCursorRow {
                peer_id: "peer_other".to_owned(),
                origin_node_id: "node_other".to_owned(),
                origin_workspace_id: "wsp_origin".to_owned(),
                last_seq: 1,
                tip_event_hash: None,
                tip_audit_hash: None,
                status: "ready".to_owned(),
                updated_at: "2026-08-04T00:00:00Z".to_owned(),
            }],
            events: Vec::new(),
        };

        let mut denied = build_artifact();
        apply_outbound_artifact_metadata_policy(
            &mut denied,
            &MeshPeerPolicyRegistry::default(),
            "wsp_local",
            "peer_target",
        );
        assert!(denied.peers.is_empty());
        assert!(denied.cursors.is_empty());

        let mut requires_redaction = build_artifact();
        apply_outbound_artifact_metadata_policy(
            &mut requires_redaction,
            &redaction_required_registry(),
            "wsp_local",
            "peer_target",
        );
        assert!(
            requires_redaction.peers.is_empty(),
            "raw peer metadata must not satisfy a redact requirement"
        );
        assert!(
            requires_redaction.cursors.is_empty(),
            "raw cursor metadata must not satisfy a redact requirement"
        );
    }

    #[test]
    fn schema_invalid_lane_is_dropped_fail_closed() {
        let events = vec![export_event("weird", "bogus_lane", None)];
        let registry = body_deny_registry();
        let filtered = apply_outbound_export_policy(events, &registry, "wsp_local", "peer_target");
        assert!(filtered.events.is_empty());
        assert_eq!(filtered.decisions[0].reason, "invalid_event_schema");
        assert_eq!(filtered.decisions[0].material_lane, "invalid");
    }

    #[test]
    fn allowed_lane_without_body_is_kept_unchanged() {
        let events = vec![export_event("graph", "graphLink", None)];
        let registry = body_deny_registry();
        let filtered = apply_outbound_export_policy(events, &registry, "wsp_local", "peer_target");
        assert_eq!(filtered.events.len(), 1);
        assert!(!filtered.decisions[0].event_dropped);
        assert!(!filtered.decisions[0].body_stripped);
    }

    #[test]
    fn canonical_redaction_classes_drive_production_export_policy() {
        let cases = [
            ("metadata_only", "metadata", "metadataOnly", true, None),
            ("preview", "metadata", "preview", true, None),
            ("secret_denied", "metadata", "secretDenied", true, None),
            (
                "raw_body",
                "body",
                "body",
                false,
                Some("outbound_payload_requires_redaction"),
            ),
            (
                "raw_embedding",
                "embedding",
                "embedding",
                false,
                Some("outbound_payload_requires_redaction"),
            ),
            // This was accepted by the buggy pre-schema mapping. It is not a
            // published ee.mesh.event.v1 redactionClass and must fail closed.
            (
                "legacy_redacted",
                "metadata",
                "redacted",
                false,
                Some("invalid_event_schema"),
            ),
        ];
        let candidates = cases
            .iter()
            .map(
                |(label, lane, redaction_class, expected_kept, expected_reason)| {
                    let event = export_event_with_redaction(label, lane, redaction_class, None);
                    (
                        event.event_id.clone(),
                        event,
                        *expected_kept,
                        *expected_reason,
                    )
                },
            )
            .collect::<Vec<_>>();
        let expectations = candidates
            .iter()
            .map(|(event_id, _, expected_kept, expected_reason)| {
                (event_id.clone(), *expected_kept, *expected_reason)
            })
            .collect::<Vec<_>>();
        let events = candidates
            .into_iter()
            .map(|(_, event, _, _)| event)
            .collect();
        let registry = redaction_required_registry();

        let filtered = apply_outbound_export_policy(events, &registry, "wsp_local", "peer_target");

        for (event_id, expected_kept, expected_reason) in expectations {
            let kept = filtered
                .events
                .iter()
                .any(|event| event.event_id == event_id);
            assert_eq!(kept, expected_kept, "unexpected verdict for {event_id}");
            let decision = filtered
                .decisions
                .iter()
                .find(|decision| decision.event_id == event_id)
                .expect("every candidate must have an outbound decision");
            assert_eq!(decision.event_dropped, !expected_kept);
            if let Some(expected_reason) = expected_reason {
                assert_eq!(decision.reason, expected_reason);
            }
        }
    }

    #[test]
    fn canonical_projection_rejects_unknown_hash_mismatch_and_outer_mismatch() {
        let canonical = export_event("canonical_projection", "metadata", None);
        let projection = project_canonical_mesh_event(&canonical).expect("valid projection");
        let projected_value: serde_json::Value =
            serde_json::from_str(&projection.event.event_json).expect("parse canonical projection");
        assert_eq!(
            projection.event.event_json,
            serde_json::to_string(&canonicalize_mesh_event_json(&projected_value))
                .expect("serialize canonical value")
        );

        let mut unknown = canonical.clone();
        reseal_event_json(&mut unknown, |object| {
            object.insert(
                "metadataPayload".to_owned(),
                serde_json::json!("covert-content"),
            );
        });
        assert_eq!(
            project_canonical_mesh_event(&unknown),
            Err(MeshEventContractError::InvalidSchema)
        );

        let mut hash_mismatch = canonical.clone();
        let mut inner: serde_json::Value =
            serde_json::from_str(&hash_mismatch.event_json).expect("parse fixture event");
        inner.as_object_mut().expect("fixture event object").insert(
            "contentHash".to_owned(),
            serde_json::json!(format!("blake3:{}", "a".repeat(64))),
        );
        hash_mismatch.event_json = serde_json::to_string(&inner).expect("serialize hash mismatch");
        assert_eq!(
            project_canonical_mesh_event(&hash_mismatch),
            Err(MeshEventContractError::EventHashMismatch)
        );

        let mut id_mismatch = canonical.clone();
        let mismatched_id = format!("mesh_evt_{}", "f".repeat(64));
        let mut inner: serde_json::Value =
            serde_json::from_str(&id_mismatch.event_json).expect("parse fixture event");
        inner.as_object_mut().expect("fixture event object").insert(
            "eventId".to_owned(),
            serde_json::json!(mismatched_id.clone()),
        );
        id_mismatch.event_id = mismatched_id;
        id_mismatch.event_json = serde_json::to_string(&inner).expect("serialize id mismatch");
        assert_eq!(
            project_canonical_mesh_event(&id_mismatch),
            Err(MeshEventContractError::EventIdMismatch)
        );

        let mut missing_required = canonical.clone();
        let mut inner: serde_json::Value =
            serde_json::from_str(&missing_required.event_json).expect("parse fixture event");
        inner
            .as_object_mut()
            .expect("fixture event object")
            .remove("producedAt");
        missing_required.event_json =
            serde_json::to_string(&inner).expect("serialize missing required field");
        assert_eq!(
            project_canonical_mesh_event(&missing_required),
            Err(MeshEventContractError::InvalidSchema)
        );

        let mut outer_mismatch = canonical;
        outer_mismatch.logical_memory_id = "mem_different_projection".to_owned();
        assert_eq!(
            project_canonical_mesh_event(&outer_mismatch),
            Err(MeshEventContractError::OuterProjectionMismatch)
        );
    }

    #[test]
    fn canonical_projection_clears_destination_local_outer_state() {
        let mut injected = export_event(
            "destination_local_outer_injection",
            "metadata",
            Some("peer-chosen-body-cache-key"),
        );
        injected.import_decision = "quarantine".to_owned();
        injected.local_memory_id = Some("mem_peer_chosen_local_reference".to_owned());
        injected.policy_failure_surface_json =
            Some(r#"{"reason":"peer-chosen-local-failure"}"#.to_owned());
        injected.policy_decision_json =
            Some(r#"{"decision":"peer-chosen-local-allow"}"#.to_owned());
        injected.policy_attestation = Some(MeshExportPolicyAttestation::allowed(4_294));
        injected.imported_at = "2099-12-31T23:59:59Z".to_owned();

        let projection = project_canonical_mesh_event(&injected).expect("valid inner event");
        let projected = &projection.event;
        assert!(projected.import_decision.is_empty());
        assert!(projected.local_memory_id.is_none());
        assert!(projected.body_cache_key.is_none());
        assert!(projected.policy_failure_surface_json.is_none());
        assert!(projected.policy_decision_json.is_none());
        assert!(projected.policy_attestation.is_none());
        assert!(
            projected.imported_at.is_empty(),
            "the import writer must assign its own local timestamp"
        );
        assert_eq!(projected.event_id, injected.event_id);
        assert_eq!(projected.event_hash, injected.event_hash);

        let projected_again = project_canonical_mesh_event(projected)
            .expect("sanitized projection remains canonical");
        assert_eq!(projected_again, projection);
    }

    #[test]
    fn arbitrary_inner_metadata_requires_an_unredacted_body_lane() {
        let mut trust_claim = export_event("trust_claim", "metadata", None);
        reseal_event_json(&mut trust_claim, |object| {
            object.insert(
                "trustClaim".to_owned(),
                serde_json::json!({"note": "arbitrary non-secret content"}),
            );
        });
        let trust_claim_id = trust_claim.event_id.clone();
        let mut remote_uri = export_event("remote_uri", "metadata", None);
        reseal_event_json(&mut remote_uri, |object| {
            object.insert(
                "bodyRef".to_owned(),
                serde_json::json!({
                    "kind": "remoteAvailable",
                    "uri": "mesh-body://arbitrary-non-secret-content",
                }),
            );
        });
        let remote_uri_id = remote_uri.event_id.clone();

        let denied = apply_outbound_export_policy(
            vec![trust_claim.clone(), remote_uri.clone()],
            &body_deny_registry(),
            "wsp_local",
            "peer_target",
        );
        assert!(denied.events.is_empty());
        assert!(denied.decisions.iter().all(|decision| {
            decision.event_dropped
                && decision.reason == "event_metadata_requires_unredacted_body_lane"
        }));

        let allowed = apply_outbound_export_policy(
            vec![trust_claim, remote_uri],
            &body_allow_registry(),
            "wsp_local",
            "peer_target",
        );
        assert_eq!(allowed.events.len(), 2);
        assert!(
            allowed
                .events
                .iter()
                .any(|event| event.event_id == trust_claim_id)
        );
        assert!(
            allowed
                .events
                .iter()
                .any(|event| event.event_id == remote_uri_id)
        );
    }
    use crate::mesh::tailscale_autodiscovery::{
        TAILSCALE_AUTODISCOVERY_SCHEMA_V1, TailscaleAutodiscoveryReport,
    };
    use asupersync::runtime::JoinError;
    use asupersync::{Budget, CancelReason, Cx, LabConfig, LabRuntime, Outcome};
    use std::sync::{Arc, Mutex as StdMutex};

    type TestResult = Result<(), String>;

    #[test]
    fn status_posture_is_disabled_local_only_when_initialized_but_disabled() {
        let snapshot = MeshForegroundSnapshot {
            workspace_id: "wsp_test".to_owned(),
            workspace_path: "/tmp/ee".to_owned(),
            database_path: "/tmp/ee/.ee/ee.db".to_owned(),
            initialized: true,
            mesh_enabled: false,
            mode: "off".to_owned(),
            storage: MeshStorageCounts::default(),
            peers: Vec::new(),
            cursors: Vec::new(),
            events: Vec::new(),
            degraded: vec![MeshCliDegradation::mesh_disabled()],
        };
        let report = snapshot.status_report();
        assert_eq!(report.posture, "disabled_local_only");
        assert_eq!(report.degraded[0].code, "mesh_disabled");
        assert_eq!(report.auto_enrollment.schema, MESH_AUTO_STATUS_SCHEMA_V2);
        assert!(!report.auto_enrollment.enabled);
        assert!(report.auto_enrollment.read_only);
        assert_eq!(
            report.auto_enrollment.drift.action_graph.schema,
            REPAIR_ACTION_GRAPH_SCHEMA_V1
        );
    }

    #[test]
    fn status_report_repair_commands_escape_shell_sensitive_workspace_path() {
        let mut snapshot = sample_snapshot(Vec::new());
        snapshot.workspace_path = "/tmp/ee \"quoted\" $HOME".to_owned();
        let report = snapshot.status_report();
        let expected = "--workspace \"/tmp/ee \\\"quoted\\\" \\$HOME\"";

        assert!(
            report
                .repair_commands
                .iter()
                .all(|command| command.contains(expected)),
            "all status repair commands should quote the workspace safely: {:?}",
            report.repair_commands
        );
    }

    #[test]
    fn auto_status_view_action_graph_field_validates_against_ee_repair_action_graph_v1_schema() {
        let snapshot = sample_snapshot(Vec::new());
        let auto_status = snapshot.status_report().auto_enrollment;

        assert_eq!(auto_status.schema, MESH_AUTO_STATUS_SCHEMA_V2);
        assert_eq!(
            auto_status.drift.action_graph.schema,
            REPAIR_ACTION_GRAPH_SCHEMA_V1
        );
        assert_eq!(
            auto_status
                .drift
                .action_graph
                .topologically_ordered_execution,
            vec![
                "ee_daemon_start".to_owned(),
                "ee_mesh_auto_enroll".to_owned()
            ]
        );
        assert_eq!(auto_status.drift.drift_severity, "info");
    }

    #[test]
    fn auto_status_view_action_graph_topological_order_matches_dependency_chain() {
        let snapshot = sample_snapshot(Vec::new());
        let auto_status = auto_enrollment_status_for_snapshot(
            &snapshot,
            MeshAutoStatusSignals {
                tailscale_authenticated: Some(false),
                ..MeshAutoStatusSignals::default()
            },
        );

        assert_eq!(
            auto_status
                .drift
                .action_graph
                .topologically_ordered_execution,
            vec![
                "tailscale_up".to_owned(),
                "ee_daemon_start".to_owned(),
                "ee_mesh_auto_enroll".to_owned()
            ]
        );
        assert_eq!(
            auto_status.drift.action_graph.parallelizable_groups,
            vec![
                vec!["tailscale_up".to_owned()],
                vec!["ee_daemon_start".to_owned()],
                vec!["ee_mesh_auto_enroll".to_owned()]
            ]
        );
    }

    #[test]
    fn auto_status_view_node_key_changed_classified_as_medium_severity() {
        let snapshot = sample_snapshot(vec![sample_peer("peer-a", true)]);
        let auto_status = auto_enrollment_status_for_snapshot(
            &snapshot,
            MeshAutoStatusSignals {
                node_key_changed: true,
                ..MeshAutoStatusSignals::default()
            },
        );

        assert!(auto_status.drift.node_key_changed);
        assert_eq!(auto_status.drift.drift_severity, "medium");
        assert_eq!(
            auto_status
                .drift
                .action_graph
                .topologically_ordered_execution,
            vec![
                "ee_mesh_disable".to_owned(),
                "ee_mesh_auto_enroll".to_owned()
            ]
        );
        assert!(
            auto_status
                .degraded
                .iter()
                .any(|item| item.code == "auto_enrollment_node_key_changed")
        );
    }

    #[test]
    fn auto_status_materialized_reports_persisted_node_key_binding() {
        let snapshot = sample_snapshot(vec![sample_auto_enrolled_peer(
            "peer-a",
            "nodekey:self-materializer",
        )]);

        let auto_status =
            auto_enrollment_status_for_snapshot(&snapshot, MeshAutoStatusSignals::default());
        let materialized = auto_status
            .materialized
            .expect("auto-enrolled peer should produce materialized status");

        assert_eq!(
            materialized.materialized_on_node_key.as_deref(),
            Some("nodekey:self-materializer")
        );
        assert_eq!(
            materialized.bound_tailnet_id.as_deref(),
            Some("tailnet-test")
        );
        assert_eq!(materialized.enrollment_source, "auto");
    }

    #[test]
    fn status_report_with_autodiscovery_detects_node_key_drift() {
        let snapshot = sample_snapshot(vec![sample_auto_enrolled_peer(
            "peer-a",
            "nodekey:old-materializer",
        )]);
        let autodiscovery = sample_autodiscovery("tailnet-test", "nodekey:new-materializer");
        let mut local = TailscaleLocalReport::mesh_disabled();
        local.authenticated = true;
        local.binary_authentic = true;
        local.shields_up = Some(false);
        local.tailnet_display_name = Some("test tailnet".to_owned());
        local.degradations.clear();

        let status = snapshot.status_report_with_autodiscovery(
            &autodiscovery,
            Some(&local),
            &MeshPeerPolicyRegistry::default(),
            &BTreeSet::new(),
        );

        assert_eq!(status.auto_enrollment.tailscale.authenticated, Some(true));
        assert_eq!(status.auto_enrollment.tailscale.shields_up, Some(false));
        assert_eq!(
            status.auto_enrollment.tailscale.binary_authentic,
            Some(true)
        );
        assert_eq!(
            status
                .auto_enrollment
                .tailscale
                .tailnet_display_name
                .as_deref(),
            Some("test tailnet")
        );
        assert!(status.auto_enrollment.drift.node_key_changed);
        assert!(!status.auto_enrollment.drift.tailnet_changed);
        assert!(status.auto_enrollment.degraded.iter().any(|item| {
            item.code == AUTO_ENROLLMENT_NODE_KEY_CHANGED_CODE && item.severity == "medium"
        }));
    }

    #[test]
    fn auto_status_projects_real_policy_probe_and_drift_observations() {
        let snapshot = sample_snapshot(vec![sample_auto_enrolled_peer(
            "peer-a",
            "nodekey:self-materializer",
        )]);
        let registry = auto_status_policy_registry();
        let mut autodiscovery = sample_autodiscovery("tailnet-test", "nodekey:self-materializer");
        autodiscovery.ee_capable_peers = vec![
            crate::mesh::tailscale_autodiscovery::TailscaleAutodiscoveryPeer {
                node_key: "nodekey:new-peer".to_owned(),
                tailscale_ip: "100.64.0.2".to_owned(),
                magic_dns_name: Some("new-peer.tailnet.test".to_owned()),
                hostname: Some("new-peer".to_owned()),
                ee_protocol_version: "1.0".to_owned(),
                workspace_match_set: vec!["wsp_test".to_owned()],
                last_probed_at: "2026-08-07T00:00:00Z".to_owned(),
                latency_ms: 4,
                discovery_policy_decision: "service_tag_match".to_owned(),
            },
        ];
        autodiscovery.skipped_peers = vec![
            crate::mesh::tailscale_autodiscovery::TailscaleAutodiscoverySkippedPeer {
                node_key: "nodekey:timeout-peer".to_owned(),
                reason: "probe_timeout".to_owned(),
            },
        ];
        let mut local = TailscaleLocalReport::mesh_disabled();
        local.authenticated = true;
        local.binary_authentic = true;
        local.shields_up = Some(false);
        local.tailnet_display_name = Some("test tailnet".to_owned());
        local.degradations.clear();

        let status = snapshot.status_report_with_autodiscovery(
            &autodiscovery,
            Some(&local),
            &registry,
            &BTreeSet::from(["peer-a-node".to_owned()]),
        );
        let auto = status.auto_enrollment;
        let materialized = auto.materialized.expect("materialized peer status");

        assert_eq!(materialized.lane_policy.metadata, "quarantine");
        assert_eq!(materialized.lane_policy.body, "allow");
        assert_eq!(materialized.lane_policy.embedding, "deny");
        assert_eq!(auto.drift.new_peers_available, vec!["nodekey:new-peer"]);
        assert_eq!(auto.drift.new_peer_count, 1);
        assert_eq!(
            auto.drift.transient_unreachable,
            vec!["nodekey:timeout-peer"]
        );
        assert_eq!(
            auto.peer_state_breakdown.liveness_status,
            "not_probed_in_this_mode"
        );
        assert_eq!(auto.peer_state_breakdown.active, None);
        assert_eq!(auto.peer_state_breakdown.soft_stale, None);
        assert_eq!(auto.peer_state_breakdown.hard_stale, None);
        assert_eq!(auto.peer_state_breakdown.denylisted, 1);
        assert_eq!(auto.discovery, autodiscovery);
        assert_eq!(auto.discovery_cache.status, "not_probed_in_this_mode");
        assert_eq!(auto.steward_posture.status, "not_probed_in_this_mode");
    }

    #[test]
    fn auto_status_does_not_infer_liveness_or_drift_from_configured_peer_rows() {
        let snapshot = sample_snapshot(vec![
            sample_peer("peer-enabled", true),
            sample_peer("peer-disabled", false),
        ]);

        let auto = auto_enrollment_status_for_snapshot(&snapshot, MeshAutoStatusSignals::default());

        assert_eq!(
            auto.peer_state_breakdown.liveness_status,
            "not_probed_in_this_mode"
        );
        assert_eq!(auto.peer_state_breakdown.active, None);
        assert_eq!(auto.peer_state_breakdown.soft_stale, None);
        assert_eq!(auto.peer_state_breakdown.hard_stale, None);
        assert_eq!(auto.drift.disabled_peers_in_config, vec!["peer-disabled"]);
        assert_eq!(
            auto.drift.drift_severity, "none",
            "disabled configuration is inventory, not a liveness observation"
        );
    }

    #[test]
    fn auto_status_degraded_repairs_use_concrete_workspace_path() {
        let snapshot = sample_snapshot(vec![sample_peer("peer-a", true)]);
        let auto_status = auto_enrollment_status_for_snapshot(
            &snapshot,
            MeshAutoStatusSignals {
                tailnet_changed: true,
                node_key_changed: true,
                ..MeshAutoStatusSignals::default()
            },
        );

        let repairs = auto_status
            .degraded
            .iter()
            .filter(|item| {
                item.code == AUTO_ENROLLMENT_TAILNET_CHANGED_CODE
                    || item.code == AUTO_ENROLLMENT_NODE_KEY_CHANGED_CODE
            })
            .map(|item| item.repair.as_str())
            .collect::<Vec<_>>();
        assert_eq!(repairs.len(), 2);
        for repair in repairs {
            assert!(
                repair.contains("--workspace \"/tmp/ee\""),
                "repair should include the concrete workspace path: {repair}"
            );
            assert!(
                !repair.contains('<') && !repair.contains('>'),
                "repair hint must not expose an unresolved metavariable: {repair}"
            );
        }
    }

    #[test]
    fn auto_status_degraded_repairs_escape_shell_sensitive_workspace_path() {
        let mut snapshot = sample_snapshot(vec![sample_peer("peer-a", true)]);
        snapshot.workspace_path = "/tmp/ee \"quoted\" $HOME".to_owned();
        let auto_status = auto_enrollment_status_for_snapshot(
            &snapshot,
            MeshAutoStatusSignals {
                tailnet_changed: true,
                node_key_changed: true,
                ..MeshAutoStatusSignals::default()
            },
        );
        let expected = "--workspace \"/tmp/ee \\\"quoted\\\" \\$HOME\"";

        let repairs = auto_status
            .degraded
            .iter()
            .filter(|item| {
                item.code == AUTO_ENROLLMENT_TAILNET_CHANGED_CODE
                    || item.code == AUTO_ENROLLMENT_NODE_KEY_CHANGED_CODE
            })
            .map(|item| item.repair.as_str())
            .collect::<Vec<_>>();
        assert_eq!(repairs.len(), 2);
        assert!(
            repairs.iter().all(|repair| repair.contains(expected)),
            "all auto-enrollment repairs should quote the workspace safely: {repairs:?}"
        );
    }

    #[test]
    fn sync_once_network_deferred_repair_uses_concrete_file_name() {
        let degraded = MeshCliDegradation::sync_once_network_deferred();

        assert!(degraded.repair.contains("mesh-export.json"));
        assert!(
            degraded
                .message
                .contains("no usable foreground peer transport path"),
            "deferred sync diagnostic should name the current transport blocker: {}",
            degraded.message
        );
        assert!(
            !degraded.message.contains("not implemented"),
            "deferred sync diagnostic must not claim closed protocol primitives are missing: {}",
            degraded.message
        );
        assert!(
            !degraded.repair.contains('<') && !degraded.repair.contains('>'),
            "repair hint must not expose an unresolved metavariable: {}",
            degraded.repair
        );
    }

    #[test]
    fn anti_entropy_transport_unavailable_names_the_gap_honestly() {
        let degraded = MeshCliDegradation::anti_entropy_transport_unavailable(3);

        assert_eq!(
            degraded.code,
            crate::mesh::anti_entropy_protocol::degraded_codes::TRANSPORT_UNAVAILABLE
        );
        assert_eq!(degraded.severity, "low");
        assert!(
            degraded.message.contains("3 active peer(s)"),
            "message must say why a round was warranted: {}",
            degraded.message
        );
        assert!(
            degraded.repair.contains("mesh-export.json"),
            "repair must offer the working foreground transfer path: {}",
            degraded.repair
        );
        assert!(
            !degraded.repair.contains('<') && !degraded.repair.contains('>'),
            "repair hint must not expose an unresolved metavariable: {}",
            degraded.repair
        );
    }

    #[test]
    fn auto_status_view_first_time_on_tailnet_hint_emitted_when_self_only_and_stable_24h() {
        let snapshot = sample_snapshot(Vec::new());
        let auto_status = auto_enrollment_status_for_snapshot(
            &snapshot,
            MeshAutoStatusSignals {
                tailscale_authenticated: Some(true),
                tailscale_authenticated_for_24h: true,
                ..MeshAutoStatusSignals::default()
            },
        );

        assert_eq!(auto_status.drift.drift_severity, "info");
        assert!(
            auto_status
                .drift
                .next_action_hint
                .contains("first ee instance")
        );
        assert!(
            auto_status
                .drift
                .next_action_hint
                .contains("waiting for first peer")
        );
    }

    #[test]
    fn export_artifact_preserves_counts_and_schema() {
        let snapshot = MeshForegroundSnapshot {
            workspace_id: "wsp_test".to_owned(),
            workspace_path: "/tmp/ee".to_owned(),
            database_path: "/tmp/ee/.ee/ee.db".to_owned(),
            initialized: true,
            mesh_enabled: true,
            mode: "cache".to_owned(),
            storage: MeshStorageCounts {
                peer_count: 1,
                cursor_count: 2,
                imported_event_count: 3,
                policy_decision_event_count: 0,
                policy_failure_event_count: 0,
                mapped_memory_count: 0,
                cached_body_count: 0,
            },
            peers: Vec::new(),
            cursors: Vec::new(),
            events: Vec::new(),
            degraded: Vec::new(),
        };
        let artifact = snapshot.export_artifact();
        assert_eq!(artifact.schema, MESH_EXPORT_ARTIFACT_SCHEMA_V1);
        assert_eq!(artifact.storage.peer_count, 1);
        assert_eq!(artifact.storage.cursor_count, 2);
        assert_eq!(artifact.storage.imported_event_count, 3);
    }

    #[test]
    fn uninitialized_degradation_points_to_ee_init() {
        let degraded = MeshCliDegradation::workspace_uninitialized("/tmp/ee");
        assert_eq!(degraded.code, MESH_WORKSPACE_UNINITIALIZED_CODE);
        assert!(degraded.repair.contains("ee init --workspace"));
    }

    #[test]
    fn sync_supervisor_lab_runtime_reports_backpressure() -> TestResult {
        let snapshot = sample_snapshot(vec![
            sample_peer("peer-a", true),
            sample_peer("peer-b", true),
        ]);
        let options = MeshSyncSupervisorOptions {
            peer_concurrency: 1,
            ..MeshSyncSupervisorOptions::default()
        };

        let report = run_supervisor_in_lab(snapshot, options, 610)?;

        assert_eq!(report.supervisor, "asupersync_foreground");
        assert_eq!(report.health, "backpressured");
        assert_eq!(report.backpressure.queued_peer_count, 1);
        assert!(!report.contacted_peers);
        assert!(!report.local_commands_blocked);
        assert_eq!(report.ticks.len(), 1);
        assert!(
            report
                .degraded
                .iter()
                .any(|item| item.code == MESH_SYNC_SUPERVISOR_BACKPRESSURE_CODE)
        );
        Ok(())
    }

    #[test]
    fn sync_supervisor_budget_exhaustion_is_mesh_specific() -> TestResult {
        let snapshot = sample_snapshot(vec![sample_peer("peer-a", true)]);
        let options = MeshSyncSupervisorOptions {
            body_fetch_budget_bytes: 0,
            ..MeshSyncSupervisorOptions::default()
        };

        let report = run_supervisor_in_lab(snapshot, options, 611)?;

        assert_eq!(report.health, "budget_exhausted");
        assert_eq!(
            report.budget.exhausted_resource.as_deref(),
            Some("bodyFetch")
        );
        assert!(
            report
                .degraded
                .iter()
                .any(|item| item.code == MESH_SYNC_SUPERVISOR_BUDGET_EXHAUSTED_CODE)
        );
        assert_eq!(report.ticks[0].replay_path, "mesh_import_replay");
        Ok(())
    }

    #[test]
    fn sync_supervisor_no_transport_emits_deferred_without_peer_contact() -> TestResult {
        let snapshot = sample_snapshot(vec![sample_peer("peer-a", true)]);

        let report = run_supervisor_in_lab(snapshot, MeshSyncSupervisorOptions::default(), 612)?;

        assert_eq!(report.health, "deferred");
        assert!(!report.contacted_peers);
        assert!(!report.ticks[0].contacted_peers);
        assert!(
            report
                .degraded
                .iter()
                .any(|item| item.code == MESH_SYNC_ONCE_NETWORK_DEFERRED_CODE)
        );
        assert!(
            report.degraded.iter().any(|item| {
                item.code
                    == crate::mesh::anti_entropy_protocol::degraded_codes::TRANSPORT_UNAVAILABLE
            }),
            "an enrolled active peer with no contacted transport must expose the anti-entropy gap"
        );
        Ok(())
    }

    #[test]
    fn authenticated_team_sync_round_is_absent_without_pair_key() -> TestResult {
        // Pair-key + LocalAPI futures are !Send (CurrentCxGuard). The product
        // path is same-thread `block_on` after the Send supervisor, not
        // LabRuntime::create_task.
        let snapshot = sample_snapshot(vec![sample_trusted_sync_peer("peer-a")]);
        let runtime = crate::core::build_cli_runtime()
            .map_err(|error| format!("auth sync lab runtime: {error}"))?;
        let found = runtime.block_on(async move {
            let Some(cx) = Cx::current() else {
                return None;
            };
            try_authenticated_team_sync_round(&cx, &snapshot, &MeshSyncSupervisorOptions::default())
                .await
        });
        assert!(
            found.is_none(),
            "unsigned lab fixtures have no pair key or LocalAPI route"
        );
        Ok(())
    }

    #[test]
    fn sync_supervisor_fake_transport_contacts_peer_without_deferred() -> TestResult {
        let snapshot = sample_snapshot(vec![sample_trusted_sync_peer("peer-b")]);
        let transport = FakeForegroundSyncTransport;

        let report = run_supervisor_in_lab_with_transport(
            snapshot,
            MeshSyncSupervisorOptions::default(),
            transport,
            613,
        )?;

        assert_eq!(report.health, "synced");
        assert!(report.contacted_peers);
        assert!(report.ticks[0].contacted_peers);
        assert_eq!(report.ticks[0].anti_entropy_summary_count, 1);
        assert_eq!(report.ticks[0].imported_event_count, 4);
        assert!(
            report
                .degraded
                .iter()
                .all(|item| item.code != MESH_SYNC_ONCE_NETWORK_DEFERRED_CODE),
            "successful fake transport contact should not retain deferred fallback"
        );
        assert!(
            report.degraded.iter().all(|item| {
                item.code
                    != crate::mesh::anti_entropy_protocol::degraded_codes::TRANSPORT_UNAVAILABLE
            }),
            "successful peer contact must not report the anti-entropy transport as unavailable"
        );
        Ok(())
    }

    #[test]
    fn sync_supervisor_observes_lab_runtime_cancellation() -> TestResult {
        let mut lab = LabRuntime::new(LabConfig::new(612));
        let root = lab.state.create_root_region(Budget::INFINITE);
        let snapshot = sample_snapshot(Vec::new());
        let options = MeshSyncSupervisorOptions {
            cadence_ms: 1_000,
            tick_limit: 2,
            ..MeshSyncSupervisorOptions::default()
        };
        let observed_cx: Arc<StdMutex<Option<Cx>>> = Arc::new(StdMutex::new(None));
        let observed_cx_for_task = Arc::clone(&observed_cx);

        let (task_id, mut handle) = lab
            .state
            .create_task(root, Budget::INFINITE, async move {
                let Some(cx) = Cx::current() else {
                    return Outcome::Err("LabRuntime task should install Cx".to_owned());
                };
                {
                    let Ok(mut slot) = observed_cx_for_task.lock() else {
                        return Outcome::Err("mesh cancellation Cx slot poisoned".to_owned());
                    };
                    *slot = Some(cx.clone());
                }
                run_mesh_sync_supervisor_supervised(&cx, &snapshot, &options).await
            })
            .map_err(|error| error.to_string())?;
        lab.scheduler.lock().schedule(task_id, 0);
        lab.run_until_idle();
        assert!(!handle.is_finished());

        let reason = CancelReason::user("mesh sync cancellation test");
        let (tasks_to_cancel, cancellation_wakes) =
            lab.state.cancel_request(root, &reason, None).into_parts();
        for (cancelled_task, priority) in tasks_to_cancel {
            lab.scheduler
                .lock()
                .schedule_cancel(cancelled_task, priority);
        }
        cancellation_wakes.dispatch();
        lab.scheduler.lock().schedule(task_id, 0);
        lab.advance_time(1_000_000_000);
        lab.run_until_quiescent();

        let cancellation_reason = match handle.try_join() {
            Ok(Some(Outcome::Cancelled(reason))) => reason,
            Err(JoinError::Cancelled(reason))
                if reason.message.as_deref() == Some("join channel closed") =>
            {
                observed_cx
                    .lock()
                    .map_err(|_| "mesh cancellation Cx slot poisoned".to_owned())?
                    .as_ref()
                    .and_then(Cx::cancel_reason)
                    .ok_or_else(|| "mesh cancellation reason missing from Cx".to_owned())?
            }
            Err(JoinError::Cancelled(reason)) => reason,
            Ok(Some(other)) => {
                return Err(format!(
                    "mesh cancellation outcome was not cancelled: {other:?}"
                ));
            }
            Ok(None) => return Err("mesh cancellation task did not finish".to_owned()),
            Err(error) => return Err(format!("mesh cancellation join failed: {error}")),
        };
        assert_eq!(
            cancellation_reason.message.as_deref(),
            Some("mesh sync cancellation test")
        );
        Ok(())
    }

    fn run_supervisor_in_lab(
        snapshot: MeshForegroundSnapshot,
        options: MeshSyncSupervisorOptions,
        seed: u64,
    ) -> Result<super::MeshSyncSupervisorReport, String> {
        run_supervisor_in_lab_with_transport(
            snapshot,
            options,
            super::NoopMeshForegroundSyncTransport,
            seed,
        )
    }

    fn run_supervisor_in_lab_with_transport<T>(
        snapshot: MeshForegroundSnapshot,
        options: MeshSyncSupervisorOptions,
        mut transport: T,
        seed: u64,
    ) -> Result<super::MeshSyncSupervisorReport, String>
    where
        T: MeshForegroundSyncTransport + Send + 'static,
    {
        let mut lab = LabRuntime::new(LabConfig::new(seed));
        let root = lab.state.create_root_region(Budget::INFINITE);
        let (task_id, mut handle) = lab
            .state
            .create_task(root, Budget::INFINITE, async move {
                let Some(cx) = Cx::current() else {
                    return Outcome::Err("LabRuntime task should install Cx".to_owned());
                };
                run_mesh_sync_supervisor_supervised_with_transport(
                    &cx,
                    &snapshot,
                    &options,
                    &mut transport,
                )
                .await
            })
            .map_err(|error| error.to_string())?;
        lab.scheduler.lock().schedule(task_id, 0);
        lab.run_until_quiescent();

        match handle
            .try_join()
            .map_err(|error| format!("mesh supervisor lab join failed: {error}"))?
            .ok_or_else(|| "mesh supervisor lab task did not finish".to_owned())?
        {
            Outcome::Ok(report) => Ok(report),
            other => Err(format!("mesh supervisor lab outcome was not ok: {other:?}")),
        }
    }

    struct FakeForegroundSyncTransport;

    impl MeshForegroundSyncTransport for FakeForegroundSyncTransport {
        fn contact_peer(
            &mut self,
            request: MeshForegroundSyncRequest<'_>,
        ) -> MeshForegroundSyncPeerOutcome {
            assert_eq!(request.peer.peer_id, "peer-b");
            assert_eq!(request.peer_record.peer_id, "peer-b");
            assert!(request.peer_record.is_trusted());
            assert_eq!(request.snapshot.workspace_id, "wsp_test");
            assert_eq!(request.options.peer_concurrency, 1);
            MeshForegroundSyncPeerOutcome {
                contacted: true,
                events_accepted: 1,
                events_duplicate: 0,
                events_forked: 0,
                ranges_requested: 1,
                ranges_fulfilled: 1,
                imported_event_count: 1,
            }
        }
    }

    fn sample_snapshot(peers: Vec<MeshPeerRow>) -> MeshForegroundSnapshot {
        MeshForegroundSnapshot {
            workspace_id: "wsp_test".to_owned(),
            workspace_path: "/tmp/ee".to_owned(),
            database_path: "/tmp/ee/.ee/ee.db".to_owned(),
            initialized: true,
            mesh_enabled: true,
            mode: "cache".to_owned(),
            storage: MeshStorageCounts {
                peer_count: peers.len() as u32,
                cursor_count: 0,
                imported_event_count: 3,
                policy_decision_event_count: 0,
                policy_failure_event_count: 0,
                mapped_memory_count: 0,
                cached_body_count: 0,
            },
            peers,
            cursors: Vec::new(),
            events: Vec::new(),
            degraded: Vec::new(),
        }
    }

    fn auto_status_policy_registry() -> MeshPeerPolicyRegistry {
        let config = ConfigFile::parse(
            r#"
[[mesh.peer_policies]]
policy_id = "pol_status"
workspace_id = "wsp_test"
peer_id = "peer-a"
origin_workspace_ids = ["wsp_test"]
trust_lane = "peerAgent"
import_trust_class = "agent_validated"
default_action = "deny"

[mesh.peer_policies.allowed_lanes]
metadata = "quarantine"
body = "allow"
embedding = "deny"
graph_link = "deny"
revision_notice = "allow"
curation_signal = "deny"

[mesh.peer_policies.redaction]
metadata = "redact"
preview = "redact"
body = "share"
embedding = "deny"

[mesh.peer_policies.body_fetch]
allowed = true
requires_consent = false
max_bytes = 1048576
"#,
        )
        .expect("mesh status policy config should parse");
        MeshPeerPolicyRegistry::from_config(&config)
    }

    #[test]
    fn import_ledger_report_exposes_local_verdict_without_event_body() {
        let mut event = export_event("ledger_secret_body", "metadata", Some("body-cache-secret"));
        event.policy_decision_json = Some(
            serde_json::json!({
                "schema": "ee.mesh.policy_decision.v1",
                "direction": "inbound",
                "action": "allow",
                "reason": "policy_allows_lane",
            })
            .to_string(),
        );
        let mut snapshot = sample_snapshot(Vec::new());
        snapshot.events = vec![event];

        let report = snapshot
            .import_ledger_report()
            .expect("stored policy JSON should decode");
        assert_eq!(report.schema, MESH_IMPORT_LEDGER_SCHEMA_V1);
        assert_eq!(report.event_count, 1);
        assert_eq!(report.events[0].import_decision, "allow");
        assert_eq!(
            report.events[0]
                .policy_decision
                .as_ref()
                .and_then(|value| { value.get("reason").and_then(serde_json::Value::as_str) }),
            Some("policy_allows_lane")
        );

        let rendered = serde_json::to_value(&report).expect("serialize ledger report");
        assert!(rendered["events"][0].get("eventJson").is_none());
        assert!(rendered["events"][0].get("bodyCacheKey").is_none());
        assert!(!rendered.to_string().contains("body-cache-secret"));
    }

    fn sample_peer(peer_id: &str, enabled: bool) -> MeshPeerRow {
        MeshPeerRow {
            peer_id: peer_id.to_owned(),
            origin_node_id: format!("{peer_id}-origin"),
            display_name: Some(peer_id.to_owned()),
            enabled,
            last_seen_at: "2026-05-20T00:00:00Z".to_owned(),
            policy_summary_json: None,
        }
    }

    #[test]
    fn tcp_sync_transport_contacts_live_loopback_hello() {
        use crate::mesh::bootstrap_envelope::{
            BootstrapCapability, decode_envelope, encode_envelope, read_std_framed,
            write_std_framed,
        };
        use crate::mesh::hello::HELLO_RESPONSE_SCHEMA_V1;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let request = read_std_framed(&mut stream).expect("read");
            let envelope = decode_envelope(&request).expect("decode");
            assert_eq!(envelope.capability, BootstrapCapability::Hello);
            let reply = encode_envelope(
                BootstrapCapability::Hello,
                serde_json::json!({
                    "schema": HELLO_RESPONSE_SCHEMA_V1,
                    "requestId": "sync:peer_live",
                    "responderNodeKey": "nodekey:live",
                    "responderEeVersion": "0.0.0",
                    "responderEeProtocolVersion": "1.0",
                    "responderWorkspaceIds": ["wsp_peer"],
                    "responderCapabilities": ["hello"],
                    "discoveryConsent": true,
                    "responseElapsedMicros": 1
                }),
            )
            .expect("encode");
            write_std_framed(&mut stream, &reply).expect("write");
            let sync_request = read_std_framed(&mut stream).expect("read sync");
            let _ = crate::mesh::bootstrap_envelope::parse_sync_round_request(&sync_request)
                .expect("sync request");
            let sync_reply =
                serde_json::to_vec(&crate::mesh::bootstrap_envelope::SyncRoundResponse {
                    schema: crate::mesh::bootstrap_envelope::SYNC_ROUND_SCHEMA_V1.to_owned(),
                    tips: vec![crate::mesh::bootstrap_envelope::SyncRoundTip {
                        origin_node_id: "peer_live-origin".to_owned(),
                        origin_workspace_id: "wsp_peer".to_owned(),
                        last_seq: 1,
                        tip_event_hash: Some("blake3:tip".to_owned()),
                    }],
                    events: vec![crate::mesh::bootstrap_envelope::SyncRoundEvent {
                        origin_node_id: "peer_live-origin".to_owned(),
                        origin_workspace_id: "wsp_peer".to_owned(),
                        seq: 1,
                        event_hash: "blake3:evt".to_owned(),
                        payload_json: "{\"schema\":\"ee.mesh.event.v1\"}".to_owned(),
                    }],
                })
                .expect("encode sync");
            write_std_framed(&mut stream, &sync_reply).expect("write sync");
        });

        let mut peer = sample_trusted_sync_peer("peer_live");
        let mut record: MeshPeerRecord = serde_json::from_str(
            peer.policy_summary_json
                .as_deref()
                .expect("trusted peer record"),
        )
        .expect("parse record");
        record.endpoint.endpoint = address.to_string();
        peer.policy_summary_json =
            Some(serde_json::to_string(&record).expect("serialize live peer"));
        let snapshot = MeshForegroundSnapshot {
            workspace_id: "wsp_test".to_owned(),
            workspace_path: "/tmp/ee-mesh-sync-test".to_owned(),
            database_path: "/tmp/ee-mesh-sync-test.db".to_owned(),
            initialized: true,
            mesh_enabled: true,
            mode: "foreground".to_owned(),
            storage: MeshStorageCounts {
                peer_count: 1,
                cursor_count: 0,
                imported_event_count: 0,
                policy_decision_event_count: 0,
                policy_failure_event_count: 0,
                mapped_memory_count: 0,
                cached_body_count: 0,
            },
            peers: vec![peer.clone()],
            cursors: Vec::new(),
            events: Vec::new(),
            degraded: Vec::new(),
        };
        let options = MeshSyncSupervisorOptions::default();
        let mut transport = TcpMeshForegroundSyncTransport {
            committed_port: address.port(),
            requester_node_key: "wsp_test".to_owned(),
            requester_workspace_ids: vec!["wsp_test".to_owned()],
            timeout: std::time::Duration::from_secs(2),
        };
        let outcome = transport.contact_peer(MeshForegroundSyncRequest {
            snapshot: &snapshot,
            options: &options,
            peer: &peer,
            peer_record: &record,
        });
        assert!(
            outcome.contacted,
            "live TCP hello must count as contact, got {outcome:?}"
        );
        assert_eq!(outcome.imported_event_count, 1);
        assert_eq!(outcome.ranges_requested, 1);
        assert_eq!(outcome.ranges_fulfilled, 1);
        server.join().expect("server");
    }

    const PERSIST_WORKSPACE_ID: &str = "wsp_persistfixture000000000001";

    fn persist_fixture_workspace(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let workspace = std::env::temp_dir().join(format!("ee-mesh-persist-{label}-{stamp}"));
        std::fs::create_dir_all(workspace.join(".ee")).expect("workspace");
        let database_path = workspace.join("ee.db");
        let connection = DbConnection::open_file(&database_path).expect("open persist db");
        connection.migrate().expect("migrate persist db");
        connection
            .insert_workspace(
                PERSIST_WORKSPACE_ID,
                &CreateWorkspaceInput {
                    path: workspace.display().to_string(),
                    name: Some("persist fixture".to_owned()),
                },
            )
            .expect("insert persist workspace");
        (workspace, database_path)
    }

    fn persist_snapshot(
        workspace: std::path::PathBuf,
        database_path: std::path::PathBuf,
    ) -> MeshForegroundSnapshot {
        MeshForegroundSnapshot {
            workspace_id: PERSIST_WORKSPACE_ID.to_owned(),
            workspace_path: workspace.display().to_string(),
            database_path: database_path.display().to_string(),
            initialized: true,
            mesh_enabled: true,
            mode: "foreground".to_owned(),
            storage: MeshStorageCounts::default(),
            peers: Vec::new(),
            cursors: Vec::new(),
            events: Vec::new(),
            degraded: Vec::new(),
        }
    }

    fn persist_origin_event() -> crate::mesh::bootstrap_envelope::SyncRoundEvent {
        crate::mesh::bootstrap_envelope::SyncRoundEvent {
            origin_node_id: "node_peer".to_owned(),
            origin_workspace_id: "wsp_peer".to_owned(),
            seq: 0,
            event_hash: "blake3:persist-evt".to_owned(),
            payload_json: r#"{"schema":"ee.mesh.event.v1","eventId":"evt_persist"}"#.to_owned(),
        }
    }

    #[test]
    fn persist_sync_round_events_skips_signed_inbound_that_fails_ingest() {
        let (workspace, database_path) = persist_fixture_workspace("signed-skip");
        let snapshot = persist_snapshot(workspace, database_path.clone());
        let inbound = crate::mesh::origin_stream::InboundOriginEvent {
            schema: crate::mesh::origin_stream::ORIGIN_EVENT_SCHEMA_V1.to_owned(),
            event_id: "mesh_oevt_aaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            team_id: "team_x".to_owned(),
            origin_node_id: "node_peer".to_owned(),
            signing_key_generation: 1,
            seq: 0,
            prev_event_hash: None,
            event_hash: "blake3:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
                .to_owned(),
            signature: format!("ed25519:{}", "00".repeat(64)),
            payload_schema: crate::mesh::origin_stream::MEMORY_EVENT_PAYLOAD_SCHEMA_V1.to_owned(),
            payload: serde_json::json!({
                "operation": "create",
                "logicalMemoryId": "olm_00000000000000000000000009",
                "revisionId": "rev_0",
                "bodyCommitment": "blake3:aa"
            }),
            required_features: Vec::new(),
            produced_at: "2026-08-13T00:00:00Z".to_owned(),
        };
        let event = crate::mesh::bootstrap_envelope::SyncRoundEvent {
            origin_node_id: "node_peer".to_owned(),
            origin_workspace_id: "wsp_peer".to_owned(),
            seq: 0,
            event_hash: inbound.event_hash.clone(),
            payload_json: serde_json::to_string(&inbound).expect("serialize inbound"),
        };
        let accepted = persist_sync_round_events(&snapshot, "peer_live", &[event]);
        assert_eq!(accepted, 0);
    }

    #[cfg(unix)]
    #[test]
    fn persist_sync_round_events_accepts_signed_inbound_from_a_bound_member() {
        let producer_dir = tempfile::tempdir().expect("producer workspace");
        let producer_db = producer_dir.path().join("ee.db");
        let producer = DbConnection::open_file(&producer_db).expect("open producer");
        producer.migrate().expect("migrate producer");
        producer
            .insert_workspace(
                PERSIST_WORKSPACE_ID,
                &CreateWorkspaceInput {
                    path: producer_dir.path().display().to_string(),
                    name: Some("producer".to_owned()),
                },
            )
            .expect("insert producer workspace");
        let created = crate::mesh::team::create_local_team_with_store(
            &producer,
            PERSIST_WORKSPACE_ID,
            "Analysts",
            "2026-08-13T00:00:00Z",
            Some(producer_dir.path()),
        )
        .expect("create");
        let inbound = crate::mesh::origin_stream::inbound_from_stored(
            &producer.list_mesh_manifest_origin_events(8).expect("list")[0],
        )
        .expect("inbound");
        let node = producer
            .get_team_member_node(&created.team.origin_node_id, 1)
            .expect("load node")
            .expect("bound");

        let (workspace, database_path) = persist_fixture_workspace("signed-allow");
        let receiver = DbConnection::open_file(&database_path).expect("open receiver");
        receiver
            .insert_team_member(&crate::db::InsertTeamMemberInput {
                member_id: "mbr_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                team_id: created.team.team_id.clone(),
                workspace_id: PERSIST_WORKSPACE_ID.to_owned(),
                display_name: "origin".to_owned(),
                state: "active".to_owned(),
                is_self: false,
                origin_node_id: created.team.origin_node_id.clone(),
                bound_via: "invite_ceremony".to_owned(),
                joined_at: "2026-08-13T00:00:00Z".to_owned(),
            })
            .expect("insert member");
        receiver
            .insert_team_member_node(&crate::db::InsertTeamMemberNodeInput {
                node_id: created.team.origin_node_id.clone(),
                member_id: "mbr_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                team_id: created.team.team_id.clone(),
                verifying_key_hex: node.verifying_key_hex,
                signing_key_generation: 1,
                state: "active".to_owned(),
                bound_at: "2026-08-13T00:00:00Z".to_owned(),
            })
            .expect("insert node");
        let snapshot = persist_snapshot(workspace, database_path.clone());
        let event = crate::mesh::bootstrap_envelope::SyncRoundEvent {
            origin_node_id: created.team.origin_node_id.clone(),
            origin_workspace_id: "wsp_peer".to_owned(),
            seq: inbound.seq,
            event_hash: inbound.event_hash.clone(),
            payload_json: serde_json::to_string(&inbound).expect("serialize inbound"),
        };
        let accepted = persist_sync_round_events(&snapshot, "peer_live", &[event]);
        assert_eq!(accepted, 1);
        let rows = DbConnection::open_file(&database_path)
            .expect("reopen")
            .list_mesh_import_ledger_events(
                PERSIST_WORKSPACE_ID,
                &created.team.origin_node_id,
                "wsp_peer",
            )
            .expect("ledger");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].import_decision, "allow");
    }

    #[test]
    fn persist_sync_round_events_bootstrap_allows_without_bindings() {
        let (workspace, database_path) = persist_fixture_workspace("allow");
        let snapshot = persist_snapshot(workspace, database_path.clone());
        let accepted = persist_sync_round_events(&snapshot, "peer_live", &[persist_origin_event()]);
        assert_eq!(accepted, 1);
        let connection = DbConnection::open_file(&database_path).expect("reopen");
        assert!(
            connection
                .list_mesh_origin_events("team-mesh", "node_peer", 0, 8)
                .expect("origin")
                .is_empty(),
            "inbound sync must not echo into the origin chain"
        );
        let rows = connection
            .list_mesh_import_ledger_events(PERSIST_WORKSPACE_ID, "node_peer", "wsp_peer")
            .expect("ledger");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].import_decision, "allow");
    }

    #[test]
    fn persist_sync_round_events_denies_when_peer_group_binding_denies() {
        let (workspace, database_path) = persist_fixture_workspace("deny");
        std::fs::write(
            workspace.join(".ee/config.toml"),
            r#"
[[mesh.peer_group_bindings]]
workspace_id = "wsp_persistfixture000000000001"
peer_group_id = "pg_deny"
peer_ids = ["peer_live"]
origin_workspace_ids = ["wsp_peer"]

[mesh.peer_group_bindings.lanes]
metadata = "deny"
"#,
        )
        .expect("write deny binding");
        let snapshot = persist_snapshot(workspace, database_path.clone());
        let accepted = persist_sync_round_events(&snapshot, "peer_live", &[persist_origin_event()]);
        assert_eq!(accepted, 0);
        let connection = DbConnection::open_file(&database_path).expect("reopen");
        assert!(
            connection
                .list_mesh_origin_events("team-mesh", "node_peer", 0, 8)
                .expect("origin")
                .is_empty()
        );
        let rows = connection
            .list_mesh_import_ledger_events(PERSIST_WORKSPACE_ID, "node_peer", "wsp_peer")
            .expect("ledger");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].import_decision, "deny");
    }

    #[test]
    fn local_sync_round_request_starts_after_the_stored_cursor() {
        let peer = sample_trusted_sync_peer("peer_live");
        let record: MeshPeerRecord = serde_json::from_str(
            peer.policy_summary_json
                .as_deref()
                .expect("trusted peer record"),
        )
        .expect("parse record");
        let snapshot = MeshForegroundSnapshot {
            workspace_id: "wsp_test".to_owned(),
            workspace_path: "/tmp/ee-mesh-cursor".to_owned(),
            database_path: "/tmp/ee-mesh-cursor.db".to_owned(),
            initialized: true,
            mesh_enabled: true,
            mode: "foreground".to_owned(),
            storage: MeshStorageCounts::default(),
            peers: vec![peer.clone()],
            cursors: vec![MeshCursorRow {
                peer_id: "peer_live".to_owned(),
                origin_node_id: "node_peer".to_owned(),
                origin_workspace_id: "wsp_peer".to_owned(),
                last_seq: 7,
                tip_event_hash: Some("blake3:tip7".to_owned()),
                tip_audit_hash: None,
                status: "current".to_owned(),
                updated_at: "2026-08-12T00:00:00Z".to_owned(),
            }],
            events: Vec::new(),
            degraded: Vec::new(),
        };
        let request = local_sync_round_request(&snapshot, &peer, &record);
        assert_eq!(request.range_start_seq, 8);
        assert_eq!(request.tips[0].last_seq, 7);
        assert_eq!(
            request.tips[0].tip_event_hash.as_deref(),
            Some("blake3:tip7")
        );
    }

    fn sample_trusted_sync_peer(peer_id: &str) -> MeshPeerRow {
        let mut peer = sample_peer(peer_id, true);
        let record = MeshPeerRecord {
            schema: MESH_PEER_RECORD_SCHEMA_V1.to_owned(),
            peer_id: peer_id.to_owned(),
            alias: peer_id.to_owned(),
            workspace_id: "wsp_peer".to_owned(),
            origin_workspace_id: String::new(),
            endpoint: MeshPeerEndpoint {
                tailscale_node_key: format!("{peer_id}-node"),
                tailnet_id: "tailnet-test".to_owned(),
                tailnet_display_name: Some("test tailnet".to_owned()),
                endpoint: format!("https://{peer_id}.tailnet.test/ee/mesh"),
                magic_dns_name: Some(format!("{peer_id}.tailnet.test")),
            },
            materialized_on_node_key: None,
            capabilities: MeshPeerCapabilities::from_profile(
                MeshPeerCapabilityProfile::MetadataOnly,
            ),
            handshake: MeshPeerHandshake::granted(
                "req-test",
                "1.0",
                format!("{peer_id}-node"),
                vec!["mesh:metadata".to_owned()],
            ),
            key: MeshPeerKey {
                generation: 1,
                public_key_fingerprint: format!("{peer_id}-fingerprint"),
                created_at: "2026-05-20T00:00:00Z".to_owned(),
                rotated_at: None,
                revoked_at: None,
            },
            state: MeshPeerState::Active,
            enrolled_at: "2026-05-20T00:00:00Z".to_owned(),
            revoked_at: None,
            trust_established_by: "explicit_human_consent".to_owned(),
        };
        peer.policy_summary_json =
            Some(serde_json::to_string(&record).expect("sample mesh peer record should serialize"));
        peer
    }

    fn sample_auto_enrolled_peer(peer_id: &str, materialized_on_node_key: &str) -> MeshPeerRow {
        let mut peer = sample_trusted_sync_peer(peer_id);
        let policy_summary_json = peer
            .policy_summary_json
            .as_deref()
            .expect("sample trusted peer should have policy summary");
        let mut record: MeshPeerRecord =
            serde_json::from_str(policy_summary_json).expect("sample record should deserialize");
        record.trust_established_by = "tailscale_auto_enrollment".to_owned();
        record.materialized_on_node_key = Some(materialized_on_node_key.to_owned());
        peer.policy_summary_json = Some(
            serde_json::to_string(&record).expect("sample auto-enrolled peer should serialize"),
        );
        peer
    }

    fn sample_autodiscovery(tailnet_id: &str, self_node_key: &str) -> TailscaleAutodiscoveryReport {
        TailscaleAutodiscoveryReport {
            schema: TAILSCALE_AUTODISCOVERY_SCHEMA_V1,
            tailnet_id: Some(tailnet_id.to_owned()),
            tailnet_display_name: Some("test tailnet".to_owned()),
            self_node_key: Some(self_node_key.to_owned()),
            probed_peer_count: 1,
            eligible_peer_count: 1,
            ee_capable_peers: Vec::new(),
            skipped_peers: Vec::new(),
            degraded: Vec::new(),
        }
    }
}
