//! Cross-platform filesystem path-safety primitives.
//!
//! Multiple subsystems (`init`, `qos`, `claims`, `repro`, `serve`, mesh
//! discovery) refuse to operate *through* a symlinked path component for
//! safety. They all share a single primitive: walk the existing leading
//! portion of an absolute path and report the first component that is a
//! symbolic link. Centralizing it here keeps the (subtle) traversal rules in
//! one place instead of six hand-rolled copies.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

/// Walk the existing leading portion of `path` and return the first component
/// that is a symbolic link, or `Ok(None)` if traversal reaches a component that
/// does not yet exist without encountering one. A non-directory parent is an
/// inspection error, not a missing tail.
///
/// Structural anchors — the path prefix (`C:`, `\\?\C:`, a UNC share, …) and
/// the root directory — are never symlinks and cannot be inspected in
/// isolation. On Windows, `symlink_metadata` on a bare verbatim prefix such as
/// `\\?\C:` fails with `ERROR_INVALID_FUNCTION` ("Incorrect function"), so the
/// anchors are pushed onto the running path but skipped for probing. This makes
/// the walk correct both for the canonicalized, `\\?\`-prefixed paths Rust
/// produces on Windows and for ordinary POSIX paths.
pub(crate) fn first_existing_symlink_component(path: &Path) -> io::Result<Option<PathBuf>> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(Some(current)),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

/// Convenience predicate: does any existing component of `path` traverse a
/// symbolic link?
pub(crate) fn path_has_symlink_component(path: &Path) -> io::Result<bool> {
    first_existing_symlink_component(path).map(|component| component.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression for the Windows verbatim-prefix bug: walking an ordinary,
    /// already-existing absolute directory must report "no symlink" rather than
    /// erroring on the prefix/root anchor. On Windows the temp dir canonicalizes
    /// to a `\\?\C:\…` path, which previously failed with "Incorrect function".
    #[test]
    fn existing_absolute_dir_has_no_symlink_component() {
        let dir = std::env::temp_dir();
        let canonical = fs::canonicalize(&dir).unwrap_or(dir);
        assert_eq!(path_has_symlink_component(&canonical).unwrap(), false);
    }

    /// A path whose deeper components do not exist yet stops cleanly at the
    /// first missing component and reports no symlink.
    #[test]
    fn nonexistent_tail_reports_no_symlink() {
        let mut path = std::env::temp_dir();
        path.push("ee-path-safety-does-not-exist-7f4e");
        path.push("nested");
        assert_eq!(first_existing_symlink_component(&path).unwrap(), None);
    }

    #[test]
    fn regular_file_parent_is_inspection_error() {
        let base = std::env::temp_dir().join(format!(
            "ee-path-safety-{}-{}",
            std::process::id(),
            "notdir"
        ));
        let parent_file = base.join("parent");
        fs::create_dir_all(&base).expect("create base dir");
        fs::write(&parent_file, b"not a directory").expect("create parent file");

        let error = first_existing_symlink_component(&parent_file.join("child"))
            .expect_err("regular file parent is not a missing tail");
        assert_eq!(error.kind(), io::ErrorKind::NotADirectory);
    }

    #[cfg(unix)]
    #[test]
    fn detects_symlinked_component() {
        use std::os::unix::fs::symlink;

        let base = std::env::temp_dir().join(format!(
            "ee-path-safety-{}-{}",
            std::process::id(),
            "linktest"
        ));
        let _ = fs::remove_dir_all(&base);
        let real_dir = base.join("real");
        fs::create_dir_all(&real_dir).expect("create real dir");
        let link = base.join("link");
        symlink(&real_dir, &link).expect("create symlink");

        let through_link = link.join("child.txt");
        let found = first_existing_symlink_component(&through_link)
            .expect("walk succeeds")
            .expect("symlink component detected");
        assert_eq!(found, link);

        let _ = fs::remove_dir_all(&base);
    }
}
