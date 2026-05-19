//! SRR6.24 peer enrollment, capability handshake, and key rotation contract.
//!
//! This module is deliberately pure: no network probes, no database writes, no
//! secret-key material. Callers supply an already-granted hello/capability
//! handshake and an explicit human consent bit; this module turns that into
//! stable peer records and command-shaped reports for `ee mesh peer ...`
//! surfaces. A reachable Tailscale node is never sufficient by itself.

use std::collections::BTreeSet;

use serde::Serialize;

pub const MESH_PEER_RECORD_SCHEMA_V1: &str = "ee.mesh.peer_record.v1";
pub const MESH_PEER_COMMAND_REPORT_SCHEMA_V1: &str = "ee.mesh.peer_command_report.v1";
pub const MESH_PEER_HANDSHAKE_SCHEMA_V1: &str = "ee.mesh.peer_handshake.v1";

pub const PEER_ENROLLMENT_EXPLICIT_CONSENT_REQUIRED_CODE: &str =
    "mesh_peer_explicit_consent_required";
pub const PEER_ENROLLMENT_HANDSHAKE_DENIED_CODE: &str = "mesh_peer_handshake_denied";
pub const PEER_ENROLLMENT_NETWORK_ONLY_DENIED_CODE: &str = "mesh_peer_network_only_denied";
pub const PEER_KEY_ROTATION_REVOKED_CODE: &str = "mesh_peer_key_rotation_revoked";
pub const PEER_UNKNOWN_ATTEMPT_DENIED_CODE: &str = "mesh_peer_unknown_attempt_denied";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize)]
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshPeerEndpoint {
    pub tailscale_node_key: String,
    pub tailnet_id: String,
    pub tailnet_display_name: Option<String>,
    pub endpoint: String,
    pub magic_dns_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshPeerKey {
    pub generation: u32,
    pub public_key_fingerprint: String,
    pub created_at: String,
    pub rotated_at: Option<String>,
    pub revoked_at: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshPeerHandshake {
    pub schema: &'static str,
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
            schema: MESH_PEER_HANDSHAKE_SCHEMA_V1,
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
            schema: MESH_PEER_HANDSHAKE_SCHEMA_V1,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshPeerRecord {
    pub schema: &'static str,
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

    let peer_id = build_peer_id(&input.workspace_id, &input.endpoint.tailscale_node_key);
    let record = MeshPeerRecord {
        schema: MESH_PEER_RECORD_SCHEMA_V1,
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
    let mut rotated = peer.clone();
    rotated.key.generation = rotated.key.generation.saturating_add(1);
    rotated.key.public_key_fingerprint = input.new_public_key_fingerprint;
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
