//! J6 coverage gate for the failure-mode fixture catalog.
//!
//! The structural contract test validates fixtures that already exist. This
//! ignored gate answers the inverse question: which degraded/fallback code
//! literals in `src/` still lack a fixture file?

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

type TestResult<T = ()> = Result<T, String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn src_dir() -> PathBuf {
    repo_root().join("src")
}

fn fixtures_dir() -> PathBuf {
    repo_root()
        .join("tests")
        .join("fixtures")
        .join("failure_modes")
}

fn collect_rust_files(root: &Path, files: &mut Vec<PathBuf>) -> TestResult {
    if root.is_file() {
        if root.extension().is_some_and(|extension| extension == "rs") {
            files.push(root.to_path_buf());
        }
        return Ok(());
    }
    for entry in fs::read_dir(root).map_err(|error| format!("read {}: {error}", root.display()))? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

fn fixture_codes() -> Result<BTreeSet<String>, String> {
    let mut codes = BTreeSet::new();
    for entry in fs::read_dir(fixtures_dir()).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let code = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| format!("invalid fixture filename: {}", path.display()))?;
            codes.insert(code.to_owned());
        }
    }
    Ok(codes)
}

fn is_code_candidate(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && value.contains('_')
        && value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

fn quoted_strings(line: &str) -> Vec<String> {
    let mut strings = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('"') {
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('"') else {
            break;
        };
        strings.push(after_start[..end].to_owned());
        rest = &after_start[end + 1..];
    }
    strings
}

fn source_roots() -> Vec<PathBuf> {
    let src = src_dir();
    vec![
        src.join("core"),
        src.join("models"),
        src.join("cli"),
        src.join("config"),
        src.join("graph"),
        src.join("output"),
        src.join("pack"),
        src.join("steward"),
        src.join("serve.rs"),
        src.join("curate").join("cluster_coherence.rs"),
    ]
}

fn line_defines_code_literal(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with("//")
        || trimmed.contains("assert")
        || trimmed.contains("unknown_code")
        || trimmed.contains("exceeds_code")
        || trimmed.contains("reason_code")
    {
        return false;
    }
    trimmed.contains("code:")
        || trimmed.contains("\"code\":")
        || trimmed.contains("degradation_code:")
}

fn excluded_code_candidate(value: &str) -> bool {
    matches!(
        value,
        // Error-envelope/category codes are not degraded[] entries.
        "config"
            | "degraded"
            | "import"
            | "migration"
            | "policy"
            | "schema_not_found"
            | "search_index"
            | "serialization_failed"
            | "storage"
            | "toon_encoding_failed"
            | "unsatisfied_degraded_mode"
            | "usage"
    )
}

fn emitted_degraded_codes() -> TestResult<BTreeSet<String>> {
    let mut files = Vec::new();
    for root in source_roots() {
        collect_rust_files(&root, &mut files)?;
    }
    let mut codes = BTreeSet::new();
    for file in files {
        let content = fs::read_to_string(&file)
            .map_err(|error| format!("read {}: {error}", file.display()))?;
        for line in content.lines() {
            if !line_defines_code_literal(line) {
                continue;
            }
            for candidate in quoted_strings(line) {
                if candidate != "code"
                    && !candidate.starts_with("test_")
                    && !excluded_code_candidate(&candidate)
                    && is_code_candidate(&candidate)
                {
                    codes.insert(candidate);
                }
            }
        }
    }
    Ok(codes)
}

#[test]
#[ignore = "J6 fixture backfill is intentionally incremental; run manually to see missing codes"]
fn every_degraded_code_emitted_in_source_has_a_fixture() -> TestResult {
    let emitted = emitted_degraded_codes()?;
    let fixtures = fixture_codes()?;
    let missing = emitted
        .difference(&fixtures)
        .cloned()
        .collect::<Vec<String>>();

    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} degraded/fallback code(s) still need failure-mode fixtures: {}",
            missing.len(),
            missing.join(", ")
        ))
    }
}
