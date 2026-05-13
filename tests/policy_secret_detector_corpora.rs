//! C5 real-world secret-pattern corpus checks.
//!
//! Fixture names are frozen subsets from upstream public catalogs:
//! - gitleaks config: https://github.com/gitleaks/gitleaks/blob/master/config/gitleaks.toml
//! - trufflehog detectors: https://github.com/trufflesecurity/trufflehog/tree/main/pkg/detectors
//!
//! Values are synthetic and intentionally non-production. The test exercises the
//! local value-shape redactor, not upstream detection code.

use std::fs::OpenOptions;
use std::io::Write;

use ee::policy::redact_secret_like_content;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct SecretPatternFixture {
    id: String,
    source: String,
    #[serde(default)]
    upstream_pattern: Option<String>,
    #[serde(default)]
    source_pattern: Option<String>,
    input: String,
    #[serde(default)]
    expected_detected: Option<bool>,
    #[serde(default)]
    expected_redacted: Option<bool>,
    #[serde(default)]
    expected_reason: Option<String>,
}

impl SecretPatternFixture {
    fn pattern(&self) -> &str {
        self.upstream_pattern
            .as_deref()
            .or(self.source_pattern.as_deref())
            .unwrap_or("unknown")
    }

    fn expects_detection(&self) -> bool {
        self.expected_detected
            .or(self.expected_redacted)
            .unwrap_or(false)
    }
}

#[derive(Debug)]
struct CorpusReport {
    corpus: &'static str,
    total: usize,
    expected_detected: usize,
    detected_expected: usize,
    false_positives: usize,
    failures: Vec<String>,
}

impl CorpusReport {
    fn detection_rate(&self) -> f64 {
        if self.expected_detected == 0 {
            return 1.0;
        }
        self.detected_expected as f64 / self.expected_detected as f64
    }

    fn false_positive_rate(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        self.false_positives as f64 / self.total as f64
    }
}

fn load_jsonl(raw: &'static str, corpus: &'static str) -> Vec<SecretPatternFixture> {
    raw.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str::<SecretPatternFixture>(line).unwrap_or_else(|error| {
                panic!("{corpus}: invalid JSONL at line {}: {error}", index + 1)
            })
        })
        .collect()
}

fn evaluate_corpus(corpus: &'static str, fixtures: &[SecretPatternFixture]) -> CorpusReport {
    let mut report = CorpusReport {
        corpus,
        total: fixtures.len(),
        expected_detected: fixtures
            .iter()
            .filter(|fixture| fixture.expects_detection())
            .count(),
        detected_expected: 0,
        false_positives: 0,
        failures: Vec::new(),
    };

    for fixture in fixtures {
        let redaction = redact_secret_like_content(&fixture.input);
        let detected = redaction.redacted;
        let reason_matched = fixture
            .expected_reason
            .as_deref()
            .is_none_or(|reason| redaction.redacted_reasons.contains(&reason));

        emit_event(
            "secret_pattern_fixture",
            json!({
                "corpus": corpus,
                "id": fixture.id,
                "source": fixture.source,
                "upstreamPattern": fixture.pattern(),
                "expectedDetected": fixture.expects_detection(),
                "detected": detected,
                "expectedReason": fixture.expected_reason,
                "redactedReasons": redaction.redacted_reasons,
            }),
        );

        match (fixture.expects_detection(), detected, reason_matched) {
            (true, true, true) => report.detected_expected += 1,
            (true, true, false) => report.failures.push(format!(
                "{}: detected but missing expected reason {:?}; got {:?}",
                fixture.id, fixture.expected_reason, redaction.redacted_reasons
            )),
            (true, false, _) => report.failures.push(format!(
                "{}: expected detection for upstream pattern {}",
                fixture.id,
                fixture.pattern()
            )),
            (false, true, _) => {
                report.false_positives += 1;
                report.failures.push(format!(
                    "{}: false positive with reasons {:?}",
                    fixture.id, redaction.redacted_reasons
                ));
            }
            (false, false, _) => {}
        }
    }

    emit_event(
        "secret_pattern_corpus_summary",
        json!({
            "corpus": corpus,
            "total": report.total,
            "expectedDetected": report.expected_detected,
            "detectedExpected": report.detected_expected,
            "detectionRate": report.detection_rate(),
            "falsePositives": report.false_positives,
            "falsePositiveRate": report.false_positive_rate(),
        }),
    );
    report
}

fn emit_event(kind: &str, fields: serde_json::Value) {
    let Some(path) = std::env::var_os("EE_TEST_LOG_PATH") else {
        return;
    };
    let event = json!({
        "schema": "ee.test_event.v1",
        "kind": kind,
        "fields": fields,
    });
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{event}");
    }
}

#[test]
fn gitleaks_subset_meets_detection_rate() {
    let fixtures = load_jsonl(
        include_str!("fixtures/secret_patterns/gitleaks_subset.jsonl"),
        "gitleaks_subset",
    );
    assert!(
        fixtures.len() >= 50,
        "gitleaks subset must cover at least 50 pattern names"
    );
    let report = evaluate_corpus("gitleaks_subset", &fixtures);
    assert!(
        report.detection_rate() >= 0.90,
        "{} detection rate {:.2} below 0.90; failures:\n{}",
        report.corpus,
        report.detection_rate(),
        report.failures.join("\n")
    );
}

#[test]
fn trufflehog_subset_meets_detection_rate() {
    let fixtures = load_jsonl(
        include_str!("fixtures/secret_patterns/trufflehog_subset.jsonl"),
        "trufflehog_subset",
    );
    assert!(
        fixtures.len() >= 50,
        "trufflehog subset must cover at least 50 detector names"
    );
    let report = evaluate_corpus("trufflehog_subset", &fixtures);
    assert!(
        report.detection_rate() >= 0.85,
        "{} detection rate {:.2} below 0.85; failures:\n{}",
        report.corpus,
        report.detection_rate(),
        report.failures.join("\n")
    );
}

#[test]
fn false_positive_corpus_has_zero_triggers() {
    let fixtures = load_jsonl(
        include_str!("fixtures/secret_patterns/false_positive_corpus.jsonl"),
        "false_positive_corpus",
    );
    assert!(
        fixtures.len() >= 100,
        "false-positive corpus must cover at least 100 plain-English mentions"
    );
    let report = evaluate_corpus("false_positive_corpus", &fixtures);
    assert_eq!(
        report.false_positives,
        0,
        "false-positive corpus triggered unexpectedly; failures:\n{}",
        report.failures.join("\n")
    );
}
