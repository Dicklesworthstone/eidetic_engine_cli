//! Complete, bounded-query retrieval of explicitly stored ask counterevidence.
//!
//! Candidate relevance is decided by the ask engine, not by database row order.
//! Consequently every memory in the caller's scope must participate in the link
//! lookup. Each SQL call stays small, and incident links are intersected with
//! the complete scope before they can influence an answer.

use std::collections::{BTreeMap, BTreeSet};

use crate::db::{DbConnection, MemoryLinkRelation};
use crate::models::DomainError;

use super::AskContradiction;

// The storage query binds each ID for both endpoints. Leave ample headroom
// for its relation predicate and for conservative SQLite parameter limits.
const LINK_QUERY_BATCH_SIZE: usize = 256;

/// Load every stored contradiction whose endpoints both belong to `memory_ids`.
///
/// The caller supplies the complete, already-authorized, non-tombstoned memory
/// scope. This operation never fetches a memory body, broadens that scope, or
/// follows a second link. Confidence/source eligibility remains the engine's
/// responsibility. Any storage failure aborts the whole read: missing opposing
/// evidence must not silently become a confident, one-sided answer.
pub fn load_scoped_contradictions(
    connection: &DbConnection,
    memory_ids: &[&str],
) -> Result<Vec<AskContradiction>, DomainError> {
    let scope: BTreeSet<&str> = memory_ids.iter().copied().collect();
    let ordered_ids: Vec<&str> = scope.iter().copied().collect();
    let mut links = BTreeMap::new();
    for batch in ordered_ids.chunks(LINK_QUERY_BATCH_SIZE) {
        let rows = connection
            .list_memory_links_for_memories(batch, Some(MemoryLinkRelation::Contradicts))
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to read ask contradiction evidence: {error}"),
                repair: Some("ee doctor --json".to_owned()),
            })?;
        for link in rows {
            if !scope.contains(link.src_memory_id.as_str())
                || !scope.contains(link.dst_memory_id.as_str())
            {
                continue;
            }
            let value = AskContradiction {
                id: link.id,
                src_memory_id: link.src_memory_id,
                dst_memory_id: link.dst_memory_id,
                confidence: link.confidence,
                source: link.source,
            };
            if let Some(previous) = links.get(value.id.as_str()) {
                // A cross-batch edge is normally returned twice. Do not select
                // an arbitrary version if it changed between those reads.
                let previous: &AskContradiction = previous;
                if previous.src_memory_id != value.src_memory_id
                    || previous.dst_memory_id != value.dst_memory_id
                    || previous.confidence.to_bits() != value.confidence.to_bits()
                    || previous.source != value.source
                {
                    return Err(DomainError::Storage {
                        message: "Ask contradiction evidence changed during retrieval".to_owned(),
                        repair: Some(
                            "retry ee ask after concurrent link changes settle".to_owned(),
                        ),
                    });
                }
            } else {
                links.insert(value.id.clone(), value);
            }
        }
    }
    Ok(links.into_values().collect())
}

#[cfg(test)]
#[path = "ask_store_tests.rs"]
mod tests;
