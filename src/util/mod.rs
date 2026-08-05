//! Shared low-level utilities.

use std::path::{Path, PathBuf};

pub mod radix_ulid_sort;

/// Canonicalize only a caller-designated trusted prefix. Never use an
/// arbitrary configured path as `prefix`: doing so would hide hostile symlink
/// components from downstream safety checks.
fn path_with_canonical_prefix(path: &Path, prefix: &Path) -> PathBuf {
    let Ok(suffix) = path.strip_prefix(prefix) else {
        return path.to_path_buf();
    };
    let Ok(canonical_prefix) = prefix.canonicalize() else {
        return path.to_path_buf();
    };
    canonical_prefix.join(suffix)
}

/// Resolve only the operating system's process-temp prefix while preserving
/// the caller's remaining path components.
///
/// macOS commonly reports the process temp directory below `/var`, a
/// root-owned compatibility symlink to `/private/var`. Security checks that
/// reject every symlink component must not reject that OS-selected prefix, but
/// they must continue to inspect attacker-controlled components below it.
#[must_use]
pub(crate) fn path_with_canonical_process_temp_prefix(path: &Path) -> PathBuf {
    let temp_dir = std::env::temp_dir();
    path_with_canonical_prefix(path, &temp_dir)
}

#[cfg(test)]
mod tests {
    use super::{path_with_canonical_prefix, path_with_canonical_process_temp_prefix};
    use std::path::Path;

    #[test]
    fn process_temp_child_uses_canonical_temp_prefix() {
        let temp_dir = std::env::temp_dir();
        let child = temp_dir.join("ee-temp-prefix-test").join("child");
        let expected = temp_dir
            .canonicalize()
            .unwrap_or_else(|_| temp_dir.clone())
            .join("ee-temp-prefix-test")
            .join("child");

        assert_eq!(path_with_canonical_process_temp_prefix(&child), expected);
    }

    #[test]
    fn path_outside_prefix_is_unchanged() {
        let path = Path::new("relative-cache/entry.json");
        let prefix = std::env::temp_dir();

        assert_eq!(path_with_canonical_prefix(path, &prefix), path);
    }

    #[cfg(unix)]
    #[test]
    fn canonical_prefix_preserves_symlinked_descendant() {
        use std::fs;
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("create isolated filesystem");
        let canonical_prefix = temp.path().join("canonical-prefix");
        fs::create_dir(&canonical_prefix).expect("create canonical prefix");
        let alias_prefix = temp.path().join("alias-prefix");
        symlink(&canonical_prefix, &alias_prefix).expect("create trusted prefix alias");

        let outside = temp.path().join("outside");
        fs::create_dir(&outside).expect("create outside directory");
        fs::write(outside.join("entry.json"), b"outside").expect("create outside entry");
        let descendant_link = canonical_prefix.join("descendant-link");
        symlink(&outside, &descendant_link).expect("create untrusted descendant symlink");

        let input = alias_prefix.join("descendant-link").join("entry.json");
        let normalized = path_with_canonical_prefix(&input, &alias_prefix);
        let expected = canonical_prefix
            .canonicalize()
            .expect("canonicalize trusted prefix")
            .join("descendant-link")
            .join("entry.json");

        assert_eq!(normalized, expected);
        assert_ne!(
            normalized.canonicalize().expect("resolve full input"),
            normalized,
            "normalization must leave descendant symlinks visible to the safety walker"
        );
    }
}
