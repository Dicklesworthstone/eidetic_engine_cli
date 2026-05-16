//! N4.4 known-violation fixture harness.
//!
//! This is a deterministic source-level UI harness for the first N4.4 lint
//! slice. It freezes the violations that the eventual proc-macro/trybuild layer
//! must reject at compile time.

use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq)]
struct Finding {
    line: usize,
    code: &'static str,
    message: &'static str,
}

#[test]
fn determinism_lint_catches_known_violations() {
    let fixture = include_str!("fixtures/determinism_lint/known_violations.rs");
    let expected = include_str!("fixtures/determinism_lint/known_violations.expected");
    let findings = scan_fixture(fixture);
    let report = render_report(&findings);

    assert_eq!(report, expected);
}

#[test]
fn determinism_lint_fixture_files_are_present() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/determinism_lint");
    assert!(root.join("known_violations.rs").is_file());
    assert!(root.join("known_violations.expected").is_file());
}

fn scan_fixture(source: &str) -> Vec<Finding> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut findings = Vec::new();

    for (index, line) in lines.iter().enumerate() {
        let line_no = index + 1;
        if line.contains("#[determinism::required]")
            && !function_signature_has_deterministic_seed(&lines, index)
        {
            findings.push(Finding {
                line: line_no,
                code: "missing_seed_param",
                message: "#[determinism::required] requires a Deterministic<Seed> parameter",
            });
        }
        if line.contains("thread_rng(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_thread_rng",
                message: "use Deterministic<Seed> instead of rand::thread_rng",
            });
        }
        if line.contains("Instant::now(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_instant_now",
                message: "inject timing at the boundary instead of calling Instant::now",
            });
        }
        if line.contains("std::env::var(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_env_var",
                message: "read env through the registered config boundary",
            });
        }
        if line.contains(".iter()") && nearby_lines_contain(&lines, index, "HashMap") {
            findings.push(Finding {
                line: line_no,
                code: "hashmap_iteration",
                message: "sort HashMap entries before deterministic output",
            });
        }
        if line.contains("std::fs::read_dir(") {
            findings.push(Finding {
                line: line_no,
                code: "unsorted_read_dir",
                message: "sort read_dir entries before deterministic output",
            });
        }
    }

    findings
}

fn function_signature_has_deterministic_seed(lines: &[&str], attribute_index: usize) -> bool {
    lines
        .iter()
        .skip(attribute_index + 1)
        .take(3)
        .any(|line| line.contains("Deterministic<Seed>"))
}

fn nearby_lines_contain(lines: &[&str], index: usize, needle: &str) -> bool {
    let start = index.saturating_sub(3);
    lines[start..=index]
        .iter()
        .any(|line| line.contains(needle))
}

fn render_report(findings: &[Finding]) -> String {
    let mut output = String::from("schema: ee.determinism_lint_fixture.v1\n");
    for finding in findings {
        output.push_str(&format!(
            "line {}: {}: {}\n",
            finding.line, finding.code, finding.message
        ));
    }
    output
}

#[cfg(test)]
mod self_tests {
    use super::{render_report, scan_fixture};

    #[test]
    fn seeded_required_function_does_not_emit_missing_seed() {
        let fixture = r#"
            #[determinism::required]
            fn seeded(_: &ee::runtime::determinism::Deterministic<Seed>) {}
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert!(!report.contains("missing_seed_param"));
    }
}
