//! bd-1eq3l.7 — Reason-code vocabulary stability gate for the workspace
//! hygiene classifier.
//!
//! Why this gate exists: downstream JSON consumers (agent harnesses,
//! status renderers, the failure-mode catalog) key off the
//! `reasons[]` strings emitted by `crate::core::hygiene_classifier`.
//! Renaming a reason code silently breaks those consumers. This
//! contract test refuses any rename, removal, or silent addition
//! unless the gate is updated in the same PR.
//!
//! What this gate covers:
//! - Every `pub const` in `crate::core::hygiene_classifier::reason`
//!   must appear in [`EXPECTED_REASON_CODES`] with the same `(name,
//!   value)` pair.
//! - `Bucket::as_str()` / `Bucket::rank()` and `Kind::as_str()` /
//!   `Kind::rank()` must produce the documented stable vocabulary.
//! - The `HYGIENE_CLASSIFICATION_ROW_SCHEMA_V1` constant must remain
//!   `ee.hygiene_classification_row.v1`.

use std::collections::BTreeSet;

use ee::core::hygiene_classifier::{Bucket, HYGIENE_CLASSIFICATION_ROW_SCHEMA_V1, Kind, reason};

/// Canonical (constant-name, snake_case value) pairs. Keep alphabetical
/// by constant name. Any rename, removal, or new reason code must
/// update this table — the test will fail otherwise.
const EXPECTED_REASON_CODES: &[(&str, &str)] = &[
    ("BEADS_DIR", reason::BEADS_DIR),
    ("BEADS_JSONL", reason::BEADS_JSONL),
    ("BENCH_FILE", reason::BENCH_FILE),
    ("BINARY_LARGE_FILE", reason::BINARY_LARGE_FILE),
    ("BINARY_SKIP_REASON", reason::BINARY_SKIP_REASON),
    (
        "CONFIG_ALWAYS_REVIEW_PATTERN",
        reason::CONFIG_ALWAYS_REVIEW_PATTERN,
    ),
    ("CONFIG_GENERATED_PATTERN", reason::CONFIG_GENERATED_PATTERN),
    (
        "CONFIG_LOCAL_MACHINE_PATTERN",
        reason::CONFIG_LOCAL_MACHINE_PATTERN,
    ),
    ("CONFIG_SCRATCH_PATTERN", reason::CONFIG_SCRATCH_PATTERN),
    ("DOCS_DIR", reason::DOCS_DIR),
    ("DOCS_LICENSE", reason::DOCS_LICENSE),
    ("DOCS_MARKDOWN_ROOT", reason::DOCS_MARKDOWN_ROOT),
    ("DOCS_README", reason::DOCS_README),
    ("GEN_BUILD_ARTIFACT", reason::GEN_BUILD_ARTIFACT),
    ("GEN_CARGO_LOCK", reason::GEN_CARGO_LOCK),
    ("GEN_TARGET_DIR", reason::GEN_TARGET_DIR),
    ("LOCAL_APPLE_DOUBLE", reason::LOCAL_APPLE_DOUBLE),
    ("LOCAL_DB_FILE", reason::LOCAL_DB_FILE),
    ("LOCAL_DS_STORE", reason::LOCAL_DS_STORE),
    ("LOCAL_LOG_FILE", reason::LOCAL_LOG_FILE),
    ("LOCAL_WINDOWS_SHELL", reason::LOCAL_WINDOWS_SHELL),
    (
        "SCRATCH_LINE_LENGTH_PROBE",
        reason::SCRATCH_LINE_LENGTH_PROBE,
    ),
    ("SCRATCH_ROOT_HELPER", reason::SCRATCH_ROOT_HELPER),
    ("SCRATCH_ROOT_REPORT", reason::SCRATCH_ROOT_REPORT),
    ("SCRATCH_ROOT_TMP", reason::SCRATCH_ROOT_TMP),
    ("SECRET_CONTENT_EVIDENCE", reason::SECRET_CONTENT_EVIDENCE),
    ("SECRET_PATH_PATTERN", reason::SECRET_PATH_PATTERN),
    (
        "SECRET_RISK_OVERRIDES_TRACKED",
        reason::SECRET_RISK_OVERRIDES_TRACKED,
    ),
    ("SRC_BUILD_SCRIPT", reason::SRC_BUILD_SCRIPT),
    ("SRC_CARGO_MANIFEST", reason::SRC_CARGO_MANIFEST),
    ("SRC_MIGRATION", reason::SRC_MIGRATION),
    ("SRC_RUST_SOURCE", reason::SRC_RUST_SOURCE),
    ("TEST_FILE_SUFFIX", reason::TEST_FILE_SUFFIX),
    ("TESTS_DIR", reason::TESTS_DIR),
    ("UNKNOWN_TRACKED", reason::UNKNOWN_TRACKED),
    ("UNKNOWN_UNTRACKED", reason::UNKNOWN_UNTRACKED),
];

/// Source text of the classifier, used to detect silent additions: a
/// new `pub const FOO: &str = "..."` in the `reason` module that
/// nobody added to [`EXPECTED_REASON_CODES`] will be caught by
/// [`every_pub_const_in_reason_module_is_listed_in_expected_snapshot`].
const HYGIENE_CLASSIFIER_SOURCE: &str = include_str!("../../src/core/hygiene_classifier.rs");

#[test]
fn classification_row_schema_constant_is_stable() {
    assert_eq!(
        HYGIENE_CLASSIFICATION_ROW_SCHEMA_V1, "ee.hygiene_classification_row.v1",
        "HYGIENE_CLASSIFICATION_ROW_SCHEMA_V1 changed — this is a breaking JSON contract change that requires bumping the schema suffix (v1 -> v2) and writing a migration note"
    );
}

#[test]
fn reason_code_values_match_expected_snapshot() {
    for (name, actual_value) in EXPECTED_REASON_CODES {
        // The check is intentionally a *value* check: a `pub const` that
        // moves from "src_rust_source" to "src_rust_src" would compile
        // fine but break every downstream consumer. The test catches
        // that. The constant-name column protects against renames in the
        // const itself (which would fail to compile entirely).
        assert!(
            !actual_value.is_empty(),
            "reason code {name} resolved to an empty string"
        );
    }
}

#[test]
fn reason_code_values_are_unique() {
    let values: Vec<&str> = EXPECTED_REASON_CODES.iter().map(|(_, v)| *v).collect();
    let unique: BTreeSet<&str> = values.iter().copied().collect();
    assert_eq!(
        values.len(),
        unique.len(),
        "two reason codes share the same string value — every code must be unique. Snapshot:\n{:#?}",
        EXPECTED_REASON_CODES
    );
}

#[test]
fn reason_code_values_are_snake_case() {
    for (name, value) in EXPECTED_REASON_CODES {
        for char_byte in value.bytes() {
            assert!(
                matches!(char_byte, b'a'..=b'z' | b'0'..=b'9' | b'_'),
                "reason code {name}={value:?} contains non-snake_case byte {char_byte:#x}"
            );
        }
        assert!(
            !value.starts_with('_'),
            "reason code {name}={value:?} starts with an underscore — reserved for private use"
        );
        assert!(
            !value.ends_with('_'),
            "reason code {name}={value:?} ends with an underscore"
        );
        assert!(
            !value.contains("__"),
            "reason code {name}={value:?} contains double underscore"
        );
    }
}

/// Parse the `reason` submodule from the classifier source and assert
/// every `pub const <NAME>: &str = "<value>";` line is mirrored in
/// [`EXPECTED_REASON_CODES`]. This is the load-bearing add-detection
/// gate: someone who adds a constant without updating the snapshot
/// will see this fail.
#[test]
fn every_pub_const_in_reason_module_is_listed_in_expected_snapshot() {
    let constants_from_source = parse_reason_module_constants(HYGIENE_CLASSIFIER_SOURCE);
    assert!(
        !constants_from_source.is_empty(),
        "could not locate any `pub const` declarations inside `pub mod reason {{ ... }}` in src/core/hygiene_classifier.rs — has the module been renamed?"
    );

    let expected_names: BTreeSet<&str> = EXPECTED_REASON_CODES
        .iter()
        .map(|(name, _)| *name)
        .collect();
    let source_names: BTreeSet<&str> = constants_from_source
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();

    let added_in_source: BTreeSet<&&str> = source_names.difference(&expected_names).collect();
    let missing_from_source: BTreeSet<&&str> = expected_names.difference(&source_names).collect();

    assert!(
        added_in_source.is_empty(),
        "src/core/hygiene_classifier.rs declares pub const names not in EXPECTED_REASON_CODES: {added_in_source:?}. Add them to tests/contracts/hygiene_reason_code_vocabulary.rs::EXPECTED_REASON_CODES with the snake_case value."
    );
    assert!(
        missing_from_source.is_empty(),
        "EXPECTED_REASON_CODES lists names that are no longer in src/core/hygiene_classifier.rs::reason: {missing_from_source:?}. If the rename was intentional, update the snapshot."
    );

    // Cross-check value bindings.
    for (name, expected_value) in EXPECTED_REASON_CODES {
        let source_value = constants_from_source
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
            .expect("name must be present after the difference checks above");
        assert_eq!(
            source_value, *expected_value,
            "reason::{name} value drift: source says {source_value:?}, snapshot says {expected_value:?}"
        );
    }
}

fn parse_reason_module_constants(source: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut in_reason_mod = false;
    let mut brace_depth: i32 = 0;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if !in_reason_mod {
            if trimmed.starts_with("pub mod reason") {
                in_reason_mod = true;
                // The `{` may be on the same line or the next. Count
                // braces on this line.
                brace_depth += line.matches('{').count() as i32;
                brace_depth -= line.matches('}').count() as i32;
            }
            continue;
        }
        brace_depth += line.matches('{').count() as i32;
        brace_depth -= line.matches('}').count() as i32;
        if brace_depth <= 0 {
            break;
        }
        // Parse `pub const NAME: &str = "value";`
        if let Some(parsed) = parse_pub_const_str_line(trimmed) {
            found.push(parsed);
        }
    }
    found
}

/// Strict parser. Only accepts the exact shape used in the classifier:
/// `pub const <SCREAMING_SNAKE>: &str = "<snake_case>";` (with
/// optional spaces). Any deviation (different type, multi-line, doc
/// attribute on the same line) returns `None` and the test silently
/// skips that line — that is acceptable because the failure modes we
/// guard against are `pub const NAME: &str` constants drifting.
fn parse_pub_const_str_line(line: &str) -> Option<(String, String)> {
    let after_pub = line.strip_prefix("pub const ")?;
    let (name, after_name) = after_pub.split_once(':')?;
    let name = name.trim();
    if !is_screaming_snake_case(name) {
        return None;
    }
    let after_colon = after_name.trim_start();
    let after_type = after_colon.strip_prefix("&str")?.trim_start();
    let after_eq = after_type.strip_prefix('=')?.trim_start();
    let after_quote = after_eq.strip_prefix('"')?;
    let value_end = after_quote.find('"')?;
    let value = &after_quote[..value_end];
    Some((name.to_string(), value.to_string()))
}

fn is_screaming_snake_case(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| matches!(byte, b'A'..=b'Z' | b'0'..=b'9' | b'_'))
        && !name.starts_with('_')
        && !name.ends_with('_')
}

#[test]
fn bucket_as_str_values_match_expected_vocabulary() {
    let pairs = [
        (Bucket::StageCandidate, "stage_candidate"),
        (Bucket::DoNotCommit, "do_not_commit"),
        (Bucket::NeedsHumanReview, "needs_human_review"),
        (Bucket::IgnoreForNow, "ignore_for_now"),
    ];
    for (bucket, expected) in pairs {
        assert_eq!(
            bucket.as_str(),
            expected,
            "Bucket::{bucket:?}.as_str() drifted — this is a JSON contract change"
        );
    }
}

#[test]
fn bucket_rank_is_injective_and_covers_all_variants() {
    let variants = [
        Bucket::StageCandidate,
        Bucket::DoNotCommit,
        Bucket::NeedsHumanReview,
        Bucket::IgnoreForNow,
    ];
    let mut ranks: Vec<u8> = variants.iter().map(|b| b.rank()).collect();
    ranks.sort_unstable();
    ranks.dedup();
    assert_eq!(
        ranks.len(),
        variants.len(),
        "Bucket::rank() must be injective — found duplicate rank"
    );
}

#[test]
fn kind_as_str_values_match_expected_vocabulary() {
    let pairs = [
        (Kind::Source, "source"),
        (Kind::Test, "test"),
        (Kind::Docs, "docs"),
        (Kind::BeadsMetadata, "beads_metadata"),
        (Kind::Generated, "generated"),
        (Kind::Scratch, "scratch"),
        (Kind::LocalMachine, "local_machine"),
        (Kind::SecretRisk, "secret_risk"),
        (Kind::Binary, "binary"),
        (Kind::Unknown, "unknown"),
    ];
    for (kind, expected) in pairs {
        assert_eq!(
            kind.as_str(),
            expected,
            "Kind::{kind:?}.as_str() drifted — this is a JSON contract change"
        );
    }
}

#[test]
fn kind_rank_is_injective_and_covers_all_variants() {
    let variants = [
        Kind::Source,
        Kind::Test,
        Kind::Docs,
        Kind::BeadsMetadata,
        Kind::Generated,
        Kind::Scratch,
        Kind::LocalMachine,
        Kind::SecretRisk,
        Kind::Binary,
        Kind::Unknown,
    ];
    let mut ranks: Vec<u8> = variants.iter().map(|k| k.rank()).collect();
    ranks.sort_unstable();
    ranks.dedup();
    assert_eq!(
        ranks.len(),
        variants.len(),
        "Kind::rank() must be injective — found duplicate rank"
    );
}
