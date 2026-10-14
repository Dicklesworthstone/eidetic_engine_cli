//! Compact only the copied lexical tier, never rebuild its embedding corpus.
//!
//! Segment pressure is a lexical maintenance decision, not evidence that the
//! vector producer or source revision changed. The caller owns an independent
//! staging generation and its publication lease. No live index is opened for
//! writing; the normal tier validation and durability barrier still follow.

use std::path::Path;

use frankensearch::lexical_tantivy::tantivy_crate::{
    Index, SegmentId, TantivyDocument, indexer::NoMergePolicy,
    tokenizer::{LowerCaser, SimpleTokenizer, TextAnalyzer},
};

use super::{
    IncrementalFallback, IncrementalFallbackReason, LEXICAL_INDEX_SUBDIR, batch,
    ensure_index_path_has_no_symlinks, incremental_fallback,
};

// One indexing worker is sufficient: this writer only merges committed
// segments. Match the backend's shipping heap allowance, without spawning an
// automatically sized indexing pool for maintenance.
const MERGE_WRITER_HEAP_BYTES: usize = 50_000_000;

#[derive(Debug, Eq, PartialEq)]
pub(super) struct Compaction {
    pub segments_before: usize,
    pub segments_after: usize,
    pub documents: u64,
}

fn failure(operation: &str, error: impl std::fmt::Display) -> IncrementalFallback {
    incremental_fallback(
        IncrementalFallbackReason::TierUnavailable,
        format!("lexical delta compaction {operation} failed: {error}"),
    )
}

/// Read committed segment identities, not all files ending in `.idx`. Retired
/// files can outlive a merge; neither they nor a directory-read error mean the
/// active lexical index has zero segments. Keep native parsing authoritative.
fn inventory(index: &Index) -> Result<(Vec<SegmentId>, u64), IncrementalFallback> {
    let metas = index
        .searchable_segment_metas()
        .map_err(|error| failure("inventory", error))?;
    let mut documents = 0_u64;
    let mut ids = Vec::with_capacity(metas.len());
    for meta in metas {
        documents = documents
            .checked_add(u64::from(meta.num_docs()))
            .ok_or_else(|| failure("inventory", "document count overflow"))?;
        ids.push(meta.id());
    }
    Ok((ids, documents))
}

/// Runs before opening the staged lexical upsert writer. A tier at or below
/// the threshold stays byte-for-byte unchanged and takes no writer lock.
/// Above it, merge existing postings/stored fields using Tantivy itself. There
/// is no text re-tokenization, provider call, or invented search implementation.
pub(super) fn compact_if_needed(
    cx: &asupersync::Cx,
    staging_dir: &Path,
    max_segments: usize,
) -> Result<Compaction, IncrementalFallback> {
    compact_with_checkpoint(staging_dir, max_segments, || batch::checkpoint(cx, "lexical"))
}

fn compact_with_checkpoint(
    staging_dir: &Path,
    max_segments: usize,
    mut checkpoint: impl FnMut() -> Result<(), IncrementalFallback>,
) -> Result<Compaction, IncrementalFallback> {
    checkpoint()?;
    let directory = staging_dir.join(LEXICAL_INDEX_SUBDIR);
    ensure_index_path_has_no_symlinks(&directory, "compact staged lexical index")
        .map_err(|error| failure("path admission", error))?;
    ensure_index_path_has_no_symlinks(&directory.join("meta.json"), "read staged lexical metadata")
        .map_err(|error| failure("metadata admission", error))?;
    checkpoint()?;
    let index = Index::open_in_dir(&directory).map_err(|error| failure("open", error))?;
    let (segments, documents) = inventory(&index)?;
    checkpoint()?;
    // A single segment cannot be reduced by merging; this also makes a zero
    // caller limit safe rather than submitting an empty merge to the backend.
    if segments.len() <= max_segments.max(1) {
        return Ok(Compaction {
            segments_before: segments.len(),
            segments_after: segments.len(),
            documents,
        });
    }
    // A merge writer still constructs Tantivy indexing workers. Supply the
    // pinned adapter's documented SimpleTokenizer + LowerCaser equivalent so
    // their initialization can resolve its named analyzer. No documents are
    // added through this writer, so maintenance never re-tokenizes source text.
    index.tokenizers().register(
        "frankensearch_default",
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(LowerCaser)
            .build(),
    );
    let mut writer = index
        .writer_with_num_threads::<TantivyDocument>(1, MERGE_WRITER_HEAP_BYTES)
        .map_err(|error| failure("writer", error))?;
    writer.set_merge_policy(Box::new(NoMergePolicy));
    // Once a writer exists, every exit joins its workers. In particular, a
    // cancellation after submitting a merge cannot leave a writer racing the
    // caller's staging cleanup or subsequent fallback. A filesystem/merge call
    // is not preemptible; checkpoints preserve cooperative cancellation.
    let merged = (|| {
        checkpoint()?;
        let (locked_segments, locked_documents) = inventory(&index)?;
        if locked_segments != segments || locked_documents != documents {
            return Err(failure(
                "inventory",
                "staged generation changed before writer acquisition",
            ));
        }
        checkpoint()?;
        let _ = writer
            .merge(&segments)
            .wait()
            .map_err(|error| failure("merge", error))?;
        checkpoint()?;
        let _ = writer
            .garbage_collect_files()
            .wait()
            .map_err(|error| failure("garbage collection", error))?;
        Ok(())
    })();
    let joined = writer
        .wait_merging_threads()
        .map_err(|error| failure("worker join", error));
    // A cancelled request wins over a backend interruption, matching the
    // surrounding publisher's existing cancellation/fallback ordering.
    checkpoint()?;
    merged?;
    joined?;
    let (remaining, after_documents) = inventory(&index)?;
    checkpoint()?;
    if remaining.len() > 1 || after_documents != documents {
        return Err(failure(
            "validation",
            "segment or live-document count changed unexpectedly",
        ));
    }
    let report = Compaction {
        segments_before: segments.len(),
        segments_after: remaining.len(),
        documents,
    };
    tracing::debug!(
        target: "ee::index",
        segments_before = report.segments_before,
        segments_after = report.segments_after,
        documents = report.documents,
        "compacted staged lexical tier without re-embedding"
    );
    Ok(report)
}

#[cfg(test)]
#[path = "index_delta_lexical_tests.rs"]
mod tests;
