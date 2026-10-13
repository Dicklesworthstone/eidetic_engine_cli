//! Unix reader/publication coordination for the existing directory layout.
//!
//! An atomic rename protects a pathname lookup, not a sequence of metadata,
//! vector and Tantivy opens. Hold a shared lease until those reads finish;
//! publishers hold the exclusive lease through job commit AND rollback.
//!
//! The existing parent directory is the lock object. It never moves during
//! generation exchange, needs no writable handle, and introduces no sidecar,
//! PID record or lease renewal. Sibling indexes share this coordination domain.
//! This is deliberately not the future SQL-pointer/immutable-generation design.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{IndexRebuildError, ensure_index_path_has_no_symlinks, index_checkpoint, index_parent};
use rustix::fs::{FlockOperation, OFlags, flock};
use rustix::io::Errno;

// A hook has other work to do inside its 9--10 second outer deadline. Do not
// spend the publisher's entire grace period merely waiting to start a read.
const MAX_READ_WAIT: Duration = Duration::from_millis(250);
const MAX_WAIT: Duration = Duration::from_secs(5);
const MAX_INSPECTION_ENTRIES: usize = 4096;
const INSPECTION_WALL_BUDGET: Duration = Duration::from_secs(1);
const RETRY_DELAY: Duration = Duration::from_millis(5);

/// One cooperative budget for the entire read-only generation selection,
/// including failed candidates. Never reset it when trying another retained
/// directory. This bounds directory work, retained-path allocation and starting
/// another candidate, not a synchronous backend syscall. A successful backend
/// open still obeys the caller's cancellation/budget before its result is used.
struct InspectionBudget {
    request: crate::core::budget::RequestBudget,
    entries_remaining: usize,
}

impl InspectionBudget {
    fn new() -> Self {
        Self {
            request: crate::core::budget::RequestBudget::unbounded()
                .with_wall_clock(INSPECTION_WALL_BUDGET),
            entries_remaining: MAX_INSPECTION_ENTRIES,
        }
    }

    fn checkpoint(&self, cx: &asupersync::Cx) -> Result<(), IndexRebuildError> {
        // Preserve an explicit cancellation, even when both budgets expired.
        index_checkpoint(cx)?;
        self.request.check().map_err(|_| inspection_budget_error())
    }

    fn entry(&mut self, cx: &asupersync::Cx) -> Result<(), IndexRebuildError> {
        self.checkpoint(cx)?;
        self.entries_remaining = self
            .entries_remaining
            .checked_sub(1)
            .ok_or_else(inspection_budget_error)?;
        Ok(())
    }
}

fn inspection_budget_error() -> IndexRebuildError {
    IndexRebuildError::Index(
        "Index generation inspection exceeded its read budget; no generation was admitted; retry after publication or inspect retained generations before explicit index maintenance".to_owned(),
    )
}

/// Closing the sole owned descriptor releases its OS lease on success, error,
/// cancellation, unwinding, or process exit. Never clone/upgrade this handle.
#[must_use = "the guard must stay alive until all generation reads have finished"]
#[derive(Debug)]
pub(crate) struct IndexGenerationLease {
    _directory: File,
}

impl IndexGenerationLease {
    /// Select a complete generation no newer than the pinned source snapshot.
    ///
    /// The live directory is preferred only after validating its actual tiers.
    /// Missing directories and damaged regular files can use a retained index;
    /// redirected or special entries cannot. Selection is read-only, and the
    /// caller must keep this lease through collection of both search arms.
    pub(crate) fn index_for_snapshot(
        &self,
        cx: &asupersync::Cx,
        index_dir: &Path,
        maximum_generation: u64,
    ) -> Result<PathBuf, IndexRebuildError> {
        index_checkpoint(cx)?;
        let identity = super::active_semantic_identity();
        self.index_for_snapshot_with_identity(
            cx,
            index_dir,
            maximum_generation,
            identity
                .as_ref()
                .map(|(id, dimension)| (id.as_str(), *dimension)),
        )
    }

    pub(crate) fn index_for_snapshot_with_identity(
        &self,
        cx: &asupersync::Cx,
        index_dir: &Path,
        maximum_generation: u64,
        identity: Option<(&str, u32)>,
    ) -> Result<PathBuf, IndexRebuildError> {
        let mut inspection = InspectionBudget::new();
        inspection.checkpoint(cx)?;
        let parent = index_parent(index_dir);
        verify_parent_identity(parent, &self._directory)?;
        ensure_index_path_has_no_symlinks(index_dir, "select snapshot index")?;
        super::ensure_index_publish_target_is_directory_or_missing(
            index_dir,
            "select snapshot index",
        )?;
        let metadata_path = index_dir.join(super::INDEX_METADATA_FILE);
        // Keep unsafe-entry refusals outside recoverable parse/backend errors.
        ensure_index_path_has_no_symlinks(&metadata_path, "select snapshot metadata")?;
        super::ensure_index_metadata_path_is_regular_or_missing(
            &metadata_path,
            "select snapshot metadata",
        )?;
        let current = super::parse_index_metadata(index_dir)
            .ok()
            .flatten()
            .filter(|metadata| {
                super::index_metadata_compatibility_error_with_identity(
                    &metadata_path,
                    metadata,
                    identity,
                )
                .is_none()
            })
            .and_then(|metadata| metadata.generation);
        inspection.checkpoint(cx)?;
        if let Some(generation) = current.filter(|generation| *generation <= maximum_generation) {
            inspect_generation_entries(cx, index_dir, &mut inspection)?;
            let valid = super::validated_index_generation_with_identity(index_dir, identity).ok()
                == Some(generation);
            index_checkpoint(cx)?;
            if valid {
                return Ok(index_dir.to_path_buf());
            }
            inspection.checkpoint(cx)?;
            // A valid manifest is not proof of a complete generation. Continue
            // to retained candidates after a missing/corrupt vector or lexical
            // tier, rather than returning a path the caller cannot open.
        }

        let prefix = format!(
            "{}{}",
            super::index_base_name(index_dir)?,
            super::INDEX_RETAINED_SUFFIX
        );
        let mut candidates = Vec::new();
        let entries = std::fs::read_dir(parent)
            .map_err(|error| lease_error("inspect retained generations", error))?;
        for entry in entries {
            inspection.entry(cx)?;
            let entry = entry.map_err(|error| lease_error("inspect retained generation", error))?;
            let path = entry.path();
            let Some(sequence) =
                super::retained_generation_sequence(&entry.file_name().to_string_lossy(), &prefix)
                    .or(super::displaced_generation_sequence(index_dir, &path)?)
            else {
                continue;
            };
            ensure_index_path_has_no_symlinks(&path, "select retained snapshot index")?;
            if !entry
                .file_type()
                .map_err(|error| lease_error("inspect retained generation", error))?
                .is_dir()
            {
                continue;
            }
            let metadata_path = path.join(super::INDEX_METADATA_FILE);
            ensure_index_path_has_no_symlinks(&metadata_path, "select retained metadata")?;
            super::ensure_index_metadata_path_is_regular_or_missing(
                &metadata_path,
                "select retained metadata",
            )?;
            // Admit metadata first, then open only the newest eligible tiers.
            // Missing watermarks never become an implicit generation zero.
            let Some(generation) = super::parse_index_metadata(&path)
                .ok()
                .flatten()
                .filter(|metadata| {
                    super::index_metadata_compatibility_error_with_identity(
                        &metadata_path,
                        metadata,
                        identity,
                    )
                    .is_none()
                })
                .and_then(|metadata| metadata.generation)
                .filter(|generation| *generation <= maximum_generation)
            else {
                continue;
            };
            candidates.push((generation, sequence, path));
        }
        candidates.sort();
        for (generation, _, path) in candidates.into_iter().rev() {
            inspection.checkpoint(cx)?;
            inspect_generation_entries(cx, &path, &mut inspection)?;
            let valid = super::validated_index_generation_with_identity(&path, identity).ok()
                == Some(generation);
            index_checkpoint(cx)?;
            if valid {
                return Ok(path);
            }
            inspection.checkpoint(cx)?;
        }
        inspection.checkpoint(cx)?;
        Err(IndexRebuildError::Index(
            if current.is_some_and(|generation| generation > maximum_generation) {
                "The live index is newer than the source snapshot and no compatible retained generation is available; retry with a fresh snapshot"
            } else {
                "No complete index generation is available for the source snapshot; rebuild the index"
            }
            .to_owned(),
        ))
    }

    /// Preserve the ordinary NoIndex result when neither a live directory nor
    /// any recognized retained directory exists. This is presence detection,
    /// not admission: every returned candidate still needs full validation.
    pub(crate) fn has_retained_generation_directory(
        &self,
        cx: &asupersync::Cx,
        index_dir: &Path,
    ) -> Result<bool, IndexRebuildError> {
        let mut inspection = InspectionBudget::new();
        inspection.checkpoint(cx)?;
        let parent = index_parent(index_dir);
        verify_parent_identity(parent, &self._directory)?;
        let prefix = format!(
            "{}{}",
            super::index_base_name(index_dir)?,
            super::INDEX_RETAINED_SUFFIX
        );
        for entry in std::fs::read_dir(parent)
            .map_err(|error| lease_error("inspect retained generation presence", error))?
        {
            inspection.entry(cx)?;
            let entry = entry.map_err(|error| lease_error("inspect retained entry", error))?;
            if super::retained_generation_sequence(&entry.file_name().to_string_lossy(), &prefix)
                .or(super::displaced_generation_sequence(
                    index_dir,
                    &entry.path(),
                )?)
                .is_none()
            {
                continue;
            }
            ensure_index_path_has_no_symlinks(&entry.path(), "inspect retained generation")?;
            if entry
                .file_type()
                .map_err(|error| lease_error("inspect retained entry", error))?
                .is_dir()
            {
                inspection.checkpoint(cx)?;
                return Ok(true);
            }
        }
        inspection.checkpoint(cx)?;
        Ok(false)
    }

    pub(crate) async fn read(
        cx: &asupersync::Cx,
        index_dir: &Path,
    ) -> Result<Self, IndexRebuildError> {
        Self::acquire(cx, index_dir, false, MAX_READ_WAIT).await
    }

    /// Best-effort lexical warming never waits for a publisher, discovers a
    /// retained generation, repairs files or initializes an embedding model.
    /// Recheck the exact generation and corpus/security contract AFTER acquiring
    /// the publication lease, then reject unsafe entries before the backend opens.
    pub(crate) async fn try_read_for_prefetch(
        cx: &asupersync::Cx,
        index_dir: &Path,
        expected_generation: u64,
    ) -> Result<Option<Self>, IndexRebuildError> {
        index_checkpoint(cx)?;
        ensure_index_path_has_no_symlinks(index_dir, "prefetch lexical generation")?;
        let lease = Self::acquire(cx, index_dir, false, Duration::ZERO).await?;
        super::ensure_index_publish_target_is_directory_or_missing(
            index_dir,
            "prefetch lexical generation",
        )?;
        let metadata_path = index_dir.join(super::INDEX_METADATA_FILE);
        let Some(metadata) =
            super::parse_index_metadata(index_dir).map_err(IndexRebuildError::Index)?
        else {
            return Ok(None);
        };
        if metadata.generation != Some(expected_generation)
            || super::index_metadata_compatibility_error(&metadata_path, &metadata).is_some()
        {
            return Ok(None);
        }
        index_checkpoint(cx)?;
        ensure_generation_entries_are_regular(cx, &index_dir.join("lexical"))?;
        Ok(Some(lease))
    }

    /// Acquire BEFORE the database writer fence: a reader may still need DB
    /// access while draining, so waiting with that writer fence would invert
    /// the lock order. Only an explicit index writer creates missing parents.
    pub(super) async fn publish(
        cx: &asupersync::Cx,
        index_dir: &Path,
    ) -> Result<Self, IndexRebuildError> {
        index_checkpoint(cx)?;
        let parent = index_parent(index_dir);
        ensure_index_path_has_no_symlinks(parent, "prepare index publication lease")?;
        std::fs::create_dir_all(parent).map_err(|error| lease_error("prepare", error))?;
        Self::acquire(cx, index_dir, true, MAX_WAIT).await
    }

    async fn acquire(
        cx: &asupersync::Cx,
        index_dir: &Path,
        exclusive: bool,
        max_wait: Duration,
    ) -> Result<Self, IndexRebuildError> {
        index_checkpoint(cx)?;
        let parent = index_parent(index_dir);
        let directory = open_directory(parent)?;
        let started = Instant::now();
        let mut attempted = false;
        loop {
            index_checkpoint(cx)?;
            // Always permit the first nonblocking attempt (including prefetch's
            // zero wait), but never acquire on a retry after an overdue timer.
            if attempted && started.elapsed() >= max_wait {
                return Err(generation_busy_error());
            }
            attempted = true;
            if try_lease(&directory, exclusive)? {
                // Opening and then waiting must not let a replaced parent
                // redirect subsequent pathname reads outside the leased inode.
                verify_parent_identity(parent, &directory)?;
                index_checkpoint(cx)?;
                return Ok(Self {
                    _directory: directory,
                });
            }
            let remaining = max_wait.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(generation_busy_error());
            }
            // Yield to the owning runtime, never block its worker with flock
            // or thread::sleep. Both the wall bound and caller budget apply.
            asupersync::time::sleep(cx.now(), RETRY_DELAY.min(remaining)).await;
        }
    }
}

/// Backend failure can authorize another generation, never path redirection.
/// Check directory entries without opening their bodies before a backend can
/// follow a segment, vector or manifest path. Cooperating publishers are held
/// out by the parent lease; this is not protection against arbitrary external
/// writers that ignore that lease and race subsequent backend opens.
fn ensure_generation_entries_are_regular(
    cx: &asupersync::Cx,
    root: &Path,
) -> Result<(), IndexRebuildError> {
    inspect_generation_entries(cx, root, &mut InspectionBudget::new())
}

fn inspect_generation_entries(
    cx: &asupersync::Cx,
    root: &Path,
    inspection: &mut InspectionBudget,
) -> Result<(), IndexRebuildError> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        inspection.entry(cx)?;
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| lease_error("inspect generation entry", error))?;
        if metadata.is_dir() {
            for entry in std::fs::read_dir(&path)
                .map_err(|error| lease_error("inspect generation directory", error))?
            {
                inspection.entry(cx)?;
                let entry =
                    entry.map_err(|error| lease_error("inspect generation entry", error))?;
                let kind = entry
                    .file_type()
                    .map_err(|error| lease_error("inspect generation entry", error))?;
                if kind.is_dir() {
                    pending.push(entry.path());
                } else if !kind.is_file() {
                    return Err(IndexRebuildError::Index(
                        "Refusing to read an index generation containing a symlink or special entry"
                            .to_owned(),
                    ));
                }
            }
        } else {
            return Err(IndexRebuildError::Index(
                "Refusing to read an index generation whose directory was replaced".to_owned(),
            ));
        }
    }
    inspection.checkpoint(cx)
}

fn open_directory(parent: &Path) -> Result<File, IndexRebuildError> {
    ensure_index_path_has_no_symlinks(parent, "lease index generation")?;
    OpenOptions::new()
        .read(true)
        .custom_flags((OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
        .open(parent)
        .map_err(|error| lease_error("open", error))
}

fn try_lease(directory: &File, exclusive: bool) -> Result<bool, IndexRebuildError> {
    let operation = if exclusive {
        FlockOperation::NonBlockingLockExclusive
    } else {
        FlockOperation::NonBlockingLockShared
    };
    match flock(directory, operation) {
        Ok(()) => Ok(true),
        Err(error) if matches!(error, Errno::WOULDBLOCK | Errno::INTR) => Ok(false),
        Err(error) => Err(lease_error("acquire", error)),
    }
}

fn verify_parent_identity(parent: &Path, directory: &File) -> Result<(), IndexRebuildError> {
    ensure_index_path_has_no_symlinks(parent, "validate index generation lease")?;
    let held = directory
        .metadata()
        .map_err(|error| lease_error("inspect", error))?;
    let current =
        std::fs::symlink_metadata(parent).map_err(|error| lease_error("inspect", error))?;
    if !held.is_dir()
        || !current.is_dir()
        || held.dev() != current.dev()
        || held.ino() != current.ino()
    {
        return Err(IndexRebuildError::Index(
            "Index generation parent changed while acquiring its lease; retry the operation"
                .to_owned(),
        ));
    }
    Ok(())
}

fn generation_busy_error() -> IndexRebuildError {
    IndexRebuildError::Index(
        "Index generation is busy; no mixed-generation read or publication was attempted; retry the operation".to_owned(),
    )
}

fn lease_error(action: &str, error: impl std::fmt::Display) -> IndexRebuildError {
    IndexRebuildError::Index(format!(
        "Could not {action} index generation lease: {error}"
    ))
}

#[cfg(test)]
#[path = "index_snapshot_recovery_tests.rs"]
mod snapshot_recovery_tests;

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn generation_lease_shared_readers_exclude_writer_without_creating_files() -> TestResult {
        let (root, index) = fixture()?;
        let parent = index_parent(&index);
        let first = open_directory(parent).map_err(|error| error.to_string())?;
        let second = open_directory(parent).map_err(|error| error.to_string())?;
        let writer = open_directory(parent).map_err(|error| error.to_string())?;
        assert!(try_lease(&first, false).map_err(|error| error.to_string())?);
        assert!(try_lease(&second, false).map_err(|error| error.to_string())?);
        assert!(!try_lease(&writer, true).map_err(|error| error.to_string())?);
        drop(first);
        assert!(!try_lease(&writer, true).map_err(|error| error.to_string())?);
        drop(second);
        assert!(try_lease(&writer, true).map_err(|error| error.to_string())?);
        assert_eq!(
            std::fs::read_dir(root.path())
                .map_err(|error| error.to_string())?
                .count(),
            0
        );
        Ok(())
    }

    #[test]
    fn generation_lease_contention_is_bounded_and_preserves_cancellation() -> TestResult {
        let (_root, index) = fixture()?;
        let held = open_directory(index_parent(&index)).map_err(|error| error.to_string())?;
        assert!(try_lease(&held, true).map_err(|error| error.to_string())?);
        crate::core::run_cli_with_cx(Duration::from_secs(5), |cx| async move {
            let busy = IndexGenerationLease::acquire(&cx, &index, false, Duration::ZERO).await;
            assert!(
                matches!(busy, Err(IndexRebuildError::Index(message)) if message.contains("busy"))
            );
            cx.set_cancel_reason(asupersync::CancelReason::user(
                "stop waiting for generation",
            ));
            let cancelled = IndexGenerationLease::read(&cx, &index).await;
            assert!(
                matches!(cancelled, Err(IndexRebuildError::Cancelled(reason))
                if reason.message.as_deref() == Some("stop waiting for generation"))
            );
            drop(held);
            let fresh = open_directory(index_parent(&index)).map_err(|error| error.to_string())?;
            assert!(try_lease(&fresh, true).map_err(|error| error.to_string())?);
            Ok::<(), String>(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn generation_lease_unwind_and_independent_workspaces_release_authority() -> TestResult {
        let (_root, index) = fixture()?;
        let (_other_root, other_index) = fixture()?;
        let held = open_directory(index_parent(&index)).map_err(|error| error.to_string())?;
        assert!(try_lease(&held, false).map_err(|error| error.to_string())?);
        let unrelated =
            open_directory(index_parent(&other_index)).map_err(|error| error.to_string())?;
        assert!(try_lease(&unrelated, true).map_err(|error| error.to_string())?);
        drop(held);
        let unwind = std::panic::catch_unwind(|| {
            let held = open_directory(index_parent(&index)).expect("open publisher lease");
            assert!(try_lease(&held, true).expect("acquire publisher lease"));
            let _guard = IndexGenerationLease { _directory: held };
            panic!("controlled publication unwind");
        });
        assert!(unwind.is_err());
        let reader = open_directory(index_parent(&index)).map_err(|error| error.to_string())?;
        assert!(try_lease(&reader, false).map_err(|error| error.to_string())?);
        Ok(())
    }

    #[test]
    fn generation_lease_rejects_symlinks_and_parent_substitution() -> TestResult {
        use std::os::unix::fs::symlink;
        let (root, _index) = fixture()?;
        let parent = root.path().join("parent");
        std::fs::create_dir(&parent).map_err(|error| error.to_string())?;
        let held = open_directory(&parent).map_err(|error| error.to_string())?;
        std::fs::rename(&parent, root.path().join("retained-parent"))
            .map_err(|error| error.to_string())?;
        std::fs::create_dir(&parent).map_err(|error| error.to_string())?;
        assert!(verify_parent_identity(&parent, &held).is_err());
        let alias = root.path().join("alias");
        symlink(&parent, &alias).map_err(|error| error.to_string())?;
        assert!(open_directory(&alias).is_err());
        assert!(open_directory(&root.path().join("missing")).is_err());
        assert!(!root.path().join("missing").exists());
        Ok(())
    }

    // Exercise the production serializer and real read-only backend validator,
    // not a manifest-only fake that could certify a generation with no tiers.
    async fn build_generation(cx: &asupersync::Cx, path: &Path, generation: u64) -> TestResult {
        let documents = vec![crate::search::IndexableDocument::new(
            "mem_snapshot_recovery",
            "Retained generations keep snapshot recovery read-only and bounded.",
        )];
        super::super::IndexBuilder::new(path)
            .with_embedder_stack(super::super::hash_fallback_embedder_stack())
            .add_documents(documents.clone())
            .build(cx)
            .await
            .map_err(|error| error.to_string())?;
        #[cfg(feature = "lexical-bm25")]
        super::super::build_lexical_tier(cx, path, &documents)
            .await
            .map_err(|error| error.to_string())?;
        super::super::write_index_metadata(
            path,
            generation,
            super::super::IndexDocumentCounts::memory_only(1),
            None,
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(super::super::validated_index_generation(path)?, generation);
        Ok(())
    }

    fn file_snapshot(root: &Path) -> Result<std::collections::BTreeMap<PathBuf, Vec<u8>>, String> {
        let mut files = std::collections::BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
                let entry = entry.map_err(|error| error.to_string())?;
                let path = entry.path();
                let kind = entry.file_type().map_err(|error| error.to_string())?;
                if kind.is_dir() {
                    pending.push(path);
                } else {
                    assert!(
                        kind.is_file(),
                        "snapshot fixture must contain only regular files"
                    );
                    files.insert(
                        path.strip_prefix(root)
                            .map_err(|error| error.to_string())?
                            .to_path_buf(),
                        std::fs::read(path).map_err(|error| error.to_string())?,
                    );
                }
            }
        }
        Ok(files)
    }

    #[test]
    fn snapshot_read_recovers_missing_malformed_and_obsolete_metadata_without_writes() -> TestResult
    {
        for manifest in [None, Some("{broken"), Some(r#"{"generation":5}"#)] {
            let (_root, index) = fixture()?;
            crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
                let parent = index_parent(&index);
                let retained = parent.join("index.previous");
                build_generation(&cx, &retained, 7).await?;
                std::fs::create_dir(&index).map_err(|error| error.to_string())?;
                if let Some(manifest) = manifest {
                    std::fs::write(index.join("meta.json"), manifest)
                        .map_err(|error| error.to_string())?;
                }
                let before = file_snapshot(parent)?;
                let lease = IndexGenerationLease::read(&cx, &index)
                    .await
                    .map_err(|error| error.to_string())?;
                assert_eq!(
                    lease
                        .index_for_snapshot(&cx, &index, 7)
                        .map_err(|error| error.to_string())?,
                    retained
                );
                assert_eq!(
                    file_snapshot(parent)?,
                    before,
                    "recovery must not repair or promote files"
                );
                // A retained generation is not authority to read beyond the DB snapshot.
                assert!(lease.index_for_snapshot(&cx, &index, 6).is_err());
                assert_eq!(file_snapshot(parent)?, before);
                Ok::<(), String>(())
            })
            .map_err(|error| error.to_string())??;
        }
        Ok(())
    }

    #[test]
    fn snapshot_read_selects_complete_committed_generations_in_snapshot_order() -> TestResult {
        let (_root, index) = fixture()?;
        crate::core::run_cli_with_cx(Duration::from_secs(60), |cx| async move {
            let parent = index_parent(&index);
            build_generation(&cx, &index, 30).await?;
            let old = parent.join("index.previous.009");
            build_generation(&cx, &old, 4).await?;
            let newest = parent.join("index.previous");
            build_generation(&cx, &newest, 7).await?;
            let broken = parent.join("index.previous.003");
            build_generation(&cx, &broken, 8).await?;
            std::fs::write(
                broken.join(super::super::VECTOR_INDEX_FAST_FILE),
                b"corrupt",
            )
            .map_err(|error| error.to_string())?;
            let future = parent.join("index.previous.001");
            build_generation(&cx, &future, 20).await?;
            // Fully valid bytes still cannot grant an uncommitted path authority.
            for name in [
                ".index.publish-uncommitted",
                ".index.rejected-rejected",
                "index.previous.backup",
            ] {
                build_generation(&cx, &parent.join(name), 9).await?;
            }
            let before = file_snapshot(parent)?;
            let lease = IndexGenerationLease::read(&cx, &index)
                .await
                .map_err(|error| error.to_string())?;
            for (ceiling, expected) in [(9, &newest), (4, &old), (20, &future), (30, &index)] {
                assert_eq!(
                    lease
                        .index_for_snapshot(&cx, &index, ceiling)
                        .map_err(|error| error.to_string())?,
                    *expected
                );
            }
            assert!(lease.index_for_snapshot(&cx, &index, 3).is_err());
            assert_eq!(file_snapshot(parent)?, before);
            cx.set_cancel_reason(asupersync::CancelReason::user("cancel retained read"));
            assert!(matches!(
                lease.index_for_snapshot(&cx, &index, 9),
                Err(IndexRebuildError::Cancelled(_))
            ));
            Ok::<(), String>(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn snapshot_read_does_not_recover_through_redirected_live_metadata() -> TestResult {
        use std::os::unix::fs::symlink;
        let (_root, index) = fixture()?;
        crate::core::run_cli_with_cx(Duration::from_secs(30), |cx| async move {
            let retained = index_parent(&index).join("index.previous");
            build_generation(&cx, &retained, 7).await?;
            std::fs::create_dir(&index).map_err(|error| error.to_string())?;
            symlink(retained.join("meta.json"), index.join("meta.json"))
                .map_err(|error| error.to_string())?;
            let lease = IndexGenerationLease::read(&cx, &index)
                .await
                .map_err(|error| error.to_string())?;
            assert!(lease.index_for_snapshot(&cx, &index, 7).is_err());
            assert!(
                std::fs::symlink_metadata(index.join("meta.json"))
                    .map_err(|error| error.to_string())?
                    .is_symlink()
            );
            Ok::<(), String>(())
        })
        .map_err(|error| error.to_string())?
    }

    #[test]
    fn production_reader_wait_is_short_without_releasing_the_publisher() -> TestResult {
        let (_root, index) = fixture()?;
        let publisher = open_directory(index_parent(&index)).map_err(|e| e.to_string())?;
        assert!(try_lease(&publisher, true).map_err(|e| e.to_string())?);
        crate::core::run_cli_with_cx(Duration::from_secs(10), |cx| async move {
            let start = Instant::now();
            let result = IndexGenerationLease::read(&cx, &index).await;
            assert!(matches!(
                result,
                Err(IndexRebuildError::Index(message)) if message.contains("busy")
            ));
            assert!(start.elapsed() >= MAX_READ_WAIT);
            assert!(start.elapsed() < Duration::from_secs(2));
            let probe = open_directory(index_parent(&index)).map_err(|e| e.to_string())?;
            assert!(!try_lease(&probe, false).map_err(|e| e.to_string())?);
            drop(publisher);
            let reader = IndexGenerationLease::read(&cx, &index)
                .await
                .map_err(|e| e.to_string())?;
            assert!(!try_lease(&probe, true).map_err(|e| e.to_string())?);
            drop(reader);
            assert!(try_lease(&probe, true).map_err(|e| e.to_string())?);
            Ok(())
        })
        .map_err(|e| e.to_string())?
    }

    #[test]
    fn inspection_budget_is_shared_across_generation_trees_and_preserves_files() -> TestResult {
        let (root, index) = fixture()?;
        let retained = root.path().join("index.previous");
        std::fs::create_dir(&index).map_err(|e| e.to_string())?;
        std::fs::create_dir(&retained).map_err(|e| e.to_string())?;
        std::fs::write(index.join("segment"), b"live bytes").map_err(|e| e.to_string())?;
        std::fs::write(retained.join("segment"), b"retained bytes").map_err(|e| e.to_string())?;
        let before = file_snapshot(root.path())?;
        let cx = asupersync::Cx::for_testing();
        let mut inspection = InspectionBudget::new();
        inspection.entries_remaining = 3;
        inspect_generation_entries(&cx, &index, &mut inspection).map_err(|e| e.to_string())?;
        assert_eq!(inspection.entries_remaining, 1);
        let error = inspect_generation_entries(&cx, &retained, &mut inspection)
            .expect_err("a second candidate must not reset the budget");
        assert!(error.to_string().contains("read budget"));
        assert_eq!(file_snapshot(root.path())?, before);
        cx.set_cancel_reason(asupersync::CancelReason::user(
            "cancel exhausted inspection",
        ));
        assert!(matches!(
            inspection.entry(&cx),
            Err(IndexRebuildError::Cancelled(reason))
                if reason.message.as_deref() == Some("cancel exhausted inspection")
        ));
        Ok(())
    }

    #[test]
    fn unrelated_directory_entries_cannot_make_missing_index_discovery_unbounded() -> TestResult {
        let (root, index) = fixture()?;
        for ordinal in 0..=MAX_INSPECTION_ENTRIES {
            std::fs::write(
                root.path().join(format!("unrelated-{ordinal}")),
                b"preserved",
            )
            .map_err(|e| e.to_string())?;
        }
        let before = file_snapshot(root.path())?;
        crate::core::run_cli_with_cx(Duration::from_secs(10), |cx| async move {
            let lease = IndexGenerationLease::read(&cx, &index)
                .await
                .map_err(|e| e.to_string())?;
            let error = lease
                .has_retained_generation_directory(&cx, &index)
                .expect_err("presence scans must charge non-candidates too");
            assert!(error.to_string().contains("read budget"));
            let error = lease
                .index_for_snapshot_with_identity(&cx, &index, 10, None)
                .expect_err("an incomplete discovery must not admit a generation");
            assert!(error.to_string().contains("read budget"));
            assert_eq!(file_snapshot(root.path())?, before);
            Ok(())
        })
        .map_err(|e| e.to_string())?
    }

    #[test]
    fn inspection_deadline_is_checked_even_without_directory_entries() -> TestResult {
        let cx = asupersync::Cx::for_testing();
        let inspection = InspectionBudget {
            request: crate::core::budget::RequestBudget::unbounded_at(
                Instant::now() - Duration::from_secs(2),
            )
            .with_wall_clock(INSPECTION_WALL_BUDGET),
            entries_remaining: MAX_INSPECTION_ENTRIES,
        };
        let error = inspection.checkpoint(&cx).expect_err("expired wall budget");
        assert!(error.to_string().contains("read budget"));
        Ok(())
    }

    #[test]
    fn overdue_reader_retry_cannot_acquire_after_the_publisher_releases() -> TestResult {
        use std::future::{Future, poll_fn};
        use std::task::Poll;

        let (_root, index) = fixture()?;
        let publisher = open_directory(index_parent(&index)).map_err(|e| e.to_string())?;
        assert!(try_lease(&publisher, true).map_err(|e| e.to_string())?);
        crate::core::run_cli_with_cx(Duration::from_secs(10), |cx| async move {
            let mut reader = std::pin::pin!(IndexGenerationLease::read(&cx, &index));
            poll_fn(|task_cx| match reader.as_mut().poll(task_cx) {
                Poll::Pending => Poll::Ready(()),
                Poll::Ready(result) => panic!("reader bypassed publisher: {result:?}"),
            })
            .await;
            // Delay polling the reader, as a busy runtime worker can do. The
            // source lease is free when it resumes, but its wait has expired.
            asupersync::time::sleep(cx.now(), MAX_READ_WAIT + Duration::from_millis(50)).await;
            drop(publisher);
            assert!(matches!(
                reader.await,
                Err(IndexRebuildError::Index(message)) if message.contains("busy")
            ));
            let fresh = IndexGenerationLease::acquire(&cx, &index, false, Duration::ZERO)
                .await
                .map_err(|e| e.to_string())?;
            drop(fresh);
            Ok(())
        })
        .map_err(|e| e.to_string())?
    }
}
