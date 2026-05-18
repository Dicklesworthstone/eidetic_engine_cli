//! Deterministic audit for the public `degraded[]` aggregation
//! emitter inventory (bd-2kj2x.1).
//!
//! Background. The `aggregate_degraded` / `aggregate_degraded_entries`
//! helper in `src/core/degraded_aggregation.rs` is wired into many
//! response renderers by passing a stable `source` label (e.g.
//! `"search"`, `"pack_dna"`, `"hits"`) so a duplicated code emitted by
//! several algorithms collapses into one aggregate with `sources[]`
//! populated. The bd-bife.27 / bd-2kj2x parent acceptance requires that
//! every renderer route through the helper; bd-2kj2x.1 in turn requires
//! that the set of source labels stays in sync with the taxonomy
//! documentation so an agent can read either side of the contract and
//! get a complete inventory.
//!
//! This contract test enforces two static invariants:
//!
//! 1. Every literal source label passed to
//!    `DegradationAggregationInput::new(...)` in `src/` is listed in
//!    the "Aggregation source labels" table of
//!    `docs/degraded_code_taxonomy.md`.
//! 2. Every literal label is snake_case (lowercase ASCII letters,
//!    digits, and underscores). Workspace paths, query text, and
//!    memory bodies are forbidden as source labels.
//!
//! The reverse direction (doc rows without literal call sites) is
//! intentionally not asserted here: many call sites pass the source
//! label as a `&str` parameter (e.g. `src/core/search.rs`,
//! `src/core/why.rs`, several `src/cli/mod.rs` helpers). Those
//! variable-passed labels are pinned by per-renderer unit tests
//! landed alongside each parent bd-2kj2x slice that wired them; see
//! `docs/degraded_aggregation_emitter_inventory.md` for the full
//! agent-facing inventory and exempt list.
//!
//! Some emitters intentionally bypass the aggregator because their
//! degraded[] entries carry richer evidence fields (cycle members,
//! next-action remediation, lock paths) that AggregatedDegradation
//! would erase. Those exemptions are documented in
//! `docs/degraded_aggregation_emitter_inventory.md`; this test does
//! not enforce them because the helper is not on the code path.
//!
//! Runtime cost: scans `src/` once and parses one markdown file. No
//! Cargo build, no DB, no fixture corpus.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn src_dir() -> PathBuf {
    project_root().join("src")
}

fn taxonomy_path() -> PathBuf {
    project_root()
        .join("docs")
        .join("degraded_code_taxonomy.md")
}

fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !root.exists() {
        return out;
    }
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Skip target dirs and dotfiles that get pulled in by
                // out-of-tree CARGO_TARGET_DIR symlinks on dev hosts.
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                if name == "target" || name.starts_with('.') {
                    continue;
                }
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Extract every distinct literal first-arg passed to
/// `DegradationAggregationInput::new("...", ...)` across `src/`.
///
/// We deliberately match the source-label arg as the FIRST string
/// literal on the line immediately following
/// `DegradationAggregationInput::new(`, OR on the same line. Variable
/// arguments (e.g. `DegradationAggregationInput::new(source, ...)`)
/// are ignored because they cannot be statically resolved here; their
/// concrete values are still asserted at runtime by per-renderer unit
/// tests that landed alongside the parent bd-2kj2x slices.
fn collect_literal_source_labels(files: &[PathBuf]) -> BTreeSet<String> {
    let mut labels: BTreeSet<String> = BTreeSet::new();
    for path in files {
        let Ok(contents) = fs::read_to_string(path) else {
            continue;
        };
        let lines: Vec<&str> = contents.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            // Skip line/doc comments so a documentation example like
            // `/// DegradationAggregationInput::new("foo", ...)` cannot
            // falsely contribute a label.
            let trimmed_start = line.trim_start();
            if trimmed_start.starts_with("//") {
                continue;
            }
            // Same-line form: `DegradationAggregationInput::new("foo",`.
            if let Some(start) = line.find("DegradationAggregationInput::new(") {
                let tail = &line[start + "DegradationAggregationInput::new(".len()..];
                if let Some(label) = first_string_literal(tail) {
                    labels.insert(label);
                    continue;
                }
            }
            // Multi-line form: `DegradationAggregationInput::new(` on
            // one line, label literal on the next line. Skip if the
            // next line is itself a comment.
            if line
                .trim_end()
                .ends_with("DegradationAggregationInput::new(")
            {
                if let Some(next) = lines.get(idx + 1) {
                    let next_trimmed = next.trim_start();
                    if next_trimmed.starts_with("//") {
                        continue;
                    }
                    if let Some(label) = first_string_literal(next_trimmed) {
                        labels.insert(label);
                    }
                }
            }
        }
    }
    labels
}

/// Return the contents of the first double-quoted string literal in
/// `tail`, if any, with simple escape handling for `\"`.
fn first_string_literal(tail: &str) -> Option<String> {
    let bytes = tail.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'"' {
            let start = idx + 1;
            let mut cursor = start;
            while cursor < bytes.len() {
                if bytes[cursor] == b'\\' && cursor + 1 < bytes.len() {
                    cursor += 2;
                    continue;
                }
                if bytes[cursor] == b'"' {
                    return Some(tail[start..cursor].to_owned());
                }
                cursor += 1;
            }
            return None;
        }
        idx += 1;
    }
    None
}

/// Parse the "## Aggregation source labels" markdown table in
/// `docs/degraded_code_taxonomy.md` into a sorted set of labels.
///
/// Format expected (per current taxonomy doc):
///
/// ```markdown
/// ## Aggregation source labels
///
/// | Label | Use |
/// |-------|-----|
/// | `insights` | Whole-bundle `ee insights` degraded signals … |
/// | `hubs`     | `ee insights --section hubs` HITS profile … |
/// ```
fn parse_taxonomy_labels(doc: &str) -> BTreeSet<String> {
    let mut labels: BTreeSet<String> = BTreeSet::new();
    let mut in_section = false;
    for line in doc.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            in_section = trimmed == "## Aggregation source labels";
            continue;
        }
        if !in_section {
            continue;
        }
        if !trimmed.starts_with('|') {
            continue;
        }
        // Skip header and separator rows.
        if trimmed.starts_with("| Label") || trimmed.starts_with("|-") {
            continue;
        }
        // First column should be `` `label` ``.
        let columns: Vec<&str> = trimmed.split('|').collect();
        if columns.len() < 3 {
            continue;
        }
        let first = columns[1].trim();
        if !first.starts_with('`') || !first.ends_with('`') || first.len() < 3 {
            continue;
        }
        let label = first[1..first.len() - 1].to_owned();
        if label.is_empty() {
            continue;
        }
        labels.insert(label);
    }
    labels
}

#[test]
fn aggregation_source_labels_are_snake_case_ascii() {
    let files = rust_files(&src_dir());
    let labels = collect_literal_source_labels(&files);
    assert!(
        !labels.is_empty(),
        "no literal DegradationAggregationInput::new(\"...\") sites \
         found under src/ — either the helper was removed (update this \
         contract) or the audit walker is broken"
    );
    let mut bad: Vec<&String> = labels
        .iter()
        .filter(|label| !is_valid_source_label(label))
        .collect();
    bad.sort();
    assert!(
        bad.is_empty(),
        "non-snake_case literal source labels found: {bad:?}. \
         Source labels must match `[a-z][a-z0-9_]*` so they stay safe \
         to render into sources[] without leaking paths or prose."
    );
}

#[test]
fn every_literal_source_label_is_documented_in_taxonomy() {
    let files = rust_files(&src_dir());
    let literal_labels = collect_literal_source_labels(&files);
    let doc = fs::read_to_string(taxonomy_path())
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", taxonomy_path().display()));
    let doc_labels = parse_taxonomy_labels(&doc);

    assert!(
        !doc_labels.is_empty(),
        "docs/degraded_code_taxonomy.md has no '## Aggregation source labels' \
         table rows — either the section was renamed (update this contract) \
         or the table was emptied (re-document the labels)"
    );

    // Direction enforced: every literal label found in src/ must
    // have an agent-facing row in the taxonomy doc. The reverse
    // direction (doc rows without literal call sites) is intentionally
    // NOT asserted here because many call sites pass the source label
    // as a `&str` parameter the static audit cannot resolve; those
    // are pinned by per-renderer unit tests under src/output, src/cli,
    // src/core/why, src/core/search, etc.
    let mut missing_in_doc: Vec<&String> = literal_labels.difference(&doc_labels).collect();
    missing_in_doc.sort();
    assert!(
        missing_in_doc.is_empty(),
        "source labels emitted as literals from src/ but not listed in \
         docs/degraded_code_taxonomy.md '## Aggregation source labels': {missing_in_doc:?}. \
         Add a row to the table for each new label in the same commit."
    );
}

fn is_valid_source_label(label: &str) -> bool {
    if label.is_empty() {
        return false;
    }
    let mut chars = label.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() {
        return false;
    }
    label
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_string_literal_handles_escapes() {
        assert_eq!(first_string_literal("\"abc\","), Some("abc".to_owned()));
        assert_eq!(
            first_string_literal("\"with\\\"quote\","),
            Some("with\\\"quote".to_owned())
        );
        assert_eq!(first_string_literal("no quotes here"), None);
        assert_eq!(first_string_literal(""), None);
    }

    #[test]
    fn is_valid_source_label_rejects_uppercase_and_paths() {
        assert!(is_valid_source_label("search"));
        assert!(is_valid_source_label("graph_centrality_read"));
        assert!(is_valid_source_label("pack_dna"));
        assert!(!is_valid_source_label(""));
        assert!(!is_valid_source_label("Search"));
        assert!(!is_valid_source_label("graph/path"));
        assert!(!is_valid_source_label("_leading"));
        assert!(!is_valid_source_label("with space"));
    }

    #[test]
    fn parse_taxonomy_labels_finds_table_rows_and_skips_other_tables() {
        let doc = "## Aggregation source labels\n\
                   \n\
                   | Label | Use |\n\
                   |-------|-----|\n\
                   | `search` | search responses |\n\
                   | `pack_dna` | pack dna graph explanation |\n\
                   \n\
                   ## Some other section\n\
                   \n\
                   | Label | Use |\n\
                   |-------|-----|\n\
                   | `not_an_aggregation_label` | ignore me |\n";
        let labels = parse_taxonomy_labels(doc);
        assert!(labels.contains("search"));
        assert!(labels.contains("pack_dna"));
        assert!(!labels.contains("not_an_aggregation_label"));
    }

    /// Synthetic-source-tree round trip: the walker must catch the
    /// same-line form, the multi-line form, ignore variable-passed
    /// arguments (no literal to extract), and skip doc-comment
    /// examples. Build the fixture as a real temp directory so the
    /// walker exercises its actual readdir/filter path.
    #[test]
    fn walker_finds_all_literal_forms_and_skips_variables_and_comments() {
        let temp =
            std::env::temp_dir().join(format!("ee_degraded_audit_fixture_{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();

        let same_line = "fn a() {\n\
                         DegradationAggregationInput::new(\"label_same_line\", x, y, z, w);\n\
                         }\n";
        let multi_line = "fn b() {\n\
                          DegradationAggregationInput::new(\n\
                              \"label_multi_line\",\n\
                              code,\n\
                          );\n\
                          }\n";
        let variable_arg = "fn c(source: &str) {\n\
                            DegradationAggregationInput::new(\n\
                                source,\n\
                                code,\n\
                            );\n\
                            }\n";
        let doc_example = "/// DegradationAggregationInput::new(\"doc_only_label\", x);\n\
                           // DegradationAggregationInput::new(\"line_comment_label\", x);\n\
                           fn d() {}\n";

        let nested = temp.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(temp.join("same.rs"), same_line).unwrap();
        fs::write(nested.join("multi.rs"), multi_line).unwrap();
        fs::write(temp.join("variable.rs"), variable_arg).unwrap();
        fs::write(temp.join("docs.rs"), doc_example).unwrap();
        fs::write(temp.join("ignored.txt"), same_line).unwrap();

        let files = rust_files(&temp);
        let labels = collect_literal_source_labels(&files);

        assert!(
            labels.contains("label_same_line"),
            "expected same-line form to register, got: {labels:?}"
        );
        assert!(
            labels.contains("label_multi_line"),
            "expected multi-line form to register, got: {labels:?}"
        );
        assert!(
            !labels.contains("doc_only_label"),
            "doc comment example must not register, got: {labels:?}"
        );
        assert!(
            !labels.contains("line_comment_label"),
            "line comment example must not register, got: {labels:?}"
        );
        // Variable-passed args produce no literal — there's nothing to
        // assert for them; success is the absence of a spurious entry.
        assert!(
            !labels.iter().any(|label| label == "source"),
            "variable name must not be captured as a label"
        );

        let _ = fs::remove_dir_all(&temp);
    }
}
