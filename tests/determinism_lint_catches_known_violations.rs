//! N4.4 known-violation fixture harness.
//!
//! This is a deterministic source-level UI harness for the first N4.4 lint
//! slice. It freezes the violations that the eventual proc-macro/trybuild layer
//! must reject at compile time.

#![allow(clippy::expect_used, clippy::unwrap_used)]

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
    let compact_code = compact_source_line(code);
    [
        "std::env::var(\"EE_",
        "std::env::var_os(\"EE_",
        "env::var(\"EE_",
        "env::var_os(\"EE_",
    ]
    .iter()
    .any(|needle| contains_path_call(&compact_code, needle))
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

        let compact_line = compact_source_line(line);

        if compact_line.contains("thread_rng(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_thread_rng",
                message: "use Deterministic<Seed> instead of rand::thread_rng",
            });
        }
        if contains_path_call(&compact_line, "rand::random::<")
            || contains_path_call(&compact_line, "rand::random(")
            || contains_path_call(&compact_line, "random::<")
            || contains_path_call(&compact_line, "random(")
        {
            findings.push(Finding {
                line: line_no,
                code: "ambient_rand_random",
                message: "use Deterministic<Seed> instead of rand::random",
            });
        }
        if contains_path_call(&compact_line, "getrandom::fill(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_getrandom_fill",
                message: "use Deterministic<Seed> instead of direct OS entropy",
            });
        }
        if contains_path_call(&compact_line, "ring::rand::SystemRandom::new(")
            || contains_path_call(&compact_line, "SystemRandom::new(")
        {
            findings.push(Finding {
                line: line_no,
                code: "ambient_ring_system_random",
                message: "use Deterministic<Seed> instead of ring::rand::SystemRandom",
            });
        }
        if compact_line.contains("Uuid::new_v4(") || compact_line.contains("uuid::Uuid::new_v4(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_uuid_v4",
                message: "use DeterministicClock/seeded ID helpers instead of Uuid::new_v4",
            });
        }
        if compact_line.contains("Uuid::now_v7(") || compact_line.contains("uuid::Uuid::now_v7(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_uuid_v7_now",
                message: "use DeterministicClock/seeded ID helpers instead of Uuid::now_v7",
            });
        }
        if compact_line.contains("Instant::now(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_instant_now",
                message: "inject timing at the boundary instead of calling Instant::now",
            });
        }
        if compact_line.contains("SystemTime::now(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_system_time_now",
                message: "inject wall-clock time at the boundary instead of calling SystemTime::now",
            });
        }
        if compact_line.contains("Utc::now(") || compact_line.contains("chrono::Utc::now(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_chrono_utc_now",
                message: "inject UTC timestamps at the boundary instead of calling Utc::now",
            });
        }
        if compact_line.contains("Local::now(") || compact_line.contains("chrono::Local::now(") {
            findings.push(Finding {
                line: line_no,
                code: "ambient_chrono_local_now",
                message: "inject local timestamps at the boundary instead of calling Local::now",
            });
        }
        if domain_id_now_call(&compact_line) {
            findings.push(Finding {
                line: line_no,
                code: "ambient_domain_id_now",
                message: "use seeded ID helpers instead of ambient typed Id::now",
            });
        }
        if contains_path_call(&compact_line, "std::env::var(")
            || contains_path_call(&compact_line, "env::var(")
        {
            findings.push(Finding {
                line: line_no,
                code: "ambient_env_var",
                message: "read env through the registered config boundary",
            });
        }
        if contains_path_call(&compact_line, "std::env::var_os(")
            || contains_path_call(&compact_line, "env::var_os(")
        {
            findings.push(Finding {
                line: line_no,
                code: "ambient_env_var_os",
                message: "read optional env through the registered config boundary",
            });
        }
        if contains_path_call(&compact_line, "std::env::vars(")
            || contains_path_call(&compact_line, "std::env::vars_os(")
            || contains_path_call(&compact_line, "env::vars(")
            || contains_path_call(&compact_line, "env::vars_os(")
        {
            findings.push(Finding {
                line: line_no,
                code: "ambient_env_iteration",
                message: "iterate env only through a deterministic registered boundary",
            });
        }
        if contains_path_call(&compact_line, "std::env::args(")
            || contains_path_call(&compact_line, "std::env::args_os(")
            || contains_path_call(&compact_line, "env::args(")
            || contains_path_call(&compact_line, "env::args_os(")
        {
            findings.push(Finding {
                line: line_no,
                code: "ambient_process_args",
                message: "read process args through the registered CLI boundary",
            });
        }
        let ambient_current_dir = contains_path_call(&compact_line, "std::env::current_dir(")
            || contains_path_call(&compact_line, "env::current_dir(");
        if ambient_current_dir {
            findings.push(Finding {
                line: line_no,
                code: "ambient_current_dir",
                message: "inject current directory/workspace at the boundary instead of calling env::current_dir",
            });
        }
        let ambient_temp_dir = contains_path_call(&compact_line, "std::env::temp_dir(")
            || contains_path_call(&compact_line, "env::temp_dir(");
        if ambient_temp_dir {
            findings.push(Finding {
                line: line_no,
                code: "ambient_temp_dir",
                message: "inject temp directory at the boundary instead of calling env::temp_dir",
            });
        }
        if hash_collection_iteration_call(&compact_line, &hash_map_bindings)
            || hash_collection_direct_iteration_call(line, "HashMap")
        {
            findings.push(Finding {
                line: line_no,
                code: "hashmap_iteration",
                message: "sort HashMap entries before deterministic output",
            });
        }
        if hash_collection_iteration_call(&compact_line, &hash_set_bindings)
            || hash_collection_direct_iteration_call(line, "HashSet")
        {
            findings.push(Finding {
                line: line_no,
                code: "hashset_iteration",
                message: "sort HashSet entries before deterministic output",
            });
        }
        if contains_path_call(&compact_line, "std::fs::read_dir(")
            || contains_path_call(&compact_line, "fs::read_dir(")
        {
            findings.push(Finding {
                line: line_no,
                code: "unsorted_read_dir",
                message: "sort read_dir entries before deterministic output",
            });
        }
        if contains_path_call(&compact_line, "std::process::id(")
            || contains_path_call(&compact_line, "process::id(")
        {
            findings.push(Finding {
                line: line_no,
                code: "ambient_process_id",
                message: "inject the host PID at the boundary instead of calling std::process::id",
            });
        }
        if contains_path_call(&compact_line, "std::thread::current(")
            || contains_path_call(&compact_line, "thread::current(")
        {
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
    let mut signature = String::new();
    for line in lines.iter().skip(attribute_index + 1).take(16) {
        if line.trim().is_empty() {
            continue;
        }
        signature.push_str(line);
        signature.push(' ');
        if line_contains_deterministic_seed_type(&signature) {
            return true;
        }
        if line.contains('{') || line.contains(';') {
            return false;
        }
    }

    false
}

fn line_contains_deterministic_seed_type(line: &str) -> bool {
    let mut markers = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if is_identifier_char(ch) {
            let mut ident = String::from(ch);
            while let Some(next) = chars.peek().copied() {
                if is_identifier_char(next) {
                    ident.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            markers.push(SignatureMarker::Ident(ident));
        } else if matches!(ch, '<' | '>') {
            markers.push(SignatureMarker::Punct(ch));
        }
    }

    markers.windows(4).any(|window| {
        matches!(
            window,
            [
                SignatureMarker::Ident(type_name),
                SignatureMarker::Punct('<'),
                SignatureMarker::Ident(seed_name),
                SignatureMarker::Punct('>'),
            ] if type_name == "Deterministic" && seed_name == "Seed"
        )
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SignatureMarker {
    Ident(String),
    Punct(char),
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
                } else if let Some(line) = lines.last_mut() {
                    line.push(ch);
                    index += 1;
                } else {
                    lines.push(ch.to_string());
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

fn compact_source_line(line: &str) -> String {
    line.chars().filter(|ch| !ch.is_whitespace()).collect()
}

fn hash_collection_bindings(line: &str, type_name: &str) -> Vec<String> {
    let markers = source_markers(line);
    let mut names = Vec::new();
    for colon_index in markers.iter().enumerate().filter_map(|(index, marker)| {
        (marker == &SourceMarker::Punct(':')
            && !matches!(
                markers.get(index.saturating_sub(1)),
                Some(SourceMarker::Punct(':'))
            )
            && !matches!(markers.get(index + 1), Some(SourceMarker::Punct(':'))))
        .then_some(index)
    }) {
        if !markers_match_hash_collection_type(&markers, colon_index + 1, type_name) {
            continue;
        }
        if let Some(name) = binding_name_before_colon(&markers, colon_index) {
            push_unique_binding(&mut names, name);
        }
    }
    for equals_index in markers
        .iter()
        .enumerate()
        .filter_map(|(index, marker)| (marker == &SourceMarker::Punct('=')).then_some(index))
    {
        if !markers_match_hash_collection_constructor(&markers, equals_index + 1, type_name) {
            continue;
        }
        if let Some(name) = binding_name_before_equals(&markers, equals_index) {
            push_unique_binding(&mut names, name);
        }
    }
    names
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SourceMarker {
    Ident(String),
    Punct(char),
}

fn source_markers(line: &str) -> Vec<SourceMarker> {
    let mut markers = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if is_identifier_char(ch) {
            let mut ident = String::from(ch);
            while let Some(next) = chars.peek().copied() {
                if is_identifier_char(next) {
                    ident.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            markers.push(SourceMarker::Ident(ident));
        } else if matches!(
            ch,
            ':' | '<' | '>' | '=' | '.' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | ','
        ) {
            markers.push(SourceMarker::Punct(ch));
        }
    }
    markers
}

fn push_unique_binding(names: &mut Vec<String>, name: String) {
    if !names.iter().any(|existing| existing == &name) {
        names.push(name);
    }
}

fn markers_match_hash_collection_type(
    markers: &[SourceMarker],
    start: usize,
    type_name: &str,
) -> bool {
    let Some(path_end) = hash_collection_type_path_end(markers, start, type_name) else {
        return false;
    };
    matches!(markers.get(path_end), Some(SourceMarker::Punct('<')))
}

fn markers_match_hash_collection_constructor(
    markers: &[SourceMarker],
    start: usize,
    type_name: &str,
) -> bool {
    let Some(path_end) = hash_collection_type_path_end(markers, start, type_name) else {
        return false;
    };
    let Some(method_path_start) = hash_collection_constructor_method_start(markers, path_end)
    else {
        return false;
    };
    matches!(
        ident_at(markers, method_path_start),
        Some("new" | "with_capacity" | "from" | "default")
    )
}

fn hash_collection_constructor_method_start(
    markers: &[SourceMarker],
    path_end: usize,
) -> Option<usize> {
    let mut double_colon_index = path_end;
    if double_colon_at(markers, double_colon_index)
        && matches!(
            markers.get(double_colon_index + 2),
            Some(SourceMarker::Punct('<'))
        )
    {
        double_colon_index = skip_angle_group(markers, double_colon_index + 2)?;
    } else if matches!(
        markers.get(double_colon_index),
        Some(SourceMarker::Punct('<'))
    ) {
        double_colon_index = skip_angle_group(markers, double_colon_index)?;
    }
    double_colon_at(markers, double_colon_index).then_some(double_colon_index + 2)
}

fn skip_angle_group(markers: &[SourceMarker], start: usize) -> Option<usize> {
    let mut depth = 0_usize;
    for (index, marker) in markers.iter().enumerate().skip(start) {
        match marker {
            SourceMarker::Punct('<') => depth += 1,
            SourceMarker::Punct('>') => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn hash_collection_type_path_end(
    markers: &[SourceMarker],
    mut start: usize,
    type_name: &str,
) -> Option<usize> {
    if double_colon_at(markers, start) {
        start += 2;
    }
    if ident_at(markers, start) == Some(type_name) {
        return Some(start + 1);
    }
    if ident_at(markers, start) == Some("std")
        && double_colon_at(markers, start + 1)
        && ident_at(markers, start + 3) == Some("collections")
        && double_colon_at(markers, start + 4)
        && ident_at(markers, start + 6) == Some(type_name)
    {
        return Some(start + 7);
    }
    None
}

fn binding_name_before_colon(markers: &[SourceMarker], colon_index: usize) -> Option<String> {
    let binding = ident_at(markers, colon_index.checked_sub(1)?)?;
    if matches!(binding, "let" | "mut" | "ref" | "_") {
        None
    } else {
        Some(binding.to_owned())
    }
}

fn binding_name_before_equals(markers: &[SourceMarker], equals_index: usize) -> Option<String> {
    let binding_index = equals_index.checked_sub(1)?;
    let binding = ident_at(markers, binding_index)?;
    if binding == "_" {
        return None;
    }

    if binding_index
        .checked_sub(1)
        .and_then(|index| ident_at(markers, index))
        == Some("let")
    {
        return Some(binding.to_owned());
    }

    if binding_index
        .checked_sub(1)
        .and_then(|index| ident_at(markers, index))
        == Some("mut")
        && binding_index
            .checked_sub(2)
            .and_then(|index| ident_at(markers, index))
            == Some("let")
    {
        return Some(binding.to_owned());
    }

    None
}

fn ident_at(markers: &[SourceMarker], index: usize) -> Option<&str> {
    match markers.get(index) {
        Some(SourceMarker::Ident(value)) => Some(value),
        _ => None,
    }
}

fn double_colon_at(markers: &[SourceMarker], index: usize) -> bool {
    matches!(
        (markers.get(index), markers.get(index + 1)),
        (
            Some(SourceMarker::Punct(':')),
            Some(SourceMarker::Punct(':'))
        )
    )
}

fn hash_collection_iteration_call(line: &str, bindings: &[String]) -> bool {
    bindings.iter().any(|binding| {
        ["iter", "keys", "values", "into_iter", "drain"]
            .iter()
            .any(|method| contains_receiver_method_call(line, binding, method))
    })
}

fn hash_collection_direct_iteration_call(line: &str, type_name: &str) -> bool {
    let markers = source_markers(line);
    let mut start = 0;
    while start < markers.len() {
        let Some(path_end) = hash_collection_type_path_end(&markers, start, type_name) else {
            start += 1;
            continue;
        };
        let Some(method_path_start) = hash_collection_constructor_method_start(&markers, path_end)
        else {
            start = path_end.max(start + 1);
            continue;
        };
        if !matches!(
            ident_at(&markers, method_path_start),
            Some("new" | "with_capacity" | "from" | "default")
        ) {
            start = method_path_start + 1;
            continue;
        }
        if constructor_chain_has_iteration(&markers, method_path_start) {
            return true;
        }
        start = method_path_start + 1;
    }

    false
}

fn constructor_chain_has_iteration(markers: &[SourceMarker], method_index: usize) -> bool {
    let Some(mut index) = constructor_invocation_end(markers, method_index) else {
        return false;
    };

    while matches!(markers.get(index), Some(SourceMarker::Punct('.'))) {
        let method_name_index = index + 1;
        if matches!(
            ident_at(markers, method_name_index),
            Some("iter" | "keys" | "values" | "into_iter" | "drain")
        ) {
            return true;
        }

        index = method_name_index + 1;
        if double_colon_at(markers, index)
            && matches!(markers.get(index + 2), Some(SourceMarker::Punct('<')))
        {
            let Some(next_index) = skip_angle_group(markers, index + 2) else {
                return false;
            };
            index = next_index;
        }
        if matches!(markers.get(index), Some(SourceMarker::Punct('('))) {
            let Some(next_index) = skip_balanced_punct_group(markers, index, '(', ')') else {
                return false;
            };
            index = next_index;
        }
    }

    false
}

fn constructor_invocation_end(markers: &[SourceMarker], method_index: usize) -> Option<usize> {
    let mut cursor = method_index + 1;
    if double_colon_at(markers, cursor)
        && matches!(markers.get(cursor + 2), Some(SourceMarker::Punct('<')))
    {
        cursor = skip_angle_group(markers, cursor + 2)?;
    }

    match markers.get(cursor) {
        Some(SourceMarker::Punct('(')) => skip_balanced_punct_group(markers, cursor, '(', ')'),
        _ => None,
    }
}

fn skip_balanced_punct_group(
    markers: &[SourceMarker],
    start: usize,
    open: char,
    close: char,
) -> Option<usize> {
    let mut depth = 0_usize;
    for (index, marker) in markers.iter().enumerate().skip(start) {
        match marker {
            SourceMarker::Punct(ch) if *ch == open => depth += 1,
            SourceMarker::Punct(ch) if *ch == close => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
    }

    None
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
        let has_left_boundary = match previous {
            None => true,
            Some(ch) if !is_identifier_char(ch) && ch != ':' => true,
            Some(':') => &line[..index] == "::",
            Some(_) => false,
        };
        if has_left_boundary {
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
    let mut search_start = 0;
    while let Some(relative_index) = line[search_start..].find("::now(") {
        let now_index = search_start + relative_index;
        let prefix = &line[..now_index];
        let type_name = prefix
            .rsplit(|ch: char| !is_identifier_char(ch))
            .next()
            .unwrap_or_default();

        if type_name.ends_with("Id") {
            return true;
        }
        search_start = now_index + "::now(".len();
    }

    false
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
    use super::{line_has_raw_ee_env_read, render_report, scan_fixture};
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
    fn token_spaced_seeded_required_function_does_not_emit_missing_seed() {
        let fixture = r#"
            #[determinism::required]
            fn seeded(
                _: &ee::runtime::determinism::Deterministic < Seed >,
            ) {}
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert!(!report.contains("missing_seed_param"));
    }

    #[test]
    fn split_seeded_required_function_does_not_emit_missing_seed() {
        let fixture = r#"
            #[determinism::required]
            fn seeded(
                _: &ee::runtime::determinism::Deterministic <
                    Seed
                >,
            ) {}
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert!(!report.contains("missing_seed_param"));
    }

    #[test]
    fn similarly_named_seed_type_still_emits_missing_seed() {
        let fixture = r#"
            #[determinism::required]
            fn seeded(_: &ee::runtime::determinism::NonDeterministic<Seed>) {}

            #[determinism::required]
            fn also_seeded(_: &NotDeterministic<Seed>) {}
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert_eq!(report.matches("missing_seed_param").count(), 2);
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
    fn whitespace_split_ambient_paths_emit_known_violations() {
        let fixture = r#"
            use std::collections::HashMap;

            #[determinism::required]
            fn ambient(_: &ee::runtime::determinism::Deterministic<Seed>) {
                let _ = rand :: random :: < u64 > ();
                let _ = std :: env :: var ("EE_SEED");
                let _ = std :: fs :: read_dir (".");
                let _ = std :: process :: id ();
                let _ = std :: thread :: current ();
                let _ = MemoryId :: now ();
                let map: HashMap<String, String> = HashMap::new();
                for _ in map . iter () {}
            }
        "#;
        let report = render_report(&scan_fixture(fixture));

        assert!(report.contains("ambient_rand_random"));
        assert!(report.contains("ambient_env_var"));
        assert!(report.contains("unsorted_read_dir"));
        assert!(report.contains("ambient_process_id"));
        assert!(report.contains("ambient_thread_current"));
        assert!(report.contains("ambient_domain_id_now"));
        assert!(report.contains("hashmap_iteration"));
    }

    #[test]
    fn longer_qualified_paths_do_not_trigger_exact_path_violations() {
        let fixture = r#"
            #[determinism::required]
            fn ambient(_: &ee::runtime::determinism::Deterministic<Seed>) {
                let _ = fake :: rand :: random ();
                let _ = fake :: getrandom :: fill (&mut []);
                let _ = fake :: std :: env :: var ("EE_SEED");
                let _ = fake :: std :: fs :: read_dir (".");
                let _ = fake :: std :: process :: id ();
                let _ = fake :: std :: thread :: current ();
            }
        "#;
        let report = render_report(&scan_fixture(fixture));

        assert!(!report.contains("ambient_rand_random"));
        assert!(!report.contains("ambient_getrandom_fill"));
        assert!(!report.contains("ambient_env_var"));
        assert!(!report.contains("unsorted_read_dir"));
        assert!(!report.contains("ambient_process_id"));
        assert!(!report.contains("ambient_thread_current"));
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
    fn inferred_hash_collection_constructor_bindings_emit_known_violations() {
        let fixture = r#"
            use std::collections::HashMap;

            #[determinism::required]
            fn ambient(_: &ee::runtime::determinism::Deterministic<Seed>) {
                let mut map = HashMap::new();
                for _ in map.iter() {}
                let mut set = std::collections::HashSet::with_capacity(4);
                for _ in set.drain() {}
                let typed_map = HashMap::<String, String>::default();
                for _ in typed_map.values() {}
            }
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert_eq!(report.matches("hashmap_iteration").count(), 2);
        assert_eq!(report.matches("hashset_iteration").count(), 1);
    }

    #[test]
    fn direct_hash_collection_constructor_chains_emit_known_violations() {
        let fixture = r#"
            use std::collections::HashMap;

            #[determinism::required]
            fn ambient(_: &ee::runtime::determinism::Deterministic<Seed>) {
                for _ in HashMap::<String, String>::from([("a".to_owned(), "b".to_owned())]).iter() {}
                for _ in std::collections::HashSet::<String>::from(["a".to_owned()]).into_iter() {}
                let _ = HashMap::<String, String>::new();
                for _ in unrelated.iter() {}
            }
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert_eq!(report.matches("hashmap_iteration").count(), 1);
        assert_eq!(report.matches("hashset_iteration").count(), 1);
    }

    #[test]
    fn direct_hash_collection_constructor_scan_ignores_argument_iteration() {
        let fixture = r#"
            use std::collections::HashMap;

            #[determinism::required]
            fn ambient(_: &ee::runtime::determinism::Deterministic<Seed>) {
                let _ = HashMap::<String, String>::from(entries.iter()).len();
                consume(std::collections::HashSet::<String>::new(), items.iter());
            }
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert!(
            !report.contains("hashmap_iteration"),
            "constructor argument iteration must not look like HashMap iteration: {report}"
        );
        assert!(
            !report.contains("hashset_iteration"),
            "sibling argument iteration must not look like HashSet iteration: {report}"
        );
    }

    #[test]
    fn hash_collection_binding_scan_ignores_wildcard_typed_parameters() {
        let fixture = r#"
            use std::collections::HashMap;

            #[determinism::required]
            fn ambient(
                _: &ee::runtime::determinism::Deterministic<Seed>,
                previous: Vec<String>,
                _: HashMap<String, String>,
            ) {
                for _ in previous.iter() {}
            }
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert!(
            !report.contains("hashmap_iteration"),
            "wildcard HashMap parameters must not bind an earlier parameter: {report}"
        );
    }

    #[test]
    fn hash_collection_constructor_assignments_without_let_do_not_bind() {
        let fixture = r#"
            use std::collections::HashMap;

            #[determinism::required]
            fn ambient(_: &ee::runtime::determinism::Deterministic<Seed>) {
                field = HashMap::new();
                for _ in field.iter() {}
                let _ = HashMap::new();
                for _ in placeholder.iter() {}
            }
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert!(
            !report.contains("hashmap_iteration"),
            "constructor expressions outside let bindings must not create tracked bindings: {report}"
        );
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
    fn domain_id_now_detection_checks_later_calls_on_same_line() {
        let fixture = r#"
            #[determinism::required]
            fn ambient(_: &ee::runtime::determinism::Deterministic<Seed>) {
                let _ = uuid::Uuid::now_v7(); let _ = MemoryId::now();
            }
        "#;
        let report = render_report(&scan_fixture(fixture));
        assert_eq!(report.matches("ambient_domain_id_now").count(), 1);
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
    fn raw_ee_env_read_detector_accepts_rust_token_spacing() {
        assert!(line_has_raw_ee_env_read(
            r#"let _ = std :: env :: var ("EE_SEED");"#
        ));
        assert!(line_has_raw_ee_env_read(
            r#"let _ = env :: var_os ("EE_SEED");"#
        ));
        assert!(!line_has_raw_ee_env_read(
            r#"let _ = fake :: std :: env :: var ("EE_SEED");"#
        ));
    }

    #[test]
    fn determinism_required_proc_macro_crate_is_present_and_dependency_free() -> Result<(), String>
    {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let manifest_path = root.join("crates/determinism/Cargo.toml");
        let source_path = root.join("crates/determinism/src/lib.rs");
        let manifest = std::fs::read_to_string(&manifest_path)
            .map_err(|error| format!("failed to read {}: {error}", manifest_path.display()))?;
        let source = std::fs::read_to_string(&source_path)
            .map_err(|error| format!("failed to read {}: {error}", source_path.display()))?;

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
        Ok(())
    }
}
