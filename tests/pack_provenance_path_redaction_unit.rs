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

type TestResult = Result<(), String>;

fn fixture_uri(raw: &str) -> Result<ProvenanceUri, String> {
    ProvenanceUri::from_str(raw).map_err(|err| format!("fixture URI should parse: {err}"))
}

fn provenance_with_note(note: &str) -> Result<PackProvenance, String> {
    let uri = fixture_uri("file://src/lib.rs#L1")?;
    PackProvenance::new(uri, note).map_err(|err| format!("fixture note should be valid: {err}"))
}

fn provenance_value(provenance: PackProvenance) -> Result<Value, String> {
    let json = pack_item_provenance_json(&[provenance]);
    serde_json::from_str(&json)
        .map_err(|err| format!("provenance JSON should parse: {err}; json={json}"))
}

fn redacted_note(note: &str) -> Result<String, String> {
    let value = provenance_value(provenance_with_note(note)?)?;
    let note = value["entries"][0]["note"]
        .as_str()
        .ok_or_else(|| format!("note field should be a string: {value}"))?;
    Ok(note.to_owned())
}

#[test]
fn redacts_home_prefix() -> TestResult {
    let out = redacted_note("crash log at /home/alice/secret.txt next")?;
    assert!(
        out.contains("[REDACTED_PATH]"),
        "expected placeholder, got: {out}"
    );
    assert!(
        !out.contains("/home/alice/secret.txt"),
        "/home/ path leaked: {out}"
    );
    Ok(())
}

#[test]
fn redacts_users_prefix() -> TestResult {
    let out = redacted_note("loaded /Users/bob/Library/keychain end")?;
    assert!(out.contains("[REDACTED_PATH]"));
    assert!(!out.contains("/Users/bob/Library/keychain"));
    Ok(())
}

#[test]
fn redacts_data_prefix() -> TestResult {
    let out = redacted_note("see /data/projects/secret/run.log here")?;
    assert!(out.contains("[REDACTED_PATH]"));
    assert!(!out.contains("/data/projects/secret/run.log"));
    Ok(())
}

#[test]
fn redacts_workspace_prefix() -> TestResult {
    let out = redacted_note("CI ran /workspace/build/cache/blob.bin done")?;
    assert!(out.contains("[REDACTED_PATH]"));
    assert!(!out.contains("/workspace/build/cache/blob.bin"));
    Ok(())
}

#[test]
fn redacts_volumes_prefix_for_macos_mounts() -> TestResult {
    let out = redacted_note("staged on /Volumes/Backup/secrets.kdbx today")?;
    assert!(out.contains("[REDACTED_PATH]"));
    assert!(!out.contains("/Volumes/Backup/secrets.kdbx"));
    Ok(())
}

#[test]
fn redacts_windows_c_drive_prefix() -> TestResult {
    let out = redacted_note("dumped C:\\Users\\carol\\AppData\\creds.txt now")?;
    assert!(out.contains("[REDACTED_PATH]"));
    assert!(!out.contains("C:\\Users\\carol\\AppData\\creds.txt"));
    Ok(())
}

#[test]
fn redacts_windows_d_drive_prefix() -> TestResult {
    let out = redacted_note("dumped D:\\opt\\secret\\token.dat now")?;
    assert!(out.contains("[REDACTED_PATH]"));
    assert!(!out.contains("D:\\opt\\secret\\token.dat"));
    Ok(())
}

#[test]
fn redacts_multiple_paths_in_one_note() -> TestResult {
    let out = redacted_note("from /home/a/x.log to /Users/b/y.log via /data/c/z.log")?;
    let placeholder_count = out.matches("[REDACTED_PATH]").count();
    assert_eq!(
        placeholder_count, 3,
        "expected 3 placeholders, got {placeholder_count}: {out}"
    );
    assert!(!out.contains("/home/a/x.log"));
    assert!(!out.contains("/Users/b/y.log"));
    assert!(!out.contains("/data/c/z.log"));
    Ok(())
}

#[test]
fn redaction_window_ends_at_quote_brackets_and_punctuation() -> TestResult {
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
        let out = redacted_note(input)?;
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
    Ok(())
}

#[test]
fn redaction_window_ends_at_whitespace_variants() -> TestResult {
    for (delim_name, delim) in [
        ("space", " "),
        ("tab", "\t"),
        ("newline", "\n"),
        ("crlf-cr", "\r"),
    ] {
        let input = format!("path /Users/a/b{delim}tail");
        let out = redacted_note(&input)?;
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
    Ok(())
}

#[test]
fn path_at_end_of_string_is_redacted_without_terminator() -> TestResult {
    let out = redacted_note("trailing path /home/zoe/log.txt")?;
    assert!(out.contains("[REDACTED_PATH]"));
    assert!(!out.contains("/home/zoe/log.txt"));
    assert!(out.ends_with("[REDACTED_PATH]"), "got: {out}");
    Ok(())
}

#[test]
fn prefix_only_input_collapses_to_placeholder() -> TestResult {
    // The walker enters the inner loop and finds no characters before EOF,
    // so the entire prefix becomes the placeholder with no trailing bytes.
    let out = redacted_note("/Users/")?;
    assert_eq!(out, "[REDACTED_PATH]");
    Ok(())
}

#[test]
fn multibyte_chars_after_prefix_advance_cursor_by_utf8_len() -> TestResult {
    // After the prefix, the loop uses len_utf8() to advance — multi-byte
    // characters must not slice the path mid-codepoint and must still be
    // bounded by the next terminator (a `,` here).
    let out = redacted_note("trace /Users/π/λ/データ, then continue")?;
    assert!(out.contains("[REDACTED_PATH]"));
    assert!(!out.contains("/Users/π/λ/データ"));
    assert!(out.contains(", then continue"));
    Ok(())
}

#[test]
fn non_path_text_is_passed_through_untouched() -> TestResult {
    // No prefix anywhere → cursor walks the input character by character and
    // emits each byte verbatim. We need a fixture that has no secret-like
    // tokens either (the upstream secret redactor handles those separately).
    let out = redacted_note("ordinary note without secrets or absolute paths")?;
    assert_eq!(out, "ordinary note without secrets or absolute paths");
    Ok(())
}

#[test]
fn relative_path_starting_with_users_segment_is_not_redacted() -> TestResult {
    // The walker checks `starts_with` against the current cursor position.
    // A bare `Users/...` segment (no leading slash) should not match the
    // `/Users/` prefix.
    let out = redacted_note("relative ref Users/alice/file.txt here")?;
    assert!(
        !out.contains("[REDACTED_PATH]"),
        "relative path was wrongly redacted: {out}"
    );
    assert!(out.contains("Users/alice/file.txt"));
    Ok(())
}

#[test]
fn embedded_path_inside_word_is_still_detected() -> TestResult {
    // The walker advances character-by-character when no prefix matches at the
    // current cursor — so a path embedded after non-whitespace context still
    // triggers the redactor when the cursor lands on the leading slash.
    let out = redacted_note("see-also/data/cache/build.log here")?;
    // The cursor reaches `/data/` after walking past `see-also`, so the path
    // is redacted but the surrounding word is preserved.
    assert!(out.contains("[REDACTED_PATH]"));
    assert!(!out.contains("/data/cache/build.log"));
    assert!(out.starts_with("see-also[REDACTED_PATH]"), "got: {out}");
    Ok(())
}

#[test]
fn file_provenance_with_screaming_snake_filename_uses_path_placeholder_not_secret_placeholder()
-> TestResult {
    let uri = fixture_uri(
        "file:///Users/jemanuel/projects/eidetic_engine_cli/CLOSE_THE_GAP_PLAN.md#L1186-1193",
    )?;
    let value = provenance_value(
        PackProvenance::new(uri, "source evidence")
            .map_err(|err| format!("fixture provenance should be valid: {err}"))?,
    )?;
    let rendered = value.to_string();

    assert_eq!(
        value["entries"][0]["uri"].as_str(),
        Some("file://[REDACTED_PATH]#L1186-1193")
    );
    assert!(
        !rendered.contains("[REDACTED:high_entropy_secret]"),
        "high-entropy placeholder leaked into provenance: {rendered}"
    );
    assert!(
        !rendered.contains("CLOSE_THE_GAP_PLAN"),
        "raw local filename leaked into provenance: {rendered}"
    );
    Ok(())
}

#[test]
fn file_provenance_still_redacts_embedded_secret_query_values() -> TestResult {
    let uri = fixture_uri(
        "file:///Users/jemanuel/projects/eidetic_engine_cli/CLOSE_THE_GAP_PLAN.md?token=AbCDefGhIjKlMnOpQrStUvWxYz0123456789abCDefGhIj#L1186",
    )?;
    let value = provenance_value(
        PackProvenance::new(uri, "source evidence")
            .map_err(|err| format!("fixture provenance should be valid: {err}"))?,
    )?;
    let rendered = value.to_string();

    assert_eq!(
        value["entries"][0]["uri"].as_str(),
        Some("file://[REDACTED_PATH]?token=[REDACTED:token]#L1186")
    );
    assert!(
        !rendered.contains("AbCDefGhIjKlMnOpQrStUvWxYz0123456789abCDefGhIj"),
        "raw query token leaked into provenance: {rendered}"
    );
    assert!(
        !rendered.contains("[REDACTED:high_entropy_secret]"),
        "high-entropy placeholder leaked into token provenance: {rendered}"
    );
    Ok(())
}
