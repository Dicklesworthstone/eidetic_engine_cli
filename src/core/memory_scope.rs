use std::collections::BTreeSet;
use std::path::Path;
use std::str::FromStr;

use serde_json::{Value as JsonValue, json};

use crate::config::{
    ConfigFile, EnvVar, MeshLane, MeshLaneDecision, MeshPeerGroupBinding, read_env_var,
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
        import_decision_ref: input
            .import_decision_id
            .or(input.ledger_cursor)
            .unwrap_or("unrecorded")
            .to_owned(),
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
    let Ok(contents) = std::fs::read_to_string(config_path) else {
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
        .unwrap_or_else(|| producer_peer_id.to_owned())
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
    fn mesh_display_provenance_uses_ledger_cursor_when_decision_id_is_absent() {
        let decision = decide_mesh_import(&mesh_input(MeshLane::Metadata), &[mesh_binding()]);
        let mut input = mesh_display_input(&decision, Some("remote-beta"));
        input.import_decision_id = None;

        let provenance = mesh_display_provenance(&input)
            .expect("allowed mesh material should expose redacted provenance");

        assert_eq!(provenance.import_decision_ref, "mesh_ledger_789");
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
                "redactionPosture": "standard"
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
            mesh_query_visibility(Some(&incomplete)),
            MeshQueryVisibility::Blocked
        );
        assert_eq!(mesh_query_visibility(None), MeshQueryVisibility::Local);
    }
}
