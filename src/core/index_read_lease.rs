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
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::{IndexRebuildError, ensure_index_path_has_no_symlinks, index_checkpoint, index_parent};
use rustix::fs::{FlockOperation, OFlags, flock};
use rustix::io::Errno;

const MAX_WAIT: Duration = Duration::from_secs(5);
const RETRY_DELAY: Duration = Duration::from_millis(5);

/// Closing the sole owned descriptor releases its OS lease on success, error,
/// cancellation, unwinding, or process exit. Never clone/upgrade this handle.
#[must_use = "the guard must stay alive until all generation reads have finished"]
#[derive(Debug)]
pub(crate) struct IndexGenerationLease {
    _directory: File,
}

impl IndexGenerationLease {
    pub(crate) async fn read(
        cx: &asupersync::Cx,
        index_dir: &Path,
    ) -> Result<Self, IndexRebuildError> {
        Self::acquire(cx, index_dir, false, MAX_WAIT).await
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
        loop {
            index_checkpoint(cx)?;
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
                return Err(IndexRebuildError::Index(
                    "Index generation is busy; no mixed-generation read or publication was attempted; retry the operation".to_owned(),
                ));
            }
            // Yield to the owning runtime, never block its worker with flock
            // or thread::sleep. Both the wall bound and caller budget apply.
            asupersync::time::sleep(cx.now(), RETRY_DELAY.min(remaining)).await;
        }
    }
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

fn lease_error(action: &str, error: impl std::fmt::Display) -> IndexRebuildError {
    IndexRebuildError::Index(format!(
        "Could not {action} index generation lease: {error}"
    ))
}

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
}
