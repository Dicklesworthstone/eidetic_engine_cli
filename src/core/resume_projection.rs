//! Bounded public projection over the already-admitted resume snapshot.
//!
//! Parse each corpus timestamp once per index/sort. Staleness is a join on
//! (memory kind, subject tag), not a new scan of the entire memory corpus for
//! each rendered item. Keep all rows eligible as superseders even when they
//! fall outside the requested session or open-loop output pages.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};

use super::{ResumeItem, StaleFlag, is_control_tag, parse_ts, public_resume_text};
use crate::db::StoredMemory;

/// RFC 3339 strings with offsets or fractional seconds are not lexically
/// chronological. Invalid timestamps cannot outrank a valid current row.
/// Equal instants retain the documented descending-ID tie-break.
pub(super) fn sort_newest_first(memories: &mut [&StoredMemory]) {
    memories.sort_by_cached_key(|memory| {
        (
            Reverse(parse_ts(&memory.created_at)),
            Reverse(memory.id.clone()),
        )
    });
}

#[derive(Clone, Copy)]
struct SubjectHead<'a> {
    memory: &'a StoredMemory,
    created_at: DateTime<Utc>,
}

impl SubjectHead<'_> {
    fn supersedes(&self, other: &Self) -> bool {
        self.created_at > other.created_at
            || (self.created_at == other.created_at && self.memory.id < other.memory.id)
    }
}

pub(super) struct StalenessIndex<'a> {
    heads: BTreeMap<&'a str, BTreeMap<&'a str, SubjectHead<'a>>>,
    tags: &'a BTreeMap<String, Vec<String>>,
}

impl<'a> StalenessIndex<'a> {
    pub(super) fn new(
        all_live: &'a [StoredMemory],
        tags: &'a BTreeMap<String, Vec<String>>,
    ) -> Self {
        let mut index = Self {
            heads: BTreeMap::new(),
            tags,
        };
        for memory in all_live {
            let Some(created_at) = parse_ts(&memory.created_at) else {
                continue;
            };
            let Some(subjects) = tags.get(&memory.id) else {
                continue;
            };
            let candidate = SubjectHead { memory, created_at };
            for subject in subjects.iter().filter(|tag| !is_control_tag(tag)) {
                let head = index
                    .heads
                    .entry(memory.kind.as_str())
                    .or_default()
                    .entry(subject.as_str())
                    .or_insert(candidate);
                if candidate.supersedes(head) {
                    *head = candidate;
                }
            }
        }
        index
    }

    pub(super) fn apply(&self, items: &mut [ResumeItem]) -> BTreeSet<String> {
        let mut flagged = BTreeSet::new();
        for surfaced in items {
            let Some(surfaced_at) = parse_ts(&surfaced.created_at) else {
                continue;
            };
            let Some(subjects) = self.tags.get(&surfaced.memory_id) else {
                continue;
            };
            let Some(heads) = self.heads.get(surfaced.kind.as_str()) else {
                continue;
            };
            let mut best: Option<&SubjectHead<'_>> = None;
            for subject in subjects.iter().filter(|tag| !is_control_tag(tag)) {
                let Some(candidate) = heads.get(subject.as_str()) else {
                    continue;
                };
                if candidate.memory.id == surfaced.memory_id || candidate.created_at <= surfaced_at
                {
                    continue;
                }
                if best.is_none_or(|prior| candidate.supersedes(prior)) {
                    best = Some(candidate);
                }
            }
            let Some(best) = best else { continue };
            // The winning row was indexed from these same tags. Reconstruct
            // the exact shared-subject explanation (sorted and deduplicated),
            // rather than reporting only the tag that found the winning row.
            let Some(winner_tags) = self.tags.get(&best.memory.id) else {
                continue;
            };
            let shared: BTreeSet<&String> = subjects
                .iter()
                .filter(|tag| !is_control_tag(tag) && winner_tags.contains(tag))
                .collect();
            let shared_tags = shared
                .into_iter()
                .map(|tag| {
                    public_resume_text(tag, "stale.sharedTag", &mut surfaced.redaction.reasons)
                })
                .collect();
            surfaced.redaction.reasons.sort();
            surfaced.redaction.reasons.dedup();
            surfaced.redaction.applied = !surfaced.redaction.reasons.is_empty();
            surfaced.stale = Some(StaleFlag {
                superseded_by: best.memory.id.clone(),
                superseded_by_created_at: best.memory.created_at.clone(),
                shared_tags,
            });
            flagged.insert(surfaced.memory_id.clone());
        }
        flagged
    }
}

#[cfg(test)]
#[path = "resume_projection_tests.rs"]
mod tests;
