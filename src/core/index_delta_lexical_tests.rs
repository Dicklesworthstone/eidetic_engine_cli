//! Native lexical maintenance and the production delta path, without mock tiers.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use frankensearch::core::generation::EmbeddingIdentityBundleV1;
use frankensearch::core::traits::{ModelCategory, SearchFuture};
use frankensearch::lexical_tantivy::tantivy_crate::{
    Term,
    tokenizer::{LowerCaser, SimpleTokenizer, TextAnalyzer},
};

use super::super::{DocDigests, MAX_LEXICAL_SEGMENTS, apply, copy_generation, plan};
use super::*;
use crate::search::{
    Embedder, EmbedderStack, HashEmbedder, IndexBuilder, IndexableDocument, LexicalRead,
};

type TestResult<T = ()> = Result<T, String>;

fn files(root: &Path) -> TestResult<BTreeMap<std::path::PathBuf, Vec<u8>>> {
    let mut result = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            if entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_dir()
            {
                pending.push(entry.path());
            } else {
                result.insert(
                    entry
                        .path()
                        .strip_prefix(root)
                        .map_err(|error| error.to_string())?
                        .to_path_buf(),
                    std::fs::read(entry.path()).map_err(|error| error.to_string())?,
                );
            }
        }
    }
    Ok(result)
}

/// Keep the actual Frankensearch schema but deliberately suppress automatic
/// merging while committing one real upsert per segment. No fabricated segment
/// metadata or dummy `.idx` files stand in for a fragmented searchable index.
fn fragment(root: &Path, documents: &[IndexableDocument]) -> TestResult<Index> {
    let directory = root.join(LEXICAL_INDEX_SUBDIR);
    if !directory.join("meta.json").exists() {
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        drop(crate::search::TantivyIndex::create(&directory).map_err(|error| error.to_string())?);
    }
    let index = Index::open_in_dir(&directory).map_err(|error| error.to_string())?;
    // The pinned adapter documents equivalence to this native analyzer. The
    // production merge registers the same name but never tokenizes documents.
    index.tokenizers().register(
        "frankensearch_default",
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(LowerCaser)
            .build(),
    );
    let schema = index.schema();
    let id = schema.get_field("id").map_err(|error| error.to_string())?;
    let content = schema
        .get_field("content")
        .map_err(|error| error.to_string())?;
    let title = schema
        .get_field("title")
        .map_err(|error| error.to_string())?;
    let metadata = schema
        .get_field("metadata_json")
        .map_err(|error| error.to_string())?;
    let ordinal = schema.get_field("ord").ok();
    let mut writer = index
        .writer_with_num_threads::<TantivyDocument>(1, MERGE_WRITER_HEAP_BYTES)
        .map_err(|error| error.to_string())?;
    writer.set_merge_policy(Box::new(NoMergePolicy));
    for (position, document) in documents.iter().enumerate() {
        writer.delete_term(Term::from_field_text(id, &document.id));
        let mut stored = TantivyDocument::new();
        stored.add_text(id, &document.id);
        stored.add_text(content, &document.content);
        stored.add_text(title, document.title.as_deref().unwrap_or(""));
        stored.add_text(
            metadata,
            serde_json::to_string(&document.metadata).map_err(|error| error.to_string())?,
        );
        if let Some(ordinal) = ordinal {
            stored.add_u64(ordinal, (documents.len() + position) as u64);
        }
        writer
            .add_document(stored)
            .map_err(|error| error.to_string())?;
        writer.commit().map_err(|error| error.to_string())?;
    }
    writer
        .wait_merging_threads()
        .map_err(|error| error.to_string())?;
    Ok(index)
}

fn documents(count: usize) -> Vec<IndexableDocument> {
    (0..count)
        .map(|position| {
            IndexableDocument::new(
                format!("mem_{position:03}"),
                format!("original quartzmarker lesson {position}"),
            )
            .with_title("priortitlemarker")
        })
        .collect()
}

#[test]
fn maintenance_reads_active_segments_and_takes_no_writer_below_the_threshold() -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let root = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let index = fragment(&root, &documents(3))?;
    let active = inventory(&index).map_err(|error| error.detail)?.0.len();
    assert_eq!(active, 3);
    // Even many unrelated/retired-looking files are not active segments.
    for number in 0..64 {
        std::fs::write(
            root.join(LEXICAL_INDEX_SUBDIR)
                .join(format!("orphan-{number}.idx")),
            b"not a segment",
        )
        .map_err(|error| error.to_string())?;
    }
    let writer = index
        .writer_with_num_threads::<TantivyDocument>(1, MERGE_WRITER_HEAP_BYTES)
        .map_err(|error| error.to_string())?;
    let before = files(&root)?;
    let report = compact_with_checkpoint(&root, MAX_LEXICAL_SEGMENTS, || Ok(()))
        .map_err(|error| error.detail)?;
    assert_eq!(
        report,
        Compaction {
            segments_before: 3,
            segments_after: 3,
            documents: 3,
        }
    );
    assert_eq!(files(&root)?, before);
    writer
        .wait_merging_threads()
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[test]
fn real_fragmentation_merges_without_changing_live_documents_and_is_idempotent() -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let root = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let count = MAX_LEXICAL_SEGMENTS + 1;
    let index = fragment(&root, &documents(count))?;
    assert_eq!(
        inventory(&index).map_err(|error| error.detail)?.0.len(),
        count
    );
    let report = compact_with_checkpoint(&root, MAX_LEXICAL_SEGMENTS, || Ok(()))
        .map_err(|error| error.detail)?;
    assert_eq!(report.segments_before, count);
    assert_eq!(report.segments_after, 1);
    assert_eq!(report.documents, count as u64);
    let before = files(&root)?;
    let again = compact_with_checkpoint(&root, MAX_LEXICAL_SEGMENTS, || Ok(()))
        .map_err(|error| error.detail)?;
    assert_eq!(again.segments_before, again.segments_after);
    assert_eq!(files(&root)?, before);
    Ok(())
}

#[test]
fn missing_or_corrupt_metadata_is_not_a_zero_segment_success() -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let root = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    assert!(compact_with_checkpoint(&root, MAX_LEXICAL_SEGMENTS, || Ok(())).is_err());
    assert!(!root.join(LEXICAL_INDEX_SUBDIR).exists());
    std::fs::create_dir(root.join(LEXICAL_INDEX_SUBDIR)).map_err(|error| error.to_string())?;
    std::fs::write(root.join(LEXICAL_INDEX_SUBDIR).join("meta.json"), b"{")
        .map_err(|error| error.to_string())?;
    let before = files(&root)?;
    assert!(compact_with_checkpoint(&root, MAX_LEXICAL_SEGMENTS, || Ok(())).is_err());
    assert_eq!(files(&root)?, before);
    Ok(())
}

#[test]
fn cancellation_after_merge_joins_workers_and_releases_the_staged_writer() -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let root = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let index = fragment(&root, &documents(MAX_LEXICAL_SEGMENTS + 1))?;
    let before = files(&root)?;
    let stopped = compact_with_checkpoint(&root, MAX_LEXICAL_SEGMENTS, || {
        Err(failure("test", "cancelled"))
    });
    assert!(stopped.is_err());
    assert_eq!(files(&root)?, before);
    let mut observed_merge = false;
    let stopped = compact_with_checkpoint(&root, MAX_LEXICAL_SEGMENTS, || {
        if inventory(&index)?.0.len() == 1 {
            observed_merge = true;
            return Err(failure("test", "cancelled after merge"));
        }
        Ok(())
    });
    assert!(observed_merge, "the fault must occur after a real merge");
    assert!(stopped.is_err());
    // Acquiring the real writer proves no detached merge still owns the lock.
    index
        .writer_with_num_threads::<TantivyDocument>(1, MERGE_WRITER_HEAP_BYTES)
        .map_err(|error| error.to_string())?
        .wait_merging_threads()
        .map_err(|error| error.to_string())?;
    Ok(())
}

struct ObservedHash {
    inner: HashEmbedder,
    inputs: AtomicUsize,
}

impl Embedder for ObservedHash {
    fn embed<'a>(&'a self, cx: &'a asupersync::Cx, text: &'a str) -> SearchFuture<'a, Vec<f32>> {
        self.inputs.fetch_add(1, Ordering::SeqCst);
        self.inner.embed(cx, text)
    }

    fn embed_batch<'a>(
        &'a self,
        cx: &'a asupersync::Cx,
        texts: &'a [&'a str],
    ) -> SearchFuture<'a, Vec<Vec<f32>>> {
        self.inputs.fetch_add(texts.len(), Ordering::SeqCst);
        self.inner.embed_batch(cx, texts)
    }

    fn identity(&self) -> frankensearch::SearchResult<&EmbeddingIdentityBundleV1> {
        self.inner.identity()
    }
    fn dimension(&self) -> usize {
        self.inner.dimension()
    }
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn model_name(&self) -> &str {
        self.inner.model_name()
    }
    fn is_semantic(&self) -> bool {
        self.inner.is_semantic()
    }
    fn category(&self) -> ModelCategory {
        self.inner.category()
    }
}

fn finalized_vectors(root: &Path) -> TestResult<BTreeMap<String, Vec<f32>>> {
    let index =
        crate::core::index::open_fast_vector_index_read_only(root).map_err(|error| error.detail)?;
    assert_eq!(index.wal_record_count(), 0);
    assert_eq!(index.tombstone_count(), 0);
    (0..index.record_count())
        .map(|position| {
            Ok((
                index
                    .doc_id_at(position)
                    .map_err(|error| error.to_string())?
                    .to_owned(),
                index
                    .vector_at_f32(position)
                    .map_err(|error| error.to_string())?,
            ))
        })
        .collect()
}

fn native_delta_case(change_body: bool) -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let root = temp
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    crate::core::run_cli_with_cx(Duration::from_secs(120), |cx| async move {
        let live = root.join("live");
        let staged = root.join("staged");
        let rebuilt = root.join("rebuilt");
        let observed = Arc::new(ObservedHash {
            inner: HashEmbedder::default_256(),
            inputs: AtomicUsize::new(0),
        });
        let stack = EmbedderStack::from_parts(observed.clone(), None);
        let original = documents(64);
        IndexBuilder::new(&live)
            .with_embedder_stack(stack.clone())
            .with_tantivy_lexical()
            .add_documents(original.clone())
            .build(&cx)
            .await
            .map_err(|error| error.to_string())?;
        let lexical = fragment(&live, &original)?;
        assert!(inventory(&lexical).map_err(|error| error.detail)?.0.len() > MAX_LEXICAL_SEGMENTS);
        DocDigests::new(&stack, &original)
            .write(&live)
            .map_err(|error| error.to_string())?;
        let before = files(&live)?;
        let mut current = original.clone();
        current[0].title = Some("updatedtitlemarker".to_owned());
        current[0]
            .metadata
            .insert("revision".to_owned(), "2".to_owned());
        if change_body {
            current[1].content = "changedquartzmarker replacement lesson".to_owned();
        }
        let delta = plan(&live, &stack, &current).map_err(|error| error.detail)?;
        assert_eq!(delta.upserts.len(), 1 + usize::from(change_body));
        std::fs::create_dir(&staged).map_err(|error| error.to_string())?;
        copy_generation(&live, &staged).map_err(|error| error.to_string())?;
        observed.inputs.store(0, Ordering::SeqCst);
        apply(&cx, &staged, &stack, &delta)
            .await
            .map_err(|error| error.detail)?;
        assert_eq!(
            observed.inputs.load(Ordering::SeqCst),
            usize::from(change_body)
        );
        assert_eq!(
            files(&live)?,
            before,
            "maintenance must never mutate the live generation"
        );
        if !change_body {
            assert_eq!(
                std::fs::read(staged.join("vector.fast.idx")).map_err(|error| error.to_string())?,
                before[Path::new("vector.fast.idx")]
            );
        }
        let staged_index = Index::open_in_dir(staged.join(LEXICAL_INDEX_SUBDIR))
            .map_err(|error| error.to_string())?;
        assert!(
            inventory(&staged_index)
                .map_err(|error| error.detail)?
                .0
                .len()
                <= MAX_LEXICAL_SEGMENTS
        );
        IndexBuilder::new(&rebuilt)
            .with_embedder_stack(stack)
            .with_tantivy_lexical()
            .add_documents(current)
            .build(&cx)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(finalized_vectors(&staged)?, finalized_vectors(&rebuilt)?);
        for query in [
            "updatedtitlemarker",
            "changedquartzmarker",
            "priortitlemarker",
            "absentplanetmarker",
        ] {
            let mut results = Vec::new();
            for directory in [&staged, &rebuilt] {
                let reader = crate::search::TantivyIndex::open_read_only(
                    &directory.join(LEXICAL_INDEX_SUBDIR),
                )
                .map_err(|error| error.to_string())?;
                let hits = reader
                    .search(&cx, query, 128)
                    .await
                    .map_err(|error| error.to_string())?;
                let hits = hits
                    .into_iter()
                    .map(|hit| (hit.doc_id, hit.metadata))
                    .collect::<BTreeMap<_, _>>();
                if query == "updatedtitlemarker" {
                    assert_eq!(hits.len(), 1);
                    assert_eq!(
                        hits.get("mem_000")
                            .and_then(Option::as_ref)
                            .and_then(|value| value.get("revision"))
                            .and_then(serde_json::Value::as_str),
                        Some("2")
                    );
                }
                if query == "absentplanetmarker" {
                    assert!(hits.is_empty());
                }
                results.push(hits);
            }
            assert_eq!(results[0], results[1], "query {query}");
        }
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn fragmented_metadata_only_delta_does_not_reembed_the_corpus() -> TestResult {
    native_delta_case(false)
}

#[test]
fn fragmented_mixed_delta_embeds_only_the_changed_input_and_matches_full_rebuild() -> TestResult {
    native_delta_case(true)
}

#[test]
fn empty_removal_batch_checks_cancellation_without_opening_a_tier() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let missing = temp.path().join("absent-stage");
    let mut checkpoints = 0;
    assert_eq!(
        delete_with_checkpoint(&missing, &[], || {
            checkpoints += 1;
            Ok(())
        })
        .map_err(|error| error.detail)?,
        0
    );
    assert_eq!(checkpoints, 1);
    assert!(!missing.exists());
    assert!(
        delete_with_checkpoint(&missing, &[], || Err(failure("test", "cancelled"))).is_err()
    );
    assert!(!missing.exists());
    Ok(())
}

async fn lexical_ids(cx: &asupersync::Cx, root: &Path, query: &str) -> TestResult<Vec<String>> {
    let reader = crate::search::TantivyIndex::open_read_only(&root.join(LEXICAL_INDEX_SUBDIR))
        .map_err(|error| error.to_string())?;
    let mut ids = reader
        .search(cx, query, 256)
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|hit| hit.doc_id.as_str().to_owned())
        .collect::<Vec<_>>();
    ids.sort();
    Ok(ids)
}

#[test]
fn batched_removals_match_individual_deletes_with_less_native_commit_work() -> TestResult {
    use crate::search::LexicalWrite;

    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let root = temp
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    crate::core::run_cli_with_cx(Duration::from_secs(120), |cx| async move {
        let live = root.join("live");
        let staged = root.join("staged");
        let individual = root.join("individual");
        let _ = fragment(&live, &documents(24))?;
        let pinned =
            crate::search::TantivyIndex::open_read_only(&live.join(LEXICAL_INDEX_SUBDIR))
                .map_err(|error| error.to_string())?;
        let before = files(&live)?;
        for copy in [&staged, &individual] {
            std::fs::create_dir(copy).map_err(|error| error.to_string())?;
            copy_generation(&live, copy).map_err(|error| error.to_string())?;
        }
        let mut removals = (0..16)
            .map(|number| format!("mem_{number:03}"))
            .collect::<Vec<_>>();
        removals.extend([
            "missing-id".to_owned(),
            "mem_000".to_owned(),
            // A query-looking ID is a literal term, never a Boolean deletion.
            "mem_016 OR mem_017".to_owned(),
        ]);
        let staged_index = Index::open_in_dir(staged.join(LEXICAL_INDEX_SUBDIR))
            .map_err(|error| error.to_string())?;
        let before_opstamp = staged_index
            .load_metas()
            .map_err(|error| error.to_string())?
            .opstamp;
        assert_eq!(
            delete_with_checkpoint(&staged, &removals, || Ok(())).map_err(|error| error.detail)?,
            16
        );
        let batch_opstamp = staged_index
            .load_metas()
            .map_err(|error| error.to_string())?
            .opstamp;
        let old = crate::search::TantivyIndex::open(&individual.join(LEXICAL_INDEX_SUBDIR))
            .map_err(|error| error.to_string())?;
        for id in &removals {
            old.delete_document(&cx, id)
                .await
                .map_err(|error| error.to_string())?;
        }
        old.commit(&cx).await.map_err(|error| error.to_string())?;
        let individual_opstamp = old
            .index_handle()
            .load_metas()
            .map_err(|error| error.to_string())?
            .opstamp;
        drop(old);
        // Compare native metadata from both actual write paths, rather than a
        // counter reported by the new helper itself. No exact stamp increment
        // is assumed: redundant commits must measurably advance it further.
        assert!(batch_opstamp > before_opstamp);
        assert!(batch_opstamp < individual_opstamp);
        let expected = (16..24)
            .map(|number| format!("mem_{number:03}"))
            .collect::<Vec<_>>();
        assert_eq!(
            lexical_ids(&cx, &staged, "quartzmarker").await?,
            expected
        );
        assert_eq!(
            lexical_ids(&cx, &individual, "quartzmarker").await?,
            expected
        );
        assert_eq!(inventory(&staged_index).map_err(|error| error.detail)?.1, 8);
        assert_eq!(pinned.doc_count().map_err(|error| error.to_string())?, 24);
        assert_eq!(
            pinned
                .search(&cx, "quartzmarker", 64)
                .await
                .map_err(|error| error.to_string())?
                .len(),
            24
        );
        assert_eq!(files(&live)?, before, "only private staging may change");
        assert_eq!(
            delete_with_checkpoint(&staged, &removals, || Ok(())).map_err(|error| error.detail)?,
            0,
            "an identical retry must not delete additional documents"
        );
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn cancellation_before_and_after_delete_commit_releases_the_writer() -> TestResult {
    for after_commit in [false, true] {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let index = fragment(&root, &documents(8))?;
        let removals = (0..4)
            .map(|number| format!("mem_{number:03}"))
            .collect::<Vec<_>>();
        let mut calls = 0;
        let mut reached = false;
        let result = delete_with_checkpoint(&root, &removals, || {
            calls += 1;
            // At call 7 the first two terms have been queued but not committed.
            // The other arm waits for native metadata to expose the real commit.
            if (!after_commit && calls == 7) || (after_commit && inventory(&index)?.1 == 4) {
                reached = true;
                return Err(failure("test", "cancelled removal batch"));
            }
            Ok(())
        });
        assert!(reached, "the intended cancellation point must execute");
        assert!(result.is_err());
        assert_eq!(
            inventory(&index).map_err(|error| error.detail)?.1,
            if after_commit { 4 } else { 8 }
        );
        // Successful acquisition proves a failed attempt did not leave a
        // worker owning staging while the publisher may discard or reuse it.
        maintenance_writer(&index)
            .map_err(|error| error.detail)?
            .wait_merging_threads()
            .map_err(|error| error.to_string())?;
        assert_eq!(
            delete_with_checkpoint(&root, &removals, || Ok(())).map_err(|error| error.detail)?,
            if after_commit { 0 } else { 4 }
        );
    }
    Ok(())
}

#[test]
fn removal_batch_refuses_a_busy_writer_and_does_not_swallow_corruption() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let root = temp
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let index = fragment(&root, &documents(3))?;
    let removals = vec!["mem_000".to_owned()];
    let writer = maintenance_writer(&index).map_err(|error| error.detail)?;
    assert!(delete_with_checkpoint(&root, &removals, || Ok(())).is_err());
    assert_eq!(inventory(&index).map_err(|error| error.detail)?.1, 3);
    writer
        .wait_merging_threads()
        .map_err(|error| error.to_string())?;
    let metadata = root.join(LEXICAL_INDEX_SUBDIR).join("meta.json");
    std::fs::write(&metadata, b"{").map_err(|error| error.to_string())?;
    let before = files(&root)?;
    assert!(delete_with_checkpoint(&root, &removals, || Ok(())).is_err());
    assert_eq!(files(&root)?, before);
    Ok(())
}

#[cfg(unix)]
#[test]
fn removal_batch_refuses_redirected_staging_without_touching_the_target() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let root = temp
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let live = root.join("live");
    let _ = fragment(&live, &documents(3))?;
    let redirected = root.join("redirected");
    std::fs::create_dir(&redirected).map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink(
        live.join(LEXICAL_INDEX_SUBDIR),
        redirected.join(LEXICAL_INDEX_SUBDIR),
    )
    .map_err(|error| error.to_string())?;
    let before = files(&live)?;
    assert!(
        delete_with_checkpoint(&redirected, &["mem_000".to_owned()], || Ok(())).is_err()
    );
    assert_eq!(files(&live)?, before);
    Ok(())
}

#[test]
fn bulk_withdrawal_keeps_vector_reuse_and_matches_a_fresh_rebuild() -> TestResult {
    for (remove_count, change_body) in [(48, false), (48, true), (64, false)] {
        let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = temp
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        crate::core::run_cli_with_cx(Duration::from_secs(120), |cx| async move {
            let live = root.join("live");
            let staged = root.join("staged");
            let rebuilt = root.join("rebuilt");
            let observed = Arc::new(ObservedHash {
                inner: HashEmbedder::default_256(),
                inputs: AtomicUsize::new(0),
            });
            let stack = EmbedderStack::from_parts(observed.clone(), None);
            let original = documents(64);
            IndexBuilder::new(&live)
                .with_embedder_stack(stack.clone())
                .with_tantivy_lexical()
                .add_documents(original.clone())
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            DocDigests::new(&stack, &original)
                .write(&live)
                .map_err(|error| error.to_string())?;
            let before = files(&live)?;
            let mut current = original[remove_count..].to_vec();
            if change_body {
                current[0].content = "changedquartzmarker replacement lesson".to_owned();
            }
            let delta = plan(&live, &stack, &current).map_err(|error| error.detail)?;
            assert_eq!(delta.removals.len(), remove_count);
            assert_eq!(delta.upserts.len(), usize::from(change_body));
            std::fs::create_dir(&staged).map_err(|error| error.to_string())?;
            copy_generation(&live, &staged).map_err(|error| error.to_string())?;
            observed.inputs.store(0, Ordering::SeqCst);
            apply(&cx, &staged, &stack, &delta)
                .await
                .map_err(|error| error.detail)?;
            assert_eq!(
                observed.inputs.load(Ordering::SeqCst),
                usize::from(change_body)
            );
            assert_eq!(files(&live)?, before);
            // IndexBuilder deliberately rejects an empty input. Complete
            // withdrawal must still leave real, readable empty tiers; check
            // those directly rather than making an invalid reference build.
            let empty = current.is_empty();
            if empty {
                assert!(finalized_vectors(&staged)?.is_empty());
            } else {
                IndexBuilder::new(&rebuilt)
                    .with_embedder_stack(stack)
                    .with_tantivy_lexical()
                    .add_documents(current)
                    .build(&cx)
                    .await
                    .map_err(|error| error.to_string())?;
                assert_eq!(finalized_vectors(&staged)?, finalized_vectors(&rebuilt)?);
            }
            for query in [
                "quartzmarker",
                "changedquartzmarker",
                "priortitlemarker",
                "absentplanetmarker",
            ] {
                let actual = lexical_ids(&cx, &staged, query).await?;
                if empty {
                    assert!(actual.is_empty(), "withdrawn corpus must answer empty");
                } else {
                    assert_eq!(actual, lexical_ids(&cx, &rebuilt, query).await?);
                }
            }
            let reader = crate::search::TantivyIndex::open_read_only(
                &staged.join(LEXICAL_INDEX_SUBDIR),
            )
            .map_err(|error| error.to_string())?;
            assert_eq!(
                reader.doc_count().map_err(|error| error.to_string())?,
                64 - remove_count
            );
            Ok(())
        })
        .map_err(|error| error.to_string())??;
    }
    Ok(())
}
