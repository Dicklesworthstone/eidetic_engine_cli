//! Screen the complete bounded upstream line before making a durable excerpt.
//!
//! Truncating first can turn a recognized credential into an unrecognized
//! fragment. The retained excerpt is the screened projection, never raw source
//! bytes. If screening happened in an omitted tail, retain an explicit marker
//! so the DB can authenticate inherited redaction classes without inventing a
//! clean-source posture.

use crate::policy::{ExternalIngestionScreenReport, screen_external_text_for_ingestion};

pub(super) const MAX_EXCERPT_BYTES: usize = 65_536;

pub(super) fn screen_excerpt(content: &str) -> ExternalIngestionScreenReport {
    let mut screen = screen_external_text_for_ingestion(content);
    if screen.content.len() > MAX_EXCERPT_BYTES {
        if screen.redacted {
            const OMITTED: &str = "\n[REDACTED:truncated_source]";
            screen.content =
                super::truncate_excerpt(&screen.content, MAX_EXCERPT_BYTES - OMITTED.len());
            screen.content.push_str(OMITTED);
        } else {
            screen.content = super::truncate_excerpt(&screen.content, MAX_EXCERPT_BYTES);
        }
    }
    screen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cass::import::{evidence_input, parse_view_line_value};
    use crate::db::{CreateSessionInput, CreateWorkspaceInput, DbConnection};
    use crate::models::{EvidenceId, SessionId, WorkspaceId};
    use serde_json::json;
    use uuid::Uuid;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn parse(
        content: &str,
    ) -> Result<super::super::CassViewSpanForImport, Box<dyn std::error::Error>> {
        Ok(parse_view_line_value(
            &json!({"line": 7, "content": content}),
            "/tmp/source.jsonl",
        )?)
    }

    #[test]
    fn full_line_screen_removes_credentials_crossing_the_old_cutoff() -> TestResult {
        for (prefix, minimum) in [("sk-proj-", 40), ("ghp_", 36), ("AKIA", 16)] {
            let token = format!("{prefix}{}", "Q".repeat(minimum));
            let lead = "Compilation completed. ";
            // The old excerpt retained prefix + five suffix bytes, below the
            // detector threshold, despite the upstream line holding a full key.
            let before = MAX_EXCERPT_BYTES - prefix.len() - 5;
            let raw = format!(
                "{lead}{}{token} Tail of result.",
                " ".repeat(before - lead.len())
            );
            let old_excerpt = super::super::truncate_excerpt(&raw, MAX_EXCERPT_BYTES);
            assert!(!screen_external_text_for_ingestion(&old_excerpt).redacted);
            let row = parse(&raw)?;
            assert!(row.redacted);
            assert!(row.excerpt.len() <= MAX_EXCERPT_BYTES);
            assert!(!row.excerpt.contains(&format!("{prefix}QQQQQ")));
            assert!(row.excerpt.starts_with(lead));
            assert!(row.excerpt.contains("[REDACTED:"));
            assert_eq!(
                row.content_hash,
                format!("blake3:{}", blake3::hash(row.excerpt.as_bytes()).to_hex())
            );
            assert_eq!(parse(&raw)?, row);
        }
        Ok(())
    }

    #[test]
    fn short_redacted_excerpt_never_retains_raw_credentials() -> TestResult {
        let token = format!("{}{}", "ghp_", "Q".repeat(36));
        let row = parse(&format!("Build succeeded. label-{token} Tests passed."))?;
        assert!(row.redacted);
        assert!(!row.excerpt.contains(&token));
        assert!(row.excerpt.contains("Build succeeded."));
        assert!(row.excerpt.contains("Tests passed."));
        assert_eq!(row.redacted_reasons, ["github_token"]);
        let input = evidence_input("workspace", "session", &row);
        assert_eq!(input.inherited_redaction_classes, row.redacted_reasons);
        assert_eq!(input.excerpt, row.excerpt);
        Ok(())
    }

    #[test]
    fn redaction_in_omitted_tail_is_not_relabelled_clean() -> TestResult {
        let token = format!("{}{}", "ghp_", "Q".repeat(36));
        let raw = format!("{} label-{token}", "Build passed. ".repeat(6000));
        let row = parse(&raw)?;
        assert!(row.redacted);
        assert!(row.excerpt.len() <= MAX_EXCERPT_BYTES);
        assert!(row.excerpt.ends_with("[REDACTED:truncated_source]"));
        assert_eq!(row.redacted_reasons, ["github_token"]);
        assert!(!row.excerpt.contains(&token));
        assert!(!screen_external_text_for_ingestion(&row.excerpt).redacted);
        Ok(())
    }

    #[test]
    fn clean_unicode_excerpts_keep_the_existing_utf8_byte_bound() -> TestResult {
        let short = "Build result: 資料 verified at /workspace/src/main.rs.";
        let row = parse(short)?;
        assert_eq!(row.excerpt, short);
        assert!(!row.redacted);
        let raw = short.repeat(2000);
        let row = parse(&raw)?;
        assert_eq!(
            row.excerpt,
            super::super::truncate_excerpt(&raw, MAX_EXCERPT_BYTES)
        );
        assert!(row.excerpt.len() <= MAX_EXCERPT_BYTES);
        assert!(!row.redacted);
        assert!(row.redacted_reasons.is_empty());
        Ok(())
    }

    #[test]
    fn persisted_excerpt_retains_exact_redaction_class_and_safe_search_document() -> TestResult {
        let db = DbConnection::open_memory()?;
        db.migrate()?;
        let ws = WorkspaceId::from_uuid(Uuid::from_u128(501)).to_string();
        let session_id = SessionId::from_uuid(Uuid::from_u128(502)).to_string();
        let id = EvidenceId::from_uuid(Uuid::from_u128(503)).to_string();
        db.insert_workspace(
            &ws,
            &CreateWorkspaceInput {
                path: "/tmp/cass-excerpts".to_owned(),
                name: None,
            },
        )?;
        db.insert_session(
            &session_id,
            &CreateSessionInput {
                workspace_id: ws.clone(),
                cass_session_id: "upstream-transcript".to_owned(),
                source_path: None,
                agent_name: Some("codex".to_owned()),
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 1,
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(b"session").to_hex()),
                metadata_json: None,
            },
        )?;
        let token = format!("{}{}", "ghp_", "Q".repeat(36));
        let raw = format!("{} label-{token}", "Compilation succeeded. ".repeat(3500));
        let row = parse(&raw)?;
        db.insert_evidence_span(&id, &evidence_input(&ws, &session_id, &row))?;
        let stored = db
            .get_search_admitted_evidence_span(&id, &ws)?
            .ok_or("screened excerpt not admitted")?;
        assert_eq!(stored.excerpt, row.excerpt);
        assert_eq!(stored.content_hash, row.content_hash);
        assert_eq!(stored.secret_redaction_status, "redacted");
        assert_eq!(stored.redaction_classes_json, "[\"github_token\"]");
        let doc = crate::search::evidence_span_to_document(&stored).into_indexable();
        assert!(doc.content.contains("Compilation succeeded."));
        assert!(!doc.content.contains(&token));
        let audit = super::super::cass_redaction_audit_input(&ws, &session_id, &id, &row);
        let audit_details = audit.details.as_deref().ok_or("missing audit details")?;
        assert!(audit_details.contains("github_token"));
        assert!(!audit_details.contains(&token));
        db.close()?;
        Ok(())
    }

    #[test]
    fn truncating_a_structured_record_does_not_create_message_authority() -> TestResult {
        let db = DbConnection::open_memory()?;
        db.migrate()?;
        let ws = WorkspaceId::from_uuid(Uuid::from_u128(601)).to_string();
        let session = SessionId::from_uuid(Uuid::from_u128(602)).to_string();
        let id = EvidenceId::from_uuid(Uuid::from_u128(603)).to_string();
        db.insert_workspace(
            &ws,
            &CreateWorkspaceInput {
                path: "/tmp/cass-truncated-record".to_owned(),
                name: None,
            },
        )?;
        db.insert_session(
            &session,
            &CreateSessionInput {
                workspace_id: ws.clone(),
                cass_session_id: "structured-transcript".to_owned(),
                source_path: None,
                agent_name: None,
                model: None,
                started_at: None,
                ended_at: None,
                message_count: 1,
                token_count: None,
                content_hash: format!("blake3:{}", blake3::hash(b"structured").to_hex()),
                metadata_json: None,
            },
        )?;
        let token = format!("{}{}", "ghp_", "Q".repeat(36));
        let raw = json!({"type": "assistant", "message": {"role": "assistant", "content": format!("{} label-{token}", "Build succeeded. ".repeat(5000))}}).to_string();
        let row = parse(&raw)?;
        assert!(row.redacted);
        assert!(serde_json::from_str::<serde_json::Value>(&row.excerpt).is_err());
        db.insert_evidence_span(&id, &evidence_input(&ws, &session, &row))?;
        let stored = db
            .get_evidence_span(&id)?
            .ok_or("missing quarantined evidence")?;
        assert_eq!(stored.pack_eligibility, "quarantined");
        assert_eq!(stored.search_eligibility, "quarantined");
        assert!(db.get_search_admitted_evidence_span(&id, &ws)?.is_none());
        assert!(!stored.excerpt.contains(&token));
        db.close()?;
        Ok(())
    }
}
