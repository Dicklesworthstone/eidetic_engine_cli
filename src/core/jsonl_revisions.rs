//! Revision identity recovery, separate from the author's validity interval.
//!
//! Prefer an exported supersession timestamp. Older post-V123 archives carry
//! only successor identity; those recover the marker from the successor's
//! validity start (or creation instant). Legacy valid_to derivation remains in
//! the storage layer and cannot overwrite an explicit recovered marker.

use super::{
    BTreeMap, BTreeSet, JsonlImportIssue, TimestampClass, ValidatedMemory,
    normalize_imported_timestamp,
};

fn invalid(reason: &'static str) -> JsonlImportIssue {
    // The stream is untrusted. Never copy source content or identifiers into
    // diagnostics, including cycles or references outside the archive.
    JsonlImportIssue::error(None, "invalid_memory_supersession", reason)
}

pub(super) fn supersession_timestamps(
    memories: &[ValidatedMemory<'_>],
) -> Result<BTreeMap<String, String>, JsonlImportIssue> {
    let by_id: BTreeMap<_, _> = memories
        .iter()
        .map(|memory| (memory.record.memory_id.as_str(), memory.record))
        .collect();
    let mut successors = BTreeMap::<&str, &str>::new();
    let mut predecessors = BTreeMap::<&str, &str>::new();
    let mut markers = BTreeMap::new();
    for memory in memories {
        let record = memory.record;
        if let Some(at) = &record.superseded_at {
            markers.insert(
                record.memory_id.clone(),
                normalize_imported_timestamp(at, TimestampClass::Validity),
            );
        }
        let edges = record
            .superseded_by
            .as_deref()
            .map(|next| (record.memory_id.as_str(), next))
            .into_iter()
            .chain(
                record
                    .supersedes
                    .as_deref()
                    .map(|prior| (prior, record.memory_id.as_str())),
            );
        for (prior_id, next_id) in edges {
            let prior = by_id
                .get(prior_id)
                .ok_or_else(|| invalid("supersession predecessor is absent from the archive"))?;
            let next = by_id
                .get(next_id)
                .ok_or_else(|| invalid("supersession successor is absent from the archive"))?;
            if prior_id == next_id
                || prior.workspace_id != next.workspace_id
                || prior.logical_id.as_deref().unwrap_or(prior_id)
                    != next.logical_id.as_deref().unwrap_or(next_id)
            {
                return Err(invalid(
                    "supersession must join distinct revisions in one workspace-local family",
                ));
            }
            if successors
                .insert(prior_id, next_id)
                .is_some_and(|existing| existing != next_id)
                || predecessors
                    .insert(next_id, prior_id)
                    .is_some_and(|existing| existing != prior_id)
            {
                return Err(invalid(
                    "supersession declares conflicting successors or predecessors",
                ));
            }
        }
    }
    // Walk each edge at most once. Iteration rather than recursion also handles
    // long histories without consuming the process stack. Do not infer order
    // from random/redacted identifiers or reject legitimate equal timestamps.
    let mut complete = BTreeSet::new();
    for &start in successors.keys() {
        let mut path = BTreeSet::new();
        let mut current = start;
        while !complete.contains(current) {
            if !path.insert(current) {
                return Err(invalid("supersession contains a cycle"));
            }
            let Some(&next) = successors.get(current) else {
                break;
            };
            current = next;
        }
        complete.extend(path);
    }
    for (prior_id, next_id) in successors {
        let next = by_id[next_id];
        markers.entry(prior_id.to_owned()).or_insert_with(|| {
            normalize_imported_timestamp(
                next.valid_from.as_deref().unwrap_or(&next.created_at),
                TimestampClass::Validity,
            )
        });
    }
    Ok(markers)
}
