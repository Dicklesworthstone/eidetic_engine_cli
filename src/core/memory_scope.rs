use std::collections::BTreeSet;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use serde_json::{Value as JsonValue, json};

use crate::config::{
    ConfigFile, EnvVar, MeshLane, MeshLaneDecision, MeshLaneGrants, MeshPeerGroupBinding,
    read_env_var,
};
use crate::db::StoredMemory;
use crate::models::{MemoryScope, MemoryScopeStats, TrustClass};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryScopeContext {
    pub scope: MemoryScope,
    pub strict_scope: bool,
    pub current_agent: Option<String>,
    pub team_members: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshEventValidity {
    Valid,
    PolicyQuarantine,
    Malformed,
    Unsafe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshTrustLane {
    LocalHuman,
    PeerHumanViaPeer,
    PeerAgent,
    PeerDerived,
    Untrusted,
}

impl MeshTrustLane {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalHuman => "localHuman",
            Self::PeerHumanViaPeer => "peerHumanViaPeer",
            Self::PeerAgent => "peerAgent",
            Self::PeerDerived => "peerDerived",
            Self::Untrusted => "untrusted",
        }
    }

    #[must_use]
    pub const fn permits_peer_import(self) -> bool {
        !matches!(self, Self::LocalHuman)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseMeshTrustLaneError {
    input: String,
}

impl std::fmt::Display for ParseMeshTrustLaneError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "unknown mesh trust lane `{}`; expected one of localHuman, peerHumanViaPeer, peerAgent, peerDerived, untrusted",
            self.input
        )
    }
}

impl std::error::Error for ParseMeshTrustLaneError {}

impl FromStr for MeshTrustLane {
    type Err = ParseMeshTrustLaneError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "localHuman" => Ok(Self::LocalHuman),
            "peerHumanViaPeer" => Ok(Self::PeerHumanViaPeer),
            "peerAgent" => Ok(Self::PeerAgent),
            "peerDerived" => Ok(Self::PeerDerived),
            "untrusted" => Ok(Self::Untrusted),
            _ => Err(ParseMeshTrustLaneError {
                input: input.to_owned(),
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshRedactionDecision {
    Share,
    Redact,
    Deny,
}

impl MeshRedactionDecision {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Share => "share",
            Self::Redact => "redact",
            Self::Deny => "deny",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshRedactionPolicy {
    pub metadata: MeshRedactionDecision,
    pub preview: MeshRedactionDecision,
    pub body: MeshRedactionDecision,
    pub embedding: MeshRedactionDecision,
}

impl MeshRedactionPolicy {
    #[must_use]
    pub const fn decision_for_lane(&self, lane: MeshLane) -> MeshRedactionDecision {
        match lane {
            MeshLane::Metadata
            | MeshLane::GraphLink
            | MeshLane::RevisionNotice
            | MeshLane::CurationSignal => self.metadata,
            MeshLane::Body => self.body,
            MeshLane::Embedding => self.embedding,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeshBodyFetchPolicy {
    pub allowed: bool,
    pub requires_consent: bool,
    pub max_bytes: Option<usize>,
}

impl MeshBodyFetchPolicy {
    #[must_use]
    pub const fn denied() -> Self {
        Self {
            allowed: false,
            requires_consent: true,
            max_bytes: Some(0),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshImportDecisionKind {
    Allow,
    Quarantine,
    Deny,
    Reject,
}

impl MeshImportDecisionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Quarantine => "quarantine",
            Self::Deny => "deny",
            Self::Reject => "reject",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshImportDecisionInput<'a> {
    pub local_workspace_id: &'a str,
    pub origin_workspace_id: &'a str,
    pub producer_peer_id: &'a str,
    pub material_lane: MeshLane,
    pub event_validity: MeshEventValidity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshPeerPolicy {
    pub policy_id: String,
    pub workspace_id: String,
    pub peer_id: String,
    pub origin_workspace_ids: Vec<String>,
    pub trust_lane: MeshTrustLane,
    pub import_trust_class: TrustClass,
    pub allowed_lanes: MeshLaneGrants,
    pub redaction: MeshRedactionPolicy,
    pub body_fetch: MeshBodyFetchPolicy,
    pub default_action: MeshLaneDecision,
}

impl From<&crate::config::MeshPeerPolicyConfig> for MeshPeerPolicy {
    fn from(config: &crate::config::MeshPeerPolicyConfig) -> Self {
        Self {
            policy_id: config.policy_id.clone(),
            workspace_id: config.workspace_id.clone(),
            peer_id: config.peer_id.clone(),
            origin_workspace_ids: config.origin_workspace_ids.clone(),
            trust_lane: mesh_trust_lane_from_config(config.trust_lane),
            import_trust_class: config.import_trust_class,
            allowed_lanes: config.allowed_lanes.clone(),
            redaction: MeshRedactionPolicy {
                metadata: mesh_redaction_decision_from_config(config.redaction.metadata),
                preview: mesh_redaction_decision_from_config(config.redaction.preview),
                body: mesh_redaction_decision_from_config(config.redaction.body),
                embedding: mesh_redaction_decision_from_config(config.redaction.embedding),
            },
            body_fetch: MeshBodyFetchPolicy {
                allowed: config.body_fetch.allowed,
                requires_consent: config.body_fetch.requires_consent,
                max_bytes: config.body_fetch.max_bytes,
            },
            default_action: config.default_action,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshPeerPolicyDecisionInput<'a> {
    pub local_workspace_id: &'a str,
    pub origin_workspace_id: &'a str,
    pub producer_peer_id: &'a str,
    pub material_lane: MeshLane,
    pub event_validity: MeshEventValidity,
    pub requested_body_bytes: Option<usize>,
    pub body_fetch_consent: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshPeerPolicyDecision {
    pub import: MeshImportDecision,
    pub policy_id: Option<String>,
    pub trust_lane: Option<MeshTrustLane>,
    pub import_trust_class: Option<TrustClass>,
    pub redaction: MeshRedactionDecision,
    pub body_fetch_allowed: bool,
}

impl MeshPeerPolicyDecision {
    #[must_use]
    pub fn permits_import_as_human_explicit(&self) -> bool {
        false
    }

    #[must_use]
    pub fn permits_body_fetch(&self) -> bool {
        self.body_fetch_allowed
            && self.import.material_lane == MeshLane::Body
            && self.import.workspace_scope_decision == MeshImportDecisionKind::Allow
    }

    #[must_use]
    pub fn redaction_posture(&self) -> &'static str {
        self.redaction.as_str()
    }

    #[must_use]
    pub fn to_json(&self) -> JsonValue {
        json!({
            "schema": "ee.mesh.policy_decision.v1",
            "direction": "inbound",
            "action": self.import.workspace_scope_decision.as_str(),
            "reason": self.import.reason,
            "policyRef": mesh_policy_ref(self.policy_id.as_deref()),
            "materialLane": mesh_lane_name(self.import.material_lane),
            "redaction": self.redaction.as_str(),
            "trustLane": self.trust_lane.map(MeshTrustLane::as_str),
            "importTrustClass": self.import_trust_class.map(TrustClass::as_str),
            "bodyFetchAllowed": self.body_fetch_allowed,
            "localTruthSideEffectsAllowed": self.import.permits_local_truth_side_effects(),
            "searchOrGraphSideEffectsAllowed": self.import.permits_search_or_graph_side_effects(),
            "failure": self.failure_surface().map(|surface| surface.to_json()),
        })
    }

    #[must_use]
    pub fn failure_surface(&self) -> Option<MeshPolicyFailureSurface> {
        let code = match self.import.workspace_scope_decision {
            MeshImportDecisionKind::Allow => return None,
            MeshImportDecisionKind::Quarantine => "mesh_peer_policy_quarantined",
            MeshImportDecisionKind::Deny => "mesh_peer_policy_denied",
            MeshImportDecisionKind::Reject => "mesh_peer_policy_rejected",
        };

        Some(MeshPolicyFailureSurface {
            code,
            action: self.import.workspace_scope_decision,
            reason: self.import.reason,
            policy_ref: mesh_policy_ref(self.policy_id.as_deref()),
            material_lane: self.import.material_lane,
            redaction: self.redaction,
            trust_lane: self.trust_lane,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshOutboundPolicyDecisionInput<'a> {
    pub local_workspace_id: &'a str,
    pub target_peer_id: &'a str,
    pub origin_workspace_id: &'a str,
    pub material_lane: MeshLane,
    pub payload_is_redacted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshOutboundPolicyDecision {
    pub action: MeshImportDecisionKind,
    pub policy_id: Option<String>,
    pub trust_lane: Option<MeshTrustLane>,
    pub material_lane: MeshLane,
    pub redaction: MeshRedactionDecision,
    pub reason: &'static str,
}

impl MeshOutboundPolicyDecision {
    #[must_use]
    pub fn permits_payload_export(&self) -> bool {
        self.action == MeshImportDecisionKind::Allow
    }

    #[must_use]
    pub fn permits_raw_payload_export(&self) -> bool {
        self.permits_payload_export() && self.redaction == MeshRedactionDecision::Share
    }

    #[must_use]
    pub fn requires_redacted_payload(&self) -> bool {
        self.permits_payload_export() && self.redaction == MeshRedactionDecision::Redact
    }

    #[must_use]
    pub fn to_json(&self) -> JsonValue {
        json!({
            "schema": "ee.mesh.policy_decision.v1",
            "direction": "outbound",
            "action": self.action.as_str(),
            "reason": self.reason,
            "policyRef": mesh_policy_ref(self.policy_id.as_deref()),
            "materialLane": mesh_lane_name(self.material_lane),
            "redaction": self.redaction.as_str(),
            "trustLane": self.trust_lane.map(MeshTrustLane::as_str),
            "payloadExportAllowed": self.permits_payload_export(),
            "rawPayloadExportAllowed": self.permits_raw_payload_export(),
            "redactedPayloadRequired": self.requires_redacted_payload(),
            "failure": self.failure_surface().map(|surface| surface.to_json()),
        })
    }

    #[must_use]
    pub fn failure_surface(&self) -> Option<MeshPolicyFailureSurface> {
        let code = match self.action {
            MeshImportDecisionKind::Allow => return None,
            MeshImportDecisionKind::Quarantine => "mesh_outbound_policy_quarantined",
            MeshImportDecisionKind::Deny => "mesh_outbound_policy_denied",
            MeshImportDecisionKind::Reject => "mesh_outbound_policy_rejected",
        };

        Some(MeshPolicyFailureSurface {
            code,
            action: self.action,
            reason: self.reason,
            policy_ref: mesh_policy_ref(self.policy_id.as_deref()),
            material_lane: self.material_lane,
            redaction: self.redaction,
            trust_lane: self.trust_lane,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshPolicyFailureSurface {
    pub code: &'static str,
    pub action: MeshImportDecisionKind,
    pub reason: &'static str,
    pub policy_ref: String,
    pub material_lane: MeshLane,
    pub redaction: MeshRedactionDecision,
    pub trust_lane: Option<MeshTrustLane>,
}

impl MeshPolicyFailureSurface {
    #[must_use]
    pub fn to_json(&self) -> JsonValue {
        json!({
            "schema": "ee.mesh.policy_failure_surface.v1",
            "code": self.code,
            "action": self.action.as_str(),
            "reason": self.reason,
            "policyRef": self.policy_ref,
            "materialLane": mesh_lane_name(self.material_lane),
            "redaction": self.redaction.as_str(),
            "trustLane": self.trust_lane.map(MeshTrustLane::as_str),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshImportDecision {
    pub workspace_scope_decision: MeshImportDecisionKind,
    pub workspace_id: String,
    pub origin_workspace_id: String,
    pub peer_group_id: Option<String>,
    pub producer_peer_id: String,
    pub material_lane: MeshLane,
    pub allowed: bool,
    pub reason: &'static str,
}

impl MeshImportDecision {
    #[must_use]
    pub fn permits_local_truth_side_effects(&self) -> bool {
        matches!(self.workspace_scope_decision, MeshImportDecisionKind::Allow)
    }

    #[must_use]
    pub fn permits_search_or_graph_side_effects(&self) -> bool {
        matches!(self.workspace_scope_decision, MeshImportDecisionKind::Allow)
    }

    #[must_use]
    pub fn permits_redacted_quarantine_state(&self) -> bool {
        matches!(
            self.workspace_scope_decision,
            MeshImportDecisionKind::Quarantine
        )
    }

    #[must_use]
    pub fn retains_payload(&self) -> bool {
        matches!(self.workspace_scope_decision, MeshImportDecisionKind::Allow)
    }

    #[must_use]
    pub fn to_log_fields(&self) -> JsonValue {
        json!({
            "workspace_scope_decision": self.workspace_scope_decision.as_str(),
            "workspace_id": self.workspace_id,
            "origin_workspace_id": self.origin_workspace_id,
            "peer_group_id": self.peer_group_id,
            "producer_peer_id": self.producer_peer_id,
            "material_lane": mesh_lane_name(self.material_lane),
            "allowed": self.allowed,
            "reason": self.reason,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshDisplayProvenanceInput<'a> {
    pub decision: &'a MeshImportDecision,
    pub cached_material_id: &'a str,
    pub origin_workspace_label: Option<&'a str>,
    pub producer_peer_label: Option<&'a str>,
    pub import_decision_id: Option<&'a str>,
    pub ledger_cursor: Option<&'a str>,
    pub trust_lane: &'a str,
    pub redaction_posture: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshDisplayProvenance {
    pub cached_material_id: String,
    pub origin_workspace_alias: String,
    pub producer_peer: String,
    pub material_lane: MeshLane,
    pub import_decision_ref: String,
    pub trust_lane: String,
    pub redaction_posture: String,
}

impl MeshDisplayProvenance {
    #[must_use]
    pub fn to_json(&self) -> JsonValue {
        json!({
            "cachedMaterialId": self.cached_material_id,
            "originWorkspaceAlias": self.origin_workspace_alias,
            "producerPeer": self.producer_peer,
            "materialLane": mesh_lane_name(self.material_lane),
            "importDecisionRef": self.import_decision_ref,
            "trustLane": self.trust_lane,
            "redactionPosture": self.redaction_posture,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MeshQueryVisibility {
    Local,
    Allowed(MeshDisplayProvenance),
    Blocked,
}

#[must_use]
pub fn mesh_display_provenance(
    input: &MeshDisplayProvenanceInput<'_>,
) -> Option<MeshDisplayProvenance> {
    if input.decision.workspace_scope_decision != MeshImportDecisionKind::Allow {
        return None;
    }

    Some(MeshDisplayProvenance {
        cached_material_id: input.cached_material_id.to_owned(),
        origin_workspace_alias: mesh_namespace_alias(
            &input.decision.origin_workspace_id,
            input.origin_workspace_label,
        ),
        producer_peer: mesh_peer_display(
            &input.decision.producer_peer_id,
            input.producer_peer_label,
        ),
        material_lane: input.decision.material_lane,
        import_decision_ref: mesh_import_decision_ref(
            input.import_decision_id,
            input.ledger_cursor,
        ),
        trust_lane: input.trust_lane.to_owned(),
        redaction_posture: input.redaction_posture.to_owned(),
    })
}

#[must_use]
pub fn mesh_query_visibility(metadata: Option<&JsonValue>) -> MeshQueryVisibility {
    let Some(metadata) = metadata else {
        return MeshQueryVisibility::Local;
    };
    if !metadata_contains_mesh_marker(metadata) {
        return MeshQueryVisibility::Local;
    }

    let Some(decision_kind) = metadata_mesh_decision_kind(metadata) else {
        return MeshQueryVisibility::Blocked;
    };
    if decision_kind != MeshImportDecisionKind::Allow {
        return MeshQueryVisibility::Blocked;
    }
    if metadata_has_mesh_policy_decision(metadata) {
        let Some(policy_decision_kind) = metadata_mesh_policy_decision_kind(metadata) else {
            return MeshQueryVisibility::Blocked;
        };
        if policy_decision_kind != MeshImportDecisionKind::Allow {
            return MeshQueryVisibility::Blocked;
        }
    }

    let Some(cached_material_id) =
        metadata_mesh_string(metadata, &["cachedMaterialId", "cached_material_id"])
    else {
        return MeshQueryVisibility::Blocked;
    };
    let Some(origin_workspace_id) =
        metadata_mesh_string(metadata, &["originWorkspaceId", "origin_workspace_id"])
    else {
        return MeshQueryVisibility::Blocked;
    };
    let Some(producer_peer_id) =
        metadata_mesh_string(metadata, &["producerPeerId", "producer_peer_id"])
    else {
        return MeshQueryVisibility::Blocked;
    };
    let Some(material_lane) = metadata_mesh_string(metadata, &["materialLane", "material_lane"])
        .and_then(parse_mesh_lane)
    else {
        return MeshQueryVisibility::Blocked;
    };
    let Some(trust_lane) = metadata_mesh_string(metadata, &["trustLane", "trust_lane"]) else {
        return MeshQueryVisibility::Blocked;
    };
    let Some(redaction_posture) =
        metadata_mesh_string(metadata, &["redactionPosture", "redaction_posture"])
    else {
        return MeshQueryVisibility::Blocked;
    };

    let decision = MeshImportDecision {
        workspace_scope_decision: decision_kind,
        workspace_id: metadata_mesh_string(metadata, &["workspaceId", "workspace_id"])
            .unwrap_or("unknown")
            .to_owned(),
        origin_workspace_id: origin_workspace_id.to_owned(),
        peer_group_id: metadata_mesh_string(metadata, &["peerGroupId", "peer_group_id"])
            .map(str::to_owned),
        producer_peer_id: producer_peer_id.to_owned(),
        material_lane,
        allowed: true,
        reason: "indexed_mesh_metadata",
    };
    let input = MeshDisplayProvenanceInput {
        decision: &decision,
        cached_material_id,
        origin_workspace_label: metadata_mesh_string(
            metadata,
            &[
                "originWorkspaceAlias",
                "originWorkspaceLabel",
                "origin_workspace_label",
            ],
        ),
        producer_peer_label: metadata_mesh_string(
            metadata,
            &["producerPeer", "producerPeerLabel", "producer_peer_label"],
        ),
        import_decision_id: metadata_mesh_string(
            metadata,
            &[
                "importDecisionRef",
                "importDecisionId",
                "import_decision_id",
            ],
        ),
        ledger_cursor: metadata_mesh_string(metadata, &["ledgerCursor", "ledger_cursor"]),
        trust_lane,
        redaction_posture,
    };

    mesh_display_provenance(&input)
        .map_or(MeshQueryVisibility::Blocked, MeshQueryVisibility::Allowed)
}

#[must_use]
pub fn decide_mesh_import(
    input: &MeshImportDecisionInput<'_>,
    bindings: &[MeshPeerGroupBinding],
) -> MeshImportDecision {
    if input.event_validity == MeshEventValidity::Malformed {
        return mesh_decision(
            input,
            None,
            MeshImportDecisionKind::Reject,
            "malformed_event",
        );
    }
    if input.event_validity == MeshEventValidity::Unsafe {
        return mesh_decision(input, None, MeshImportDecisionKind::Reject, "unsafe_event");
    }

    let matching_workspace: Vec<&MeshPeerGroupBinding> = bindings
        .iter()
        .filter(|binding| binding.workspace_id.as_deref() == Some(input.local_workspace_id))
        .collect();
    if matching_workspace.is_empty() {
        return mesh_decision(
            input,
            None,
            MeshImportDecisionKind::Deny,
            "missing_workspace_binding",
        );
    }

    let matching_peer: Vec<&MeshPeerGroupBinding> = matching_workspace
        .into_iter()
        .filter(|binding| binding_has_peer(binding, input.producer_peer_id))
        .collect();
    if matching_peer.is_empty() {
        return mesh_decision(input, None, MeshImportDecisionKind::Deny, "unknown_peer");
    }

    let Some(binding) = matching_peer
        .into_iter()
        .find(|binding| binding_has_origin_workspace(binding, input.origin_workspace_id))
    else {
        return mesh_decision(
            input,
            None,
            MeshImportDecisionKind::Deny,
            "unknown_origin_workspace",
        );
    };

    let peer_group_id = binding.peer_group_id.clone();
    if peer_group_id.is_none() {
        return mesh_decision(
            input,
            None,
            MeshImportDecisionKind::Deny,
            "malformed_binding",
        );
    }

    match lane_grant(binding, input.material_lane) {
        Some(MeshLaneDecision::Allow) if input.event_validity == MeshEventValidity::Valid => {
            mesh_decision(
                input,
                peer_group_id,
                MeshImportDecisionKind::Allow,
                "lane_allowed",
            )
        }
        Some(MeshLaneDecision::Allow) => mesh_decision(
            input,
            peer_group_id,
            MeshImportDecisionKind::Quarantine,
            "event_policy_quarantine",
        ),
        Some(MeshLaneDecision::Quarantine) => mesh_decision(
            input,
            peer_group_id,
            MeshImportDecisionKind::Quarantine,
            "lane_quarantined",
        ),
        Some(MeshLaneDecision::Deny) => mesh_decision(
            input,
            peer_group_id,
            MeshImportDecisionKind::Deny,
            "lane_denied",
        ),
        None => mesh_decision(
            input,
            peer_group_id,
            MeshImportDecisionKind::Deny,
            "missing_lane_grant",
        ),
    }
}

#[must_use]
pub fn decide_mesh_peer_policy(
    input: &MeshPeerPolicyDecisionInput<'_>,
    policy: Option<&MeshPeerPolicy>,
) -> MeshPeerPolicyDecision {
    let import_input = MeshImportDecisionInput {
        local_workspace_id: input.local_workspace_id,
        origin_workspace_id: input.origin_workspace_id,
        producer_peer_id: input.producer_peer_id,
        material_lane: input.material_lane,
        event_validity: input.event_validity,
    };

    let Some(policy) = policy else {
        return mesh_peer_policy_decision(
            &import_input,
            None,
            MeshImportDecisionKind::Deny,
            "missing_peer_policy",
            None,
            None,
            MeshRedactionDecision::Deny,
            false,
        );
    };

    let policy_id = Some(policy.policy_id.clone());
    if policy.default_action != MeshLaneDecision::Deny {
        return mesh_peer_policy_decision(
            &import_input,
            policy_id,
            MeshImportDecisionKind::Reject,
            "non_deny_default_action",
            Some(policy.trust_lane),
            Some(policy.import_trust_class),
            MeshRedactionDecision::Deny,
            false,
        );
    }
    if input.event_validity == MeshEventValidity::Malformed {
        return mesh_peer_policy_decision(
            &import_input,
            policy_id,
            MeshImportDecisionKind::Reject,
            "malformed_event",
            Some(policy.trust_lane),
            Some(policy.import_trust_class),
            MeshRedactionDecision::Deny,
            false,
        );
    }
    if input.event_validity == MeshEventValidity::Unsafe {
        return mesh_peer_policy_decision(
            &import_input,
            policy_id,
            MeshImportDecisionKind::Reject,
            "unsafe_event",
            Some(policy.trust_lane),
            Some(policy.import_trust_class),
            MeshRedactionDecision::Deny,
            false,
        );
    }
    if policy.workspace_id != input.local_workspace_id {
        return mesh_peer_policy_decision(
            &import_input,
            policy_id,
            MeshImportDecisionKind::Deny,
            "peer_policy_workspace_mismatch",
            Some(policy.trust_lane),
            Some(policy.import_trust_class),
            MeshRedactionDecision::Deny,
            false,
        );
    }
    if policy.peer_id != input.producer_peer_id {
        return mesh_peer_policy_decision(
            &import_input,
            policy_id,
            MeshImportDecisionKind::Deny,
            "peer_policy_peer_mismatch",
            Some(policy.trust_lane),
            Some(policy.import_trust_class),
            MeshRedactionDecision::Deny,
            false,
        );
    }
    if !policy
        .origin_workspace_ids
        .iter()
        .any(|origin| origin == input.origin_workspace_id)
    {
        return mesh_peer_policy_decision(
            &import_input,
            policy_id,
            MeshImportDecisionKind::Deny,
            "peer_policy_origin_workspace_mismatch",
            Some(policy.trust_lane),
            Some(policy.import_trust_class),
            MeshRedactionDecision::Deny,
            false,
        );
    }
    if !policy.trust_lane.permits_peer_import() {
        return mesh_peer_policy_decision(
            &import_input,
            policy_id,
            MeshImportDecisionKind::Reject,
            "peer_import_local_human_trust_lane",
            Some(policy.trust_lane),
            Some(policy.import_trust_class),
            MeshRedactionDecision::Deny,
            false,
        );
    }
    if policy.import_trust_class == TrustClass::HumanExplicit {
        return mesh_peer_policy_decision(
            &import_input,
            policy_id,
            MeshImportDecisionKind::Reject,
            "peer_import_human_explicit_disallowed",
            Some(policy.trust_lane),
            Some(policy.import_trust_class),
            MeshRedactionDecision::Deny,
            false,
        );
    }

    let redaction = policy.redaction.decision_for_lane(input.material_lane);
    if redaction == MeshRedactionDecision::Deny {
        return mesh_peer_policy_decision(
            &import_input,
            policy_id,
            MeshImportDecisionKind::Deny,
            "peer_policy_redaction_denied",
            Some(policy.trust_lane),
            Some(policy.import_trust_class),
            redaction,
            false,
        );
    }

    if input.requested_body_bytes.is_some() && input.material_lane != MeshLane::Body {
        return mesh_peer_policy_decision(
            &import_input,
            policy_id,
            MeshImportDecisionKind::Deny,
            "body_fetch_requires_body_lane",
            Some(policy.trust_lane),
            Some(policy.import_trust_class),
            MeshRedactionDecision::Deny,
            false,
        );
    }

    if input.material_lane == MeshLane::Body {
        if !policy.body_fetch.allowed {
            return mesh_peer_policy_decision(
                &import_input,
                policy_id,
                MeshImportDecisionKind::Deny,
                "body_fetch_denied",
                Some(policy.trust_lane),
                Some(policy.import_trust_class),
                redaction,
                false,
            );
        }
        if policy.body_fetch.requires_consent && !input.body_fetch_consent {
            return mesh_peer_policy_decision(
                &import_input,
                policy_id,
                MeshImportDecisionKind::Deny,
                "body_fetch_requires_consent",
                Some(policy.trust_lane),
                Some(policy.import_trust_class),
                redaction,
                false,
            );
        }
        if policy
            .body_fetch
            .max_bytes
            .zip(input.requested_body_bytes)
            .is_some_and(|(max_bytes, requested_bytes)| requested_bytes > max_bytes)
        {
            return mesh_peer_policy_decision(
                &import_input,
                policy_id,
                MeshImportDecisionKind::Deny,
                "body_fetch_too_large",
                Some(policy.trust_lane),
                Some(policy.import_trust_class),
                redaction,
                false,
            );
        }
    }

    match policy.allowed_lanes.decision(input.material_lane) {
        MeshLaneDecision::Allow if input.event_validity == MeshEventValidity::Valid => {
            mesh_peer_policy_decision(
                &import_input,
                policy_id,
                MeshImportDecisionKind::Allow,
                "peer_policy_lane_allowed",
                Some(policy.trust_lane),
                Some(policy.import_trust_class),
                redaction,
                input.material_lane == MeshLane::Body,
            )
        }
        MeshLaneDecision::Allow => mesh_peer_policy_decision(
            &import_input,
            policy_id,
            MeshImportDecisionKind::Quarantine,
            "event_policy_quarantine",
            Some(policy.trust_lane),
            Some(policy.import_trust_class),
            redaction,
            false,
        ),
        MeshLaneDecision::Quarantine => mesh_peer_policy_decision(
            &import_input,
            policy_id,
            MeshImportDecisionKind::Quarantine,
            "peer_policy_lane_quarantined",
            Some(policy.trust_lane),
            Some(policy.import_trust_class),
            redaction,
            false,
        ),
        MeshLaneDecision::Deny => mesh_peer_policy_decision(
            &import_input,
            policy_id,
            MeshImportDecisionKind::Deny,
            "peer_policy_lane_denied",
            Some(policy.trust_lane),
            Some(policy.import_trust_class),
            redaction,
            false,
        ),
    }
}

#[must_use]
pub fn decide_mesh_outbound_policy(
    input: &MeshOutboundPolicyDecisionInput<'_>,
    policy: Option<&MeshPeerPolicy>,
) -> MeshOutboundPolicyDecision {
    let Some(policy) = policy else {
        return mesh_outbound_policy_decision(
            input,
            None,
            MeshImportDecisionKind::Deny,
            "missing_peer_policy",
            None,
            MeshRedactionDecision::Deny,
        );
    };

    let policy_id = Some(policy.policy_id.clone());
    if policy.default_action != MeshLaneDecision::Deny {
        return mesh_outbound_policy_decision(
            input,
            policy_id,
            MeshImportDecisionKind::Reject,
            "non_deny_default_action",
            Some(policy.trust_lane),
            MeshRedactionDecision::Deny,
        );
    }
    if policy.workspace_id != input.local_workspace_id {
        return mesh_outbound_policy_decision(
            input,
            policy_id,
            MeshImportDecisionKind::Deny,
            "peer_policy_workspace_mismatch",
            Some(policy.trust_lane),
            MeshRedactionDecision::Deny,
        );
    }
    if policy.peer_id != input.target_peer_id {
        return mesh_outbound_policy_decision(
            input,
            policy_id,
            MeshImportDecisionKind::Deny,
            "peer_policy_peer_mismatch",
            Some(policy.trust_lane),
            MeshRedactionDecision::Deny,
        );
    }
    if !policy
        .origin_workspace_ids
        .iter()
        .any(|origin| origin == input.origin_workspace_id)
    {
        return mesh_outbound_policy_decision(
            input,
            policy_id,
            MeshImportDecisionKind::Deny,
            "peer_policy_origin_workspace_mismatch",
            Some(policy.trust_lane),
            MeshRedactionDecision::Deny,
        );
    }

    let redaction = policy.redaction.decision_for_lane(input.material_lane);
    if redaction == MeshRedactionDecision::Deny {
        return mesh_outbound_policy_decision(
            input,
            policy_id,
            MeshImportDecisionKind::Deny,
            "outbound_redaction_denied",
            Some(policy.trust_lane),
            redaction,
        );
    }
    if redaction == MeshRedactionDecision::Redact && !input.payload_is_redacted {
        return mesh_outbound_policy_decision(
            input,
            policy_id,
            MeshImportDecisionKind::Deny,
            "outbound_payload_requires_redaction",
            Some(policy.trust_lane),
            redaction,
        );
    }

    match policy.allowed_lanes.decision(input.material_lane) {
        MeshLaneDecision::Allow => mesh_outbound_policy_decision(
            input,
            policy_id,
            MeshImportDecisionKind::Allow,
            "outbound_lane_allowed",
            Some(policy.trust_lane),
            redaction,
        ),
        MeshLaneDecision::Quarantine => mesh_outbound_policy_decision(
            input,
            policy_id,
            MeshImportDecisionKind::Quarantine,
            "outbound_lane_quarantined",
            Some(policy.trust_lane),
            redaction,
        ),
        MeshLaneDecision::Deny => mesh_outbound_policy_decision(
            input,
            policy_id,
            MeshImportDecisionKind::Deny,
            "outbound_lane_denied",
            Some(policy.trust_lane),
            redaction,
        ),
    }
}

impl MemoryScopeContext {
    #[must_use]
    pub fn for_workspace(workspace_path: &Path, scope: MemoryScope, strict_scope: bool) -> Self {
        Self {
            scope,
            strict_scope,
            current_agent: current_agent_name(),
            team_members: load_team_members(workspace_path),
        }
    }

    #[must_use]
    pub fn stats(&self) -> MemoryScopeStats {
        MemoryScopeStats::new(
            self.scope,
            self.strict_scope,
            self.current_agent.clone(),
            self.team_members.len(),
        )
    }

    #[must_use]
    pub fn memory_in_scope(&self, memory: &StoredMemory) -> bool {
        match self.scope {
            MemoryScope::Swarm | MemoryScope::Workspace => true,
            MemoryScope::Verified => is_verified_memory(memory),
            MemoryScope::SelfOnly => self
                .current_agent
                .as_deref()
                .is_some_and(|agent| memory_producer_agent(memory).as_deref() == Some(agent)),
            MemoryScope::Team => memory_producer_agent(memory).is_some_and(|producer| {
                self.current_agent.as_deref() == Some(producer.as_str())
                    || self.team_members.contains(&producer)
            }),
        }
    }
}

#[must_use]
pub fn current_agent_name() -> Option<String> {
    read_env_var(EnvVar::AgentName).and_then(normalized_non_empty)
}

#[must_use]
pub fn remember_trust_subclass(base: &str) -> Option<String> {
    let base = base.trim();
    let Some(agent) = current_agent_name() else {
        return normalized_non_empty(base.to_owned());
    };
    if base.is_empty() {
        Some(format!("agent:{agent}"))
    } else {
        Some(format!("{base}; agent:{agent}"))
    }
}

#[must_use]
pub fn memory_producer_agent(memory: &StoredMemory) -> Option<String> {
    memory
        .trust_subclass
        .as_deref()
        .and_then(agent_from_trust_subclass)
        .or_else(|| {
            memory
                .provenance_uri
                .as_deref()
                .and_then(agent_from_provenance_uri)
        })
}

#[must_use]
pub fn is_verified_memory(memory: &StoredMemory) -> bool {
    matches!(
        TrustClass::from_str(&memory.trust_class),
        Ok(TrustClass::HumanExplicit | TrustClass::AgentValidated)
    )
}

fn load_team_members(workspace_path: &Path) -> BTreeSet<String> {
    let config_path = workspace_path.join(".ee").join("config.toml");
    let Some(contents) = read_memory_scope_config(&config_path) else {
        return BTreeSet::new();
    };
    let Ok(config) = ConfigFile::parse(&contents) else {
        return BTreeSet::new();
    };
    config
        .trust
        .team_members
        .unwrap_or_default()
        .into_iter()
        .filter_map(normalized_non_empty)
        .collect()
}

fn read_memory_scope_config(config_path: &Path) -> Option<String> {
    if memory_scope_path_has_symlink_component(config_path).ok()? {
        return None;
    }
    if !std::fs::symlink_metadata(config_path)
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false)
    {
        return None;
    }
    std::fs::read_to_string(config_path).ok()
}

fn memory_scope_path_has_symlink_component(path: &Path) -> io::Result<bool> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                current.push(component.as_os_str());
                continue;
            }
            Component::CurDir => continue,
            Component::ParentDir | Component::Normal(_) => {
                current.push(component.as_os_str());
            }
        }

        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

fn agent_from_trust_subclass(value: &str) -> Option<String> {
    value
        .split([';', ',', '|'])
        .map(str::trim)
        .find_map(|part| {
            part.strip_prefix("agent:")
                .or_else(|| part.strip_prefix("agent="))
                .map(str::to_owned)
        })
        .and_then(normalized_non_empty)
}

fn agent_from_provenance_uri(value: &str) -> Option<String> {
    let rest = value
        .strip_prefix("agent://")
        .or_else(|| value.strip_prefix("agent-mail://"))
        .or_else(|| value.strip_prefix("agent_mail://"))?;
    let name = rest
        .split(['/', '#', '?'])
        .next()
        .unwrap_or_default()
        .to_owned();
    normalized_non_empty(name)
}

fn normalized_non_empty(value: impl Into<String>) -> Option<String> {
    let value = value.into().trim().to_owned();
    if value.is_empty() { None } else { Some(value) }
}

fn mesh_decision(
    input: &MeshImportDecisionInput<'_>,
    peer_group_id: Option<String>,
    workspace_scope_decision: MeshImportDecisionKind,
    reason: &'static str,
) -> MeshImportDecision {
    MeshImportDecision {
        workspace_scope_decision,
        workspace_id: input.local_workspace_id.to_owned(),
        origin_workspace_id: input.origin_workspace_id.to_owned(),
        peer_group_id,
        producer_peer_id: input.producer_peer_id.to_owned(),
        material_lane: input.material_lane,
        allowed: workspace_scope_decision == MeshImportDecisionKind::Allow,
        reason,
    }
}

fn mesh_peer_policy_decision(
    input: &MeshImportDecisionInput<'_>,
    policy_id: Option<String>,
    workspace_scope_decision: MeshImportDecisionKind,
    reason: &'static str,
    trust_lane: Option<MeshTrustLane>,
    import_trust_class: Option<TrustClass>,
    redaction: MeshRedactionDecision,
    body_fetch_allowed: bool,
) -> MeshPeerPolicyDecision {
    MeshPeerPolicyDecision {
        import: mesh_decision(input, None, workspace_scope_decision, reason),
        policy_id,
        trust_lane,
        import_trust_class,
        redaction,
        body_fetch_allowed,
    }
}

fn mesh_outbound_policy_decision(
    input: &MeshOutboundPolicyDecisionInput<'_>,
    policy_id: Option<String>,
    action: MeshImportDecisionKind,
    reason: &'static str,
    trust_lane: Option<MeshTrustLane>,
    redaction: MeshRedactionDecision,
) -> MeshOutboundPolicyDecision {
    MeshOutboundPolicyDecision {
        action,
        policy_id,
        trust_lane,
        material_lane: input.material_lane,
        redaction,
        reason,
    }
}

fn mesh_trust_lane_from_config(lane: crate::config::MeshTrustLane) -> MeshTrustLane {
    match lane {
        crate::config::MeshTrustLane::LocalHuman => MeshTrustLane::LocalHuman,
        crate::config::MeshTrustLane::PeerHumanViaPeer => MeshTrustLane::PeerHumanViaPeer,
        crate::config::MeshTrustLane::PeerAgent => MeshTrustLane::PeerAgent,
        crate::config::MeshTrustLane::PeerDerived => MeshTrustLane::PeerDerived,
        crate::config::MeshTrustLane::Untrusted => MeshTrustLane::Untrusted,
    }
}

fn mesh_redaction_decision_from_config(
    decision: crate::config::MeshRedactionDecision,
) -> MeshRedactionDecision {
    match decision {
        crate::config::MeshRedactionDecision::Share => MeshRedactionDecision::Share,
        crate::config::MeshRedactionDecision::Redact => MeshRedactionDecision::Redact,
        crate::config::MeshRedactionDecision::Deny => MeshRedactionDecision::Deny,
    }
}

fn binding_has_peer(binding: &MeshPeerGroupBinding, producer_peer_id: &str) -> bool {
    binding
        .peer_ids
        .as_ref()
        .is_some_and(|peers| peers.iter().any(|peer| peer == producer_peer_id))
}

fn binding_has_origin_workspace(binding: &MeshPeerGroupBinding, origin_workspace_id: &str) -> bool {
    binding
        .origin_workspace_ids
        .as_ref()
        .is_some_and(|origins| origins.iter().any(|origin| origin == origin_workspace_id))
}

fn lane_grant(binding: &MeshPeerGroupBinding, lane: MeshLane) -> Option<MeshLaneDecision> {
    match lane {
        MeshLane::Metadata => binding.lanes.metadata,
        MeshLane::Body => binding.lanes.body,
        MeshLane::Embedding => binding.lanes.embedding,
        MeshLane::GraphLink => binding.lanes.graph_link,
        MeshLane::RevisionNotice => binding.lanes.revision_notice,
        MeshLane::CurationSignal => binding.lanes.curation_signal,
    }
}

fn mesh_lane_name(lane: MeshLane) -> &'static str {
    match lane {
        MeshLane::Metadata => "metadata",
        MeshLane::Body => "body",
        MeshLane::Embedding => "embedding",
        MeshLane::GraphLink => "graphLink",
        MeshLane::RevisionNotice => "revisionNotice",
        MeshLane::CurationSignal => "curationSignal",
    }
}

fn parse_mesh_lane(lane: &str) -> Option<MeshLane> {
    match lane {
        "metadata" => Some(MeshLane::Metadata),
        "body" => Some(MeshLane::Body),
        "embedding" => Some(MeshLane::Embedding),
        "graphLink" | "graph_link" => Some(MeshLane::GraphLink),
        "revisionNotice" | "revision_notice" => Some(MeshLane::RevisionNotice),
        "curationSignal" | "curation_signal" => Some(MeshLane::CurationSignal),
        _ => None,
    }
}

fn mesh_namespace_alias(origin_workspace_id: &str, label: Option<&str>) -> String {
    label
        .and_then(redaction_safe_label)
        .unwrap_or_else(|| stable_mesh_alias("mesh_ns", origin_workspace_id))
}

fn mesh_peer_display(producer_peer_id: &str, label: Option<&str>) -> String {
    label
        .and_then(redaction_safe_label)
        .unwrap_or_else(|| stable_mesh_alias("mesh_peer", producer_peer_id))
}

fn mesh_import_decision_ref(
    import_decision_id: Option<&str>,
    ledger_cursor: Option<&str>,
) -> String {
    let Some(reference) = import_decision_id.or(ledger_cursor) else {
        return "unrecorded".to_owned();
    };
    redaction_safe_label(reference).unwrap_or_else(|| stable_mesh_alias("mesh_dec", reference))
}

fn redaction_safe_label(label: &str) -> Option<String> {
    let label = label.trim();
    if label.is_empty()
        || label.contains('/')
        || label.contains('\\')
        || label.contains(':')
        || label.contains('~')
    {
        None
    } else {
        Some(label.to_owned())
    }
}

fn stable_mesh_alias(prefix: &str, value: &str) -> String {
    let hash = blake3::hash(value.as_bytes()).to_hex();
    format!("{prefix}_{}", &hash[..10])
}

fn mesh_policy_ref(policy_id: Option<&str>) -> String {
    let Some(policy_id) = policy_id else {
        return "missing".to_owned();
    };
    redaction_safe_label(policy_id).unwrap_or_else(|| stable_mesh_alias("mesh_pol", policy_id))
}

fn metadata_contains_mesh_marker(metadata: &JsonValue) -> bool {
    [
        "mesh",
        "workspaceScopeDecision",
        "workspace_scope_decision",
        "cachedMaterialId",
        "cached_material_id",
        "originWorkspaceId",
        "origin_workspace_id",
        "materialLane",
        "material_lane",
        "policyDecision",
        "policy_decision",
        "policyDecisionJson",
        "policy_decision_json",
        "policyFailureSurface",
        "policy_failure_surface",
        "policyFailureSurfaceJson",
        "policy_failure_surface_json",
    ]
    .iter()
    .any(|key| metadata.get(key).is_some())
}

fn metadata_mesh_decision_kind(metadata: &JsonValue) -> Option<MeshImportDecisionKind> {
    match metadata_mesh_string(
        metadata,
        &["workspaceScopeDecision", "workspace_scope_decision"],
    )? {
        "allow" => Some(MeshImportDecisionKind::Allow),
        "quarantine" => Some(MeshImportDecisionKind::Quarantine),
        "deny" => Some(MeshImportDecisionKind::Deny),
        "reject" => Some(MeshImportDecisionKind::Reject),
        _ => None,
    }
}

fn metadata_has_mesh_policy_decision(metadata: &JsonValue) -> bool {
    metadata_mesh_policy_decision(metadata).is_some()
        || metadata_mesh_string(metadata, &["policyDecisionJson", "policy_decision_json"]).is_some()
}

fn metadata_mesh_policy_decision_kind(metadata: &JsonValue) -> Option<MeshImportDecisionKind> {
    let policy_decision = metadata_mesh_policy_decision(metadata)?;
    if json_string(policy_decision, "schema")? != "ee.mesh.policy_decision.v1" {
        return None;
    }
    if json_string(policy_decision, "direction")? != "inbound" {
        return None;
    }
    match json_string(policy_decision, "action")? {
        "allow" => Some(MeshImportDecisionKind::Allow),
        "quarantine" => Some(MeshImportDecisionKind::Quarantine),
        "deny" => Some(MeshImportDecisionKind::Deny),
        "reject" => Some(MeshImportDecisionKind::Reject),
        _ => None,
    }
}

fn metadata_mesh_policy_decision(metadata: &JsonValue) -> Option<&JsonValue> {
    metadata
        .get("policyDecision")
        .or_else(|| metadata.get("policy_decision"))
        .or_else(|| {
            metadata.get("mesh").and_then(|mesh| {
                mesh.get("policyDecision")
                    .or_else(|| mesh.get("policy_decision"))
            })
        })
}

fn metadata_mesh_string<'a>(metadata: &'a JsonValue, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| json_string(metadata, key))
        .or_else(|| {
            metadata
                .get("mesh")
                .and_then(|mesh| keys.iter().find_map(|key| json_string(mesh, key)))
        })
}

fn json_string<'a>(metadata: &'a JsonValue, key: &str) -> Option<&'a str> {
    metadata
        .get(key)
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MeshLaneGrants;

    fn memory_with_scope(trust_class: TrustClass, trust_subclass: Option<&str>) -> StoredMemory {
        StoredMemory {
            id: "mem-1".to_owned(),
            workspace_id: "workspace".to_owned(),
            level: "procedural".to_owned(),
            kind: "rule".to_owned(),
            content: "Run cargo fmt --check before release.".to_owned(),
            workflow_id: None,
            confidence: 0.9,
            utility: 0.5,
            importance: 0.5,
            provenance_uri: None,
            trust_class: trust_class.as_str().to_owned(),
            trust_subclass: trust_subclass.map(str::to_owned),
            provenance_chain_hash: None,
            provenance_chain_hash_version: "none".to_owned(),
            provenance_verification_status: "unverified".to_owned(),
            provenance_verified_at: None,
            provenance_verification_note: None,
            created_at: "2026-05-13T00:00:00Z".to_owned(),
            updated_at: "2026-05-13T00:00:00Z".to_owned(),
            tombstoned_at: None,
            valid_from: None,
            valid_to: None,
        }
    }

    #[test]
    fn verified_scope_accepts_human_and_agent_validated_only() {
        let context = MemoryScopeContext {
            scope: MemoryScope::Verified,
            strict_scope: false,
            current_agent: None,
            team_members: BTreeSet::new(),
        };

        assert!(context.memory_in_scope(&memory_with_scope(TrustClass::HumanExplicit, None)));
        assert!(context.memory_in_scope(&memory_with_scope(TrustClass::AgentValidated, None)));
        assert!(!context.memory_in_scope(&memory_with_scope(TrustClass::AgentAssertion, None)));
    }

    #[test]
    fn self_scope_requires_matching_producer_agent() {
        let context = MemoryScopeContext {
            scope: MemoryScope::SelfOnly,
            strict_scope: false,
            current_agent: Some("BlueLake".to_owned()),
            team_members: BTreeSet::new(),
        };

        assert!(context.memory_in_scope(&memory_with_scope(
            TrustClass::HumanExplicit,
            Some("ee remember; agent:BlueLake")
        )));
        assert!(!context.memory_in_scope(&memory_with_scope(
            TrustClass::HumanExplicit,
            Some("ee remember; agent:GreenField")
        )));
    }

    #[test]
    fn team_scope_accepts_configured_team_members() {
        let context = MemoryScopeContext {
            scope: MemoryScope::Team,
            strict_scope: false,
            current_agent: Some("BlueLake".to_owned()),
            team_members: BTreeSet::from(["GreenField".to_owned()]),
        };

        assert!(context.memory_in_scope(&memory_with_scope(
            TrustClass::HumanExplicit,
            Some("agent=GreenField")
        )));
        assert!(!context.memory_in_scope(&memory_with_scope(
            TrustClass::HumanExplicit,
            Some("agent=RedStone")
        )));
    }

    #[test]
    fn team_scope_loads_configured_team_members_from_workspace_config() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_dir = tempdir.path().join(".ee");
        std::fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
        std::fs::write(
            config_dir.join("config.toml"),
            r#"
[trust]
team_members = ["GreenField", "  ", "BlueLake"]
"#,
        )
        .map_err(|error| error.to_string())?;

        let context = MemoryScopeContext::for_workspace(tempdir.path(), MemoryScope::Team, false);

        assert_eq!(
            context.team_members,
            BTreeSet::from(["BlueLake".to_owned(), "GreenField".to_owned()])
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn team_scope_ignores_symlinked_workspace_config_file() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_dir = tempdir.path().join(".ee");
        let outside_dir = tempdir.path().join("outside");
        std::fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&outside_dir).map_err(|error| error.to_string())?;
        std::fs::write(
            outside_dir.join("config.toml"),
            r#"
[trust]
team_members = ["OutsideAgent"]
"#,
        )
        .map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(
            outside_dir.join("config.toml"),
            config_dir.join("config.toml"),
        )
        .map_err(|error| error.to_string())?;

        let context = MemoryScopeContext::for_workspace(tempdir.path(), MemoryScope::Team, false);

        assert!(context.team_members.is_empty());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn team_scope_ignores_symlinked_workspace_config_parent() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let outside_dir = tempdir.path().join("outside-ee");
        std::fs::create_dir_all(&outside_dir).map_err(|error| error.to_string())?;
        std::fs::write(
            outside_dir.join("config.toml"),
            r#"
[trust]
team_members = ["OutsideAgent"]
"#,
        )
        .map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&outside_dir, tempdir.path().join(".ee"))
            .map_err(|error| error.to_string())?;

        let context = MemoryScopeContext::for_workspace(tempdir.path(), MemoryScope::Team, false);

        assert!(context.team_members.is_empty());
        Ok(())
    }

    #[test]
    fn team_scope_ignores_non_regular_workspace_config_file() -> Result<(), String> {
        let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let config_path = tempdir.path().join(".ee").join("config.toml");
        std::fs::create_dir_all(&config_path).map_err(|error| error.to_string())?;

        let context = MemoryScopeContext::for_workspace(tempdir.path(), MemoryScope::Team, false);

        assert!(context.team_members.is_empty());
        assert!(
            config_path.is_dir(),
            "team scope config read must leave non-regular config path untouched"
        );
        Ok(())
    }

    #[test]
    fn workspace_and_swarm_scopes_include_any_memory() {
        let mut context = MemoryScopeContext {
            scope: MemoryScope::Workspace,
            strict_scope: false,
            current_agent: None,
            team_members: BTreeSet::new(),
        };
        let memory = memory_with_scope(TrustClass::AgentAssertion, Some("agent=RedStone"));

        assert!(context.memory_in_scope(&memory));
        context.scope = MemoryScope::Swarm;
        assert!(context.memory_in_scope(&memory));
    }

    fn mesh_binding() -> MeshPeerGroupBinding {
        MeshPeerGroupBinding {
            workspace_id: Some("wsp_local_alpha".to_owned()),
            workspace_alias: Some("local-alpha".to_owned()),
            peer_group_id: Some("pg_alpha_mesh".to_owned()),
            peer_group_label: Some("alpha-mesh".to_owned()),
            peer_ids: Some(vec!["peer_builder_one".to_owned()]),
            origin_workspace_ids: Some(vec!["wsp_remote_beta".to_owned()]),
            lanes: MeshLaneGrants {
                metadata: Some(MeshLaneDecision::Allow),
                body: Some(MeshLaneDecision::Deny),
                embedding: None,
                graph_link: Some(MeshLaneDecision::Allow),
                revision_notice: Some(MeshLaneDecision::Allow),
                curation_signal: Some(MeshLaneDecision::Quarantine),
            },
            default_action: Some(MeshLaneDecision::Deny),
        }
    }

    fn mesh_input(lane: MeshLane) -> MeshImportDecisionInput<'static> {
        MeshImportDecisionInput {
            local_workspace_id: "wsp_local_alpha",
            origin_workspace_id: "wsp_remote_beta",
            producer_peer_id: "peer_builder_one",
            material_lane: lane,
            event_validity: MeshEventValidity::Valid,
        }
    }

    fn peer_policy() -> MeshPeerPolicy {
        MeshPeerPolicy {
            policy_id: "pol_alpha_mesh_policy".to_owned(),
            workspace_id: "wsp_local_alpha".to_owned(),
            peer_id: "peer_builder_one".to_owned(),
            origin_workspace_ids: vec!["wsp_remote_beta".to_owned()],
            trust_lane: MeshTrustLane::PeerAgent,
            import_trust_class: TrustClass::AgentValidated,
            allowed_lanes: MeshLaneGrants {
                metadata: Some(MeshLaneDecision::Allow),
                body: Some(MeshLaneDecision::Deny),
                embedding: Some(MeshLaneDecision::Deny),
                graph_link: Some(MeshLaneDecision::Allow),
                revision_notice: Some(MeshLaneDecision::Allow),
                curation_signal: Some(MeshLaneDecision::Quarantine),
            },
            redaction: MeshRedactionPolicy {
                metadata: MeshRedactionDecision::Share,
                preview: MeshRedactionDecision::Redact,
                body: MeshRedactionDecision::Deny,
                embedding: MeshRedactionDecision::Deny,
            },
            body_fetch: MeshBodyFetchPolicy::denied(),
            default_action: MeshLaneDecision::Deny,
        }
    }

    fn peer_policy_input(lane: MeshLane) -> MeshPeerPolicyDecisionInput<'static> {
        MeshPeerPolicyDecisionInput {
            local_workspace_id: "wsp_local_alpha",
            origin_workspace_id: "wsp_remote_beta",
            producer_peer_id: "peer_builder_one",
            material_lane: lane,
            event_validity: MeshEventValidity::Valid,
            requested_body_bytes: None,
            body_fetch_consent: false,
        }
    }

    fn outbound_policy_input(lane: MeshLane) -> MeshOutboundPolicyDecisionInput<'static> {
        MeshOutboundPolicyDecisionInput {
            local_workspace_id: "wsp_local_alpha",
            origin_workspace_id: "wsp_remote_beta",
            target_peer_id: "peer_builder_one",
            material_lane: lane,
            payload_is_redacted: false,
        }
    }

    #[test]
    fn mesh_import_allows_authorized_metadata_side_effects_only_for_allow() {
        let decision = decide_mesh_import(&mesh_input(MeshLane::Metadata), &[mesh_binding()]);

        assert_eq!(
            decision.workspace_scope_decision,
            MeshImportDecisionKind::Allow
        );
        assert!(decision.allowed);
        assert!(decision.permits_local_truth_side_effects());
        assert!(decision.permits_search_or_graph_side_effects());
        assert!(decision.retains_payload());
        assert!(!decision.permits_redacted_quarantine_state());
        assert_eq!(decision.reason, "lane_allowed");
        assert_eq!(decision.peer_group_id.as_deref(), Some("pg_alpha_mesh"));
    }

    #[test]
    fn mesh_peer_policy_allows_metadata_with_non_human_import_trust() {
        let decision =
            decide_mesh_peer_policy(&peer_policy_input(MeshLane::Metadata), Some(&peer_policy()));

        assert_eq!(
            decision.import.workspace_scope_decision,
            MeshImportDecisionKind::Allow
        );
        assert_eq!(decision.import.reason, "peer_policy_lane_allowed");
        assert_eq!(decision.policy_id.as_deref(), Some("pol_alpha_mesh_policy"));
        assert_eq!(decision.trust_lane, Some(MeshTrustLane::PeerAgent));
        assert_eq!(
            decision.import_trust_class,
            Some(TrustClass::AgentValidated)
        );
        assert_eq!(decision.redaction_posture(), "share");
        assert!(!decision.permits_import_as_human_explicit());
        assert!(!decision.permits_body_fetch());
    }

    #[test]
    fn mesh_peer_policy_rejects_local_human_trust_lane_for_peer_import() {
        let mut policy = peer_policy();
        policy.trust_lane = MeshTrustLane::LocalHuman;

        let decision =
            decide_mesh_peer_policy(&peer_policy_input(MeshLane::Metadata), Some(&policy));

        assert_eq!(
            decision.import.workspace_scope_decision,
            MeshImportDecisionKind::Reject
        );
        assert_eq!(decision.import.reason, "peer_import_local_human_trust_lane");
        assert_eq!(decision.redaction, MeshRedactionDecision::Deny);
        assert!(!decision.import.permits_local_truth_side_effects());
    }

    #[test]
    fn mesh_peer_policy_rejects_human_explicit_import_trust_class() {
        let mut policy = peer_policy();
        policy.import_trust_class = TrustClass::HumanExplicit;

        let decision =
            decide_mesh_peer_policy(&peer_policy_input(MeshLane::Metadata), Some(&policy));

        assert_eq!(
            decision.import.workspace_scope_decision,
            MeshImportDecisionKind::Reject
        );
        assert_eq!(
            decision.import.reason,
            "peer_import_human_explicit_disallowed"
        );
        assert!(!decision.permits_import_as_human_explicit());
    }

    #[test]
    fn mesh_peer_policy_denies_body_when_body_fetch_is_not_granted() {
        let mut policy = peer_policy();
        policy.allowed_lanes.body = Some(MeshLaneDecision::Allow);
        policy.redaction.body = MeshRedactionDecision::Redact;

        let decision = decide_mesh_peer_policy(&peer_policy_input(MeshLane::Body), Some(&policy));

        assert_eq!(
            decision.import.workspace_scope_decision,
            MeshImportDecisionKind::Deny
        );
        assert_eq!(decision.import.reason, "body_fetch_denied");
        assert_eq!(decision.redaction, MeshRedactionDecision::Redact);
        assert!(!decision.permits_body_fetch());
        assert!(!decision.import.retains_payload());
    }

    #[test]
    fn mesh_peer_policy_allows_body_only_with_consent_size_and_redaction_grants() {
        let mut policy = peer_policy();
        policy.allowed_lanes.body = Some(MeshLaneDecision::Allow);
        policy.redaction.body = MeshRedactionDecision::Redact;
        policy.body_fetch = MeshBodyFetchPolicy {
            allowed: true,
            requires_consent: true,
            max_bytes: Some(512),
        };
        let mut input = peer_policy_input(MeshLane::Body);
        input.requested_body_bytes = Some(256);
        input.body_fetch_consent = true;

        let decision = decide_mesh_peer_policy(&input, Some(&policy));

        assert_eq!(
            decision.import.workspace_scope_decision,
            MeshImportDecisionKind::Allow
        );
        assert_eq!(decision.import.reason, "peer_policy_lane_allowed");
        assert_eq!(decision.redaction, MeshRedactionDecision::Redact);
        assert!(decision.permits_body_fetch());
    }

    #[test]
    fn mesh_peer_policy_denies_oversized_or_unconsented_body_fetch() {
        let mut policy = peer_policy();
        policy.allowed_lanes.body = Some(MeshLaneDecision::Allow);
        policy.redaction.body = MeshRedactionDecision::Redact;
        policy.body_fetch = MeshBodyFetchPolicy {
            allowed: true,
            requires_consent: true,
            max_bytes: Some(512),
        };
        let mut without_consent = peer_policy_input(MeshLane::Body);
        without_consent.requested_body_bytes = Some(256);

        let consent_decision = decide_mesh_peer_policy(&without_consent, Some(&policy));
        assert_eq!(
            consent_decision.import.workspace_scope_decision,
            MeshImportDecisionKind::Deny
        );
        assert_eq!(
            consent_decision.import.reason,
            "body_fetch_requires_consent"
        );

        let mut oversized = without_consent;
        oversized.body_fetch_consent = true;
        oversized.requested_body_bytes = Some(1024);
        let size_decision = decide_mesh_peer_policy(&oversized, Some(&policy));
        assert_eq!(
            size_decision.import.workspace_scope_decision,
            MeshImportDecisionKind::Deny
        );
        assert_eq!(size_decision.import.reason, "body_fetch_too_large");
    }

    #[test]
    fn mesh_peer_policy_rejects_body_fetch_smuggled_through_metadata_lane() {
        let mut policy = peer_policy();
        policy.allowed_lanes.body = Some(MeshLaneDecision::Allow);
        policy.redaction.body = MeshRedactionDecision::Share;
        policy.body_fetch = MeshBodyFetchPolicy {
            allowed: true,
            requires_consent: false,
            max_bytes: Some(512),
        };
        let mut input = peer_policy_input(MeshLane::Metadata);
        input.requested_body_bytes = Some(128);

        let decision = decide_mesh_peer_policy(&input, Some(&policy));

        assert_eq!(
            decision.import.workspace_scope_decision,
            MeshImportDecisionKind::Deny
        );
        assert_eq!(decision.import.reason, "body_fetch_requires_body_lane");
        assert_eq!(decision.redaction, MeshRedactionDecision::Deny);
        assert!(!decision.permits_body_fetch());
        assert!(!decision.import.retains_payload());
    }

    #[test]
    fn mesh_peer_policy_quarantines_quarantine_lanes_and_policy_quarantine_events() {
        let curation = decide_mesh_peer_policy(
            &peer_policy_input(MeshLane::CurationSignal),
            Some(&peer_policy()),
        );

        assert_eq!(
            curation.import.workspace_scope_decision,
            MeshImportDecisionKind::Quarantine
        );
        assert_eq!(curation.import.reason, "peer_policy_lane_quarantined");
        assert!(curation.import.permits_redacted_quarantine_state());

        let mut input = peer_policy_input(MeshLane::Metadata);
        input.event_validity = MeshEventValidity::PolicyQuarantine;
        let event = decide_mesh_peer_policy(&input, Some(&peer_policy()));
        assert_eq!(
            event.import.workspace_scope_decision,
            MeshImportDecisionKind::Quarantine
        );
        assert_eq!(event.import.reason, "event_policy_quarantine");
    }

    #[test]
    fn mesh_peer_policy_failure_surface_uses_stable_structured_codes() {
        let missing = decide_mesh_peer_policy(&peer_policy_input(MeshLane::Metadata), None);
        let missing_surface = missing
            .failure_surface()
            .expect("denied peer policy should expose failure fields");
        assert_eq!(missing_surface.code, "mesh_peer_policy_denied");
        assert_eq!(missing_surface.action, MeshImportDecisionKind::Deny);
        assert_eq!(missing_surface.policy_ref, "missing");
        assert_eq!(missing_surface.reason, "missing_peer_policy");

        let quarantined = decide_mesh_peer_policy(
            &peer_policy_input(MeshLane::CurationSignal),
            Some(&peer_policy()),
        );
        let quarantine_surface = quarantined
            .failure_surface()
            .expect("quarantined peer policy should expose failure fields");
        assert_eq!(quarantine_surface.code, "mesh_peer_policy_quarantined");
        assert_eq!(
            quarantine_surface.action,
            MeshImportDecisionKind::Quarantine
        );

        let mut policy = peer_policy();
        policy.trust_lane = MeshTrustLane::LocalHuman;
        let rejected =
            decide_mesh_peer_policy(&peer_policy_input(MeshLane::Metadata), Some(&policy));
        let reject_surface = rejected
            .failure_surface()
            .expect("rejected peer policy should expose failure fields");
        assert_eq!(reject_surface.code, "mesh_peer_policy_rejected");
        assert_eq!(reject_surface.action, MeshImportDecisionKind::Reject);

        let allowed =
            decide_mesh_peer_policy(&peer_policy_input(MeshLane::Metadata), Some(&peer_policy()));
        assert_eq!(allowed.failure_surface(), None);
    }

    #[test]
    fn mesh_peer_policy_failure_surface_redacts_path_like_policy_refs() {
        let mut policy = peer_policy();
        policy.policy_id = "/Users/alice/private/inbound-policy.toml".to_owned();
        let decision = decide_mesh_peer_policy(&peer_policy_input(MeshLane::Body), Some(&policy));
        let surface = decision
            .failure_surface()
            .expect("denied inbound body should expose failure fields");

        assert_eq!(surface.code, "mesh_peer_policy_denied");
        assert!(surface.policy_ref.starts_with("mesh_pol_"));
        assert_ne!(
            surface.policy_ref,
            "/Users/alice/private/inbound-policy.toml"
        );

        let fields = surface.to_json();
        assert_eq!(fields["code"], "mesh_peer_policy_denied");
        assert_eq!(fields["action"], "deny");
        assert_eq!(fields["reason"], "peer_policy_redaction_denied");
        assert_eq!(fields["materialLane"], "body");
        assert_eq!(fields["redaction"], "deny");
        assert_eq!(fields["trustLane"], "peerAgent");
        assert!(
            !fields.to_string().contains("/Users/alice/private"),
            "failure surface leaked raw policy path: {fields}"
        );
    }

    #[test]
    fn mesh_peer_policy_converts_from_config_record_for_runtime_decision() {
        let config = ConfigFile::parse(
            r#"
[[mesh.peer_policies]]
policy_id = "pol_configured_001"
workspace_id = "wsp_local_alpha"
peer_id = "peer_builder_one"
origin_workspace_ids = ["wsp_remote_beta"]
trust_lane = "peerAgent"
import_trust_class = "agent_validated"
default_action = "deny"

[mesh.peer_policies.allowed_lanes]
metadata = "allow"
body = "deny"
embedding = "deny"
graph_link = "allow"
revision_notice = "allow"
curation_signal = "quarantine"

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
        .expect("configured peer policy should parse");
        let configured_policy = config
            .mesh
            .peer_policies
            .as_ref()
            .and_then(|policies| policies.first())
            .expect("peer policy should exist");
        let policy = MeshPeerPolicy::from(configured_policy);

        let decision =
            decide_mesh_peer_policy(&peer_policy_input(MeshLane::Metadata), Some(&policy));

        assert_eq!(
            decision.import.workspace_scope_decision,
            MeshImportDecisionKind::Allow
        );
        assert_eq!(decision.import.reason, "peer_policy_lane_allowed");
        assert_eq!(decision.policy_id.as_deref(), Some("pol_configured_001"));
        assert_eq!(
            decision.import_trust_class,
            Some(TrustClass::AgentValidated)
        );
        assert!(!decision.permits_import_as_human_explicit());
    }

    #[test]
    fn mesh_peer_policy_decision_json_is_stable_and_redaction_safe() {
        let allowed =
            decide_mesh_peer_policy(&peer_policy_input(MeshLane::Metadata), Some(&peer_policy()));
        let allowed_json = allowed.to_json();
        assert_eq!(allowed_json["schema"], "ee.mesh.policy_decision.v1");
        assert_eq!(allowed_json["direction"], "inbound");
        assert_eq!(allowed_json["action"], "allow");
        assert_eq!(allowed_json["reason"], "peer_policy_lane_allowed");
        assert_eq!(allowed_json["policyRef"], "pol_alpha_mesh_policy");
        assert_eq!(allowed_json["materialLane"], "metadata");
        assert_eq!(allowed_json["redaction"], "share");
        assert_eq!(allowed_json["trustLane"], "peerAgent");
        assert_eq!(allowed_json["importTrustClass"], "agent_validated");
        assert_eq!(allowed_json["bodyFetchAllowed"], false);
        assert_eq!(allowed_json["localTruthSideEffectsAllowed"], true);
        assert_eq!(allowed_json["searchOrGraphSideEffectsAllowed"], true);
        assert_eq!(allowed_json["failure"], JsonValue::Null);

        let mut path_policy = peer_policy();
        path_policy.policy_id = "/Users/alice/private/inbound-policy.toml".to_owned();
        let denied =
            decide_mesh_peer_policy(&peer_policy_input(MeshLane::Body), Some(&path_policy));
        let denied_json = denied.to_json();
        assert_eq!(denied_json["action"], "deny");
        assert_eq!(denied_json["reason"], "peer_policy_redaction_denied");
        assert_eq!(denied_json["materialLane"], "body");
        assert_eq!(
            denied_json["failure"]["schema"],
            "ee.mesh.policy_failure_surface.v1"
        );
        assert!(
            !denied_json.to_string().contains("/Users/alice/private"),
            "decision JSON leaked raw policy path: {denied_json}"
        );
    }

    #[test]
    fn mesh_outbound_policy_allows_metadata_share_for_authorized_peer() {
        let decision = decide_mesh_outbound_policy(
            &outbound_policy_input(MeshLane::Metadata),
            Some(&peer_policy()),
        );

        assert_eq!(decision.action, MeshImportDecisionKind::Allow);
        assert_eq!(decision.reason, "outbound_lane_allowed");
        assert_eq!(decision.policy_id.as_deref(), Some("pol_alpha_mesh_policy"));
        assert_eq!(decision.trust_lane, Some(MeshTrustLane::PeerAgent));
        assert_eq!(decision.redaction, MeshRedactionDecision::Share);
        assert!(decision.permits_payload_export());
        assert!(decision.permits_raw_payload_export());
        assert!(!decision.requires_redacted_payload());
    }

    #[test]
    fn mesh_outbound_policy_fails_closed_without_policy_or_with_non_deny_default() {
        let missing = decide_mesh_outbound_policy(&outbound_policy_input(MeshLane::Metadata), None);
        assert_eq!(missing.action, MeshImportDecisionKind::Deny);
        assert_eq!(missing.reason, "missing_peer_policy");
        assert_eq!(missing.redaction, MeshRedactionDecision::Deny);
        assert!(!missing.permits_payload_export());

        let mut policy = peer_policy();
        policy.default_action = MeshLaneDecision::Allow;
        let non_deny_default =
            decide_mesh_outbound_policy(&outbound_policy_input(MeshLane::Metadata), Some(&policy));
        assert_eq!(non_deny_default.action, MeshImportDecisionKind::Reject);
        assert_eq!(non_deny_default.reason, "non_deny_default_action");
        assert!(!non_deny_default.permits_payload_export());
    }

    #[test]
    fn mesh_outbound_policy_denies_workspace_peer_and_origin_mismatches() {
        let mut wrong_workspace = outbound_policy_input(MeshLane::Metadata);
        wrong_workspace.local_workspace_id = "wsp_other";
        let workspace = decide_mesh_outbound_policy(&wrong_workspace, Some(&peer_policy()));
        assert_eq!(workspace.action, MeshImportDecisionKind::Deny);
        assert_eq!(workspace.reason, "peer_policy_workspace_mismatch");

        let mut wrong_peer = outbound_policy_input(MeshLane::Metadata);
        wrong_peer.target_peer_id = "peer_other";
        let peer = decide_mesh_outbound_policy(&wrong_peer, Some(&peer_policy()));
        assert_eq!(peer.action, MeshImportDecisionKind::Deny);
        assert_eq!(peer.reason, "peer_policy_peer_mismatch");

        let mut wrong_origin = outbound_policy_input(MeshLane::Metadata);
        wrong_origin.origin_workspace_id = "wsp_other_origin";
        let origin = decide_mesh_outbound_policy(&wrong_origin, Some(&peer_policy()));
        assert_eq!(origin.action, MeshImportDecisionKind::Deny);
        assert_eq!(origin.reason, "peer_policy_origin_workspace_mismatch");
    }

    #[test]
    fn mesh_outbound_policy_preserves_quarantine_lane_decisions() {
        let decision = decide_mesh_outbound_policy(
            &outbound_policy_input(MeshLane::CurationSignal),
            Some(&peer_policy()),
        );

        assert_eq!(decision.action, MeshImportDecisionKind::Quarantine);
        assert_eq!(decision.reason, "outbound_lane_quarantined");
        assert_eq!(decision.redaction, MeshRedactionDecision::Share);
        assert!(!decision.permits_payload_export());
    }

    #[test]
    fn mesh_outbound_policy_denies_body_and_embedding_by_default() {
        let body = decide_mesh_outbound_policy(
            &outbound_policy_input(MeshLane::Body),
            Some(&peer_policy()),
        );
        assert_eq!(body.action, MeshImportDecisionKind::Deny);
        assert_eq!(body.reason, "outbound_redaction_denied");
        assert_eq!(body.redaction, MeshRedactionDecision::Deny);
        assert!(!body.permits_payload_export());

        let embedding = decide_mesh_outbound_policy(
            &outbound_policy_input(MeshLane::Embedding),
            Some(&peer_policy()),
        );
        assert_eq!(embedding.action, MeshImportDecisionKind::Deny);
        assert_eq!(embedding.reason, "outbound_redaction_denied");
        assert_eq!(embedding.redaction, MeshRedactionDecision::Deny);
        assert!(!embedding.permits_payload_export());
    }

    #[test]
    fn mesh_outbound_policy_allows_only_redacted_body_when_redaction_is_required() {
        let mut policy = peer_policy();
        policy.allowed_lanes.body = Some(MeshLaneDecision::Allow);
        policy.redaction.body = MeshRedactionDecision::Redact;

        let raw =
            decide_mesh_outbound_policy(&outbound_policy_input(MeshLane::Body), Some(&policy));
        assert_eq!(raw.action, MeshImportDecisionKind::Deny);
        assert_eq!(raw.reason, "outbound_payload_requires_redaction");
        assert!(!raw.permits_payload_export());

        let mut redacted_input = outbound_policy_input(MeshLane::Body);
        redacted_input.payload_is_redacted = true;
        let redacted = decide_mesh_outbound_policy(&redacted_input, Some(&policy));
        assert_eq!(redacted.action, MeshImportDecisionKind::Allow);
        assert_eq!(redacted.reason, "outbound_lane_allowed");
        assert_eq!(redacted.redaction, MeshRedactionDecision::Redact);
        assert!(redacted.permits_payload_export());
        assert!(!redacted.permits_raw_payload_export());
        assert!(redacted.requires_redacted_payload());
    }

    #[test]
    fn mesh_outbound_policy_requires_redacted_metadata_when_metadata_redaction_is_required() {
        let mut policy = peer_policy();
        policy.redaction.metadata = MeshRedactionDecision::Redact;

        let raw =
            decide_mesh_outbound_policy(&outbound_policy_input(MeshLane::Metadata), Some(&policy));
        assert_eq!(raw.action, MeshImportDecisionKind::Deny);
        assert_eq!(raw.reason, "outbound_payload_requires_redaction");
        assert!(!raw.permits_payload_export());

        let mut redacted_input = outbound_policy_input(MeshLane::Metadata);
        redacted_input.payload_is_redacted = true;
        let redacted = decide_mesh_outbound_policy(&redacted_input, Some(&policy));
        assert_eq!(redacted.action, MeshImportDecisionKind::Allow);
        assert_eq!(redacted.reason, "outbound_lane_allowed");
        assert_eq!(redacted.redaction, MeshRedactionDecision::Redact);
        assert!(redacted.permits_payload_export());
        assert!(!redacted.permits_raw_payload_export());
        assert!(redacted.requires_redacted_payload());
    }

    #[test]
    fn mesh_outbound_policy_requires_embedding_grant_and_redaction_posture() {
        let mut policy = peer_policy();
        policy.allowed_lanes.embedding = Some(MeshLaneDecision::Allow);
        policy.redaction.embedding = MeshRedactionDecision::Redact;

        let raw =
            decide_mesh_outbound_policy(&outbound_policy_input(MeshLane::Embedding), Some(&policy));
        assert_eq!(raw.action, MeshImportDecisionKind::Deny);
        assert_eq!(raw.reason, "outbound_payload_requires_redaction");

        let mut redacted_input = outbound_policy_input(MeshLane::Embedding);
        redacted_input.payload_is_redacted = true;
        let redacted = decide_mesh_outbound_policy(&redacted_input, Some(&policy));
        assert_eq!(redacted.action, MeshImportDecisionKind::Allow);
        assert_eq!(redacted.redaction, MeshRedactionDecision::Redact);
        assert!(redacted.requires_redacted_payload());

        policy.allowed_lanes.embedding = Some(MeshLaneDecision::Deny);
        let denied = decide_mesh_outbound_policy(&redacted_input, Some(&policy));
        assert_eq!(denied.action, MeshImportDecisionKind::Deny);
        assert_eq!(denied.reason, "outbound_lane_denied");
    }

    #[test]
    fn mesh_outbound_policy_decision_json_is_stable_and_redaction_safe() {
        let allowed = decide_mesh_outbound_policy(
            &outbound_policy_input(MeshLane::Metadata),
            Some(&peer_policy()),
        );
        let allowed_json = allowed.to_json();
        assert_eq!(allowed_json["schema"], "ee.mesh.policy_decision.v1");
        assert_eq!(allowed_json["direction"], "outbound");
        assert_eq!(allowed_json["action"], "allow");
        assert_eq!(allowed_json["reason"], "outbound_lane_allowed");
        assert_eq!(allowed_json["policyRef"], "pol_alpha_mesh_policy");
        assert_eq!(allowed_json["materialLane"], "metadata");
        assert_eq!(allowed_json["redaction"], "share");
        assert_eq!(allowed_json["trustLane"], "peerAgent");
        assert_eq!(allowed_json["payloadExportAllowed"], true);
        assert_eq!(allowed_json["rawPayloadExportAllowed"], true);
        assert_eq!(allowed_json["redactedPayloadRequired"], false);
        assert_eq!(allowed_json["failure"], JsonValue::Null);

        let mut path_policy = peer_policy();
        path_policy.policy_id = "/Users/alice/private/outbound-policy.toml".to_owned();
        let denied =
            decide_mesh_outbound_policy(&outbound_policy_input(MeshLane::Body), Some(&path_policy));
        let denied_json = denied.to_json();
        assert_eq!(denied_json["action"], "deny");
        assert_eq!(denied_json["reason"], "outbound_redaction_denied");
        assert_eq!(denied_json["materialLane"], "body");
        assert_eq!(
            denied_json["failure"]["schema"],
            "ee.mesh.policy_failure_surface.v1"
        );
        assert!(
            !denied_json.to_string().contains("/Users/alice/private"),
            "outbound decision JSON leaked raw policy path: {denied_json}"
        );
    }

    #[test]
    fn mesh_outbound_policy_failure_surface_uses_stable_structured_codes() {
        let missing = decide_mesh_outbound_policy(&outbound_policy_input(MeshLane::Metadata), None);
        let missing_surface = missing
            .failure_surface()
            .expect("denied outbound policy should expose failure fields");
        assert_eq!(missing_surface.code, "mesh_outbound_policy_denied");
        assert_eq!(missing_surface.action, MeshImportDecisionKind::Deny);
        assert_eq!(missing_surface.policy_ref, "missing");
        assert_eq!(missing_surface.reason, "missing_peer_policy");

        let quarantined = decide_mesh_outbound_policy(
            &outbound_policy_input(MeshLane::CurationSignal),
            Some(&peer_policy()),
        );
        let quarantine_surface = quarantined
            .failure_surface()
            .expect("quarantined outbound policy should expose failure fields");
        assert_eq!(quarantine_surface.code, "mesh_outbound_policy_quarantined");
        assert_eq!(
            quarantine_surface.action,
            MeshImportDecisionKind::Quarantine
        );

        let mut policy = peer_policy();
        policy.default_action = MeshLaneDecision::Allow;
        let rejected =
            decide_mesh_outbound_policy(&outbound_policy_input(MeshLane::Metadata), Some(&policy));
        let reject_surface = rejected
            .failure_surface()
            .expect("rejected outbound policy should expose failure fields");
        assert_eq!(reject_surface.code, "mesh_outbound_policy_rejected");
        assert_eq!(reject_surface.action, MeshImportDecisionKind::Reject);

        let allowed = decide_mesh_outbound_policy(
            &outbound_policy_input(MeshLane::Metadata),
            Some(&peer_policy()),
        );
        assert_eq!(allowed.failure_surface(), None);
    }

    #[test]
    fn mesh_outbound_policy_failure_surface_redacts_path_like_policy_refs() {
        let mut policy = peer_policy();
        policy.policy_id = "/Users/alice/private/policy.toml".to_owned();
        let decision =
            decide_mesh_outbound_policy(&outbound_policy_input(MeshLane::Body), Some(&policy));
        let surface = decision
            .failure_surface()
            .expect("denied outbound body should expose failure fields");

        assert_eq!(surface.code, "mesh_outbound_policy_denied");
        assert!(surface.policy_ref.starts_with("mesh_pol_"));
        assert_ne!(surface.policy_ref, "/Users/alice/private/policy.toml");

        let fields = surface.to_json();
        assert_eq!(fields["code"], "mesh_outbound_policy_denied");
        assert_eq!(fields["action"], "deny");
        assert_eq!(fields["reason"], "outbound_redaction_denied");
        assert_eq!(fields["materialLane"], "body");
        assert_eq!(fields["redaction"], "deny");
        assert_eq!(fields["trustLane"], "peerAgent");
        assert!(
            !fields.to_string().contains("/Users/alice/private"),
            "failure surface leaked raw policy path: {fields}"
        );
    }

    #[test]
    fn mesh_import_denied_body_records_decision_without_local_side_effects() {
        let decision = decide_mesh_import(&mesh_input(MeshLane::Body), &[mesh_binding()]);

        assert_eq!(
            decision.workspace_scope_decision,
            MeshImportDecisionKind::Deny
        );
        assert!(!decision.allowed);
        assert!(!decision.permits_local_truth_side_effects());
        assert!(!decision.permits_search_or_graph_side_effects());
        assert!(!decision.retains_payload());
        assert!(!decision.permits_redacted_quarantine_state());
        assert_eq!(decision.reason, "lane_denied");
    }

    #[test]
    fn mesh_import_rejects_malformed_event_and_discards_payload() {
        let input = MeshImportDecisionInput {
            event_validity: MeshEventValidity::Malformed,
            ..mesh_input(MeshLane::Metadata)
        };

        let decision = decide_mesh_import(&input, &[mesh_binding()]);

        assert_eq!(
            decision.workspace_scope_decision,
            MeshImportDecisionKind::Reject
        );
        assert_eq!(decision.reason, "malformed_event");
        assert!(!decision.allowed);
        assert!(!decision.retains_payload());
        assert!(!decision.permits_redacted_quarantine_state());
    }

    #[test]
    fn mesh_import_quarantine_keeps_redacted_evidence_only() {
        let decision = decide_mesh_import(&mesh_input(MeshLane::CurationSignal), &[mesh_binding()]);

        assert_eq!(
            decision.workspace_scope_decision,
            MeshImportDecisionKind::Quarantine
        );
        assert_eq!(decision.reason, "lane_quarantined");
        assert!(!decision.allowed);
        assert!(!decision.permits_local_truth_side_effects());
        assert!(!decision.permits_search_or_graph_side_effects());
        assert!(!decision.retains_payload());
        assert!(decision.permits_redacted_quarantine_state());
    }

    #[test]
    fn mesh_import_denies_unknown_origin_and_missing_lane_by_default() {
        let unknown_origin = MeshImportDecisionInput {
            origin_workspace_id: "wsp_remote_gamma",
            ..mesh_input(MeshLane::Metadata)
        };
        let unknown_origin_decision = decide_mesh_import(&unknown_origin, &[mesh_binding()]);
        assert_eq!(
            unknown_origin_decision.workspace_scope_decision,
            MeshImportDecisionKind::Deny
        );
        assert_eq!(unknown_origin_decision.reason, "unknown_origin_workspace");

        let missing_lane = decide_mesh_import(&mesh_input(MeshLane::Embedding), &[mesh_binding()]);
        assert_eq!(
            missing_lane.workspace_scope_decision,
            MeshImportDecisionKind::Deny
        );
        assert_eq!(missing_lane.reason, "missing_lane_grant");
    }

    #[test]
    fn mesh_import_log_fields_include_workspace_scope_decision_contract() {
        let decision = decide_mesh_import(&mesh_input(MeshLane::Metadata), &[mesh_binding()]);
        let fields = decision.to_log_fields();

        for key in [
            "workspace_scope_decision",
            "workspace_id",
            "origin_workspace_id",
            "peer_group_id",
            "producer_peer_id",
            "material_lane",
            "allowed",
            "reason",
        ] {
            assert!(fields.get(key).is_some(), "missing log field {key}");
        }
        assert_eq!(fields["workspace_scope_decision"], "allow");
        assert_eq!(fields["workspace_id"], "wsp_local_alpha");
        assert_eq!(fields["origin_workspace_id"], "wsp_remote_beta");
        assert_eq!(fields["peer_group_id"], "pg_alpha_mesh");
        assert_eq!(fields["producer_peer_id"], "peer_builder_one");
        assert_eq!(fields["material_lane"], "metadata");
        assert_eq!(fields["allowed"], true);
        assert_eq!(fields["reason"], "lane_allowed");
    }

    fn mesh_display_input<'a>(
        decision: &'a MeshImportDecision,
        origin_workspace_label: Option<&'a str>,
    ) -> MeshDisplayProvenanceInput<'a> {
        MeshDisplayProvenanceInput {
            decision,
            cached_material_id: "mesh_mat_123",
            origin_workspace_label,
            producer_peer_label: Some("builder-one"),
            import_decision_id: Some("mesh_dec_456"),
            ledger_cursor: Some("mesh_ledger_789"),
            trust_lane: "mesh_metadata",
            redaction_posture: "standard",
        }
    }

    #[test]
    fn mesh_display_provenance_preserves_allowed_namespace_contract() {
        let decision = decide_mesh_import(&mesh_input(MeshLane::Metadata), &[mesh_binding()]);
        let provenance =
            mesh_display_provenance(&mesh_display_input(&decision, Some("remote-beta")))
                .expect("allowed mesh material should expose redacted provenance");

        assert_eq!(provenance.cached_material_id, "mesh_mat_123");
        assert_eq!(provenance.origin_workspace_alias, "remote-beta");
        assert_eq!(provenance.producer_peer, "builder-one");
        assert_eq!(provenance.material_lane, MeshLane::Metadata);
        assert_eq!(provenance.import_decision_ref, "mesh_dec_456");
        assert_eq!(provenance.trust_lane, "mesh_metadata");
        assert_eq!(provenance.redaction_posture, "standard");

        let json = provenance.to_json();
        for key in [
            "cachedMaterialId",
            "originWorkspaceAlias",
            "producerPeer",
            "materialLane",
            "importDecisionRef",
            "trustLane",
            "redactionPosture",
        ] {
            assert!(json.get(key).is_some(), "missing mesh provenance key {key}");
        }
    }

    #[test]
    fn mesh_display_provenance_redacts_path_like_origin_workspace_label() {
        let decision = decide_mesh_import(&mesh_input(MeshLane::Metadata), &[mesh_binding()]);
        let provenance = mesh_display_provenance(&mesh_display_input(
            &decision,
            Some("/Users/alice/private/repo"),
        ))
        .expect("allowed mesh material should expose redacted provenance");

        assert!(provenance.origin_workspace_alias.starts_with("mesh_ns_"));
        assert_ne!(
            provenance.origin_workspace_alias,
            "/Users/alice/private/repo"
        );
        assert_eq!(
            provenance.origin_workspace_alias,
            stable_mesh_alias("mesh_ns", "wsp_remote_beta")
        );
    }

    #[test]
    fn mesh_display_provenance_redacts_path_like_producer_peer_label() {
        let decision = decide_mesh_import(&mesh_input(MeshLane::Metadata), &[mesh_binding()]);
        let mut input = mesh_display_input(&decision, Some("remote-beta"));
        input.producer_peer_label = Some("/Users/alice/private/peer-agent");

        let provenance = mesh_display_provenance(&input)
            .expect("allowed mesh material should expose redacted provenance");

        assert_eq!(
            provenance.producer_peer,
            stable_mesh_alias("mesh_peer", "peer_builder_one")
        );
        assert_ne!(provenance.producer_peer, "peer_builder_one");
        assert_ne!(provenance.producer_peer, "/Users/alice/private/peer-agent");
    }

    #[test]
    fn mesh_display_provenance_aliases_raw_producer_peer_id_without_safe_label() {
        let input_decision = MeshImportDecisionInput {
            producer_peer_id: "nodekey:0123456789abcdef0123456789abcdef",
            ..mesh_input(MeshLane::Metadata)
        };
        let decision = mesh_decision(
            &input_decision,
            Some("pg_alpha_mesh".to_owned()),
            MeshImportDecisionKind::Allow,
            "lane_allowed",
        );
        let mut input = mesh_display_input(&decision, Some("remote-beta"));
        input.producer_peer_label = None;

        let provenance = mesh_display_provenance(&input)
            .expect("allowed mesh material should expose redacted provenance");

        assert_eq!(
            provenance.producer_peer,
            stable_mesh_alias("mesh_peer", input_decision.producer_peer_id)
        );
        assert_ne!(provenance.producer_peer, input_decision.producer_peer_id);
        assert!(!provenance.producer_peer.contains("nodekey"));
    }

    #[test]
    fn mesh_display_provenance_uses_ledger_cursor_when_decision_id_is_absent() {
        let decision = decide_mesh_import(&mesh_input(MeshLane::Metadata), &[mesh_binding()]);
        let mut input = mesh_display_input(&decision, Some("remote-beta"));
        input.import_decision_id = None;

        let provenance = mesh_display_provenance(&input)
            .expect("allowed mesh material should expose redacted provenance");

        assert_eq!(provenance.import_decision_ref, "mesh_ledger_789");
    }

    #[test]
    fn mesh_display_provenance_aliases_path_like_import_decision_ref() {
        let decision = decide_mesh_import(&mesh_input(MeshLane::Metadata), &[mesh_binding()]);
        let mut input = mesh_display_input(&decision, Some("remote-beta"));
        input.import_decision_id = Some("/Users/alice/private/mesh_decision.json");

        let provenance = mesh_display_provenance(&input)
            .expect("allowed mesh material should expose redacted provenance");

        assert_eq!(
            provenance.import_decision_ref,
            stable_mesh_alias("mesh_dec", "/Users/alice/private/mesh_decision.json")
        );
        assert_ne!(
            provenance.import_decision_ref,
            "/Users/alice/private/mesh_decision.json"
        );
    }

    #[test]
    fn mesh_display_provenance_aliases_path_like_ledger_cursor() {
        let decision = decide_mesh_import(&mesh_input(MeshLane::Metadata), &[mesh_binding()]);
        let mut input = mesh_display_input(&decision, Some("remote-beta"));
        input.import_decision_id = None;
        input.ledger_cursor = Some("agent-mail://BlueLake/private/mesh-ledger");

        let provenance = mesh_display_provenance(&input)
            .expect("allowed mesh material should expose redacted provenance");

        assert_eq!(
            provenance.import_decision_ref,
            stable_mesh_alias("mesh_dec", "agent-mail://BlueLake/private/mesh-ledger")
        );
        assert!(!provenance.import_decision_ref.contains("agent-mail"));
    }

    #[test]
    fn mesh_display_provenance_filters_non_allowed_material_from_query_surfaces() {
        let denied = decide_mesh_import(&mesh_input(MeshLane::Body), &[mesh_binding()]);
        let quarantined =
            decide_mesh_import(&mesh_input(MeshLane::CurationSignal), &[mesh_binding()]);
        let rejected_input = MeshImportDecisionInput {
            event_validity: MeshEventValidity::Malformed,
            ..mesh_input(MeshLane::Metadata)
        };
        let rejected = decide_mesh_import(&rejected_input, &[mesh_binding()]);

        assert!(
            mesh_display_provenance(&mesh_display_input(&denied, Some("remote-beta"))).is_none()
        );
        assert!(
            mesh_display_provenance(&mesh_display_input(&quarantined, Some("remote-beta")))
                .is_none()
        );
        assert!(
            mesh_display_provenance(&mesh_display_input(&rejected, Some("remote-beta"))).is_none()
        );
    }

    #[test]
    fn mesh_query_visibility_allows_complete_allowed_metadata() {
        let metadata = json!({
            "mesh": {
                "workspaceScopeDecision": "allow",
                "workspaceId": "wsp_local_alpha",
                "cachedMaterialId": "mesh_mat_123",
                "originWorkspaceId": "wsp_remote_beta",
                "originWorkspaceLabel": "remote-beta",
                "producerPeerId": "peer_builder_one",
                "producerPeerLabel": "builder-one",
                "materialLane": "metadata",
                "importDecisionId": "mesh_dec_456",
                "trustLane": "mesh_metadata",
                "redactionPosture": "standard",
                "policyDecision": {
                    "schema": "ee.mesh.policy_decision.v1",
                    "direction": "inbound",
                    "action": "allow"
                }
            }
        });

        let MeshQueryVisibility::Allowed(provenance) = mesh_query_visibility(Some(&metadata))
        else {
            panic!("allowed mesh metadata should be query visible");
        };

        assert_eq!(provenance.cached_material_id, "mesh_mat_123");
        assert_eq!(provenance.origin_workspace_alias, "remote-beta");
        assert_eq!(provenance.producer_peer, "builder-one");
        assert_eq!(provenance.import_decision_ref, "mesh_dec_456");
    }

    #[test]
    fn mesh_query_visibility_blocks_incomplete_or_denied_mesh_metadata() {
        let denied = json!({
            "mesh": {
                "workspaceScopeDecision": "deny",
                "cachedMaterialId": "mesh_mat_123",
                "originWorkspaceId": "wsp_remote_beta",
                "producerPeerId": "peer_builder_one",
                "materialLane": "metadata",
                "trustLane": "mesh_metadata",
                "redactionPosture": "standard"
            }
        });
        let incomplete = json!({
            "mesh": {
                "workspaceScopeDecision": "allow",
                "cachedMaterialId": "mesh_mat_123",
                "originWorkspaceId": "wsp_remote_beta",
                "producerPeerId": "peer_builder_one",
                "materialLane": "metadata"
            }
        });
        let quarantined = json!({
            "mesh": {
                "workspaceScopeDecision": "quarantine",
                "cachedMaterialId": "mesh_mat_123",
                "originWorkspaceId": "wsp_remote_beta",
                "producerPeerId": "peer_builder_one",
                "materialLane": "curationSignal",
                "trustLane": "mesh_curation",
                "redactionPosture": "standard"
            }
        });
        let rejected = json!({
            "mesh": {
                "workspaceScopeDecision": "reject",
                "cachedMaterialId": "mesh_mat_123",
                "originWorkspaceId": "wsp_remote_beta",
                "producerPeerId": "peer_builder_one",
                "materialLane": "metadata",
                "trustLane": "mesh_metadata",
                "redactionPosture": "standard"
            }
        });
        let missing_decision = json!({
            "mesh": {
                "cachedMaterialId": "mesh_mat_123",
                "originWorkspaceId": "wsp_remote_beta",
                "producerPeerId": "peer_builder_one",
                "materialLane": "metadata",
                "trustLane": "mesh_metadata",
                "redactionPosture": "standard"
            }
        });
        let denied_policy_decision = json!({
            "mesh": {
                "workspaceScopeDecision": "allow",
                "cachedMaterialId": "mesh_mat_123",
                "originWorkspaceId": "wsp_remote_beta",
                "producerPeerId": "peer_builder_one",
                "materialLane": "metadata",
                "trustLane": "mesh_metadata",
                "redactionPosture": "standard",
                "policyDecision": {
                    "schema": "ee.mesh.policy_decision.v1",
                    "direction": "inbound",
                    "action": "deny"
                }
            }
        });
        let malformed_policy_decision = json!({
            "mesh": {
                "workspaceScopeDecision": "allow",
                "cachedMaterialId": "mesh_mat_123",
                "originWorkspaceId": "wsp_remote_beta",
                "producerPeerId": "peer_builder_one",
                "materialLane": "metadata",
                "trustLane": "mesh_metadata",
                "redactionPosture": "standard",
                "policyDecision": {
                    "schema": "ee.mesh.policy_decision.v1",
                    "direction": "inbound"
                }
            }
        });
        let outbound_policy_decision = json!({
            "mesh": {
                "workspaceScopeDecision": "allow",
                "cachedMaterialId": "mesh_mat_123",
                "originWorkspaceId": "wsp_remote_beta",
                "producerPeerId": "peer_builder_one",
                "materialLane": "metadata",
                "trustLane": "mesh_metadata",
                "redactionPosture": "standard",
                "policyDecision": {
                    "schema": "ee.mesh.policy_decision.v1",
                    "direction": "outbound",
                    "action": "allow"
                }
            }
        });
        let string_policy_decision = json!({
            "mesh": {
                "workspaceScopeDecision": "allow",
                "cachedMaterialId": "mesh_mat_123",
                "originWorkspaceId": "wsp_remote_beta",
                "producerPeerId": "peer_builder_one",
                "materialLane": "metadata",
                "trustLane": "mesh_metadata",
                "redactionPosture": "standard",
                "policyDecisionJson": "{\"schema\":\"ee.mesh.policy_decision.v1\",\"direction\":\"inbound\",\"action\":\"allow\"}"
            }
        });
        let standalone_policy_decision = json!({
            "policyDecision": {
                "schema": "ee.mesh.policy_decision.v1",
                "direction": "inbound",
                "action": "allow"
            }
        });
        let standalone_policy_failure = json!({
            "policyFailureSurface": {
                "schema": "ee.mesh.policy_failure_surface.v1",
                "code": "mesh_peer_policy_denied",
                "action": "deny"
            }
        });

        assert_eq!(
            mesh_query_visibility(Some(&denied)),
            MeshQueryVisibility::Blocked
        );
        assert_eq!(
            mesh_query_visibility(Some(&quarantined)),
            MeshQueryVisibility::Blocked
        );
        assert_eq!(
            mesh_query_visibility(Some(&rejected)),
            MeshQueryVisibility::Blocked
        );
        assert_eq!(
            mesh_query_visibility(Some(&missing_decision)),
            MeshQueryVisibility::Blocked
        );
        assert_eq!(
            mesh_query_visibility(Some(&denied_policy_decision)),
            MeshQueryVisibility::Blocked
        );
        assert_eq!(
            mesh_query_visibility(Some(&malformed_policy_decision)),
            MeshQueryVisibility::Blocked
        );
        assert_eq!(
            mesh_query_visibility(Some(&outbound_policy_decision)),
            MeshQueryVisibility::Blocked
        );
        assert_eq!(
            mesh_query_visibility(Some(&string_policy_decision)),
            MeshQueryVisibility::Blocked
        );
        assert_eq!(
            mesh_query_visibility(Some(&standalone_policy_decision)),
            MeshQueryVisibility::Blocked
        );
        assert_eq!(
            mesh_query_visibility(Some(&standalone_policy_failure)),
            MeshQueryVisibility::Blocked
        );
        assert_eq!(
            mesh_query_visibility(Some(&incomplete)),
            MeshQueryVisibility::Blocked
        );
        assert_eq!(mesh_query_visibility(None), MeshQueryVisibility::Local);
    }
}
