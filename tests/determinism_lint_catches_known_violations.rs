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
    let mut findings = Vec::new();

    let scan_lines = strip_rust_noise(source);

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
        if line.contains("Uuid::now_v7(") || line.contains("uuid::Uuid::now_v7(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_uuid_v7_now",
                message: "use DeterministicClock/seeded ID helpers instead of Uuid::now_v7",
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
        if line.contains("std::env::var_os(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_env_var_os",
                message: "read optional env through the registered config boundary",
            });
        }
        if line.contains("std::env::vars(") || line.contains("std::env::vars_os(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_env_iteration",
                message: "iterate env only through a deterministic registered boundary",
            });
        }
        if hash_map_iteration_call(line) && nearby_lines_contain(&scan_lines, index, "HashMap") {
            findings.push(Finding {
                line: line_no,
                code: "hashmap_iteration",
                message: "sort HashMap entries before deterministic output",
            });
        }
        if line.contains("std::fs::read_dir(") || line.contains("fs::read_dir(") {
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

fn strip_rust_noise(source: &str) -> Vec<String> {
    let chars = source.chars().collect::<Vec<_>>();
    let mut lines = vec![String::new()];
    let mut index = 0;
    let mut state = StripState::Normal;

    while index < chars.len() {
        let ch = chars[index];
        match &mut state {
            StripState::Normal => {
                if ch == '\n' {
                    lines.push(String::new());
                    index += 1;
                } else if starts_with(&chars, index, "//") {
                    index = skip_until_newline(&chars, index + 2);
                } else if starts_with(&chars, index, "/*") {
                    state = StripState::BlockComment { depth: 1 };
                    index += 2;
                } else if let Some((consumed, hashes)) = raw_string_start(&chars, index) {
                    state = StripState::RawString { hashes };
                    index += consumed;
                } else if ch == '"' {
                    state = StripState::String { escaped: false };
                    index += 1;
                } else if ch == '\'' {
                    state = StripState::Char { escaped: false };
                    index += 1;
                } else {
                    lines.last_mut().expect("at least one output line").push(ch);
                    index += 1;
                }
            }
            StripState::String { escaped } => {
                if ch == '\n' {
                    lines.push(String::new());
                    *escaped = false;
                    index += 1;
                } else if *escaped {
                    *escaped = false;
                    index += 1;
                } else if ch == '\\' {
                    *escaped = true;
                    index += 1;
                } else if ch == '"' {
                    state = StripState::Normal;
                    index += 1;
                } else {
                    index += 1;
                }
            }
            StripState::Char { escaped } => {
                if ch == '\n' {
                    lines.push(String::new());
                    *escaped = false;
                    index += 1;
                } else if *escaped {
                    *escaped = false;
                    index += 1;
                } else if ch == '\\' {
                    *escaped = true;
                    index += 1;
                } else if ch == '\'' {
                    state = StripState::Normal;
                    index += 1;
                } else {
                    index += 1;
                }
            }
            StripState::BlockComment { depth } => {
                if ch == '\n' {
                    lines.push(String::new());
                    index += 1;
                } else if starts_with(&chars, index, "/*") {
                    *depth += 1;
                    index += 2;
                } else if starts_with(&chars, index, "*/") {
                    *depth -= 1;
                    index += 2;
                    if *depth == 0 {
                        state = StripState::Normal;
                    }
                } else {
                    index += 1;
                }
            }
            StripState::RawString { hashes } => {
                if ch == '\n' {
                    lines.push(String::new());
                    index += 1;
                } else if raw_string_end(&chars, index, *hashes) {
                    let delimiter_len = *hashes;
                    state = StripState::Normal;
                    index += 1 + delimiter_len;
                } else {
                    index += 1;
                }
            }
        }
    }

    lines
}

#[derive(Debug)]
enum StripState {
    Normal,
    String { escaped: bool },
    Char { escaped: bool },
    BlockComment { depth: usize },
    RawString { hashes: usize },
}

fn starts_with(chars: &[char], index: usize, needle: &str) -> bool {
    needle
        .chars()
        .enumerate()
        .all(|(offset, expected)| chars.get(index + offset) == Some(&expected))
}

fn skip_until_newline(chars: &[char], mut index: usize) -> usize {
    while index < chars.len() && chars[index] != '\n' {
        index += 1;
    }
    index
}

fn raw_string_start(chars: &[char], index: usize) -> Option<(usize, usize)> {
    if index > 0 && is_identifier_char(chars[index - 1]) {
        return None;
    }

    let raw_prefix_len = if chars.get(index) == Some(&'r') {
        1
    } else if chars.get(index) == Some(&'b') && chars.get(index + 1) == Some(&'r') {
        2
    } else {
        return None;
    };

    let mut cursor = index + raw_prefix_len;
    let mut hashes = 0;
    while chars.get(cursor) == Some(&'#') {
        hashes += 1;
        cursor += 1;
    }
    if chars.get(cursor) == Some(&'"') {
        Some((raw_prefix_len + hashes + 1, hashes))
    } else {
        None
    }
}

fn raw_string_end(chars: &[char], index: usize, hashes: usize) -> bool {
    chars.get(index) == Some(&'"')
        && (0..hashes).all(|offset| chars.get(index + 1 + offset) == Some(&'#'))
}

fn is_identifier_char(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

fn nearby_lines_contain(lines: &[String], index: usize, needle: &str) -> bool {
    let start = index.saturating_sub(3);
    lines[start..=index]
        .iter()
        .any(|line| line.contains(needle))
}

fn hash_map_iteration_call(line: &str) -> bool {
    [".iter()", ".keys()", ".values()", ".into_iter()"]
        .iter()
        .any(|needle| line.contains(needle))
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
                // std::env::var_os("EE_SEED");
                // std::env::vars();
                // fs::read_dir(".");
            }
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert_eq!(report, "schema: ee.determinism_lint_fixture.v1\n");
    }

    #[test]
    fn block_comments_and_raw_strings_do_not_emit_known_violations() {
        let fixture = r##"
            /*
             * rand::thread_rng();
             * std::env::var("EE_SEED");
             * std::env::var_os("EE_SEED");
             * std::env::vars();
             * std::fs::read_dir(".");
             * fs::read_dir(".");
             */
            fn documentation_mentions() {
                let _ = r#"Uuid::new_v4() Instant::now() SystemTime::now()"#;
            }
        "##;
        let report = render_report(&scan_fixture(fixture));
        assert_eq!(report, "schema: ee.determinism_lint_fixture.v1\n");
    }

    #[test]
    fn env_and_read_dir_aliases_emit_known_violations() {
        let fixture = r#"
            use std::fs;

            fn ambient() {
                let _ = std::env::var_os("EE_SEED");
                let _ = std::env::vars();
                let _ = std::env::vars_os();
                let _ = fs::read_dir(".");
            }
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert!(report.contains("ambient_env_var_os"));
        assert!(report.contains("ambient_env_iteration"));
        assert!(report.contains("unsorted_read_dir"));
    }

    #[test]
    fn hashmap_iteration_aliases_emit_known_violations() {
        let fixture = r#"
            use std::collections::HashMap;

            fn ambient(map: HashMap<String, String>) {
                for _ in map.keys() {}
                for _ in map.values() {}
                for _ in map.into_iter() {}
            }
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert_eq!(report.matches("hashmap_iteration").count(), 3);
    }
}
