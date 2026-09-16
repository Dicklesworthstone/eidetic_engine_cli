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

/// Prefixes that mark a filesystem path as sensitive in public output.
///
/// This is the union of the hand-maintained lists that used to live in each
/// redactor, reduced to a minimal cover: every longer entry those lists carried
/// (`/etc/ssh/`, `/var/lib/docker/`, `/private/var/run/`) is already implied by
/// a shorter one here. Twenty copies diverged into three incompatible families,
/// and `/root/` reached exactly one of them, so a provenance URI naming
/// `/root/.ssh/id_rsa` was redacted on one surface and emitted verbatim by
/// nineteen (bd-redactor-prefix-divergence-lsy52).
pub(crate) const SENSITIVE_PATH_PREFIXES: &[&str] = &[
    "/Users/",
    "/Volumes/",
    "/__w/",
    "/app/",
    "/data/",
    "/dev/",
    "/dp/",
    "/etc/",
    "/github/workspace/",
    "/home/",
    "/media/",
    "/mnt/",
    "/private/",
    "/proc/",
    "/repo/",
    "/root/",
    "/run/",
    "/sys/",
    "/tmp/",
    "/var/",
    "/workspace/",
    "/workspaces/",
];

/// Is the byte at `start` the first character of a redactable path?
///
/// Three categories, because a prefix list alone cannot see the last two: a
/// sensitive POSIX prefix, a Windows drive path (`C:\` or `C:/`), or a UNC
/// share. Anything directly after a `file://` scheme also counts, because the
/// scheme declares the value to be a filesystem location even when the path
/// that follows matches no prefix (`file://relative/notes.md`).
///
/// The scheme itself is deliberately NOT a start position. It says "this is a
/// path" without saying where, so redacting from the scheme destroys a signal
/// while protecting nothing, and turns `file://[REDACTED_PATH]` into a bare
/// `[REDACTED_PATH]`. Seven surfaces assert the prefix survives; an earlier
/// revision of this predicate returned `true` for the scheme and broke them.
///
/// The drive and UNC forms additionally require a token boundary before them so
/// that a bare `C:` inside a word is not mistaken for a path root.
fn sensitive_path_starts_at(value: &str, start: usize) -> bool {
    const FILE_SCHEME: &str = "file://";

    let candidate = &value[start..];
    if start
        .checked_sub(FILE_SCHEME.len())
        .and_then(|scheme_start| value.get(scheme_start..start))
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case(FILE_SCHEME))
    {
        return true;
    }
    if SENSITIVE_PATH_PREFIXES
        .iter()
        .any(|prefix| candidate.starts_with(prefix))
    {
        return true;
    }

    let token_boundary_before = value[..start].chars().next_back().is_none_or(|previous| {
        previous.is_whitespace() || matches!(previous, '"' | '\'' | '`' | '(' | '[' | '{' | '=')
    });
    if !token_boundary_before {
        return false;
    }

    let bytes = candidate.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
    {
        return true;
    }
    candidate.starts_with(r"\\")
}

/// Replace every redactable path in `value` with `[REDACTED_PATH]`.
///
/// `boundary` decides where each redacted run ends and stays caller-supplied on
/// purpose: prose surfaces terminate a path at whitespace, whole-field surfaces
/// (a captured source path may legitimately contain spaces) terminate only at a
/// newline. Sharing that decision would either eat the words after a path in an
/// error message or truncate a real path at its first space, so only the
/// *start* predicate is shared.
pub(crate) fn redact_path_like_segments(value: &str, boundary: fn(char) -> bool) -> String {
    const REDACTED_PATH: &str = "[REDACTED_PATH]";

    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < value.len() {
        let Some(start) = value[cursor..]
            .char_indices()
            .map(|(relative, _)| cursor + relative)
            .find(|start| sensitive_path_starts_at(value, *start))
        else {
            output.push_str(&value[cursor..]);
            break;
        };

        output.push_str(&value[cursor..start]);
        output.push_str(REDACTED_PATH);
        cursor = value[start..]
            .char_indices()
            .skip(1)
            .find_map(|(index, ch)| boundary(ch).then_some(start + index))
            .unwrap_or(value.len());
    }
    output
}
