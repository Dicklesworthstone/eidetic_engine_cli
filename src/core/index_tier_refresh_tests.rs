//! Real native publication, inference counts and retained-reader isolation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

use frankensearch::core::generation::EmbeddingIdentityBundleV1;
use frankensearch::core::traits::{ModelCategory, SearchFuture};

use crate::core::index;
use crate::db::{CreateWorkspaceInput, DbConnection};
use crate::search::{Embedder, EmbedderStack, HashEmbedder, IndexableDocument};

use super::super::DocDigests;
use super::{TierRefreshPlan, producer_pair, stage, stage_with_capacity};

type TestResult = Result<(), String>;
const WORKSPACE: &str = "wsp_01234567890123456789012345";

struct CountingHash {
    inner: HashEmbedder,
    alternate: HashEmbedder,
    inputs: AtomicUsize,
    fault: AtomicU8,
    drifted: AtomicBool,
}

impl CountingHash {
    fn new(dimension: usize, seed: u64) -> Self {
        Self {
            inner: HashEmbedder::new(
                dimension,
                frankensearch::HashAlgorithm::JLProjection { seed },
            ),
            alternate: HashEmbedder::new(
                dimension,
                frankensearch::HashAlgorithm::JLProjection { seed: seed + 1 },
            ),
            inputs: AtomicUsize::new(0),
            fault: AtomicU8::new(0),
            drifted: AtomicBool::new(false),
        }
    }
}

impl Embedder for CountingHash {
    fn bound_batch_is_native(&self) -> bool {
        true
    }

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
        Box::pin(async move {
            let mut vectors = self.inner.embed_batch(cx, texts).await?;
            match self.fault.load(Ordering::SeqCst) {
                1 => {
                    let _ = vectors.pop();
                }
                2 => cx.set_cancel_reason(asupersync::CancelReason::user(
                    "cancel selective inference",
                )),
                3 => self.drifted.store(true, Ordering::SeqCst),
                4 | 5 => {
                    if self.fault.load(Ordering::SeqCst) == 5 {
                        self.drifted.store(true, Ordering::SeqCst);
                    }
                    return Err(frankensearch::SearchError::Cancelled {
                        phase: "native bound batch".to_owned(),
                        reason: "poll quota: selective inference budget exhausted".to_owned(),
                    });
                }
                _ => {}
            }
            Ok(vectors)
        })
    }

    fn identity(&self) -> frankensearch::SearchResult<&EmbeddingIdentityBundleV1> {
        if self.drifted.load(Ordering::SeqCst) {
            self.alternate.identity()
        } else {
            self.inner.identity()
        }
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

fn selected(fast: &Arc<CountingHash>, quality: Option<&Arc<CountingHash>>) -> EmbedderStack {
    EmbedderStack::from_parts(
        fast.clone(),
        quality.map(|quality| quality.clone() as Arc<dyn Embedder>),
    )
}

fn documents(count: usize) -> Vec<IndexableDocument> {
    (0..count)
        .map(|number| {
            let mut document = IndexableDocument::new(
                format!("doc-{number:05}"),
                format!("historical lesson number {number} originalmarker"),
            );
            document
                .metadata
                .insert("origin".to_owned(), format!("session-{number}"));
            document
        })
        .collect()
}

fn database(parent: &Path) -> Result<DbConnection, String> {
    let db = DbConnection::open_memory().map_err(|error| error.to_string())?;
    db.migrate().map_err(|error| error.to_string())?;
    db.insert_workspace(
        WORKSPACE,
        &CreateWorkspaceInput {
            path: parent.to_string_lossy().into_owned(),
            name: Some("selective native tier publication".to_owned()),
        },
    )
    .map_err(|error| error.to_string())?;
    Ok(db)
}

fn counts(documents: &[IndexableDocument]) -> Result<index::IndexDocumentCounts, String> {
    Ok(index::IndexDocumentCounts::memory_only(
        u32::try_from(documents.len()).map_err(|error| error.to_string())?,
    ))
}

fn rows(reader: &frankensearch::VectorIndex) -> Result<BTreeMap<String, Vec<f32>>, String> {
    assert_eq!(reader.wal_record_count(), 0);
    assert_eq!(reader.tombstone_count(), 0);
    (0..reader.record_count())
        .map(|row| {
            Ok((
                reader
                    .doc_id_at(row)
                    .map_err(|error| error.to_string())?
                    .to_owned(),
                reader
                    .vector_at_f32(row)
                    .map_err(|error| error.to_string())?,
            ))
        })
        .collect()
}

fn generation_files(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>, String> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
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
                    std::fs::read(path).map_err(|error| error.to_string())?,
                );
            } else {
                return Err("unexpected native generation entry".to_owned());
            }
        }
    }
    Ok(files)
}

#[cfg(unix)]
fn identity(path: &Path) -> Result<(u64, u64), String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    Ok((metadata.dev(), metadata.ino()))
}

async fn assert_equivalent(
    cx: &asupersync::Cx,
    live: &Path,
    reference: &Path,
    expected: &[IndexableDocument],
    quality: bool,
) -> TestResult {
    let fast = index::open_fast_vector_index_read_only(live).map_err(|error| error.detail)?;
    let reference_fast =
        index::open_fast_vector_index_read_only(reference).map_err(|error| error.detail)?;
    assert_eq!(rows(&fast)?, rows(&reference_fast)?);
    let query = vec![0.1_f32; fast.dimension()];
    assert_eq!(
        fast.search_top_k(&query, 12, None)
            .map_err(|error| error.to_string())?,
        reference_fast
            .search_top_k(&query, 12, None)
            .map_err(|error| error.to_string())?,
    );
    let native = frankensearch::TwoTierIndex::open(live, frankensearch::TwoTierConfig::default())
        .map_err(|error| error.to_string())?;
    assert_eq!(
        native.quality_index_path().is_some(),
        quality,
        "a native pair nonce mismatch must not silently discard a rebuilt quality tier"
    );
    if quality {
        let actual = index::open_quality_vector_index_read_only(live)
            .map_err(|error| error.detail)?
            .ok_or("quality tier")?;
        let reference = index::open_quality_vector_index_read_only(reference)
            .map_err(|error| error.detail)?
            .ok_or("reference quality tier")?;
        assert_eq!(rows(&actual)?, rows(&reference)?);
        assert_eq!(fast.publication_nonce(), actual.publication_nonce());
    }
    #[cfg(feature = "lexical-bm25")]
    {
        use crate::search::LexicalRead;
        let lexical = crate::search::open_lexical_index_read_only(&live.join("lexical"))
            .map_err(|error| error.to_string())?;
        for query in ["originalmarker", "refreshedmarker"] {
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
            let actual = lexical
                .search(cx, query, expected.len() + 1)
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(actual.len(), wanted.len());
            for hit in actual {
                let document = wanted
                    .get(hit.doc_id.as_str())
                    .ok_or("unexpected lexical result")?;
                assert_eq!(
                    hit.metadata
                        .as_ref()
                        .and_then(|value| value.get("origin"))
                        .and_then(serde_json::Value::as_str),
                    document.metadata.get("origin").map(String::as_str)
                );
            }
        }
    }
    #[cfg(not(feature = "lexical-bm25"))]
    let _ = (cx, expected);
    Ok(())
}

#[test]
fn add_drop_and_replace_tiers_preserve_compatible_native_vectors_and_readers() -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let parent = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    crate::core::run_cli_with_cx(Duration::from_secs(180), |cx| async move {
        let _root = root;
        let db = database(&parent)?;
        let lock = index::IndexPublishLockOwner::acquire(&cx, &db, WORKSPACE)
            .map_err(|error| error.to_string())?;
        for (case, old_quality, fast_seed, quality_seed, refresh_metadata) in [
            ("add-quality", None, 17, Some(29), false),
            ("legacy-add-quality", None, 17, Some(29), false),
            ("replace-quality", Some(29), 17, Some(31), true),
            ("drop-quality", Some(29), 17, None, false),
            ("replace-fast", Some(29), 41, Some(29), true),
        ] {
            let live = parent.join(case);
            let reference = parent.join(format!("reference-{case}"));
            let original = documents(65);
            let old_fast = Arc::new(CountingHash::new(64, 17));
            let old_quality = old_quality.map(|seed| Arc::new(CountingHash::new(96, seed)));
            let old_stack = selected(&old_fast, old_quality.as_ref());
            index::publish_full_index_generation_with_stack(
                &lock,
                &live,
                old_stack.clone(),
                original.clone(),
                1,
                counts(&original)?,
                || Ok(()),
            )
            .await
            .map_err(|error| error.to_string())?;
            let fast_name = if case == "legacy-add-quality" {
                std::fs::rename(
                    live.join(index::VECTOR_INDEX_FAST_FILE),
                    live.join(index::VECTOR_INDEX_FALLBACK_FILE),
                )
                .map_err(|error| error.to_string())?;
                index::VECTOR_INDEX_FALLBACK_FILE
            } else {
                index::VECTOR_INDEX_FAST_FILE
            };
            #[cfg(not(unix))]
            let _ = fast_name;
            // A normal no-op remains a delta. It also produces multiply linked
            // sources on Unix, exercising selective reuse across generations.
            old_fast.inputs.store(0, Ordering::SeqCst);
            if let Some(quality) = &old_quality {
                quality.inputs.store(0, Ordering::SeqCst);
            }
            let (_, kind) = index::publish_index_generation_preferring_delta(
                &lock,
                &live,
                old_stack,
                original.clone(),
                2,
                counts(&original)?,
                |_| Ok(()),
            )
            .await
            .map_err(|error| error.to_string())?;
            assert!(matches!(kind, index::PublishedGenerationKind::Delta));
            assert_eq!(old_fast.inputs.load(Ordering::SeqCst), 0);
            assert!(
                old_quality
                    .as_ref()
                    .is_none_or(|quality| quality.inputs.load(Ordering::SeqCst) == 0)
            );
            let original_fast =
                index::open_fast_vector_index_read_only(&live).map_err(|error| error.detail)?;
            let original_quality =
                index::open_quality_vector_index_read_only(&live).map_err(|error| error.detail)?;
            let old_fast_rows = rows(&original_fast)?;
            let old_quality_rows = original_quality.as_ref().map(rows).transpose()?;
            #[cfg(unix)]
            let fast_inode = identity(&live.join(fast_name))?;
            #[cfg(unix)]
            let quality_inode = original_quality
                .as_ref()
                .map(|_| identity(&live.join(index::VECTOR_INDEX_QUALITY_FILE)))
                .transpose()?;
            let fast = Arc::new(CountingHash::new(
                if fast_seed == 17 { 64 } else { 128 },
                fast_seed,
            ));
            let quality = quality_seed.map(|seed| Arc::new(CountingHash::new(96, seed)));
            let stack = selected(&fast, quality.as_ref());
            let mut current = original.clone();
            current.reverse();
            if refresh_metadata {
                current[0].title = Some("refreshedmarker".to_owned());
                current[0]
                    .metadata
                    .insert("origin".to_owned(), "verified-session".to_owned());
            }
            let mut commits = 0;
            let (_, kind) = index::publish_index_generation_preferring_delta(
                &lock,
                &live,
                stack.clone(),
                current.clone(),
                3,
                counts(&current)?,
                |_| {
                    commits += 1;
                    Ok(())
                },
            )
            .await
            .map_err(|error| error.to_string())?;
            assert!(
                matches!(kind, index::PublishedGenerationKind::TierRefresh),
                "{case}"
            );
            assert_eq!(commits, 1);
            assert_eq!(
                fast.inputs.load(Ordering::SeqCst),
                if fast_seed == 17 { 0 } else { current.len() }
            );
            if let Some(quality) = &quality {
                let retained = old_quality.is_some() && quality_seed == Some(29);
                assert_eq!(
                    quality.inputs.load(Ordering::SeqCst),
                    if retained { 0 } else { current.len() }
                );
            }
            #[cfg(unix)]
            {
                assert_eq!(
                    fast_inode == identity(&live.join(fast_name))?,
                    fast_seed == 17
                );
                if quality_seed == Some(29)
                    && let Some(before) = quality_inode
                {
                    assert_eq!(
                        before,
                        identity(&live.join(index::VECTOR_INDEX_QUALITY_FILE))?
                    );
                }
            }
            assert_eq!(rows(&original_fast)?, old_fast_rows);
            assert_eq!(
                original_quality.as_ref().map(rows).transpose()?,
                old_quality_rows
            );
            index::build_index_generation(&cx, &reference, stack, current.clone())
                .await
                .map_err(|error| error.to_string())?;
            assert_equivalent(&cx, &live, &reference, &current, quality.is_some()).await?;
            let metadata: serde_json::Value = serde_json::from_slice(
                &std::fs::read(live.join("meta.json")).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            assert_eq!(metadata["generation"], 3);
        }
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn metadata_beyond_delta_budget_rebuilds_lexical_without_any_vector_inference() -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let parent = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    crate::core::run_cli_with_cx(Duration::from_secs(180), |cx| async move {
        let _root = root;
        let db = database(&parent)?;
        let lock = index::IndexPublishLockOwner::acquire(&cx, &db, WORKSPACE)
            .map_err(|error| error.to_string())?;
        let live = parent.join("index");
        let original = documents(8193);
        let fast = Arc::new(CountingHash::new(16, 17));
        let quality = Arc::new(CountingHash::new(32, 29));
        let stack = selected(&fast, Some(&quality));
        index::publish_full_index_generation_with_stack(
            &lock,
            &live,
            stack.clone(),
            original.clone(),
            1,
            counts(&original)?,
            || Ok(()),
        )
        .await
        .map_err(|error| error.to_string())?;
        let before_fast =
            rows(&index::open_fast_vector_index_read_only(&live).map_err(|error| error.detail)?)?;
        fast.inputs.store(0, Ordering::SeqCst);
        quality.inputs.store(0, Ordering::SeqCst);
        let mut current = original;
        for document in &mut current {
            document.title = Some("refreshedmarker".to_owned());
        }
        assert!(
            super::super::plan(&live, &stack, &current).is_err(),
            "fixture must exceed the real mutation budget"
        );
        let (_, kind) = index::publish_index_generation_preferring_delta(
            &lock,
            &live,
            stack,
            current.clone(),
            2,
            counts(&current)?,
            |_| Ok(()),
        )
        .await
        .map_err(|error| error.to_string())?;
        assert!(matches!(kind, index::PublishedGenerationKind::TierRefresh));
        assert_eq!(fast.inputs.load(Ordering::SeqCst), 0);
        assert_eq!(quality.inputs.load(Ordering::SeqCst), 0);
        assert_eq!(
            rows(&index::open_fast_vector_index_read_only(&live).map_err(|error| error.detail)?)?,
            before_fast
        );
        #[cfg(feature = "lexical-bm25")]
        {
            use crate::search::LexicalRead;
            let lexical = crate::search::open_lexical_index_read_only(&live.join("lexical"))
                .map_err(|error| error.to_string())?;
            assert_eq!(
                lexical
                    .search(&cx, "refreshedmarker", current.len() + 1)
                    .await
                    .map_err(|error| error.to_string())?
                    .len(),
                current.len()
            );
        }
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn incomplete_proof_changed_inputs_and_changed_membership_take_full_rebuild() -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let parent = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    crate::core::run_cli_with_cx(Duration::from_secs(180), |cx| async move {
        let _root = root;
        let db = database(&parent)?;
        let lock = index::IndexPublishLockOwner::acquire(&cx, &db, WORKSPACE)
            .map_err(|error| error.to_string())?;
        for variant in [
            "content",
            "new-id",
            "removed-id",
            "v2",
            "broken-producer",
            "native-id",
            "native-duplicate",
            "native-producer",
            "native-nonce",
            "missing-quality",
            "unexpected-quality",
            "ambiguous-fast",
            "pending-wal",
            "both-producers",
        ] {
            let live = parent.join(variant);
            let original = documents(8);
            let old_fast = Arc::new(CountingHash::new(64, 17));
            let old_quality = Arc::new(CountingHash::new(96, 29));
            let old_stack = selected(&old_fast, Some(&old_quality));
            index::publish_full_index_generation_with_stack(
                &lock,
                &live,
                old_stack.clone(),
                original.clone(),
                1,
                counts(&original)?,
                || Ok(()),
            )
            .await
            .map_err(|error| error.to_string())?;
            let mut current = original.clone();
            match variant {
                "content" => current[0].content.push_str(" content change"),
                "new-id" => current.push(IndexableDocument::new("new-id", "new source input")),
                "removed-id" => {
                    let _ = current.pop();
                }
                "v2" | "broken-producer" => {
                    let mut digests = DocDigests::new(&old_stack, &original);
                    if variant == "v2" {
                        digests.schema = super::super::DOC_DIGESTS_SCHEMA_V2.to_owned();
                        digests.embedding_inputs = None;
                    } else {
                        digests.embedder = Some("fast=unknown;quality=unknown".to_owned());
                    }
                    digests.write(&live).map_err(|error| error.to_string())?;
                }
                "native-id" | "native-duplicate" | "native-producer" | "native-nonce" => {
                    let path = live.join(index::VECTOR_INDEX_FAST_FILE);
                    let old = index::open_fast_vector_index_read_only(&live)
                        .map_err(|error| error.detail)?;
                    let revision = if variant == "native-producer" {
                        "f".repeat(64)
                    } else {
                        old.embedder_revision().to_owned()
                    };
                    let nonce = old.publication_nonce() + u16::from(variant == "native-nonce");
                    let mut values = rows(&old)?;
                    drop(old);
                    if variant == "native-id" {
                        let vector = values.remove("doc-00000").ok_or("native source row")?;
                        values.insert("foreign-document".to_owned(), vector);
                    }
                    let mut writer = frankensearch::VectorIndex::create_with_revision(
                        &path,
                        "native-fixture",
                        &revision,
                        64,
                        frankensearch::index::Quantization::F16,
                    )
                    .map_err(|error| error.to_string())?
                    .with_publication_nonce(nonce);
                    for (id, vector) in values {
                        let id = if variant == "native-duplicate" && id == "doc-00007" {
                            "doc-00000".to_owned()
                        } else {
                            id
                        };
                        writer
                            .write_record(&id, &vector)
                            .map_err(|error| error.to_string())?;
                    }
                    writer.finish().map_err(|error| error.to_string())?;
                }
                "missing-quality" => {
                    std::fs::rename(
                        live.join(index::VECTOR_INDEX_QUALITY_FILE),
                        parent.join("detached-quality.idx"),
                    )
                    .map_err(|error| error.to_string())?;
                }
                "unexpected-quality" => {
                    DocDigests::new(&selected(&old_fast, None), &original)
                        .write(&live)
                        .map_err(|error| error.to_string())?;
                }
                "ambiguous-fast" => {
                    std::fs::hard_link(
                        live.join(index::VECTOR_INDEX_FAST_FILE),
                        live.join(index::VECTOR_INDEX_FALLBACK_FILE),
                    )
                    .map_err(|error| error.to_string())?;
                }
                "pending-wal" => {
                    let mut writer =
                        index::open_fast_vector_index(&live).map_err(|error| error.detail)?;
                    writer
                        .append("unpublished", &vec![0.1; 64])
                        .map_err(|error| error.to_string())?;
                }
                _ => {}
            }
            let fast = Arc::new(CountingHash::new(
                64,
                if variant == "both-producers" { 41 } else { 17 },
            ));
            let quality = Arc::new(CountingHash::new(96, 31));
            let stack = selected(&fast, Some(&quality));
            let (_, kind) = index::publish_index_generation_preferring_delta(
                &lock,
                &live,
                stack,
                current.clone(),
                2,
                counts(&current)?,
                |_| Ok(()),
            )
            .await
            .map_err(|error| error.to_string())?;
            assert!(
                matches!(kind, index::PublishedGenerationKind::Full { .. }),
                "{variant}"
            );
            assert_eq!(
                fast.inputs.load(Ordering::SeqCst),
                current.len(),
                "{variant}"
            );
            assert_eq!(
                quality.inputs.load(Ordering::SeqCst),
                current.len(),
                "{variant}"
            );
        }
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn failed_inference_producer_drift_and_cancelled_refresh_preserve_source_bytes() -> TestResult {
    for fault in [1, 3, 2, 4, 5] {
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let parent = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        crate::core::run_cli_with_cx(Duration::from_secs(90), |cx| async move {
            let _root = root;
            let db = database(&parent)?;
            let lock = index::IndexPublishLockOwner::acquire(&cx, &db, WORKSPACE)
                .map_err(|error| error.to_string())?;
            let live = parent.join("index");
            let original = documents(65);
            let fast = Arc::new(CountingHash::new(64, 17));
            let old_quality = Arc::new(CountingHash::new(96, 29));
            index::publish_full_index_generation_with_stack(
                &lock,
                &live,
                selected(&fast, Some(&old_quality)),
                original.clone(),
                1,
                counts(&original)?,
                || Ok(()),
            )
            .await
            .map_err(|error| error.to_string())?;
            let before = generation_files(&live)?;
            let reader =
                index::open_fast_vector_index_read_only(&live).map_err(|error| error.detail)?;
            let retained = rows(&reader)?;
            fast.inputs.store(0, Ordering::SeqCst);
            let quality = Arc::new(CountingHash::new(96, 31));
            quality.fault.store(fault, Ordering::SeqCst);
            let result = stage(
                &lock,
                &live,
                &selected(&fast, Some(&quality)),
                &original,
                2,
                counts(&original)?,
            )
            .await;
            if matches!(fault, 2 | 4 | 5) {
                assert!(matches!(result, Err(index::DeltaStageError::Cancelled(_))));
            } else {
                assert!(matches!(result, Err(index::DeltaStageError::Refused(_))));
            }
            assert!(quality.inputs.load(Ordering::SeqCst) > 0);
            assert_eq!(fast.inputs.load(Ordering::SeqCst), 0);
            assert_eq!(generation_files(&live)?, before);
            assert_eq!(rows(&reader)?, retained);
            Ok(())
        })
        .map_err(|error| error.to_string())??;
    }
    Ok(())
}

#[test]
fn cancelled_publisher_does_not_retry_full_and_commit_failure_rolls_back() -> TestResult {
    for fault in [0, 2, 4, 5] {
        let cancel = fault != 0;
        let root = tempfile::tempdir().map_err(|error| error.to_string())?;
        let parent = root
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        crate::core::run_cli_with_cx(Duration::from_secs(90), |cx| async move {
            let _root = root;
            let db = database(&parent)?;
            let lock = index::IndexPublishLockOwner::acquire(&cx, &db, WORKSPACE)
                .map_err(|error| error.to_string())?;
            let live = parent.join("index");
            let original = documents(65);
            let fast = Arc::new(CountingHash::new(64, 17));
            let old_quality = Arc::new(CountingHash::new(96, 29));
            index::publish_full_index_generation_with_stack(
                &lock,
                &live,
                selected(&fast, Some(&old_quality)),
                original.clone(),
                1,
                counts(&original)?,
                || Ok(()),
            )
            .await
            .map_err(|error| error.to_string())?;
            let before = generation_files(&live)?;
            fast.inputs.store(0, Ordering::SeqCst);
            let quality = Arc::new(CountingHash::new(96, 31));
            if cancel {
                quality.fault.store(fault, Ordering::SeqCst);
            }
            let mut commits = 0;
            let result = index::publish_index_generation_preferring_delta(
                &lock,
                &live,
                selected(&fast, Some(&quality)),
                original.clone(),
                2,
                counts(&original)?,
                |_| {
                    commits += 1;
                    Err(index::IndexRebuildError::Index(
                        "deliberate commit refusal".to_owned(),
                    ))
                },
            )
            .await;
            if cancel {
                assert!(matches!(
                    result,
                    Err(index::IndexRebuildError::Cancelled(_))
                ));
                assert_eq!(commits, 0);
                assert_eq!(quality.inputs.load(Ordering::SeqCst), 32);
            } else {
                assert!(result.is_err());
                assert_eq!(commits, 1);
                assert_eq!(quality.inputs.load(Ordering::SeqCst), original.len());
            }
            assert_eq!(
                fast.inputs.load(Ordering::SeqCst),
                0,
                "a cancelled selective build must not fall through to a complete rebuild"
            );
            assert_eq!(generation_files(&live)?, before);
            Ok(())
        })
        .map_err(|error| error.to_string())??;
    }
    Ok(())
}

#[test]
fn capacity_loss_before_and_after_copy_prevents_inference_and_preserves_source() -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let parent = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    crate::core::run_cli_with_cx(Duration::from_secs(90), |cx| async move {
        let _root = root;
        let db = database(&parent)?;
        let lock = index::IndexPublishLockOwner::acquire(&cx, &db, WORKSPACE)
            .map_err(|error| error.to_string())?;
        let live = parent.join("index");
        let original = documents(8);
        let fast = Arc::new(CountingHash::new(64, 17));
        let old_quality = Arc::new(CountingHash::new(96, 29));
        index::publish_full_index_generation_with_stack(
            &lock,
            &live,
            selected(&fast, Some(&old_quality)),
            original.clone(),
            1,
            counts(&original)?,
            || Ok(()),
        )
        .await
        .map_err(|error| error.to_string())?;
        let before = generation_files(&live)?;
        let quality = Arc::new(CountingHash::new(96, 31));
        fast.inputs.store(0, Ordering::SeqCst);
        for fail_at in [1, 2] {
            let mut calls = 0;
            let result = stage_with_capacity(
                &lock,
                &live,
                &selected(&fast, Some(&quality)),
                &original,
                2,
                counts(&original)?,
                |_| {
                    calls += 1;
                    Ok(index::storage::Capacity {
                        available_bytes: if calls == fail_at { 0 } else { u64::MAX },
                        available_inodes: Some(u64::MAX),
                    })
                },
            )
            .await;
            assert!(matches!(result, Err(index::DeltaStageError::Refused(_))));
            assert_eq!(calls, fail_at);
            assert_eq!(quality.inputs.load(Ordering::SeqCst), 0);
            assert_eq!(fast.inputs.load(Ordering::SeqCst), 0);
            assert_eq!(generation_files(&live)?, before);
        }
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn tier_plan_rejects_noop_duplicate_sources_and_noncanonical_producer_proof() -> TestResult {
    assert!(producer_pair("fast=unknown;quality=").is_none());
    assert!(
        producer_pair(&format!(
            "fast={};quality={};extra=1",
            "a".repeat(64),
            "b".repeat(64)
        ))
        .is_none()
    );
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
        let original = documents(8);
        let fast = Arc::new(CountingHash::new(64, 17));
        let stack = selected(&fast, None);
        DocDigests::new(&stack, &original)
            .write(root.path())
            .map_err(|error| error.to_string())?;
        assert!(TierRefreshPlan::read(&cx, root.path(), &stack, &original).is_err());
        let quality = Arc::new(CountingHash::new(96, 29));
        let stack = selected(&fast, Some(&quality));
        let mut duplicate = original.clone();
        duplicate[1] = duplicate[0].clone();
        assert!(TierRefreshPlan::read(&cx, root.path(), &stack, &duplicate).is_err());
        let mut changed_input = original;
        changed_input[0].content.push(' ');
        assert!(TierRefreshPlan::read(&cx, root.path(), &stack, &changed_input).is_err());
        Ok(())
    })
    .map_err(|error| error.to_string())?
}
