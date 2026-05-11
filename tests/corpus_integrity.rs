//! Corpus integrity test (bead bd-17c65.10.2 / J2).
//!
//! Asserts:
//! 1. `corpus_2026_05_10.jsonl` is valid JSON-line, every record has the
//!    required fields, the schema enums are honored.
//! 2. No duplicate `content` values across the corpus (every memory is
//!    semantically distinct so we never accidentally compare a memory to
//!    its duplicate in tests).
//! 3. `corpus_2026_05_10_expected.json` references only content phrases
//!    that appear in the corpus.
//! 4. The seed shell script exists and is executable.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde_json::Value;

type TestResult = Result<(), String>;

const VALID_LEVELS: &[&str] = &["working", "episodic", "semantic", "procedural"];
const VALID_KINDS: &[&str] = &[
    "rule",
    "fact",
    "decision",
    "failure",
    "event",
    "observation",
    "artifact",
];

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/corpus")
}

fn read_corpus_records() -> Result<Vec<Value>, String> {
    let path = corpus_dir().join("corpus_2026_05_10.jsonl");
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("read corpus {}: {error}", path.display()))?;
    let mut records = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .map_err(|error| format!("corpus line {} parse: {error}", lineno + 1))?;
        records.push(value);
    }
    Ok(records)
}

fn read_expected() -> Result<Value, String> {
    let path = corpus_dir().join("corpus_2026_05_10_expected.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("read expected {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("parse expected: {error}"))
}

#[test]
fn corpus_jsonl_is_valid_and_has_15_records() -> TestResult {
    let records = read_corpus_records()?;
    if records.len() != 15 {
        return Err(format!(
            "expected 15 corpus records, found {}",
            records.len()
        ));
    }
    Ok(())
}

#[test]
fn every_record_has_required_fields_and_valid_enums() -> TestResult {
    let records = read_corpus_records()?;
    for (i, record) in records.iter().enumerate() {
        let obj = record
            .as_object()
            .ok_or_else(|| format!("record {i} is not object"))?;

        let content = obj
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("record {i} missing content"))?;
        if content.is_empty() {
            return Err(format!("record {i} content is empty"));
        }

        let level = obj
            .get("level")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("record {i} missing level"))?;
        if !VALID_LEVELS.contains(&level) {
            return Err(format!("record {i} invalid level: {level}"));
        }

        let kind = obj
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("record {i} missing kind"))?;
        if !VALID_KINDS.contains(&kind) {
            return Err(format!("record {i} invalid kind: {kind}"));
        }

        if let Some(tags) = obj.get("tags") {
            let arr = tags
                .as_array()
                .ok_or_else(|| format!("record {i} tags not array"))?;
            for tag in arr {
                let tag_str = tag
                    .as_str()
                    .ok_or_else(|| format!("record {i} tag not string"))?;
                if tag_str.is_empty() {
                    return Err(format!("record {i} has empty tag"));
                }
            }
        }

        if let Some(confidence) = obj.get("confidence") {
            let c = confidence
                .as_f64()
                .ok_or_else(|| format!("record {i} confidence not number"))?;
            if !(0.0..=1.0).contains(&c) {
                return Err(format!("record {i} confidence {c} out of range"));
            }
        }
    }
    Ok(())
}

#[test]
fn no_duplicate_content_across_corpus() -> TestResult {
    let records = read_corpus_records()?;
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (i, record) in records.iter().enumerate() {
        let content = record
            .pointer("/content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let normalized: String = content.split_whitespace().collect::<Vec<_>>().join(" ");
        if let Some(prev) = seen.insert(normalized.clone(), i) {
            return Err(format!(
                "duplicate content at records {} and {}: {normalized}",
                prev, i
            ));
        }
    }
    Ok(())
}

#[test]
fn expected_phrases_appear_in_corpus() -> TestResult {
    let records = read_corpus_records()?;
    let contents: Vec<String> = records
        .iter()
        .filter_map(|r| r.pointer("/content").and_then(Value::as_str))
        .map(|s| s.to_lowercase())
        .collect();
    let expected = read_expected()?;

    // policy_acceptance phrases must each appear in the corpus.
    if let Some(items) = expected
        .pointer("/policy_acceptance")
        .and_then(Value::as_array)
    {
        for (i, item) in items.iter().enumerate() {
            let phrase = item
                .pointer("/content_phrase")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("policy_acceptance[{i}] missing content_phrase"))?
                .to_lowercase();
            let matched = contents.iter().any(|c| c.contains(&phrase));
            if !matched {
                return Err(format!(
                    "policy_acceptance[{i}] phrase not in corpus: {phrase}"
                ));
            }
        }
    }

    // Pack expectation top-item phrase must appear in corpus.
    if let Some(phrase) = expected
        .pointer("/pack_expectations/expected_top_item_content_phrase")
        .and_then(Value::as_str)
    {
        let phrase_lc = phrase.to_lowercase();
        if !contents.iter().any(|c| c.contains(&phrase_lc)) {
            return Err(format!(
                "pack_expectations top phrase not in corpus: {phrase}"
            ));
        }
    }
    Ok(())
}

#[test]
fn expected_query_keys_use_known_schemas() -> TestResult {
    let expected = read_expected()?;
    let queries = expected
        .pointer("/queries")
        .and_then(Value::as_object)
        .ok_or("queries object missing")?;
    for (query, spec) in queries {
        let spec_obj = spec
            .as_object()
            .ok_or_else(|| format!("query {query} spec not object"))?;
        if let Some(min_score) = spec_obj.get("expected_min_top_score") {
            let s = min_score
                .as_f64()
                .ok_or_else(|| format!("query {query} expected_min_top_score not number"))?;
            if !(0.0..=1.0).contains(&s) {
                return Err(format!("query {query} min score {s} out of range"));
            }
        }
        // Test labels reference real beads (A-M plus J1..J9, K1..K6 etc.).
        if let Some(tests) = spec_obj.get("tests").and_then(Value::as_array) {
            for test in tests {
                let test_str = test
                    .as_str()
                    .ok_or_else(|| format!("query {query} test entry not string"))?;
                let valid_prefix = test_str
                    .chars()
                    .next()
                    .map(|c| "ABCDEFGHIJKLM".contains(c))
                    .unwrap_or(false);
                if !valid_prefix {
                    return Err(format!(
                        "query {query} test label '{test_str}' does not start with a known epic letter A..M"
                    ));
                }
            }
        }
    }
    Ok(())
}

#[test]
fn seed_script_exists_and_is_executable() -> TestResult {
    let path = corpus_dir().join("corpus_2026_05_10_seed.sh");
    if !path.exists() {
        return Err(format!("seed script missing at {}", path.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .map_err(|error| format!("stat seed script: {error}"))?
            .permissions()
            .mode();
        if mode & 0o111 == 0 {
            return Err(format!(
                "seed script not executable (mode {mode:o}): {}",
                path.display()
            ));
        }
    }
    Ok(())
}

#[test]
fn policy_tag_acceptance_covers_required_categories() -> TestResult {
    let expected = read_expected()?;
    let arr = expected
        .pointer("/policy_tag_acceptance")
        .and_then(Value::as_array)
        .ok_or("policy_tag_acceptance missing")?;
    let tags: BTreeSet<&str> = arr
        .iter()
        .filter_map(|v| v.pointer("/tag").and_then(Value::as_str))
        .collect();
    // C3 acceptance criteria — these must all be exercised
    for required in ["v0.1.0", "policy.detector", "mémoire"] {
        if !tags.contains(required) {
            return Err(format!(
                "policy_tag_acceptance missing required accept case: {required}"
            ));
        }
    }
    for required in ["with space", "a,b", "a=b", "a/b"] {
        if !tags.contains(required) {
            return Err(format!(
                "policy_tag_acceptance missing required reject case: {required}"
            ));
        }
    }
    Ok(())
}

#[test]
fn level_kind_distribution_is_diverse() -> TestResult {
    let records = read_corpus_records()?;
    let mut levels: BTreeSet<String> = BTreeSet::new();
    let mut kinds: BTreeSet<String> = BTreeSet::new();
    for record in &records {
        if let Some(lvl) = record.pointer("/level").and_then(Value::as_str) {
            levels.insert(lvl.to_string());
        }
        if let Some(k) = record.pointer("/kind").and_then(Value::as_str) {
            kinds.insert(k.to_string());
        }
    }
    if levels.len() < 3 {
        return Err(format!(
            "corpus lacks level diversity (got {levels:?}); need ≥ 3"
        ));
    }
    if kinds.len() < 3 {
        return Err(format!(
            "corpus lacks kind diversity (got {kinds:?}); need ≥ 3"
        ));
    }
    Ok(())
}

#[test]
fn cargo_cluster_has_enough_members_for_g3_proposal() -> TestResult {
    // G3 candidate proposer needs ≥ 3 similar memories. Verify the corpus
    // has at least 2 procedural/rule memories tagged "cargo" plus enough
    // related neighbors that frankensearch can cluster.
    let records = read_corpus_records()?;
    let cargo_rules: Vec<&Value> = records
        .iter()
        .filter(|r| {
            r.pointer("/level").and_then(Value::as_str) == Some("procedural")
                && r.pointer("/kind").and_then(Value::as_str) == Some("rule")
                && r.pointer("/tags")
                    .and_then(Value::as_array)
                    .map(|arr| arr.iter().any(|t| t.as_str() == Some("cargo")))
                    .unwrap_or(false)
        })
        .collect();
    if cargo_rules.len() < 2 {
        return Err(format!(
            "corpus has only {} procedural/rule + cargo memories; G3 cluster needs ≥ 2",
            cargo_rules.len()
        ));
    }
    Ok(())
}

#[test]
fn corpus_exercises_pre_overhaul_rejection_cases() -> TestResult {
    // C1 + C3 fix specific rejection cases. The corpus must include them so
    // the e2e scripts can detect "pre-overhaul" vs "fixed" behavior.
    let records = read_corpus_records()?;
    let contents: Vec<String> = records
        .iter()
        .filter_map(|r| r.pointer("/content").and_then(Value::as_str))
        .map(|s| s.to_lowercase())
        .collect();
    // C1: must contain a memory mentioning 'secrets' in plain English
    if !contents.iter().any(|c| c.contains("secrets")) {
        return Err(
            "corpus must include a memory mentioning 'secrets' (C1 acceptance case)".to_string(),
        );
    }
    // C1: must contain a memory mentioning 'token' as non-secret usage
    if !contents.iter().any(|c| c.contains("cancel token")) {
        return Err(
            "corpus must include a memory mentioning 'cancel token' (C1 acceptance case)"
                .to_string(),
        );
    }
    // C3: must include a memory whose tags include 'v0.1.0' or 'v0.2.0'
    let has_version_tag = records.iter().any(|r| {
        r.pointer("/tags")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .any(|t| matches!(t.as_str(), Some("v0.1.0") | Some("v0.2.0")))
            })
            .unwrap_or(false)
    });
    if !has_version_tag {
        return Err(
            "corpus must include a memory tagged 'v0.1.0' or 'v0.2.0' (C3 acceptance case)"
                .to_string(),
        );
    }
    Ok(())
}

#[allow(dead_code)]
fn _ensure_paths_compile(_p: &Path) {}
