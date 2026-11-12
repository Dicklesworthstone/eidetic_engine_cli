//! Native two-tier generations must share unchanged vectors, then isolate every
//! later vector writer without changing active or retained reader results.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Arc;

use frankensearch::VectorIndex;
use frankensearch::index::wal_path_for;

use crate::search::{EmbedderStack, HashEmbedder, IndexableDocument};

use super::{DocDigests, apply, copy_generation_for_delta, plan};

type TestResult = Result<(), String>;

const TIERS: [&str; 2] = ["vector.fast.idx", "vector.quality.idx"];

fn stack(seed: u64) -> EmbedderStack {
    EmbedderStack::from_parts(
        Arc::new(HashEmbedder::default_256()),
        Some(Arc::new(HashEmbedder::jl_384(seed))),
    )
}

fn documents() -> Vec<IndexableDocument> {
    (0..20)
        .map(|number| {
            IndexableDocument::new(
                format!("source-{number:02}"),
                format!("original retained lesson {number}"),
            )
        })
        .collect()
}

fn identity(path: &Path) -> Result<(u64, u64), String> {
    fs::symlink_metadata(path)
        .map(|metadata| (metadata.dev(), metadata.ino()))
        .map_err(|error| error.to_string())
}

fn snapshot(directory: &Path) -> Result<BTreeMap<String, Vec<u8>>, String> {
    TIERS
        .into_iter()
        .map(|name| {
            fs::read(directory.join(name))
                .map(|bytes| (name.to_owned(), bytes))
                .map_err(|error| error.to_string())
        })
        .collect()
}

fn vectors(path: &Path) -> Result<BTreeMap<String, Vec<f32>>, String> {
    let reader =
        super::super::vector_admission::open_read_only(path).map_err(|error| error.to_string())?;
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

async fn build(
    cx: &asupersync::Cx,
    directory: &Path,
    stack: &EmbedderStack,
    documents: &[IndexableDocument],
) -> TestResult {
    let builder = super::super::IndexBuilder::new(directory).with_embedder_stack(stack.clone());
    #[cfg(feature = "lexical-bm25")]
    let builder = builder.with_tantivy_lexical();
    builder
        .add_documents(documents.to_vec())
        .build(cx)
        .await
        .map_err(|error| error.to_string())?;
    DocDigests::new(stack, documents)
        .write(directory)
        .map_err(|error| error.to_string())
}

#[test]
fn metadata_generations_share_both_tiers_then_content_and_deletion_get_private_vectors()
-> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let parent = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    crate::core::run_cli_with_cx(std::time::Duration::from_secs(120), |cx| async move {
        let _root = root;
        let stack = stack(17);
        let live = parent.join("live");
        let mut current = documents();
        build(&cx, &live, &stack, &current).await?;
        // Empty native sidecars are valid, but unlike main vectors they must
        // remain private because future append/compaction can mutate them.
        for name in TIERS {
            fs::write(wal_path_for(&live.join(name)), []).map_err(|error| error.to_string())?;
        }
        let before = snapshot(&live)?;
        let readers = TIERS
            .into_iter()
            .map(|name| {
                super::super::vector_admission::open_read_only(&live.join(name))
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut previous = live.clone();
        for generation in 0..2 {
            current[generation].title = Some(format!("metadata revision {generation}"));
            current[generation]
                .metadata
                .insert("outcome".to_owned(), "helpful".to_owned());
            let delta = plan(&previous, &stack, &current).map_err(|error| error.detail)?;
            assert!(
                !delta
                    .requires_vector_rewrite(&stack)
                    .map_err(|error| error.detail)?
            );
            let staged = parent.join(format!("metadata-{generation}"));
            fs::create_dir(&staged).map_err(|error| error.to_string())?;
            copy_generation_for_delta(&previous, &staged, &stack, &delta)
                .map_err(|error| error.to_string())?;
            for name in TIERS {
                assert_eq!(identity(&live.join(name))?, identity(&staged.join(name))?);
                let source_wal = wal_path_for(&previous.join(name));
                let staged_wal = wal_path_for(&staged.join(name));
                assert_ne!(identity(&source_wal)?, identity(&staged_wal)?);
                assert_eq!(
                    fs::metadata(staged_wal)
                        .map_err(|error| error.to_string())?
                        .nlink(),
                    1
                );
            }
            apply(&cx, &staged, &stack, &delta)
                .await
                .map_err(|error| error.detail)?;
            DocDigests::new(&stack, &current)
                .write(&staged)
                .map_err(|error| error.to_string())?;
            super::super::verify_published_tier_counts(
                &staged,
                u32::try_from(current.len()).map_err(|error| error.to_string())?,
                true,
            )?;
            assert_eq!(snapshot(&staged)?, before);
            previous = staged;
        }
        for removal in [false, true] {
            let mut changed = current.clone();
            if removal {
                changed.retain(|document| document.id != "source-03");
            } else {
                changed[2].content = "changed content requiring new vectors".to_owned();
            }
            let delta = plan(&previous, &stack, &changed).map_err(|error| error.detail)?;
            assert!(
                delta
                    .requires_vector_rewrite(&stack)
                    .map_err(|error| error.detail)?
            );
            let staged = parent.join(format!("changed-{removal}"));
            fs::create_dir(&staged).map_err(|error| error.to_string())?;
            copy_generation_for_delta(&previous, &staged, &stack, &delta)
                .map_err(|error| error.to_string())?;
            for name in TIERS {
                assert_ne!(
                    identity(&previous.join(name))?,
                    identity(&staged.join(name))?
                );
                assert_eq!(
                    fs::metadata(staged.join(name))
                        .map_err(|error| error.to_string())?
                        .nlink(),
                    1
                );
            }
            apply(&cx, &staged, &stack, &delta)
                .await
                .map_err(|error| error.detail)?;
            super::super::verify_published_tier_counts(
                &staged,
                u32::try_from(changed.len()).map_err(|error| error.to_string())?,
                true,
            )?;
            let rebuilt = parent.join(format!("rebuilt-{removal}"));
            build(&cx, &rebuilt, &stack, &changed).await?;
            for name in TIERS {
                assert_eq!(vectors(&staged.join(name))?, vectors(&rebuilt.join(name))?);
            }
            assert_eq!(snapshot(&live)?, before);
            assert_eq!(snapshot(&previous)?, before);
        }
        for reader in readers {
            assert_eq!(
                reader
                    .live_doc_ids()
                    .map_err(|error| error.to_string())?
                    .len(),
                20
            );
            assert_eq!(reader.tombstone_count(), 0);
        }
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn late_vector_mutation_or_producer_change_never_writes_shared_generations() -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let parent = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    crate::core::run_cli_with_cx(std::time::Duration::from_secs(120), |cx| async move {
        let _root = root;
        let stack = stack(17);
        let live = parent.join("live");
        let staged = parent.join("staged");
        let mut current = documents();
        build(&cx, &live, &stack, &current).await?;
        let before = snapshot(&live)?;
        current[0].title = Some("updated metadata".to_owned());
        let mut delta = plan(&live, &stack, &current).map_err(|error| error.detail)?;
        fs::create_dir(&staged).map_err(|error| error.to_string())?;
        copy_generation_for_delta(&live, &staged, &stack, &delta)
            .map_err(|error| error.to_string())?;
        assert!(super::super::open_fast_vector_index(&staged).is_err());
        assert!(super::super::open_quality_vector_index(&staged).is_err());
        delta.upserts[0].content.push_str(" changed after staging");
        assert!(apply(&cx, &staged, &stack, &delta).await.is_err());
        assert_eq!(snapshot(&staged)?, before);
        assert_eq!(snapshot(&live)?, before);
        delta = plan(&live, &stack, &current).map_err(|error| error.detail)?;
        delta.removals.push("source-03".to_owned());
        assert!(apply(&cx, &staged, &stack, &delta).await.is_err());
        let changed_producer = self::stack(29);
        delta = plan(&live, &stack, &current).map_err(|error| error.detail)?;
        assert!(
            apply(&cx, &staged, &changed_producer, &delta)
                .await
                .is_err()
        );
        // A producer mismatch takes the ordinary full-build path, whose new
        // private files must coexist with the shared prior generations.
        let rebuilt = parent.join("rebuilt");
        build(&cx, &rebuilt, &changed_producer, &current).await?;
        for name in TIERS {
            assert_ne!(identity(&live.join(name))?, identity(&rebuilt.join(name))?);
        }
        assert_eq!(snapshot(&staged)?, before);
        assert_eq!(snapshot(&live)?, before);
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn cancelled_shared_delta_preserves_live_and_staged_vector_bytes() -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let parent = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    crate::core::run_cli_with_cx(std::time::Duration::from_secs(120), |cx| async move {
        let _root = root;
        let stack = stack(17);
        let live = parent.join("live");
        let staged = parent.join("staged");
        let mut current = documents();
        build(&cx, &live, &stack, &current).await?;
        let before = snapshot(&live)?;
        current[0].title = Some("updated metadata".to_owned());
        let delta = plan(&live, &stack, &current).map_err(|error| error.detail)?;
        fs::create_dir(&staged).map_err(|error| error.to_string())?;
        copy_generation_for_delta(&live, &staged, &stack, &delta)
            .map_err(|error| error.to_string())?;
        cx.set_cancel_reason(asupersync::CancelReason::user("cancel shared vector stage"));
        assert!(apply(&cx, &staged, &stack, &delta).await.is_err());
        assert_eq!(snapshot(&live)?, before);
        assert_eq!(snapshot(&staged)?, before);
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn pending_wal_or_unsupported_vector_version_is_rejected_before_sharing() -> TestResult {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let parent = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    crate::core::run_cli_with_cx(std::time::Duration::from_secs(120), |cx| async move {
        let _root = root;
        let stack = stack(17);
        for pending_wal in [false, true] {
            let live = parent.join(format!("live-{pending_wal}"));
            let staged = parent.join(format!("staged-{pending_wal}"));
            let mut current = documents();
            build(&cx, &live, &stack, &current).await?;
            current[0].title = Some("updated metadata".to_owned());
            let delta = plan(&live, &stack, &current).map_err(|error| error.detail)?;
            let path = live.join(TIERS[0]);
            if pending_wal {
                let mut writer = VectorIndex::open(&path).map_err(|error| error.to_string())?;
                writer
                    .append("pending", &[0.25; 256])
                    .map_err(|error| error.to_string())?;
            } else {
                let mut bytes = fs::read(&path).map_err(|error| error.to_string())?;
                bytes[4..6].copy_from_slice(&2_u16.to_le_bytes());
                fs::write(&path, bytes).map_err(|error| error.to_string())?;
            }
            let before = snapshot(&live)?;
            fs::create_dir(&staged).map_err(|error| error.to_string())?;
            assert!(copy_generation_for_delta(&live, &staged, &stack, &delta).is_err());
            assert_eq!(
                fs::read_dir(&staged)
                    .map_err(|error| error.to_string())?
                    .count(),
                0
            );
            assert_eq!(snapshot(&live)?, before);
        }
        Ok(())
    })
    .map_err(|error| error.to_string())?
}
