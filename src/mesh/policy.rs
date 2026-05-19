//! SRR6.5 mesh peer policy selection and authorization facade.
//!
//! The low-level policy decisions live in `core::memory_scope`; this module
//! owns the mesh-facing registry that selects exactly one policy before callers
//! evaluate inbound imports or outbound exports. Missing and ambiguous matches
//! are fail-closed so a peer can never gain access because a policy record is
//! absent, duplicated, or too broad.

use serde_json::{Value as JsonValue, json};

use crate::config::{ConfigFile, MeshLane};

pub use crate::core::memory_scope::{
    MeshBodyFetchPolicy, MeshEventValidity, MeshImportDecisionKind, MeshOutboundPolicyDecision,
    MeshOutboundPolicyDecisionInput, MeshPeerPolicy, MeshPeerPolicyDecision,
    MeshPeerPolicyDecisionInput, MeshPolicyFailureSurface, MeshRedactionDecision,
    MeshRedactionPolicy, MeshTrustLane, decide_mesh_outbound_policy, decide_mesh_peer_policy,
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
}

impl MeshPeerPolicyRegistry {
    #[must_use]
    pub fn new(policies: impl IntoIterator<Item = MeshPeerPolicy>) -> Self {
        Self {
            policies: policies.into_iter().collect(),
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

    #[must_use]
    pub fn policies(&self) -> &[MeshPeerPolicy] {
        &self.policies
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
        decide_mesh_peer_policy(input, self.select_inbound_policy(input).ok())
    }

    /// Inbound authorization with a structured lookup error for diagnostics.
    pub fn decide_inbound_checked(
        &self,
        input: &MeshPeerPolicyDecisionInput<'_>,
    ) -> Result<MeshPeerPolicyDecision, MeshPeerPolicyLookupError> {
        let policy = self.select_inbound_policy(input)?;
        Ok(decide_mesh_peer_policy(input, Some(policy)))
    }

    /// Fail-closed outbound authorization. Missing or ambiguous policy lookup is
    /// treated the same as no policy: the returned decision denies the export.
    #[must_use]
    pub fn decide_outbound(
        &self,
        input: &MeshOutboundPolicyDecisionInput<'_>,
    ) -> MeshOutboundPolicyDecision {
        decide_mesh_outbound_policy(input, self.select_outbound_policy(input).ok())
    }

    /// Outbound authorization with a structured lookup error for diagnostics.
    pub fn decide_outbound_checked(
        &self,
        input: &MeshOutboundPolicyDecisionInput<'_>,
    ) -> Result<MeshOutboundPolicyDecision, MeshPeerPolicyLookupError> {
        let policy = self.select_outbound_policy(input)?;
        Ok(decide_mesh_outbound_policy(input, Some(policy)))
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
        0 => Err(MeshPeerPolicyLookupError::Missing(Box::new(lookup_failure(
            direction,
            "missing_peer_policy",
            local_workspace_id,
            peer_id,
            origin_workspace_id,
            material_lane,
            Vec::new(),
        )))),
        _ => {
            let policy_refs = matches
                .iter()
                .map(|policy| mesh_policy_ref(&policy.policy_id))
                .collect();
            Err(MeshPeerPolicyLookupError::Ambiguous(Box::new(lookup_failure(
                direction,
                "ambiguous_peer_policy",
                local_workspace_id,
                peer_id,
                origin_workspace_id,
                material_lane,
                policy_refs,
            ))))
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
    let normalized = label.to_ascii_lowercase();
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
    .any(|marker| normalized.contains(marker))
}

fn stable_mesh_alias(prefix: &str, value: &str) -> String {
    let hash = blake3::hash(value.as_bytes()).to_hex();
    format!("{prefix}_{}", &hash[..10])
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
