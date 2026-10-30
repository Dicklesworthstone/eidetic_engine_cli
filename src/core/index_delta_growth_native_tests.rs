//! Growing-import acceptance at the real planner/publisher boundary.
//!
//! Hash embedders produce actual independent fast/quality vectors; counters
//! observe inference without substituting a fake index, source digest or writer.
//! These are publisher tests, not public CASS import or latency benchmarks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use frankensearch::core::generation::EmbeddingIdentityBundleV1;
use frankensearch::core::traits::{ModelCategory, SearchFuture};

use super::super::super::{DOC_DIGESTS_SCHEMA_V2, DocDigests, embedder_identity, plan};
use crate::core::index;
use crate::db::{CreateWorkspaceInput, DbConnection};
use crate::search::{Embedder, EmbedderStack, HashEmbedder, IndexableDocument};

type TestResult = Result<(), String>;

struct CountingHash {
    inner: HashEmbedder,
    inputs: AtomicUsize,
}

impl CountingHash {
    fn new(inner: HashEmbedder) -> Self {
        Self {
            inner,
            inputs: AtomicUsize::new(0),
        }
    }
}

impl Embedder for CountingHash {
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

fn original_documents() -> Vec<IndexableDocument> {
    (0..1024)
        .map(|number| {
            let mut document = IndexableDocument::new(
                format!("doc-{number:05}"),
                format!("lesson {number} historicalmarker"),
            );
            document
                .metadata
                .insert("origin".to_owned(), format!("source-{number}"));
            document
        })
        .collect()
}

fn growing_documents(original: &[IndexableDocument]) -> Vec<IndexableDocument> {
    let mut current = original[16..].to_vec();
    for document in &mut current[..16] {
        document.content.push_str(" repairedmarker");
    }
    for document in &mut current[16..48] {
        document.title = Some("updatedmarker".to_owned());
        document
            .metadata
            .insert("revision".to_owned(), "2".to_owned());
    }
    current.extend((1024..1536).map(|number| {
        let mut document = IndexableDocument::new(
            format!("doc-{number:05}"),
            format!("lesson {number} appendedmarker"),
        );
        document
            .metadata
            .insert("origin".to_owned(), format!("new-{number}"));
        document
    }));
    assert_eq!(current.len(), 1520);
    current
}

fn vector_rows(vector: &frankensearch::VectorIndex) -> Result<BTreeMap<String, Vec<f32>>, String> {
    assert_eq!(vector.wal_record_count(), 0);
    assert_eq!(vector.tombstone_count(), 0);
    (0..vector.record_count())
        .map(|position| {
            Ok((
                vector
                    .doc_id_at(position)
                    .map_err(|error| error.to_string())?
                    .to_owned(),
                vector
                    .vector_at_f32(position)
                    .map_err(|error| error.to_string())?,
            ))
        })
        .collect()
}

fn tier_rows(directory: &Path, quality: bool) -> Result<BTreeMap<String, Vec<f32>>, String> {
    let vector = if quality {
        index::open_quality_vector_index_read_only(directory)
            .map_err(|error| error.detail)?
            .ok_or("expected quality tier")?
    } else {
        index::open_fast_vector_index_read_only(directory).map_err(|error| error.detail)?
    };
    vector_rows(&vector)
}

fn generation_files(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>, String> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|error| error.to_string())? {
            let path = entry.map_err(|error| error.to_string())?.path();
            let kind = std::fs::symlink_metadata(&path)
                .map_err(|error| error.to_string())?
                .file_type();
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() {
                files.insert(
                    path.strip_prefix(root)
                        .map_err(|error| error.to_string())?
                        .to_path_buf(),
                    std::fs::read(&path).map_err(|error| error.to_string())?,
                );
            } else {
                return Err("unexpected generation entry".to_owned());
            }
        }
    }
    Ok(files)
}

#[cfg(feature = "lexical-bm25")]
async fn assert_lexical(
    cx: &asupersync::Cx,
    directory: &Path,
    expected: &[IndexableDocument],
) -> TestResult {
    use std::collections::BTreeSet;

    use crate::search::LexicalRead;

    let reader = crate::search::TantivyIndex::open_read_only(&directory.join("lexical"))
        .map_err(|error| error.to_string())?;
    assert_eq!(
        reader.doc_count().map_err(|error| error.to_string())?,
        expected.len()
    );
    for query in [
        "lesson",
        "historicalmarker",
        "repairedmarker",
        "updatedmarker",
        "appendedmarker",
    ] {
        let wanted: BTreeMap<_, _> = expected
            .iter()
            .filter(|document| {
                document.content.contains(query)
                    || document
                        .title
                        .as_ref()
                        .is_some_and(|title| title.contains(query))
            })
            .map(|document| (document.id.as_str(), document))
            .collect();
        let hits = reader
            .search(cx, query, expected.len() + 1)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(hits.len(), wanted.len(), "{query}");
        assert_eq!(
            hits.iter()
                .map(|hit| hit.doc_id.as_str())
                .collect::<BTreeSet<_>>(),
            wanted.keys().copied().collect::<BTreeSet<_>>(),
            "{query}"
        );
        for hit in hits {
            let document = wanted
                .get(hit.doc_id.as_str())
                .ok_or("unexpected lexical identity")?;
            // Absence matters too: unchanged/new documents must not acquire
            // a neighboring session's refreshed revision metadata.
            for key in ["origin", "revision"] {
                assert_eq!(
                    hit.metadata
                        .as_ref()
                        .and_then(|value| value.get(key))
                        .and_then(serde_json::Value::as_str),
                    document.metadata.get(key).map(String::as_str)
                );
            }
        }
    }
    Ok(())
}

fn publish_case(two_tiers: bool, fail_commit: bool) -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let parent = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    crate::core::run_cli_with_cx(Duration::from_secs(180), |cx| async move {
        let _root = root;
        let live = parent.join("index");
        let reference = parent.join("reference");
        let db = DbConnection::open_memory().map_err(|error| error.to_string())?;
        db.migrate().map_err(|error| error.to_string())?;
        let workspace = "wsp_01234567890123456789012345";
        db.insert_workspace(
            workspace,
            &CreateWorkspaceInput {
                path: parent.to_string_lossy().into_owned(),
                name: Some("growing import publication".to_owned()),
            },
        )
        .map_err(|error| error.to_string())?;
        let lock = index::IndexPublishLockOwner::acquire(&cx, &db, workspace)
            .map_err(|error| error.to_string())?;
        let fast = Arc::new(CountingHash::new(HashEmbedder::default_256()));
        let quality = two_tiers.then(|| Arc::new(CountingHash::new(HashEmbedder::jl_384(17))));
        let stack = EmbedderStack::from_parts(
            fast.clone(),
            quality
                .as_ref()
                .map(|quality| quality.clone() as Arc<dyn Embedder>),
        );
        let original = original_documents();
        index::publish_full_index_generation_with_stack(
            &lock,
            &live,
            stack.clone(),
            original.clone(),
            1,
            index::IndexDocumentCounts::memory_only(1024),
            || Ok(()),
        )
        .await
        .map_err(|error| error.to_string())?;
        let before = generation_files(&live)?;
        let current = growing_documents(&original);
        let identity = embedder_identity(&stack).ok_or("complete producer identity")?;
        let delta = plan(&live, &stack, &current).map_err(|error| error.detail)?;
        assert_eq!(delta.upserts.len(), 560);
        assert_eq!(delta.removals.len(), 16);
        assert_eq!(
            delta
                .vector_upserts(&identity)
                .map_err(|error| error.detail)?
                .len(),
            528
        );
        assert!(delta.vector_upserts("changed producer").is_err());
        let reset = || {
            fast.inputs.store(0, Ordering::SeqCst);
            if let Some(quality) = &quality {
                quality.inputs.store(0, Ordering::SeqCst);
            }
        };
        let assert_inputs = |expected| {
            assert_eq!(fast.inputs.load(Ordering::SeqCst), expected);
            if let Some(quality) = &quality {
                assert_eq!(quality.inputs.load(Ordering::SeqCst), expected);
            }
        };
        let counts = index::IndexDocumentCounts::memory_only(1520);
        if fail_commit {
            reset();
            let mut commit_calls = 0;
            let failed = index::publish_index_generation_preferring_delta(
                &lock,
                &live,
                stack.clone(),
                current.clone(),
                2,
                counts,
                |_| {
                    commit_calls += 1;
                    Err(index::IndexRebuildError::Index(
                        "injected growth commit refusal".to_owned(),
                    ))
                },
            )
            .await;
            assert!(failed.is_err());
            assert_eq!(commit_calls, 1);
            assert_inputs(528);
            assert_eq!(index::validated_index_generation(&live)?, 1);
            assert_eq!(generation_files(&live)?, before);
            #[cfg(feature = "lexical-bm25")]
            assert_lexical(&cx, &live, &original).await?;
        }
        reset();
        let (_, kind) = index::publish_index_generation_preferring_delta(
            &lock,
            &live,
            stack.clone(),
            current.clone(),
            2,
            counts,
            |_| Ok(()),
        )
        .await
        .map_err(|error| error.to_string())?;
        assert!(matches!(kind, index::PublishedGenerationKind::Delta));
        assert_inputs(528);
        assert_eq!(index::validated_index_generation(&live)?, 2);
        index::verify_published_tier_counts(&live, 1520, two_tiers)?;
        let fast_rows = tier_rows(&live, false)?;
        let quality_rows = two_tiers.then(|| tier_rows(&live, true)).transpose()?;
        #[cfg(feature = "lexical-bm25")]
        assert_lexical(&cx, &live, &current).await?;

        // Re-open the published commitments, then publish the unchanged source.
        // Neither a withdrawn ID nor a stale input digest may return on replay.
        let repeated = plan(&live, &stack, &current).map_err(|error| error.detail)?;
        assert!(repeated.upserts.is_empty() && repeated.removals.is_empty());
        reset();
        let (_, kind) = index::publish_index_generation_preferring_delta(
            &lock,
            &live,
            stack.clone(),
            current.clone(),
            3,
            counts,
            |_| Ok(()),
        )
        .await
        .map_err(|error| error.to_string())?;
        assert!(matches!(kind, index::PublishedGenerationKind::Delta));
        assert_inputs(0);
        assert_eq!(index::validated_index_generation(&live)?, 3);
        assert_eq!(tier_rows(&live, false)?, fast_rows);
        if let Some(expected) = &quality_rows {
            assert_eq!(&tier_rows(&live, true)?, expected);
        }
        let builder = index::IndexBuilder::new(&reference).with_embedder_stack(stack);
        #[cfg(feature = "lexical-bm25")]
        let builder = builder.with_tantivy_lexical();
        builder
            .add_documents(current.clone())
            .build(&cx)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(tier_rows(&reference, false)?, fast_rows);
        if let Some(expected) = quality_rows {
            assert_eq!(tier_rows(&reference, true)?, expected);
        }
        #[cfg(feature = "lexical-bm25")]
        assert_lexical(&cx, &reference, &current).await?;
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn growing_import_publishes_only_new_and_changed_inputs() -> TestResult {
    publish_case(false, false)
}

#[test]
fn growing_import_matches_a_rebuild_in_both_tiers_and_lexical_metadata() -> TestResult {
    publish_case(true, false)
}

#[test]
fn growing_import_commit_refusal_restores_all_files_before_retry() -> TestResult {
    publish_case(true, true)
}

#[test]
fn bulk_growth_requires_v3_complete_inputs_and_the_actual_producer() -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let directory = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let stack = index::hash_fallback_embedder_stack();
    let original = original_documents();
    let current = growing_documents(&original);
    let mut legacy = DocDigests::new(&stack, &original);
    legacy.schema = DOC_DIGESTS_SCHEMA_V2.to_owned();
    legacy.embedding_inputs = None;
    legacy
        .write(&directory)
        .map_err(|error| error.to_string())?;
    assert!(plan(&directory, &stack, &current).is_err());
    let valid = DocDigests::new(&stack, &original);
    valid.write(&directory).map_err(|error| error.to_string())?;
    let delta = plan(&directory, &stack, &current).map_err(|error| error.detail)?;
    let identity = embedder_identity(&stack).ok_or("complete producer")?;
    assert_eq!(
        delta
            .vector_upserts(&identity)
            .map_err(|error| error.detail)?
            .len(),
        528
    );
    for incomplete in [false, true] {
        let mut invalid = DocDigests::new(&stack, &original);
        if incomplete {
            let _ = invalid
                .embedding_inputs
                .as_mut()
                .ok_or("v3 inputs")?
                .pop_first();
        } else {
            invalid.embedder = Some("different producer".to_owned());
        }
        invalid
            .write(&directory)
            .map_err(|error| error.to_string())?;
        assert!(plan(&directory, &stack, &current).is_err());
    }
    Ok(())
}

#[path = "index_delta_maintenance_native_tests.rs"]
mod maintenance;
