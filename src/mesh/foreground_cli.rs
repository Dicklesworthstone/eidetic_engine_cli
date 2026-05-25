//! Foreground `ee mesh` CLI support.
//!
//! These types deliberately model local, daemon-free operations only. They are
//! safe to use with mesh disabled, with no Tailscale installation, and against a
//! workspace that has no mesh rows yet.

use std::time::Duration;

use asupersync::runtime::yield_now::yield_now;
use asupersync::time::sleep as asupersync_sleep;
use asupersync::{CancelReason, Cx, Outcome};
use serde::{Deserialize, Serialize};

use crate::db::{
    MeshStorageStatus, StoredMeshImportLedgerEvent, StoredMeshPeer, StoredMeshPeerCursor,
};
use crate::mesh::anti_entropy_protocol::{
    MeshAntiEntropyRetryPolicy, MeshRoundPeerOutcome, MeshSyncSummaryInput, build_sync_summary,
};
use crate::mesh::identity_change_guard::{
    AUTO_ENROLLMENT_NODE_KEY_CHANGED_CODE, AUTO_ENROLLMENT_TAILNET_CHANGED_CODE,
};
use crate::mesh::peer::{MESH_PEER_RECORD_SCHEMA_V1, MeshPeerRecord};
use crate::mesh::repair_action_graph::{
    ActionKind, ExecutionContext, ExpectedOutcome, Priority, REPAIR_ACTION_GRAPH_SCHEMA_V1,
    RepairAction, RepairActionGraph, build_repair_action_graph,
};
use crate::mesh::sync::{SelectiveSyncConfig, SelectiveSyncStatusSummary};
use crate::mesh::tailscale_autodiscovery::{
    TAILSCALE_AUTODISCOVERY_SCHEMA_V1, TAILSCALE_PEER_LIST_UNAVAILABLE_CODE,
    TailscaleAutodiscoveryDegradation, TailscaleAutodiscoveryReport,
};
use crate::policy::{
    MeshExportPolicyAttestation, MeshExportSecretScanReport, MeshExportSecretScanSubject,
    scan_mesh_export_subjects,
};

pub const MESH_CLI_STATUS_SCHEMA_V1: &str = "ee.mesh.cli.status.v1";
pub const MESH_CLI_PEERS_SCHEMA_V1: &str = "ee.mesh.cli.peers.v1";
pub const MESH_CLI_EXPORT_SCHEMA_V1: &str = "ee.mesh.cli.export.v1";
pub const MESH_CLI_IMPORT_SCHEMA_V1: &str = "ee.mesh.cli.import.v1";
pub const MESH_CLI_SYNC_SCHEMA_V1: &str = "ee.mesh.cli.sync.v1";
pub const MESH_EXPORT_ARTIFACT_SCHEMA_V1: &str = "ee.mesh.foreground_export.v1";
pub const MESH_AUTO_STATUS_SCHEMA_V1: &str = "ee.mesh.auto_status.v1";

pub const MESH_WORKSPACE_UNINITIALIZED_CODE: &str = "mesh_workspace_uninitialized";
pub const MESH_DISABLED_POSTURE_CODE: &str = "mesh_disabled";
pub const MESH_SYNC_ONCE_NETWORK_DEFERRED_CODE: &str = "mesh_sync_once_network_deferred";
pub const MESH_SYNC_SUPERVISOR_BUDGET_EXHAUSTED_CODE: &str =
    "mesh_sync_supervisor_budget_exhausted";
pub const MESH_SYNC_SUPERVISOR_BACKPRESSURE_CODE: &str = "mesh_sync_supervisor_backpressure";
pub const MESH_SYNC_SUPERVISOR_RUNTIME_ERROR_CODE: &str = "mesh_sync_supervisor_runtime_error";

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
        Self {
            code: MESH_WORKSPACE_UNINITIALIZED_CODE,
            severity: "warning",
            message: format!(
                "Mesh foreground storage was not inspected because {workspace_path}/.ee/ee.db does not exist."
            ),
            repair: format!("Run `ee init --workspace \"{workspace_path}\" --json`."),
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
            repair: "Use `ee mesh export --out mesh-export.json` and `ee mesh import --file mesh-export.json` for local file exchange, or configure an enrolled peer transport before retrying sync."
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAutoPeerStateBreakdown {
    pub active: u32,
    pub soft_stale: u32,
    pub hard_stale: u32,
    pub denylisted: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAutoDriftStatus {
    pub new_peers_available: Vec<String>,
    pub new_peer_count: u32,
    pub stale_peers_in_config: Vec<String>,
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
            time_budget_ms: 5_000,
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

pub async fn run_mesh_sync_supervisor_supervised(
    cx: &Cx,
    snapshot: &MeshForegroundSnapshot,
    options: &MeshSyncSupervisorOptions,
) -> Outcome<MeshSyncSupervisorReport, String> {
    let mut transport = NoopMeshForegroundSyncTransport;
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshCheckedExportArtifact {
    pub artifact: MeshExportArtifact,
    pub secret_scan: MeshExportSecretScanReport,
}

#[derive(Clone, Copy, Debug, Default)]
struct MeshAutoStatusSignals {
    tailscale_authenticated: Option<bool>,
    tailscale_authenticated_for_24h: bool,
    hello_responder_running: Option<bool>,
    discovered_peer_count: u32,
    tailnet_changed: bool,
    node_key_changed: bool,
    manual_conflict_present: bool,
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
                format!("ee init --workspace \"{}\" --json", self.workspace_path),
                format!(
                    "ee mesh export --workspace \"{}\" --out mesh-export.json --json",
                    self.workspace_path
                ),
                format!(
                    "ee mesh import --workspace \"{}\" --file mesh-export.json --json",
                    self.workspace_path
                ),
            ],
            degraded: self.degraded.clone(),
        }
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
    let active_peer_count = active_peer_count(snapshot) as u32;
    let stale_peers_in_config = stale_mesh_peer_ids(snapshot);
    let new_peer_count = signals
        .discovered_peer_count
        .saturating_sub(snapshot.storage.peer_count);
    let drift_severity = auto_drift_severity(snapshot, &signals, new_peer_count);
    let action_graph = auto_status_action_graph(snapshot, &signals);
    let next_action_hint = auto_status_next_action_hint(snapshot, &signals, new_peer_count);
    let mut degraded = snapshot.degraded.clone();
    degraded.extend(auto_status_degradations(snapshot, &signals));

    MeshAutoEnrollmentStatus {
        schema: MESH_AUTO_STATUS_SCHEMA_V1,
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
            shields_up: None,
            binary_authentic: None,
            tailnet_display_name: None,
            peer_count: signals.discovered_peer_count,
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
        discovery: auto_status_discovery_report(&signals),
        discovery_cache: MeshAutoDiscoveryCacheStatus {
            schema: "ee.mesh.discovery_cache.status.v1",
            status: "not_loaded".to_owned(),
            ttl_seconds: 30,
            hit: None,
            refreshed_at: None,
        },
        materialized: auto_materialized_status(snapshot),
        peer_state_breakdown: MeshAutoPeerStateBreakdown {
            active: active_peer_count,
            soft_stale: 0,
            hard_stale: stale_peers_in_config.len() as u32,
            denylisted: 0,
        },
        drift: MeshAutoDriftStatus {
            new_peers_available: Vec::new(),
            new_peer_count,
            stale_peers_in_config,
            transient_unreachable: Vec::new(),
            tailnet_changed: signals.tailnet_changed,
            node_key_changed: signals.node_key_changed,
            manual_conflict_present: signals.manual_conflict_present,
            drift_severity,
            action_graph,
            next_action_hint,
        },
        steward_posture: MeshAutoStewardPosture {
            schema: "ee.mesh.steward_posture.v1",
            status: "not_inspected".to_owned(),
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

fn auto_materialized_status(
    snapshot: &MeshForegroundSnapshot,
) -> Option<MeshAutoMaterializedStatus> {
    if snapshot.storage.peer_count == 0 && snapshot.peers.is_empty() {
        return None;
    }

    let peer_set_hash = mesh_auto_peer_set_hash(&snapshot.peers);
    let digest = peer_set_hash
        .strip_prefix("blake3:")
        .unwrap_or(peer_set_hash.as_str());
    let peer_group_suffix: String = digest.chars().take(16).collect();
    Some(MeshAutoMaterializedStatus {
        peer_group_id: format!("pg_{peer_group_suffix}"),
        peer_set_hash,
        peer_count: snapshot.storage.peer_count,
        lane_policy: MeshAutoLanePolicy {
            metadata: "allow",
            body: "deny",
            embedding: "deny",
            graph_link: "deny",
            revision_notice: "allow",
            curation_signal: "allow",
        },
        bound_tailnet_id: None,
        materialized_on_node_key: None,
        last_materialized_at: latest_peer_seen_at(snapshot),
        enrollment_source: "manual".to_owned(),
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

fn stale_mesh_peer_ids(snapshot: &MeshForegroundSnapshot) -> Vec<String> {
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

    let stale_count = stale_mesh_peer_ids(snapshot).len() as u32;
    if new_peer_count > 2 || stale_count > 2 {
        return "warning";
    }
    if new_peer_count > 0 || stale_count > 0 {
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
    if !snapshot.initialized {
        return format!(
            "Run `ee init --workspace \"{}\" --json` before mesh auto-enrollment can inspect local state.",
            snapshot.workspace_path
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
            "Auto-enrollment is blocked because nodeKeyChanged=true. Run `ee mesh disable --workspace \"{}\" --reason \"restored from different machine\"`, then re-run auto-enroll.",
            snapshot.workspace_path
        );
    }
    if signals.tailnet_changed {
        return format!(
            "Auto-enrollment is blocked because tailnetChanged=true. Run `ee mesh disable --workspace \"{}\"`, then re-run auto-enroll.",
            snapshot.workspace_path
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
            "{} peers discovered. Run `ee mesh auto-enroll --workspace \"{}\"` to enroll them.",
            signals.discovered_peer_count, snapshot.workspace_path
        );
    }
    if new_peer_count > 0 {
        return format!(
            "{new_peer_count} new peers are available. Run `ee mesh auto-enroll --workspace \"{}\"` to reconcile the peer set.",
            snapshot.workspace_path
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
    if signals.tailnet_changed {
        degraded.push(MeshCliDegradation {
            code: AUTO_ENROLLMENT_TAILNET_CHANGED_CODE,
            severity: "medium",
            message: "Auto-enrollment materialized state belongs to a different tailnet."
                .to_owned(),
            repair: format!(
                "Run `ee mesh disable --workspace \"{}\"` and then `ee mesh auto-enroll --workspace \"{}\"`.",
                snapshot.workspace_path, snapshot.workspace_path
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
                "Run `ee mesh disable --workspace \"{}\" --reason \"restored from different machine\"` and then `ee mesh auto-enroll --workspace \"{}\"`.",
                snapshot.workspace_path, snapshot.workspace_path
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
    RepairAction {
        id: "ee_daemon_start".to_owned(),
        kind: ActionKind::EeSubcommand,
        command: format!("ee daemon --foreground --workspace \"{workspace_path}\""),
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
    RepairAction {
        id: "ee_mesh_auto_enroll".to_owned(),
        kind: ActionKind::EeSubcommand,
        command: format!("ee mesh auto-enroll --workspace \"{workspace_path}\""),
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
            "ee mesh disable --workspace \"{workspace_path}\" --reason \"revert auto-enrollment\""
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
    RepairAction {
        id: "ee_mesh_disable".to_owned(),
        kind: ActionKind::EeSubcommand,
        command: format!("ee mesh disable --workspace \"{workspace_path}\" --reason \"{reason}\""),
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
    use super::{
        AUTO_ENROLLMENT_NODE_KEY_CHANGED_CODE, AUTO_ENROLLMENT_TAILNET_CHANGED_CODE,
        MESH_AUTO_STATUS_SCHEMA_V1, MESH_EXPORT_ARTIFACT_SCHEMA_V1,
        MESH_SYNC_ONCE_NETWORK_DEFERRED_CODE, MESH_SYNC_SUPERVISOR_BACKPRESSURE_CODE,
        MESH_SYNC_SUPERVISOR_BUDGET_EXHAUSTED_CODE, MESH_WORKSPACE_UNINITIALIZED_CODE,
        MeshAutoStatusSignals, MeshCliDegradation, MeshForegroundSnapshot,
        MeshForegroundSyncPeerOutcome, MeshForegroundSyncRequest, MeshForegroundSyncTransport,
        MeshPeerRow, MeshStorageCounts, MeshSyncSupervisorOptions, REPAIR_ACTION_GRAPH_SCHEMA_V1,
        auto_enrollment_status_for_snapshot, run_mesh_sync_supervisor_supervised,
        run_mesh_sync_supervisor_supervised_with_transport,
    };
    use crate::mesh::peer::{
        MESH_PEER_RECORD_SCHEMA_V1, MeshPeerCapabilities, MeshPeerCapabilityProfile,
        MeshPeerEndpoint, MeshPeerHandshake, MeshPeerKey, MeshPeerRecord, MeshPeerState,
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
        assert_eq!(report.auto_enrollment.schema, MESH_AUTO_STATUS_SCHEMA_V1);
        assert!(!report.auto_enrollment.enabled);
        assert!(report.auto_enrollment.read_only);
        assert_eq!(
            report.auto_enrollment.drift.action_graph.schema,
            REPAIR_ACTION_GRAPH_SCHEMA_V1
        );
    }

    #[test]
    fn auto_status_view_action_graph_field_validates_against_ee_repair_action_graph_v1_schema() {
        let snapshot = sample_snapshot(Vec::new());
        let auto_status = snapshot.status_report().auto_enrollment;

        assert_eq!(auto_status.schema, MESH_AUTO_STATUS_SCHEMA_V1);
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
        for (cancelled_task, priority) in lab.state.cancel_request(root, &reason, None) {
            lab.scheduler.lock().schedule(cancelled_task, priority);
        }
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

    fn sample_trusted_sync_peer(peer_id: &str) -> MeshPeerRow {
        let mut peer = sample_peer(peer_id, true);
        let record = MeshPeerRecord {
            schema: MESH_PEER_RECORD_SCHEMA_V1.to_owned(),
            peer_id: peer_id.to_owned(),
            alias: peer_id.to_owned(),
            workspace_id: "wsp_peer".to_owned(),
            endpoint: MeshPeerEndpoint {
                tailscale_node_key: format!("{peer_id}-node"),
                tailnet_id: "tailnet-test".to_owned(),
                tailnet_display_name: Some("test tailnet".to_owned()),
                endpoint: format!("https://{peer_id}.tailnet.test/ee/mesh"),
                magic_dns_name: Some(format!("{peer_id}.tailnet.test")),
            },
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
}
