//! Filesystem boundary for store authentication keys.
//!
//! Read a bounded regular file, never a path-selected stream. On Unix, walk
//! directories without following links and open children relative to the held
//! directory descriptor. Rotation uses that same descriptor for exclusive
//! staging and rename. No failed attempt removes or truncates existing bytes.

use std::fs::{self, File, Metadata};
use std::io::{Read, Write};
use std::path::Path;

use zeroize::Zeroizing;

use super::{KEY_LOCK_FILE_NAME, MAX_KEY_FILE_BYTES, StoreAuthError, recovery_error};

fn io_error(path: &Path, error: impl std::fmt::Display) -> StoreAuthError {
    StoreAuthError::Io {
        path: path.display().to_string(),
        message: error.to_string(),
    }
}

pub(super) fn reject_symlink_components(
    keys_dir: &Path,
    path: &Path,
) -> Result<(), StoreAuthError> {
    for candidate in keys_dir.ancestors().chain(path.ancestors()) {
        if candidate.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(candidate) {
            Ok(metadata) if metadata.is_symlink() => {
                return Err(StoreAuthError::SymlinkComponent {
                    path: candidate.display().to_string(),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(candidate, error)),
        }
    }
    Ok(())
}

fn check_owner(path: &Path, metadata: &Metadata) -> Result<(), StoreAuthError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != rustix::process::geteuid().as_raw() {
            return Err(StoreAuthError::InsecurePermissions {
                path: path.display().to_string(),
                detail: "key-store entry belongs to another user".to_owned(),
            });
        }
    }
    #[cfg(not(unix))]
    let _ = (path, metadata);
    Ok(())
}

fn check_mode(path: &Path, metadata: &Metadata) -> Result<(), StoreAuthError> {
    check_owner(path, metadata)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(StoreAuthError::InsecurePermissions {
                path: path.display().to_string(),
                detail: format!("key-store mode {mode:04o} grants group/other access"),
            });
        }
    }
    Ok(())
}

fn regular_file(path: &Path, metadata: &Metadata) -> Result<(), StoreAuthError> {
    if !metadata.is_file() {
        return Err(recovery_error("key-store entry is not a regular file"));
    }
    check_mode(path, metadata)
}

// Resolve each component through the descriptor obtained for its parent. A
// path check alone cannot protect against a swapped ancestor at open time.
#[cfg(unix)]
fn open_directory(path: &Path) -> Result<File, StoreAuthError> {
    use rustix::fs::{Mode, OFlags};
    use std::path::Component;

    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let start = if path.is_absolute() { "/" } else { "." };
    let mut directory = File::from(
        rustix::fs::open(start, flags, Mode::empty()).map_err(|error| io_error(path, error))?,
    );
    for component in path.components() {
        let name = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => name,
            Component::ParentDir => std::ffi::OsStr::new(".."),
            Component::Prefix(_) => {
                return Err(recovery_error("unsupported key-directory prefix"));
            }
        };
        directory = File::from(
            rustix::fs::openat(&directory, name, flags, Mode::empty())
                .map_err(|error| io_error(path, error))?,
        );
    }
    Ok(directory)
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(unix)]
fn private_parent(path: &Path) -> Result<File, StoreAuthError> {
    let parent = parent(path);
    let directory = open_directory(parent)?;
    check_mode(
        parent,
        &directory
            .metadata()
            .map_err(|error| io_error(parent, error))?,
    )?;
    Ok(directory)
}

#[cfg(unix)]
fn name(path: &Path) -> Result<&std::ffi::OsStr, StoreAuthError> {
    path.file_name()
        .ok_or_else(|| recovery_error("key-store file name is missing"))
}

pub(super) fn ensure_hardened_dir(keys_dir: &Path) -> Result<(), StoreAuthError> {
    reject_symlink_components(keys_dir, keys_dir)?;
    #[cfg(unix)]
    {
        create_key_directory_with(keys_dir, |path, directory| {
            directory.sync_all().map_err(|error| io_error(path, error))
        })
    }
    #[cfg(not(unix))]
    {
        fs::DirBuilder::new()
            .recursive(true)
            .create(keys_dir)
            .map_err(|error| io_error(keys_dir, error))
    }
}

/// Create through held parent descriptors, never through a re-resolved full
/// path after the symlink check. Persist each directory and its parent entry
/// before descending. Existing entries receive the same barriers: a retry or
/// concurrent creator may encounter a directory whose first barrier failed.
/// Only the final key directory is chmod'ed; existing ancestors keep their mode.
#[cfg(unix)]
fn create_key_directory_with(
    keys_dir: &Path,
    mut sync: impl FnMut(&Path, &File) -> Result<(), StoreAuthError>,
) -> Result<(), StoreAuthError> {
    use rustix::fs::{Mode, OFlags};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Component;

    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let start = if keys_dir.is_absolute() { "/" } else { "." };
    let mut directory = File::from(
        rustix::fs::open(start, flags, Mode::empty()).map_err(|error| io_error(keys_dir, error))?,
    );
    let mut current = std::path::PathBuf::from(start);
    for component in keys_dir.components() {
        let (name, may_create) = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => (name, true),
            Component::ParentDir => (std::ffi::OsStr::new(".."), false),
            Component::Prefix(_) => {
                return Err(recovery_error("unsupported key-directory prefix"));
            }
        };
        let next_path = current.join(name);
        let descriptor = match rustix::fs::openat(&directory, name, flags, Mode::empty()) {
            Ok(descriptor) => descriptor,
            Err(error) if may_create && error == rustix::io::Errno::NOENT => {
                match rustix::fs::mkdirat(&directory, name, Mode::RWXU) {
                    Ok(()) => {}
                    Err(error) if error == rustix::io::Errno::EXIST => {}
                    Err(error) => return Err(io_error(&next_path, error)),
                }
                // A racing mkdir is harmless; a racing symlink is not. Open
                // with NOFOLLOW even when mkdir reported that the name exists.
                rustix::fs::openat(&directory, name, flags, Mode::empty())
                    .map_err(|error| io_error(&next_path, error))?
            }
            Err(error) => return Err(io_error(&next_path, error)),
        };
        let child = File::from(descriptor);
        sync(&next_path, &child)?;
        sync(&current, &directory)?;
        directory = child;
        current = next_path;
    }
    check_owner(
        keys_dir,
        &directory
            .metadata()
            .map_err(|error| io_error(keys_dir, error))?,
    )?;
    directory
        .set_permissions(fs::Permissions::from_mode(0o700))
        .map_err(|error| io_error(keys_dir, error))?;
    sync(keys_dir, &directory)
}

pub(super) fn enforce_owner_only_dir(path: &Path) -> Result<(), StoreAuthError> {
    reject_symlink_components(path, path)?;
    #[cfg(unix)]
    let metadata = open_directory(path)?
        .metadata()
        .map_err(|error| io_error(path, error))?;
    #[cfg(not(unix))]
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error(path, error))?;
    if !metadata.is_dir() {
        return Err(recovery_error("key-store directory is not a directory"));
    }
    check_mode(path, &metadata)
}

pub(super) fn enforce_owner_only_file(path: &Path) -> Result<(), StoreAuthError> {
    reject_symlink_components(parent(path), path)?;
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error(path, error))?;
    regular_file(path, &metadata)
}

fn read_bounded(path: &Path, reader: impl Read) -> Result<Vec<u8>, StoreAuthError> {
    let mut bytes = Zeroizing::new(Vec::new());
    reader
        .take(MAX_KEY_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(path, error))?;
    if bytes.len() as u64 > MAX_KEY_FILE_BYTES {
        return Err(recovery_error("key file exceeds the size limit"));
    }
    Ok(std::mem::take(&mut *bytes))
}

pub(super) fn read_key_file(path: &Path) -> Result<Vec<u8>, StoreAuthError> {
    enforce_owner_only_file(path)?;
    #[cfg(unix)]
    let file = {
        use rustix::fs::{Mode, OFlags};
        let directory = private_parent(path)?;
        File::from(
            rustix::fs::openat(
                &directory,
                name(path)?,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| io_error(path, error))?,
        )
    };
    #[cfg(not(unix))]
    let file = File::open(path).map_err(|error| io_error(path, error))?;
    // Validate the opened inode, not merely the pathname checked above.
    // NONBLOCK prevents a swapped FIFO from hanging before this type check.
    let metadata = file.metadata().map_err(|error| io_error(path, error))?;
    regular_file(path, &metadata)?;
    if metadata.len() > MAX_KEY_FILE_BYTES {
        return Err(recovery_error("key file exceeds the size limit"));
    }
    read_bounded(path, file)
}

pub(super) fn open_key_lock_file(keys_dir: &Path) -> Result<File, StoreAuthError> {
    // Existing-root reads/rotations do not create or repair the key directory.
    enforce_owner_only_dir(keys_dir)?;
    let path = keys_dir.join(KEY_LOCK_FILE_NAME);
    reject_symlink_components(keys_dir, &path)?;
    #[cfg(unix)]
    let file = {
        use rustix::fs::{Mode, OFlags};
        let directory = private_parent(&path)?;
        File::from(
            rustix::fs::openat(
                &directory,
                name(&path)?,
                OFlags::RDWR
                    | OFlags::CREATE
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(|error| io_error(&path, error))?,
        )
    };
    #[cfg(not(unix))]
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| io_error(&path, error))?;
    regular_file(
        &path,
        &file.metadata().map_err(|error| io_error(&path, error))?,
    )?;
    Ok(file)
}

fn write_and_sync(path: &Path, mut file: File, bytes: &[u8]) -> Result<(), StoreAuthError> {
    regular_file(
        path,
        &file.metadata().map_err(|error| io_error(path, error))?,
    )?;
    file.write_all(bytes)
        .map_err(|error| io_error(path, error))?;
    file.sync_all().map_err(|error| io_error(path, error))
}

#[cfg(unix)]
fn create_exclusive_at(directory: &File, path: &Path) -> Result<File, StoreAuthError> {
    use rustix::fs::{Mode, OFlags};
    let descriptor = rustix::fs::openat(
        directory,
        name(path)?,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|error| {
        if error == rustix::io::Errno::EXIST {
            StoreAuthError::AlreadyInitialized {
                path: path.display().to_string(),
            }
        } else {
            io_error(path, error)
        }
    })?;
    Ok(File::from(descriptor))
}

#[cfg(unix)]
fn exclusive_at(directory: &File, path: &Path, bytes: &[u8]) -> Result<(), StoreAuthError> {
    write_and_sync(path, create_exclusive_at(directory, path)?, bytes)
}

/// Initial creation must not reserve the live name with an empty/partial key.
/// A reader either sees no root or a complete, file-synced root. NOREPLACE is
/// essential: a racing creator's successful key must never be overwritten.
/// Retain failed staging files for inspection; they never authorize a root.
#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn publish_new_key_with(
    path: &Path,
    bytes: &[u8],
    write: impl FnOnce(&Path, File, &[u8]) -> Result<(), StoreAuthError>,
    sync: impl FnOnce(&Path, &File) -> Result<(), StoreAuthError>,
) -> Result<(), StoreAuthError> {
    use rustix::fs::{AtFlags, RenameFlags};

    let directory = private_parent(path)?;
    // This avoids writing another secret on ordinary duplicate creation. It
    // is only an optimization; the no-replace rename decides the actual race.
    match rustix::fs::statat(&directory, name(path)?, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => {
            return Err(StoreAuthError::AlreadyInitialized {
                path: path.display().to_string(),
            });
        }
        Err(error) if error == rustix::io::Errno::NOENT => {}
        Err(error) => return Err(io_error(path, error)),
    }
    let temporary = parent(path).join(format!(".ee-store-auth-init-{}", uuid::Uuid::now_v7()));
    let file = create_exclusive_at(&directory, &temporary).map_err(|error| match error {
        // Only an occupied LIVE name means another root can be adopted by
        // open_or_create. A staging collision must remain an ordinary failure.
        StoreAuthError::AlreadyInitialized { .. } => {
            io_error(path, "key initialization staging name is occupied; retry")
        }
        error => error,
    })?;
    write(&temporary, file, bytes)?;
    rustix::fs::renameat_with(
        &directory,
        name(&temporary)?,
        &directory,
        name(path)?,
        RenameFlags::NOREPLACE,
    )
    .map_err(|error| {
        if error == rustix::io::Errno::EXIST {
            StoreAuthError::AlreadyInitialized {
                path: path.display().to_string(),
            }
        } else {
            io_error(path, error)
        }
    })?;
    // Persist the entry in the parent inode used for publication, not a
    // newly resolved pathname. A barrier error does not undo the installed key.
    sync(parent(path), &directory)
}

pub(super) fn write_exclusive(path: &Path, bytes: &[u8]) -> Result<(), StoreAuthError> {
    reject_symlink_components(parent(path), path)?;
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    {
        publish_new_key_with(path, bytes, write_and_sync, |parent, directory| {
            directory
                .sync_all()
                .map_err(|error| io_error(parent, error))
        })
    }
    // Other platforms retain exclusive direct creation until they have a
    // supported atomic no-replace publisher. Never emulate it with a racy
    // exists check followed by an overwriting rename.
    #[cfg(all(
        unix,
        not(any(target_os = "linux", target_os = "android", target_vendor = "apple"))
    ))]
    {
        exclusive_at(&private_parent(path)?, path, bytes)
    }
    #[cfg(not(unix))]
    {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    StoreAuthError::AlreadyInitialized {
                        path: path.display().to_string(),
                    }
                } else {
                    io_error(path, error)
                }
            })?;
        write_and_sync(path, file, bytes)
    }
}

pub(super) fn write_replace(tmp: &Path, path: &Path, bytes: &[u8]) -> Result<(), StoreAuthError> {
    if tmp == path || parent(tmp) != parent(path) {
        return Err(recovery_error(
            "key replacement requires a distinct sibling temporary",
        ));
    }
    reject_symlink_components(parent(path), path)?;
    reject_symlink_components(parent(tmp), tmp)?;
    #[cfg(unix)]
    {
        // Both names belong to this exact parent inode even if its pathname
        // changes. No secret is ever written via a second path resolution.
        let directory = private_parent(path)?;
        exclusive_at(&directory, tmp, bytes)?;
        rustix::fs::renameat(&directory, name(tmp)?, &directory, name(path)?)
            .map_err(|error| io_error(path, error))?;
    }
    #[cfg(not(unix))]
    {
        write_exclusive(tmp, bytes)?;
        fs::rename(tmp, path).map_err(|error| io_error(path, error))?;
    }
    Ok(())
}

// Unix has an explicit directory barrier. Other platforms retain the original
// file-sync guarantee; this adapter does not claim equivalent directory fsync.
pub(super) fn sync_key_directory(path: &Path) -> Result<(), StoreAuthError> {
    #[cfg(unix)]
    {
        open_directory(path)?
            .sync_all()
            .map_err(|error| io_error(path, error))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{KEY_FILE_NAME, StoreAuthRoot};
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let directory = tempfile::tempdir_in(
            std::env::temp_dir()
                .canonicalize()
                .expect("physical temporary root"),
        )
        .expect("temporary directory");
        // Low-level publishers require an already-private key directory.
        // Do not let the host's umask decide whether these fixtures reach
        // their injected write and publication failures.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
                .expect("private fixture directory");
        }
        directory
    }

    #[cfg(unix)]
    mod directory_creation {
        use super::*;
        use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

        fn sync(path: &Path, directory: &File) -> Result<(), StoreAuthError> {
            directory.sync_all().map_err(|error| io_error(path, error))
        }

        fn mode(path: &Path) -> u32 {
            fs::metadata(path)
                .expect("directory metadata")
                .permissions()
                .mode()
                & 0o777
        }

        #[test]
        fn key_directory_bootstrap_persists_each_child_and_parent_before_returning() {
            let root = fixture();
            let project = root.path().join("new-project");
            let marker = project.join(".ee");
            let keys = marker.join("keys");
            let mut barriers = Vec::new();
            create_key_directory_with(&keys, |path, directory| {
                barriers.push(path.to_path_buf());
                sync(path, directory)
            })
            .expect("durable directory bootstrap");
            for child in [&project, &marker, &keys] {
                assert_eq!(mode(child), 0o700);
                assert!(barriers.windows(2).any(|pair| {
                    pair[0].as_path() == child.as_path()
                        && Some(pair[1].as_path()) == child.parent()
                }));
            }
            assert_eq!(barriers.last(), Some(&keys));
            assert!(!keys.join(KEY_FILE_NAME).exists());
            let created = StoreAuthRoot::create(&keys).expect("initialize after bootstrap");
            assert_eq!(
                created.current_key_id(),
                StoreAuthRoot::open(&keys)
                    .expect("reopen root")
                    .current_key_id()
            );
        }

        #[test]
        fn key_directory_bootstrap_hardens_only_the_requested_leaf() {
            let root = fixture();
            let project = root.path().join("project");
            let keys = project.join("keys");
            fs::create_dir_all(&keys).expect("existing tree");
            fs::set_permissions(&project, fs::Permissions::from_mode(0o750)).expect("parent mode");
            fs::set_permissions(&keys, fs::Permissions::from_mode(0o755)).expect("leaf mode");
            let original = fs::metadata(&project).expect("parent inode").ino();
            ensure_hardened_dir(&keys).expect("harden actual leaf descriptor");
            assert_eq!(mode(&project), 0o750);
            assert_eq!(fs::metadata(&project).expect("same inode").ino(), original);
            assert_eq!(mode(&keys), 0o700);
        }

        #[test]
        fn key_directory_bootstrap_does_not_follow_a_swapped_ancestor() {
            let root = fixture();
            let project = root.path().join("project");
            let retained = root.path().join("retained-project");
            let outside = root.path().join("outside");
            fs::create_dir(&project).expect("project");
            fs::create_dir(&outside).expect("outside");
            let keys = project.join(".ee/keys");
            let mut swapped = false;
            create_key_directory_with(&keys, |path, directory| {
                if path == project.as_path() && !swapped {
                    fs::rename(&project, &retained).map_err(|error| io_error(path, error))?;
                    symlink(&outside, &project).map_err(|error| io_error(path, error))?;
                    swapped = true;
                    assert_eq!(
                        directory.metadata().expect("held descriptor").ino(),
                        fs::metadata(&retained).expect("retained inode").ino()
                    );
                }
                sync(path, directory)
            })
            .expect("descriptor remains bound to the opened tree");
            assert!(swapped);
            assert!(!outside.join(".ee").exists());
            assert!(retained.join(".ee/keys").is_dir());
            assert_eq!(mode(&retained.join(".ee/keys")), 0o700);
            // The enclosing constructor's file boundary rechecks the path;
            // it must not issue a root for the substituted namespace.
            assert!(StoreAuthRoot::create(&keys).is_err());
            assert!(!retained.join(".ee/keys").join(KEY_FILE_NAME).exists());
        }

        #[test]
        fn key_directory_bootstrap_retries_failed_child_and_parent_barriers() {
            for fail_parent in [false, true] {
                let root = fixture();
                let marker = root.path().join("new-project/.ee");
                let keys = marker.join("keys");
                let failed_path = if fail_parent { &marker } else { &keys };
                let error = create_key_directory_with(&keys, |path, directory| {
                    if path == failed_path.as_path() && keys.is_dir() {
                        return Err(io_error(path, "injected directory persistence failure"));
                    }
                    sync(path, directory)
                })
                .expect_err("bootstrap durability must not be assumed");
                assert!(matches!(error, StoreAuthError::Io { .. }));
                assert!(keys.is_dir(), "incomplete directory work is retained");
                assert!(!keys.join(KEY_FILE_NAME).exists());
                let mut barriers = Vec::new();
                create_key_directory_with(&keys, |path, directory| {
                    barriers.push(path.to_path_buf());
                    sync(path, directory)
                })
                .expect("retry persists already-existing directories too");
                assert!(barriers.windows(2).any(|pair| {
                    pair[0].as_path() == keys.as_path() && pair[1].as_path() == marker.as_path()
                }));
                let created = StoreAuthRoot::open_or_create(&keys).expect("usable after retry");
                assert_eq!(
                    created.current_key_id(),
                    StoreAuthRoot::open(&keys)
                        .expect("reopen root")
                        .current_key_id()
                );
            }
        }

        #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
        #[test]
        fn key_directory_bootstrap_allows_concurrent_first_time_creators() {
            let root = fixture();
            let keys = root.path().join("new-project/.ee/keys");
            let gate = std::sync::Barrier::new(8);
            let ids = std::thread::scope(|scope| {
                let mut workers = Vec::new();
                for _ in 0..8 {
                    let gate = &gate;
                    let keys = &keys;
                    workers.push(scope.spawn(move || {
                        gate.wait();
                        StoreAuthRoot::open_or_create(keys)
                            .expect("concurrent directory and root creation")
                            .current_key_id()
                    }));
                }
                workers
                    .into_iter()
                    .map(|worker| worker.join().expect("creator finished"))
                    .collect::<Vec<_>>()
            });
            let root = StoreAuthRoot::open(&keys).expect("complete root");
            assert_eq!(ids.len(), 8);
            assert!(ids.iter().all(|id| *id == root.current_key_id()));
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    mod initialization {
        use super::*;
        use crate::policy::store_auth::MacDomain;

        fn sync(parent: &Path, directory: &File) -> Result<(), StoreAuthError> {
            directory
                .sync_all()
                .map_err(|error| io_error(parent, error))
        }

        #[test]
        fn atomic_initialization_exposes_only_a_complete_authenticated_root() {
            let source = fixture();
            let original = StoreAuthRoot::create(source.path()).expect("source root");
            let bytes = fs::read(source.path().join(KEY_FILE_NAME)).expect("source bytes");
            let destination = fixture();
            let path = destination.path().join(KEY_FILE_NAME);
            publish_new_key_with(
                &path,
                &bytes,
                |temporary, file, bytes| {
                    assert_ne!(temporary, path.as_path());
                    assert!(!path.exists(), "the live name is not a reservation");
                    write_and_sync(temporary, file, bytes)?;
                    assert!(matches!(
                        StoreAuthRoot::open(destination.path()),
                        Err(StoreAuthError::NotInitialized { .. })
                    ));
                    Ok(())
                },
                sync,
            )
            .expect("publish complete root");
            let reopened = StoreAuthRoot::open(destination.path()).expect("authenticated root");
            assert_eq!(reopened.current_key_id(), original.current_key_id());
            assert_eq!(fs::read(&path).expect("published bytes"), bytes);
            assert_eq!(
                fs::read_dir(destination.path()).expect("entries").count(),
                1
            );
        }

        #[test]
        fn atomic_initialization_failures_leave_the_live_name_free_for_retry() {
            let source = fixture();
            StoreAuthRoot::create(source.path()).expect("source root");
            let bytes = fs::read(source.path().join(KEY_FILE_NAME)).expect("source bytes");
            for length in [0, bytes.len() / 2, bytes.len()] {
                let destination = fixture();
                let path = destination.path().join(KEY_FILE_NAME);
                let mut retained = None;
                let error = publish_new_key_with(
                    &path,
                    &bytes,
                    |temporary, mut file, bytes| {
                        retained = Some(temporary.to_path_buf());
                        file.write_all(&bytes[..length])
                            .map_err(|error| io_error(temporary, error))?;
                        file.sync_all()
                            .map_err(|error| io_error(temporary, error))?;
                        Err(io_error(temporary, "injected interrupted initialization"))
                    },
                    |_, _| panic!("a failed write must not reach publication sync"),
                )
                .expect_err("interrupted write");
                assert!(matches!(error, StoreAuthError::Io { .. }));
                assert!(
                    !path.exists(),
                    "partial bytes must not occupy the root name"
                );
                let retained = retained.expect("staging retained");
                assert_eq!(
                    fs::read(&retained).expect("retained prefix"),
                    &bytes[..length]
                );
                let root = StoreAuthRoot::open_or_create(destination.path()).expect("fresh retry");
                let reopened = StoreAuthRoot::open(destination.path()).expect("valid retry");
                assert_eq!(root.current_key_id(), reopened.current_key_id());
                assert_eq!(
                    fs::read(&retained).expect("preserved prefix"),
                    &bytes[..length]
                );
            }
        }

        #[test]
        fn atomic_initialization_adopts_one_complete_root_under_concurrent_creation() {
            let destination = fixture();
            let gate = std::sync::Barrier::new(8);
            let results = std::thread::scope(|scope| {
                let mut workers = Vec::new();
                for _ in 0..8 {
                    let gate = &gate;
                    let path = destination.path();
                    workers.push(scope.spawn(move || {
                        gate.wait();
                        let root = StoreAuthRoot::open_or_create(path).expect("concurrent root");
                        (
                            root.current_key_id(),
                            root.mac(MacDomain::NativeImportRecordsRoot, b"one store")
                                .expect("MAC"),
                        )
                    }));
                }
                workers
                    .into_iter()
                    .map(|worker| worker.join().expect("creator finished"))
                    .collect::<Vec<_>>()
            });
            let reopened = StoreAuthRoot::open(destination.path()).expect("durable winner");
            let expected = (
                reopened.current_key_id(),
                reopened
                    .mac(MacDomain::NativeImportRecordsRoot, b"one store")
                    .expect("durable MAC"),
            );
            assert_eq!(results.len(), 8);
            assert!(results.iter().all(|result| *result == expected));
        }

        #[test]
        fn atomic_initialization_never_overwrites_a_winner_published_during_staging() {
            let source = fixture();
            let losing = StoreAuthRoot::create(source.path()).expect("losing root");
            let bytes = fs::read(source.path().join(KEY_FILE_NAME)).expect("losing bytes");
            let destination = fixture();
            let path = destination.path().join(KEY_FILE_NAME);
            let mut winner = None;
            let mut retained = None;
            let error = publish_new_key_with(
                &path,
                &bytes,
                |temporary, file, bytes| {
                    write_and_sync(temporary, file, bytes)?;
                    retained = Some(temporary.to_path_buf());
                    winner = Some(StoreAuthRoot::create(destination.path())?.current_key_id());
                    Ok(())
                },
                |_, _| panic!("the losing publisher must not report a successful rename"),
            )
            .expect_err("winner must not be replaced");
            assert!(matches!(error, StoreAuthError::AlreadyInitialized { .. }));
            let reopened = StoreAuthRoot::open(destination.path()).expect("winner remains valid");
            assert_eq!(Some(reopened.current_key_id()), winner);
            assert_ne!(reopened.current_key_id(), losing.current_key_id());
            assert_eq!(
                fs::read(retained.expect("losing stage")).expect("stage bytes"),
                bytes
            );
        }

        #[test]
        fn atomic_initialization_refuses_occupied_names_without_writing_another_key() {
            use std::os::unix::fs::{PermissionsExt, symlink};

            for entry in ["corrupt", "directory", "symlink", "hardlink"] {
                let destination = fixture();
                let path = destination.path().join(KEY_FILE_NAME);
                let target = destination.path().join("preserved");
                fs::write(&target, b"preserve existing material").expect("existing material");
                fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).expect("mode");
                match entry {
                    "corrupt" => fs::write(&path, b"partial old key").expect("partial key"),
                    "directory" => fs::create_dir(&path).expect("occupied directory"),
                    "symlink" => symlink(&target, &path).expect("occupied symlink"),
                    _ => fs::hard_link(&target, &path).expect("occupied hardlink"),
                }
                assert!(write_exclusive(&path, b"replacement is forbidden").is_err());
                assert_eq!(
                    fs::read(&target).expect("unchanged"),
                    b"preserve existing material"
                );
                assert_eq!(
                    fs::read_dir(destination.path()).expect("no stage").count(),
                    2
                );
                if entry == "corrupt" {
                    assert_eq!(
                        fs::read(&path).expect("old key retained"),
                        b"partial old key"
                    );
                }
            }
        }

        #[test]
        fn atomic_initialization_reports_barrier_failure_without_replacing_the_installed_root() {
            let source = fixture();
            let original = StoreAuthRoot::create(source.path()).expect("source root");
            let bytes = fs::read(source.path().join(KEY_FILE_NAME)).expect("source bytes");
            let destination = fixture();
            let path = destination.path().join(KEY_FILE_NAME);
            let error = publish_new_key_with(&path, &bytes, write_and_sync, |parent, _| {
                let installed = StoreAuthRoot::open(parent).expect("rename already happened");
                assert_eq!(installed.current_key_id(), original.current_key_id());
                Err(io_error(parent, "injected directory barrier failure"))
            })
            .expect_err("durability failure must be reported");
            assert!(matches!(error, StoreAuthError::Io { .. }));
            let retry = StoreAuthRoot::open_or_create(destination.path()).expect("adopt installed");
            assert_eq!(retry.current_key_id(), original.current_key_id());
            assert_eq!(fs::read(path).expect("installed key retained"), bytes);
        }
    }

    #[test]
    fn bounded_read_rejects_growth_after_metadata_without_reading_the_whole_stream() {
        let mut source = std::io::Cursor::new(vec![b'x'; MAX_KEY_FILE_BYTES as usize + 8192]);
        assert!(read_bounded(Path::new("synthetic-key"), &mut source).is_err());
        assert_eq!(source.position(), MAX_KEY_FILE_BYTES + 1);
        let exact = vec![b'x'; MAX_KEY_FILE_BYTES as usize];
        assert_eq!(
            read_bounded(Path::new("synthetic-key"), exact.as_slice()).expect("at limit"),
            exact
        );
    }

    #[test]
    fn key_and_lock_directories_are_not_regular_files() {
        let dir = fixture();
        let key = dir.path().join(KEY_FILE_NAME);
        fs::create_dir(&key).expect("directory instead of key");
        assert!(read_key_file(&key).is_err());
        fs::create_dir(dir.path().join(KEY_LOCK_FILE_NAME)).expect("directory instead of lock");
        assert!(open_key_lock_file(dir.path()).is_err());
    }

    #[test]
    fn read_lock_does_not_initialize_a_missing_key_directory() {
        let dir = fixture();
        let missing = dir.path().join("missing/keys");
        assert!(StoreAuthRoot::open_read_locked(&missing).is_err());
        assert!(!dir.path().join("missing").exists());
    }

    #[cfg(unix)]
    #[test]
    fn ancestor_links_are_refused_before_creation_or_permission_changes() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = fixture();
        let outside = dir.path().join("outside");
        let keys = outside.join("keys");
        fs::create_dir_all(&keys).expect("outside directory");
        fs::set_permissions(&keys, fs::Permissions::from_mode(0o755)).expect("original mode");
        let alias = dir.path().join("alias");
        symlink(&outside, &alias).expect("ancestor link");
        assert!(StoreAuthRoot::create(alias.join("keys")).is_err());
        assert!(StoreAuthRoot::open_read_locked(alias.join("keys")).is_err());
        assert!(StoreAuthRoot::create(alias.join("new/keys")).is_err());
        assert_eq!(
            fs::metadata(&keys)
                .expect("unchanged mode")
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            fs::read_dir(&keys).expect("no key or lock written").count(),
            0
        );
        assert!(!outside.join("new").exists());
    }

    #[cfg(unix)]
    #[test]
    fn leaf_links_cannot_supply_keys_or_redirect_lock_creation() {
        use std::os::unix::fs::symlink;
        let dir = fixture();
        let target = dir.path().join("outside");
        fs::write(&target, b"do not change these bytes").expect("target");
        let key = dir.path().join(KEY_FILE_NAME);
        symlink(&target, &key).expect("key link");
        assert!(read_key_file(&key).is_err());
        symlink(&target, dir.path().join(KEY_LOCK_FILE_NAME)).expect("lock link");
        assert!(open_key_lock_file(dir.path()).is_err());
        assert_eq!(
            fs::read(&target).expect("preserved"),
            b"do not change these bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn existing_insecure_directory_is_not_silently_repaired_by_a_read_lock() {
        use std::os::unix::fs::PermissionsExt;
        let dir = fixture();
        StoreAuthRoot::create(dir.path()).expect("root");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).expect("widen mode");
        assert!(StoreAuthRoot::open_read_locked(dir.path()).is_err());
        assert_eq!(
            fs::metadata(dir.path()).expect("mode").permissions().mode() & 0o777,
            0o755
        );
        assert!(!dir.path().join(KEY_LOCK_FILE_NAME).exists());
    }

    #[cfg(unix)]
    #[test]
    fn special_key_and_lock_entries_are_refused_without_reading_a_body() {
        let dir = fixture();
        let key = dir.path().join(KEY_FILE_NAME);
        let _key_socket = std::os::unix::net::UnixListener::bind(&key).expect("key socket");
        assert!(read_key_file(&key).is_err());
        let _lock_socket =
            std::os::unix::net::UnixListener::bind(dir.path().join(KEY_LOCK_FILE_NAME))
                .expect("lock socket");
        assert!(open_key_lock_file(dir.path()).is_err());
    }
}
