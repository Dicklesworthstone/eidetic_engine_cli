#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use std::cell::Cell;
use crate::db::{CreateWorkspaceInput, ObserveMeshPeerTransportIdentityInput, MeshLaneGrantMutationInput, MeshLaneGrantTargetAdapter};
use crate::config::{MeshLane, MeshLaneDecision};
use crate::models::WorkspaceId;

const BEFORE: &str = "2026-09-17T10:00:00Z";
const AFTER: &str = "2026-09-17T11:00:00Z";
const TAILNET: &str = "tailnet-atomic-enrollment";

fn candidate(key: char, stable: &str) -> AutoEnrollmentCandidate {
    AutoEnrollmentCandidate {
        node_key: format!("nodekey:{}", key.to_string().repeat(64)),
        stable_node_id: Some(stable.to_owned()), tailscale_ip: "100.64.0.2".to_owned(),
        magic_dns_name: None, hostname: format!("device-{key}"), ee_protocol_version: "1.0".to_owned(),
        discovery_policy_decision: "service_tag_match".to_owned(),
    }
}

fn discovery() -> TailscaleAutodiscoveryReport {
    TailscaleAutodiscoveryReport {
        schema: crate::mesh::tailscale_autodiscovery::TAILSCALE_AUTODISCOVERY_SCHEMA_V1,
        tailnet_id: Some(TAILNET.to_owned()), tailnet_display_name: None,
        self_node_key: Some("nodekey:self".to_owned()), probed_peer_count: 1, eligible_peer_count: 1,
        ee_capable_peers: Vec::new(), skipped_peers: Vec::new(), degraded: Vec::new(),
    }
}

fn fixture() -> (tempfile::TempDir, DbConnection, MeshForegroundSnapshot) {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("mesh.db");
    let connection = DbConnection::open_file(&database).unwrap();
    connection.migrate().unwrap();
    let workspace = WorkspaceId::from_uuid(uuid::Uuid::from_bytes([72; 16])).to_string();
    connection.insert_workspace(&workspace, &CreateWorkspaceInput {
        path: root.path().to_string_lossy().into_owned(), name: None,
    }).unwrap();
    let snapshot = MeshForegroundSnapshot {
        workspace_id: workspace, workspace_path: root.path().to_string_lossy().into_owned(),
        database_path: database.to_string_lossy().into_owned(), initialized: true, mesh_enabled: true,
        mode: "auto".to_owned(), storage: MeshStorageCounts::default(), peers: Vec::new(),
        cursors: Vec::new(), events: Vec::new(), degraded: Vec::new(),
    };
    (root, connection, snapshot)
}

fn snapshot(connection: &DbConnection, old: &MeshForegroundSnapshot) -> MeshForegroundSnapshot {
    let mut value = old.clone();
    value.peers = connection.list_mesh_peers(&old.workspace_id).unwrap().iter().map(MeshPeerRow::from).collect();
    value
}

fn seed(connection: &DbConnection, snapshot: &MeshForegroundSnapshot, key: char, stable: &str, manual: bool) -> UpsertMeshPeerInput {
    let mut input = auto_enrollment_peer_upserts(
        &snapshot.workspace_id, TAILNET, None, Some("nodekey:self"), BEFORE, &[], &[candidate(key, stable)],
    ).unwrap().remove(0);
    if manual {
        let mut record: MeshPeerRecord = serde_json::from_str(input.policy_summary_json.as_deref().unwrap()).unwrap();
        record.trust_established_by = "explicit_human_consent".to_owned();
        input.policy_summary_json = Some(serde_json::to_string(&record).unwrap());
    }
    connection.upsert_mesh_peer(&input).unwrap();
    input
}

fn plan(expected: &MeshForegroundSnapshot, candidates: Vec<AutoEnrollmentCandidate>, replace: bool) -> AutoEnrollmentMaterializationPlan {
    plan_auto_enrollment(AutoEnrollmentInput {
        workspace_id: expected.workspace_id.clone(), workspace_path: expected.workspace_path.clone(),
        now: AFTER.to_owned(), fresh_probe_invocations: 1, tailnet_id: Some(TAILNET.to_owned()),
        tailnet_display_name: None, self_node_key: Some("nodekey:self".to_owned()),
        discovered_peers: candidates, tailnet_peers: Vec::new(),
        existing_peers: auto_enrollment_existing_peers(expected).unwrap(),
        options: AutoEnrollmentOptions { replace_manual_with_auto: replace,
            sync_once: AutoEnrollmentSyncOnceMode::DeferredToCaller, ..AutoEnrollmentOptions::default() },
    }).materialization
}

#[test]
fn manual_migration_retains_the_refreshed_principal_and_revokes_only_unmatched_manual_peers() {
    let (_root, connection, empty) = fixture();
    let retained = seed(&connection, &empty, 'a', "stable-a", true);
    let removed = seed(&connection, &empty, 'c', "stable-c", true);
    let expected = snapshot(&connection, &empty);
    let plan = plan(&expected, vec![candidate('b', "stable-a")], true);
    assert!(plan.writes_peer_rows && plan.manual_to_auto_migration_intended);
    assert_eq!(plan.peers_to_revoke.len(), 2, "real planner includes the migrated old key");
    materialize(&connection, &expected, &discovery(), &plan, AFTER, || Ok(())).unwrap();
    let rows = connection.list_mesh_peers(&empty.workspace_id).unwrap();
    assert_eq!(rows.len(), 2);
    let active = rows.iter().find(|row| row.peer_id == retained.peer_id).unwrap();
    assert!(active.enabled, "migration must not revoke the principal just refreshed");
    assert_eq!(active.origin_node_id, retained.origin_node_id);
    let record: MeshPeerRecord = serde_json::from_str(active.policy_summary_json.as_deref().unwrap()).unwrap();
    assert_eq!(record.endpoint.tailscale_node_key, candidate('b', "stable-a").node_key);
    assert_eq!(record.trust_established_by, "tailscale_auto_enrollment");
    assert!(!rows.iter().find(|row| row.peer_id == removed.peer_id).unwrap().enabled);
}

#[test]
fn stale_discovery_cannot_resurrect_a_peer_revoked_by_another_connection() {
    let (_root, connection, empty) = fixture();
    let mut first = seed(&connection, &empty, 'a', "stable-a", false);
    let expected = snapshot(&connection, &empty);
    let plan = plan(&expected, vec![candidate('b', "stable-a")], false);
    assert!(plan.writes_peer_rows);
    let writer = DbConnection::open_file(Path::new(&empty.database_path)).unwrap();
    first.enabled = false;
    writer.upsert_mesh_peer(&first).unwrap();
    let before = connection.list_mesh_peers(&empty.workspace_id).unwrap();
    let invoked = Cell::new(false);
    let error = materialize(&connection, &expected, &discovery(), &plan, AFTER, || { invoked.set(true); Ok(()) }).unwrap_err();
    assert!(error.message().contains("changed during discovery"));
    assert!(!invoked.get(), "stale plans must not even persist configuration overrides");
    assert_eq!(connection.list_mesh_peers(&empty.workspace_id).unwrap(), before);
}

#[test]
fn competing_new_peer_or_policy_change_requires_replanning() {
    for add_peer in [false, true] {
        let (_root, connection, empty) = fixture();
        let mut first = seed(&connection, &empty, 'a', "stable-a", false);
        let expected = snapshot(&connection, &empty);
        let plan = plan(&expected, vec![candidate('b', "stable-a")], false);
        if add_peer {
            seed(&connection, &empty, 'c', "stable-c", false);
        } else {
            let mut policy: MeshPeerRecord = serde_json::from_str(first.policy_summary_json.as_deref().unwrap()).unwrap();
            policy.key.revoked_at = Some(AFTER.to_owned());
            first.policy_summary_json = Some(serde_json::to_string(&policy).unwrap());
            connection.upsert_mesh_peer(&first).unwrap();
        }
        let before = connection.list_mesh_peers(&empty.workspace_id).unwrap();
        assert!(materialize(&connection, &expected, &discovery(), &plan, AFTER, || Ok(())).is_err());
        assert_eq!(connection.list_mesh_peers(&empty.workspace_id).unwrap(), before);
    }
}

#[test]
fn fresh_observation_timestamps_do_not_spuriously_invalidate_a_plan() {
    let (_root, connection, empty) = fixture();
    let mut first = seed(&connection, &empty, 'a', "stable-a", false);
    let expected = snapshot(&connection, &empty);
    let plan = plan(&expected, vec![candidate('b', "stable-a")], false);
    first.last_seen_at = Some(AFTER.to_owned());
    connection.upsert_mesh_peer(&first).unwrap();
    materialize(&connection, &expected, &discovery(), &plan, AFTER, || Ok(())).unwrap();
    assert_eq!(connection.list_mesh_peers(&empty.workspace_id).unwrap().len(), 1);
}

#[test]
fn explicit_exclusion_of_the_old_key_cannot_be_evaded_by_stable_id_rotation() {
    let (_root, connection, empty) = fixture();
    seed(&connection, &empty, 'a', "stable-a", true);
    let expected = snapshot(&connection, &empty);
    let mut plan = plan(&expected, vec![candidate('b', "stable-a")], true);
    plan.append_denylist_node_keys.push(candidate('a', "stable-a").node_key);
    let before = connection.list_mesh_peers(&empty.workspace_id).unwrap();
    let error = materialize(&connection, &expected, &discovery(), &plan, AFTER, || panic!("exclusion must be decided before writing")).unwrap_err();
    assert!(error.message().contains("explicit revocation"));
    assert_eq!(connection.list_mesh_peers(&empty.workspace_id).unwrap(), before);
}

#[test]
fn nonmigration_upsert_and_revocation_collision_is_rejected() {
    let (_root, connection, empty) = fixture();
    seed(&connection, &empty, 'a', "stable-a", false);
    let expected = snapshot(&connection, &empty);
    let mut plan = plan(&expected, vec![candidate('b', "stable-a")], false);
    plan.peers_to_revoke.push(candidate('a', "stable-a").node_key);
    assert!(materialize(&connection, &expected, &discovery(), &plan, AFTER, || Ok(())).is_err());
    assert!(connection.list_mesh_peers(&empty.workspace_id).unwrap()[0].enabled);
}

#[test]
fn stored_policy_cannot_override_an_authoritative_transport_device_binding() {
    let (_root, connection, empty) = fixture();
    let first = seed(&connection, &empty, 'a', "policy-stable", false);
    connection.observe_mesh_peer_transport_identity(&ObserveMeshPeerTransportIdentityInput {
        workspace_id: empty.workspace_id.clone(), peer_id: first.peer_id.clone(), tailnet_id: TAILNET.to_owned(),
        stable_node_id: "authoritative-stable".to_owned(), current_node_pubkey: candidate('a', "").node_key,
        observed_at: Some(BEFORE.to_owned()),
    }).unwrap();
    let expected = snapshot(&connection, &empty);
    let plan = plan(&expected, vec![candidate('b', "policy-stable")], false);
    let before = connection.list_mesh_peers(&empty.workspace_id).unwrap();
    let error = materialize(&connection, &expected, &discovery(), &plan, AFTER, || Ok(())).unwrap_err();
    assert!(error.message().contains("authoritative transport identity"));
    assert_eq!(connection.list_mesh_peers(&empty.workspace_id).unwrap(), before);
}

#[test]
fn failure_after_a_peer_write_rolls_back_peer_rows_and_consent_generation() {
    let (_root, connection, empty) = fixture();
    let first = seed(&connection, &empty, 'a', "stable-a", false);
    let (grant, ()) = connection.apply_mesh_lane_grant_with_effect(&MeshLaneGrantMutationInput {
        workspace_id: empty.workspace_id.clone(), peer_id: first.peer_id.clone(),
        target_adapter: MeshLaneGrantTargetAdapter::new(first.peer_id.clone(), first.origin_node_id.clone()),
        material_lane: MeshLane::Metadata, expected_generation: 0,
        approval_config_digest: Some(format!("blake3:{}", "a".repeat(64))), updated_at: Some(BEFORE.to_owned()),
    }, |_| Ok::<(), std::convert::Infallible>(())).unwrap();
    assert_eq!(grant.metadata_override, Some(MeshLaneDecision::Allow));
    let expected = snapshot(&connection, &empty);
    let plan = plan(&expected, vec![candidate('b', "stable-a"), candidate('c', "stable-c")], false);
    let before = connection.list_mesh_peers(&empty.workspace_id).unwrap();
    let writes = Cell::new(0);
    let error = materialize_with_hooks(&connection, &expected, &discovery(), &plan, AFTER, || Ok(()), || {
        writes.set(writes.get() + 1);
        Err(denied("injected after the first real peer write"))
    }).unwrap_err();
    assert!(error.message().contains("injected"));
    assert_eq!(writes.get(), 1, "rollback test must actually reach a write");
    assert_eq!(connection.list_mesh_peers(&empty.workspace_id).unwrap(), before);
    assert_eq!(connection.get_mesh_lane_grant_state(&empty.workspace_id, &first.peer_id).unwrap(), Some(grant));
    // A successful retry proves cleanup and exercises the same invalidation path.
    materialize(&connection, &expected, &discovery(), &plan, AFTER, || Ok(())).unwrap();
    let changed = connection.get_mesh_lane_grant_state(&empty.workspace_id, &first.peer_id).unwrap().unwrap();
    assert!(changed.grant_generation > 0);
    assert_eq!(changed.metadata_override, None);
    assert_eq!(connection.list_mesh_peers(&empty.workspace_id).unwrap().len(), 2);
}

#[test]
fn failed_override_persistence_writes_no_peers_and_releases_the_transaction() {
    let (_root, connection, empty) = fixture();
    let plan = plan(&empty, vec![candidate('a', "stable-a")], false);
    assert!(materialize(&connection, &empty, &discovery(), &plan, AFTER, || Err(denied("override failure"))).is_err());
    assert!(connection.list_mesh_peers(&empty.workspace_id).unwrap().is_empty());
    materialize(&connection, &empty, &discovery(), &plan, AFTER, || Ok(())).unwrap();
    assert_eq!(connection.list_mesh_peers(&empty.workspace_id).unwrap().len(), 1);
}

#[test]
fn audit_only_plans_never_open_a_write_transaction_or_call_hooks() {
    let root = tempfile::tempdir().unwrap();
    let connection = DbConnection::open_file(&root.path().join("unmigrated.db")).unwrap();
    let (_other, _db, empty) = fixture();
    materialize(&connection, &empty, &discovery(), &AutoEnrollmentMaterializationPlan::default(), AFTER, || panic!("audit-only must not persist overrides")).unwrap();
}

#[test]
fn matching_authoritative_device_can_rotate_without_reminting_the_peer() {
    let (_root, connection, empty) = fixture();
    let first = seed(&connection, &empty, 'a', "stable-a", false);
    let bound = connection.observe_mesh_peer_transport_identity(&ObserveMeshPeerTransportIdentityInput {
        workspace_id: empty.workspace_id.clone(), peer_id: first.peer_id.clone(), tailnet_id: TAILNET.to_owned(),
        stable_node_id: "stable-a".to_owned(), current_node_pubkey: candidate('a', "stable-a").node_key,
        observed_at: Some(BEFORE.to_owned()),
    }).unwrap();
    let expected = snapshot(&connection, &empty);
    let plan = plan(&expected, vec![candidate('b', "stable-a")], false);
    assert!(plan.writes_peer_rows);
    materialize(&connection, &expected, &discovery(), &plan, AFTER, || Ok(())).unwrap();
    let rows = connection.list_mesh_peers(&empty.workspace_id).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].peer_id, bound.peer_id);
    assert_eq!(rows[0].origin_node_id, bound.origin_node_id);
    assert!(rows[0].enabled);
    assert!(rows[0].transport_identity.is_none(), "changed enrollment still requires fresh LocalAPI authorization");
}
