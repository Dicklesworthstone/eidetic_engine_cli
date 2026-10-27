//! Bulk maintenance through the actual publisher, not just a planner counter.
//!
//! Reuse the existing hash/native-backend fixtures. Cross the ordinary 8192
//! threshold without altering it. Exact vectors, lexical membership, metadata,
//! commit rollback and unchanged replay are all observable requirements.

use super::*;

const ORIGINAL_COUNT: u32 = 9000;

fn archive() -> Vec<IndexableDocument> {
    (0..ORIGINAL_COUNT)
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

fn publish_maintenance(retained: u32, fail_commit: bool) -> TestResult {
    publish_mixed_maintenance(retained, 0, 0, fail_commit)
}

fn publish_mixed_maintenance(
    retained: u32,
    rewrites: u32,
    additions: u32,
    fail_commit: bool,
) -> TestResult {
    assert!(rewrites <= retained);
    assert!(retained + additions <= ORIGINAL_COUNT);
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
                name: Some("bulk maintenance publication".to_owned()),
            },
        )
        .map_err(|error| error.to_string())?;
        let lock = index::IndexPublishLockOwner::acquire(&cx, &db, workspace)
            .map_err(|error| error.to_string())?;
        let fast = Arc::new(CountingHash::new(HashEmbedder::default_256()));
        let quality = Arc::new(CountingHash::new(HashEmbedder::jl_384(17)));
        let stack = EmbedderStack::from_parts(fast.clone(), Some(quality.clone()));
        let original = archive();
        index::publish_full_index_generation_with_stack(
            &lock,
            &live,
            stack.clone(),
            original.clone(),
            1,
            index::IndexDocumentCounts::memory_only(ORIGINAL_COUNT),
            || Ok(()),
        )
        .await
        .map_err(|error| error.to_string())?;
        assert!(fast.inputs.load(Ordering::SeqCst) >= original.len());
        assert!(quality.inputs.load(Ordering::SeqCst) >= original.len());
        index::verify_published_tier_counts(&live, ORIGINAL_COUNT, true)?;
        let before = generation_files(&live)?;
        let old_fast = tier_rows(&live, false)?;
        let old_quality = tier_rows(&live, true)?;
        assert_eq!(old_fast.len(), original.len());
        assert_eq!(old_quality.len(), original.len());
        let first_survivor =
            usize::try_from(ORIGINAL_COUNT - retained).map_err(|error| error.to_string())?;
        let mut current = original[first_survivor..].to_vec();
        for document in &mut current {
            document.title = Some("updatedmarker".to_owned());
            document
                .metadata
                .insert("revision".to_owned(), "2".to_owned());
        }
        let rewrite_count = usize::try_from(rewrites).map_err(|error| error.to_string())?;
        let addition_count = usize::try_from(additions).map_err(|error| error.to_string())?;
        let inferred_count = rewrite_count + addition_count;
        for document in &mut current[..rewrite_count] {
            document.content.push_str(" repairedmarker");
        }
        current.extend((0..additions).map(|number| {
            let mut document = IndexableDocument::new(
                format!("new-{number:05}"),
                format!("lesson new {number} appendedmarker"),
            );
            document
                .metadata
                .insert("origin".to_owned(), format!("new-{number}"));
            document
        }));
        current.reverse();
        let fresh_ids: std::collections::BTreeSet<_> = current
            .iter()
            .filter(|document| {
                document.id.starts_with("new-") || document.content.contains("repairedmarker")
            })
            .map(|document| document.id.clone())
            .collect();
        assert_eq!(fresh_ids.len(), inferred_count);
        let identity = embedder_identity(&stack).ok_or("complete stack identity")?;
        let delta = plan(&live, &stack, &current).map_err(|error| error.detail)?;
        assert_eq!(delta.upserts.len(), current.len());
        assert_eq!(
            delta.removals.len(),
            original.len() - current.len() + addition_count
        );
        assert_eq!(
            delta.upserts.len() + delta.removals.len(),
            9000 + addition_count
        );
        let vector_upserts = delta
            .vector_upserts(&identity)
            .map_err(|error| error.detail)?;
        assert_eq!(vector_upserts.len(), inferred_count);
        assert_eq!(
            vector_upserts
                .iter()
                .map(|document| document.id.clone())
                .collect::<std::collections::BTreeSet<_>>(),
            fresh_ids,
        );
        assert!(delta.vector_upserts("changed producer").is_err());
        let reset = || {
            fast.inputs.store(0, Ordering::SeqCst);
            quality.inputs.store(0, Ordering::SeqCst);
        };
        let assert_no_inference = || {
            assert_eq!(fast.inputs.load(Ordering::SeqCst), 0);
            assert_eq!(quality.inputs.load(Ordering::SeqCst), 0);
        };
        let assert_changed_input_inference = || {
            assert_eq!(fast.inputs.load(Ordering::SeqCst), inferred_count);
            assert_eq!(quality.inputs.load(Ordering::SeqCst), inferred_count);
        };
        let counts = index::IndexDocumentCounts::memory_only(retained + additions);
        if fail_commit {
            reset();
            let mut commits = 0;
            let failed = index::publish_index_generation_preferring_delta(
                &lock,
                &live,
                stack.clone(),
                current.clone(),
                2,
                counts,
                || {
                    commits += 1;
                    Err(index::IndexRebuildError::Index(
                        "injected maintenance commit refusal".to_owned(),
                    ))
                },
            )
            .await;
            assert!(failed.is_err());
            assert_eq!(
                commits, 1,
                "complete maintenance must reach the commit tail"
            );
            assert_changed_input_inference();
            assert_eq!(index::validated_index_generation(&live)?, 1);
            assert_eq!(generation_files(&live)?, before);
            assert_eq!(tier_rows(&live, false)?, old_fast);
            assert_eq!(tier_rows(&live, true)?, old_quality);
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
            || Ok(()),
        )
        .await
        .map_err(|error| error.to_string())?;
        assert!(matches!(kind, index::PublishedGenerationKind::Delta));
        assert_changed_input_inference();
        assert_eq!(index::validated_index_generation(&live)?, 2);
        index::verify_published_tier_counts(&live, retained + additions, true)?;
        let fast_rows = tier_rows(&live, false)?;
        let quality_rows = tier_rows(&live, true)?;
        // Unchanged embedding inputs retain exact previous vectors in both
        // spaces. Changed/new vectors must be present now, and are compared
        // to an independent full rebuild below rather than a synthetic oracle.
        assert_eq!(fast_rows.len(), current.len());
        assert_eq!(quality_rows.len(), current.len());
        for document in &current {
            let current_fast = fast_rows.get(&document.id).ok_or("missing fast result")?;
            let current_quality = quality_rows
                .get(&document.id)
                .ok_or("missing quality result")?;
            if !fresh_ids.contains(&document.id) {
                assert_eq!(
                    current_fast,
                    old_fast.get(&document.id).ok_or("missing fast source")?
                );
                assert_eq!(
                    current_quality,
                    old_quality
                        .get(&document.id)
                        .ok_or("missing quality source")?
                );
            } else if let Some(previous) = old_fast.get(&document.id) {
                assert_ne!(
                    current_fast, previous,
                    "edited input kept an obsolete fast vector"
                );
                assert_ne!(
                    current_quality,
                    old_quality
                        .get(&document.id)
                        .ok_or("missing edited quality source")?,
                    "edited input kept an obsolete quality vector"
                );
            }
        }
        for removed in &delta.removals {
            assert!(!fast_rows.contains_key(removed));
            assert!(!quality_rows.contains_key(removed));
        }
        if retained == ORIGINAL_COUNT && inferred_count == 0 {
            for file in ["vector.fast.idx", "vector.quality.idx"] {
                let old = before
                    .get(Path::new(file))
                    .ok_or("missing original vector file")?;
                assert_eq!(
                    &std::fs::read(live.join(file)).map_err(|error| error.to_string())?,
                    old
                );
            }
        }
        #[cfg(feature = "lexical-bm25")]
        assert_lexical(&cx, &live, &current).await?;
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
            || Ok(()),
        )
        .await
        .map_err(|error| error.to_string())?;
        assert!(matches!(kind, index::PublishedGenerationKind::Delta));
        assert_no_inference();
        assert_eq!(index::validated_index_generation(&live)?, 3);
        assert_eq!(tier_rows(&live, false)?, fast_rows);
        assert_eq!(tier_rows(&live, true)?, quality_rows);
        if !current.is_empty() {
            let builder = index::IndexBuilder::new(&reference).with_embedder_stack(stack);
            #[cfg(feature = "lexical-bm25")]
            let builder = builder.with_tantivy_lexical();
            builder
                .add_documents(current.clone())
                .build(&cx)
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(tier_rows(&reference, false)?, fast_rows);
            assert_eq!(tier_rows(&reference, true)?, quality_rows);
            #[cfg(feature = "lexical-bm25")]
            assert_lexical(&cx, &reference, &current).await?;
        }
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn native_bulk_metadata_keeps_both_vector_files_unchanged() -> TestResult {
    publish_maintenance(ORIGINAL_COUNT, false)
}

#[test]
fn native_bulk_maintenance_preserves_survivor_vectors_and_lexical_metadata() -> TestResult {
    publish_maintenance(8000, false)
}

#[test]
fn native_bulk_withdrawal_finalizes_empty_tiers_without_inference() -> TestResult {
    publish_maintenance(0, false)
}

#[test]
fn native_bulk_maintenance_commit_refusal_restores_files_before_retry() -> TestResult {
    publish_maintenance(8000, true)
}

#[test]
fn native_mixed_metadata_refresh_reembeds_only_edited_inputs_in_both_tiers() -> TestResult {
    publish_mixed_maintenance(ORIGINAL_COUNT, 16, 0, false)
}

#[test]
fn native_shrinking_mixed_archive_publishes_new_edits_metadata_and_withdrawals_together()
-> TestResult {
    publish_mixed_maintenance(8000, 16, 32, false)
}

#[test]
fn native_mixed_commit_failure_restores_both_tiers_and_retry_stays_incremental() -> TestResult {
    publish_mixed_maintenance(8000, 16, 32, true)
}

#[test]
fn bulk_maintenance_requires_current_v3_inputs_and_the_actual_producer() -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let directory = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let stack = index::hash_fallback_embedder_stack();
    let original = archive();
    let mut current = original.clone();
    for document in &mut current {
        document.title = Some("updatedmarker".to_owned());
    }
    let mut legacy = DocDigests::new(&stack, &original);
    legacy.schema = DOC_DIGESTS_SCHEMA_V2.to_owned();
    legacy.embedding_inputs = None;
    legacy
        .write(&directory)
        .map_err(|error| error.to_string())?;
    assert!(plan(&directory, &stack, &current).is_err());
    DocDigests::new(&stack, &original)
        .write(&directory)
        .map_err(|error| error.to_string())?;
    let delta = plan(&directory, &stack, &current).map_err(|error| error.detail)?;
    let identity = embedder_identity(&stack).ok_or("complete identity")?;
    assert_eq!(delta.upserts.len(), 9000);
    assert!(
        delta
            .vector_upserts(&identity)
            .map_err(|error| error.detail)?
            .is_empty()
    );
    for incomplete in [false, true] {
        let mut invalid = DocDigests::new(&stack, &original);
        if incomplete {
            let _ = invalid
                .embedding_inputs
                .as_mut()
                .ok_or("v3 map")?
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
