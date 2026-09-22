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

/// Imported identities eligible for the pre-V123 expiry-based fallback.
///
/// Call after validation has checked references, family membership and cycles.
/// An explicit nullable marker preserves the stored supersession state. An
/// explicit edge defines the headship of BOTH endpoints; its terminal node
/// can have an author-supplied expiry without being superseded. Protect these
/// nodes even when creation timestamps disagree with the explicit edge order.
/// Do not exclude an entire family: a mixed-era archive may still contain an
/// older, unreferenced ancestor whose only history marker is its expiry.
pub(super) fn legacy_supersession_ids(memories: &[ValidatedMemory<'_>]) -> BTreeSet<String> {
    let mut explicit = BTreeSet::new();
    for memory in memories {
        let record = memory.record;
        if record.superseded_at.is_some() {
            explicit.insert(record.memory_id.as_str());
        }
        if let Some(next) = record.superseded_by.as_deref() {
            explicit.insert(record.memory_id.as_str());
            explicit.insert(next);
        }
        if let Some(prior) = record.supersedes.as_deref() {
            explicit.insert(record.memory_id.as_str());
            explicit.insert(prior);
        }
    }
    memories
        .iter()
        .filter(|memory| {
            let record = memory.record;
            (record.valid_to.is_some() || record.expires_at.is_some())
                && !explicit.contains(record.memory_id.as_str())
        })
        // The writer uses imported IDs, which differ from archive aliases for
        // redacted records. Never pass the unparsed source identity to SQL.
        .map(|memory| memory.id.clone())
        .collect()
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
        if let Some(Some(at)) = &record.superseded_at {
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
            if prior.superseded_at == Some(None) {
                return Err(invalid(
                    "supersession edge contradicts an explicit unsuperseded predecessor",
                ));
            }
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
    // Explicit terminal revisions remain current even when the author gave
    // them an expiry. The legacy lineage gate treats expiry-only rows as
    // possible history; letting that compatibility rule cover two explicit
    // terminals would silently accept disconnected current heads in one
    // family. Tombstoned or explicitly superseded terminals are history.
    let mut current_families = BTreeSet::new();
    for &head_id in predecessors.keys() {
        let head = by_id[head_id];
        if successors.contains_key(head_id)
            || head.superseded_at.as_ref().is_some_and(Option::is_some)
            || head.tombstoned_at.is_some()
        {
            continue;
        }
        let family = (
            head.workspace_id.as_str(),
            head.logical_id.as_deref().unwrap_or(head_id),
        );
        if !current_families.insert(family) {
            return Err(invalid(
                "revision family declares multiple explicit current heads",
            ));
        }
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

#[cfg(test)]
#[path = "jsonl_revision_heads_tests.rs"]
mod head_tests;
