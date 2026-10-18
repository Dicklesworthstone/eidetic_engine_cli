//! Maintain only the copied lexical tier, never rebuild its embedding corpus.
//!
//! Segment pressure is a lexical maintenance decision, not evidence that the
//! vector producer or source revision changed. The caller owns an independent
//! staging generation and its publication lease. No live index is opened for
//! writing; the normal tier validation and durability barrier still follow.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::search::IndexableDocument;

use frankensearch::lexical_tantivy::tantivy_crate::{
    Index, IndexWriter, TantivyDocument, Term,
    index::SegmentId,
    indexer::NoMergePolicy,
    tokenizer::{LowerCaser, SimpleTokenizer, TextAnalyzer},
};

use super::{
    IncrementalFallback, IncrementalFallbackReason, LEXICAL_INDEX_SUBDIR, batch,
    ensure_index_path_has_no_symlinks, incremental_fallback, metadata::document_payload_bytes,
};

// One indexing worker is sufficient: these writers only merge committed
// segments or delete exact ID terms. Match the backend's shipping heap
// allowance without spawning an automatically sized pool for maintenance.
const MERGE_WRITER_HEAP_BYTES: usize = 50_000_000;

const MAX_UPSERT_BATCH_DOCUMENTS: usize = 128;
const MAX_UPSERT_BATCH_PAYLOAD_BYTES: usize = 256 * 1024;

/// Borrow bounded slices for the adapter's native batch writer. The planner
/// and writer share payload accounting, including IDs, titles and metadata;
/// bounding only body text misses the point of a metadata-only refresh.
/// Nothing is cloned, normalized, deduplicated or reordered. A single large
/// document is passed through unchanged, alone, to guarantee progress.
///
/// These limits bound work between the caller's cancellation checkpoints,
/// not provider allocations or total RSS. The caller still commits once after
/// all batches and must abandon the entire private stage on any failure.
pub(super) fn upsert_batches(
    mut documents: &[IndexableDocument],
) -> impl Iterator<Item = &[IndexableDocument]> {
    std::iter::from_fn(move || {
        if documents.is_empty() {
            return None;
        }
        let mut count = 0;
        let mut bytes = 0_usize;
        for document in documents.iter().take(MAX_UPSERT_BATCH_DOCUMENTS) {
            let payload = document_payload_bytes(document);
            if count > 0
                && (bytes > MAX_UPSERT_BATCH_PAYLOAD_BYTES
                    || payload > MAX_UPSERT_BATCH_PAYLOAD_BYTES.saturating_sub(bytes))
            {
                break;
            }
            count += 1;
            bytes = bytes.saturating_add(payload);
        }
        let (batch, remaining) = documents.split_at(count);
        documents = remaining;
        Some(batch)
    })
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct Compaction {
    pub segments_before: usize,
    pub segments_after: usize,
    pub documents: u64,
}

fn failure(operation: &str, error: impl std::fmt::Display) -> IncrementalFallback {
    incremental_fallback(
        IncrementalFallbackReason::TierUnavailable,
        format!("lexical delta maintenance {operation} failed: {error}"),
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SegmentState {
    max_doc: u32,
    live_docs: u32,
    delete_opstamp: Option<u64>,
}

type SegmentStates = BTreeMap<SegmentId, SegmentState>;

/// Capture native identities and deletion state, not directory filenames.
/// Ordered identities make the before/after fence independent of meta ordering.
fn segment_states(index: &Index) -> Result<SegmentStates, IncrementalFallback> {
    let metas = index
        .searchable_segment_metas()
        .map_err(|error| failure("inventory", error))?;
    let mut states = BTreeMap::new();
    for meta in metas {
        let state = SegmentState {
            max_doc: meta.max_doc(),
            live_docs: meta.num_docs(),
            delete_opstamp: meta.delete_opstamp(),
        };
        if states.insert(meta.id(), state).is_some() {
            return Err(failure("inventory", "duplicate active segment identity"));
        }
    }
    Ok(states)
}

fn live_documents(states: &SegmentStates) -> Result<u64, IncrementalFallback> {
    states.values().try_fold(0_u64, |total, state| {
        total
            .checked_add(u64::from(state.live_docs))
            .ok_or_else(|| failure("inventory", "document count overflow"))
    })
}

/// Read committed segment identities, not all files ending in `.idx`. Retired
/// files can outlive a merge; neither they nor a directory-read error mean the
/// active lexical index has zero segments. Keep native parsing authoritative.
fn inventory(index: &Index) -> Result<(Vec<SegmentId>, u64), IncrementalFallback> {
    let states = segment_states(index)?;
    Ok((states.keys().copied().collect(), live_documents(&states)?))
}

/// Merge the smallest segments until at least half the segment allowance is
/// free. Include the entire equal-size boundary cohort, so a uniform set of
/// tiny deltas collapses in one operation rather than repeatedly merging pairs.
/// A large unchanged base is not rewritten merely because small deltas arrived.
///
/// max_doc includes deleted ordinals: it is a cheap work estimate, NOT a byte,
/// latency or RSS bound. Exact identities break ties deterministically. With a
/// zero/one limit, reducing to one segment necessarily includes every segment.
fn compaction_candidates(states: &SegmentStates, max_segments: usize) -> Vec<SegmentId> {
    let threshold = max_segments.max(1);
    if states.len() <= threshold {
        return Vec::new();
    }
    let target = (threshold / 2).max(1);
    let minimum = states.len() - target + 1;
    let mut ordered = states.iter().collect::<Vec<_>>();
    ordered.sort_unstable_by(|left, right| {
        left.1
            .max_doc
            .cmp(&right.1.max_doc)
            .then_with(|| left.0.cmp(right.0))
    });
    let boundary = ordered[minimum - 1].1.max_doc;
    ordered
        .into_iter()
        .take_while(|(_, state)| state.max_doc <= boundary)
        .map(|(id, _)| *id)
        .collect()
}

/// A partial merge must retain every unselected segment, including its delete
/// state. Equal total counts alone cannot prove that the right segments survived.
fn validate_compaction(
    before: &SegmentStates,
    after: &SegmentStates,
    selected: &[SegmentId],
    replacement: Option<SegmentId>,
    max_segments: usize,
) -> Result<(), IncrementalFallback> {
    let selected = selected.iter().copied().collect::<BTreeSet<_>>();
    if selected.len() < 2 || !selected.iter().all(|id| before.contains_key(id)) {
        return Err(failure("validation", "invalid compaction selection"));
    }
    let mut expected = BTreeSet::new();
    for (id, state) in before {
        if !selected.contains(id) {
            if after.get(id) != Some(state) {
                return Err(failure("validation", "unselected segment changed"));
            }
            expected.insert(*id);
        }
    }
    if let Some(id) = replacement {
        if before.contains_key(&id) {
            return Err(failure(
                "validation",
                "merge reused an existing segment identity",
            ));
        }
        expected.insert(id);
    }
    if after.keys().copied().collect::<BTreeSet<_>>() != expected
        || after.len() > (max_segments.max(1) / 2).max(1)
        || live_documents(before)? != live_documents(after)?
    {
        return Err(failure(
            "validation",
            "segment identities or live-document count changed unexpectedly",
        ));
    }
    Ok(())
}

fn open_staged(
    staging_dir: &Path,
    checkpoint: &mut impl FnMut() -> Result<(), IncrementalFallback>,
) -> Result<Index, IncrementalFallback> {
    checkpoint()?;
    let directory = staging_dir.join(LEXICAL_INDEX_SUBDIR);
    ensure_index_path_has_no_symlinks(&directory, "maintain staged lexical index")
        .map_err(|error| failure("path admission", error))?;
    ensure_index_path_has_no_symlinks(&directory.join("meta.json"), "read staged lexical metadata")
        .map_err(|error| failure("metadata admission", error))?;
    checkpoint()?;
    Index::open_in_dir(&directory).map_err(|error| failure("open", error))
}

fn maintenance_writer(index: &Index) -> Result<IndexWriter, IncrementalFallback> {
    // Workers still resolve the named analyzer at construction. No documents
    // are added here, so neither deletion nor compaction tokenizes source text
    // or assigns ordinals. The adapter's ordinal sidecar remains unchanged.
    index.tokenizers().register(
        "frankensearch_default",
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(LowerCaser)
            .build(),
    );
    let writer = index
        .writer_with_num_threads::<TantivyDocument>(1, MERGE_WRITER_HEAP_BYTES)
        .map_err(|error| failure("writer", error))?;
    writer.set_merge_policy(Box::new(NoMergePolicy));
    Ok(writer)
}

/// Commit all removals once, before the normal adapter opens for upserts.
/// Its single-document delete API commits and reloads on every call; using it
/// in the delta loop made N withdrawals perform N commits before finalization.
/// Exact native ID terms need no document hydration or ordinal-table update.
///
/// Only this caller-owned staging copy changes. A cancelled or failed attempt
/// is still discarded by the publisher, even if its private delete commit has
/// completed. Every path after writer acquisition joins the backend workers.
pub(super) fn delete_documents(
    cx: &asupersync::Cx,
    staging_dir: &Path,
    removals: &[String],
) -> Result<u64, IncrementalFallback> {
    delete_with_checkpoint(staging_dir, removals, || batch::checkpoint(cx, "lexical"))
}

fn delete_with_checkpoint(
    staging_dir: &Path,
    removals: &[String],
    mut checkpoint: impl FnMut() -> Result<(), IncrementalFallback>,
) -> Result<u64, IncrementalFallback> {
    if removals.is_empty() {
        checkpoint()?;
        return Ok(0);
    }
    let index = open_staged(staging_dir, &mut checkpoint)?;
    let (segments, documents) = inventory(&index)?;
    let id_field = index
        .schema()
        .get_field("id")
        .map_err(|error| failure("delete ID field", error))?;
    checkpoint()?;
    let mut writer = maintenance_writer(&index)?;
    let deleted = (|| {
        checkpoint()?;
        if inventory(&index)? != (segments, documents) {
            return Err(failure(
                "delete inventory",
                "staged generation changed before writer acquisition",
            ));
        }
        for id in removals {
            checkpoint()?;
            // This is an exact term, not a parsed query. Missing or repeated
            // IDs retain the single-document API's idempotent semantics.
            writer.delete_term(Term::from_field_text(id_field, id));
        }
        checkpoint()?;
        writer
            .commit()
            .map_err(|error| failure("delete commit", error))?;
        Ok(())
    })();
    let joined = writer
        .wait_merging_threads()
        .map_err(|error| failure("delete worker join", error));
    checkpoint()?;
    deleted?;
    joined?;
    let (_, after_documents) = inventory(&index)?;
    checkpoint()?;
    let removed = documents
        .checked_sub(after_documents)
        .filter(|removed| *removed <= removals.len() as u64)
        .ok_or_else(|| failure("delete validation", "unexpected live-document count"))?;
    tracing::debug!(
        target: "ee::index",
        requested_removals = removals.len(),
        removed_documents = removed,
        "committed staged lexical removals in one batch"
    );
    Ok(removed)
}

/// Runs before opening the staged lexical upsert writer. A tier at or below
/// the threshold stays byte-for-byte unchanged and takes no writer lock.
/// Above it, merge a size-selected subset using Tantivy itself, preserving the
/// larger untouched segments. There is no re-tokenization or provider call.
pub(super) fn compact_if_needed(
    cx: &asupersync::Cx,
    staging_dir: &Path,
    max_segments: usize,
) -> Result<Compaction, IncrementalFallback> {
    compact_with_checkpoint(staging_dir, max_segments, || {
        batch::checkpoint(cx, "lexical")
    })
}

fn compact_with_checkpoint(
    staging_dir: &Path,
    max_segments: usize,
    mut checkpoint: impl FnMut() -> Result<(), IncrementalFallback>,
) -> Result<Compaction, IncrementalFallback> {
    let index = open_staged(staging_dir, &mut checkpoint)?;
    let before = segment_states(&index)?;
    let documents = live_documents(&before)?;
    checkpoint()?;
    let selected = compaction_candidates(&before, max_segments);
    checkpoint()?;
    if selected.is_empty() {
        return Ok(Compaction {
            segments_before: before.len(),
            segments_after: before.len(),
            documents,
        });
    }
    let mut writer = maintenance_writer(&index)?;
    // Every exit joins the native workers. A cancelled merge must not leave a
    // writer racing staging cleanup or fallback. Native IO is not preemptible.
    let merged = (|| {
        checkpoint()?;
        if segment_states(&index)? != before {
            return Err(failure(
                "inventory",
                "staged generation changed before writer acquisition",
            ));
        }
        checkpoint()?;
        let replacement = writer
            .merge(&selected)
            .wait()
            .map_err(|error| failure("merge", error))?
            .map(|meta| meta.id());
        checkpoint()?;
        let _ = writer
            .garbage_collect_files()
            .wait()
            .map_err(|error| failure("garbage collection", error))?;
        Ok(replacement)
    })();
    let joined = writer
        .wait_merging_threads()
        .map_err(|error| failure("worker join", error));
    // A cancelled request wins over a backend interruption, matching the
    // surrounding publisher's existing cancellation/fallback ordering.
    checkpoint()?;
    let replacement = merged?;
    joined?;
    let after = segment_states(&index)?;
    checkpoint()?;
    validate_compaction(&before, &after, &selected, replacement, max_segments)?;
    let report = Compaction {
        segments_before: before.len(),
        segments_after: after.len(),
        documents,
    };
    tracing::debug!(
        target: "ee::index",
        segments_before = report.segments_before,
        segments_after = report.segments_after,
        merged_segments = selected.len(),
        retained_segments = before.len() - selected.len(),
        documents = report.documents,
        "compacted staged lexical tier without re-embedding or rewriting larger segments"
    );
    Ok(report)
}

#[cfg(test)]
#[path = "index_delta_lexical_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "index_delta_selective_compaction_tests.rs"]
mod selective_compaction_tests;
