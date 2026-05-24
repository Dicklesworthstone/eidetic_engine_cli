//! N4.4 known-violation fixture harness.
//!
//! This is a deterministic source-level UI harness for the first N4.4 lint
//! slice. It freezes the violations that the eventual proc-macro/trybuild layer
//! must reject at compile time.

use std::fs;
use std::path::{Path, PathBuf};

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

#[test]
fn raw_ee_env_reads_are_forbidden_outside_env_registry() -> Result<(), String> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let allowed_path = manifest_dir.join("src/config/env_registry.rs");
    let mut rust_files = Vec::new();
    collect_rust_files(&manifest_dir.join("src"), &mut rust_files)?;
    rust_files.sort();

    let mut violations = Vec::new();
    for path in rust_files {
        if path == allowed_path {
            continue;
        }
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        for (index, line) in source.lines().enumerate() {
            if line_has_raw_ee_env_read(line) {
                let rel = path.strip_prefix(manifest_dir).unwrap_or(path.as_path());
                violations.push(format!(
                    "{}:{}: raw EE_* env read outside src/config/env_registry.rs",
                    rel.display(),
                    index + 1
                ));
            }
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "raw literal EE_* environment reads must go through EnvVar/read/is_set:\n{}",
            violations.join("\n")
        ))
    }
}

fn collect_rust_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(dir)
        .map_err(|error| format!("failed to read dir {}: {error}", dir.display()))?
    {
        let entry = entry
            .map_err(|error| format!("failed to read dir entry in {}: {error}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to stat {}: {error}", path.display()))?;
        if file_type.is_dir() {
            collect_rust_files(&path, files)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            files.push(path);
        }
    }
    Ok(())
}

fn line_has_raw_ee_env_read(line: &str) -> bool {
    let Some(code) = line.split("//").next() else {
        return false;
    };
    [
        "std::env::var(\"EE_",
        "std::env::var_os(\"EE_",
        "env::var(\"EE_",
        "env::var_os(\"EE_",
    ]
    .iter()
    .any(|needle| code.contains(needle))
}

fn scan_fixture(source: &str) -> Vec<Finding> {
    let mut findings = Vec::new();

    let scan_lines = strip_rust_noise(source);
    let mut hash_map_bindings = Vec::new();
    let mut hash_set_bindings = Vec::new();
    let mut awaiting_required_body = false;
    let mut required_body_depth = 0_usize;

    for (index, line) in scan_lines.iter().enumerate() {
        let line_no = index + 1;
        if line_declares_function(line) {
            hash_map_bindings.clear();
            hash_set_bindings.clear();
        }
        hash_map_bindings.extend(hash_collection_bindings(line, "HashMap"));
        hash_set_bindings.extend(hash_collection_bindings(line, "HashSet"));

        if line.contains("#[determinism::required]") {
            awaiting_required_body = true;
            if !function_signature_has_deterministic_seed(&scan_lines, index) {
                findings.push(Finding {
                    line: line_no,
                    code: "missing_seed_param",
                    message: "#[determinism::required] requires a Deterministic<Seed> parameter",
                });
            }
        }

        let begins_required_body = awaiting_required_body && line.contains('{');
        let in_required_body = required_body_depth > 0 || begins_required_body;

        if !in_required_body {
            if awaiting_required_body && line.contains(';') {
                awaiting_required_body = false;
            }
            continue;
        }

        if line.contains("thread_rng(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_thread_rng",
                message: "use Deterministic<Seed> instead of rand::thread_rng",
            });
        }
        if line.contains("rand::random::<")
            || line.contains("rand::random(")
            || contains_path_call(line, "random::<")
            || contains_path_call(line, "random(")
        {
            findings.push(Finding {
                line: line_no,
                code: "ambient_rand_random",
                message: "use Deterministic<Seed> instead of rand::random",
            });
        }
        if line.contains("getrandom::fill(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_getrandom_fill",
                message: "use Deterministic<Seed> instead of direct OS entropy",
            });
        }
        if line.contains("ring::rand::SystemRandom::new(")
            || contains_path_call(line, "SystemRandom::new(")
        {
            findings.push(Finding {
                line: line_no,
                code: "ambient_ring_system_random",
                message: "use Deterministic<Seed> instead of ring::rand::SystemRandom",
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
        if line.contains("Utc::now(") || line.contains("chrono::Utc::now(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_chrono_utc_now",
                message: "inject UTC timestamps at the boundary instead of calling Utc::now",
            });
        }
        if line.contains("Local::now(") || line.contains("chrono::Local::now(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_chrono_local_now",
                message: "inject local timestamps at the boundary instead of calling Local::now",
            });
        }
        if domain_id_now_call(line) {
            findings.push(Finding {
                line: line_no,
                code: "ambient_domain_id_now",
                message: "use seeded ID helpers instead of ambient typed Id::now",
            });
        }
        if line.contains("std::env::var(") || contains_path_call(line, "env::var(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_env_var",
                message: "read env through the registered config boundary",
            });
        }
        if line.contains("std::env::var_os(") || contains_path_call(line, "env::var_os(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_env_var_os",
                message: "read optional env through the registered config boundary",
            });
        }
        if line.contains("std::env::vars(")
            || line.contains("std::env::vars_os(")
            || contains_path_call(line, "env::vars(")
            || contains_path_call(line, "env::vars_os(")
        {
            findings.push(Finding {
                line: line_no,
                code: "ambient_env_iteration",
                message: "iterate env only through a deterministic registered boundary",
            });
        }
        if line.contains("std::env::args(")
            || line.contains("std::env::args_os(")
            || contains_path_call(line, "env::args(")
            || contains_path_call(line, "env::args_os(")
        {
            findings.push(Finding {
                line: line_no,
                code: "ambient_process_args",
                message: "read process args through the registered CLI boundary",
            });
        }
        let ambient_current_dir = line.contains("std::env::current_dir(")
            || contains_path_call(line, "env::current_dir(");
        if ambient_current_dir {
            findings.push(Finding {
                line: line_no,
                code: "ambient_current_dir",
                message: "inject current directory/workspace at the boundary instead of calling env::current_dir",
            });
        }
        let ambient_temp_dir =
            line.contains("std::env::temp_dir(") || contains_path_call(line, "env::temp_dir(");
        if ambient_temp_dir {
            findings.push(Finding {
                line: line_no,
                code: "ambient_temp_dir",
                message: "inject temp directory at the boundary instead of calling env::temp_dir",
            });
        }
        if hash_collection_iteration_call(line, &hash_map_bindings) {
            findings.push(Finding {
                line: line_no,
                code: "hashmap_iteration",
                message: "sort HashMap entries before deterministic output",
            });
        }
        if hash_collection_iteration_call(line, &hash_set_bindings) {
            findings.push(Finding {
                line: line_no,
                code: "hashset_iteration",
                message: "sort HashSet entries before deterministic output",
            });
        }
        if line.contains("std::fs::read_dir(") || line.contains("fs::read_dir(") {
            findings.push(Finding {
                line: line_no,
                code: "unsorted_read_dir",
                message: "sort read_dir entries before deterministic output",
            });
        }
        if line.contains("std::process::id(") || contains_path_call(line, "process::id(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_process_id",
                message: "inject the host PID at the boundary instead of calling std::process::id",
            });
        }
        if line.contains("std::thread::current(") || contains_path_call(line, "thread::current(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_thread_current",
                message: "inject the thread identifier at the boundary instead of std::thread::current",
            });
        }

        if begins_required_body {
            awaiting_required_body = false;
            required_body_depth = update_brace_depth(0, line);
        } else if required_body_depth > 0 {
            required_body_depth = update_brace_depth(required_body_depth, line);
        } else if awaiting_required_body && line.contains(';') {
            awaiting_required_body = false;
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

fn hash_collection_bindings(line: &str, type_name: &str) -> Vec<String> {
    let mut names = Vec::new();
    let short = format!(": {type_name}");
    let qualified = format!(": std::collections::{type_name}");
    collect_hash_collection_bindings(line, &short, &mut names);
    collect_hash_collection_bindings(line, &qualified, &mut names);
    names
}

fn collect_hash_collection_bindings(line: &str, needle: &str, names: &mut Vec<String>) {
    let mut search_start = 0;
    while let Some(relative_index) = line[search_start..].find(needle) {
        let index = search_start + relative_index;
        let prefix = &line[..index];
        if let Some(name) = prefix
            .rsplit(|ch: char| !is_identifier_char(ch))
            .next()
            .filter(|name| !name.is_empty())
        {
            if !names.iter().any(|existing| existing == name) {
                names.push(name.to_owned());
            }
        }
        search_start = index + needle.len();
    }
}

fn hash_collection_iteration_call(line: &str, bindings: &[String]) -> bool {
    bindings.iter().any(|binding| {
        ["iter", "keys", "values", "into_iter", "drain"]
            .iter()
            .any(|method| contains_receiver_method_call(line, binding, method))
    })
}

fn contains_receiver_method_call(line: &str, receiver: &str, method: &str) -> bool {
    let needle = format!("{receiver}.{method}()");
    let mut search_start = 0;
    while let Some(relative_index) = line[search_start..].find(&needle) {
        let index = search_start + relative_index;
        let previous = line[..index].chars().next_back();
        if !matches!(previous, Some(ch) if is_identifier_char(ch)) {
            return true;
        }
        search_start = index + needle.len();
    }
    false
}

fn line_declares_function(line: &str) -> bool {
    let mut search_start = 0;
    while let Some(relative_index) = line[search_start..].find("fn ") {
        let index = search_start + relative_index;
        let previous = line[..index].chars().next_back();
        if !matches!(previous, Some(ch) if is_identifier_char(ch)) {
            return true;
        }
        search_start = index + "fn ".len();
    }

    false
}

fn contains_path_call(line: &str, needle: &str) -> bool {
    let mut search_start = 0;
    while let Some(relative_index) = line[search_start..].find(needle) {
        let index = search_start + relative_index;
        let previous = line[..index].chars().next_back();
        if !matches!(previous, Some(ch) if is_identifier_char(ch) || ch == ':') {
            return true;
        }
        search_start = index + needle.len();
    }

    false
}

fn update_brace_depth(mut depth: usize, line: &str) -> usize {
    for ch in line.chars() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }

    depth
}

fn domain_id_now_call(line: &str) -> bool {
    let Some(now_index) = line.find("::now(") else {
        return false;
    };
    let prefix = &line[..now_index];
    let type_name = prefix
        .rsplit(|ch: char| !(ch == '_' || ch.is_ascii_alphanumeric()))
        .next()
        .unwrap_or_default();

    type_name.ends_with("Id")
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
    use std::path::Path;

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
    fn untagged_boundary_ambient_calls_do_not_emit_known_violations() {
        let fixture = r#"
            fn boundary_context() {
                let _ = rand::random::<u64>();
                let _ = std::time::Instant::now();
                let _ = std::env::current_dir();
                let _ = std::env::temp_dir();
                let _ = std::fs::read_dir(".");
            }
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert_eq!(report, "schema: ee.determinism_lint_fixture.v1\n");
    }

    #[test]
    fn comments_and_strings_do_not_emit_known_violations() {
        let fixture = r#"
            fn documentation_mentions() {
                let _ = "rand::random::<u64>() Instant::now() chrono::Utc::now() std::fs::read_dir(.) HashSet";
                // rand::thread_rng();
                // getrandom::fill(&mut bytes);
                // chrono::Local::now();
                // std::env::var("EE_SEED");
                // std::env::var_os("EE_SEED");
                // std::env::vars();
                // env::var("EE_SEED");
                // env::var_os("EE_SEED");
                // env::vars();
                // env::vars_os();
                // std::env::args();
                // std::env::args_os();
                // env::args();
                // env::args_os();
                // std::env::current_dir();
                // env::current_dir();
                // std::env::temp_dir();
                // env::temp_dir();
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
             * getrandom::fill(&mut bytes);
             * std::env::var("EE_SEED");
             * std::env::var_os("EE_SEED");
             * std::env::vars();
             * env::var("EE_SEED");
             * env::var_os("EE_SEED");
             * env::vars();
             * env::vars_os();
             * std::env::args();
             * std::env::args_os();
             * env::args();
             * env::args_os();
             * std::env::current_dir();
             * env::current_dir();
             * std::env::temp_dir();
             * env::temp_dir();
             * chrono::Utc::now();
             * std::fs::read_dir(".");
             * fs::read_dir(".");
             */
            fn documentation_mentions() {
                let _ = r#"Uuid::new_v4() Instant::now() SystemTime::now() chrono::Local::now() std::env::current_dir() env::current_dir() std::env::temp_dir() env::temp_dir()"#;
            }
        "##;
        let report = render_report(&scan_fixture(fixture));
        assert_eq!(report, "schema: ee.determinism_lint_fixture.v1\n");
    }

    #[test]
    fn env_and_read_dir_aliases_emit_known_violations() {
        let fixture = r#"
            use std::{env, fs};

            #[determinism::required]
            fn ambient(_: &ee::runtime::determinism::Deterministic<Seed>) {
                let _ = std::env::var_os("EE_SEED");
                let _ = std::env::vars();
                let _ = std::env::vars_os();
                let _ = std::env::args();
                let _ = std::env::args_os();
                let _ = env::var("EE_ALIAS_SEED");
                let _ = env::var_os("EE_ALIAS_SEED");
                let _ = env::vars();
                let _ = env::vars_os();
                let _ = env::args();
                let _ = env::args_os();
                let _ = std::env::current_dir();
                let _ = env::current_dir();
                let _ = std::env::temp_dir();
                let _ = env::temp_dir();
                let _ = fs::read_dir(".");
            }
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert!(report.contains("ambient_env_var"));
        assert!(report.contains("ambient_env_var_os"));
        assert_eq!(report.matches(": ambient_env_iteration:").count(), 4);
        assert_eq!(report.matches(": ambient_process_args:").count(), 4);
        assert_eq!(report.matches(": ambient_current_dir:").count(), 2);
        assert_eq!(report.matches(": ambient_temp_dir:").count(), 2);
        assert!(report.contains("unsorted_read_dir"));
    }

    #[test]
    fn hash_collection_iteration_aliases_emit_known_violations() {
        let fixture = r#"
            use std::collections::{HashMap, HashSet};

            #[determinism::required]
            fn ambient(
                _: &ee::runtime::determinism::Deterministic<Seed>,
                mut map: HashMap<String, String>,
                mut set: HashSet<String>,
            ) {
                for _ in map.keys() {}
                for _ in map.values() {}
                for _ in map.drain() {}
                for _ in map.into_iter() {}
                for _ in set.iter() {}
                for _ in set.drain() {}
                for _ in set.into_iter() {}
            }
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert_eq!(report.matches("hashmap_iteration").count(), 4);
        assert_eq!(report.matches("hashset_iteration").count(), 3);
    }

    #[test]
    fn hash_collection_bindings_do_not_leak_across_pub_functions() {
        let fixture = r#"
            use std::collections::HashMap;

            #[determinism::required]
            pub fn first(
                _: &ee::runtime::determinism::Deterministic<Seed>,
                map: HashMap<String, String>,
            ) {
                let _ = map.len();
            }

            #[determinism::required]
            pub(crate) fn second(_: &ee::runtime::determinism::Deterministic<Seed>) {
                for _ in map.iter() {}
            }
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert!(
            !report.contains("hashmap_iteration"),
            "hash bindings from one function must not leak into the next function: {report}"
        );
    }

    #[test]
    fn domain_id_now_calls_emit_known_violations() {
        let fixture = r#"
            #[determinism::required]
            fn ambient(_: &ee::runtime::determinism::Deterministic<Seed>) {
                let _ = ee::models::MemoryId::now();
                let _ = RuleId::now();
                let _ = uuid::Uuid::now_v7();
            }
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert_eq!(report.matches("ambient_domain_id_now").count(), 2);
    }

    #[test]
    fn direct_os_entropy_calls_emit_known_violations() {
        let fixture = r#"
            #[determinism::required]
            fn ambient(_: &ee::runtime::determinism::Deterministic<Seed>) {
                let mut bytes = [0u8; 32];
                getrandom::fill(&mut bytes).unwrap();
                let _ = ring::rand::SystemRandom::new();
                let _ = SystemRandom::new();
            }
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert_eq!(report.matches("ambient_getrandom_fill").count(), 1);
        assert_eq!(report.matches("ambient_ring_system_random").count(), 2);
    }

    #[test]
    fn imported_rand_random_calls_emit_known_violations() {
        let fixture = r#"
            use rand::random;

            #[determinism::required]
            fn ambient(_: &ee::runtime::determinism::Deterministic<Seed>) {
                let _: u64 = random();
                let _: u64 = random::<u64>();
            }
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert_eq!(report.matches("ambient_rand_random").count(), 2);
    }

    #[test]
    fn process_id_and_thread_current_calls_emit_known_violations() {
        // Two distinct ambient classes: process::id() leaks the host
        // PID into deterministic output, and thread::current() leaks
        // the runtime-assigned thread identifier. Both must be caught
        // through either the fully-qualified `std::` path or a
        // pre-imported `process::` / `thread::` path.
        let fixture = r#"
            use std::{process, thread};

            #[determinism::required]
            fn ambient(_: &ee::runtime::determinism::Deterministic<Seed>) {
                let _ = std::process::id();
                let _ = process::id();
                let _ = std::thread::current();
                let _ = thread::current();
            }
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert_eq!(report.matches("ambient_process_id").count(), 2);
        assert_eq!(report.matches("ambient_thread_current").count(), 2);
    }

    #[test]
    fn determinism_required_proc_macro_crate_is_present_and_dependency_free() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let manifest = std::fs::read_to_string(root.join("crates/determinism/Cargo.toml"))
            .expect("determinism proc-macro manifest");
        let source = std::fs::read_to_string(root.join("crates/determinism/src/lib.rs"))
            .expect("determinism proc-macro source");

        assert!(manifest.contains("proc-macro = true"));
        assert!(!manifest.contains("[dependencies]"));
        assert!(source.contains("#[proc_macro_attribute]"));
        assert!(source.contains("pub fn required"));
        assert!(source.contains("Deterministic<Seed>"));
        assert!(source.contains("fn has_deterministic_seed_parameter"));
        assert!(source.contains("group.delimiter() == Delimiter::Parenthesis"));
        assert!(!source.contains("if !compact.contains(\"Deterministic<Seed>\")"));
        assert!(source.contains("thread_rng("));
        assert!(source.contains("SystemTime::now("));
        assert!(source.contains("std::fs::read_dir("));
        assert!(source.contains("contains_domain_id_now"));
    }
}
