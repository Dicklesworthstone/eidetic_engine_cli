//! K5 drift gate (eidetic_engine_cli bd-17c65.11.5).
//!
//! Asserts that v0.1-era schema strings and field names are not
//! re-introduced into `src/`. Per K5 spec the test grepts `src/`
//! only — tests, docs, and scripts can legitimately reference the
//! old names (migration guides cite them, contract tests assert
//! either-version envelopes for forward-compat).
//!
//! The forbidden list is calibrated: each entry is something that
//! is verified gone from `src/` today. Adding a new entry that
//! still appears in production code will fail this test loudly
//! and force the author to either remove the v1 emission or
//! retract the migration claim. That asymmetry is the point —
//! migration claims need teeth.

use std::fs;
use std::path::{Path, PathBuf};

type TestResult = Result<(), String>;

/// Forbidden v1-era literals that must not appear in `src/`.
///
/// Each entry is split with `concat!` so the literal does not match
/// against this test file itself (this file is in tests/, which we
/// don't scan, but defending against future refactors that scope
/// the walker more broadly).
///
/// Why these specific entries:
///
/// - `ee.error.v1` — the v1 error envelope schema; v0.2 uses
///   `ee.error.v2`. Verified gone after `ERROR_SCHEMA_V1` const was
///   deleted (bd-17c65.11.5).
fn forbidden_terms() -> Vec<&'static str> {
    // Use Box::leak to get 'static slice from concat! at runtime.
    // concat! produces a &'static str, but the split-string idiom
    // protects against this file accidentally matching its own
    // literal.
    vec![
        // Schema versions retired in v0.2:
        concat!("ee.error", ".v1"),
    ]
}

/// Walk `src/` collecting `*.rs` files we want to scan.
///
/// Skips test modules (`#[cfg(test)]`) implicitly by file structure
/// — inline test modules live in the same `.rs` files as production
/// code, so we can't separate them cheaply. The forbidden list is
/// calibrated to only call out things that should be gone from BOTH
/// production and inline tests.
fn rust_source_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|error| format!("read {}: {error}", dir.display()))? {
        let entry = entry.map_err(|error| format!("read {} entry: {error}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            rust_source_files(&path, files)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            files.push(path);
        }
    }
    Ok(())
}

#[test]
fn src_does_not_emit_retired_v1_literals() -> TestResult {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_source_files(&root.join("src"), &mut files)?;

    let mut failures = Vec::new();
    for path in &files {
        let text = fs::read_to_string(path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        for needle in forbidden_terms() {
            if text.contains(needle) {
                failures.push(format!(
                    "{} contains forbidden v1 literal `{needle}`",
                    path.display()
                ));
            }
        }
    }

    if !failures.is_empty() {
        return Err(failures.join("\n"));
    }
    Ok(())
}
