//! Privacy-preserving export projections must be fixed points under re-backup.
//!
//! Evidence kinds and recognized producer roles are schema vocabulary, not
//! prose. Redacting them makes the next restore uninsertable; rehashing already
//! opaque references changes history on every recovery. Neither operation is
//! needed to remove source text, and neither is allowed to confer admission.

use super::{BackupCassEvidenceRecord, RedactionLevel, hash_bytes, redact_content};

fn canonical_digest(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        suffix.len() == 64
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn opaque_reference(value: &str) -> String {
    if canonical_digest(value, "blake3:") {
        value.to_owned()
    } else {
        hash_bytes(value.as_bytes())
    }
}

/// Identity keys are not prose. Preserve the precise opaque format emitted by
/// this projection, never an arbitrary string merely beginning with `key_`.
pub(super) fn redact_identity(key: &str, level: RedactionLevel) -> String {
    crate::output::jsonl_export::redact_recovery_identity(key, level)
}

pub(super) fn redact_evidence(
    row: &mut BackupCassEvidenceRecord,
    level: RedactionLevel,
    provenance_admitted: bool,
) {
    // An explicit unredacted backup retains its original recovery contract.
    if level == RedactionLevel::None {
        return;
    }
    let excerpt = redact_content(
        &row.excerpt,
        if provenance_admitted {
            level
        } else {
            RedactionLevel::Full
        },
    );
    if !provenance_admitted {
        row.cass_span_id = opaque_reference(&row.cass_span_id);
        row.upstream_ref_hash = row.upstream_ref_hash.as_deref().map(opaque_reference);
        // span_kind is constrained by the DB to message/tool_call/tool_result/
        // file/summary. It must stay structural, even for denied legacy rows.
        // A legacy role, unlike span_kind, can contain arbitrary source text.
        if !matches!(
            row.role.as_deref(),
            None | Some(
                "user"
                    | "assistant"
                    | "system"
                    | "developer"
                    | "tool"
                    | "unknown"
                    | "agentsmd_import"
                    | "docs_bootstrap"
                    | "journal_distill"
                    | "reinforcement"
            )
        ) {
            row.role = None;
        }
    }
    if excerpt != row.excerpt || !provenance_admitted {
        row.excerpt = excerpt;
        row.content_hash = hash_bytes(row.excerpt.as_bytes());
        row.canonical_excerpt_hash = None;
        row.canonical_provenance_revision = 0;
        row.security_policy_epoch = 0;
        row.metadata_json = None;
        row.secret_redaction_status = "redacted".to_owned();
        row.redaction_classes_json = "[\"backup_redaction\"]".to_owned();
        row.search_eligibility = "denied".to_owned();
        row.pack_eligibility = "denied".to_owned();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> BackupCassEvidenceRecord {
        let excerpt = "The release tests completed successfully.";
        BackupCassEvidenceRecord {
            id: "ev_fixture".to_owned(),
            workspace_id: "ws_fixture".to_owned(),
            session_id: "ses_fixture".to_owned(),
            memory_id: None,
            cass_span_id: hash_bytes(b"upstream reference"),
            span_kind: "message".to_owned(),
            start_line: 1,
            end_line: 2,
            start_byte: None,
            end_byte: None,
            role: Some("assistant".to_owned()),
            excerpt: excerpt.to_owned(),
            content_hash: hash_bytes(excerpt.as_bytes()),
            metadata_json: Some("{}".to_owned()),
            producer_kind: "cass_import".to_owned(),
            screening_version: 1,
            secret_redaction_status: "clean".to_owned(),
            redaction_classes_json: "[]".to_owned(),
            instruction_risk: "none".to_owned(),
            search_eligibility: "admitted".to_owned(),
            pack_eligibility: "admitted".to_owned(),
            canonical_provenance_revision: 1,
            canonical_excerpt_hash: Some(hash_bytes(excerpt.as_bytes())),
            security_policy_epoch: 1,
            upstream_ref_hash: Some(hash_bytes(b"upstream reference")),
            created_at: "2026-09-01T00:00:00Z".to_owned(),
            updated_at: "2026-09-01T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn redacted_evidence_is_a_fixed_point_and_never_regains_admission() {
        for level in [RedactionLevel::Paranoid, RedactionLevel::Full] {
            let mut value = row();
            redact_evidence(&mut value, level, true);
            let first = value.clone();
            for next in [level, RedactionLevel::Standard, RedactionLevel::None] {
                redact_evidence(&mut value, next, false);
                assert_eq!(value, first);
                assert_eq!(value.pack_eligibility, "denied");
                assert_eq!(value.search_eligibility, "denied");
                assert!(value.canonical_excerpt_hash.is_none());
                assert_eq!(value.security_policy_epoch, 0);
            }
        }
    }

    #[test]
    fn denied_evidence_preserves_all_schema_kinds_and_recognized_roles() {
        for kind in ["message", "tool_call", "tool_result", "file", "summary"] {
            for role in [
                "user",
                "assistant",
                "system",
                "developer",
                "tool",
                "unknown",
                "agentsmd_import",
                "docs_bootstrap",
                "journal_distill",
                "reinforcement",
            ] {
                let mut value = row();
                value.span_kind = kind.to_owned();
                value.role = Some(role.to_owned());
                redact_evidence(&mut value, RedactionLevel::Full, false);
                assert_eq!(value.span_kind, kind);
                assert_eq!(value.role.as_deref(), Some(role));
                assert_eq!(value.pack_eligibility, "denied");
            }
        }
    }

    #[test]
    fn unchecked_legacy_text_and_references_are_removed_once() {
        for level in [
            RedactionLevel::Minimal,
            RedactionLevel::Standard,
            RedactionLevel::Strict,
            RedactionLevel::Paranoid,
            RedactionLevel::Full,
        ] {
            let mut value = row();
            value.role = Some("api_key=PRIVATE_SENTINEL".to_owned());
            value.cass_span_id = "/Users/private/PRIVATE_SENTINEL".to_owned();
            value.upstream_ref_hash = Some("PRIVATE_SENTINEL".to_owned());
            value.excerpt = "PRIVATE_SENTINEL".to_owned();
            redact_evidence(&mut value, level, false);
            assert_eq!(value.role, None);
            assert_ne!(value.excerpt, "PRIVATE_SENTINEL");
            assert_eq!(
                value.cass_span_id,
                hash_bytes(b"/Users/private/PRIVATE_SENTINEL")
            );
            assert_eq!(
                value.upstream_ref_hash,
                Some(hash_bytes(b"PRIVATE_SENTINEL"))
            );
            assert!(value.metadata_json.is_none());
            let first = value.clone();
            redact_evidence(&mut value, level, false);
            assert_eq!(value, first);
        }
    }

    #[test]
    fn malformed_opaque_lookalikes_are_not_a_redaction_bypass() {
        for value in ["blake3:secret", "key_secret", "blake3:", "key_"] {
            assert_eq!(opaque_reference(value), hash_bytes(value.as_bytes()));
            assert_ne!(redact_identity(value, RedactionLevel::Full), value);
        }
        for suffix in ["a".repeat(63), "a".repeat(65), "G".repeat(64)] {
            assert!(!canonical_digest(&format!("blake3:{suffix}"), "blake3:"));
            assert!(!canonical_digest(&format!("key_{suffix}"), "key_"));
        }
    }

    #[test]
    fn agent_and_task_pseudonyms_survive_recovery_without_aliasing() {
        for key in [
            "RecoveryAgent",
            "/Users/private/task",
            "api_key=PRIVATE_SENTINEL",
        ] {
            let once = redact_identity(key, RedactionLevel::Full);
            assert_ne!(once, key);
            for level in [
                RedactionLevel::None,
                RedactionLevel::Minimal,
                RedactionLevel::Standard,
                RedactionLevel::Strict,
                RedactionLevel::Paranoid,
                RedactionLevel::Full,
            ] {
                assert_eq!(redact_identity(&once, level), once);
            }
        }
        assert_ne!(
            redact_identity("one", RedactionLevel::Full),
            redact_identity("two", RedactionLevel::Full)
        );
    }

    #[test]
    fn unredacted_and_admitted_unchanged_evidence_remain_exact() {
        let original = row();
        for (level, admitted) in [
            (RedactionLevel::None, false),
            (RedactionLevel::None, true),
            (RedactionLevel::Standard, true),
        ] {
            let mut value = original.clone();
            redact_evidence(&mut value, level, admitted);
            assert_eq!(value, original);
        }
    }
}
