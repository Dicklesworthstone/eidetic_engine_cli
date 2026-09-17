#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::db::{
    CreateWorkspaceInput, MeshLaneGrantTargetAdapter, ObserveMeshPeerTransportIdentityInput,
};
use crate::models::WorkspaceId;

const WORKSPACE: &str = "wsp_stable_peer_test";
const TAILNET: &str = "tailnet-device-test";
const BEFORE: &str = "2026-09-17T10:00:00Z";
const AFTER: &str = "2026-09-17T11:00:00Z";

fn candidate(key: char, stable: Option<&str>) -> AutoEnrollmentCandidate {
    AutoEnrollmentCandidate {
        node_key: format!("nodekey:{}", key.to_string().repeat(64)),
        stable_node_id: stable.map(str::to_owned),
        tailscale_ip: "100.64.0.2".to_owned(),
        magic_dns_name: Some("same-name.tailnet.test".to_owned()),
        hostname: "same-name".to_owned(),
        ee_protocol_version: "1.0".to_owned(),
        discovery_policy_decision: "service_tag_match".to_owned(),
    }
}

fn compose(
    workspace: &str,
    tailnet: &str,
    now: &str,
    rows: &[MeshPeerRow],
    candidates: &[AutoEnrollmentCandidate],
) -> Result<Vec<UpsertMeshPeerInput>, DomainError> {
    auto_enrollment_peer_upserts(
        workspace,
        tailnet,
        None,
        Some("nodekey:self"),
        now,
        rows,
        candidates,
    )
}

fn as_row(input: &UpsertMeshPeerInput) -> MeshPeerRow {
    MeshPeerRow {
        peer_id: input.peer_id.clone(),
        origin_node_id: input.origin_node_id.clone(),
        display_name: input.display_name.clone(),
        enabled: input.enabled,
        last_seen_at: input.last_seen_at.clone().unwrap(),
        policy_summary_json: input.policy_summary_json.clone(),
    }
}

fn record(input: &UpsertMeshPeerInput) -> MeshPeerRecord {
    serde_json::from_str(input.policy_summary_json.as_deref().unwrap()).unwrap()
}

fn original(stable: Option<&str>) -> UpsertMeshPeerInput {
    compose(WORKSPACE, TAILNET, BEFORE, &[], &[candidate('a', stable)])
        .unwrap()
        .remove(0)
}

fn alter(row: &mut MeshPeerRow, change: impl FnOnce(&mut MeshPeerRecord)) {
    let mut peer: MeshPeerRecord =
        serde_json::from_str(row.policy_summary_json.as_deref().unwrap()).unwrap();
    change(&mut peer);
    row.policy_summary_json = Some(serde_json::to_string(&peer).unwrap());
}

#[test]
fn stable_device_reuses_both_opaque_principals_after_node_key_rotation() {
    let first = original(Some("nDeviceStableDistinctFromPublicKey"));
    let prior = record(&first);
    let fresh = candidate('b', Some("nDeviceStableDistinctFromPublicKey"));
    assert_ne!(
        fresh.stable_node_id.as_deref(),
        Some(fresh.node_key.as_str())
    );
    let rotated = compose(WORKSPACE, TAILNET, AFTER, &[as_row(&first)], &[fresh])
        .unwrap()
        .remove(0);
    let current = record(&rotated);
    assert_eq!(rotated.peer_id, first.peer_id);
    assert_eq!(rotated.origin_node_id, first.origin_node_id);
    assert_eq!(current.enrolled_at, prior.enrolled_at);
    assert_eq!(current.key.created_at, prior.key.created_at);
    assert_eq!(current.key.generation, prior.key.generation + 1);
    assert_eq!(current.key.rotated_at.as_deref(), Some(AFTER));
    assert_eq!(
        current.endpoint.tailscale_node_key,
        candidate('b', None).node_key
    );
    assert_eq!(
        current.handshake.responder_node_key,
        current.endpoint.tailscale_node_key
    );
    assert!(!current.capabilities.may_receive.body && !current.capabilities.may_send.body);
    assert_eq!(rotated.last_seen_at.as_deref(), Some(AFTER));
}

#[test]
fn unchanged_key_preserves_the_known_anchor_and_key_history() {
    let first = original(Some("nStable"));
    let refreshed = compose(
        WORKSPACE,
        TAILNET,
        AFTER,
        &[as_row(&first)],
        &[candidate('a', None)],
    )
    .unwrap()
    .remove(0);
    assert_eq!(record(&refreshed), record(&first));
    assert_eq!(refreshed.peer_id, first.peer_id);
    assert_eq!(refreshed.origin_node_id, first.origin_node_id);
}

#[test]
fn exact_key_upgrade_binds_a_legacy_peer_before_later_rotation() {
    let legacy = original(None);
    let bound = compose(
        WORKSPACE,
        TAILNET,
        AFTER,
        &[as_row(&legacy)],
        &[candidate('a', Some("nStable"))],
    )
    .unwrap()
    .remove(0);
    assert_eq!(bound.peer_id, legacy.peer_id);
    let rotated = compose(
        WORKSPACE,
        TAILNET,
        AFTER,
        &[as_row(&bound)],
        &[candidate('b', Some("nStable"))],
    )
    .unwrap()
    .remove(0);
    assert_eq!(rotated.peer_id, legacy.peer_id);
    assert_eq!(rotated.origin_node_id, legacy.origin_node_id);
    assert_eq!(record(&rotated).key.generation, 2);
}

#[test]
fn missing_anchor_never_guesses_rotation_from_hostname_dns_or_ip() {
    let first = original(None);
    let next = compose(
        WORKSPACE,
        TAILNET,
        AFTER,
        &[as_row(&first)],
        &[candidate('b', None)],
    )
    .unwrap()
    .remove(0);
    assert_ne!(next.peer_id, first.peer_id);
    assert_ne!(next.origin_node_id, first.origin_node_id);
}

#[test]
fn stable_binding_is_scoped_to_both_tailnet_and_workspace() {
    let first = original(Some("nStable"));
    for (workspace, tailnet) in [("wsp_other", TAILNET), (WORKSPACE, "tailnet-other")] {
        let next = compose(
            workspace,
            tailnet,
            AFTER,
            &[as_row(&first)],
            &[candidate('b', Some("nStable"))],
        )
        .unwrap()
        .remove(0);
        assert_ne!(next.peer_id, first.peer_id);
        assert_ne!(next.origin_node_id, first.origin_node_id);
    }
}

#[test]
fn same_key_cannot_be_rebound_to_a_different_stable_device() {
    let first = original(Some("nOriginal"));
    let error = compose(
        WORKSPACE,
        TAILNET,
        AFTER,
        &[as_row(&first)],
        &[candidate('a', Some("nSubstitute"))],
    )
    .unwrap_err();
    assert!(error.message().contains("conflicts"));
}

#[test]
fn duplicate_stable_bindings_fail_independently_of_row_order() {
    let one = as_row(&original(Some("nStable")));
    let two = as_row(
        &compose(
            WORKSPACE,
            TAILNET,
            BEFORE,
            &[],
            &[candidate('b', Some("nStable"))],
        )
        .unwrap()
        .remove(0),
    );
    for rows in [vec![one.clone(), two.clone()], vec![two, one]] {
        let error = compose(
            WORKSPACE,
            TAILNET,
            AFTER,
            &rows,
            &[candidate('c', Some("nStable"))],
        )
        .unwrap_err();
        assert!(error.message().contains("ambiguous durable principals"));
    }
}

#[test]
fn disabled_or_revoked_peers_cannot_evade_revocation_by_rotating_keys() {
    let original = as_row(&original(Some("nStable")));
    for state in 0..4 {
        let mut row = original.clone();
        match state {
            0 => row.enabled = false,
            1 => alter(&mut row, |record| record.state = MeshPeerState::Revoked),
            2 => alter(&mut row, |record| {
                record.revoked_at = Some(BEFORE.to_owned())
            }),
            _ => alter(&mut row, |record| {
                record.key.revoked_at = Some(BEFORE.to_owned())
            }),
        }
        assert!(
            compose(
                WORKSPACE,
                TAILNET,
                AFTER,
                &[row],
                &[candidate('b', Some("nStable"))]
            )
            .is_err()
        );
    }
}

#[test]
fn duplicate_candidates_cannot_overwrite_one_principal_in_a_batch() {
    assert!(
        compose(
            WORKSPACE,
            TAILNET,
            AFTER,
            &[],
            &[
                candidate('a', Some("nStable")),
                candidate('b', Some("nStable"))
            ]
        )
        .is_err()
    );
    assert!(
        compose(
            WORKSPACE,
            TAILNET,
            AFTER,
            &[],
            &[candidate('a', None), candidate('a', None)]
        )
        .is_err()
    );
}

#[test]
fn stable_and_legacy_key_matches_cannot_reuse_one_row_twice() {
    let first = original(Some("nStable"));
    assert!(
        compose(
            WORKSPACE,
            TAILNET,
            AFTER,
            &[as_row(&first)],
            &[candidate('a', None), candidate('b', Some("nStable"))]
        )
        .is_err()
    );
}

#[test]
fn invalid_identity_and_malformed_policy_do_not_silently_mint_new_peers() {
    for value in ["", " nStable", "nStable\n"] {
        assert!(
            compose(
                WORKSPACE,
                TAILNET,
                AFTER,
                &[],
                &[candidate('a', Some(value))]
            )
            .is_err()
        );
    }
    let mut row = as_row(&original(Some("nStable")));
    row.policy_summary_json = Some("{".to_owned());
    assert!(
        compose(
            WORKSPACE,
            TAILNET,
            AFTER,
            &[row],
            &[candidate('b', Some("nStable"))]
        )
        .is_err()
    );
}

#[test]
fn key_generation_exhaustion_fails_without_saturating_or_resetting_history() {
    let mut row = as_row(&original(Some("nStable")));
    alter(&mut row, |record| record.key.generation = u32::MAX);
    assert!(
        compose(
            WORKSPACE,
            TAILNET,
            AFTER,
            &[row],
            &[candidate('b', Some("nStable"))]
        )
        .unwrap_err()
        .message()
        .contains("exhausted")
    );
}

#[test]
fn existing_peer_candidates_keep_the_persisted_stable_identity() {
    let existing = ExistingAutoEnrollmentPeer {
        peer_id: "peer_opaque".to_owned(),
        node_key: candidate('a', None).node_key,
        stable_node_id: Some("nStable".to_owned()),
        tailnet_id: Some(TAILNET.to_owned()),
        tailnet_display_name: None,
        materialized_on_node_key: None,
        hostname: "same-name".to_owned(),
        tailscale_ip: "100.64.0.2".to_owned(),
        magic_dns_name: None,
        ee_protocol_version: "1.0".to_owned(),
        enrollment_source: "explicit_human_consent".to_owned(),
        enabled: true,
    };
    assert_eq!(existing.candidate().stable_node_id, existing.stable_node_id);
}

#[test]
fn real_store_rotation_keeps_one_peer_and_invalidates_prior_transport_authority() {
    let root = tempfile::tempdir().unwrap();
    let connection = DbConnection::open_file(&root.path().join("mesh.db")).unwrap();
    connection.migrate().unwrap();
    let workspace = WorkspaceId::from_uuid(uuid::Uuid::from_bytes([71; 16])).to_string();
    connection
        .insert_workspace(
            &workspace,
            &CreateWorkspaceInput {
                path: root.path().to_string_lossy().into_owned(),
                name: None,
            },
        )
        .unwrap();
    let first = compose(
        &workspace,
        TAILNET,
        BEFORE,
        &[],
        &[candidate('a', Some("nStable"))],
    )
    .unwrap()
    .remove(0);
    connection.upsert_mesh_peer(&first).unwrap();
    let bound = connection
        .observe_mesh_peer_transport_identity(&ObserveMeshPeerTransportIdentityInput {
            workspace_id: workspace.clone(),
            peer_id: first.peer_id.clone(),
            tailnet_id: TAILNET.to_owned(),
            stable_node_id: "nStable".to_owned(),
            current_node_pubkey: candidate('a', None).node_key,
            observed_at: Some(BEFORE.to_owned()),
        })
        .unwrap();
    assert!(bound.transport_identity.is_some());
    let rotated = compose(
        &workspace,
        TAILNET,
        AFTER,
        &[MeshPeerRow::from(&bound)],
        &[candidate('b', Some("nStable"))],
    )
    .unwrap()
    .remove(0);
    let saved = connection.upsert_mesh_peer(&rotated).unwrap();
    assert_eq!(connection.list_mesh_peers(&workspace).unwrap().len(), 1);
    assert_eq!(saved.peer_id, bound.peer_id);
    assert_eq!(saved.origin_node_id, bound.origin_node_id);
    assert!(
        saved.transport_identity.is_none(),
        "endpoint refresh is NOT a LocalAPI authorization observation"
    );
    let adapter =
        MeshLaneGrantTargetAdapter::new(saved.peer_id.clone(), saved.origin_node_id.clone());
    assert_eq!(adapter.peer_id, first.peer_id);
    let persisted: MeshPeerRecord =
        serde_json::from_str(saved.policy_summary_json.as_deref().unwrap()).unwrap();
    assert_eq!(persisted.key.generation, 2);
    assert_eq!(
        persisted.endpoint.stable_node_id.as_deref(),
        Some("nStable")
    );
    assert_eq!(persisted.enrolled_at, BEFORE);
}
