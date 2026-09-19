//! The capture budget limits presentation, never the identity of the evidence.

use super::*;

fn raw(diff: String) -> RememberGitCaptureInput {
    RememberGitCaptureInput {
        mode: RememberGitCaptureMode::Diff,
        reference: Some("HEAD".to_owned()),
        commit_sha: None,
        commit_subject: None,
        commit_body: None,
        changed_files: vec!["src/change.rs".to_owned()],
        diff_text: diff,
    }
}

fn digest(text: &str) -> String {
    format!("blake3:{}", blake3::hash(text.as_bytes()).to_hex())
}

#[test]
fn different_omitted_tails_never_alias_the_captured_source() {
    let prefix = format!(
        "+{}\n",
        "x".repeat(REMEMBER_GIT_CAPTURE_DIFF_MAX_BYTES + 100)
    );
    let left = raw(format!("{prefix}+pub fn tail_alpha() {{}}\n"));
    let right = raw(format!("{prefix}+pub fn tail_omega() {{}}\n"));
    assert_eq!(
        git_capture_diff_view(&left.diff_text).visible,
        git_capture_diff_view(&right.diff_text).visible,
    );
    let first = build_remember_git_capture_candidate(&left);
    let second = build_remember_git_capture_candidate(&right);
    assert_eq!(first.diff_fingerprint, digest(&left.diff_text));
    assert_eq!(second.diff_fingerprint, digest(&right.diff_text));
    assert_ne!(first.diff_fingerprint, second.diff_fingerprint);
    assert_ne!(first.source, second.source);
    assert_ne!(
        first.content, second.content,
        "deduplication must see the changed evidence"
    );
    assert!(!first.content.contains("tail_alpha"));
    assert!(!second.content.contains("tail_omega"));
    assert!(first.changed_symbols.is_empty());
    assert!(second.changed_symbols.is_empty());
    assert!(
        first
            .content
            .contains("fingerprint covers the complete sanitized comparison")
    );
}

#[test]
fn omitted_credentials_are_scrubbed_before_full_evidence_identity_is_computed() {
    let prefix = format!(
        "+{}\n",
        "x".repeat(REMEMBER_GIT_CAPTURE_DIFF_MAX_BYTES + 100)
    );
    let first_value = format!("sk-proj-{}", "q".repeat(44));
    let second_value = format!("sk-proj-{}", "r".repeat(44));
    let first_raw = raw(format!("{prefix}+let value = \"{first_value}\";\n"));
    let second_raw = raw(format!("{prefix}+let value = \"{second_value}\";\n"));
    let sanitized = redact_git_capture_text(&first_raw.diff_text).content;
    assert_eq!(
        sanitized,
        redact_git_capture_text(&second_raw.diff_text).content
    );
    let first = build_remember_git_capture_candidate(&first_raw);
    let second = build_remember_git_capture_candidate(&second_raw);
    assert!(first.redacted && second.redacted);
    assert_eq!(first.diff_fingerprint, digest(&sanitized));
    assert_eq!(first.diff_fingerprint, second.diff_fingerprint);
    assert_eq!(first.source, second.source);
    assert_ne!(first.diff_fingerprint, digest(&first_raw.diff_text));
    for candidate in [first, second] {
        assert!(!format!("{candidate:?}").contains(&first_value));
        assert!(!format!("{candidate:?}").contains(&second_value));
        assert!(!crate::policy::redact_secret_like_content(&candidate.content).redacted);
    }
}

#[test]
fn byte_truncation_never_turns_a_partial_declaration_into_an_anchor() {
    let prefix = "+pub fn complete_before() {}\n";
    let diff = format!(
        "{prefix}+pub fn {}() {{}}\n",
        "n".repeat(REMEMBER_GIT_CAPTURE_DIFF_MAX_BYTES)
    );
    let view = git_capture_diff_view(&diff);
    assert!(view.truncated);
    assert_eq!(view.complete_lines, prefix);
    assert!(view.visible.contains("+pub fn n"));
    let candidate = build_remember_git_capture_candidate(&raw(diff));
    assert_eq!(candidate.changed_symbols, ["complete_before"]);
    assert!(!candidate.content.contains("ee-anchor:symbol:n"));
}

#[test]
fn line_truncation_withholds_invisible_anchors_but_classifies_the_complete_change() {
    let prefix = "+// ordinary context\n".repeat(REMEMBER_GIT_CAPTURE_DIFF_EXCERPT_LINES);
    let diff = format!("{prefix}+// decision: chosen layout\n+pub fn omitted_declaration() {{}}\n");
    let view = git_capture_diff_view(&diff);
    assert_eq!(view.visible, prefix);
    assert!(view.truncated);
    let candidate = build_remember_git_capture_candidate(&raw(diff.clone()));
    assert!(candidate.changed_symbols.is_empty());
    assert!(!candidate.content.contains("omitted_declaration"));
    assert_eq!(candidate.kind, "decision");
    assert_eq!(candidate.diff_fingerprint, digest(&diff));
    assert!(candidate.content.contains("... [truncated]"));
}

#[test]
fn byte_budget_is_utf8_safe_and_only_complete_visible_lines_can_supply_anchors() {
    let diff = format!(
        "+pub fn visible() {{}}\n+{}\n",
        "日本語".repeat(REMEMBER_GIT_CAPTURE_DIFF_MAX_BYTES)
    );
    let view = git_capture_diff_view(&diff);
    assert!(view.truncated);
    assert!(view.visible.len() <= REMEMBER_GIT_CAPTURE_DIFF_MAX_BYTES);
    assert!(REMEMBER_GIT_CAPTURE_DIFF_MAX_BYTES - view.visible.len() < '日'.len_utf8());
    assert!(diff.is_char_boundary(view.visible.len()));
    assert_eq!(view.complete_lines, "+pub fn visible() {}\n");
    assert_eq!(
        extract_git_capture_symbols(view.complete_lines),
        ["visible"]
    );
}

#[test]
fn exact_byte_and_line_boundaries_do_not_falsely_report_truncation() {
    for diff in [
        "x".repeat(REMEMBER_GIT_CAPTURE_DIFF_MAX_BYTES),
        "+// line\n".repeat(REMEMBER_GIT_CAPTURE_DIFF_EXCERPT_LINES),
        String::new(),
    ] {
        let view = git_capture_diff_view(&diff);
        assert_eq!(view.visible, diff);
        assert_eq!(view.complete_lines, diff);
        assert!(!view.truncated);
    }
}

#[test]
fn short_diff_identity_and_visible_symbol_contract_are_unchanged() {
    let diff = "+pub fn retained() {}\n";
    let candidate = build_remember_git_capture_candidate(&raw(diff.to_owned()));
    assert_eq!(candidate.diff_fingerprint, digest(diff));
    assert_eq!(candidate.changed_symbols, ["retained"]);
    assert!(candidate.content.contains("ee-anchor:symbol:retained"));
    assert!(!candidate.content.contains("[truncated]"));
    assert!(!candidate.redacted);
}
