//! Production discovery for colocated Unix ee/cass installations.
//!
//! The running executable supplies the extra installation directory, never
//! HOME, PATH, argv[0], or the working directory. Explicit choices and system
//! locations keep precedence. Inspection does not execute either binary.

use std::path::{Path, PathBuf};

use super::{CassError, DiscoveredBinary, DiscoverySource, client};

/// Discover an import executable, including a safely installed sibling of ee.
///
/// Environment/config overrides and the system allowlist retain the existing
/// validation and precedence. If neither resolves, Unix installations may use
/// `cass` beside the actual running `ee` executable. Both files and every
/// ancestor must be non-symlinked, not group/world-writable, and owned by root
/// or the owner of ee. An untrusted sibling preserves the original diagnostic.
/// Other platforms retain the existing discovery behavior.
///
/// # Errors
///
/// Returns the original discovery error if no trusted executable is available.
/// An invalid explicit override is never replaced by automatic discovery.
pub fn discover_import_binary(
    config_override: Option<&Path>,
) -> Result<DiscoveredBinary, CassError> {
    with_colocated_candidate(client::discover_import_binary(config_override), || {
        #[cfg(unix)]
        {
            let executable = std::env::current_exe().ok()?;
            trusted_sibling_with(&executable, inspect_entry)
        }
        #[cfg(not(unix))]
        {
            None
        }
    })
}

fn with_colocated_candidate(
    discovered: Result<DiscoveredBinary, CassError>,
    sibling: impl FnOnce() -> Option<PathBuf>,
) -> Result<DiscoveredBinary, CassError> {
    match discovered {
        Err(error)
            if matches!(
                &error,
                CassError::BinaryNotFound { .. } | CassError::FoundButUntrusted { .. }
            ) =>
        {
            sibling()
                .map(|path| DiscoveredBinary::new(path, DiscoverySource::Path))
                .ok_or(error)
        }
        result => result,
    }
}

#[cfg(unix)]
#[derive(Clone, Copy)]
struct Entry {
    file: bool,
    directory: bool,
    mode: u32,
    owner: u32,
}

#[cfg(unix)]
fn inspect_entry(path: &Path) -> std::io::Result<Entry> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::symlink_metadata(path)?;
    Ok(Entry {
        file: metadata.file_type().is_file(),
        directory: metadata.file_type().is_dir(),
        mode: metadata.mode(),
        owner: metadata.uid(),
    })
}

#[cfg(unix)]
fn trusted_sibling_with(
    executable: &Path,
    mut inspect: impl FnMut(&Path) -> std::io::Result<Entry>,
) -> Option<PathBuf> {
    // A library host, renamed executable, or test harness is not an ee install.
    // Reject parent traversal before inspection, not after canonicalization.
    if !executable.is_absolute()
        || executable.file_name() != Some(std::ffi::OsStr::new("ee"))
        || executable
            .components()
            .any(|component| component == std::path::Component::ParentDir)
    {
        return None;
    }
    let own = inspect(executable).ok()?;
    let trusted =
        |entry: Entry| entry.mode & 0o022 == 0 && (entry.owner == 0 || entry.owner == own.owner);
    if !own.file || own.mode & 0o111 == 0 || !trusted(own) {
        return None;
    }
    let parent = executable.parent()?;
    let candidate = parent.join(super::DEFAULT_BINARY);
    let sibling = inspect(&candidate).ok()?;
    if !sibling.file || sibling.mode & 0o111 == 0 || !trusted(sibling) {
        return None;
    }
    // Checking only the direct parent would admit a writable HOME ancestor.
    // Sticky temporary directories are not automatic installation authorities.
    for ancestor in parent.ancestors() {
        let entry = inspect(ancestor).ok()?;
        if !entry.directory || !trusted(entry) {
            return None;
        }
    }
    Some(candidate)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn explicit_and_system_discovery_win_without_inspecting_a_sibling() {
        for source in [
            DiscoverySource::EnvVar,
            DiscoverySource::Config,
            DiscoverySource::Path,
        ] {
            let expected = DiscoveredBinary::new(PathBuf::from("/chosen/cass"), source);
            let actual = with_colocated_candidate(Ok(expected.clone()), || {
                panic!("successful discovery must not consult another location")
            });
            assert_eq!(actual.expect("existing discovery"), expected);
        }
        let invalid = CassError::InvalidBinary {
            binary: PathBuf::from("relative/cass"),
            reason: "explicit override is invalid".to_owned(),
        };
        assert!(matches!(
            with_colocated_candidate(Err(invalid), || panic!("invalid override must fail")),
            Err(CassError::InvalidBinary { .. })
        ));
    }

    #[test]
    fn absent_and_untrusted_path_results_can_use_a_proven_sibling() {
        for error in [
            CassError::BinaryNotFound {
                binary: PathBuf::from("cass"),
            },
            CassError::FoundButUntrusted {
                found_at: PathBuf::from("/hostile/cass"),
            },
        ] {
            let expected = PathBuf::from("/home/operator/.local/bin/cass");
            let actual = with_colocated_candidate(Err(error), || Some(expected.clone()))
                .expect("proven installation sibling");
            assert_eq!(actual.path, expected);
            assert_eq!(actual.source, DiscoverySource::Path);
            let client = super::super::CassClient::from_discovered(actual);
            // The client retains an absolute executable, not a PATH lookup.
            assert_eq!(client.binary(), expected.as_path());
        }
    }

    #[test]
    fn rejected_sibling_preserves_the_original_untrusted_path_diagnostic() {
        let found_at = PathBuf::from("/hostile/cass");
        let result = with_colocated_candidate(
            Err(CassError::FoundButUntrusted {
                found_at: found_at.clone(),
            }),
            || None,
        );
        assert!(
            matches!(result, Err(CassError::FoundButUntrusted { found_at: actual }) if actual == found_at)
        );
        assert!(matches!(
            with_colocated_candidate(
                Err(CassError::BinaryNotFound {
                    binary: PathBuf::from("cass"),
                }),
                || None,
            ),
            Err(CassError::BinaryNotFound { .. })
        ));
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::collections::BTreeMap;

        const EXECUTABLE: &str = "/home/operator/.local/bin/ee";
        const SIBLING: &str = "/home/operator/.local/bin/cass";

        fn installation() -> BTreeMap<PathBuf, Entry> {
            let mut entries = BTreeMap::new();
            for path in [EXECUTABLE, SIBLING] {
                entries.insert(
                    PathBuf::from(path),
                    Entry {
                        file: true,
                        directory: false,
                        mode: 0o755,
                        owner: 1000,
                    },
                );
            }
            for path in Path::new(EXECUTABLE).parent().unwrap().ancestors() {
                entries.insert(
                    path.to_owned(),
                    Entry {
                        file: false,
                        directory: true,
                        mode: 0o755,
                        owner: if path == Path::new("/") || path == Path::new("/home") {
                            0
                        } else {
                            1000
                        },
                    },
                );
            }
            entries
        }

        fn resolve(entries: &BTreeMap<PathBuf, Entry>) -> Option<PathBuf> {
            trusted_sibling_with(Path::new(EXECUTABLE), |path| {
                entries.get(path).copied().ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::NotFound, "missing fixture entry")
                })
            })
        }

        #[test]
        fn owner_installation_under_local_bin_is_accepted_without_home_or_path() {
            let mut entries = installation();
            assert_eq!(resolve(&entries), Some(PathBuf::from(SIBLING)));
            entries.get_mut(Path::new(SIBLING)).unwrap().owner = 0;
            assert_eq!(resolve(&entries), Some(PathBuf::from(SIBLING)));
        }

        #[test]
        fn every_file_and_ancestor_rejects_group_and_world_writes() {
            let original = installation();
            for path in original.keys() {
                for bits in [0o020, 0o002, 0o022, 0o1002] {
                    let mut entries = original.clone();
                    entries.get_mut(path).unwrap().mode |= bits;
                    assert!(
                        resolve(&entries).is_none(),
                        "{} mode {bits:o}",
                        path.display()
                    );
                }
            }
        }

        #[test]
        fn untrusted_owners_and_non_directory_ancestors_are_rejected() {
            let original = installation();
            for path in original
                .keys()
                .filter(|path| path.as_path() != Path::new(EXECUTABLE))
            {
                let mut entries = original.clone();
                entries.get_mut(path).unwrap().owner = 2000;
                assert!(resolve(&entries).is_none(), "owner: {}", path.display());
            }
            for path in Path::new(EXECUTABLE).parent().unwrap().ancestors() {
                let mut entries = original.clone();
                let entry = entries.get_mut(path).unwrap();
                entry.directory = false;
                entry.file = true;
                assert!(
                    resolve(&entries).is_none(),
                    "not a directory: {}",
                    path.display()
                );
            }
        }

        #[test]
        fn symlinks_missing_entries_and_nonexecutables_are_rejected() {
            let original = installation();
            for path in original.keys() {
                let mut entries = original.clone();
                let entry = entries.get_mut(path).unwrap();
                entry.file = false;
                entry.directory = false;
                assert!(
                    resolve(&entries).is_none(),
                    "symlink/special: {}",
                    path.display()
                );
                entries.remove(path);
                assert!(resolve(&entries).is_none(), "missing: {}", path.display());
            }
            for path in [EXECUTABLE, SIBLING] {
                let mut entries = original.clone();
                entries.get_mut(Path::new(path)).unwrap().mode = 0o644;
                assert!(resolve(&entries).is_none(), "not executable: {path}");
            }
        }

        #[test]
        fn library_hosts_relative_paths_and_parent_traversal_cannot_supply_locations() {
            for path in ["ee", "./ee", "/opt/host", "/opt/ee-test", "/opt/../bin/ee"] {
                assert!(
                    trusted_sibling_with(Path::new(path), |_| {
                        panic!("invalid executable identity must reject before inspection")
                    })
                    .is_none(),
                    "{path}"
                );
            }
        }

        #[test]
        fn filesystem_errors_cannot_authorize_an_installation() {
            for kind in [
                std::io::ErrorKind::PermissionDenied,
                std::io::ErrorKind::NotFound,
            ] {
                assert!(
                    trusted_sibling_with(Path::new(EXECUTABLE), |_| {
                        Err(std::io::Error::new(kind, "injected inspection failure"))
                    })
                    .is_none()
                );
            }
        }
    }
}
