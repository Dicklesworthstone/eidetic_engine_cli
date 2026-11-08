//! Selective compaction preserves untouched native segments and source generations.

use super::*;
use crate::search::{LexicalRead, LexicalWrite};
use std::time::Duration;

type TestResult<T = ()> = Result<T, String>;

fn segment_id(number: usize) -> SegmentId {
    SegmentId::from_uuid_string(&format!("{number:032x}")).expect("fixture segment identity")
}

fn states(sizes: &[u32]) -> SegmentStates {
    sizes
        .iter()
        .enumerate()
        .map(|(number, &size)| {
            (
                segment_id(number + 1),
                SegmentState {
                    max_doc: size,
                    live_docs: size,
                    delete_opstamp: None,
                },
            )
        })
        .collect()
}

#[test]
fn small_delta_cohort_does_not_rewrite_the_large_base() {
    let mut sizes = vec![1_000_000];
    sizes.extend([1; 32]);
    let before = states(&sizes);
    let selected = compaction_candidates(&before, 32);
    assert_eq!(selected.len(), 32);
    assert!(!selected.contains(&segment_id(1)));
    assert_eq!(
        selected.iter().map(|id| before[id].max_doc).sum::<u32>(),
        32
    );
    assert_eq!(before.len() - selected.len() + 1, 2);
}

#[test]
fn nonuniform_compaction_leaves_headroom_and_is_independent_of_input_order() {
    let before = states(&(1..=65).collect::<Vec<_>>());
    let selected = compaction_candidates(&before, 32);
    assert_eq!(selected.len(), 50);
    assert_eq!(before.len() - selected.len() + 1, 16);
    assert!(selected.iter().all(|id| before[id].max_doc <= 50));
    let reversed: SegmentStates = before
        .iter()
        .rev()
        .map(|(id, state)| (*id, state.clone()))
        .collect();
    assert_eq!(compaction_candidates(&reversed, 32), selected);
}

#[test]
fn uniform_fragmentation_keeps_the_existing_single_merge_behavior() {
    for size in [0, 1, 17, u32::MAX] {
        let before = states(&[size; 33]);
        assert_eq!(
            compaction_candidates(&before, 32),
            before.keys().copied().collect::<Vec<_>>()
        );
    }
}

#[test]
fn threshold_and_headroom_hold_across_boundary_shapes() {
    for count in 0..96 {
        for limit in [0, 1, 2, 3, 4, 7, 16, 32, 64, usize::MAX] {
            let before = states(&(0..count).map(|i| (i * 7 % 19) as u32).collect::<Vec<_>>());
            let selected = compaction_candidates(&before, limit);
            if count <= limit.max(1) {
                assert!(selected.is_empty());
            } else {
                assert!(selected.len() >= 2 && selected.len() <= count);
                assert_eq!(
                    selected.iter().collect::<BTreeSet<_>>().len(),
                    selected.len()
                );
                assert!(selected.iter().all(|id| before.contains_key(id)));
                // Unselected segments survive and the selection merges into one.
                let after = count - selected.len() + 1;
                assert!(after <= (limit.max(1) / 2).max(1));
            }
        }
    }
}

#[test]
fn validation_rejects_same_count_substitutions_and_changed_retained_deletion_state() {
    let before = states(&[100, 1, 1, 1, 1]);
    let selected = compaction_candidates(&before, 4);
    let replacement = segment_id(99);
    let mut after = BTreeMap::from([
        (segment_id(1), before[&segment_id(1)].clone()),
        (
            replacement,
            SegmentState {
                max_doc: 4,
                live_docs: 4,
                delete_opstamp: None,
            },
        ),
    ]);
    assert!(validate_compaction(&before, &after, &selected, Some(replacement), 4).is_ok());
    let valid = after.clone();
    let retained = after.remove(&segment_id(1)).expect("retained base");
    after.insert(segment_id(100), retained);
    assert!(validate_compaction(&before, &after, &selected, Some(replacement), 4).is_err());
    after = valid.clone();
    after.get_mut(&segment_id(1)).expect("base").delete_opstamp = Some(9);
    assert!(validate_compaction(&before, &after, &selected, Some(replacement), 4).is_err());
    after = valid.clone();
    after.get_mut(&segment_id(1)).expect("base").live_docs -= 1;
    after.get_mut(&replacement).expect("merged").live_docs += 1;
    assert_eq!(
        live_documents(&before).unwrap(),
        live_documents(&after).unwrap()
    );
    assert!(validate_compaction(&before, &after, &selected, Some(replacement), 4).is_err());
    assert!(validate_compaction(&before, &valid, &selected, None, 4).is_err());
    assert!(validate_compaction(&before, &valid, &selected, Some(segment_id(100)), 4).is_err());
    assert!(validate_compaction(&before, &valid, &[segment_id(1)], Some(replacement), 4).is_err());
}

#[test]
fn fully_deleted_selected_segments_may_have_no_replacement() {
    let mut before = states(&[100, 1, 1, 1, 1]);
    for (id, state) in &mut before {
        if *id != segment_id(1) {
            state.live_docs = 0;
            state.delete_opstamp = Some(7);
        }
    }
    let selected = compaction_candidates(&before, 4);
    let after = BTreeMap::from([(segment_id(1), before[&segment_id(1)].clone())]);
    assert!(validate_compaction(&before, &after, &selected, None, 4).is_ok());
}

fn file_inventory(root: &Path) -> TestResult<BTreeMap<std::path::PathBuf, Vec<u8>>> {
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

/// Commit real documents in five unequal native segments; no synthetic index
/// metadata or invented segment files serve as the successful integration case.
fn unequal_segments(root: &Path) -> TestResult<Index> {
    let directory = root.join(LEXICAL_INDEX_SUBDIR);
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    drop(crate::search::create_lexical_index(&directory).map_err(|error| error.to_string())?);
    let index = Index::open_in_dir(&directory).map_err(|error| error.to_string())?;
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
    let mut writer = maintenance_writer(&index).map_err(|error| error.detail)?;
    let mut number = 0_u64;
    for count in [96, 1, 1, 1, 1] {
        for _ in 0..count {
            let mut document = TantivyDocument::new();
            document.add_text(id, format!("mem_selective_{number:03}"));
            document.add_text(content, "quartzmarker retained lexical evidence");
            document.add_text(title, "selective compaction");
            document.add_text(metadata, "{}");
            if let Some(ordinal) = ordinal {
                document.add_u64(ordinal, number);
            }
            writer
                .add_document(document)
                .map_err(|error| error.to_string())?;
            number += 1;
        }
        writer.commit().map_err(|error| error.to_string())?;
    }
    writer
        .wait_merging_threads()
        .map_err(|error| error.to_string())?;
    Ok(index)
}

async fn ids(cx: &asupersync::Cx, index: &crate::search::TantivyIndex) -> TestResult<Vec<String>> {
    let mut ids = index
        .search(cx, "quartzmarker", 256)
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|hit| hit.doc_id.as_str().to_owned())
        .collect::<Vec<_>>();
    ids.sort();
    Ok(ids)
}

#[test]
fn native_partial_merge_preserves_the_base_bytes_search_results_and_live_generation() -> TestResult
{
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let root = temp
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    crate::core::run_cli_with_cx(Duration::from_secs(120), |cx| async move {
        let _temp = temp;
        let live = root.join("live");
        let staged = root.join("staged");
        let native = unequal_segments(&live)?;
        let metas = native
            .searchable_segment_metas()
            .map_err(|error| error.to_string())?;
        assert_eq!(metas.len(), 5);
        let base = metas
            .iter()
            .find(|meta| meta.max_doc() == 96)
            .ok_or("large base absent")?;
        let base_id = base.id();
        let original = file_inventory(&live)?;
        let base_files = base
            .list_files()
            .into_iter()
            .filter_map(|path| {
                let relative = Path::new(LEXICAL_INDEX_SUBDIR).join(path);
                original
                    .get(&relative)
                    .map(|bytes| (relative, bytes.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        assert!(
            !base_files.is_empty(),
            "base-file preservation must not be vacuous"
        );
        std::fs::create_dir(&staged).map_err(|error| error.to_string())?;
        super::super::copy_generation(&live, &staged).map_err(|error| error.to_string())?;
        let pinned =
            crate::search::open_lexical_index_read_only(&staged.join(LEXICAL_INDEX_SUBDIR))
                .map_err(|error| error.to_string())?;
        let expected = ids(&cx, &pinned).await?;
        assert_eq!(expected.len(), 100);
        let staged_native = Index::open_in_dir(staged.join(LEXICAL_INDEX_SUBDIR))
            .map_err(|error| error.to_string())?;
        let states = segment_states(&staged_native).map_err(|error| error.detail)?;
        let selected = compaction_candidates(&states, 4);
        let selected_bytes =
            selected_segment_bytes(&staged_native, &staged, &selected, &mut || Ok(()))
                .map_err(|error| error.detail)?;
        let all_bytes = selected_segment_bytes(
            &staged_native,
            &staged,
            &states.keys().copied().collect::<Vec<_>>(),
            &mut || Ok(()),
        )
        .map_err(|error| error.detail)?;
        assert!(selected_bytes > 0 && selected_bytes < all_bytes);
        let required = 4 * selected_bytes + 80 * 1024 * 1024;
        let before_refusal = file_inventory(&staged)?;
        for failure in 0..3 {
            let result = compact_with_checkpoint_and_capacity(
                &staged,
                4,
                || batch::checkpoint(&cx, "lexical"),
                |_| match failure {
                    0 => Ok(storage::Capacity {
                        available_bytes: required - 1,
                        available_inodes: Some(128),
                    }),
                    1 => Ok(storage::Capacity {
                        available_bytes: required,
                        available_inodes: Some(127),
                    }),
                    _ => Err(IndexRebuildError::Index("capacity probe failed".to_owned())),
                },
            );
            assert!(result.unwrap_err().detail.contains("storage admission"));
            assert_eq!(file_inventory(&staged)?, before_refusal);
            assert_eq!(ids(&cx, &pinned).await?, expected);
        }
        let interrupted = std::cell::Cell::new(false);
        let cancelled = compact_with_checkpoint_and_capacity(
            &staged,
            4,
            || {
                if interrupted.get() {
                    Err(failure("test", "cancel storage probe"))
                } else {
                    batch::checkpoint(&cx, "lexical")
                }
            },
            |_| {
                interrupted.set(true);
                Err(IndexRebuildError::Index("capacity probe failed".to_owned()))
            },
        );
        assert!(
            cancelled
                .unwrap_err()
                .detail
                .contains("cancel storage probe")
        );
        assert_eq!(file_inventory(&staged)?, before_refusal);
        let report = compact_with_checkpoint_and_capacity(
            &staged,
            4,
            || batch::checkpoint(&cx, "lexical"),
            |_| {
                Ok(storage::Capacity {
                    available_bytes: required,
                    available_inodes: Some(128),
                })
            },
        )
        .map_err(|error| error.detail)?;
        assert_eq!(
            report,
            Compaction {
                segments_before: 5,
                segments_after: 2,
                documents: 100
            }
        );
        let compacted = Index::open_in_dir(staged.join(LEXICAL_INDEX_SUBDIR))
            .map_err(|error| error.to_string())?;
        assert!(
            segment_states(&compacted)
                .map_err(|error| error.detail)?
                .contains_key(&base_id)
        );
        let retained = file_inventory(&staged)?;
        for (path, bytes) in base_files {
            assert_eq!(
                retained.get(&path),
                Some(&bytes),
                "base segment was rewritten: {path:?}"
            );
        }
        assert_eq!(
            file_inventory(&live)?,
            original,
            "the live generation must not change"
        );
        let fresh = crate::search::open_lexical_index_read_only(&staged.join(LEXICAL_INDEX_SUBDIR))
            .map_err(|error| error.to_string())?;
        assert_eq!(ids(&cx, &fresh).await?, expected);
        assert_eq!(
            ids(&cx, &pinned).await?,
            expected,
            "pinned reader remains valid"
        );
        let again = compact_with_checkpoint_and_capacity(
            &staged,
            4,
            || Ok(()),
            |_| panic!("an unfragmented tier must not reserve merge space"),
        )
        .map_err(|error| error.detail)?;
        assert_eq!(again.segments_before, again.segments_after);
        assert_eq!(file_inventory(&staged)?, retained);
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn cancellation_after_a_partial_merge_still_joins_and_releases_the_writer() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let root = temp
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let index = unequal_segments(&root)?;
    let mut observed = false;
    let result = compact_with_checkpoint(&root, 4, || {
        if inventory(&index)?.0.len() == 2 {
            observed = true;
            return Err(failure("test", "cancel after partial merge"));
        }
        Ok(())
    });
    assert!(
        observed,
        "must observe the native partial merge before cancelling"
    );
    assert!(result.is_err());
    maintenance_writer(&index)
        .map_err(|error| error.detail)?
        .wait_merging_threads()
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[test]
fn ordinary_upsert_admission_covers_native_merges_and_short_ordinal_sidecars() -> TestResult {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let root = temp
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    crate::core::run_cli_with_cx(Duration::from_secs(120), |cx| async move {
        let _temp = temp;
        let live = root.join("live");
        let staged = root.join("staged");
        let _native = unequal_segments(&live)?;
        // Native fast fields contain ordinals 0..99. The shipping adapter must
        // pad this valid but stale-short table before appending another ID.
        std::fs::write(live.join(LEXICAL_INDEX_SUBDIR).join("ord_table.json"), "[]")
            .map_err(|error| error.to_string())?;
        let original = file_inventory(&live)?;
        std::fs::create_dir(&staged).map_err(|error| error.to_string())?;
        super::super::copy_generation(&live, &staged).map_err(|error| error.to_string())?;
        let pinned =
            crate::search::open_lexical_index_read_only(&staged.join(LEXICAL_INDEX_SUBDIR))
                .map_err(|error| error.to_string())?;
        let expected = ids(&cx, &pinned).await?;
        assert_eq!(expected.len(), 100);
        let native = Index::open_in_dir(staged.join(LEXICAL_INDEX_SUBDIR))
            .map_err(|error| error.to_string())?;
        let (segments, _) = inventory(&native).map_err(|error| error.detail)?;
        let segment_bytes = selected_segment_bytes(&native, &staged, &segments, &mut || Ok(()))
            .map_err(|error| error.detail)?;
        let documents = [IndexableDocument::new(
            "mem_selective_099",
            "quartzmarker refreshedmarker lexical evidence",
        )];
        let payload_bytes = document_payload_bytes(&documents[0]) as u64;
        let id_bytes = serde_json::to_vec(&documents[0].id)
            .map_err(|error| error.to_string())?
            .len() as u64
            + 1;
        let without_padding = 4 * (segment_bytes + 2 + payload_bytes + id_bytes) + 80 * 1024 * 1024;
        let required = without_padding + 4 * (3 * 100 + 1);
        let before = file_inventory(&staged)?;
        for failure in 0..4 {
            let refused = admit_upserts_with_capacity(
                &staged,
                &documents,
                || batch::checkpoint(&cx, "lexical"),
                |_| match failure {
                    0 => Ok(storage::Capacity {
                        available_bytes: without_padding,
                        available_inodes: Some(128),
                    }),
                    1 => Ok(storage::Capacity {
                        available_bytes: required - 1,
                        available_inodes: Some(128),
                    }),
                    2 => Ok(storage::Capacity {
                        available_bytes: required,
                        available_inodes: Some(127),
                    }),
                    _ => Err(IndexRebuildError::Index("capacity probe failed".to_owned())),
                },
            );
            assert!(
                refused
                    .unwrap_err()
                    .detail
                    .contains("upsert storage admission")
            );
            assert_eq!(file_inventory(&staged)?, before);
            assert_eq!(file_inventory(&live)?, original);
            assert_eq!(ids(&cx, &pinned).await?, expected);
        }
        let interrupted = std::cell::Cell::new(false);
        let cancelled = admit_upserts_with_capacity(
            &staged,
            &documents,
            || {
                if interrupted.get() {
                    Err(failure("test", "cancel upsert storage probe"))
                } else {
                    batch::checkpoint(&cx, "lexical")
                }
            },
            |_| {
                interrupted.set(true);
                Err(IndexRebuildError::Index("capacity probe failed".to_owned()))
            },
        );
        assert!(
            cancelled
                .unwrap_err()
                .detail
                .contains("cancel upsert storage probe")
        );
        assert_eq!(file_inventory(&staged)?, before);
        admit_upserts_with_capacity(
            &staged,
            &documents,
            || batch::checkpoint(&cx, "lexical"),
            |_| {
                Ok(storage::Capacity {
                    available_bytes: required,
                    available_inodes: Some(128),
                })
            },
        )
        .map_err(|error| error.detail)?;
        let writer = crate::search::open_lexical_index(&staged.join(LEXICAL_INDEX_SUBDIR))
            .map_err(|error| error.to_string())?;
        writer
            .index_documents(&cx, &documents)
            .await
            .map_err(|error| error.to_string())?;
        writer
            .commit(&cx)
            .await
            .map_err(|error| error.to_string())?;
        let hits = writer
            .search(&cx, "refreshedmarker", 8)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc_id.as_str(), "mem_selective_099");
        assert_eq!(ids(&cx, &writer).await?, expected);
        assert_eq!(file_inventory(&live)?, original);
        Ok(())
    })
    .map_err(|error| error.to_string())?
}
