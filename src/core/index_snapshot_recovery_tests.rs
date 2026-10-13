//! Exercise retained-generation selection with actual vector and lexical tiers.
//! No model download, database mock, or manifest-only stand-in is used.

use super::*;
use std::collections::BTreeMap;

use super::super::{
    IndexBuilder, IndexDocumentCounts, VECTOR_INDEX_FAST_FILE, hash_fallback_embedder_stack,
    validated_index_generation, write_index_metadata,
};

type TestResult = Result<(), String>;

fn fixture() -> Result<(tempfile::TempDir, PathBuf), String> {
    let root = tempfile::tempdir().map_err(|error| error.to_string())?;
    let index = root
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?
        .join("index");
    Ok((root, index))
}

async fn build_generation(cx: &asupersync::Cx, path: &Path, generation: u64) -> TestResult {
    let documents = vec![crate::search::IndexableDocument::new(
        "mem_complete_snapshot",
        "Complete retained generations preserve the original snapshot evidence.",
    )];
    IndexBuilder::new(path)
        .with_embedder_stack(hash_fallback_embedder_stack())
        .add_documents(documents.clone())
        .build(cx)
        .await
        .map_err(|error| error.to_string())?;
    #[cfg(feature = "lexical-bm25")]
    super::super::build_lexical_tier(cx, path, &documents)
        .await
        .map_err(|error| error.to_string())?;
    write_index_metadata(path, generation, IndexDocumentCounts::memory_only(1), None)
        .map_err(|error| error.to_string())?;
    assert_eq!(validated_index_generation(path)?, generation);
    Ok(())
}

// Include directory names as well as file bytes: creating an empty directory
// during a supposedly read-only recovery must not escape the assertion.
fn inventory(root: &Path) -> Result<BTreeMap<PathBuf, Option<Vec<u8>>>, String> {
    let mut result = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let relative = path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .to_path_buf();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.is_dir() {
            result.insert(relative, None);
            for entry in std::fs::read_dir(&path).map_err(|error| error.to_string())? {
                pending.push(entry.map_err(|error| error.to_string())?.path());
            }
        } else if metadata.is_file() {
            result.insert(
                relative,
                Some(std::fs::read(&path).map_err(|error| error.to_string())?),
            );
        } else {
            return Err("the byte-inventory fixture contains an unexpected special entry".into());
        }
    }
    Ok(result)
}

#[test]
fn missing_live_directory_uses_retained_tiers_without_creating_an_active_index() -> TestResult {
    let (_root, index) = fixture()?;
    crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
        let retained = index_parent(&index).join("index.previous");
        build_generation(&cx, &retained, 7).await?;
        let before = inventory(index_parent(&index))?;
        let lease = IndexGenerationLease::read(&cx, &index)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            lease
                .index_for_snapshot(&cx, &index, 7)
                .map_err(|error| error.to_string())?,
            retained
        );
        assert!(
            !index.exists(),
            "a reader must not promote the retained directory"
        );
        assert!(lease.index_for_snapshot(&cx, &index, 6).is_err());
        assert_eq!(inventory(index_parent(&index))?, before);
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn intact_live_manifest_does_not_hide_missing_or_corrupt_vector_tiers() -> TestResult {
    for missing in [true, false] {
        let (_root, index) = fixture()?;
        crate::core::run_cli_with_cx(Duration::from_secs(40), |cx| async move {
            let retained = index_parent(&index).join("index.previous");
            build_generation(&cx, &retained, 7).await?;
            build_generation(&cx, &index, 8).await?;
            let vector = index.join(VECTOR_INDEX_FAST_FILE);
            if missing {
                std::fs::rename(&vector, index.join("injected-missing-vector.saved"))
                    .map_err(|error| error.to_string())?;
            } else {
                std::fs::write(&vector, b"injected corrupt vector")
                    .map_err(|error| error.to_string())?;
            }
            assert!(validated_index_generation(&index).is_err());
            let before = inventory(index_parent(&index))?;
            let lease = IndexGenerationLease::read(&cx, &index)
                .await
                .map_err(|error| error.to_string())?;
            assert_eq!(
                lease
                    .index_for_snapshot(&cx, &index, 8)
                    .map_err(|error| error.to_string())?,
                retained
            );
            assert_eq!(inventory(index_parent(&index))?, before);
            Ok::<(), String>(())
        })
        .map_err(|error| error.to_string())??;
    }
    Ok(())
}

#[cfg(feature = "lexical-bm25")]
#[test]
fn missing_lexical_tier_cannot_be_selected_from_an_intact_manifest() -> TestResult {
    let (_root, index) = fixture()?;
    crate::core::run_cli_with_cx(Duration::from_secs(40), |cx| async move {
        let retained = index_parent(&index).join("index.previous");
        build_generation(&cx, &retained, 7).await?;
        build_generation(&cx, &index, 8).await?;
        std::fs::rename(
            index.join("lexical"),
            index.join("injected-missing-lexical.saved"),
        )
        .map_err(|error| error.to_string())?;
        std::fs::create_dir(index.join("lexical")).map_err(|error| error.to_string())?;
        assert!(validated_index_generation(&index).is_err());
        let before = inventory(index_parent(&index))?;
        let lease = IndexGenerationLease::read(&cx, &index)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            lease
                .index_for_snapshot(&cx, &index, 8)
                .map_err(|error| error.to_string())?,
            retained
        );
        assert_eq!(inventory(index_parent(&index))?, before);
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn a_complete_live_generation_remains_preferred_to_retained_candidates() -> TestResult {
    let (_root, index) = fixture()?;
    crate::core::run_cli_with_cx(Duration::from_secs(40), |cx| async move {
        build_generation(&cx, &index, 7).await?;
        build_generation(&cx, &index_parent(&index).join("index.previous"), 6).await?;
        let before = inventory(index_parent(&index))?;
        let lease = IndexGenerationLease::read(&cx, &index)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            lease
                .index_for_snapshot(&cx, &index, 7)
                .map_err(|error| error.to_string())?,
            index
        );
        assert_eq!(inventory(index_parent(&index))?, before);
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn recovery_refuses_redirected_vector_entries_in_live_or_retained_generations() -> TestResult {
    use std::os::unix::fs::symlink;

    for redirect_live in [true, false] {
        let (_root, index) = fixture()?;
        crate::core::run_cli_with_cx(Duration::from_secs(40), |cx| async move {
            let retained = index_parent(&index).join("index.previous");
            build_generation(&cx, &retained, 7).await?;
            build_generation(&cx, &index, 8).await?;
            let selected = if redirect_live { &index } else { &retained };
            let vector = selected.join(VECTOR_INDEX_FAST_FILE);
            let saved = selected.join("injected-private-vector.saved");
            std::fs::rename(&vector, &saved).map_err(|error| error.to_string())?;
            symlink(&saved, &vector).map_err(|error| error.to_string())?;
            let before = std::fs::read(&saved).map_err(|error| error.to_string())?;
            let lease = IndexGenerationLease::read(&cx, &index)
                .await
                .map_err(|error| error.to_string())?;
            let ceiling = if redirect_live { 8 } else { 7 };
            let error = lease
                .index_for_snapshot(&cx, &index, ceiling)
                .expect_err("a redirected tier must not turn into successful fallback");
            assert!(!error.to_string().contains("injected-private-vector"));
            assert_eq!(
                std::fs::read(&saved).map_err(|error| error.to_string())?,
                before
            );
            assert!(
                std::fs::symlink_metadata(&vector)
                    .map_err(|error| error.to_string())?
                    .is_symlink()
            );
            Ok::<(), String>(())
        })
        .map_err(|error| error.to_string())??;
    }
    Ok(())
}

#[test]
fn recovery_refuses_special_entries_before_a_backend_can_open_them() -> TestResult {
    let (_root, index) = fixture()?;
    crate::core::run_cli_with_cx(Duration::from_secs(40), |cx| async move {
        let retained = index_parent(&index).join("index.previous");
        build_generation(&cx, &retained, 7).await?;
        build_generation(&cx, &index, 8).await?;
        // A FIFO, not a Unix socket (bd-ns6fp). Both are "special entries" to the
        // reader -- index_read_lease.rs:305 refuses anything that is neither a
        // directory nor a regular file -- but a socket PATH is bounded by
        // sockaddr_un.sun_path, about 104 bytes. The fixture canonicalizes a
        // tempdir, so under a long TMPDIR (every RCH worker: the project sits at
        // /data/rch/eidetic_engine_cli/paired-<32 hex>/) `bind` failed with
        // "path must be shorter than SUN_LEN" and the `?` aborted SETUP before the
        // assertion below. The test then passed judgement on nothing while
        // reporting a failure that looked like a product defect. mkfifo has no
        // path bound, so the subject is now reachable wherever the suite runs.
        //
        // The two error arms below are deliberately distinct from the assertion:
        // a fixture that cannot be built must never be mistakable for a reader
        // that failed to refuse.
        let special = index.join("unexpected.fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&special)
            .status()
            .map_err(|error| format!("mkfifo could not run: {error}"))?;
        if !status.success() {
            return Err(format!(
                "mkfifo refused to create {}: {status}",
                special.display()
            ));
        }
        let lease = IndexGenerationLease::read(&cx, &index)
            .await
            .map_err(|error| error.to_string())?;
        let error = lease
            .index_for_snapshot(&cx, &index, 8)
            .expect_err("special entries must not reach tier readers");
        assert!(error.to_string().contains("special entry"));
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn cancellation_is_preserved_before_generation_entry_traversal() -> TestResult {
    let (_root, index) = fixture()?;
    crate::core::run_cli_with_cx(Duration::from_secs(10), |cx| async move {
        cx.set_cancel_reason(asupersync::CancelReason::user("cancel recovery traversal"));
        assert!(matches!(
            ensure_generation_entries_are_regular(&cx, &index),
            Err(IndexRebuildError::Cancelled(reason))
                if reason.message.as_deref() == Some("cancel recovery traversal")
        ));
        assert!(!index.exists());
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn retained_presence_preserves_missing_index_and_never_admits_staging_directories() -> TestResult {
    let (_root, index) = fixture()?;
    crate::core::run_cli_with_cx(Duration::from_secs(10), |cx| async move {
        let parent = index_parent(&index);
        for name in [
            ".index.publish-incomplete",
            ".index.rejected-incomplete",
            "index.previous.backup",
            "index.previous.000",
        ] {
            std::fs::create_dir(parent.join(name)).map_err(|error| error.to_string())?;
        }
        let lease = IndexGenerationLease::read(&cx, &index)
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            !lease
                .has_retained_generation_directory(&cx, &index)
                .map_err(|error| error.to_string())?
        );
        drop(lease);
        std::fs::create_dir(parent.join("index.previous.998"))
            .map_err(|error| error.to_string())?;
        let before = inventory(parent)?;
        let lease = IndexGenerationLease::read(&cx, &index)
            .await
            .map_err(|error| error.to_string())?;
        assert!(
            lease
                .has_retained_generation_directory(&cx, &index)
                .map_err(|error| error.to_string())?
        );
        assert!(lease.index_for_snapshot(&cx, &index, 7).is_err());
        assert_eq!(
            inventory(parent)?,
            before,
            "presence is not generation admission"
        );
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

// #78: a hook can be cancelled after acquiring its lease, or while a rebuild
// holds publication authority. Neither case may poison the next recovery read.
#[test]
fn cancellation_after_lease_acquisition_preserves_generations_for_a_fresh_request() -> TestResult {
    let (_root, index) = fixture()?;
    let retry_index = index.clone();
    let before = crate::core::run_cli_with_cx(Duration::from_secs(40), |cx| async move {
        build_generation(&cx, &index, 8).await?;
        build_generation(&cx, &index_parent(&index).join("index.previous"), 7).await?;
        let before = inventory(index_parent(&index))?;
        let lease = IndexGenerationLease::read(&cx, &index)
            .await
            .map_err(|error| error.to_string())?;
        cx.set_cancel_reason(asupersync::CancelReason::user("cancel leased source snapshot"));
        // Exercise both the live and retained selection branches. A complete
        // backend must not turn cancellation into success or a generic error.
        for ceiling in [8, 7] {
            assert!(matches!(
                lease.index_for_snapshot(&cx, &index, ceiling),
                Err(IndexRebuildError::Cancelled(reason))
                    if reason.message.as_deref() == Some("cancel leased source snapshot")
            ));
        }
        drop(lease);
        let writer = open_directory(index_parent(&index)).map_err(|error| error.to_string())?;
        assert!(try_lease(&writer, true).map_err(|error| error.to_string())?);
        assert_eq!(inventory(index_parent(&index))?, before);
        Ok::<_, String>(before)
    })
    .map_err(|error| error.to_string())??;

    // Use a fresh runtime/request, not a reset cancellation flag. The same
    // on-disk generations must still support both source snapshot ceilings.
    crate::core::run_cli_with_cx(Duration::from_secs(10), |cx| async move {
        let lease = IndexGenerationLease::read(&cx, &retry_index)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            lease
                .index_for_snapshot(&cx, &retry_index, 8)
                .map_err(|error| error.to_string())?,
            retry_index
        );
        assert_eq!(
            lease
                .index_for_snapshot(&cx, &retry_index, 7)
                .map_err(|error| error.to_string())?,
            index_parent(&retry_index).join("index.previous")
        );
        assert_eq!(inventory(index_parent(&retry_index))?, before);
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn publication_contention_times_out_without_changing_generations_then_recovers() -> TestResult {
    let (_root, index) = fixture()?;
    crate::core::run_cli_with_cx(Duration::from_secs(40), |cx| async move {
        build_generation(&cx, &index, 8).await?;
        let retained = index_parent(&index).join("index.previous");
        build_generation(&cx, &retained, 7).await?;
        let before = inventory(index_parent(&index))?;
        let publisher = IndexGenerationLease::publish(&cx, &index)
            .await
            .map_err(|error| error.to_string())?;
        let wait = Duration::from_millis(75);
        let started = Instant::now();
        let reader = IndexGenerationLease::acquire(&cx, &index, false, wait).await;
        let elapsed = started.elapsed();
        assert!(matches!(
            reader,
            Err(IndexRebuildError::Index(message)) if message.contains("busy")
        ));
        assert!(elapsed >= wait, "exercise retries, not only a zero-wait probe");
        assert!(elapsed < Duration::from_secs(2), "bounded contention: {elapsed:?}");
        assert_eq!(inventory(index_parent(&index))?, before);
        // Timing out a waiter must not release another owner's publisher lock.
        let probe = open_directory(index_parent(&index)).map_err(|error| error.to_string())?;
        assert!(!try_lease(&probe, false).map_err(|error| error.to_string())?);
        drop(publisher);
        let reader = IndexGenerationLease::read(&cx, &index)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            reader
                .index_for_snapshot(&cx, &index, 7)
                .map_err(|error| error.to_string())?,
            retained,
            "retry must not admit the live generation newer than its source snapshot"
        );
        assert_eq!(inventory(index_parent(&index))?, before);
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn cancellation_during_publication_wait_preserves_reason_and_releases_waiter() -> TestResult {
    use std::future::{Future, poll_fn};
    use std::task::Poll;

    let (_root, index) = fixture()?;
    crate::core::run_cli_with_cx(Duration::from_secs(10), |cx| async move {
        let publisher = IndexGenerationLease::publish(&cx, &index)
            .await
            .map_err(|error| error.to_string())?;
        let before = inventory(index_parent(&index))?;
        let started = Instant::now();
        let mut waiting = std::pin::pin!(IndexGenerationLease::read(&cx, &index));
        // Reach an actual pending lock retry before cancelling. This catches a
        // lost cancellation inside the sleep/retry loop, unlike pre-cancelling.
        poll_fn(|task_cx| match waiting.as_mut().poll(task_cx) {
            Poll::Pending => Poll::Ready(()),
            Poll::Ready(result) => panic!("reader bypassed the publisher: {result:?}"),
        })
        .await;
        cx.set_cancel_reason(asupersync::CancelReason::user("cancel waiting hook"));
        assert!(matches!(
            waiting.await,
            Err(IndexRebuildError::Cancelled(reason))
                if reason.message.as_deref() == Some("cancel waiting hook")
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(inventory(index_parent(&index))?, before);
        let probe = open_directory(index_parent(&index)).map_err(|error| error.to_string())?;
        assert!(!try_lease(&probe, false).map_err(|error| error.to_string())?);
        drop(publisher);
        assert!(try_lease(&probe, true).map_err(|error| error.to_string())?);
        assert_eq!(inventory(index_parent(&index))?, before);
        Ok(())
    })
    .map_err(|error| error.to_string())?
}
