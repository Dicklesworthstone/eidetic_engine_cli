//! Reuse durable, opaque peer principals across a verified device's key rotation.
//!
//! Stable IDs are lookup bindings, never bytes from which an ee principal is
//! derived. They are meaningful only inside the probed tailnet and workspace.
//! This runs after discovery/hello admission; it does not grant any new lane.

use super::*;
use crate::mesh::foreground_cli::MeshPeerRow;
use crate::mesh::peer::MeshPeerState;

fn identity_error(message: &str) -> DomainError {
    DomainError::PolicyDenied {
        message: message.to_owned(),
        repair: Some(
            "Inspect ee mesh peer list --json and resolve the stored enrollment before retrying."
                .to_owned(),
        ),
    }
}

fn stable_id(value: Option<&str>) -> Result<Option<&str>, DomainError> {
    match value {
        Some(value)
            if value.is_empty()
                || value.len() > 256
                || value.trim() != value
                || value.chars().any(char::is_control) =>
        {
            Err(identity_error(
                "Mesh enrollment contains an invalid stable device identity",
            ))
        }
        value => Ok(value),
    }
}

/// Resolve at most one existing principal. A conflicting stable binding on the
/// same current key is an error, not permission to mint a second principal.
pub(super) fn resolve_existing<'a>(
    workspace_id: &str,
    tailnet_id: &str,
    candidate: &AutoEnrollmentCandidate,
    rows: &'a [MeshPeerRow],
) -> Result<Option<(&'a MeshPeerRow, Option<MeshPeerRecord>)>, DomainError> {
    let incoming = stable_id(candidate.stable_node_id.as_deref())?;
    let mut matched = None;
    for row in rows {
        let record = enrolled_peer_record_from_policy_summary(
            row.policy_summary_json.as_deref(),
            &row.peer_id,
        )?;
        let matches = if let Some(record) = &record {
            if record.workspace_id != workspace_id || record.endpoint.tailnet_id != tailnet_id {
                continue;
            }
            let stored = stable_id(record.endpoint.stable_node_id.as_deref())?;
            let same_key = record.endpoint.tailscale_node_key == candidate.node_key;
            if same_key && incoming.is_some() && stored.is_some() && incoming != stored {
                return Err(identity_error(
                    "Mesh node key conflicts with its stored stable device identity",
                ));
            }
            same_key || incoming.is_some_and(|id| Some(id) == stored)
        } else {
            // Pre-enrollment rows have only the original exact-key binding.
            // No stable-ID, hostname, DNS or IP inference is permitted here.
            row.origin_node_id == candidate.node_key
        };
        if !matches {
            continue;
        }
        if matched.is_some() {
            return Err(identity_error(
                "Auto-enrollment found ambiguous durable principals",
            ));
        }
        if !row.enabled
            || record.as_ref().is_some_and(|record| {
                record.state == MeshPeerState::Revoked
                    || record.revoked_at.is_some()
                    || record.key.revoked_at.is_some()
            })
        {
            return Err(identity_error(
                "Auto-enrollment cannot reactivate a disabled or revoked peer",
            ));
        }
        matched = Some((row, record));
    }
    Ok(matched)
}

/// Keep continuity metadata while accepting a freshly admitted endpoint and
/// conservative capabilities. Never erase a known stable binding when a
/// manual-to-auto candidate (or an older probe) supplies none.
pub(super) fn retain_identity(
    peer: &mut MeshPeerRecord,
    previous: &MeshPeerRecord,
    now: &str,
) -> Result<(), DomainError> {
    if peer.endpoint.stable_node_id.is_none() {
        peer.endpoint
            .stable_node_id
            .clone_from(&previous.endpoint.stable_node_id);
    }
    peer.enrolled_at.clone_from(&previous.enrolled_at);
    peer.origin_workspace_id
        .clone_from(&previous.origin_workspace_id);
    let fingerprint = peer.key.public_key_fingerprint.clone();
    peer.key = previous.key.clone();
    if peer.key.public_key_fingerprint != fingerprint {
        peer.key.generation = peer.key.generation.checked_add(1).ok_or_else(|| {
            identity_error("Mesh peer key generation is exhausted; rotation withheld")
        })?;
        peer.key.public_key_fingerprint = fingerprint;
        peer.key.rotated_at = Some(now.to_owned());
    }
    Ok(())
}

/// Two independently admitted candidates cannot address one principal in the
/// same batch, even when their current keys differ. No last-write-wins merge.
pub(super) fn check_candidate_identities(
    candidates: &[AutoEnrollmentCandidate],
) -> Result<(), DomainError> {
    let mut keys = BTreeSet::new();
    let mut stable_ids = BTreeSet::new();
    for candidate in candidates {
        if !keys.insert(candidate.node_key.as_str()) {
            return Err(identity_error(
                "Auto-enrollment contains duplicate current node keys",
            ));
        }
        if let Some(id) = stable_id(candidate.stable_node_id.as_deref())?
            && !stable_ids.insert(id)
        {
            return Err(identity_error(
                "Auto-enrollment contains ambiguous stable device identities",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "mesh_peer_identity_tests.rs"]
mod tests;
