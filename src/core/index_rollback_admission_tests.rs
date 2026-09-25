//! Full vector/lexical generations, not manifest-only recovery stand-ins.

use super::super::*;
use std::collections::BTreeMap;

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
        format!("mem_recovery_{generation}"),
        format!("Independent generation {generation} contains actual search evidence."),
    )];
    IndexBuilder::new(path)
        .with_embedder_stack(hash_fallback_embedder_stack())
        .add_documents(documents.clone())
        .build(cx)
        .await
        .map_err(|error| error.to_string())?;
    #[cfg(feature = "lexical-bm25")]
    build_lexical_tier(cx, path, &documents)
        .await
        .map_err(|error| error.to_string())?;
    write_index_metadata(path, generation, IndexDocumentCounts::memory_only(1), None)
        .map_err(|error| error.to_string())?;
    assert_eq!(validated_index_generation(path)?, generation);
    Ok(())
}

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
            return Err("unexpected fixture entry".into());
        }
    }
    Ok(result)
}

fn rejected_directory(index: &Path) -> Result<PathBuf, String> {
    let prefix = format!(
        ".{}{}",
        index_base_name(index).map_err(|error| error.to_string())?,
        INDEX_REJECTED_PREFIX
    );
    let entries = std::fs::read_dir(index_parent(index)).map_err(|error| error.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            return Ok(entry.path());
        }
    }
    Err("quarantined generation is missing".into())
}

fn assert_retired_manifest(path: &Path, expected: &[u8]) -> TestResult {
    assert!(parse_index_metadata(path)?.is_none());
    assert!(validated_index_generation(path).is_err());
    let mut saved = Vec::new();
    for entry in std::fs::read_dir(path).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(super::RETIRED_PREFIX)
        {
            saved.push(std::fs::read(entry.path()).map_err(|error| error.to_string())?);
        }
    }
    assert_eq!(saved, vec![expected.to_vec()]);
    Ok(())
}

#[test]
fn failed_rollback_preparation_cannot_admit_the_rejected_live_index() -> TestResult {
    let (_root, index) = fixture()?;
    crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
        let parent = index_parent(&index);
        let accepted = parent.join("index.previous");
        build_generation(&cx, &accepted, 7).await?;
        build_generation(&cx, &index, 8).await?;
        let manifest =
            std::fs::read(index.join(INDEX_METADATA_FILE)).map_err(|error| error.to_string())?;
        // A vanished/replaced retained generation makes reverse-exchange
        // preparation fail before it can move the rejected live directory.
        let broken_retained = parent.join("broken-retained");
        std::fs::write(&broken_retained, b"not a directory").map_err(|error| error.to_string())?;
        let error =
            rollback_published_index(&index, &parent.join("absent-stage"), Some(&broken_retained))
                .expect_err("invalid retained directory must fail rollback");
        assert!(error.to_string().contains("not a directory"));
        assert!(
            index.is_dir(),
            "failure must exercise a still-live rejected directory"
        );
        assert_retired_manifest(&index, &manifest)?;
        let before = inventory(parent)?;
        let lease = IndexGenerationLease::read(&cx, &index)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            lease
                .index_for_snapshot(&cx, &index, 8)
                .map_err(|error| error.to_string())?,
            accepted
        );
        assert_eq!(
            inventory(parent)?,
            before,
            "reader recovery must remain read-only"
        );
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn failed_commit_retires_only_the_rejected_generation_manifest() -> TestResult {
    let (_root, index) = fixture()?;
    crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
        build_generation(&cx, &index, 7).await?;
        let accepted_manifest =
            std::fs::read(index.join(INDEX_METADATA_FILE)).map_err(|error| error.to_string())?;
        let staging = create_publish_staging_dir(&index).map_err(|error| error.to_string())?;
        build_generation(&cx, &staging, 8).await?;
        let rejected_manifest =
            std::fs::read(staging.join(INDEX_METADATA_FILE)).map_err(|error| error.to_string())?;
        let error = publish_staged_index_with_commit(&index, &staging, || {
            Err(IndexRebuildError::Index(
                "controlled bookkeeping failure".into(),
            ))
        })
        .expect_err("publication must fail");
        assert!(error.to_string().contains("controlled bookkeeping failure"));
        assert_eq!(validated_index_generation(&index)?, 7);
        assert_eq!(
            std::fs::read(index.join(INDEX_METADATA_FILE)).map_err(|error| error.to_string())?,
            accepted_manifest
        );
        assert_retired_manifest(&rejected_directory(&index)?, &rejected_manifest)?;
        let lease = IndexGenerationLease::read(&cx, &index)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            lease
                .index_for_snapshot(&cx, &index, 8)
                .map_err(|error| error.to_string())?,
            index
        );
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn rejected_first_publication_cannot_be_reintroduced_as_retained() -> TestResult {
    let (_root, index) = fixture()?;
    crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
        let staging = create_publish_staging_dir(&index).map_err(|error| error.to_string())?;
        build_generation(&cx, &staging, 8).await?;
        let manifest =
            std::fs::read(staging.join(INDEX_METADATA_FILE)).map_err(|error| error.to_string())?;
        assert!(
            publish_staged_index_with_commit(&index, &staging, || {
                Err(IndexRebuildError::Index(
                    "controlled first publication failure".into(),
                ))
            })
            .is_err()
        );
        assert!(!index.exists());
        let rejected = rejected_directory(&index)?;
        assert_retired_manifest(&rejected, &manifest)?;
        let retained = index_parent(&index).join("index.previous");
        std::fs::rename(&rejected, &retained).map_err(|error| error.to_string())?;
        let before = inventory(index_parent(&index))?;
        let lease = IndexGenerationLease::read(&cx, &index)
            .await
            .map_err(|error| error.to_string())?;
        assert!(lease.index_for_snapshot(&cx, &index, 8).is_err());
        assert_eq!(inventory(index_parent(&index))?, before);
        drop(lease);
        assert_eq!(
            recover_interrupted_publish(&index).map_err(|error| error.to_string())?,
            IndexPublishRecoveryAction::NoRecoverableGeneration
        );
        assert!(!index.exists());
        assert_eq!(inventory(index_parent(&index))?, before);
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn manifest_retirement_failure_does_not_skip_restoration_of_the_accepted_index() -> TestResult {
    let (_root, index) = fixture()?;
    crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
        let parent = index_parent(&index);
        let retained = parent.join("index.previous");
        build_generation(&cx, &retained, 7).await?;
        build_generation(&cx, &index, 8).await?;
        std::fs::rename(
            index.join(INDEX_METADATA_FILE),
            index.join("injected-manifest.saved"),
        )
        .map_err(|error| error.to_string())?;
        std::fs::create_dir(index.join(INDEX_METADATA_FILE)).map_err(|error| error.to_string())?;
        let error = rollback_published_index(&index, &parent.join("absent-stage"), Some(&retained))
            .expect_err("manifest retirement failure must remain visible");
        assert!(error.to_string().contains("metadata is not a regular file"));
        assert_eq!(validated_index_generation(&index)?, 7);
        assert!(
            rejected_directory(&index)?
                .join("injected-manifest.saved")
                .is_file()
        );
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn unsafe_retained_path_cannot_leave_rejected_live_manifest_admissible() -> TestResult {
    let (_root, index) = fixture()?;
    crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
        let parent = index_parent(&index);
        let accepted = parent.join("index.previous");
        build_generation(&cx, &accepted, 7).await?;
        build_generation(&cx, &index, 8).await?;
        let manifest =
            std::fs::read(index.join(INDEX_METADATA_FILE)).map_err(|error| error.to_string())?;
        let link = parent.join("linked-retained");
        std::os::unix::fs::symlink(&accepted, &link).map_err(|error| error.to_string())?;
        assert!(
            rollback_published_index(&index, &parent.join("absent-stage"), Some(&link)).is_err()
        );
        assert_retired_manifest(&index, &manifest)?;
        assert_eq!(validated_index_generation(&accepted)?, 7);
        assert!(
            std::fs::symlink_metadata(&link)
                .map_err(|error| error.to_string())?
                .is_symlink()
        );
        let lease = IndexGenerationLease::read(&cx, &index)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(
            lease
                .index_for_snapshot(&cx, &index, 8)
                .map_err(|error| error.to_string())?,
            accepted
        );
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

#[test]
fn rejected_live_generation_with_no_accepted_fallback_fails_closed() -> TestResult {
    let (_root, index) = fixture()?;
    crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
        build_generation(&cx, &index, 8).await?;
        let parent = index_parent(&index);
        let invalid = parent.join("invalid-retained");
        std::fs::write(&invalid, b"not a generation").map_err(|error| error.to_string())?;
        assert!(
            rollback_published_index(&index, &parent.join("absent-stage"), Some(&invalid)).is_err()
        );
        let before = inventory(parent)?;
        let lease = IndexGenerationLease::read(&cx, &index)
            .await
            .map_err(|error| error.to_string())?;
        assert!(lease.index_for_snapshot(&cx, &index, 8).is_err());
        assert_eq!(inventory(parent)?, before);
        Ok(())
    })
    .map_err(|error| error.to_string())?
}
