//! Remove admission authority from a generation whose publication failed.
//!
//! Directory quarantine and reverse exchange can themselves fail. Retire the
//! rejected manifest BEFORE either operation so a complete but unaccepted live
//! index cannot be selected by readers after the publication lease is released.
//! Renaming preserves every manifest byte for diagnosis, needs no replacement
//! file allocation, and moves the rejection with its generation directory.
//! This complements inode-attested rollback retention; it is not a transaction
//! spanning the database and filesystem or a claim that failed fsync is durable.

use std::io;
use std::path::Path;

use super::{
    INDEX_METADATA_FILE, IndexRebuildError, ensure_index_path_has_no_symlinks, monotonicish_stamp,
    sync_index_directory,
};

const RETIRED_PREFIX: &str = ".rejected-meta-";

pub(super) fn retire(index_dir: &Path) -> Result<(), IndexRebuildError> {
    retire_with(index_dir, rename_manifest, sync_index_directory)
}

fn retire_with(
    index_dir: &Path,
    mut rename: impl FnMut(&Path, &Path) -> io::Result<()>,
    sync: impl FnOnce(&Path) -> Result<(), IndexRebuildError>,
) -> Result<(), IndexRebuildError> {
    let source = index_dir.join(INDEX_METADATA_FILE);
    ensure_index_path_has_no_symlinks(&source, "retire rejected index manifest")?;
    match std::fs::symlink_metadata(&source) {
        // Already missing metadata is already inadmissible. A previous attempt
        // may nevertheless have failed its barrier: retry it on an existing
        // directory without ever creating a missing generation.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return match std::fs::symlink_metadata(index_dir) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(retirement_error("inspect generation", error)),
                Ok(_) => sync(index_dir),
            };
        }
        Err(error) => return Err(retirement_error("inspect", error)),
        Ok(metadata) if !metadata.is_file() => {
            return Err(IndexRebuildError::Index(
                "Refusing to retire rejected index manifest: metadata is not a regular file"
                    .to_owned(),
            ));
        }
        Ok(_) => {}
    }
    let stamp = monotonicish_stamp();
    for sequence in 0_u32..1000 {
        let destination = index_dir.join(format!("{RETIRED_PREFIX}{stamp}-{sequence:03}.json"));
        match rename(&source, &destination) {
            Ok(()) => {
                // Persist the loss of admission authority on THIS inode before
                // moving/exchanging its parent-directory entry. The caller must
                // still attempt restoration if this barrier reports failure.
                return sync(index_dir);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(retirement_error("rename", error)),
        }
    }
    Err(IndexRebuildError::Index(
        "Failed to allocate a non-overwriting retired index manifest name".to_owned(),
    ))
}

fn retirement_error(action: &str, error: io::Error) -> IndexRebuildError {
    IndexRebuildError::Index(format!(
        "Failed to {action} rejected index manifest: {error}"
    ))
}

fn rename_manifest(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    {
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            source,
            rustix::fs::CWD,
            destination,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(io::Error::from)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
    {
        // Same cooperating-writer lease assumption as directory publication.
        // Never intentionally overwrite even a dangling link or special entry.
        match std::fs::symlink_metadata(destination) {
            Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::fs::rename(source, destination)
    }
}

#[cfg(all(
    test,
    any(target_os = "linux", target_os = "android", target_vendor = "apple")
))]
#[path = "index_rollback_admission_tests.rs"]
mod admission_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::path::PathBuf;

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().expect("temporary directory");
        let index = root
            .path()
            .canonicalize()
            .expect("canonical root")
            .join("index");
        std::fs::create_dir(&index).expect("index directory");
        (root, index)
    }

    fn retired_manifests(index: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(index)
            .expect("read index")
            .map(|entry| entry.expect("index entry").path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(RETIRED_PREFIX))
            })
            .collect()
    }

    #[test]
    fn retirement_preserves_manifest_bytes_and_is_idempotent() {
        let (_root, index) = fixture();
        let bytes = b"{\"sourceGeneration\":8,\"private\":\"never logged\"}\n";
        std::fs::write(index.join(INDEX_METADATA_FILE), bytes).expect("manifest");
        std::fs::write(index.join("tier"), b"unchanged").expect("tier");
        retire(&index).expect("retire");
        retire(&index).expect("repeated retirement");
        assert!(!index.join(INDEX_METADATA_FILE).exists());
        let saved = retired_manifests(&index);
        assert_eq!(saved.len(), 1);
        assert_eq!(std::fs::read(&saved[0]).expect("saved manifest"), bytes);
        assert_eq!(
            std::fs::read(index.join("tier")).expect("tier"),
            b"unchanged"
        );
    }

    #[test]
    fn missing_generation_is_not_created_by_retirement() {
        let root = tempfile::tempdir().expect("root");
        let index = root
            .path()
            .canonicalize()
            .expect("canonical root")
            .join("absent");
        retire(&index).expect("already inadmissible");
        assert!(!index.exists());
    }

    #[test]
    fn directory_barrier_follows_manifest_retirement() {
        let (_root, index) = fixture();
        std::fs::write(index.join(INDEX_METADATA_FILE), b"manifest").expect("manifest");
        let calls = Cell::new(0);
        retire_with(&index, rename_manifest, |directory| {
            assert_eq!(directory, index);
            assert!(!directory.join(INDEX_METADATA_FILE).exists());
            assert_eq!(retired_manifests(directory).len(), 1);
            calls.set(calls.get() + 1);
            sync_index_directory(directory)
        })
        .expect("retire with barrier");
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn failed_barrier_never_reports_durable_retirement() {
        let (_root, index) = fixture();
        std::fs::write(index.join(INDEX_METADATA_FILE), b"manifest").expect("manifest");
        let error = retire_with(&index, rename_manifest, |_| {
            Err(IndexRebuildError::Index(
                "injected manifest barrier failure".into(),
            ))
        })
        .expect_err("failed barrier");
        assert!(
            error
                .to_string()
                .contains("injected manifest barrier failure")
        );
        assert!(!index.join(INDEX_METADATA_FILE).exists());
        assert_eq!(retired_manifests(&index).len(), 1);
    }

    #[test]
    fn retry_after_failed_barrier_flushes_the_existing_generation() {
        let (_root, index) = fixture();
        std::fs::write(index.join(INDEX_METADATA_FILE), b"manifest").expect("manifest");
        assert!(
            retire_with(&index, rename_manifest, |_| {
                Err(IndexRebuildError::Index("injected barrier failure".into()))
            })
            .is_err()
        );
        let calls = Cell::new(0);
        retire_with(
            &index,
            |_, _| panic!("manifest is already retired"),
            |directory| {
                calls.set(calls.get() + 1);
                sync_index_directory(directory)
            },
        )
        .expect("retry barrier");
        assert_eq!(calls.get(), 1);
        assert_eq!(retired_manifests(&index).len(), 1);
    }

    #[test]
    fn failed_rename_preserves_original_manifest_and_reports_failure() {
        let (_root, index) = fixture();
        std::fs::write(index.join(INDEX_METADATA_FILE), b"manifest").expect("manifest");
        let error = retire_with(
            &index,
            |_, _| Err(io::ErrorKind::PermissionDenied.into()),
            |_| panic!("must not report a rename that never happened"),
        )
        .expect_err("rename failure");
        assert!(
            error
                .to_string()
                .contains("Failed to rename rejected index manifest")
        );
        assert_eq!(
            std::fs::read(index.join(INDEX_METADATA_FILE)).expect("manifest"),
            b"manifest"
        );
        assert!(retired_manifests(&index).is_empty());
    }

    #[test]
    fn collision_never_overwrites_existing_diagnostic_bytes() {
        let (_root, index) = fixture();
        std::fs::write(index.join(INDEX_METADATA_FILE), b"new manifest").expect("manifest");
        let mut collision = None;
        retire_with(
            &index,
            |source, destination| {
                if collision.is_none() {
                    std::fs::write(destination, b"existing diagnostic")?;
                    collision = Some(destination.to_path_buf());
                }
                rename_manifest(source, destination)
            },
            sync_index_directory,
        )
        .expect("retry after collision");
        assert_eq!(
            std::fs::read(collision.expect("collision")).expect("existing bytes"),
            b"existing diagnostic"
        );
        assert_eq!(retired_manifests(&index).len(), 2);
    }

    #[test]
    fn non_regular_manifest_is_refused_without_moving_it() {
        let (_root, index) = fixture();
        std::fs::create_dir(index.join(INDEX_METADATA_FILE)).expect("unexpected directory");
        assert!(retire(&index).is_err());
        assert!(index.join(INDEX_METADATA_FILE).is_dir());
        assert!(retired_manifests(&index).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_manifest_cannot_touch_its_target() {
        let (root, index) = fixture();
        let outside = root.path().join("outside");
        std::fs::write(&outside, b"private bytes").expect("outside file");
        let link = index.join(INDEX_METADATA_FILE);
        std::os::unix::fs::symlink(&outside, &link).expect("metadata link");
        assert!(retire(&index).is_err());
        assert!(
            std::fs::symlink_metadata(link)
                .expect("link remains")
                .is_symlink()
        );
        assert_eq!(
            std::fs::read(outside).expect("outside remains"),
            b"private bytes"
        );
        assert!(retired_manifests(&index).is_empty());
    }
}
