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

pub(super) fn link_is_admitted(link: &StoredMemoryLink, workspaces: &BTreeMap<&str, &str>) -> bool {
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

/// Repeated rows can arise when a caller joins or batches its source reads.
/// One link ID must still describe exactly one source record. Reconcile that
/// identity BEFORE visibility filtering: a visible twin cannot launder a
/// contradictory denied, foreign or stale version of the same source ID.
///
/// Retain borrowed source representatives, then one winner per undirected
/// pair. No source body or metadata is cloned. The maps retain at most one
/// entry per distinct source ID/pair, not per repeated input row.
pub(super) fn canonical_links<'a>(
    links: &'a [StoredMemoryLink],
    workspaces: &BTreeMap<&str, &str>,
) -> Vec<&'a StoredMemoryLink> {
    let mut sources = BTreeMap::<&str, Option<&StoredMemoryLink>>::new();
    for link in links {
        if link.id.trim().is_empty() {
            continue;
        }
        match sources.entry(link.id.as_str()) {
            Entry::Vacant(entry) => {
                entry.insert(Some(link));
            }
            Entry::Occupied(mut entry) => {
                if (*entry.get()).is_some_and(|previous| !same_source_link(previous, link)) {
                    entry.insert(None);
                }
            }
        }
    }
    let mut pairs = BTreeMap::new();
    // BTreeMap visits source IDs lexically. First insertion for a canonical
    // pair therefore preserves the historical smallest-link-ID convention.
    for link in sources.into_values().flatten() {
        if !link_is_admitted(link, workspaces) {
            continue;
        }
        let pair = super::canonical_pair(link);
        if pair.0 != pair.1 {
            pairs.entry(pair).or_insert(link);
        }
    }
    pairs.into_values().collect()
}

/// Compare the complete persisted record, not just its graph attributes.
/// Metadata formatting is intentionally not normalized: there is no authority
/// to reconcile two distinct observations of one source ID here. Float bits
/// preserve that distinction without making NaN look equal to a valid score.
fn same_source_link(left: &StoredMemoryLink, right: &StoredMemoryLink) -> bool {
    left.src_memory_id == right.src_memory_id
        && left.dst_memory_id == right.dst_memory_id
        && left.relation == right.relation
        && left.weight.to_bits() == right.weight.to_bits()
        && left.confidence.to_bits() == right.confidence.to_bits()
        && left.directed == right.directed
        && left.evidence_count == right.evidence_count
        && left.last_reinforced_at == right.last_reinforced_at
        && left.source == right.source
        && left.created_at == right.created_at
        && left.created_by == right.created_by
        && left.metadata_json == right.metadata_json
}
