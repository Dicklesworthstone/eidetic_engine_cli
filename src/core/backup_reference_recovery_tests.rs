#[test]
fn opaque_external_references_survive_rebackup_without_exempting_private_text() {
    use std::collections::{BTreeMap, BTreeSet};
    let memory_ids = BTreeMap::new();
    let references = BTreeSet::new();
    let redact = |text: &str, level| {
        crate::core::backup::redact_learning_reference(text, level, &memory_ids, &references)
    };
    let first = redact("untracked private target", RedactionLevel::Full);
    assert!(first.starts_with("backup-ref:"));
    assert_ne!(first, "untracked private target");
    for level in [RedactionLevel::None, RedactionLevel::Minimal, RedactionLevel::Standard,
        RedactionLevel::Strict, RedactionLevel::Paranoid, RedactionLevel::Full] {
        assert_eq!(redact(&first, level), first);
    }
    assert_ne!(first, redact("another private target", RedactionLevel::Full));
    for text in ["backup-ref:PRIVATE_SENTINEL".to_owned(),
        format!("backup-ref:{}", "a".repeat(63)),
        format!("backup-ref:{}", "a".repeat(65)),
        format!("backup-ref:{}", "G".repeat(64))] {
        assert_ne!(redact(&text, RedactionLevel::Full), text);
    }
}

use crate::models::RedactionLevel;
