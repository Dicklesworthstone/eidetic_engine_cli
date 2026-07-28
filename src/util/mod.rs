//! Shared low-level utilities.

use std::path::{Path, PathBuf};

pub mod radix_ulid_sort;

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
    let Ok(suffix) = path.strip_prefix(&temp_dir) else {
        return path.to_path_buf();
    };
    let Ok(canonical_temp_dir) = temp_dir.canonicalize() else {
        return path.to_path_buf();
    };
    canonical_temp_dir.join(suffix)
}

#[cfg(test)]
mod tests {
    use super::path_with_canonical_process_temp_prefix;

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
}
