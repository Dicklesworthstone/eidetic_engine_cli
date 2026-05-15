//! bd-3usjw.56 forbidden-suffix audit.
//!
//! AGENTS.md "Code Editing Discipline > No File Proliferation" forbids
//! file variants like `mainV2.rs`, `main_improved.rs`, `main_enhanced.rs`,
//! and `__OPUS`-suffixed siblings. Without a CI gate, the policy relies on
//! reviewer vigilance, which the 2026-05-14 reality-check audit found
//! insufficient (COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md slipped in).
//!
//! This test walks the repo tree (excluding `.git`, `target`, `.beads`,
//! `.beads.recovery_*`, `node_modules`, `/Volumes`) and asserts no file
//! or directory basename matches the forbidden suffix patterns. Items
//! that legitimately contain a forbidden token in a compound test-fixture
//! name (e.g. an insta golden snapshot whose scenario label happens to
//! be "improved") are allow-listed in `FORBIDDEN_SUFFIX_ALLOWLIST` with
//! a documented reason. Each allowlist entry must reference either an
//! open bead that tracks resolution or a permanent justification.
//!
//! See bd-3usjw.56. The companion bd-3usjw.15 tracks the
//! COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md rename; once that lands the
//! corresponding allowlist entry should be removed.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};

const FORBIDDEN_SUFFIX_PATTERNS: &[&str] = &["__OPUS", "_improved", "_enhanced", "_alt"];

/// Paths (relative to CARGO_MANIFEST_DIR, normalized to forward slashes)
/// that legitimately contain a forbidden suffix and are intentionally
/// not file-proliferation. Each entry must include a justification that
/// either (a) names an open tracking bead expected to remove the entry,
/// or (b) explains why the suffix is a load-bearing fixture label rather
/// than a duplicated source file.
const FORBIDDEN_SUFFIX_ALLOWLIST: &[(&str, &str)] = &[
    (
        "COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md",
        "transitional: bd-3usjw.15 (plan_doc_rename) tracks renaming this plan file. \
         Remove this allowlist entry when bd-3usjw.15 closes.",
    ),
    (
        "tests/snapshots/perf_compare_golden__perf_compare_improved.snap",
        "permanent: insta golden snapshot for the `perf_compare_improved` test \
         scenario in tests/perf_compare_golden.rs. The 'improved' substring is a \
         scenario label describing a perf comparison where a later run improved \
         over a baseline; it is not a duplicated-file pattern.",
    ),
    (
        "tests/snapshots/perf_compare_golden__perf_compare_improved.snap.new",
        "permanent: insta's pending-snapshot temp file (matches *.snap.new in \
         .gitignore but tracked git instances may surface in working trees). \
         Same scenario-label justification as the .snap above.",
    ),
];

const PRUNE_DIRS: &[&str] = &[
    ".git",
    "target",
    ".beads",
    "node_modules",
    ".cargo",
    ".rust-cache",
];

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn matches_forbidden(name: &str) -> Option<&'static str> {
    let stem = name.split('.').next().unwrap_or(name);

    for pattern in FORBIDDEN_SUFFIX_PATTERNS {
        if stem.ends_with(pattern) {
            return Some(pattern);
        }
        if name.ends_with(pattern) {
            return Some(pattern);
        }
        let needle = format!("{pattern}.");
        if name.contains(&needle) {
            return Some(pattern);
        }
    }

    if let Some(rest) = stem.rsplit_once("__V") {
        if !rest.1.is_empty() && rest.1.chars().all(|c| c.is_ascii_digit()) {
            return Some("__V<digits>");
        }
    }
    if let Some(rest) = stem.rsplit_once("mainV") {
        if !rest.1.is_empty() && rest.1.chars().all(|c| c.is_ascii_digit()) {
            return Some("mainV<digits>");
        }
    }
    if stem.ends_with("_new") && !is_documented_new_exception(stem) {
        return Some("_new");
    }

    None
}

/// Allow `*_new` only when it is part of a compound identifier ending in a
/// real word, not a "new variant of an existing file". The pattern fires
/// only when `_new` is the entire trailing token after the last underscore.
/// Today, no such file exists in the repo; this helper exists to make the
/// rule's intent self-documenting and to leave a single spot to extend if
/// a legitimate `_new` fixture appears later.
fn is_documented_new_exception(_stem: &str) -> bool {
    false
}

fn is_pruned(path: &Path) -> bool {
    path.components().any(|component| {
        let s = component.as_os_str().to_string_lossy();
        if PRUNE_DIRS.iter().any(|dir| *dir == s) {
            return true;
        }
        if s.starts_with(".beads.recovery_") {
            return true;
        }
        if s.starts_with("Volumes") || s == "/" {
            return false;
        }
        false
    })
}

fn relative_to_manifest(path: &Path, root: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.to_string_lossy().replace('\\', "/")
}

fn walk(root: &Path, hits: &mut Vec<(String, &'static str)>) {
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if is_pruned(&path) {
            continue;
        }

        let basename = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();

        if let Some(pattern) = matches_forbidden(&basename) {
            let rel = relative_to_manifest(&path, &manifest_dir());
            hits.push((rel, pattern));
        }

        if path.is_dir() {
            walk(&path, hits);
        }
    }
}

#[test]
fn no_forbidden_file_or_directory_suffixes() {
    let root = manifest_dir();
    let mut hits: Vec<(String, &'static str)> = Vec::new();
    walk(&root, &mut hits);

    let allowed: HashSet<&'static str> = FORBIDDEN_SUFFIX_ALLOWLIST
        .iter()
        .map(|(path, _)| *path)
        .collect();

    let unexpected: Vec<&(String, &'static str)> = hits
        .iter()
        .filter(|(p, _)| !allowed.contains(p.as_str()))
        .collect();

    assert!(
        unexpected.is_empty(),
        "AGENTS.md 'No File Proliferation' policy violation. Found {} file(s) with \
         forbidden suffix(es) that are NOT documented in FORBIDDEN_SUFFIX_ALLOWLIST. \
         Either rename the file, or add it to the allowlist with a documented reason \
         (e.g. an open tracking bead that will resolve it).\n\nViolations:\n{}",
        unexpected.len(),
        unexpected
            .iter()
            .map(|(p, pat)| format!("  - {p}  (matched pattern: {pat})"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn allowlist_entries_actually_exist_on_disk() {
    let root = manifest_dir();
    let mut missing: Vec<&str> = Vec::new();
    for (path, _reason) in FORBIDDEN_SUFFIX_ALLOWLIST {
        if !root.join(path).exists() {
            missing.push(path);
        }
    }
    assert!(
        missing.is_empty(),
        "FORBIDDEN_SUFFIX_ALLOWLIST contains stale entries (paths that no longer \
         exist on disk). Remove them so the allowlist accurately documents only \
         live exceptions.\n\nStale entries:\n{}",
        missing
            .iter()
            .map(|p| format!("  - {p}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn allowlist_reasons_are_non_empty() {
    for (path, reason) in FORBIDDEN_SUFFIX_ALLOWLIST {
        assert!(
            !reason.trim().is_empty(),
            "FORBIDDEN_SUFFIX_ALLOWLIST entry {path:?} has an empty reason. \
             Every exception must document why."
        );
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn matches_forbidden_catches_opus_suffix() {
        assert_eq!(
            matches_forbidden("COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md"),
            Some("__OPUS")
        );
        assert_eq!(matches_forbidden("foo__OPUS"), Some("__OPUS"));
        assert_eq!(matches_forbidden("foo__OPUS.rs"), Some("__OPUS"));
    }

    #[test]
    fn matches_forbidden_catches_v_digit_suffix() {
        assert_eq!(matches_forbidden("foo__V2"), Some("__V<digits>"));
        assert_eq!(matches_forbidden("foo__V42.rs"), Some("__V<digits>"));
        assert_eq!(matches_forbidden("mainV2.rs"), Some("mainV<digits>"));
    }

    #[test]
    fn matches_forbidden_catches_improved_enhanced_alt() {
        assert_eq!(matches_forbidden("foo_improved.rs"), Some("_improved"));
        assert_eq!(matches_forbidden("foo_enhanced.rs"), Some("_enhanced"));
        assert_eq!(matches_forbidden("foo_alt.rs"), Some("_alt"));
    }

    #[test]
    fn matches_forbidden_skips_innocent_names() {
        assert_eq!(matches_forbidden("main.rs"), None);
        assert_eq!(matches_forbidden("README.md"), None);
        assert_eq!(matches_forbidden("Cargo.toml"), None);
        assert_eq!(matches_forbidden("opus_pricing.rs"), None);
        assert_eq!(matches_forbidden("v2_module.rs"), None);
    }

    #[test]
    fn is_pruned_skips_target_and_git() {
        assert!(is_pruned(Path::new("target/debug/ee")));
        assert!(is_pruned(Path::new("./target/debug/ee")));
        assert!(is_pruned(Path::new(".git/objects/aa/bb")));
        assert!(is_pruned(Path::new("./.beads/issues.jsonl")));
        assert!(is_pruned(Path::new(
            "./.beads.recovery_20260514T045635Z/foo"
        )));
        assert!(!is_pruned(Path::new("src/main.rs")));
        assert!(!is_pruned(Path::new("./tests/no_forbidden_suffixes.rs")));
    }

    #[test]
    fn allowlist_documents_known_exceptions() {
        let paths: Vec<&str> = FORBIDDEN_SUFFIX_ALLOWLIST.iter().map(|(p, _)| *p).collect();
        assert!(
            paths.contains(&"COMPREHENSIVE_PLAN_TO_MAKE_EE__OPUS.md"),
            "the known plan-rename target must remain in the allowlist until \
             bd-3usjw.15 closes"
        );
    }
}
