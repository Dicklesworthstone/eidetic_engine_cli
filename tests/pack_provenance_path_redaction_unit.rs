//! Coverage for `src/pack/mod.rs::redact_pack_absolute_path_like_segments`.
//!
//! That helper scrubs absolute paths from pack-provenance text before items
//! leave the agent surface. It recognizes seven prefix forms and ends the
//! redaction window at any of thirteen terminator characters (plus whitespace).
//! Existing coverage only exercised the `/Users/` arm via
//! `pack_item_provenance_json_redacts_sensitive_sources`. This file exercises
//! the remaining prefixes, terminator characters, and walking edge cases via
//! the public `pack_item_provenance_json` entry point.
//!
//! Tracked under bd-2vymd.
//
// The helper itself is private, so we drive it indirectly: the redactor runs
// over both `uri` and `note` for each entry. We embed each fixture path in the
// note (which accepts arbitrary text after a non-empty trim check) and assert
// that the rendered JSON contains the placeholder rather than the raw path.

use std::str::FromStr;

use ee::models::ProvenanceUri;
use ee::pack::{PackProvenance, pack_item_provenance_json};
use serde_json::Value;

fn provenance_with_note(note: &str) -> PackProvenance {
    let uri = ProvenanceUri::from_str("file://src/lib.rs#L1").expect("fixture URI parses");
    PackProvenance::new(uri, note).expect("fixture note is non-empty")
}

fn redacted_note(note: &str) -> String {
    let json = pack_item_provenance_json(&[provenance_with_note(note)]);
    let value: Value = serde_json::from_str(&json).expect("provenance JSON parses");
    value["entries"][0]["note"]
        .as_str()
        .expect("note is a string")
        .to_owned()
}

#[test]
fn redacts_home_prefix() {
    let out = redacted_note("crash log at /home/alice/secret.txt next");
    assert!(
        out.contains("[REDACTED_PATH]"),
        "expected placeholder, got: {out}"
    );
    assert!(
        !out.contains("/home/alice/secret.txt"),
        "/home/ path leaked: {out}"
    );
}

#[test]
fn redacts_users_prefix() {
    let out = redacted_note("loaded /Users/bob/Library/keychain end");
    assert!(out.contains("[REDACTED_PATH]"));
    assert!(!out.contains("/Users/bob/Library/keychain"));
}

#[test]
fn redacts_data_prefix() {
    let out = redacted_note("see /data/projects/secret/run.log here");
    assert!(out.contains("[REDACTED_PATH]"));
    assert!(!out.contains("/data/projects/secret/run.log"));
}

#[test]
fn redacts_workspace_prefix() {
    let out = redacted_note("CI ran /workspace/build/cache/blob.bin done");
    assert!(out.contains("[REDACTED_PATH]"));
    assert!(!out.contains("/workspace/build/cache/blob.bin"));
}

#[test]
fn redacts_volumes_prefix_for_macos_mounts() {
    let out = redacted_note("staged on /Volumes/Backup/secrets.kdbx today");
    assert!(out.contains("[REDACTED_PATH]"));
    assert!(!out.contains("/Volumes/Backup/secrets.kdbx"));
}

#[test]
fn redacts_windows_c_drive_prefix() {
    let out = redacted_note("dumped C:\\Users\\carol\\AppData\\creds.txt now");
    assert!(out.contains("[REDACTED_PATH]"));
    assert!(!out.contains("C:\\Users\\carol\\AppData\\creds.txt"));
}

#[test]
fn redacts_windows_d_drive_prefix() {
    let out = redacted_note("dumped D:\\opt\\secret\\token.dat now");
    assert!(out.contains("[REDACTED_PATH]"));
    assert!(!out.contains("D:\\opt\\secret\\token.dat"));
}

#[test]
fn redacts_multiple_paths_in_one_note() {
    let out = redacted_note("from /home/a/x.log to /Users/b/y.log via /data/c/z.log");
    let placeholder_count = out.matches("[REDACTED_PATH]").count();
    assert_eq!(
        placeholder_count, 3,
        "expected 3 placeholders, got {placeholder_count}: {out}"
    );
    assert!(!out.contains("/home/a/x.log"));
    assert!(!out.contains("/Users/b/y.log"));
    assert!(!out.contains("/data/c/z.log"));
}

#[test]
fn redaction_window_ends_at_quote_brackets_and_punctuation() {
    // The match arm enumerates `" ' \` < > ) ] } , ; | ? #` plus whitespace.
    // Use a delimiter cocktail so each terminator is exercised at least once
    // and the surrounding context is preserved verbatim around the placeholder.
    let cases = [
        ("path \"/Users/a/b\" rest", "\" rest"),
        ("path '/Users/a/b' rest", "' rest"),
        ("path `/Users/a/b` rest", "` rest"),
        ("path </Users/a/b> rest", "> rest"),
        ("path (/Users/a/b) rest", ") rest"),
        ("path [/Users/a/b] rest", "] rest"),
        ("path {/Users/a/b} rest", "} rest"),
        ("path /Users/a/b, rest", ", rest"),
        ("path /Users/a/b; rest", "; rest"),
        ("path /Users/a/b| rest", "| rest"),
        ("path /Users/a/b? rest", "? rest"),
        ("path /Users/a/b#frag rest", "#frag rest"),
    ];
    for (input, trailing) in cases {
        let out = redacted_note(input);
        assert!(
            out.contains("[REDACTED_PATH]"),
            "no placeholder for input `{input}`, got `{out}`"
        );
        assert!(
            !out.contains("/Users/a/b"),
            "raw path leaked for input `{input}`: `{out}`"
        );
        assert!(
            out.contains(trailing),
            "terminator/trailing `{trailing}` not preserved for input `{input}`: `{out}`"
        );
    }
}

#[test]
fn redaction_window_ends_at_whitespace_variants() {
    for (delim_name, delim) in [
        ("space", " "),
        ("tab", "\t"),
        ("newline", "\n"),
        ("crlf-cr", "\r"),
    ] {
        let input = format!("path /Users/a/b{delim}tail");
        let out = redacted_note(&input);
        assert!(
            out.contains("[REDACTED_PATH]"),
            "no placeholder for {delim_name} delimiter: `{out}`"
        );
        assert!(
            !out.contains("/Users/a/b"),
            "raw path leaked for {delim_name} delimiter: `{out}`"
        );
        assert!(
            out.contains("tail"),
            "trailing token lost for {delim_name} delimiter: `{out}`"
        );
    }
}

#[test]
fn path_at_end_of_string_is_redacted_without_terminator() {
    let out = redacted_note("trailing path /home/zoe/log.txt");
    assert!(out.contains("[REDACTED_PATH]"));
    assert!(!out.contains("/home/zoe/log.txt"));
    assert!(out.ends_with("[REDACTED_PATH]"), "got: {out}");
}

#[test]
fn prefix_only_input_collapses_to_placeholder() {
    // The walker enters the inner loop and finds no characters before EOF,
    // so the entire prefix becomes the placeholder with no trailing bytes.
    let out = redacted_note("/Users/");
    assert_eq!(out, "[REDACTED_PATH]");
}

#[test]
fn multibyte_chars_after_prefix_advance_cursor_by_utf8_len() {
    // After the prefix, the loop uses len_utf8() to advance — multi-byte
    // characters must not slice the path mid-codepoint and must still be
    // bounded by the next terminator (a `,` here).
    let out = redacted_note("trace /Users/π/λ/データ, then continue");
    assert!(out.contains("[REDACTED_PATH]"));
    assert!(!out.contains("/Users/π/λ/データ"));
    assert!(out.contains(", then continue"));
}

#[test]
fn non_path_text_is_passed_through_untouched() {
    // No prefix anywhere → cursor walks the input character by character and
    // emits each byte verbatim. We need a fixture that has no secret-like
    // tokens either (the upstream secret redactor handles those separately).
    let out = redacted_note("ordinary note without secrets or absolute paths");
    assert_eq!(out, "ordinary note without secrets or absolute paths");
}

#[test]
fn relative_path_starting_with_users_segment_is_not_redacted() {
    // The walker checks `starts_with` against the current cursor position.
    // A bare `Users/...` segment (no leading slash) should not match the
    // `/Users/` prefix.
    let out = redacted_note("relative ref Users/alice/file.txt here");
    assert!(
        !out.contains("[REDACTED_PATH]"),
        "relative path was wrongly redacted: {out}"
    );
    assert!(out.contains("Users/alice/file.txt"));
}

#[test]
fn embedded_path_inside_word_is_still_detected() {
    // The walker advances character-by-character when no prefix matches at the
    // current cursor — so a path embedded after non-whitespace context still
    // triggers the redactor when the cursor lands on the leading slash.
    let out = redacted_note("see-also/data/cache/build.log here");
    // The cursor reaches `/data/` after walking past `see-also`, so the path
    // is redacted but the surrounding word is preserved.
    assert!(out.contains("[REDACTED_PATH]"));
    assert!(!out.contains("/data/cache/build.log"));
    assert!(out.starts_with("see-also[REDACTED_PATH]"), "got: {out}");
}
