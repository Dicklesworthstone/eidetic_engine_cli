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

    let scan_lines = lines
        .iter()
        .map(|line| strip_rust_line_noise(line))
        .collect::<Vec<_>>();

    for (index, line) in scan_lines.iter().enumerate() {
        let line_no = index + 1;
        if line.contains("#[determinism::required]")
            && !function_signature_has_deterministic_seed(&scan_lines, index)
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
        if line.contains("rand::random::<") || line.contains("rand::random(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_rand_random",
                message: "use Deterministic<Seed> instead of rand::random",
            });
        }
        if line.contains("Uuid::new_v4(") || line.contains("uuid::Uuid::new_v4(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_uuid_v4",
                message: "use DeterministicClock/seeded ID helpers instead of Uuid::new_v4",
            });
        }
        if line.contains("Instant::now(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_instant_now",
                message: "inject timing at the boundary instead of calling Instant::now",
            });
        }
        if line.contains("SystemTime::now(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_system_time_now",
                message: "inject wall-clock time at the boundary instead of calling SystemTime::now",
            });
        }
        if line.contains("std::env::var(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_env_var",
                message: "read env through the registered config boundary",
            });
        }
        if line.contains(".iter()") && nearby_lines_contain(&scan_lines, index, "HashMap") {
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

fn function_signature_has_deterministic_seed(lines: &[String], attribute_index: usize) -> bool {
    for line in lines.iter().skip(attribute_index + 1).take(16) {
        if line.trim().is_empty() {
            continue;
        }
        if line.contains("Deterministic<Seed>") {
            return true;
        }
        if line.contains('{') || line.contains(';') {
            return false;
        }
    }

    false
}

fn strip_rust_line_noise(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut escaped = false;
    let mut in_string = false;
    let mut in_char = false;
    let mut cleaned = String::with_capacity(line.len());

    for index in 0..bytes.len() {
        let byte = bytes[index];
        if escaped {
            escaped = false;
            continue;
        }
        if byte == b'\\' && (in_string || in_char) {
            escaped = true;
            continue;
        }
        if byte == b'"' && !in_char {
            in_string = !in_string;
            continue;
        }
        if byte == b'\'' && !in_string {
            in_char = !in_char;
            continue;
        }
        if !in_string && !in_char && byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            return cleaned;
        }
        if !in_string && !in_char {
            cleaned.push(char::from(byte));
        }
    }

    cleaned
}

fn nearby_lines_contain(lines: &[String], index: usize, needle: &str) -> bool {
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

    #[test]
    fn multiline_seeded_required_function_does_not_emit_missing_seed() {
        let fixture = r#"
            #[determinism::required]
            fn seeded(
                _: &ee::runtime::determinism::Deterministic<Seed>,
            ) {}
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert!(!report.contains("missing_seed_param"));
    }

    #[test]
    fn comments_and_strings_do_not_emit_known_violations() {
        let fixture = r#"
            fn documentation_mentions() {
                let _ = "rand::random::<u64>() Instant::now() std::fs::read_dir(.)";
                // rand::thread_rng();
                // std::env::var("EE_SEED");
            }
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert_eq!(report, "schema: ee.determinism_lint_fixture.v1\n");
    }
}
