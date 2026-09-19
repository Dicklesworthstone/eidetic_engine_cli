use super::*;

#[test]
fn git_capture_redaction_catches_every_supported_bearer_prefix_inside_labels() {
    for &(prefix, reason, suffix_length, requires_context) in RAW_TOKEN_PATTERNS {
        let token = format!("{prefix}{}", "q".repeat(suffix_length));
        let context = if requires_context {
            "twilio build result"
        } else {
            "build result"
        };
        let raw = format!("{context}: label-{token}; the release succeeded.");
        let report = redact_git_capture_text(&raw);
        assert!(report.redacted, "prefix {prefix} was not screened");
        assert!(!report.content.contains(&token), "prefix {prefix} leaked");
        assert!(report.content.contains("the release succeeded."));
        assert!(report.redacted_reasons.contains(&reason));
        assert!(
            report.matches.iter().any(|matched| {
                matched.pattern_id == reason && &raw[matched.start..matched.end] == token
            }),
            "original source offsets lost for {prefix}"
        );
    }
}

#[test]
fn git_capture_redaction_preserves_generic_shape_thresholds_and_manual_boundaries() {
    let token = format!("ghp_{}", "q".repeat(36));
    let embedded = format!("label-{token}");
    assert!(!redact_secret_like_content(&embedded).redacted);
    assert!(redact_git_capture_text(&embedded).redacted);
    for (prefix, count) in [("ghp_", 35), ("sk-proj-", 39), ("AKIA", 15)] {
        let raw = format!("label-{prefix}{}", "q".repeat(count));
        assert_eq!(redact_git_capture_text(&raw).content, raw);
    }
    let ordinary = "cancel token; context packs must never include secrets; fn capture_result()";
    assert_eq!(redact_git_capture_text(ordinary).content, ordinary);
}

#[test]
fn git_capture_redaction_preserves_context_required_for_ambiguous_prefixes() {
    let value = format!("label-AC{}", "q".repeat(32));
    assert_eq!(redact_git_capture_text(&value).content, value);
    assert!(redact_git_capture_text(&format!("twilio {value}")).redacted);
}

#[test]
fn git_capture_redaction_offsets_survive_unicode_and_earlier_replacements() {
    let token = format!("ghp_{}", "q".repeat(36));
    let raw = format!("日本語 password=private_fixture; label-{token}; safe result");
    let report = redact_git_capture_text(&raw);
    assert!(!report.content.contains("private_fixture"));
    assert!(!report.content.contains(&token));
    let matching = report
        .matches
        .iter()
        .filter(|matched| {
            matched.pattern_id == "github_token" && &raw[matched.start..matched.end] == token
        })
        .count();
    assert_eq!(
        matching, 1,
        "the same source span must not be counted twice"
    );
    assert!(report.content.contains("日本語"));
    assert!(report.content.contains("safe result"));
}

#[test]
fn git_capture_redaction_is_idempotent_and_preserves_surrounding_text() {
    let raw = format!("label-ghp_{}; the release completed", "q".repeat(36));
    let first = redact_git_capture_text(&raw);
    assert!(first.redacted);
    assert!(first.content.contains("the release completed"));
    let second = redact_git_capture_text(&first.content);
    assert_eq!(second.content, first.content);
    assert!(!second.redacted);
    let safe = redact_git_capture_text("The release verification succeeded.");
    assert!(!safe.redacted);
    assert_eq!(safe.content, "The release verification succeeded.");
}

#[test]
fn git_capture_redaction_removes_whole_bearers_before_partial_pii_replacements() {
    let token = format!("ghp_{}-202-555-0174-{}", "q".repeat(10), "q".repeat(14));
    let raw = format!("label-{token}; keep the incident context");
    // The generic boundary excludes the fused token, but PII screening changes
    // its middle. Running the bearer scanner after that would leak both ends.
    let generic = redact_secret_like_content(&raw);
    assert!(generic.redacted_reasons.contains(&"phone_number"));
    assert!(generic.content.contains(&format!("ghp_{}", "q".repeat(10))));
    let report = redact_git_capture_text(&raw);
    assert!(!report.content.contains(&format!("ghp_{}", "q".repeat(10))));
    assert!(!report.content.contains(&"q".repeat(14)));
    assert!(report.content.contains("keep the incident context"));
    assert!(report.matches.iter().any(|matched| {
        matched.pattern_id == "github_token" && &raw[matched.start..matched.end] == token
    }));
}

#[test]
fn git_capture_redaction_resolves_context_before_removing_other_bearers() {
    let first = format!("ghp_{}-twilio", "q".repeat(36));
    let second = format!("AC{}", "q".repeat(32));
    let raw = format!("label-{first} label-{second}; keep context");
    // The first match contains the only qualifying context for the second.
    // Both decisions must use original text, not sequentially shortened text.
    let report = redact_git_capture_text(&raw);
    assert!(!report.content.contains(&first));
    assert!(!report.content.contains(&second));
    assert!(report.redacted_reasons.contains(&"twilio_account_sid"));
    assert!(report.content.contains("keep context"));
}
