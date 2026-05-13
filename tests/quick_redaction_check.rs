use ee::policy::redact_secret_like_content;
#[test]
fn test_redaction() {
    let report = redact_secret_like_content("Document redacted sample sk-FAKEabc123def456ghi789jkl012.");
    assert!(report.redacted, "It was NOT redacted!");
}
