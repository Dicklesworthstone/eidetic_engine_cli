//! SRR6.24 peer enrollment, capability handshake, and key rotation contract.
//!
//! This module is deliberately pure: no network probes, no database writes, no
//! secret-key material. Callers supply an already-granted hello/capability
//! handshake and an explicit human consent bit; this module turns that into
//! stable peer records and command-shaped reports for `ee mesh peer ...`
//! surfaces. A reachable Tailscale node is never sufficient by itself.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

pub const MESH_PEER_RECORD_SCHEMA_V1: &str = "ee.mesh.peer_record.v1";
pub const MESH_PEER_COMMAND_REPORT_SCHEMA_V1: &str = "ee.mesh.peer_command_report.v1";
pub const MESH_PEER_HANDSHAKE_SCHEMA_V1: &str = "ee.mesh.peer_handshake.v1";

pub const PEER_ENROLLMENT_EXPLICIT_CONSENT_REQUIRED_CODE: &str =
    "mesh_peer_explicit_consent_required";
pub const PEER_ENROLLMENT_HANDSHAKE_DENIED_CODE: &str = "mesh_peer_handshake_denied";
pub const PEER_ENROLLMENT_NETWORK_ONLY_DENIED_CODE: &str = "mesh_peer_network_only_denied";
pub const PEER_ENROLLMENT_CAPABILITY_MISMATCH_CODE: &str =
    "mesh_peer_capability_handshake_mismatch";
pub const PEER_KEY_ROTATION_INVALID_KEY_CODE: &str = "mesh_peer_key_rotation_invalid_key";
pub const PEER_KEY_ROTATION_UNCHANGED_CODE: &str = "mesh_peer_key_rotation_unchanged";
pub const PEER_KEY_ROTATION_REVOKED_CODE: &str = "mesh_peer_key_rotation_revoked";
pub const PEER_UNKNOWN_ATTEMPT_DENIED_CODE: &str = "mesh_peer_unknown_attempt_denied";
pub const MESH_PEER_E2E_SURFACE: &str = "mesh_peer_enrollment";
pub use crate::models::TEST_EVENT_SCHEMA_V1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshPeerCapabilityProfile {
    MetadataOnly,
    BodyAllowed,
    EmbeddingsDenied,
    FullyDenied,
}

impl MeshPeerCapabilityProfile {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MetadataOnly => "metadata_only",
            Self::BodyAllowed => "body_allowed",
            Self::EmbeddingsDenied => "embeddings_denied",
            Self::FullyDenied => "fully_denied",
        }
    }

    #[must_use]
    pub const fn lane_capabilities(self) -> MeshPeerLaneCapabilities {
        match self {
            Self::MetadataOnly => MeshPeerLaneCapabilities {
                metadata: true,
                body: false,
                embedding: false,
                graph_link: false,
                revision_notice: true,
                curation_signal: false,
            },
            Self::BodyAllowed => MeshPeerLaneCapabilities {
                metadata: true,
                body: true,
                embedding: false,
                graph_link: false,
                revision_notice: true,
                curation_signal: false,
            },
            Self::EmbeddingsDenied => MeshPeerLaneCapabilities {
                metadata: true,
                body: true,
                embedding: false,
                graph_link: true,
                revision_notice: true,
                curation_signal: true,
            },
            Self::FullyDenied => MeshPeerLaneCapabilities {
                metadata: false,
                body: false,
                embedding: false,
                graph_link: false,
                revision_notice: false,
                curation_signal: false,
            },
        }
    }

    #[must_use]
    pub const fn required_material_capabilities(self) -> &'static [&'static str] {
        match self {
            Self::MetadataOnly => &["metadata"],
            Self::BodyAllowed | Self::EmbeddingsDenied => &["metadata", "body"],
            Self::FullyDenied => &[],
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshPeerLaneCapabilities {
    pub metadata: bool,
    pub body: bool,
    pub embedding: bool,
    pub graph_link: bool,
    pub revision_notice: bool,
    pub curation_signal: bool,
}

impl MeshPeerLaneCapabilities {
    #[must_use]
    pub fn allowed_lanes(self) -> Vec<&'static str> {
        let mut lanes = Vec::new();
        if self.metadata {
            lanes.push("metadata");
        }
        if self.body {
            lanes.push("body");
        }
        if self.embedding {
            lanes.push("embedding");
        }
        if self.graph_link {
            lanes.push("graphLink");
        }
        if self.revision_notice {
            lanes.push("revisionNotice");
        }
        if self.curation_signal {
            lanes.push("curationSignal");
        }
        lanes
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshPeerCapabilities {
    pub profile: MeshPeerCapabilityProfile,
    pub may_send: MeshPeerLaneCapabilities,
    pub may_receive: MeshPeerLaneCapabilities,
}

impl MeshPeerCapabilities {
    #[must_use]
    pub const fn from_profile(profile: MeshPeerCapabilityProfile) -> Self {
        let lanes = profile.lane_capabilities();
        Self {
            profile,
            may_send: lanes,
            may_receive: lanes,
        }
    }

    #[must_use]
    pub const fn fully_denied() -> Self {
        Self::from_profile(MeshPeerCapabilityProfile::FullyDenied)
    }

    #[must_use]
    pub fn wire_capability_names(&self) -> Vec<String> {
        let mut out: BTreeSet<String> = BTreeSet::new();
        for lane in self.may_send.allowed_lanes() {
            out.insert(format!("send:{lane}"));
        }
        for lane in self.may_receive.allowed_lanes() {
            out.insert(format!("receive:{lane}"));
        }
        out.into_iter().collect()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshPeerState {
    Active,
    Revoked,
}

impl MeshPeerState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Revoked => "revoked",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeshPeerEnrollmentScenario {
    Pair,
    Deny,
    Rotate,
    Revoke,
    UnknownPeer,
}

impl MeshPeerEnrollmentScenario {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pair => "pair",
            Self::Deny => "deny",
            Self::Rotate => "rotate",
            Self::Revoke => "revoke",
            Self::UnknownPeer => "unknown_peer",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshPeerEndpoint {
    pub tailscale_node_key: String,
    pub tailnet_id: String,
    pub tailnet_display_name: Option<String>,
    pub endpoint: String,
    pub magic_dns_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshPeerKey {
    pub generation: u32,
    pub public_key_fingerprint: String,
    pub created_at: String,
    pub rotated_at: Option<String>,
    pub revoked_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshPeerHandshake {
    pub schema: String,
    pub request_id: String,
    pub granted: bool,
    pub requester_protocol_version: String,
    pub responder_protocol_version: String,
    pub responder_node_key: String,
    pub responder_capabilities: Vec<String>,
    pub discovery_consent: bool,
}

impl MeshPeerHandshake {
    #[must_use]
    pub fn granted(
        request_id: impl Into<String>,
        protocol_version: impl Into<String>,
        responder_node_key: impl Into<String>,
        responder_capabilities: Vec<String>,
    ) -> Self {
        let protocol_version = protocol_version.into();
        Self {
            schema: MESH_PEER_HANDSHAKE_SCHEMA_V1.to_owned(),
            request_id: request_id.into(),
            granted: true,
            requester_protocol_version: protocol_version.clone(),
            responder_protocol_version: protocol_version,
            responder_node_key: responder_node_key.into(),
            responder_capabilities,
            discovery_consent: true,
        }
    }

    #[must_use]
    pub fn denied(request_id: impl Into<String>, responder_node_key: impl Into<String>) -> Self {
        Self {
            schema: MESH_PEER_HANDSHAKE_SCHEMA_V1.to_owned(),
            request_id: request_id.into(),
            granted: false,
            requester_protocol_version: "1.0".to_owned(),
            responder_protocol_version: "1.0".to_owned(),
            responder_node_key: responder_node_key.into(),
            responder_capabilities: Vec::new(),
            discovery_consent: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshPeerRecord {
    pub schema: String,
    pub peer_id: String,
    pub alias: String,
    pub workspace_id: String,
    pub endpoint: MeshPeerEndpoint,
    pub capabilities: MeshPeerCapabilities,
    pub handshake: MeshPeerHandshake,
    pub key: MeshPeerKey,
    pub state: MeshPeerState,
    pub enrolled_at: String,
    pub revoked_at: Option<String>,
    pub trust_established_by: String,
}

impl MeshPeerRecord {
    #[must_use]
    pub fn is_trusted(&self) -> bool {
        self.state == MeshPeerState::Active
            && self.handshake.granted
            && self.handshake.discovery_consent
            && self.trust_established_by == "explicit_human_consent"
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshPeerEnrollInput {
    pub workspace_id: String,
    pub alias: String,
    pub endpoint: MeshPeerEndpoint,
    pub capability_profile: MeshPeerCapabilityProfile,
    pub handshake: MeshPeerHandshake,
    pub public_key_fingerprint: String,
    pub now: String,
    pub explicit_human_consent: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshPeerRotateInput {
    pub new_public_key_fingerprint: String,
    pub rotated_at: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshPeerCommandReport {
    pub schema: &'static str,
    pub command: &'static str,
    pub success: bool,
    pub peer_id: Option<String>,
    pub peer: Option<MeshPeerRecord>,
    pub peers: Vec<MeshPeerRecord>,
    pub denied_code: Option<&'static str>,
    pub message: String,
    pub next_commands: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshPeerTestEvent {
    pub schema: &'static str,
    pub surface: &'static str,
    pub scenario: MeshPeerEnrollmentScenario,
    pub phase: &'static str,
    pub command: &'static str,
    pub success: bool,
    pub peer_id: Option<String>,
    pub denied_code: Option<&'static str>,
    pub capability_profile: Option<MeshPeerCapabilityProfile>,
    pub state: Option<MeshPeerState>,
    pub key_generation: Option<u32>,
    pub message: String,
}

impl MeshPeerCommandReport {
    fn success(
        command: &'static str,
        peer: MeshPeerRecord,
        message: impl Into<String>,
        next_commands: Vec<String>,
    ) -> Self {
        Self {
            schema: MESH_PEER_COMMAND_REPORT_SCHEMA_V1,
            command,
            success: true,
            peer_id: Some(peer.peer_id.clone()),
            peer: Some(peer),
            peers: Vec::new(),
            denied_code: None,
            message: message.into(),
            next_commands,
        }
    }

    fn denied(command: &'static str, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            schema: MESH_PEER_COMMAND_REPORT_SCHEMA_V1,
            command,
            success: false,
            peer_id: None,
            peer: None,
            peers: Vec::new(),
            denied_code: Some(code),
            message: message.into(),
            next_commands: Vec::new(),
        }
    }
}

#[must_use]
pub fn peer_command_test_event(
    scenario: MeshPeerEnrollmentScenario,
    report: &MeshPeerCommandReport,
) -> MeshPeerTestEvent {
    let peer = report.peer.as_ref();
    MeshPeerTestEvent {
        schema: TEST_EVENT_SCHEMA_V1,
        surface: MESH_PEER_E2E_SURFACE,
        scenario,
        phase: "assert",
        command: report.command,
        success: report.success,
        peer_id: report
            .peer_id
            .clone()
            .or_else(|| peer.map(|peer| peer.peer_id.clone())),
        denied_code: report.denied_code,
        capability_profile: peer.map(|peer| peer.capabilities.profile),
        state: peer.map(|peer| peer.state),
        key_generation: peer.map(|peer| peer.key.generation),
        message: report.message.clone(),
    }
}

#[must_use]
pub fn build_peer_id(workspace_id: &str, tailscale_node_key: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.mesh.peer.v1\n");
    hasher.update(workspace_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(tailscale_node_key.as_bytes());
    let digest = hasher.finalize().to_hex().to_string();
    format!("peer_{}", &digest[..24])
}

#[must_use]
pub fn build_peer_origin_node_id(tailscale_node_key: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ee.mesh.peer.origin_node.v1\n");
    hasher.update(tailscale_node_key.as_bytes());
    let digest = hasher.finalize().to_hex().to_string();
    format!("node_{}", &digest[..24])
}

#[must_use]
pub fn enroll_peer(input: MeshPeerEnrollInput) -> MeshPeerCommandReport {
    if !input.explicit_human_consent {
        return MeshPeerCommandReport::denied(
            "mesh peer add",
            PEER_ENROLLMENT_EXPLICIT_CONSENT_REQUIRED_CODE,
            "peer enrollment requires explicit human consent",
        );
    }
    if !input.handshake.granted || !input.handshake.discovery_consent {
        return MeshPeerCommandReport::denied(
            "mesh peer add",
            PEER_ENROLLMENT_HANDSHAKE_DENIED_CODE,
            "peer enrollment requires a granted capability handshake",
        );
    }
    if input.handshake.responder_node_key != input.endpoint.tailscale_node_key {
        return MeshPeerCommandReport::denied(
            "mesh peer add",
            PEER_ENROLLMENT_NETWORK_ONLY_DENIED_CODE,
            "reachable endpoint identity does not match the granted handshake",
        );
    }
    if let Some(missing_capability) =
        first_missing_material_capability(input.capability_profile, &input.handshake)
    {
        return MeshPeerCommandReport::denied(
            "mesh peer add",
            PEER_ENROLLMENT_CAPABILITY_MISMATCH_CODE,
            format!(
                "granted handshake did not advertise required capability: mesh:{missing_capability}"
            ),
        );
    }

    let peer_id = build_peer_id(&input.workspace_id, &input.endpoint.tailscale_node_key);
    let record = MeshPeerRecord {
        schema: MESH_PEER_RECORD_SCHEMA_V1.to_owned(),
        peer_id: peer_id.clone(),
        alias: input.alias,
        workspace_id: input.workspace_id,
        endpoint: input.endpoint,
        capabilities: MeshPeerCapabilities::from_profile(input.capability_profile),
        handshake: input.handshake,
        key: MeshPeerKey {
            generation: 1,
            public_key_fingerprint: input.public_key_fingerprint,
            created_at: input.now.clone(),
            rotated_at: None,
            revoked_at: None,
        },
        state: MeshPeerState::Active,
        enrolled_at: input.now,
        revoked_at: None,
        trust_established_by: "explicit_human_consent".to_owned(),
    };

    MeshPeerCommandReport::success(
        "mesh peer add",
        record,
        "peer enrolled with explicit consent and granted capability handshake",
        vec![
            format!("ee mesh peer show {peer_id} --json"),
            format!("ee mesh peer rotate {peer_id} --json"),
            format!("ee mesh peer revoke {peer_id} --json"),
        ],
    )
}

#[must_use]
pub fn first_missing_material_capability(
    profile: MeshPeerCapabilityProfile,
    handshake: &MeshPeerHandshake,
) -> Option<&'static str> {
    profile
        .required_material_capabilities()
        .iter()
        .copied()
        .find(|required| !handshake_advertises_material_capability(handshake, required))
}

#[must_use]
pub fn handshake_advertises_material_capability(
    handshake: &MeshPeerHandshake,
    material_capability: &str,
) -> bool {
    handshake.responder_capabilities.iter().any(|capability| {
        material_capability_aliases(material_capability).contains(&capability.as_str())
    })
}

fn material_capability_aliases(material_capability: &str) -> &'static [&'static str] {
    match material_capability {
        "metadata" => &[
            "metadata",
            "mesh:metadata",
            "send:metadata",
            "receive:metadata",
        ],
        "body" => &["body", "mesh:body", "send:body", "receive:body"],
        "embedding" => &[
            "embedding",
            "embeddings",
            "mesh:embedding",
            "mesh:embeddings",
            "send:embedding",
            "receive:embedding",
        ],
        _ => &[],
    }
}

#[must_use]
pub fn show_peer(peer: &MeshPeerRecord) -> MeshPeerCommandReport {
    MeshPeerCommandReport::success(
        "mesh peer show",
        peer.clone(),
        "peer details",
        vec![format!("ee mesh peer revoke {} --json", peer.peer_id)],
    )
}

#[must_use]
pub fn list_peers(peers: &[MeshPeerRecord]) -> MeshPeerCommandReport {
    let mut sorted = peers.to_vec();
    sorted.sort_by(|left, right| left.peer_id.cmp(&right.peer_id));
    MeshPeerCommandReport {
        schema: MESH_PEER_COMMAND_REPORT_SCHEMA_V1,
        command: "mesh peer list",
        success: true,
        peer_id: None,
        peer: None,
        peers: sorted,
        denied_code: None,
        message: "peer list".to_owned(),
        next_commands: vec!["ee mesh peer add --json".to_owned()],
    }
}

#[must_use]
pub fn rotate_peer_key(peer: &MeshPeerRecord, input: MeshPeerRotateInput) -> MeshPeerCommandReport {
    if peer.state == MeshPeerState::Revoked {
        return MeshPeerCommandReport::denied(
            "mesh peer rotate",
            PEER_KEY_ROTATION_REVOKED_CODE,
            "revoked peers cannot rotate keys",
        );
    }
    let new_public_key_fingerprint = input.new_public_key_fingerprint.trim();
    if new_public_key_fingerprint.is_empty() {
        return MeshPeerCommandReport::denied(
            "mesh peer rotate",
            PEER_KEY_ROTATION_INVALID_KEY_CODE,
            "peer key rotation requires a non-empty public key fingerprint",
        );
    }
    if new_public_key_fingerprint == peer.key.public_key_fingerprint {
        return MeshPeerCommandReport::denied(
            "mesh peer rotate",
            PEER_KEY_ROTATION_UNCHANGED_CODE,
            "peer key rotation requires a new public key fingerprint",
        );
    }
    let mut rotated = peer.clone();
    rotated.key.generation = rotated.key.generation.saturating_add(1);
    rotated.key.public_key_fingerprint = new_public_key_fingerprint.to_owned();
    rotated.key.rotated_at = Some(input.rotated_at);
    MeshPeerCommandReport::success(
        "mesh peer rotate",
        rotated,
        format!("peer key rotated: {}", input.reason),
        Vec::new(),
    )
}

#[must_use]
pub fn revoke_peer(peer: &MeshPeerRecord, revoked_at: impl Into<String>) -> MeshPeerCommandReport {
    let mut revoked = peer.clone();
    let revoked_at = revoked_at.into();
    revoked.state = MeshPeerState::Revoked;
    revoked.revoked_at = Some(revoked_at.clone());
    revoked.key.revoked_at = Some(revoked_at);
    revoked.capabilities = MeshPeerCapabilities::fully_denied();
    MeshPeerCommandReport::success(
        "mesh peer revoke",
        revoked,
        "peer revoked and all capabilities denied",
        Vec::new(),
    )
}

#[must_use]
pub fn unknown_peer_attempt_report(
    known_peers: &[MeshPeerRecord],
    workspace_id: &str,
    tailscale_node_key: &str,
) -> MeshPeerCommandReport {
    let peer_id = build_peer_id(workspace_id, tailscale_node_key);
    let known_active = known_peers
        .iter()
        .any(|peer| peer.peer_id == peer_id && peer.is_trusted());
    if known_active {
        return MeshPeerCommandReport {
            schema: MESH_PEER_COMMAND_REPORT_SCHEMA_V1,
            command: "mesh peer unknown-attempt",
            success: true,
            peer_id: Some(peer_id),
            peer: None,
            peers: Vec::new(),
            denied_code: None,
            message: "known trusted peer".to_owned(),
            next_commands: Vec::new(),
        };
    }
    MeshPeerCommandReport::denied(
        "mesh peer unknown-attempt",
        PEER_UNKNOWN_ATTEMPT_DENIED_CODE,
        "network reachability is not trust; enroll the peer explicitly first",
    )
}

#[cfg(test)]
mod tests {
    use super::{
        MeshPeerCapabilityProfile, MeshPeerEndpoint, MeshPeerEnrollInput,
        MeshPeerEnrollmentScenario, MeshPeerHandshake, MeshPeerRotateInput, MeshPeerState,
        PEER_ENROLLMENT_CAPABILITY_MISMATCH_CODE, PEER_UNKNOWN_ATTEMPT_DENIED_CODE, enroll_peer,
        first_missing_material_capability, handshake_advertises_material_capability,
        peer_command_test_event, revoke_peer, rotate_peer_key, unknown_peer_attempt_report,
    };

    const WORKSPACE_ID: &str = "wsp_peer_capability_contract";
    const NODE_KEY: &str = "nodekey:peer-capability";
    const NOW: &str = "2026-05-20T00:05:00Z";

    fn endpoint() -> MeshPeerEndpoint {
        MeshPeerEndpoint {
            tailscale_node_key: NODE_KEY.to_owned(),
            tailnet_id: "tn_peer_capability".to_owned(),
            tailnet_display_name: Some("capability-tailnet".to_owned()),
            endpoint: "100.64.20.2:4747".to_owned(),
            magic_dns_name: Some("peer-capability.tailnet.ts.net".to_owned()),
        }
    }

    fn handshake(capabilities: &[&str]) -> MeshPeerHandshake {
        MeshPeerHandshake::granted(
            "hello_req_capability",
            "1.0",
            NODE_KEY,
            capabilities
                .iter()
                .map(|capability| (*capability).to_owned())
                .collect(),
        )
    }

    fn enroll_with_capabilities(
        profile: MeshPeerCapabilityProfile,
        capabilities: &[&str],
    ) -> super::MeshPeerCommandReport {
        enroll_peer(MeshPeerEnrollInput {
            workspace_id: WORKSPACE_ID.to_owned(),
            alias: "peer-capability".to_owned(),
            endpoint: endpoint(),
            capability_profile: profile,
            handshake: handshake(capabilities),
            public_key_fingerprint: "blake3:pubkey-capability".to_owned(),
            now: NOW.to_owned(),
            explicit_human_consent: true,
        })
    }

    #[test]
    fn enrollment_requires_handshake_capabilities_for_requested_profile() {
        let metadata =
            enroll_with_capabilities(MeshPeerCapabilityProfile::MetadataOnly, &["mesh:metadata"]);
        assert!(metadata.success);

        let body = enroll_with_capabilities(
            MeshPeerCapabilityProfile::BodyAllowed,
            &["mesh:metadata", "mesh:body"],
        );
        assert!(body.success);

        let missing_body =
            enroll_with_capabilities(MeshPeerCapabilityProfile::BodyAllowed, &["mesh:metadata"]);
        assert!(!missing_body.success);
        assert_eq!(
            missing_body.denied_code,
            Some(PEER_ENROLLMENT_CAPABILITY_MISMATCH_CODE)
        );
    }

    #[test]
    fn capability_contract_accepts_stable_wire_aliases() {
        let handshake = handshake(&["send:metadata", "receive:body"]);
        assert!(handshake_advertises_material_capability(
            &handshake, "metadata"
        ));
        assert!(handshake_advertises_material_capability(&handshake, "body"));
        assert_eq!(
            first_missing_material_capability(MeshPeerCapabilityProfile::BodyAllowed, &handshake),
            None
        );
    }

    #[test]
    fn fully_denied_profile_needs_no_material_lanes_but_still_records_handshake() {
        let report = enroll_with_capabilities(MeshPeerCapabilityProfile::FullyDenied, &[]);
        let peer = report.peer.expect("fully denied enrollment record");
        assert!(report.success);
        assert!(peer.capabilities.wire_capability_names().is_empty());
        assert!(peer.handshake.responder_capabilities.is_empty());
    }

    #[test]
    fn command_reports_emit_structured_peer_enrollment_events() {
        let pair_report = enroll_with_capabilities(
            MeshPeerCapabilityProfile::BodyAllowed,
            &["metadata", "body"],
        );
        let pair_event = peer_command_test_event(MeshPeerEnrollmentScenario::Pair, &pair_report);
        let peer = pair_report.peer.expect("paired peer");
        let rotate_report = rotate_peer_key(
            &peer,
            MeshPeerRotateInput {
                new_public_key_fingerprint: "blake3:rotated-capability".to_owned(),
                rotated_at: "2026-05-20T00:10:00Z".to_owned(),
                reason: "operator requested rotation".to_owned(),
            },
        );
        let rotate_event =
            peer_command_test_event(MeshPeerEnrollmentScenario::Rotate, &rotate_report);
        let rotated_peer = rotate_report.peer.expect("rotated peer");
        let revoke_report = revoke_peer(&rotated_peer, "2026-05-20T00:15:00Z");
        let revoke_event =
            peer_command_test_event(MeshPeerEnrollmentScenario::Revoke, &revoke_report);
        let deny_report =
            enroll_with_capabilities(MeshPeerCapabilityProfile::BodyAllowed, &["metadata"]);
        let deny_event = peer_command_test_event(MeshPeerEnrollmentScenario::Deny, &deny_report);
        let unknown_report = unknown_peer_attempt_report(&[], WORKSPACE_ID, NODE_KEY);
        let unknown_event =
            peer_command_test_event(MeshPeerEnrollmentScenario::UnknownPeer, &unknown_report);

        assert_eq!(pair_event.schema, super::TEST_EVENT_SCHEMA_V1);
        assert_eq!(pair_event.surface, super::MESH_PEER_E2E_SURFACE);
        assert_eq!(pair_event.scenario.as_str(), "pair");
        assert!(pair_event.success);
        assert_eq!(
            pair_event.capability_profile,
            Some(MeshPeerCapabilityProfile::BodyAllowed)
        );
        assert_eq!(rotate_event.key_generation, Some(2));
        assert_eq!(revoke_event.state, Some(MeshPeerState::Revoked));
        assert_eq!(
            deny_event.denied_code,
            Some(PEER_ENROLLMENT_CAPABILITY_MISMATCH_CODE)
        );
        assert_eq!(
            unknown_event.denied_code,
            Some(PEER_UNKNOWN_ATTEMPT_DENIED_CODE)
        );

        let json = serde_json::to_string(&[
            pair_event,
            rotate_event,
            revoke_event,
            deny_event,
            unknown_event,
        ])
        .expect("serialize peer test events");
        assert!(json.contains("\"schema\":\"ee.test_event.v1\""));
        assert!(json.contains("\"scenario\":\"unknown_peer\""));
        assert!(!json.contains("private_key"));
    }

    #[test]
    fn key_rotation_fails_closed_for_empty_or_unchanged_public_key() {
        let peer =
            enroll_with_capabilities(MeshPeerCapabilityProfile::MetadataOnly, &["mesh:metadata"])
                .peer
                .expect("paired peer");

        let empty = rotate_peer_key(
            &peer,
            MeshPeerRotateInput {
                new_public_key_fingerprint: "  ".to_owned(),
                rotated_at: "2026-05-20T00:20:00Z".to_owned(),
                reason: "operator pasted an empty key".to_owned(),
            },
        );
        assert!(!empty.success);
        assert_eq!(
            empty.denied_code,
            Some(super::PEER_KEY_ROTATION_INVALID_KEY_CODE)
        );
        assert!(empty.peer.is_none());

        let unchanged = rotate_peer_key(
            &peer,
            MeshPeerRotateInput {
                new_public_key_fingerprint: peer.key.public_key_fingerprint.clone(),
                rotated_at: "2026-05-20T00:25:00Z".to_owned(),
                reason: "operator retried the active key".to_owned(),
            },
        );
        assert!(!unchanged.success);
        assert_eq!(
            unchanged.denied_code,
            Some(super::PEER_KEY_ROTATION_UNCHANGED_CODE)
        );
        assert!(unchanged.peer.is_none());
    }
}
