//! Redaction fuzz corpus tests (EE-TST-REDACT-FUZZ-001).
//!
//! Tests the redaction leak detector against a corpus of positive and negative
//! fixtures covering various secret types, encodings, and structural contexts.

use ee::eval::{RedactionClass, RedactionLeakDetector};
use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct RedactionFixture {
    id: String,
    #[serde(rename = "description")]
    _description: String,
    input: String,
    expected_class: Option<String>,
    encoding: String,
    context: String,
    expected_detected: bool,
    #[serde(default, rename = "notes")]
    _notes: Option<String>,
}

fn load_fixtures(dir: &Path) -> Vec<RedactionFixture> {
    let mut fixtures = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                if let Ok(content) = fs::read_to_string(&path) {
                    match serde_json::from_str::<RedactionFixture>(&content) {
                        Ok(fixture) => fixtures.push(fixture),
                        Err(e) => eprintln!("Failed to parse {:?}: {}", path, e),
                    }
                }
            }
        }
    }
    fixtures
}

fn class_from_str(s: &str) -> Option<RedactionClass> {
    match s {
        "secret" => Some(RedactionClass::Secret),
        "pii" => Some(RedactionClass::Pii),
        "internal_path" => Some(RedactionClass::InternalPath),
        "proprietary" => Some(RedactionClass::Proprietary),
        "custom" => Some(RedactionClass::Custom),
        _ => None,
    }
}

#[test]
fn positive_corpus_detects_all_secrets() {
    let fixtures_dir = Path::new("tests/fixtures/redaction/positive");
    let fixtures = load_fixtures(fixtures_dir);
    assert!(!fixtures.is_empty(), "No positive fixtures found");

    let detector = RedactionLeakDetector::new();
    let mut failures = Vec::new();

    for fixture in &fixtures {
        let leaks = detector.detect_leaks(&fixture.input);
        let detected = !leaks.is_empty();

        if detected != fixture.expected_detected {
            failures.push(format!(
                "{}: expected detected={}, got detected={} (input: {:?})",
                fixture.id, fixture.expected_detected, detected, fixture.input
            ));
            continue;
        }

        // Only check expected_class when detection was expected AND occurred
        if fixture.expected_detected && detected {
            if let Some(ref expected_class_str) = fixture.expected_class {
                if let Some(expected_class) = class_from_str(expected_class_str) {
                    let has_expected_class = leaks.iter().any(|l| l.class == expected_class);
                    if !has_expected_class {
                        failures.push(format!(
                            "{}: expected class {:?} but found {:?}",
                            fixture.id,
                            expected_class,
                            leaks.iter().map(|l| l.class).collect::<Vec<_>>()
                        ));
                    }
                }
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "Positive corpus failures ({}/{}):\n{}",
            failures.len(),
            fixtures.len(),
            failures.join("\n")
        );
    }

    eprintln!(
        "Positive corpus: {}/{} fixtures passed",
        fixtures.len(),
        fixtures.len()
    );
}

#[test]
fn negative_corpus_no_false_positives() {
    let fixtures_dir = Path::new("tests/fixtures/redaction/negative");
    let fixtures = load_fixtures(fixtures_dir);
    assert!(!fixtures.is_empty(), "No negative fixtures found");

    let detector = RedactionLeakDetector::new();
    let mut failures = Vec::new();

    for fixture in &fixtures {
        let leaks = detector.detect_leaks(&fixture.input);
        let detected = !leaks.is_empty();

        if detected != fixture.expected_detected {
            failures.push(format!(
                "{}: expected no detection but found {:?} (input: {:?})",
                fixture.id,
                leaks.iter().map(|l| &l.matched_text).collect::<Vec<_>>(),
                fixture.input
            ));
        }
    }

    if !failures.is_empty() {
        panic!(
            "Negative corpus failures ({}/{}):\n{}",
            failures.len(),
            fixtures.len(),
            failures.join("\n")
        );
    }

    eprintln!(
        "Negative corpus: {}/{} fixtures passed (no false positives)",
        fixtures.len(),
        fixtures.len()
    );
}

#[test]
fn redaction_is_deterministic() {
    let fixtures_dir = Path::new("tests/fixtures/redaction/positive");
    let fixtures = load_fixtures(fixtures_dir);
    let detector = RedactionLeakDetector::new();

    for fixture in &fixtures {
        let leaks1 = detector.detect_leaks(&fixture.input);
        let leaks2 = detector.detect_leaks(&fixture.input);

        assert_eq!(
            leaks1.len(),
            leaks2.len(),
            "{}: detection count not deterministic",
            fixture.id
        );

        for (l1, l2) in leaks1.iter().zip(leaks2.iter()) {
            assert_eq!(
                l1.matched_text, l2.matched_text,
                "{}: matched text not deterministic",
                fixture.id
            );
            assert_eq!(
                l1.class, l2.class,
                "{}: class not deterministic",
                fixture.id
            );
        }
    }
}

#[test]
fn generate_coverage_report() {
    let positive_dir = Path::new("tests/fixtures/redaction/positive");
    let negative_dir = Path::new("tests/fixtures/redaction/negative");
    let gaps_dir = Path::new("tests/fixtures/redaction/gaps");

    let positive = load_fixtures(positive_dir);
    let negative = load_fixtures(negative_dir);
    let gaps = load_fixtures(gaps_dir);

    let mut encodings: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut contexts: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut classes: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for f in positive.iter().chain(negative.iter()).chain(gaps.iter()) {
        encodings.insert(&f.encoding);
        contexts.insert(&f.context);
        if let Some(ref c) = f.expected_class {
            classes.insert(c);
        }
    }

    let report = serde_json::json!({
        "schema": "ee.redaction_coverage.v1",
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "positive_fixtures": positive.len(),
        "negative_fixtures": negative.len(),
        "gap_fixtures": gaps.len(),
        "encodings_covered": encodings.into_iter().collect::<Vec<_>>(),
        "contexts_covered": contexts.into_iter().collect::<Vec<_>>(),
        "classes_covered": classes.into_iter().collect::<Vec<_>>(),
        "fixture_ids": {
            "positive": positive.iter().map(|f| &f.id).collect::<Vec<_>>(),
            "negative": negative.iter().map(|f| &f.id).collect::<Vec<_>>(),
            "gaps": gaps.iter().map(|f| &f.id).collect::<Vec<_>>()
        }
    });

    eprintln!(
        "Coverage Report:\n{}",
        serde_json::to_string_pretty(&report)
            .unwrap_or_else(|error| format!("failed to serialize coverage report: {error}"))
    );
}
