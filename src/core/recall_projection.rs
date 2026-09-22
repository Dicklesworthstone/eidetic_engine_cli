//! Redaction-safe recall text before previewing and token-budget selection.
//!
//! This projection never rewrites stored content or claims a redacted source
//! URI is the original. When an origin is private, emit the admitted memory's
//! native locator, explicitly labeled as identity rather than source evidence.

use super::super::{RecallCandidateRow, RecallProvenanceRef};
use crate::db::StoredMemory;

pub(super) fn locator_is_public(value: &str) -> bool {
    !crate::policy::redact_public_replay_text(value).redacted
}

fn text(value: &str, changed: &mut bool) -> String {
    let report = crate::policy::redact_public_replay_text(value);
    *changed |= report.redacted;
    report.content
}

fn provenance(id: &str, uri: Option<&str>, changed: &mut bool) -> Vec<RecallProvenanceRef> {
    let Some(uri) = uri else {
        return Vec::new();
    };
    // Scheme delimiters can hide a leading absolute path from a prose scanner.
    // Inspect the opaque locator as well, and reuse pack's URI/path redactor.
    let locator = uri.split_once("://").map_or(uri, |(_, body)| body);
    if !locator_is_public(uri)
        || !locator_is_public(locator)
        || crate::pack::redact_pack_provenance_text(uri) != uri
    {
        *changed = true;
        return vec![RecallProvenanceRef {
            uri: format!("ee-mem://{id}"),
            source_type: "memory_identity".to_owned(),
        }];
    }
    vec![RecallProvenanceRef {
        uri: uri.to_owned(),
        source_type: "memory_provenance".to_owned(),
    }]
}

pub(super) fn apply(row: &mut RecallCandidateRow, source: &StoredMemory, tags: &[String]) -> bool {
    let mut changed = false;
    row.content = text(&source.content, &mut changed);
    row.tags = tags.iter().map(|tag| text(tag, &mut changed)).collect();
    row.provenance = provenance(
        &row.memory_id,
        source.provenance_uri.as_deref(),
        &mut changed,
    );
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMORY: &str = "mem_00000000000000000000000701";

    #[test]
    fn safe_origins_preserve_the_existing_recall_contract() {
        for uri in [
            "manual://release-guide",
            "test://recall-golden",
            "https://example.org/docs#api",
        ] {
            let mut changed = false;
            let result = provenance(MEMORY, Some(uri), &mut changed);
            assert_eq!(result[0].uri, uri);
            assert_eq!(result[0].source_type, "memory_provenance");
            assert!(!changed);
        }
        let mut changed = false;
        assert!(provenance(MEMORY, None, &mut changed).is_empty());
        assert!(!changed, "absent provenance is not invented");
    }

    #[test]
    fn private_origins_fall_back_to_honestly_labeled_native_identity() {
        let token = ["AKIA", "ABCDEFGHIJKLMNOP"].concat();
        for uri in [
            "file:///custom-private-root/release.txt#L1".to_owned(),
            "cass-session:///home/private/session.jsonl#L2-L8".to_owned(),
            "file://C:\\private\\notes.txt".to_owned(),
            format!("manual://source-{token}"),
        ] {
            let mut changed = false;
            let result = provenance(MEMORY, Some(&uri), &mut changed);
            assert_eq!(result[0].uri, format!("ee-mem://{MEMORY}"));
            assert_eq!(result[0].source_type, "memory_identity");
            assert!(changed, "{uri}");
        }
    }

    #[test]
    fn code_locators_are_not_redacted_into_phantom_paths() {
        assert!(locator_is_public("src/café.rs"));
        assert!(locator_is_public("Release::publish"));
        assert!(!locator_is_public("/home/private/project.rs"));
        assert!(!locator_is_public(&format!(
            "src/{}{}.rs",
            "AKIA", "ABCDEFGHIJKLMNOP"
        )));
    }

    #[test]
    fn complete_text_is_screened_before_a_preview_can_cut_off_a_credential() {
        let token = ["AKIA", "ABCDEFGHIJKLMNOP"].concat();
        let mut changed = false;
        let raw = format!("{}prefix-{token}", "word ".repeat(45));
        let safe = text(&raw, &mut changed);
        assert!(changed);
        assert!(!safe.contains(&token));
        assert!(!super::super::super::recall_content_preview(&safe).contains("AKIA"));
    }
}
