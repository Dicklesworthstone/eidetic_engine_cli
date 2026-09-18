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

/// Prefixes that mark a filesystem path as sensitive in public output.
///
/// This is the union of the hand-maintained lists that used to live in each
/// redactor, reduced to a minimal cover: every longer entry those lists carried
/// (`/etc/ssh/`, `/var/lib/docker/`, `/private/var/run/`) is already implied by
/// a shorter one here. Twenty copies diverged into three incompatible families,
/// and `/root/` reached exactly one of them, so a provenance URI naming
/// `/root/.ssh/id_rsa` was redacted on one surface and emitted verbatim by
/// nineteen (bd-redactor-prefix-divergence-lsy52).
///
/// # Finding the copies, if you need to audit this population again
///
/// The count has moved 21 → 22 → 24 → 27, and **not once because a sweep said
/// so**. Every correction came from resolving a grep hit to its enclosing
/// function. Two sweeps are needed and neither is sufficient alone:
///
/// 1. **By data shape.** `grep -rn '"/Users/"' src/` finds copies carrying their
///    own literal prefix list. Then resolve EVERY hit to its enclosing function
///    and classify by hand — redactor, test assertion, or deny-list. Most hits
///    are `assert!(!x.contains(...))`, which are leak DETECTORS, not redactors;
///    the function name is the only thing that separates them, so counting hits
///    is worthless.
///
/// 2. **By set difference**, which is what finds a copy that took the DATA and
///    kept its own PREDICATE — invisible to sweep 1 (no literal list) and to a
///    helper-name grep (own fn name, and it may reference the shared helper
///    elsewhere in the same file):
///
///    ```text
///    A = files referencing SENSITIVE_PATH_PREFIXES      (took the data)
///    B = files calling redact_path_like_segments        (took the rule)
///    A \ B                                              = divergence-capable
///    ```
///
/// SHARING THE DATA IS NOT SHARING THE RULE, and that is the standing risk.
/// Several surfaces now use this const with their own walker, so they can
/// diverge in PREDICATE logic while every prefix-based audit reports them
/// clean. That is exactly how the 27th broke: correct prefixes, wrong boundary
/// rule — `src/output/jsonl_export.rs` omitted the token-boundary check that
/// [`sensitive_path_starts_at`] applies before a Windows drive root, so the
/// `e:` inside `file://` read as drive `e:` and it emitted
/// `fil[REDACTED_PATH]` for `file:///Users/...`, eating the scheme (df1f10fa9).
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
/// share.
///
/// A `file://` scheme is NOT one of them, in either direction. The scheme
/// itself is not a start position — redacting from it turns
/// `file://[REDACTED_PATH]` into a bare `[REDACTED_PATH]`, destroying a signal
/// while protecting nothing. But the position immediately AFTER it is not a
/// start position either, and an earlier revision of this predicate made it
/// one. That over-redacted every repo-relative citation: `ee search --json`
/// emitted `file://[REDACTED_PATH]` for `file://AGENTS.md#L42`, destroying the
/// provenance this product exists to preserve, and protecting nothing — a
/// repo-relative path is the pinnable, non-sensitive kind.
///
/// No special case is needed in either direction. `file:///Users/alice/x`
/// redacts because `/Users/` is a sensitive prefix found at offset 7, which
/// leaves the scheme intact for free; `file://AGENTS.md` matches no prefix and
/// stays whole. Every existing assertion on this behaviour uses an absolute
/// path after the scheme, and all of them are unaffected.
///
/// The drive and UNC forms additionally require a token boundary before them so
/// that a bare `C:` inside a word is not mistaken for a path root.
///
/// Prefixes compare case-insensitively. macOS and Windows both resolve
/// `/USERS/alice` and `/Users/alice` to the same file, so a case-sensitive
/// comparison redacts one spelling and emits the other. The repository's own
/// leak detectors already lowercase before matching these exact tokens
/// (`fixtures_do_not_leak_pids_paths_or_secrets`,
/// `contains_forbidden_secret_or_private_path`), so a case-sensitive redactor
/// can emit output that those gates classify as a leak.
/// Is `start` the second slash of a `scheme://` authority marker?
///
/// In `scheme://authority/path` the `//` introduces an AUTHORITY, not a path,
/// and its leading slash is not a path root. The prefix list cannot see that
/// distinction on its own: `agent://run/public-feedback` contains the literal
/// `/run/` at offset 7, matches the `/run/` prefix, and redacts to
/// `agent:/[REDACTED_PATH]` -- destroying a safe, non-filesystem source id
/// because a URI host happens to share a name with a Linux runtime directory.
/// `feedback_health_source_counts_preserve_safe_source_ids` is the assertion
/// that catches it.
///
/// This does not weaken redaction of a real path carried by a URI. Only the
/// authority's own leading slash is excluded, so `agent://host/root/.ssh/id_rsa`
/// still redacts from `/root/`, and a bare `//run/` with no scheme in front of
/// it is untouched by this rule. `file://` is likewise unaffected in either
/// direction: its authority is empty, so `file:///Users/alice/x` matches at the
/// THIRD slash -- the genuine path root -- which is why the scheme survives.
fn uri_authority_slash_at(value: &str, start: usize) -> bool {
    if start == 0 || value.as_bytes().get(start) != Some(&b'/') {
        return false;
    }
    if value.as_bytes().get(start - 1) != Some(&b'/') {
        return false;
    }
    let Some(colon) = start.checked_sub(2) else {
        return false;
    };
    if value.as_bytes().get(colon) != Some(&b':') {
        return false;
    }
    value[..colon]
        .bytes()
        .next_back()
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
}

pub(crate) fn sensitive_path_starts_at(value: &str, start: usize) -> bool {
    if uri_authority_slash_at(value, start) {
        return false;
    }

    let candidate = &value[start..];
    if SENSITIVE_PATH_PREFIXES.iter().any(|prefix| {
        candidate
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    }) {
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

    /// Both directions, because either alone is satisfiable by a redactor that
    /// is simply wrong in the other: one that redacts everything satisfies the
    /// absolute case, one that redacts nothing satisfies the relative case.
    ///
    /// The relative half is a regression pin. An earlier revision treated the
    /// position immediately after a `file://` scheme as a redaction start, so
    /// `ee search --json` emitted `file://[REDACTED_PATH]` for
    /// `file://AGENTS.md#L42` and destroyed a repo-relative citation.
    #[test]
    fn file_scheme_redacts_absolute_targets_and_preserves_relative_ones() {
        fn at_whitespace(ch: char) -> bool {
            ch.is_whitespace()
        }

        // Absolute, sensitive: redacted, and the scheme SURVIVES so a reader can
        // still tell a filesystem location was withheld.
        assert_eq!(
            super::redact_path_like_segments(
                "source=file:///Users/alice/private/x.json tail",
                at_whitespace,
            ),
            "source=file://[REDACTED_PATH] tail",
        );

        // Repo-relative: preserved whole. Redacting this protects nothing and
        // destroys the provenance the pack, search and why surfaces exist to
        // carry.
        for preserved in [
            "file://AGENTS.md#L42",
            "file://docs/adr/0001-runtime.md",
            "why=file://src/core/search.rs done",
        ] {
            assert_eq!(
                super::redact_path_like_segments(preserved, at_whitespace),
                preserved,
                "repo-relative provenance must survive redaction",
            );
        }

        // A URI AUTHORITY is not a path root, even when it is spelled like a
        // sensitive directory. `agent://run/public-feedback` carries the
        // literal `/run/` at offset 7 and redacted to `agent:/[REDACTED_PATH]`
        // once `/run/` joined the shared prefix list, destroying a safe,
        // non-filesystem source id. Pinned here as well as at the surface
        // (`feedback_health_source_counts_preserve_safe_source_ids`) because
        // this predicate is shared by every redactor the lsy52 sweep
        // consolidated, so a regression here is a regression in all of them.
        for preserved in [
            "agent://run/public-feedback",
            "https://run/foo",
            "ee-export://tmp/fixture",
        ] {
            assert_eq!(
                super::redact_path_like_segments(preserved, at_whitespace),
                preserved,
                "a URI authority must not be redacted as a path root",
            );
        }

        // The narrowing above is confined to the authority's own slash: a real
        // sensitive path carried AFTER an authority still redacts, and a bare
        // `//run/` with no scheme in front of it is untouched by the rule.
        assert_eq!(
            super::redact_path_like_segments("agent://host/root/.ssh/id_rsa", at_whitespace),
            "agent://host[REDACTED_PATH]",
        );
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
