#![allow(clippy::expect_used)]

use ee::mesh::peer;
use ee::mesh::peer::{
    MeshPeerCapabilityProfile, MeshPeerEndpoint, MeshPeerEnrollInput, MeshPeerHandshake,
    MeshPeerRotateInput, MeshPeerState, PEER_ENROLLMENT_EXPLICIT_CONSENT_REQUIRED_CODE,
    PEER_ENROLLMENT_HANDSHAKE_DENIED_CODE, PEER_ENROLLMENT_NETWORK_ONLY_DENIED_CODE,
    PEER_KEY_ROTATION_REVOKED_CODE, PEER_UNKNOWN_ATTEMPT_DENIED_CODE, build_peer_id, enroll_peer,
    list_peers, revoke_peer, rotate_peer_key, show_peer, unknown_peer_attempt_report,
};

const WORKSPACE_ID: &str = "wsp_peer_enroll_00000000000001";
const NODE_KEY: &str = "nodekey:peer-alpha";
const NOW: &str = "2026-05-19T23:40:00Z";

fn endpoint(node_key: &str) -> MeshPeerEndpoint {
    MeshPeerEndpoint {
        tailscale_node_key: node_key.to_owned(),
        tailnet_id: "tn_peer_enroll_001".to_owned(),
        tailnet_display_name: Some("team-tailnet".to_owned()),
        endpoint: "100.64.10.2:4747".to_owned(),
        magic_dns_name: Some("peer-alpha.tailnet.ts.net".to_owned()),
    }
}

fn handshake(node_key: &str) -> MeshPeerHandshake {
    MeshPeerHandshake::granted(
        "hello_req_001",
        "1.0",
        node_key,
        vec!["mesh:metadata".to_owned(), "mesh:body".to_owned()],
    )
}

fn enroll(profile: MeshPeerCapabilityProfile) -> peer::MeshPeerCommandReport {
    enroll_peer(MeshPeerEnrollInput {
        workspace_id: WORKSPACE_ID.to_owned(),
        alias: "peer-alpha".to_owned(),
        endpoint: endpoint(NODE_KEY),
        capability_profile: profile,
        handshake: handshake(NODE_KEY),
        public_key_fingerprint: "blake3:pubkey-alpha".to_owned(),
        now: NOW.to_owned(),
        explicit_human_consent: true,
    })
}

#[test]
fn enrollment_requires_explicit_human_consent() {
    let report = enroll_peer(MeshPeerEnrollInput {
        workspace_id: WORKSPACE_ID.to_owned(),
        alias: "peer-alpha".to_owned(),
        endpoint: endpoint(NODE_KEY),
        capability_profile: MeshPeerCapabilityProfile::MetadataOnly,
        handshake: handshake(NODE_KEY),
        public_key_fingerprint: "blake3:pubkey-alpha".to_owned(),
        now: NOW.to_owned(),
        explicit_human_consent: false,
    });

    assert!(!report.success);
    assert_eq!(
        report.denied_code,
        Some(PEER_ENROLLMENT_EXPLICIT_CONSENT_REQUIRED_CODE)
    );
    assert!(report.peer.is_none());
}

#[test]
fn enrollment_requires_granted_handshake() {
    let report = enroll_peer(MeshPeerEnrollInput {
        workspace_id: WORKSPACE_ID.to_owned(),
        alias: "peer-alpha".to_owned(),
        endpoint: endpoint(NODE_KEY),
        capability_profile: MeshPeerCapabilityProfile::MetadataOnly,
        handshake: MeshPeerHandshake::denied("hello_req_001", NODE_KEY),
        public_key_fingerprint: "blake3:pubkey-alpha".to_owned(),
        now: NOW.to_owned(),
        explicit_human_consent: true,
    });

    assert!(!report.success);
    assert_eq!(
        report.denied_code,
        Some(PEER_ENROLLMENT_HANDSHAKE_DENIED_CODE)
    );
}

#[test]
fn enrollment_rejects_network_reachability_without_matching_handshake_identity() {
    let report = enroll_peer(MeshPeerEnrollInput {
        workspace_id: WORKSPACE_ID.to_owned(),
        alias: "peer-alpha".to_owned(),
        endpoint: endpoint("nodekey:reachable-but-not-handshaken"),
        capability_profile: MeshPeerCapabilityProfile::MetadataOnly,
        handshake: handshake(NODE_KEY),
        public_key_fingerprint: "blake3:pubkey-alpha".to_owned(),
        now: NOW.to_owned(),
        explicit_human_consent: true,
    });

    assert!(!report.success);
    assert_eq!(
        report.denied_code,
        Some(PEER_ENROLLMENT_NETWORK_ONLY_DENIED_CODE)
    );
}

#[test]
fn capability_profiles_serialize_the_required_four_modes() {
    let metadata = enroll(MeshPeerCapabilityProfile::MetadataOnly)
        .peer
        .expect("metadata peer");
    assert!(metadata.capabilities.may_send.metadata);
    assert!(!metadata.capabilities.may_send.body);
    assert!(!metadata.capabilities.may_send.embedding);

    let body = enroll(MeshPeerCapabilityProfile::BodyAllowed)
        .peer
        .expect("body peer");
    assert!(body.capabilities.may_receive.body);
    assert!(!body.capabilities.may_receive.embedding);

    let no_embeddings = enroll(MeshPeerCapabilityProfile::EmbeddingsDenied)
        .peer
        .expect("embedding-denied peer");
    assert!(no_embeddings.capabilities.may_send.body);
    assert!(!no_embeddings.capabilities.may_send.embedding);
    assert!(no_embeddings.capabilities.may_send.graph_link);

    let denied = enroll(MeshPeerCapabilityProfile::FullyDenied)
        .peer
        .expect("denied peer");
    assert!(denied.capabilities.wire_capability_names().is_empty());

    let json = serde_json::to_string(&denied).expect("serialize peer");
    assert!(json.contains("\"profile\":\"fully_denied\""));
    assert!(json.contains("\"maySend\""));
    assert!(json.contains("\"mayReceive\""));
}

#[test]
fn add_show_list_rotate_revoke_reports_are_stable_json_shapes() {
    let add = enroll(MeshPeerCapabilityProfile::BodyAllowed);
    assert!(add.success);
    assert_eq!(add.command, "mesh peer add");
    let peer = add.peer.expect("peer");
    assert_eq!(peer.schema, peer::MESH_PEER_RECORD_SCHEMA_V1);
    assert_eq!(peer.state, MeshPeerState::Active);
    assert!(peer.is_trusted());

    let show = show_peer(&peer);
    assert_eq!(show.command, "mesh peer show");
    assert_eq!(show.peer.as_ref().expect("show peer").peer_id, peer.peer_id);

    let listed = list_peers(std::slice::from_ref(&peer));
    assert_eq!(listed.command, "mesh peer list");
    assert_eq!(listed.peers.len(), 1);

    let rotated = rotate_peer_key(
        &peer,
        MeshPeerRotateInput {
            new_public_key_fingerprint: "blake3:pubkey-alpha-rotated".to_owned(),
            rotated_at: "2026-05-19T23:45:00Z".to_owned(),
            reason: "operator requested rotation".to_owned(),
        },
    );
    let rotated_peer = rotated.peer.expect("rotated peer");
    assert_eq!(rotated_peer.key.generation, 2);
    assert_eq!(
        rotated_peer.key.public_key_fingerprint,
        "blake3:pubkey-alpha-rotated"
    );

    let revoked = revoke_peer(&rotated_peer, "2026-05-19T23:50:00Z");
    let revoked_peer = revoked.peer.expect("revoked peer");
    assert_eq!(revoked_peer.state, MeshPeerState::Revoked);
    assert!(!revoked_peer.is_trusted());
    assert!(revoked_peer.capabilities.wire_capability_names().is_empty());

    let serialized = serde_json::to_string(&revoked_peer).expect("serialize revoked peer");
    assert!(serialized.contains("\"schema\":\"ee.mesh.peer_record.v1\""));
    assert!(serialized.contains("\"state\":\"revoked\""));
    assert!(!serialized.contains("private_key"));
}

#[test]
fn revoked_peers_cannot_rotate_keys() {
    let peer = enroll(MeshPeerCapabilityProfile::MetadataOnly)
        .peer
        .expect("peer");
    let revoked = revoke_peer(&peer, "2026-05-19T23:50:00Z")
        .peer
        .expect("revoked");
    let report = rotate_peer_key(
        &revoked,
        MeshPeerRotateInput {
            new_public_key_fingerprint: "blake3:pubkey-after-revoke".to_owned(),
            rotated_at: "2026-05-19T23:55:00Z".to_owned(),
            reason: "should fail".to_owned(),
        },
    );
    assert!(!report.success);
    assert_eq!(report.denied_code, Some(PEER_KEY_ROTATION_REVOKED_CODE));
}

#[test]
fn unknown_peer_attempt_is_denied_even_when_node_is_reachable() {
    let report = unknown_peer_attempt_report(&[], WORKSPACE_ID, NODE_KEY);
    assert!(!report.success);
    assert_eq!(report.denied_code, Some(PEER_UNKNOWN_ATTEMPT_DENIED_CODE));

    let peer = enroll(MeshPeerCapabilityProfile::MetadataOnly)
        .peer
        .expect("peer");
    let known = unknown_peer_attempt_report(&[peer], WORKSPACE_ID, NODE_KEY);
    assert!(known.success);
    assert_eq!(known.peer_id, Some(build_peer_id(WORKSPACE_ID, NODE_KEY)));
}
