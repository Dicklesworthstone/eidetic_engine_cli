//! Storage admission before and after a generation allocates its index tiers.
//!
//! This is a conservative preflight estimate, not a reservation or a disk quota:
//! another process can consume capacity after the check. Existing publication
//! rollback remains authoritative for subsequent I/O failures. No source text,
//! private path, model download, cleanup, or filesystem mutation belongs here.

use std::io::{self, Write};
use std::path::Path;

use super::{EmbedderStack, IndexRebuildError, index_checkpoint};
use crate::search::IndexableDocument;

const MIB: u64 = 1024 * 1024;
const FIXED_BUILD_ALLOWANCE: u64 = 16 * MIB;
const FREE_SPACE_RESERVE: u64 = 64 * MIB;
const MIN_AVAILABLE_INODES: u64 = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Capacity {
    pub available_bytes: u64,
    // Some filesystems do not expose a meaningful inode count.
    pub available_inodes: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Estimate {
    source_bytes: u64,
    vector_bytes: u64,
    required_bytes: u64,
}

fn arithmetic_error() -> IndexRebuildError {
    IndexRebuildError::Index(
        "index_storage_estimate_overflow: generation size cannot be represented; reduce the corpus or vector dimensions".to_owned(),
    )
}

fn add(left: u64, right: u64) -> Result<u64, IndexRebuildError> {
    left.checked_add(right).ok_or_else(arithmetic_error)
}

fn multiply(left: u64, right: u64) -> Result<u64, IndexRebuildError> {
    left.checked_mul(right).ok_or_else(arithmetic_error)
}

fn length(value: usize) -> Result<u64, IndexRebuildError> {
    u64::try_from(value).map_err(|_| arithmetic_error())
}

impl Estimate {
    fn from_sizes(
        source_bytes: u64,
        documents: u64,
        dimensions: u64,
    ) -> Result<Self, IndexRebuildError> {
        // Budget uncompressed f32 vectors for every selected tier. Four times
        // the source/vector payload allows for backend tables, stored fields
        // and merge scratch; the fixed allowance covers a tiny/empty corpus.
        let vector_bytes = multiply(multiply(documents, dimensions)?, 4)?;
        let required_bytes = add(
            add(
                multiply(add(source_bytes, vector_bytes)?, 4)?,
                FIXED_BUILD_ALLOWANCE,
            )?,
            FREE_SPACE_RESERVE,
        )?;
        Ok(Self {
            source_bytes,
            vector_bytes,
            required_bytes,
        })
    }

    fn check(self, capacity: Capacity) -> Result<(), IndexRebuildError> {
        if capacity.available_bytes < self.required_bytes {
            return Err(IndexRebuildError::Index(format!(
                "index_storage_bytes_insufficient: generation preflight requires {} available bytes including a {} byte reserve, but only {} are available; free space explicitly or choose a different --index-dir; the active generation was not replaced",
                self.required_bytes, FREE_SPACE_RESERVE, capacity.available_bytes,
            )));
        }
        if capacity
            .available_inodes
            .is_some_and(|available| available < MIN_AVAILABLE_INODES)
        {
            return Err(IndexRebuildError::Index(format!(
                "index_storage_inodes_insufficient: generation preflight requires at least {MIN_AVAILABLE_INODES} available inodes; free filesystem entries explicitly or choose a different --index-dir; the active generation was not replaced",
            )));
        }
        Ok(())
    }
}

// Count serialized metadata without allocating another copy of the corpus.
#[derive(Default)]
struct ByteCount(u64);

impl Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("index metadata size overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn serialized_bytes(value: &impl serde::Serialize) -> Result<u64, IndexRebuildError> {
    let mut bytes = ByteCount::default();
    serde_json::to_writer(&mut bytes, value).map_err(|_| {
        IndexRebuildError::Index(
            "index_storage_estimate_failed: serialized metadata size could not be measured; generation was not published".to_owned(),
        )
    })?;
    Ok(bytes.0)
}

fn estimate(
    cx: &asupersync::Cx,
    stack: &EmbedderStack,
    documents: &[IndexableDocument],
) -> Result<Estimate, IndexRebuildError> {
    let dimensions = add(
        length(stack.fast().dimension())?,
        stack
            .quality()
            .map(|quality| length(quality.dimension()))
            .transpose()?
            .unwrap_or(0),
    )?;
    let mut bytes = 0;
    for document in documents {
        index_checkpoint(cx)?;
        bytes = add(bytes, length(document.id.len())?)?;
        bytes = add(bytes, length(document.content.len())?)?;
        bytes = add(
            bytes,
            length(document.title.as_ref().map_or(0, |title| title.len()))?,
        )?;
        let mut metadata = ByteCount::default();
        serde_json::to_writer(&mut metadata, &document.metadata).map_err(|_| {
            IndexRebuildError::Index(
                "index_storage_estimate_failed: metadata size could not be measured; generation build was not started".to_owned(),
            )
        })?;
        bytes = add(bytes, metadata.0)?;
    }
    index_checkpoint(cx)?;
    Estimate::from_sizes(bytes, length(documents.len())?, dimensions)
}

pub(super) fn admit(
    cx: &asupersync::Cx,
    index_dir: &Path,
    stack: &EmbedderStack,
    documents: &[IndexableDocument],
    probe: impl FnOnce(&Path) -> Result<Capacity, IndexRebuildError>,
) -> Result<(), IndexRebuildError> {
    index_checkpoint(cx)?;
    let estimate = estimate(cx, stack, documents)?;
    let capacity = existing_directory(index_dir).and_then(probe);
    index_checkpoint(cx)?;
    estimate.check(capacity?)
}

/// Admit the upcoming delta allocations separately from its staging copy.
/// Native vector compaction/vacuum rewrites the existing main tiers; a reflink
/// makes the initial copy cheap, but does not make those rewrites free. Digest
/// metadata is also rewritten in full even when only one document changes.
pub(super) fn admit_delta(
    cx: &asupersync::Cx,
    index_dir: &Path,
    stack: &EmbedderStack,
    delta: &super::delta::Delta,
    digest_bytes: u64,
    probe: impl FnOnce(&Path) -> Result<Capacity, IndexRebuildError>,
) -> Result<(), IndexRebuildError> {
    index_checkpoint(cx)?;
    let mut estimate = estimate(cx, stack, &delta.upserts)?;
    estimate.required_bytes = add(estimate.required_bytes, digest_bytes)?;
    let rewrite = delta.requires_vector_rewrite(stack).map_err(|fallback| {
        IndexRebuildError::Index(format!("{}: {}", fallback.reason.as_str(), fallback.detail,))
    })?;
    if rewrite {
        for name in [
            Some(super::VECTOR_INDEX_FAST_FILE),
            stack.quality().map(|_| super::VECTOR_INDEX_QUALITY_FILE),
        ]
        .into_iter()
        .flatten()
        {
            index_checkpoint(cx)?;
            let path = index_dir.join(name);
            let measured = (|| {
                super::ensure_index_path_has_no_symlinks(&path, "measure index rewrite allocation")
                    .map_err(|_| capacity_error())?;
                let metadata = std::fs::symlink_metadata(&path).map_err(|_| capacity_error())?;
                if !metadata.is_file() || metadata.is_symlink() {
                    return Err(capacity_error());
                }
                Ok(metadata.len())
            })();
            index_checkpoint(cx)?;
            estimate.required_bytes = add(estimate.required_bytes, measured?)?;
        }
    }
    index_checkpoint(cx)?;
    let capacity = existing_directory(index_dir).and_then(probe);
    index_checkpoint(cx)?;
    estimate.check(capacity?)
}

/// Price native lexical allocations immediately before acquiring the writer.
/// The caller supplies the bytes that writer may rewrite: selected segments
/// for explicit compaction, or every active segment for automatic merges.
#[cfg(feature = "lexical-bm25")]
pub(super) fn admit_lexical_rewrite(
    directory: &Path,
    source_bytes: u64,
    probe: impl FnOnce(&Path) -> Result<Capacity, IndexRebuildError>,
) -> Result<(), IndexRebuildError> {
    let estimate = Estimate::from_sizes(source_bytes, 0, 0)?;
    estimate.check(probe(existing_directory(directory)?)?)
}

/// Reprice the complete manifest after native backend allocations have ended.
/// Earlier admission cannot reserve bytes across those independent phases.
pub(super) fn admit_manifest(
    cx: &asupersync::Cx,
    index_dir: &Path,
    bytes: u64,
    probe: impl FnOnce(&Path) -> Result<Capacity, IndexRebuildError>,
) -> Result<(), IndexRebuildError> {
    index_checkpoint(cx)?;
    let admission = admit_copy(
        &index_dir.join(super::delta::DOC_DIGESTS_FILE),
        bytes,
        probe,
    );
    index_checkpoint(cx)?;
    admission
}

/// Preserve the source store's reserve after allocating a staging inode and
/// before a byte-copy fallback writes its payload. Reflinks pass zero payload
/// bytes but still need free metadata space and inodes. This is admission, not a
/// reservation against other filesystem writers. The caller checkpoints around
/// the probe and owns the exclusively created destination and partial cleanup.
pub(super) fn admit_copy(
    target: &Path,
    bytes: u64,
    probe: impl FnOnce(&Path) -> Result<Capacity, IndexRebuildError>,
) -> Result<(), IndexRebuildError> {
    let required = add(bytes, FREE_SPACE_RESERVE)?;
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let capacity = probe(existing_directory(parent)?)?;
    if capacity.available_bytes < required {
        return Err(IndexRebuildError::Index(format!(
            "index_storage_bytes_insufficient: staging allocation requires {required} available bytes including a {FREE_SPACE_RESERVE} byte reserve, but only {} are available; free space explicitly or choose a different --index-dir; the active generation was not replaced",
            capacity.available_bytes,
        )));
    }
    if capacity
        .available_inodes
        .is_some_and(|available| available < 16)
    {
        return Err(IndexRebuildError::Index(
            "index_storage_inodes_insufficient: staging allocation requires 16 available inodes when reported; free filesystem entries explicitly or choose a different --index-dir; the active generation was not replaced".to_owned(),
        ));
    }
    Ok(())
}

/// Recheck actual free capacity after backend allocation. A preflight estimate
/// cannot observe concurrent consumers or backend scratch growth; neither a
/// depleted reserve nor a failed capacity probe may authorize publication.
pub(super) fn confirm_reserve(
    cx: &asupersync::Cx,
    index_dir: &Path,
    probe: impl FnOnce(&Path) -> Result<Capacity, IndexRebuildError>,
) -> Result<(), IndexRebuildError> {
    index_checkpoint(cx)?;
    let capacity = existing_directory(index_dir).and_then(probe);
    index_checkpoint(cx)?;
    let capacity = capacity?;
    // Final metadata and unrelated source-of-truth writes still need room.
    // This is a point-in-time check, not a reservation against other writers.
    if capacity.available_bytes < FREE_SPACE_RESERVE
        || capacity
            .available_inodes
            .is_some_and(|available| available < 16)
    {
        return Err(IndexRebuildError::Index(format!(
            "index_storage_reserve_exhausted: after staging, publication requires {FREE_SPACE_RESERVE} free bytes and 16 available inodes when reported; only {} free bytes are available; the staged generation was not published; free space explicitly or choose a different --index-dir",
            capacity.available_bytes,
        )));
    }
    Ok(())
}

fn existing_directory(mut path: &Path) -> Result<&Path, IndexRebuildError> {
    super::ensure_index_path_has_no_symlinks(path, "inspect index storage capacity")?;
    loop {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() && !metadata.is_symlink() => return Ok(path),
            Ok(_) => return Err(capacity_error()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                path = match path.parent() {
                    Some(parent) if !parent.as_os_str().is_empty() => parent,
                    Some(_) if path != Path::new(".") => Path::new("."),
                    // Do not spin on a missing current directory or switch
                    // filesystems when the absolute root is unavailable.
                    _ => return Err(capacity_error()),
                };
            }
            Err(_) => return Err(capacity_error()),
        }
    }
}

fn capacity_error() -> IndexRebuildError {
    IndexRebuildError::Index(
        "index_storage_capacity_unavailable: cannot inspect the index destination filesystem; check its permissions and mount, or choose a different --index-dir; generation was not published".to_owned(),
    )
}

pub(super) fn filesystem_capacity(path: &Path) -> Result<Capacity, IndexRebuildError> {
    #[cfg(unix)]
    {
        let stat = rustix::fs::statvfs(path).map_err(|_| capacity_error())?;
        let block_size = if stat.f_frsize == 0 {
            stat.f_bsize
        } else {
            stat.f_frsize
        };
        let available_bytes = stat
            .f_bavail
            .checked_mul(block_size)
            .ok_or_else(capacity_error)?;
        Ok(Capacity {
            available_bytes,
            available_inodes: (stat.f_files != 0 && stat.f_favail != u64::MAX)
                .then_some(stat.f_favail),
        })
    }
    #[cfg(windows)]
    {
        fs4::available_space(path)
            .map(|available_bytes| Capacity {
                available_bytes,
                available_inodes: None,
            })
            .map_err(|_| capacity_error())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(capacity_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::index;
    use crate::db::{CreateWorkspaceInput, DbConnection};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::Duration;

    type TestResult = Result<(), String>;

    #[test]
    fn storage_estimate_covers_both_tiers_and_exact_boundaries() -> TestResult {
        let estimate = Estimate::from_sizes(100, 2, 256 + 768).map_err(|e| e.to_string())?;
        assert_eq!(estimate.vector_bytes, 8192);
        assert_eq!(estimate.required_bytes, 4 * (100 + 8192) + 80 * MIB);
        let exact = Capacity {
            available_bytes: estimate.required_bytes,
            available_inodes: Some(128),
        };
        assert!(estimate.check(exact).is_ok());
        assert!(
            estimate
                .check(Capacity {
                    available_bytes: exact.available_bytes - 1,
                    ..exact
                })
                .is_err()
        );
        assert!(
            estimate
                .check(Capacity {
                    available_inodes: Some(127),
                    ..exact
                })
                .is_err()
        );
        assert!(
            estimate
                .check(Capacity {
                    available_inodes: None,
                    ..exact
                })
                .is_ok()
        );
        Ok(())
    }

    #[test]
    fn storage_estimate_overflow_is_never_admitted_as_a_small_build() {
        for sizes in [(u64::MAX, 1, 256), (0, u64::MAX, 256), (0, 1, u64::MAX)] {
            assert!(Estimate::from_sizes(sizes.0, sizes.1, sizes.2).is_err());
        }
        assert_eq!(
            Estimate::from_sizes(0, 0, 256)
                .expect("empty estimate")
                .required_bytes,
            80 * MIB
        );
    }

    #[test]
    fn staging_copy_preserves_exact_reserve_and_inode_boundaries() -> TestResult {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let path = root.path().canonicalize().map_err(|e| e.to_string())?;
        let target = path.join("not-created");
        for bytes in [0, 123_456] {
            let exact = Capacity {
                available_bytes: FREE_SPACE_RESERVE + bytes,
                available_inodes: Some(16),
            };
            assert!(admit_copy(&target, bytes, |_| Ok(exact)).is_ok());
            assert!(
                admit_copy(&target, bytes, |_| Ok(Capacity {
                    available_inodes: None,
                    ..exact
                }))
                .is_ok()
            );
            for deficient in [
                Capacity {
                    available_bytes: exact.available_bytes - 1,
                    ..exact
                },
                Capacity {
                    available_inodes: Some(15),
                    ..exact
                },
            ] {
                let error = admit_copy(&target, bytes, |_| Ok(deficient)).unwrap_err();
                assert!(error.to_string().contains("index_storage_"));
                assert!(
                    !error
                        .to_string()
                        .contains(&path.to_string_lossy().to_string())
                );
            }
        }
        let overflow = admit_copy(&target, u64::MAX, |_| panic!("overflow must not probe"));
        assert!(
            overflow
                .unwrap_err()
                .to_string()
                .contains("index_storage_estimate_overflow")
        );
        assert!(admit_copy(&target, 0, |_| Err(capacity_error())).is_err());
        assert!(
            !target.exists(),
            "capacity checks never allocate a destination"
        );
        Ok(())
    }

    #[test]
    fn delta_admission_includes_the_complete_digest_manifest() -> TestResult {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let target = root
            .path()
            .canonicalize()
            .map_err(|e| e.to_string())?
            .join("index");
        crate::core::run_cli_with_cx(Duration::from_secs(5), |cx| async move {
            let stack = index::hash_fallback_embedder_stack();
            let documents = (0..256)
                .map(|number| IndexableDocument::new(format!("doc-{number}"), "Café release"))
                .collect::<Vec<_>>();
            let digests = index::delta::DocDigests::new(&stack, &documents);
            let encoded = serde_json::to_vec(&digests).map_err(|e| e.to_string())?;
            let required = estimate(&cx, &stack, &[])
                .map_err(|e| e.to_string())?
                .required_bytes
                + encoded.len() as u64;
            let exact = Capacity {
                available_bytes: required,
                available_inodes: Some(128),
            };
            let delta = index::delta::Delta::default();
            let digest_bytes = serialized_bytes(&digests).map_err(|e| e.to_string())?;
            assert!(admit_delta(&cx, &target, &stack, &delta, digest_bytes, |_| Ok(exact)).is_ok());
            let error = admit_delta(&cx, &target, &stack, &delta, digest_bytes, |_| {
                Ok(Capacity {
                    available_bytes: required - 1,
                    ..exact
                })
            })
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("index_storage_bytes_insufficient")
            );
            assert!(!target.exists());
            Ok::<(), String>(())
        })
        .map_err(|e| e.to_string())?
    }

    #[test]
    fn storage_probe_uses_destination_ancestor_without_creating_it() -> TestResult {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let root_path = root.path().canonicalize().map_err(|e| e.to_string())?;
        let destination = root_path.join("absent").join("index");
        assert_eq!(
            existing_directory(&destination).map_err(|e| e.to_string())?,
            root_path
        );
        let capacity = filesystem_capacity(&root_path).map_err(|e| e.to_string())?;
        assert!(capacity.available_bytes > 0);
        assert!(!root_path.join("absent").exists());
        assert!(existing_directory(Path::new("")).is_err());
        std::fs::write(root_path.join("file"), "unchanged").map_err(|e| e.to_string())?;
        assert!(existing_directory(&root_path.join("file")).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn storage_probe_rejects_symlinked_destinations() -> TestResult {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let root_path = root.path().canonicalize().map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink(&root_path, root_path.join("alias"))
            .map_err(|e| e.to_string())?;
        assert!(existing_directory(&root_path.join("alias/index")).is_err());
        Ok(())
    }

    #[test]
    fn storage_estimate_counts_unicode_titles_and_structured_metadata() -> TestResult {
        crate::core::run_cli_with_cx(Duration::from_secs(5), |cx| async move {
            let stack = super::super::hash_fallback_embedder_stack();
            let plain = IndexableDocument::new("doc", "Café");
            let enriched = IndexableDocument::new("doc", "Café").with_title("Release 日本語");
            let a = estimate(&cx, &stack, &[plain]).map_err(|e| e.to_string())?;
            let b = estimate(&cx, &stack, &[enriched]).map_err(|e| e.to_string())?;
            assert!(a.source_bytes >= "docCafé{}".len() as u64);
            assert_eq!(
                b.source_bytes - a.source_bytes,
                "Release 日本語".len() as u64
            );
            assert_eq!(a.vector_bytes, b.vector_bytes);
            Ok::<(), String>(())
        })
        .map_err(|e| e.to_string())?
    }

    #[test]
    fn storage_refusal_precedes_real_tier_build_and_preserves_existing_generation() -> TestResult {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let path = root.path().canonicalize().map_err(|e| e.to_string())?;
        std::fs::create_dir(path.join("active")).map_err(|e| e.to_string())?;
        std::fs::write(path.join("active/evidence"), "previous generation")
            .map_err(|e| e.to_string())?;
        crate::core::run_cli_with_cx(Duration::from_secs(5), |cx| async move {
            for (number, capacity) in [
                Capacity {
                    available_bytes: 0,
                    available_inodes: Some(1000),
                },
                Capacity {
                    available_bytes: u64::MAX,
                    available_inodes: Some(0),
                },
            ]
            .into_iter()
            .enumerate()
            {
                let destination = path.join(format!("staged-{number}"));
                let result = super::super::build_index_generation_with_capacity(
                    &cx,
                    &destination,
                    super::super::hash_fallback_embedder_stack(),
                    vec![IndexableDocument::new(
                        "doc",
                        "Run cargo fmt before release.",
                    )],
                    |_| Ok(capacity),
                )
                .await;
                assert!(result.is_err());
                assert!(
                    !destination.exists(),
                    "admission must precede all tier writes"
                );
                assert_eq!(
                    std::fs::read(path.join("active/evidence")).map_err(|e| e.to_string())?,
                    b"previous generation"
                );
            }
            Ok::<(), String>(())
        })
        .map_err(|e| e.to_string())?
    }

    #[test]
    fn storage_probe_failure_and_cancellation_do_not_build_empty_tiers() -> TestResult {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let destination = root
            .path()
            .canonicalize()
            .map_err(|e| e.to_string())?
            .join("index");
        crate::core::run_cli_with_cx(Duration::from_secs(5), |cx| async move {
            let error = super::super::build_index_generation_with_capacity(
                &cx,
                &destination,
                super::super::hash_fallback_embedder_stack(),
                vec![],
                |_| Err(capacity_error()),
            )
            .await;
            assert!(error.is_err());
            assert!(!destination.exists());
            cx.set_cancel_reason(asupersync::CancelReason::user("stop capacity admission"));
            let cancelled = super::super::build_index_generation_with_capacity(
                &cx,
                &destination,
                super::super::hash_fallback_embedder_stack(),
                vec![],
                |_| panic!("cancelled requests must not probe capacity"),
            )
            .await;
            assert!(matches!(cancelled, Err(IndexRebuildError::Cancelled(_))));
            assert!(!destination.exists());
            Ok::<(), String>(())
        })
        .map_err(|e| e.to_string())?
    }

    #[test]
    fn storage_admitted_generation_builds_real_tiers() -> TestResult {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let destination = root
            .path()
            .canonicalize()
            .map_err(|e| e.to_string())?
            .join("index");
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            let stats = super::super::build_index_generation_with_capacity(
                &cx,
                &destination,
                super::super::hash_fallback_embedder_stack(),
                vec![IndexableDocument::new(
                    "doc",
                    "Run cargo fmt before release.",
                )],
                filesystem_capacity,
            )
            .await
            .map_err(|e| e.to_string())?;
            assert_eq!(stats.doc_count, 1);
            assert_eq!(stats.error_count, 0);
            assert!(
                destination
                    .join(super::super::VECTOR_INDEX_FAST_FILE)
                    .is_file()
            );
            #[cfg(feature = "lexical-bm25")]
            assert!(
                destination
                    .join(super::super::LEXICAL_INDEX_SUBDIR)
                    .is_dir()
            );
            Ok::<(), String>(())
        })
        .map_err(|e| e.to_string())?
    }

    #[test]
    fn storage_post_build_capacity_loss_preserves_the_active_generation() -> TestResult {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let parent = root.path().canonicalize().map_err(|e| e.to_string())?;
        let active = parent.join("active");
        std::fs::create_dir(&active).map_err(|e| e.to_string())?;
        std::fs::write(active.join("body"), b"previous generation").map_err(|e| e.to_string())?;
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            for failure in 0..3 {
                let staged = parent.join(format!("staged-{failure}"));
                let mut calls = 0;
                let result = super::super::build_index_generation_with_capacity(
                    &cx,
                    &staged,
                    super::super::hash_fallback_embedder_stack(),
                    vec![IndexableDocument::new(
                        "doc",
                        "Run cargo fmt before release.",
                    )],
                    |_| {
                        calls += 1;
                        if calls == 1 {
                            return Ok(Capacity {
                                available_bytes: u64::MAX,
                                available_inodes: Some(1000),
                            });
                        }
                        match failure {
                            0 => Ok(Capacity {
                                available_bytes: 0,
                                available_inodes: Some(1000),
                            }),
                            1 => Ok(Capacity {
                                available_bytes: u64::MAX,
                                available_inodes: Some(0),
                            }),
                            _ => Err(capacity_error()),
                        }
                    },
                )
                .await;
                assert!(result.is_err());
                assert_eq!(
                    calls, 2,
                    "the second probe must observe capacity after tier allocation"
                );
                assert!(
                    staged.join(super::super::VECTOR_INDEX_FAST_FILE).is_file(),
                    "this must fail after a real build, not at initial admission"
                );
                assert_eq!(
                    std::fs::read(active.join("body")).map_err(|e| e.to_string())?,
                    b"previous generation"
                );
            }
            Ok::<(), String>(())
        })
        .map_err(|e| e.to_string())?
    }

    #[test]
    fn storage_post_build_reserve_has_exact_boundaries_and_preserves_cancellation() -> TestResult {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let path = root.path().canonicalize().map_err(|e| e.to_string())?;
        crate::core::run_cli_with_cx(Duration::from_secs(5), |cx| async move {
            let exact = Capacity {
                available_bytes: FREE_SPACE_RESERVE,
                available_inodes: Some(16),
            };
            assert!(confirm_reserve(&cx, &path, |_| Ok(exact)).is_ok());
            assert!(
                confirm_reserve(&cx, &path, |_| Ok(Capacity {
                    available_inodes: None,
                    ..exact
                }))
                .is_ok()
            );
            for deficient in [
                Capacity {
                    available_bytes: FREE_SPACE_RESERVE - 1,
                    ..exact
                },
                Capacity {
                    available_inodes: Some(15),
                    ..exact
                },
            ] {
                let error = confirm_reserve(&cx, &path, |_| Ok(deficient)).unwrap_err();
                assert!(
                    error
                        .to_string()
                        .contains("index_storage_reserve_exhausted")
                );
                assert!(
                    !error
                        .to_string()
                        .contains(&path.to_string_lossy().to_string())
                );
            }
            cx.set_cancel_reason(asupersync::CancelReason::user("stop publication admission"));
            assert!(matches!(
                confirm_reserve(&cx, &path, |_| panic!("cancelled request probed storage")),
                Err(IndexRebuildError::Cancelled(_))
            ));
            Ok::<(), String>(())
        })
        .map_err(|e| e.to_string())?
    }

    fn generation_files(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>, String> {
        let mut files = BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
                let path = entry.map_err(|e| e.to_string())?.path();
                let kind = std::fs::symlink_metadata(&path)
                    .map_err(|e| e.to_string())?
                    .file_type();
                if kind.is_dir() {
                    pending.push(path);
                } else if kind.is_file() {
                    files.insert(
                        path.strip_prefix(root)
                            .map_err(|e| e.to_string())?
                            .to_path_buf(),
                        std::fs::read(path).map_err(|e| e.to_string())?,
                    );
                } else {
                    return Err("unexpected generation entry".to_owned());
                }
            }
        }
        Ok(files)
    }

    #[test]
    fn delta_capacity_refusals_preserve_live_readers_and_guard_full_fallback() -> TestResult {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let parent = root.path().canonicalize().map_err(|e| e.to_string())?;
        crate::core::run_cli_with_cx(Duration::from_secs(90), |cx| async move {
            let _root = root;
            let live = parent.join("index");
            let db = DbConnection::open_memory().map_err(|e| e.to_string())?;
            db.migrate().map_err(|e| e.to_string())?;
            let workspace = "wsp_01234567890123456789012345";
            db.insert_workspace(
                workspace,
                &CreateWorkspaceInput {
                    path: parent.to_string_lossy().into_owned(),
                    name: Some("delta capacity admission".to_owned()),
                },
            )
            .map_err(|e| e.to_string())?;
            let lock = index::IndexPublishLockOwner::acquire(&cx, &db, workspace)
                .map_err(|e| e.to_string())?;
            let stack = EmbedderStack::from_parts(
                std::sync::Arc::new(crate::search::HashEmbedder::default_256()),
                Some(std::sync::Arc::new(crate::search::HashEmbedder::jl_384(17))),
            );
            let original = (0..8)
                .map(|number| {
                    IndexableDocument::new(format!("doc-{number}"), format!("old lesson {number}"))
                })
                .collect::<Vec<_>>();
            let counts = index::IndexDocumentCounts::memory_only(8);
            index::publish_full_index_generation_with_stack(
                &lock,
                &live,
                stack.clone(),
                original.clone(),
                1,
                counts,
                || Ok(()),
            )
            .await
            .map_err(|e| e.to_string())?;
            let before = generation_files(&live)?;
            let reader = index::open_fast_vector_index_read_only(&live).map_err(|e| e.detail)?;
            let query = [0.1_f32; 256];
            let old_hits = reader
                .search_top_k(&query, 8, None)
                .map_err(|e| e.to_string())?;
            assert_eq!(
                old_hits.len(),
                8,
                "the live-reader control must be populated"
            );
            let fast_bytes = std::fs::metadata(live.join(index::VECTOR_INDEX_FAST_FILE))
                .map_err(|e| e.to_string())?
                .len();
            let quality_bytes = std::fs::metadata(live.join(index::VECTOR_INDEX_QUALITY_FILE))
                .map_err(|e| e.to_string())?
                .len();
            assert!(fast_bytes > 0 && quality_bytes > 0);
            let input_and_manifest_bytes = |delta: &index::delta::Delta,
                                            digests: &index::delta::DocDigests|
             -> Result<u64, String> {
                Ok(estimate(&cx, &stack, &delta.upserts)
                    .map_err(|e| e.to_string())?
                    .required_bytes
                    + serde_json::to_vec(digests)
                        .map_err(|e| e.to_string())?
                        .len() as u64)
            };
            let mut metadata_only = original.clone();
            metadata_only[0].title = Some("new metadata, identical vector input".to_owned());
            let metadata_delta =
                index::delta::plan(&live, &stack, &metadata_only).map_err(|e| e.detail)?;
            let metadata_digests = index::delta::DocDigests::new(&stack, &metadata_only);
            assert!(
                admit_delta(
                    &cx,
                    &live,
                    &stack,
                    &metadata_delta,
                    serialized_bytes(&metadata_digests).map_err(|e| e.to_string())?,
                    |_| Ok(Capacity {
                        available_bytes: input_and_manifest_bytes(
                            &metadata_delta,
                            &metadata_digests
                        )
                        .map_err(IndexRebuildError::Index)?,
                        available_inodes: Some(128),
                    }),
                )
                .is_ok(),
                "metadata-only changes must not pay for a vector rewrite"
            );

            let mut current = original.clone();
            current[0].content = "a new repair replaces the original lesson".to_owned();
            for changed in [&current[..], &original[1..]] {
                let delta = index::delta::plan(&live, &stack, changed).map_err(|e| e.detail)?;
                let digests = index::delta::DocDigests::new(&stack, changed);
                let digest_bytes = serialized_bytes(&digests).map_err(|e| e.to_string())?;
                let input_bytes = input_and_manifest_bytes(&delta, &digests)?;
                let exact = input_bytes + fast_bytes + quality_bytes;
                for available_bytes in [input_bytes, input_bytes + fast_bytes, exact - 1] {
                    let error = admit_delta(&cx, &live, &stack, &delta, digest_bytes, |_| {
                        Ok(Capacity {
                            available_bytes,
                            available_inodes: Some(128),
                        })
                    })
                    .unwrap_err();
                    assert!(
                        error
                            .to_string()
                            .contains("index_storage_bytes_insufficient")
                    );
                }
                admit_delta(&cx, &live, &stack, &delta, digest_bytes, |_| {
                    Ok(Capacity {
                        available_bytes: exact,
                        available_inodes: Some(128),
                    })
                })
                .map_err(|e| e.to_string())?;
            }
            assert_eq!(generation_files(&live)?, before);

            for phase in 1..=4 {
                for failure in 0..3 {
                    let deficient = || match failure {
                        0 => Ok(Capacity {
                            available_bytes: 0,
                            available_inodes: Some(1000),
                        }),
                        1 => Ok(Capacity {
                            available_bytes: u64::MAX,
                            available_inodes: Some(0),
                        }),
                        _ => Err(capacity_error()),
                    };
                    let applied =
                        index::delta::plan(&live, &stack, &current).map_err(|e| e.detail)?;
                    assert_eq!(applied.upserts.len(), 1);
                    let mut calls = 0;
                    let staged = index::stage_planned_index_delta_with_capacity(
                        &lock,
                        &live,
                        &stack,
                        applied,
                        index::delta::DocDigests::new(&stack, &current),
                        counts,
                        |path| {
                            calls += 1;
                            if calls == 2 {
                                assert!(path.join(index::VECTOR_INDEX_FAST_FILE).is_file());
                                assert!(!path.join(index::delta::DOC_DIGESTS_FILE).exists());
                            }
                            if calls >= 3 {
                                assert_eq!(
                                    path.join(index::delta::DOC_DIGESTS_FILE).is_file(),
                                    calls == 4
                                );
                                index::verify_published_tier_counts(path, 8, true)
                                    .map_err(IndexRebuildError::Index)?;
                            }
                            if calls == phase {
                                deficient()
                            } else {
                                Ok(Capacity {
                                    available_bytes: u64::MAX,
                                    available_inodes: Some(1000),
                                })
                            }
                        },
                    )
                    .await;
                    let Err(index::DeltaStageError::Refused(reason)) = staged else {
                        return Err("capacity failure must refuse the staged delta".to_owned());
                    };
                    assert!(reason.contains("index_storage_"), "{reason}");
                    assert_eq!(calls, phase);
                    assert_eq!(generation_files(&live)?, before);
                    assert_eq!(
                        reader
                            .search_top_k(&query, 8, None)
                            .map_err(|e| e.to_string())?,
                        old_hits
                    );
                    index::verify_published_tier_counts(&live, 8, true)?;

                    // The refusal's ordinary full-build fallback must apply
                    // its own admission, not bypass the same exhausted disk.
                    let fallback = parent.join(format!("fallback-{phase}-{failure}"));
                    let rebuilt = index::build_index_generation_with_capacity(
                        &cx,
                        &fallback,
                        stack.clone(),
                        current.clone(),
                        |_| deficient(),
                    )
                    .await;
                    assert!(rebuilt.is_err());
                    assert!(!fallback.exists());
                }
            }

            let applied = index::delta::plan(&live, &stack, &current).map_err(|e| e.detail)?;
            let mut calls = 0;
            let staged = index::stage_planned_index_delta_with_capacity(
                &lock,
                &live,
                &stack,
                applied,
                index::delta::DocDigests::new(&stack, &current),
                counts,
                |_| {
                    calls += 1;
                    Ok(Capacity {
                        available_bytes: u64::MAX,
                        available_inodes: Some(1000),
                    })
                },
            )
            .await;
            let (stage, _) = match staged {
                Ok(staged) => staged,
                Err(index::DeltaStageError::Refused(reason)) => return Err(reason),
                Err(index::DeltaStageError::Cancelled(error)) => return Err(error.to_string()),
            };
            assert_eq!(calls, 4);
            let fingerprint = index::embedder_fingerprint_for_index_metadata(&stack);
            index::publish_staged_generation(
                &lock,
                &live,
                stage,
                2,
                counts,
                fingerprint.as_ref(),
                || Ok(()),
            )
            .await
            .map_err(|e| e.to_string())?;
            assert_eq!(index::read_index_metadata(&live).generation, Some(2));
            assert_ne!(generation_files(&live)?, before);
            index::verify_published_tier_counts(&live, 8, true)?;
            Ok::<(), String>(())
        })
        .map_err(|e| e.to_string())?
    }

    #[test]
    fn cancellation_during_capacity_probe_wins_over_a_probe_failure() -> TestResult {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let path = root.path().canonicalize().map_err(|e| e.to_string())?;
        for phase in 0..4 {
            let path = path.clone();
            crate::core::run_cli_with_cx(Duration::from_secs(5), |cx| async move {
                let stack = index::hash_fallback_embedder_stack();
                let digests = index::delta::DocDigests::new(&stack, &[]);
                let digest_bytes = serialized_bytes(&digests).map_err(|e| e.to_string())?;
                let probe = |_: &Path| {
                    cx.set_cancel_reason(asupersync::CancelReason::user("cancel storage probe"));
                    Err(capacity_error())
                };
                let result = match phase {
                    0 => admit(&cx, &path, &stack, &[], probe),
                    1 => admit_delta(
                        &cx,
                        &path,
                        &stack,
                        &index::delta::Delta::default(),
                        digest_bytes,
                        probe,
                    ),
                    2 => admit_manifest(&cx, &path, digest_bytes, probe),
                    _ => confirm_reserve(&cx, &path, probe),
                };
                assert!(matches!(
                    result,
                    Err(IndexRebuildError::Cancelled(reason))
                        if reason.message.as_deref() == Some("cancel storage probe")
                ));
                Ok::<(), String>(())
            })
            .map_err(|e| e.to_string())??;
        }
        Ok(())
    }
}
