//! SRR6.5 mesh peer policy selection and authorization facade.
//!
//! The low-level policy decisions live in `core::memory_scope`; this module
//! owns the mesh-facing registry that selects exactly one policy before callers
//! evaluate inbound imports or outbound exports. Missing and ambiguous matches
//! are fail-closed so a peer can never gain access because a policy record is
//! absent, duplicated, or too broad.

use std::collections::BTreeMap;

use serde_json::{Value as JsonValue, json};

use crate::config::{ConfigFile, MeshLane, MeshLaneDecision};
use crate::db::StoredMeshLaneGrantState;

pub use crate::core::memory_scope::{
    MeshBodyFetchPolicy, MeshEventValidity, MeshImportDecisionInput, MeshImportDecisionKind,
    MeshOutboundPolicyDecision, MeshOutboundPolicyDecisionInput, MeshPeerLaneOverride,
    MeshPeerPolicy, MeshPeerPolicyDecision, MeshPeerPolicyDecisionInput, MeshPolicyFailureSurface,
    MeshRedactionDecision, MeshRedactionPolicy, MeshTrustLane,
    decide_mesh_import_with_lane_override, decide_mesh_outbound_policy, decide_mesh_peer_policy,
};

/// Pure in-memory registry for configured mesh peer policies.
///
/// A registry lookup is intentionally exact: workspace id, peer id, and origin
/// workspace id must all match. Callers use [`decide_inbound`] and
/// [`decide_outbound`] when they only need fail-closed authorization, or the
/// `*_checked` variants when they need to surface a structured lookup failure.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MeshPeerPolicyRegistry {
    policies: Vec<MeshPeerPolicy>,
    lane_grant_states: BTreeMap<(String, String), StoredMeshLaneGrantState>,
    current_approval_config_digest: Option<String>,
}

impl MeshPeerPolicyRegistry {
    #[must_use]
    pub fn new(policies: impl IntoIterator<Item = MeshPeerPolicy>) -> Self {
        Self {
            policies: policies.into_iter().collect(),
            lane_grant_states: BTreeMap::new(),
            current_approval_config_digest: None,
        }
    }

    #[must_use]
    pub fn from_config(config: &ConfigFile) -> Self {
        let policies = config
            .mesh
            .peer_policies
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(MeshPeerPolicy::from);
        Self::new(policies)
    }

    /// Build a registry from one successfully parsed, exact config snapshot.
    /// Missing config is represented by a valid empty byte slice; callers must
    /// use [`Self::from_config`] instead when bytes were unreadable or invalid.
    #[must_use]
    pub fn from_config_snapshot(config: &ConfigFile, config_bytes: &[u8]) -> Self {
        Self::from_config(config).with_approval_config_snapshot(config_bytes)
    }

    /// Attach the exact successfully parsed config bytes to an existing
    /// registry. This is useful when policy records are supplied by a caller
    /// that already owns one coherent config snapshot.
    #[must_use]
    pub fn with_approval_config_snapshot(mut self, config_bytes: &[u8]) -> Self {
        self.current_approval_config_digest = Some(
            crate::mesh::lane_grant::approval_config_digest(config_bytes),
        );
        self
    }

    #[must_use]
    pub fn policies(&self) -> &[MeshPeerPolicy] {
        &self.policies
    }

    /// Add durable exact-peer lane-grant state loaded from the same database
    /// snapshot as the command's target workspace.
    #[must_use]
    pub fn with_lane_grant_states(
        mut self,
        states: impl IntoIterator<Item = StoredMeshLaneGrantState>,
    ) -> Self {
        self.lane_grant_states = states
            .into_iter()
            .map(|state| ((state.workspace_id.clone(), state.peer_id.clone()), state))
            .collect();
        self
    }

    #[must_use]
    pub fn lane_grant_states(&self) -> &BTreeMap<(String, String), StoredMeshLaneGrantState> {
        &self.lane_grant_states
    }

    #[must_use]
    pub fn current_approval_config_digest(&self) -> Option<&str> {
        self.current_approval_config_digest.as_deref()
    }

    #[must_use]
    pub fn lane_grant_state_for(
        &self,
        local_workspace_id: &str,
        peer_id: &str,
    ) -> Option<&StoredMeshLaneGrantState> {
        self.lane_grant_states
            .get(&(local_workspace_id.to_owned(), peer_id.to_owned()))
    }

    /// Return the durable peer consent generation, with the absent all-inherit
    /// state represented as generation zero.
    #[must_use]
    pub fn lane_grant_generation_for(&self, local_workspace_id: &str, peer_id: &str) -> u64 {
        self.lane_grant_state_for(local_workspace_id, peer_id)
            .map_or(0, |state| state.grant_generation)
    }

    /// Resolve one current exact peer/lane override. A state whose target
    /// adapter no longer matches the enrolled peer is ignored fail-closed and
    /// must be refreshed by a new approval mutation.
    #[must_use]
    pub fn lane_override_for(
        &self,
        local_workspace_id: &str,
        peer_id: &str,
        material_lane: MeshLane,
    ) -> Option<MeshLaneDecision> {
        self.lane_grant_state_for(local_workspace_id, peer_id)
            .filter(|state| {
                state.target_matches_current_peer
                    && state.target_adapter.peer_id == peer_id
                    && state.workspace_id == local_workspace_id
            })
            .and_then(|state| {
                state.effective_override_for(
                    material_lane,
                    self.current_approval_config_digest.as_deref(),
                )
            })
    }

    /// Project the exact durable override needed by the inbound membership
    /// gate. The gate rechecks this identity tuple after workspace/peer/origin
    /// membership succeeds.
    #[must_use]
    pub fn inbound_membership_override(
        &self,
        input: &MeshImportDecisionInput<'_>,
    ) -> Option<MeshPeerLaneOverride> {
        self.lane_override_for(
            input.local_workspace_id,
            input.producer_peer_id,
            input.material_lane,
        )
        .map(|decision| MeshPeerLaneOverride {
            local_workspace_id: input.local_workspace_id.to_owned(),
            peer_id: input.producer_peer_id.to_owned(),
            material_lane: input.material_lane,
            decision,
        })
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.policies.len()
    }

    /// Select exactly one inbound policy for a peer-produced material lane.
    ///
    /// Multiple matching records are rejected as an authorization error; callers
    /// must make the policy table unambiguous instead of relying on iteration
    /// order.
    pub fn select_inbound_policy(
        &self,
        input: &MeshPeerPolicyDecisionInput<'_>,
    ) -> Result<&MeshPeerPolicy, MeshPeerPolicyLookupError> {
        let matches = self
            .policies
            .iter()
            .filter(|policy| {
                policy.workspace_id == input.local_workspace_id
                    && policy.peer_id == input.producer_peer_id
                    && policy
                        .origin_workspace_ids
                        .iter()
                        .any(|origin| origin == input.origin_workspace_id)
            })
            .collect::<Vec<_>>();

        select_unique_policy(
            MeshPolicyDirection::Inbound,
            input.local_workspace_id,
            input.producer_peer_id,
            input.origin_workspace_id,
            input.material_lane,
            matches,
        )
    }

    /// Select exactly one outbound policy for material we intend to expose to a
    /// peer.
    pub fn select_outbound_policy(
        &self,
        input: &MeshOutboundPolicyDecisionInput<'_>,
    ) -> Result<&MeshPeerPolicy, MeshPeerPolicyLookupError> {
        let matches = self
            .policies
            .iter()
            .filter(|policy| {
                policy.workspace_id == input.local_workspace_id
                    && policy.peer_id == input.target_peer_id
                    && policy
                        .origin_workspace_ids
                        .iter()
                        .any(|origin| origin == input.origin_workspace_id)
            })
            .collect::<Vec<_>>();

        select_unique_policy(
            MeshPolicyDirection::Outbound,
            input.local_workspace_id,
            input.target_peer_id,
            input.origin_workspace_id,
            input.material_lane,
            matches,
        )
    }

    /// Fail-closed inbound authorization. Missing or ambiguous policy lookup is
    /// treated the same as no policy: the returned decision denies the import.
    #[must_use]
    pub fn decide_inbound(
        &self,
        input: &MeshPeerPolicyDecisionInput<'_>,
    ) -> MeshPeerPolicyDecision {
        match self.effective_inbound_policy(input) {
            Ok(policy) => decide_mesh_peer_policy(input, Some(&policy)),
            Err(_) => decide_mesh_peer_policy(input, None),
        }
    }

    /// Inbound authorization with a structured lookup error for diagnostics.
    pub fn decide_inbound_checked(
        &self,
        input: &MeshPeerPolicyDecisionInput<'_>,
    ) -> Result<MeshPeerPolicyDecision, MeshPeerPolicyLookupError> {
        let policy = self.effective_inbound_policy(input)?;
        Ok(decide_mesh_peer_policy(input, Some(&policy)))
    }

    /// Fail-closed outbound authorization. Missing or ambiguous policy lookup is
    /// treated the same as no policy: the returned decision denies the export.
    #[must_use]
    pub fn decide_outbound(
        &self,
        input: &MeshOutboundPolicyDecisionInput<'_>,
    ) -> MeshOutboundPolicyDecision {
        match self.effective_outbound_policy(input) {
            Ok(policy) => decide_mesh_outbound_policy(input, Some(&policy)),
            Err(_) => decide_mesh_outbound_policy(input, None),
        }
    }

    /// Outbound authorization with a structured lookup error for diagnostics.
    pub fn decide_outbound_checked(
        &self,
        input: &MeshOutboundPolicyDecisionInput<'_>,
    ) -> Result<MeshOutboundPolicyDecision, MeshPeerPolicyLookupError> {
        let policy = self.effective_outbound_policy(input)?;
        Ok(decide_mesh_outbound_policy(input, Some(&policy)))
    }

    /// Return the selected inbound policy with only this exact peer/lane's
    /// durable override applied to an owned clone.
    pub fn effective_inbound_policy(
        &self,
        input: &MeshPeerPolicyDecisionInput<'_>,
    ) -> Result<MeshPeerPolicy, MeshPeerPolicyLookupError> {
        let policy = self.select_inbound_policy(input)?;
        Ok(self.policy_with_lane_override(
            policy,
            input.local_workspace_id,
            input.producer_peer_id,
            input.material_lane,
        ))
    }

    /// Return the selected outbound policy with only this exact peer/lane's
    /// durable override applied to an owned clone.
    pub fn effective_outbound_policy(
        &self,
        input: &MeshOutboundPolicyDecisionInput<'_>,
    ) -> Result<MeshPeerPolicy, MeshPeerPolicyLookupError> {
        let policy = self.select_outbound_policy(input)?;
        Ok(self.policy_with_lane_override(
            policy,
            input.local_workspace_id,
            input.target_peer_id,
            input.material_lane,
        ))
    }

    fn policy_with_lane_override(
        &self,
        policy: &MeshPeerPolicy,
        local_workspace_id: &str,
        peer_id: &str,
        material_lane: MeshLane,
    ) -> MeshPeerPolicy {
        let mut effective = policy.clone();
        if let Some(decision) = self.lane_override_for(local_workspace_id, peer_id, material_lane) {
            set_lane_override(&mut effective, material_lane, decision);
        }
        effective
    }
}

impl From<&ConfigFile> for MeshPeerPolicyRegistry {
    fn from(config: &ConfigFile) -> Self {
        Self::from_config(config)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeshPolicyDirection {
    Inbound,
    Outbound,
}

impl MeshPolicyDirection {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MeshPeerPolicyLookupError {
    Missing(Box<MeshPeerPolicyLookupFailure>),
    Ambiguous(Box<MeshPeerPolicyLookupFailure>),
}

impl MeshPeerPolicyLookupError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Missing(_) => "mesh_peer_policy_lookup_missing",
            Self::Ambiguous(_) => "mesh_peer_policy_lookup_ambiguous",
        }
    }

    #[must_use]
    pub fn failure(&self) -> &MeshPeerPolicyLookupFailure {
        match self {
            Self::Missing(failure) | Self::Ambiguous(failure) => failure,
        }
    }

    #[must_use]
    pub fn to_json(&self) -> JsonValue {
        let failure = self.failure();
        json!({
            "schema": "ee.mesh.policy_lookup_failure.v1",
            "code": self.code(),
            "direction": failure.direction.as_str(),
            "reason": failure.reason,
            "localWorkspaceRef": &failure.local_workspace_ref,
            "peerRef": &failure.peer_ref,
            "originWorkspaceRef": &failure.origin_workspace_ref,
            "materialLane": mesh_lane_name(failure.material_lane),
            "matchingPolicyRefs": &failure.matching_policy_refs,
        })
    }
}

impl std::fmt::Display for MeshPeerPolicyLookupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let failure = self.failure();
        write!(
            formatter,
            "{}: {} policy for peer {} in workspace {} / origin {} on {}",
            self.code(),
            failure.reason,
            failure.peer_ref,
            failure.local_workspace_ref,
            failure.origin_workspace_ref,
            mesh_lane_name(failure.material_lane)
        )
    }
}

impl std::error::Error for MeshPeerPolicyLookupError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshPeerPolicyLookupFailure {
    pub direction: MeshPolicyDirection,
    pub reason: &'static str,
    pub local_workspace_ref: String,
    pub peer_ref: String,
    pub origin_workspace_ref: String,
    pub material_lane: MeshLane,
    pub matching_policy_refs: Vec<String>,
}

fn select_unique_policy<'a>(
    direction: MeshPolicyDirection,
    local_workspace_id: &str,
    peer_id: &str,
    origin_workspace_id: &str,
    material_lane: MeshLane,
    matches: Vec<&'a MeshPeerPolicy>,
) -> Result<&'a MeshPeerPolicy, MeshPeerPolicyLookupError> {
    match matches.len() {
        1 => Ok(matches[0]),
        0 => Err(MeshPeerPolicyLookupError::Missing(Box::new(
            lookup_failure(
                direction,
                "missing_peer_policy",
                local_workspace_id,
                peer_id,
                origin_workspace_id,
                material_lane,
                Vec::new(),
            ),
        ))),
        _ => {
            let policy_refs = matches
                .iter()
                .map(|policy| mesh_policy_ref(&policy.policy_id))
                .collect();
            Err(MeshPeerPolicyLookupError::Ambiguous(Box::new(
                lookup_failure(
                    direction,
                    "ambiguous_peer_policy",
                    local_workspace_id,
                    peer_id,
                    origin_workspace_id,
                    material_lane,
                    policy_refs,
                ),
            )))
        }
    }
}

fn lookup_failure(
    direction: MeshPolicyDirection,
    reason: &'static str,
    local_workspace_id: &str,
    peer_id: &str,
    origin_workspace_id: &str,
    material_lane: MeshLane,
    matching_policy_refs: Vec<String>,
) -> MeshPeerPolicyLookupFailure {
    MeshPeerPolicyLookupFailure {
        direction,
        reason,
        local_workspace_ref: redaction_safe_ref("mesh_ws", local_workspace_id),
        peer_ref: redaction_safe_ref("mesh_peer", peer_id),
        origin_workspace_ref: redaction_safe_ref("mesh_origin", origin_workspace_id),
        material_lane,
        matching_policy_refs,
    }
}

fn mesh_policy_ref(policy_id: &str) -> String {
    redaction_safe_ref("mesh_pol", policy_id)
}

fn redaction_safe_ref(prefix: &str, value: &str) -> String {
    redaction_safe_label(value).unwrap_or_else(|| stable_mesh_alias(prefix, value))
}

fn redaction_safe_label(label: &str) -> Option<String> {
    let label = label.trim();
    if label.is_empty()
        || label.contains('/')
        || label.contains('\\')
        || label.contains(':')
        || label.contains('~')
        || label.chars().any(char::is_control)
        || label_has_secret_marker(label)
    {
        None
    } else {
        Some(label.to_owned())
    }
}

fn label_has_secret_marker(label: &str) -> bool {
    // bd-brt3i: normalize both label and markers so casing- and
    // separator-variant labels (apiKey, api-key, api.key,
    // PRIVATE_KEY, private-key, ssh.key, sk-live, etc.) cannot
    // slip past the substring match. Lowercase + drop the three
    // separator chars we observe in the wild (`_`, `-`, `.`).
    // Markers with intentional separator prefixes (`ghp_`,
    // `xoxb-`, `xoxp-`) survive because their alphanumeric core
    // (`ghp`, `xoxb`, `xoxp`) is preserved under the same
    // normalization.
    fn normalize(value: &str) -> String {
        value
            .chars()
            .filter_map(|c| match c {
                '_' | '-' | '.' => None,
                other => Some(other.to_ascii_lowercase()),
            })
            .collect()
    }
    let normalized = normalize(label);
    [
        "api_key",
        "apikey",
        "secret",
        "token",
        "password",
        "passwd",
        "credential",
        "private_key",
        "ssh_key",
        "bearer",
        "sk_live",
        "sk_test",
        "ghp_",
        "xoxb-",
        "xoxp-",
    ]
    .iter()
    .any(|marker| normalized.contains(&normalize(marker)))
}

fn stable_mesh_alias(prefix: &str, value: &str) -> String {
    let hash = blake3::hash(value.as_bytes()).to_hex();
    format!("{prefix}_{}", &hash[..10])
}

fn set_lane_override(policy: &mut MeshPeerPolicy, lane: MeshLane, decision: MeshLaneDecision) {
    match lane {
        MeshLane::Metadata => policy.allowed_lanes.metadata = Some(decision),
        MeshLane::Body => policy.allowed_lanes.body = Some(decision),
        MeshLane::Embedding => policy.allowed_lanes.embedding = Some(decision),
        MeshLane::GraphLink => policy.allowed_lanes.graph_link = Some(decision),
        MeshLane::RevisionNotice => policy.allowed_lanes.revision_notice = Some(decision),
        MeshLane::CurationSignal => policy.allowed_lanes.curation_signal = Some(decision),
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

#[cfg(test)]
mod tests {
    use serde_json::Value as JsonValue;

    use super::*;
    use crate::config::{MeshLaneDecision, MeshLaneGrants};
    use crate::models::TrustClass;

    #[test]
    fn label_has_secret_marker_catches_casing_and_separator_variants() {
        // bd-brt3i: labels mixing case and the three separator
        // variants (_, -, .) must all be flagged as secret-like.
        // Prior implementation lowercased + substring-matched a
        // fixed marker list, missing kebab/dot variants of
        // underscore-named markers (private_key, ssh_key, sk_live,
        // sk_test) and camelCase forms when the camelCase
        // lowercased shape was not separately enumerated.
        for label in [
            // api_key family
            "api_key",
            "api-key",
            "api.key",
            "apiKey",
            "API_KEY",
            "API-KEY",
            "API.KEY",
            // private_key family
            "private_key",
            "private-key",
            "private.key",
            "privateKey",
            "PRIVATE_KEY",
            "PRIVATE-KEY",
            // ssh_key family
            "ssh_key",
            "ssh-key",
            "ssh.key",
            "sshKey",
            "SSH_KEY",
            // sk_live / sk_test (Stripe live/test secret keys)
            "sk_live",
            "sk-live",
            "sk.live",
            "skLive",
            "sk_test",
            "sk-test",
            // existing-tested forms remain caught
            "secret",
            "TOKEN",
            "password",
            "passWord",
            "PassWord",
            "credential",
            "Credentials",
            "bearer",
            "ghp_abc123",
            "ghp-abc123",
            "xoxb-something",
            "xoxbsomething",
            "xoxp-team",
            "xoxpteam",
        ] {
            assert!(
                label_has_secret_marker(label),
                "expected `{label}` to be flagged as secret-like"
            );
        }
    }

    #[test]
    fn label_has_secret_marker_does_not_overmatch_benign_labels() {
        // bd-brt3i: normalization must not turn legitimate labels
        // into spurious matches. Verify a handful of benign mesh
        // labels survive — these are domain terms that appear in
        // policy IDs, peer aliases, and lane names.
        for label in [
            "policy_alpha",
            "peer_builder_one",
            "metadata",
            "graphLink",
            "revisionNotice",
            "wsp_remote_beta",
            "trust_lane",
            "redaction_posture",
            "share",
            "redact",
            "deny",
            // Tricky edge: contains "ke" + "y" but not the marker
            "fake_y_label",
            "monkey-business",
        ] {
            assert!(
                !label_has_secret_marker(label),
                "expected `{label}` to NOT be flagged as secret-like"
            );
        }
    }

    fn policy(policy_id: &str, peer_id: &str, origin_workspace_id: &str) -> MeshPeerPolicy {
        MeshPeerPolicy {
            policy_id: policy_id.to_owned(),
            workspace_id: "wsp_local_alpha".to_owned(),
            peer_id: peer_id.to_owned(),
            origin_workspace_ids: vec![origin_workspace_id.to_owned()],
            trust_lane: MeshTrustLane::PeerAgent,
            import_trust_class: TrustClass::AgentValidated,
            allowed_lanes: MeshLaneGrants {
                metadata: Some(MeshLaneDecision::Allow),
                body: Some(MeshLaneDecision::Deny),
                embedding: Some(MeshLaneDecision::Deny),
                graph_link: Some(MeshLaneDecision::Deny),
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

    fn lane_state(peer_id: &str, decision: MeshLaneDecision) -> StoredMeshLaneGrantState {
        let target_adapter = crate::db::MeshLaneGrantTargetAdapter::new(
            peer_id,
            format!("node_{}", peer_id.trim_start_matches("peer_")),
        );
        StoredMeshLaneGrantState {
            workspace_id: "wsp_local_alpha".to_owned(),
            peer_id: peer_id.to_owned(),
            target_adapter_json: target_adapter
                .canonical_json()
                .expect("test target adapter is canonical"),
            target_adapter,
            target_matches_current_peer: true,
            grant_generation: 1,
            metadata_override: Some(decision),
            body_override: None,
            embedding_override: None,
            graph_link_override: None,
            revision_notice_override: None,
            curation_signal_override: None,
            metadata_approval_config_digest: None,
            body_approval_config_digest: None,
            embedding_approval_config_digest: None,
            graph_link_approval_config_digest: None,
            revision_notice_approval_config_digest: None,
            curation_signal_approval_config_digest: None,
            updated_at: "2026-08-04T00:00:00Z".to_owned(),
        }
    }

    fn inbound_input(peer_id: &'static str) -> MeshPeerPolicyDecisionInput<'static> {
        MeshPeerPolicyDecisionInput {
            local_workspace_id: "wsp_local_alpha",
            origin_workspace_id: "wsp_remote_beta",
            producer_peer_id: peer_id,
            material_lane: MeshLane::Metadata,
            event_validity: MeshEventValidity::Valid,
            requested_body_bytes: None,
            body_fetch_consent: false,
        }
    }

    fn outbound_input(peer_id: &'static str) -> MeshOutboundPolicyDecisionInput<'static> {
        MeshOutboundPolicyDecisionInput {
            local_workspace_id: "wsp_local_alpha",
            origin_workspace_id: "wsp_remote_beta",
            target_peer_id: peer_id,
            material_lane: MeshLane::Metadata,
            payload_is_redacted: false,
        }
    }

    #[test]
    fn mesh_policy_registry_selects_exact_inbound_and_outbound_policy() {
        let registry = MeshPeerPolicyRegistry::new([
            policy("pol_other", "peer_other", "wsp_remote_beta"),
            policy("pol_selected", "peer_builder_one", "wsp_remote_beta"),
        ]);

        let inbound = registry
            .decide_inbound_checked(&inbound_input("peer_builder_one"))
            .expect("unique inbound policy should authorize through decision engine");
        assert_eq!(
            inbound.import.workspace_scope_decision,
            MeshImportDecisionKind::Allow
        );
        assert_eq!(inbound.policy_id.as_deref(), Some("pol_selected"));
        assert_eq!(inbound.import_trust_class, Some(TrustClass::AgentValidated));
        assert_eq!(inbound.to_json()["failure"], JsonValue::Null);

        let outbound = registry
            .decide_outbound_checked(&outbound_input("peer_builder_one"))
            .expect("unique outbound policy should authorize through decision engine");
        assert_eq!(outbound.action, MeshImportDecisionKind::Allow);
        assert_eq!(outbound.policy_id.as_deref(), Some("pol_selected"));
        assert!(outbound.permits_payload_export());
        assert!(outbound.permits_raw_payload_export());
    }

    #[test]
    fn mesh_policy_registry_applies_exact_peer_override_in_both_directions() {
        let base_builder = policy("pol_builder", "peer_builder_one", "wsp_remote_beta");
        let base_other = policy("pol_other", "peer_other", "wsp_remote_beta");
        let registry = MeshPeerPolicyRegistry::new([base_builder.clone(), base_other])
            .with_lane_grant_states([lane_state("peer_builder_one", MeshLaneDecision::Deny)]);

        let inbound = registry.decide_inbound(&inbound_input("peer_builder_one"));
        assert_eq!(
            inbound.import.workspace_scope_decision,
            MeshImportDecisionKind::Deny
        );
        assert_eq!(inbound.import.reason, "peer_policy_lane_denied");

        let outbound = registry.decide_outbound(&outbound_input("peer_builder_one"));
        assert_eq!(outbound.action, MeshImportDecisionKind::Deny);
        assert_eq!(outbound.reason, "outbound_lane_denied");

        let unaffected = registry.decide_outbound(&outbound_input("peer_other"));
        assert_eq!(unaffected.action, MeshImportDecisionKind::Allow);
        assert_eq!(
            registry.policies()[0].allowed_lanes.metadata,
            base_builder.allowed_lanes.metadata,
            "effective override must not mutate the shared configured policy"
        );
    }

    #[test]
    fn widened_lane_requires_matching_config_binding_in_both_directions() {
        let mut unbound = lane_state("peer_builder_one", MeshLaneDecision::Allow);
        let policy = policy("pol_builder", "peer_builder_one", "wsp_remote_beta");

        let missing =
            MeshPeerPolicyRegistry::new([policy.clone()]).with_lane_grant_states([unbound.clone()]);
        assert_eq!(
            missing
                .decide_inbound(&inbound_input("peer_builder_one"))
                .import
                .workspace_scope_decision,
            MeshImportDecisionKind::Deny
        );
        assert_eq!(
            missing
                .decide_outbound(&outbound_input("peer_builder_one"))
                .action,
            MeshImportDecisionKind::Deny
        );

        let approved_bytes = b"[mesh]\nenabled = true\n";
        unbound.metadata_approval_config_digest = Some(
            crate::mesh::lane_grant::approval_config_digest(approved_bytes),
        );
        let mismatched = MeshPeerPolicyRegistry::new([policy.clone()])
            .with_approval_config_snapshot(b"[mesh]\nenabled = false\n")
            .with_lane_grant_states([unbound.clone()]);
        assert_eq!(
            mismatched
                .decide_inbound(&inbound_input("peer_builder_one"))
                .import
                .workspace_scope_decision,
            MeshImportDecisionKind::Deny
        );
        assert_eq!(
            mismatched
                .decide_outbound(&outbound_input("peer_builder_one"))
                .action,
            MeshImportDecisionKind::Deny
        );

        let matching = MeshPeerPolicyRegistry::new([policy])
            .with_approval_config_snapshot(approved_bytes)
            .with_lane_grant_states([unbound]);
        assert_eq!(
            matching
                .decide_inbound(&inbound_input("peer_builder_one"))
                .import
                .workspace_scope_decision,
            MeshImportDecisionKind::Allow
        );
        assert_eq!(
            matching
                .decide_outbound(&outbound_input("peer_builder_one"))
                .action,
            MeshImportDecisionKind::Allow
        );
    }

    #[test]
    fn config_bindings_are_per_lane_and_denies_survive_digest_drift() {
        let config_a = b"config-a";
        let config_b = b"config-b";
        let mut state = lane_state("peer_builder_one", MeshLaneDecision::Allow);
        state.metadata_approval_config_digest =
            Some(crate::mesh::lane_grant::approval_config_digest(config_a));
        state.body_override = Some(MeshLaneDecision::Allow);
        state.body_approval_config_digest =
            Some(crate::mesh::lane_grant::approval_config_digest(config_b));
        state.graph_link_override = Some(MeshLaneDecision::Deny);

        let under_a = MeshPeerPolicyRegistry::new([policy(
            "pol_builder",
            "peer_builder_one",
            "wsp_remote_beta",
        )])
        .with_approval_config_snapshot(config_a)
        .with_lane_grant_states([state.clone()]);
        assert_eq!(
            under_a.lane_override_for("wsp_local_alpha", "peer_builder_one", MeshLane::Metadata,),
            Some(MeshLaneDecision::Allow)
        );
        assert_eq!(
            under_a.lane_override_for("wsp_local_alpha", "peer_builder_one", MeshLane::Body,),
            Some(MeshLaneDecision::Deny)
        );

        let under_b = MeshPeerPolicyRegistry::new([policy(
            "pol_builder",
            "peer_builder_one",
            "wsp_remote_beta",
        )])
        .with_approval_config_snapshot(config_b)
        .with_lane_grant_states([state]);
        assert_eq!(
            under_b.lane_override_for("wsp_local_alpha", "peer_builder_one", MeshLane::Metadata,),
            Some(MeshLaneDecision::Deny)
        );
        assert_eq!(
            under_b.lane_override_for("wsp_local_alpha", "peer_builder_one", MeshLane::Body,),
            Some(MeshLaneDecision::Allow)
        );
        assert_eq!(
            under_b.lane_override_for("wsp_local_alpha", "peer_builder_one", MeshLane::GraphLink,),
            Some(MeshLaneDecision::Deny)
        );
    }

    #[test]
    fn mesh_policy_registry_ignores_stale_target_and_projects_membership_override() {
        let mut current = lane_state("peer_builder_one", MeshLaneDecision::Deny);
        current.target_matches_current_peer = false;
        let stale_registry = MeshPeerPolicyRegistry::new([policy(
            "pol_builder",
            "peer_builder_one",
            "wsp_remote_beta",
        )])
        .with_lane_grant_states([current]);
        assert_eq!(
            stale_registry
                .decide_outbound(&outbound_input("peer_builder_one"))
                .action,
            MeshImportDecisionKind::Allow
        );

        let registry = MeshPeerPolicyRegistry::new([policy(
            "pol_builder",
            "peer_builder_one",
            "wsp_remote_beta",
        )])
        .with_lane_grant_states([lane_state("peer_builder_one", MeshLaneDecision::Deny)]);
        let membership_input = MeshImportDecisionInput {
            local_workspace_id: "wsp_local_alpha",
            origin_workspace_id: "wsp_remote_beta",
            producer_peer_id: "peer_builder_one",
            material_lane: MeshLane::Metadata,
            event_validity: MeshEventValidity::Valid,
        };
        let projected = registry
            .inbound_membership_override(&membership_input)
            .expect("current exact state should project an override");
        assert!(projected.matches(&membership_input));
        assert_eq!(projected.decision, MeshLaneDecision::Deny);
    }

    #[test]
    fn mesh_policy_registry_fails_closed_when_policy_is_missing() {
        let registry = MeshPeerPolicyRegistry::new([policy(
            "pol_selected",
            "peer_builder_one",
            "wsp_remote_beta",
        )]);

        let error = registry
            .select_inbound_policy(&inbound_input("peer_unknown"))
            .expect_err("unknown peer should not match a policy");
        assert_eq!(error.code(), "mesh_peer_policy_lookup_missing");
        assert_eq!(error.failure().reason, "missing_peer_policy");
        assert_eq!(error.to_json()["peerRef"], "peer_unknown");

        let decision = registry.decide_inbound(&inbound_input("peer_unknown"));
        assert_eq!(
            decision.import.workspace_scope_decision,
            MeshImportDecisionKind::Deny
        );
        assert_eq!(decision.import.reason, "missing_peer_policy");
        assert!(!decision.import.permits_local_truth_side_effects());
        assert_eq!(
            decision
                .failure_surface()
                .expect("missing policy should emit structured failure")
                .code,
            "mesh_peer_policy_denied"
        );
    }

    #[test]
    fn mesh_policy_registry_rejects_ambiguous_matches_without_leaking_policy_paths() {
        let sensitive_policy_id = "/Users/alice/private/api_token_policy.toml";
        let registry = MeshPeerPolicyRegistry::new([
            policy("pol_selected", "peer_builder_one", "wsp_remote_beta"),
            policy(sensitive_policy_id, "peer_builder_one", "wsp_remote_beta"),
        ]);

        let error = registry
            .select_outbound_policy(&outbound_input("peer_builder_one"))
            .expect_err("duplicate matching policies should fail closed");
        assert_eq!(error.code(), "mesh_peer_policy_lookup_ambiguous");
        assert_eq!(error.failure().reason, "ambiguous_peer_policy");
        assert_eq!(error.failure().matching_policy_refs.len(), 2);

        let fields = error.to_json();
        assert_eq!(fields["schema"], "ee.mesh.policy_lookup_failure.v1");
        assert_eq!(fields["direction"], "outbound");
        assert_eq!(fields["materialLane"], "metadata");
        assert!(
            fields.to_string().contains("mesh_pol_"),
            "ambiguous lookup should expose a stable policy alias: {fields}"
        );
        assert!(
            !fields.to_string().contains("/Users/alice/private"),
            "lookup failure leaked raw policy path: {fields}"
        );
        assert!(
            !fields.to_string().contains("api_token"),
            "lookup failure leaked secret-like policy token: {fields}"
        );

        let decision = registry.decide_outbound(&outbound_input("peer_builder_one"));
        assert_eq!(decision.action, MeshImportDecisionKind::Deny);
        assert_eq!(decision.reason, "missing_peer_policy");
        assert!(!decision.permits_payload_export());
    }

    #[test]
    fn mesh_policy_registry_builds_from_config_peer_policies() {
        let config = ConfigFile::parse(
            r#"
[[mesh.peer_policies]]
policy_id = "pol_from_config"
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
graph_link = "deny"
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
        .expect("mesh peer policy config should parse");
        let registry = MeshPeerPolicyRegistry::from_config(&config);

        assert_eq!(registry.len(), 1);
        let decision = registry
            .decide_inbound_checked(&inbound_input("peer_builder_one"))
            .expect("config policy should select");
        assert_eq!(decision.policy_id.as_deref(), Some("pol_from_config"));
        assert_eq!(
            decision.import.workspace_scope_decision,
            MeshImportDecisionKind::Allow
        );
    }
}
