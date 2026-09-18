//! Storage admission before a generation allocates its vector and lexical tiers.
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
    let ancestor = existing_directory(index_dir)?;
    let capacity = probe(ancestor)?;
    index_checkpoint(cx)?;
    estimate.check(capacity)
}

fn existing_directory(mut path: &Path) -> Result<&Path, IndexRebuildError> {
    super::ensure_index_path_has_no_symlinks(path, "inspect index storage capacity")?;
    loop {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() && !metadata.is_symlink() => return Ok(path),
            Ok(_) => return Err(capacity_error()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                path = path
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."));
            }
            Err(_) => return Err(capacity_error()),
        }
    }
}

fn capacity_error() -> IndexRebuildError {
    IndexRebuildError::Index(
        "index_storage_capacity_unavailable: cannot inspect the index destination filesystem; check its permissions and mount, or choose a different --index-dir; generation build was not started".to_owned(),
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
}
