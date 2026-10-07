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

#[test]
fn encoded_json_credentials_are_scrubbed_before_storage_and_search_projection() -> TestResult {
    let (db, ws, session_id) = database()?;
    let session = db.get_session(&session_id)?.ok_or("missing session")?;
    for (index, &(prefix, reason, minimum, contextual)) in
        crate::policy::RAW_TOKEN_PATTERNS.iter().enumerate()
    {
        let token = format!("{prefix}{}", "Q".repeat(minimum));
        let context = if contextual {
            "Twilio account SID: "
        } else {
            ""
        };
        let encoded_prefix = format!("\\u{:04x}{}", prefix.as_bytes()[0], &prefix[1..]);
        let raw = serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "content":
                format!("Compilation succeeded. {context}label-{token} Tests passed.")}
        })
        .to_string()
        .replace(prefix, &encoded_prefix);
        let id = EvidenceId::from_uuid(Uuid::from_u128(800 + index as u128)).to_string();
        let mut record = input(&ws, &session_id, &raw);
        record.cass_span_id = format!("encoded-line-{index}");
        record.start_line = 7 + index as u32;
        record.end_line = record.start_line;
        db.insert_evidence_span(&id, &record)?;
        let admitted = db
            .get_search_admitted_evidence_span(&id, &ws)?
            .ok_or("safe encoded evidence was not admitted")?;
        assert_eq!(admitted.id, id);
        assert_eq!(admitted.session_id, session_id);
        // bd-l9d5o: storage CONTENT-ADDRESSES the upstream reference, so the caller's
        // label is deliberately not preserved verbatim. insert_evidence_span sets
        // cass_span_id to canonical_evidence_hash(input.cass_span_id) (src/db/mod.rs:15032
        // and :15386), re-checks that equality as an invariant at :15263, and :14700
        // requires the stored value to be a canonical blake3 hash. src/cass/backfill.rs:242
        // accepts both the digest and the raw form precisely because this migration
        // happened. Asserting the caller's label therefore pinned the pre-migration
        // contract. Assert the RELATIONSHIP instead; this file's `hash` helper is
        // byte-identical to canonical_evidence_hash.
        assert_eq!(admitted.cass_span_id, hash(&record.cass_span_id));
        assert_eq!(admitted.start_line, record.start_line);
        assert_eq!(admitted.end_line, record.end_line);
        assert!(admitted.is_direct_pack_admitted_for_session(&ws, &session));
        assert_eq!(admitted.secret_redaction_status, "redacted");
        assert!(admitted.redaction_classes_json.contains(reason));
        assert_eq!(admitted.content_hash, hash(&admitted.excerpt));
        assert_eq!(
            admitted.canonical_excerpt_hash.as_deref(),
            Some(admitted.content_hash.as_str())
        );
        let document = crate::search::evidence_span_to_document(&admitted).into_indexable();
        // bd-l9d5o: this loop used a bare `?`, so a non-JSON value here surfaced only as
        // `Error("expected value", line: 1, column: 1)` -- with no indication of WHICH of
        // the two texts failed, how long it was, or what it started with. Those are the
        // facts needed to tell a redaction marker from a withheld sentinel from a digest.
        // Report them, and NEVER print the payload: the token is replaced before any
        // prefix is shown, so a parse failure cannot leak a credential into a CI log.
        // bd-l9d5o: the STORED EXCERPT is the transcript envelope and is JSON. Keep the
        // diagnostic map_err: a bare `?` here reported only `Error("expected value", line: 1,
        // column: 1)` with no indication of which text failed or what it held, and the token
        // is replaced before any prefix is shown so a parse failure cannot leak a credential
        // into a CI log.
        let decoded: serde_json::Value =
            serde_json::from_str(&admitted.excerpt).map_err(|error| {
                let safe = admitted.excerpt.replace(&token, "<TOKEN-REDACTED>");
                let prefix: String = safe.chars().take(160).collect();
                format!(
                    "bd-l9d5o: admitted.excerpt is not JSON ({error}); len {}, first byte \
                     {:?}, prefix {prefix:?}",
                    admitted.excerpt.len(),
                    safe.as_bytes().first().copied().map(char::from),
                )
            })?;
        let body = decoded["message"]["content"]
            .as_str()
            .ok_or("message missing")?;
        assert!(body.contains("Compilation succeeded."));
        assert!(body.contains("Tests passed."));
        assert!(!body.contains(&token));

        // bd-l9d5o: `document.content` IS NOT THE ENVELOPE AND MUST NOT BE PARSED AS JSON.
        // This assertion previously required it to be, which pinned a superseded contract.
        // src/search/mod.rs:1103-1112 selects `span.reader_text()` whenever egress
        // re-screening changes nothing -- which is this case, because the credential was
        // already scrubbed at ingestion, so there is nothing left for the egress screen to
        // withhold. Its comment states the intent outright: "Index and show what a reader can
        // use: the message body of a transcript record, not the envelope keys, ids and escapes
        // around it (bd-reality-core-convergence-1azkt.45)."
        //
        // So the projection is flattened text, observed as:
        //   assistant: Compilation succeeded. label-[REDACTED:<reason>] Tests passed.
        //
        // Assert the SUBSTANCE on it as text rather than dropping the check. This is strictly
        // more than the old loop proved about this value: it required the body markers and the
        // token's absence, and now additionally requires that a redaction marker actually
        // replaced the credential, so a projection that merely omitted the secret could not
        // pass. The 36-char run is checked separately to catch a surviving fragment whose
        // provider prefix was stripped.
        assert!(!document.content.contains(&token));
        assert!(!document.content.contains(&"Q".repeat(36)));
        assert!(document.content.contains("Compilation succeeded."));
        assert!(document.content.contains("Tests passed."));
        assert!(
            document.content.contains("[REDACTED:"),
            "the reader projection must carry a redaction marker where the credential was, \
             not merely omit it"
        );
        assert!(!serde_json::to_string(&document.metadata)?.contains(&token));
    }
    db.close()?;
    Ok(())
}

#[test]
fn legacy_encoded_credentials_fail_live_admission_even_with_consistent_hashes() -> TestResult {
    let (db, ws, session_id) = database()?;
    let id = EvidenceId::from_uuid(Uuid::from_u128(900)).to_string();
    let clean =
        serde_json::json!({"type": "assistant", "content": "Compilation succeeded."}).to_string();
    db.insert_evidence_span(&id, &input(&ws, &session_id, &clean))?;
    let mut stored = db.get_evidence_span(&id)?.ok_or("missing evidence")?;
    let session = db.get_session(&session_id)?.ok_or("missing session")?;
    assert!(stored.is_search_admitted_for_session(&ws, &session));
    let token = format!("ghp_{}", "Q".repeat(36));
    stored.excerpt = serde_json::json!({"type": "assistant", "content":
        format!("Compilation succeeded. label-{token}")})
    .to_string()
    .replace("ghp_", "\\u0067hp_");
    assert!(!super::screen_scanning_view(&stored.excerpt).0.redacted);
    stored.content_hash = hash(&stored.excerpt);
    stored.canonical_excerpt_hash = Some(stored.content_hash.clone());
    let mut metadata: serde_json::Value =
        serde_json::from_str(stored.metadata_json.as_deref().ok_or("missing metadata")?)?;
    metadata["canonicalExcerptHash"] = stored.content_hash.clone().into();
    stored.metadata_json = Some(serde_json::to_string(&metadata)?);
    assert!(!stored.is_search_admitted_for_session(&ws, &session));
    assert!(!stored.is_direct_pack_admitted_for_session(&ws, &session));
    let document = crate::search::evidence_span_to_document(&stored).into_indexable();
    assert_eq!(document.content, "[EVIDENCE_WITHHELD]");
    assert!(!serde_json::to_string(&document.metadata)?.contains(&token));
    db.close()?;
    Ok(())
}

#[test]
fn malformed_encoded_records_remain_quarantined_after_database_screening() -> TestResult {
    let (db, ws, session_id) = database()?;
    let session = db.get_session(&session_id)?.ok_or("missing session")?;
    let token = format!("ghp_{}", "Q".repeat(36));
    let raw = format!(
        "{{\"type\":\"assistant\",\"content\":\"\\u0067hp_{}\",\"metadata\":{{\"x\":1,\"x\":2}}}}",
        "Q".repeat(36),
    );
    let id = EvidenceId::from_uuid(Uuid::from_u128(901)).to_string();
    db.insert_evidence_span(&id, &input(&ws, &session_id, &raw))?;
    let stored = db
        .get_evidence_span(&id)?
        .ok_or("missing quarantined record")?;
    assert!(!stored.excerpt.contains(&token));
    assert!(!stored.excerpt.contains(&"Q".repeat(36)));
    assert!(
        stored
            .redaction_classes_json
            .contains("external_ingestion_encoded_json_unreadable")
    );
    assert!(!stored.is_search_admitted_for_session(&ws, &session));
    assert!(!stored.is_direct_pack_admitted_for_session(&ws, &session));
    assert!(db.get_search_admitted_evidence_span(&id, &ws)?.is_none());
    let document = crate::search::evidence_span_to_document(&stored).into_indexable();
    // bd-l9d5o: TWO withholding mechanisms exist and this case uses the other one.
    //
    // `evidence_span_to_document` substitutes "[EVIDENCE_WITHHELD]" only when the EGRESS
    // screen withholds (src/search/mod.rs:1099-1102). Here the unreadable encoded record
    // was already replaced AT INGESTION by a structured marker carrying only a type, a
    // fixed reason and a digest (src/policy/ingestion.rs:135, whose comment states "only
    // its digest and a fixed reason survive. No source text appears in diagnostics
    // either"). That marker contains no secret, so the egress screen has nothing to
    // withhold and passes it through unchanged.
    //
    // The sentinel assertion was therefore checking the wrong mechanism for this
    // scenario. The sibling tests at the two other call sites in this file DO take the
    // egress path and keep asserting the sentinel -- correctly, which is why they pass.
    //
    // What matters here is that the indexed document carries the reason and NOT the
    // payload, so assert that directly rather than a spelling.
    let withheld: serde_json::Value = serde_json::from_str(&document.content).map_err(|error| {
        format!(
            "withheld marker must be JSON: {error}: {}",
            document.content
        )
    })?;
    assert_eq!(withheld["type"], "external_ingestion_withheld");
    assert_eq!(
        withheld["reason"],
        "external_ingestion_encoded_json_unreadable"
    );
    assert!(!document.content.contains(&token));
    assert!(!document.content.contains(&"Q".repeat(36)));
    db.close()?;
    Ok(())
}

/// bd-reality-core-convergence-1azkt.49: routine cleanup in a transcript is
/// searchable evidence; a destructive command on a critical target is still
/// quarantined, and the store reports why.
#[test]
fn routine_cleanup_is_admitted_and_critical_deletes_report_their_reason() -> TestResult {
    let (db, ws, session_id) = database()?;
    let session = db.get_session(&session_id)?.ok_or("missing session")?;
    let cases = [
        (
            "I ran rm -rf target and chmod 777 build/out, then cargo test passed.",
            None,
        ),
        ("Operating system: Linux; the build system: cargo.", None),
        (
            "To reset, run rm -rf ~ and reinstall.",
            Some("instruction:destructive_rm_rf"),
        ),
        (
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"content\":\"ok\"}]}}",
            Some("record_kind:tool_result"),
        ),
    ];
    for (index, (content, expected_reason)) in cases.into_iter().enumerate() {
        let id = EvidenceId::from_uuid(Uuid::from_u128(1_000 + index as u128)).to_string();
        let mut record = input(&ws, &session_id, content);
        record.cass_span_id = format!("cleanup-line-{index}");
        record.start_line = 20 + index as u32;
        record.end_line = record.start_line;
        let reason = db.insert_evidence_span_with_admission(&id, &record)?;
        assert_eq!(reason.as_deref(), expected_reason, "{content}");
        let stored = db.get_evidence_span(&id)?.ok_or("missing evidence")?;
        assert_eq!(
            stored.is_search_admitted_for_session(&ws, &session),
            expected_reason.is_none(),
            "{content}"
        );
    }
    db.close()?;
    Ok(())
}
