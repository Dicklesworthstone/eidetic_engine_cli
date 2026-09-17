//! Commit an enrollment plan against the peer state that discovery actually saw.
//!
//! Discovery can perform network IO. Its snapshot is not authority to overwrite
//! a revocation or a competing enrollment that completed while those probes ran.
//! Recheck security-relevant peer state inside the same writer-fenced transaction
//! as every upsert, revocation and lane-grant invalidation.

use super::*;
use std::collections::BTreeMap;
use crate::db::StoredMeshPeer;
use crate::mesh::auto_enrollment::AutoEnrollmentMaterializationPlan;
use crate::mesh::foreground_cli::MeshPeerRow;

pub(super) fn materialize(
    connection: &DbConnection,
    expected: &MeshForegroundSnapshot,
    discovery: &TailscaleAutodiscoveryReport,
    plan: &AutoEnrollmentMaterializationPlan,
    now: &str,
    before_write: impl FnOnce() -> Result<(), DomainError>,
) -> Result<(), DomainError> {
    materialize_with_hooks(connection, expected, discovery, plan, now, before_write, || Ok(()))
}

fn materialize_with_hooks(
    connection: &DbConnection,
    expected: &MeshForegroundSnapshot,
    discovery: &TailscaleAutodiscoveryReport,
    plan: &AutoEnrollmentMaterializationPlan,
    now: &str,
    before_write: impl FnOnce() -> Result<(), DomainError>,
    mut after_write: impl FnMut() -> Result<(), DomainError>,
) -> Result<(), DomainError> {
    if !plan.writes_peer_rows {
        return Ok(());
    }
    let result: Result<(), WriteError> = connection.with_transaction_error(|| {
        let current = connection.list_mesh_peers(&expected.workspace_id)?;
        let rows: Vec<_> = current.iter().map(MeshPeerRow::from).collect();
        if security_rows(&expected.peers)? != security_rows(&rows)? {
            return Err(denied("Mesh peers changed during discovery; enrollment was not applied").into());
        }
        let upserts = auto_enrollment_peer_upserts(
            &expected.workspace_id,
            discovery.tailnet_id.as_deref().unwrap_or("tailnet_unknown"),
            discovery.tailnet_display_name.as_deref(),
            discovery.self_node_key.as_deref(),
            now,
            &rows,
            &plan.peers_to_upsert,
        )?;
        for input in &upserts {
            if let Some(previous) = current.iter().find(|row| row.peer_id == input.peer_id) {
                validate_transport_binding(previous, input)?;
            }
        }
        let mut snapshot = expected.clone();
        snapshot.peers = rows;
        let revocations = auto_enrollment_peer_revocations(
            &expected.workspace_id, &snapshot, &plan.peers_to_revoke, now,
        )?;
        let refreshed: BTreeSet<_> = upserts.iter().map(|input| input.peer_id.as_str()).collect();
        let manual_ids: BTreeSet<_> = auto_enrollment_existing_peers(&snapshot)?
            .into_iter().filter(|peer| !peer.is_auto_managed()).map(|peer| peer.peer_id).collect();
        let mut retained_revocations = Vec::new();
        for revocation in revocations {
            if refreshed.contains(revocation.peer_id.as_str()) {
                let old_row = snapshot.peers.iter().find(|row| row.peer_id == revocation.peer_id)
                    .ok_or_else(|| denied("Mesh revocation target is missing; enrollment withheld"))?;
                let old_key = auto_enrollment_node_key_for_row(old_row)?;
                // Replacing manual management must not disable the SAME opaque
                // principal immediately after refreshing it. Explicit exclusions,
                // however, must never be neutralized by an enrollment candidate.
                if !plan.manual_to_auto_migration_intended
                    || !manual_ids.contains(revocation.peer_id.as_str())
                    || plan.append_denylist_node_keys.contains(&old_key)
                {
                    return Err(denied("Mesh enrollment conflicts with an explicit revocation").into());
                }
            } else {
                retained_revocations.push(revocation);
            }
        }
        // Filesystem overrides retain their existing separate persistence
        // contract. Validate all identity decisions BEFORE invoking this hook;
        // a later database failure does not claim to roll back configuration.
        before_write()?;
        for input in upserts.iter().chain(&retained_revocations) {
            connection.upsert_mesh_peer_with_grant_invalidation_in_current_transaction(input)?;
            after_write()?;
        }
        Ok(())
    });
    result.map_err(|error| error.0)
}

/// Observation timestamps and display labels do not confer authority. Changes
/// to membership, enabled state, identity, or policy must cause a fresh plan.
fn security_rows(rows: &[MeshPeerRow]) -> Result<BTreeMap<&str, (&str, bool, Option<&str>)>, DomainError> {
    let mut keys = BTreeMap::new();
    for row in rows {
        if keys.insert(row.peer_id.as_str(), (
            row.origin_node_id.as_str(), row.enabled, row.policy_summary_json.as_deref(),
        )).is_some() {
            return Err(denied("Mesh snapshot contains duplicate peer principals"));
        }
    }
    Ok(keys)
}

fn validate_transport_binding(previous: &StoredMeshPeer, input: &UpsertMeshPeerInput) -> Result<(), DomainError> {
    let Some(binding) = &previous.transport_identity else { return Ok(()); };
    let record = enrolled_peer_record_from_policy_summary(input.policy_summary_json.as_deref(), &input.peer_id)?
        .ok_or_else(|| denied("Mesh enrollment lacks its typed peer identity"))?;
    let endpoint = &record.endpoint;
    let same_device = endpoint.tailnet_id == binding.tailnet_id
        && match endpoint.stable_node_id.as_deref() {
            Some(stable) => stable == binding.stable_node_id,
            None => endpoint.tailscale_node_key == binding.current_node_pubkey,
        };
    if !same_device {
        return Err(denied("Mesh enrollment conflicts with the authoritative transport identity"));
    }
    Ok(())
}

fn denied(message: &str) -> DomainError {
    DomainError::PolicyDenied {
        message: message.to_owned(),
        repair: Some("Inspect ee mesh peer list --json and rerun fresh discovery after resolving the change.".to_owned()),
    }
}

struct WriteError(DomainError);

impl From<DomainError> for WriteError {
    fn from(error: DomainError) -> Self { Self(error) }
}

impl From<DbError> for WriteError {
    fn from(_error: DbError) -> Self {
        Self(DomainError::Storage {
            message: "Failed to atomically apply mesh enrollment; peer writes were rolled back".to_owned(),
            repair: Some("Run ee doctor --json and retry fresh auto-enrollment.".to_owned()),
        })
    }
}

#[cfg(test)]
#[path = "mesh_auto_enrollment_write_tests.rs"]
mod tests;
