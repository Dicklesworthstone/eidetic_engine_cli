//! Enforce the supplied live-memory boundary before structural graph creation.
//!
//! FNX edge insertion can create endpoints. A link is therefore not authority
//! to introduce a memory missing from the caller's selection, resurrect a
//! tombstone, or connect two workspaces. Mesh visibility uses the same policy
//! adapter as ordinary graph projection, before parallel-edge deduplication.

use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};

use crate::db::{StoredMemory, StoredMemoryLink};

/// Borrow only identity and scope, never source bodies. Repeated coherent rows
/// are harmless; conflicting scope or liveness for one ID is a permanent
/// refusal within this input. Input order cannot choose its authority.
pub(super) fn live_memory_workspaces(memories: &[StoredMemory]) -> BTreeMap<&str, &str> {
    let mut admitted = BTreeMap::new();
    let mut refused = BTreeSet::new();
    for memory in memories {
        let id = memory.id.as_str();
        if id.trim().is_empty() || refused.contains(id) {
            continue;
        }
        if memory.tombstoned_at.is_some() || memory.workspace_id.trim().is_empty() {
            admitted.remove(id);
            refused.insert(id);
            continue;
        }
        match admitted.entry(id) {
            Entry::Vacant(entry) => {
                entry.insert(memory.workspace_id.as_str());
            }
            Entry::Occupied(entry) if *entry.get() != memory.workspace_id.as_str() => {
                entry.remove();
                refused.insert(id);
            }
            Entry::Occupied(_) => {}
        }
    }
    admitted
}

pub(super) fn link_is_admitted(
    link: &StoredMemoryLink,
    workspaces: &BTreeMap<&str, &str>,
) -> bool {
    let Some(source_workspace) = workspaces.get(link.src_memory_id.as_str()) else {
        return false;
    };
    let Some(target_workspace) = workspaces.get(link.dst_memory_id.as_str()) else {
        return false;
    };
    source_workspace == target_workspace
        && link.weight.is_finite()
        && link.confidence.is_finite()
        && super::super::memory_link_mesh_metadata_visible(link.metadata_json.as_deref())
}
