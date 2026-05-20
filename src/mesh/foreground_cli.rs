//! Foreground `ee mesh` CLI support.
//!
//! These types deliberately model local, daemon-free operations only. They are
//! safe to use with mesh disabled, with no Tailscale installation, and against a
//! workspace that has no mesh rows yet.

use serde::{Deserialize, Serialize};

use crate::db::{
    MeshStorageStatus, StoredMeshImportLedgerEvent, StoredMeshPeer, StoredMeshPeerCursor,
};
use crate::mesh::sync::{SelectiveSyncConfig, SelectiveSyncStatusSummary};
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

pub const MESH_WORKSPACE_UNINITIALIZED_CODE: &str = "mesh_workspace_uninitialized";
pub const MESH_DISABLED_POSTURE_CODE: &str = "mesh_disabled";
pub const MESH_SYNC_ONCE_NETWORK_DEFERRED_CODE: &str = "mesh_sync_once_network_deferred";

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
            message: "Foreground sync --once did not contact peers because SRR6.7 anti-entropy transport is not implemented yet."
                .to_owned(),
            repair: "Use `ee mesh export --out <file>` and `ee mesh import --file <file>` for local file exchange until peer sync lands."
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
    pub repair_commands: Vec<String>,
    pub degraded: Vec<MeshCliDegradation>,
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
    pub export_command: String,
    pub import_command: String,
    pub degraded: Vec<MeshCliDegradation>,
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
        MESH_EXPORT_ARTIFACT_SCHEMA_V1, MESH_WORKSPACE_UNINITIALIZED_CODE, MeshCliDegradation,
        MeshForegroundSnapshot, MeshStorageCounts,
    };

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
}
