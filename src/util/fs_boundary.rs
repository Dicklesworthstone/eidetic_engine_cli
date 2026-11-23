//! Opt-in filesystem walks rooted at an operator-trusted sandbox boundary.
//!
//! The boundary's ancestors are trusted, not inspected or canonicalized. The
//! boundary itself must be an existing directory, never a symlink. Callers
//! performing sensitive writes must still use their descriptor-relative,
//! no-follow operations beneath this root; canonicalization is not a lock.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::config::env_registry::{self, EnvVar};

const MAX_SYMLINK_DEPTH: usize = 40;

/// A validated, explicitly trusted starting directory for filesystem checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FsBoundary {
    root: PathBuf,
}

impl FsBoundary {
    /// Validate the boundary without probing any of its ancestors.
    pub(crate) fn new(root: &Path) -> io::Result<Self> {
        if root.as_os_str().is_empty() || !root.is_absolute() {
            return Err(invalid_boundary("must name an absolute existing directory"));
        }
        if root.components().any(|part| part == Component::ParentDir) {
            return Err(invalid_boundary("must not contain '..'"));
        }
        let root = root.components().collect::<PathBuf>();
        let metadata = std::fs::symlink_metadata(&root).map_err(boundary_root_inspection_error)?;
        validate_root_metadata(&metadata)?;
        Ok(Self { root })
    }

    #[must_use]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Return an absolute, component-normalized path inside this boundary.
    ///
    /// No filesystem metadata is read. Parent components in caller paths are
    /// refused, even when they would lexically stay inside: erasing `link/..`
    /// would make a subsequent no-follow check inspect a different path from
    /// the caller's eventual open. Relative paths use the process directory.
    pub(crate) fn ensure_within(&self, path: &Path) -> io::Result<PathBuf> {
        if path.is_absolute() {
            self.ensure_within_from(path, &self.root)
        } else {
            self.ensure_within_from(path, &std::env::current_dir()?)
        }
    }

    fn ensure_within_from(&self, path: &Path, cwd: &Path) -> io::Result<PathBuf> {
        if path.as_os_str().is_empty() {
            return Err(invalid_boundary("cannot resolve an empty path"));
        }
        if path.components().any(|part| part == Component::ParentDir) {
            return Err(boundary_refusal("caller paths must not contain '..'"));
        }
        if !path.is_absolute()
            && path
                .components()
                .any(|part| matches!(part, Component::Prefix(_) | Component::RootDir))
        {
            return Err(boundary_refusal(
                "ambiguous root-relative or drive-relative path",
            ));
        }
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        let normalized = absolute.components().collect::<PathBuf>();
        if !normalized.starts_with(&self.root) {
            return Err(boundary_refusal("path is outside the configured boundary"));
        }
        Ok(normalized)
    }

    /// Keep the parent unprobed and place the boundary first in the walk.
    pub(crate) fn walk_plan(&self, path: &Path) -> io::Result<(PathBuf, PathBuf)> {
        let path = self.ensure_within(path)?;
        let rest = path
            .strip_prefix(&self.root)
            .map_err(|_| boundary_refusal("path is outside the configured boundary"))?;
        match (self.root.parent(), self.root.file_name()) {
            (Some(parent), Some(name)) => Ok((parent.to_path_buf(), Path::new(name).join(rest))),
            // Filesystem/volume roots have no parent. Construction already
            // checked the root itself; the walker starts with its children.
            _ => Ok((self.root.clone(), rest.to_path_buf())),
        }
    }

    /// Resolve symlinks using only paths at or below the trusted boundary.
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        self.canonicalize_with(
            path,
            |path| std::fs::symlink_metadata(path),
            |path| std::fs::read_link(path),
        )
    }

    fn canonicalize_with(
        &self,
        path: &Path,
        mut metadata: impl FnMut(&Path) -> io::Result<std::fs::Metadata>,
        mut read_link: impl FnMut(&Path) -> io::Result<PathBuf>,
    ) -> io::Result<PathBuf> {
        let absolute = self.ensure_within(path)?;
        let relative = absolute
            .strip_prefix(&self.root)
            .map_err(|_| boundary_refusal("path is outside the configured boundary"))?;
        let mut pending = resolution_steps(relative)?;
        if requires_directory(path) {
            pending.push_back(ResolutionStep::Directory);
        }
        validate_root_metadata(&metadata(&self.root).map_err(boundary_root_inspection_error)?)?;
        let mut resolved = self.root.clone();
        let mut links = 0_usize;

        while let Some(step) = pending.pop_front() {
            match step {
                ResolutionStep::Parent => {
                    if resolved == self.root || !resolved.pop() {
                        return Err(boundary_refusal("symlink target escapes the boundary"));
                    }
                }
                ResolutionStep::Directory => {
                    if !metadata(&resolved)?.is_dir() {
                        return Err(io::Error::new(
                            io::ErrorKind::NotADirectory,
                            "filesystem boundary path component is not a directory",
                        ));
                    }
                }
                ResolutionStep::Name(name) => {
                    let next = resolved.join(name);
                    let info = metadata(&next)?;
                    if info.file_type().is_symlink() {
                        links += 1;
                        if links > MAX_SYMLINK_DEPTH {
                            return Err(boundary_refusal(
                                "too many symbolic links below the boundary",
                            ));
                        }
                        let target = read_link(&next)?;
                        let mut target_steps = if target.is_absolute() {
                            let rest = target.strip_prefix(&self.root).map_err(|_| {
                                boundary_refusal("symlink target is outside the boundary")
                            })?;
                            resolved.clone_from(&self.root);
                            resolution_steps(rest)?
                        } else {
                            resolution_steps(&target)?
                        };
                        // Preserve `link/..` evaluation order within targets:
                        // resolve the link first, then check the parent step.
                        if requires_directory(&target) {
                            target_steps.push_back(ResolutionStep::Directory);
                        }
                        target_steps.append(&mut pending);
                        pending = target_steps;
                    } else {
                        if !pending.is_empty() && !info.is_dir() {
                            return Err(io::Error::new(
                                io::ErrorKind::NotADirectory,
                                "filesystem boundary path component is not a directory",
                            ));
                        }
                        resolved = next;
                    }
                }
            }
        }
        Ok(resolved)
    }
}

#[derive(Debug)]
enum ResolutionStep {
    Name(OsString),
    Parent,
    Directory,
}

fn resolution_steps(path: &Path) -> io::Result<VecDeque<ResolutionStep>> {
    let mut steps = VecDeque::new();
    for part in path.components() {
        match part {
            Component::Normal(name) => steps.push_back(ResolutionStep::Name(name.to_os_string())),
            Component::ParentDir => steps.push_back(ResolutionStep::Parent),
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir => {
                return Err(boundary_refusal("symlink target has an unbounded root"));
            }
        }
    }
    Ok(steps)
}

fn requires_directory(path: &Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    bytes == b"."
        || bytes.ends_with(b"/")
        || bytes.ends_with(b"/.")
        || (cfg!(windows) && (bytes.ends_with(b"\\") || bytes.ends_with(b"\\.")))
}

fn validate_root_metadata(metadata: &std::fs::Metadata) -> io::Result<()> {
    if metadata.file_type().is_symlink() {
        return Err(boundary_refusal(
            "the boundary itself must not be a symlink",
        ));
    }
    if !metadata.is_dir() {
        return Err(invalid_boundary("must name an existing directory"));
    }
    Ok(())
}

fn invalid_boundary(reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("EE_FS_BOUNDARY {reason}"),
    )
}

fn boundary_root_inspection_error(error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("EE_FS_BOUNDARY could not inspect the configured root: {error}"),
    )
}

fn boundary_refusal(reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("EE_FS_BOUNDARY: {reason}"),
    )
}

/// Load and validate the opt-in boundary. An invalid value never disables it.
pub(crate) fn fs_boundary() -> io::Result<Option<FsBoundary>> {
    env_registry::read_os(EnvVar::FsBoundary)
        .map(|value| FsBoundary::new(Path::new(&value)))
        .transpose()
}

/// Plan an existing no-follow walk without touching trusted ancestors.
pub(crate) fn boundary_walk_plan(path: &Path) -> io::Result<Option<(PathBuf, PathBuf)>> {
    fs_boundary()?
        .map(|boundary| boundary.walk_plan(path))
        .transpose()
}

/// Preserve ordinary canonicalization when no filesystem boundary is set.
pub(crate) fn canonicalize_path(path: &Path) -> io::Result<PathBuf> {
    canonicalize_with_boundary(path, fs_boundary()?.as_ref())
}

fn canonicalize_with_boundary(path: &Path, boundary: Option<&FsBoundary>) -> io::Result<PathBuf> {
    match boundary {
        Some(boundary) => boundary.canonicalize(path),
        None => path.canonicalize(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, FsBoundary) {
        let temp = tempfile::tempdir().unwrap();
        let boundary = FsBoundary::new(temp.path()).unwrap();
        (temp, boundary)
    }

    #[test]
    fn invalid_boundary_configuration_is_never_treated_as_unset() {
        let (temp, _) = fixture();
        for path in [Path::new(""), Path::new("relative")] {
            assert_eq!(
                FsBoundary::new(path).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        let parent = temp.path().join("child/..");
        assert_eq!(
            FsBoundary::new(&parent).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            FsBoundary::new(&temp.path().join("missing"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        let file = temp.path().join("file");
        std::fs::write(&file, b"not a directory").unwrap();
        assert_eq!(
            FsBoundary::new(&file).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn lexical_admission_preserves_boundary_and_refuses_parent_traversal() {
        let (temp, boundary) = fixture();
        let child = temp.path().join("nested/child");
        assert_eq!(boundary.ensure_within(&child).unwrap(), child);
        assert_eq!(boundary.ensure_within(temp.path()).unwrap(), temp.path());
        assert_eq!(
            boundary
                .ensure_within_from(Path::new("./nested/child"), temp.path())
                .unwrap(),
            child
        );
        assert_eq!(
            boundary
                .ensure_within(&temp.path().join("./nested//child"))
                .unwrap(),
            child
        );
        for path in [
            temp.path().join("../other"),
            temp.path().join("nested/../child"),
            temp.path().join("link/../child"),
            temp.path()
                .join("../")
                .join(temp.path().file_name().unwrap()),
            temp.path().with_extension("sibling"),
        ] {
            assert_eq!(
                boundary.ensure_within(&path).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied,
                "{}",
                path.display()
            );
        }
        assert!(
            boundary
                .ensure_within(&temp.path().join("..ordinary-name"))
                .is_ok()
        );
    }

    #[test]
    fn walk_begins_at_boundary_including_the_boundary_itself() {
        let (temp, boundary) = fixture();
        let path = temp.path().join(".ee/keys/root.json");
        let (mut base, suffix) = boundary.walk_plan(&path).unwrap();
        let mut inspected = Vec::new();
        for component in suffix.components() {
            base.push(component.as_os_str());
            inspected.push(base.clone());
        }
        assert_eq!(inspected.first().unwrap(), boundary.root());
        assert_eq!(inspected.last().unwrap(), &path);
        assert!(
            inspected
                .iter()
                .all(|path| path.starts_with(boundary.root()))
        );
        let (base, suffix) = boundary.walk_plan(boundary.root()).unwrap();
        assert_eq!(base.join(suffix), boundary.root());
    }

    #[cfg(unix)]
    #[test]
    fn filesystem_root_has_no_parent_probe() {
        let boundary = FsBoundary::new(Path::new("/")).unwrap();
        assert_eq!(
            boundary.walk_plan(Path::new("/")).unwrap(),
            (PathBuf::from("/"), PathBuf::new())
        );
        assert_eq!(
            boundary.walk_plan(Path::new("/child")).unwrap(),
            (PathBuf::from("/"), PathBuf::from("child"))
        );
    }

    #[test]
    fn bounded_canonicalization_checks_real_components_and_missing_paths() {
        let (temp, boundary) = fixture();
        let dir = temp.path().join("nested");
        std::fs::create_dir(&dir).unwrap();
        let file = dir.join("file");
        std::fs::write(&file, b"inside").unwrap();
        let mut probes = Vec::new();
        let resolved = boundary
            .canonicalize_with(
                &file,
                |path| {
                    probes.push(path.to_path_buf());
                    std::fs::symlink_metadata(path)
                },
                |_| panic!("plain path must not read a link"),
            )
            .unwrap();
        assert_eq!(resolved, file);
        assert_eq!(probes, [boundary.root().to_path_buf(), dir, file.clone()]);
        assert_eq!(
            boundary
                .canonicalize(&file.join("child"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotADirectory
        );
        assert_eq!(
            boundary.canonicalize(&file.join(".")).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        assert_eq!(
            boundary
                .canonicalize(&temp.path().join("missing"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            canonicalize_with_boundary(&file, None).unwrap(),
            file.canonicalize().unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn boundary_symlink_is_refused_without_following_it() {
        let (temp, _) = fixture();
        let link = temp.path().join("boundary-link");
        std::os::unix::fs::symlink(temp.path(), &link).unwrap();
        assert_eq!(
            FsBoundary::new(&link).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_canonicalization_allows_only_internal_symlink_targets() {
        use std::os::unix::fs::symlink;
        let (temp, boundary) = fixture();
        let nested = temp.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let file = temp.path().join("file");
        std::fs::write(&file, b"inside").unwrap();
        symlink("../file", nested.join("relative")).unwrap();
        symlink(&file, nested.join("absolute")).unwrap();
        symlink("nested/relative", temp.path().join("chain")).unwrap();
        for path in [
            nested.join("relative"),
            nested.join("absolute"),
            temp.path().join("chain"),
        ] {
            let mut probes = Vec::new();
            assert_eq!(
                boundary
                    .canonicalize_with(
                        &path,
                        |path| {
                            probes.push(path.to_path_buf());
                            std::fs::symlink_metadata(path)
                        },
                        |path| std::fs::read_link(path)
                    )
                    .unwrap(),
                file
            );
            assert!(
                probes
                    .iter()
                    .all(|probe| probe.starts_with(boundary.root()))
            );
        }
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), temp.path().join("outside")).unwrap();
        symlink("../outside", temp.path().join("relative-outside")).unwrap();
        symlink(
            format!(
                "../{}/file",
                temp.path().file_name().unwrap().to_string_lossy()
            ),
            temp.path().join("reentry"),
        )
        .unwrap();
        symlink("outside/../file", temp.path().join("hidden-escape")).unwrap();
        for name in ["outside", "relative-outside", "reentry", "hidden-escape"] {
            let mut probes = Vec::new();
            let error = boundary
                .canonicalize_with(
                    &temp.path().join(name),
                    |path| {
                        probes.push(path.to_path_buf());
                        std::fs::symlink_metadata(path)
                    },
                    |path| std::fs::read_link(path),
                )
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{name}");
            assert!(
                probes
                    .iter()
                    .all(|probe| probe.starts_with(boundary.root()))
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn bounded_canonicalization_limits_link_cycles() {
        use std::os::unix::fs::symlink;
        let (temp, boundary) = fixture();
        symlink("cycle-b", temp.path().join("cycle-a")).unwrap();
        symlink("cycle-a", temp.path().join("cycle-b")).unwrap();
        let error = boundary
            .canonicalize(&temp.path().join("cycle-a"))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("too many symbolic links"));
    }

    // APFS rejects the invalid UTF-8 fixture before canonicalization can run.
    #[cfg(all(unix, not(target_vendor = "apple")))]
    #[test]
    fn bounded_canonicalization_preserves_non_utf8_names() {
        use std::os::unix::ffi::OsStringExt;
        let (temp, boundary) = fixture();
        let file = temp.path().join(OsString::from_vec(vec![b'n', 0xff]));
        std::fs::write(&file, b"inside").unwrap();
        assert_eq!(boundary.canonicalize(&file).unwrap(), file);
    }

    #[cfg(unix)]
    #[test]
    fn bounded_canonicalization_preserves_symlink_parent_and_directory_semantics() {
        use std::os::unix::fs::symlink;
        let (temp, boundary) = fixture();
        let nested = temp.path().join("nested");
        let deeper = nested.join("deeper");
        std::fs::create_dir_all(&deeper).unwrap();
        let file = nested.join("file");
        std::fs::write(&file, b"inside").unwrap();
        symlink("nested/deeper", temp.path().join("directory-link")).unwrap();
        symlink(
            "directory-link/../file",
            temp.path().join("parent-after-link"),
        )
        .unwrap();
        symlink(
            temp.path().join("./nested/deeper/../file"),
            temp.path().join("absolute-parent"),
        )
        .unwrap();
        for name in ["parent-after-link", "absolute-parent"] {
            assert_eq!(
                boundary.canonicalize(&temp.path().join(name)).unwrap(),
                file
            );
        }
        symlink("nested/file/", temp.path().join("file-with-slash")).unwrap();
        assert_eq!(
            boundary
                .canonicalize(&temp.path().join("file-with-slash"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotADirectory
        );
        let untouched = boundary
            .canonicalize_with(
                &temp.path().join("directory-link/../file"),
                |_| panic!("caller parent traversal must be rejected before probing"),
                |_| panic!("caller parent traversal must be rejected before reading links"),
            )
            .unwrap_err();
        assert_eq!(untouched.kind(), io::ErrorKind::PermissionDenied);
    }
}
