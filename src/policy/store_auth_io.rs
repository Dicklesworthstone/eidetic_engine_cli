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
    // Refuse redirection before mkdir/chmod, not after changing another tree.
    reject_symlink_components(keys_dir, keys_dir)?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(keys_dir)
        .map_err(|error| io_error(keys_dir, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let directory = open_directory(keys_dir)?;
        check_owner(
            keys_dir,
            &directory
                .metadata()
                .map_err(|error| io_error(keys_dir, error))?,
        )?;
        directory
            .set_permissions(fs::Permissions::from_mode(0o700))
            .map_err(|error| io_error(keys_dir, error))?;
    }
    Ok(())
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
fn exclusive_at(directory: &File, path: &Path, bytes: &[u8]) -> Result<(), StoreAuthError> {
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
    write_and_sync(path, File::from(descriptor), bytes)
}

pub(super) fn write_exclusive(path: &Path, bytes: &[u8]) -> Result<(), StoreAuthError> {
    reject_symlink_components(parent(path), path)?;
    #[cfg(unix)]
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
        tempfile::tempdir_in(
            std::env::temp_dir()
                .canonicalize()
                .expect("physical temporary root"),
        )
        .expect("temporary directory")
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
