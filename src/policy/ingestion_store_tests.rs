//! Use real FrankenSQLite and the production evidence admission/projection.
use super::*;
use crate::db::{
    CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput, DbConnection,
    EvidenceProducerKind,
};
use crate::models::{EvidenceId, SessionId, WorkspaceId};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn hash(text: &str) -> String {
    format!("blake3:{}", blake3::hash(text.as_bytes()).to_hex())
}

fn database() -> Result<(DbConnection, String, String), Box<dyn std::error::Error>> {
    let db = DbConnection::open_memory()?;
    db.migrate()?;
    let ws = WorkspaceId::from_uuid(Uuid::from_u128(100)).to_string();
    let session = SessionId::from_uuid(Uuid::from_u128(101)).to_string();
    db.insert_workspace(
        &ws,
        &CreateWorkspaceInput {
            path: "/tmp/external-ingestion-tests".to_owned(),
            name: None,
        },
    )?;
    db.insert_session(
        &session,
        &CreateSessionInput {
            workspace_id: ws.clone(),
            cass_session_id: "test-transcript".to_owned(),
            source_path: None,
            agent_name: Some("codex".to_owned()),
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 1,
            token_count: None,
            content_hash: hash("transcript"),
            metadata_json: None,
        },
    )?;
    Ok((db, ws, session))
}

fn input(ws: &str, session: &str, content: &str) -> CreateEvidenceSpanInput {
    CreateEvidenceSpanInput {
        workspace_id: ws.to_owned(),
        session_id: session.to_owned(),
        memory_id: None,
        producer_kind: EvidenceProducerKind::CassImport,
        cass_span_id: "transcript-line".to_owned(),
        span_kind: "message".to_owned(),
        start_line: 1,
        end_line: 1,
        start_byte: None,
        end_byte: None,
        role: Some("assistant".to_owned()),
        excerpt: content.to_owned(),
        content_hash: hash(content),
        metadata_json: None,
        inherited_redaction_classes: Vec::new(),
    }
}

#[test]
fn database_preserves_useful_evidence_without_any_provider_credential() -> TestResult {
    let (db, ws, session_id) = database()?;
    for (index, &(prefix, reason, minimum, contextual)) in
        crate::policy::RAW_TOKEN_PATTERNS.iter().enumerate()
    {
        let token = format!("{prefix}{}", "Q".repeat(minimum));
        let context = if contextual {
            "Twilio account SID: "
        } else {
            ""
        };
        let content = format!("Compilation succeeded. {context}label-{token} Tests passed.");
        let id = EvidenceId::from_uuid(Uuid::from_u128(200 + index as u128)).to_string();
        let mut row = input(&ws, &session_id, &content);
        row.cass_span_id = format!("transcript-line-{index}");
        db.insert_evidence_span(&id, &row)?;
        let admitted = db
            .get_search_admitted_evidence_span(&id, &ws)?
            .ok_or("screened evidence not admitted")?;
        assert_eq!(admitted.secret_redaction_status, "redacted");
        assert!(admitted.redaction_classes_json.contains(reason));
        assert!(!admitted.excerpt.contains(&token));
        assert_eq!(admitted.content_hash, hash(&admitted.excerpt));
        assert_eq!(
            admitted.canonical_excerpt_hash.as_deref(),
            Some(admitted.content_hash.as_str())
        );
        let document = crate::search::evidence_span_to_document(&admitted).into_indexable();
        assert!(document.content.contains("Compilation succeeded."));
        assert!(document.content.contains("Tests passed."));
        assert!(!document.content.contains(&token));
        assert!(!serde_json::to_string(&document.metadata)?.contains(&token));
    }
    db.close()?;
    Ok(())
}

#[test]
fn old_self_consistent_evidence_cannot_bypass_live_rescreening() -> TestResult {
    let (db, ws, session_id) = database()?;
    let id = EvidenceId::from_uuid(Uuid::from_u128(300)).to_string();
    db.insert_evidence_span(&id, &input(&ws, &session_id, "Compilation succeeded."))?;
    let mut row = db.get_evidence_span(&id)?.ok_or("missing evidence")?;
    let session = db.get_session(&session_id)?.ok_or("missing session")?;
    assert!(row.is_search_admitted_for_session(&ws, &session));
    let token = format!("{}{}", "AKIA", "Q".repeat(16));
    row.excerpt = format!("Compilation succeeded. label-{token}");
    assert!(!redact_secret_like_content(&row.excerpt).redacted);
    row.content_hash = hash(&row.excerpt);
    row.canonical_excerpt_hash = Some(row.content_hash.clone());
    let mut metadata: serde_json::Value =
        serde_json::from_str(row.metadata_json.as_deref().ok_or("missing metadata")?)?;
    metadata["canonicalExcerptHash"] = row.content_hash.clone().into();
    row.metadata_json = Some(serde_json::to_string(&metadata)?);
    // Hashes, producer, session, policy markers and the clean posture remain
    // internally consistent, exactly like a row admitted by the former policy.
    assert!(!row.is_search_admitted_for_session(&ws, &session));
    let document = crate::search::evidence_span_to_document(&row).into_indexable();
    assert_eq!(document.content, "[EVIDENCE_WITHHELD]");
    assert!(!serde_json::to_string(&document.metadata)?.contains(&token));
    db.close()?;
    Ok(())
}

#[test]
fn other_external_producers_are_scrubbed_without_gaining_cass_authority() -> TestResult {
    let (db, ws, session_id) = database()?;
    let session = db.get_session(&session_id)?.ok_or("missing session")?;
    let token = format!("{}{}", "ghp_", "Q".repeat(36));
    for (index, (producer, role)) in [
        (EvidenceProducerKind::AgentsmdImport, "agentsmd_import"),
        (EvidenceProducerKind::DocsBootstrap, "docs_bootstrap"),
        (EvidenceProducerKind::JournalDistill, "journal_distill"),
        (EvidenceProducerKind::RememberReinforcement, "reinforcement"),
    ]
    .into_iter()
    .enumerate()
    {
        let id = EvidenceId::from_uuid(Uuid::from_u128(400 + index as u128)).to_string();
        let mut input = input(
            &ws,
            &session_id,
            &format!("Compilation succeeded. label-{token}"),
        );
        input.producer_kind = producer;
        input.role = Some(role.to_owned());
        input.cass_span_id = format!("source-{index}");
        db.insert_evidence_span(&id, &input)?;
        let row = db.get_evidence_span(&id)?.ok_or("missing evidence")?;
        assert!(!row.excerpt.contains(&token));
        assert_eq!(row.secret_redaction_status, "redacted");
        assert_eq!(row.search_eligibility, "denied");
        assert_eq!(row.pack_eligibility, "denied");
        assert!(row.is_derivation_admitted_for_session(&ws, &session));
        assert!(!row.is_search_admitted_for_session(&ws, &session));
    }
    db.close()?;
    Ok(())
}

#[test]
fn overlapping_credentials_never_reach_stored_evidence_or_derived_search_text() -> TestResult {
    let (db, ws, session_id) = database()?;
    let session = db.get_session(&session_id)?.ok_or("missing session")?;
    for (index, &(prefix, reason, minimum, contextual)) in
        crate::policy::RAW_TOKEN_PATTERNS.iter().enumerate()
    {
        let tail = "Q".repeat(minimum);
        let token = format!("{prefix}Q-123-45-6789-{tail}");
        let context = if contextual {
            "Twilio account SID: "
        } else {
            ""
        };
        let content = format!("Compilation succeeded. {context}label-{token} Tests passed.");
        let id = EvidenceId::from_uuid(Uuid::from_u128(500 + index as u128)).to_string();
        let mut row = input(&ws, &session_id, &content);
        row.cass_span_id = format!("overlap-{index}");
        db.insert_evidence_span(&id, &row)?;
        let admitted = db
            .get_search_admitted_evidence_span(&id, &ws)?
            .ok_or("useful screened evidence was lost")?;
        assert!(admitted.is_direct_pack_admitted_for_session(&ws, &session));
        assert_eq!(admitted.secret_redaction_status, "redacted");
        assert!(admitted.redaction_classes_json.contains(reason));
        assert_eq!(admitted.content_hash, hash(&admitted.excerpt));
        assert_eq!(
            admitted.canonical_excerpt_hash.as_deref(),
            Some(admitted.content_hash.as_str())
        );
        let document = crate::search::evidence_span_to_document(&admitted).into_indexable();
        for text in [&admitted.excerpt, &document.content] {
            assert!(!text.contains(&format!("{prefix}Q-")));
            assert!(!text.contains(&tail));
            assert!(text.contains("Compilation succeeded."));
            assert!(text.contains("Tests passed."));
        }
        assert!(!serde_json::to_string(&document.metadata)?.contains(&tail));
        let repeated = db
            .get_search_admitted_evidence_span(&id, &ws)?
            .ok_or("repeat live admission failed")?;
        assert_eq!(repeated.excerpt, admitted.excerpt);
        assert_eq!(repeated.content_hash, admitted.content_hash);
    }
    db.close()?;
    Ok(())
}

#[test]
fn overlap_scrubbing_does_not_promote_non_cass_producer_authority() -> TestResult {
    let (db, ws, session_id) = database()?;
    let session = db.get_session(&session_id)?.ok_or("missing session")?;
    let tail = "Q".repeat(36);
    let content = format!("Compilation succeeded. label-ghp_Q-123-45-6789-{tail}");
    for (index, (producer, role)) in [
        (EvidenceProducerKind::AgentsmdImport, "agentsmd_import"),
        (EvidenceProducerKind::DocsBootstrap, "docs_bootstrap"),
        (EvidenceProducerKind::JournalDistill, "journal_distill"),
        (EvidenceProducerKind::RememberReinforcement, "reinforcement"),
    ]
    .into_iter()
    .enumerate()
    {
        let id = EvidenceId::from_uuid(Uuid::from_u128(600 + index as u128)).to_string();
        let mut row = input(&ws, &session_id, &content);
        row.producer_kind = producer;
        row.role = Some(role.to_owned());
        row.cass_span_id = format!("external-overlap-{index}");
        db.insert_evidence_span(&id, &row)?;
        let stored = db.get_evidence_span(&id)?.ok_or("missing evidence")?;
        assert!(!stored.excerpt.contains("ghp_Q-"));
        assert!(!stored.excerpt.contains(&tail));
        assert!(stored.excerpt.contains("Compilation succeeded."));
        assert_eq!(stored.secret_redaction_status, "redacted");
        assert_eq!(stored.search_eligibility, "denied");
        assert_eq!(stored.pack_eligibility, "denied");
        assert!(stored.is_derivation_admitted_for_session(&ws, &session));
        assert!(!stored.is_search_admitted_for_session(&ws, &session));
        assert!(!stored.is_direct_pack_admitted_for_session(&ws, &session));
    }
    db.close()?;
    Ok(())
}

#[test]
fn contextual_provider_survives_screening_as_safe_evidence_not_as_a_credential() -> TestResult {
    let (db, ws, session_id) = database()?;
    let id = EvidenceId::from_uuid(Uuid::from_u128(700)).to_string();
    let first = format!("ghp_{}-twilio", "Q".repeat(36));
    let second = format!("AC{}", "Q".repeat(32));
    let content = format!("Compilation succeeded. label-{first} label-{second}; Tests passed.");
    db.insert_evidence_span(&id, &input(&ws, &session_id, &content))?;
    let admitted = db
        .get_search_admitted_evidence_span(&id, &ws)?
        .ok_or("useful screened evidence was lost")?;
    assert_eq!(admitted.secret_redaction_status, "redacted");
    for reason in ["github_token", "twilio_account_sid"] {
        assert!(admitted.redaction_classes_json.contains(reason));
    }
    let session = db.get_session(&session_id)?.ok_or("missing session")?;
    assert!(admitted.is_direct_pack_admitted_for_session(&ws, &session));
    let document = crate::search::evidence_span_to_document(&admitted).into_indexable();
    for text in [&admitted.excerpt, &document.content] {
        assert!(!text.contains(&first));
        assert!(!text.contains(&second));
        assert!(text.contains("Compilation succeeded."));
        assert!(text.contains("Tests passed."));
    }
    assert_eq!(admitted.content_hash, hash(&admitted.excerpt));
    assert!(!serde_json::to_string(&document.metadata)?.contains(&second));
    db.close()?;
    Ok(())
}
