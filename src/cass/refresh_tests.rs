//! Database-backed refresh, retry, privacy and transaction regressions.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::cass::CassAgent;
use crate::db::CreateWorkspaceInput;
use crate::models::WorkspaceId;
use serde_json::json;

fn discovered(count: u32) -> CassSessionInfo {
    let mut session =
        CassSessionInfo::new("/private/refresh-source.jsonl").with_agent(CassAgent::Codex);
    session.workspace_dir = Some("/private/source-workspace".to_owned());
    session.started_at = Some("2026-09-20T01:00:00Z".to_owned());
    session.message_count = Some(count);
    session.content_hash = Some(format!(
        "blake3:{}",
        blake3::hash(&count.to_le_bytes()).to_hex()
    ));
    session.content_hash_source = Some("provided".to_owned());
    session
}

fn span(session: &CassSessionInfo, line: u32, content: &str) -> CassViewSpanForImport {
    super::super::parse_view_line_value(
        &json!({"line": line, "content": content}),
        &session.source_path,
    )
    .unwrap()
}

/// Model an already-persisted historical producer without passing its bytes
/// through today's insertion screener. Recovery/upgrade keeps these exact rows.
fn historical_excerpt(
    db: &DbConnection,
    id: &str,
    excerpt: &str,
    keep_source: bool,
    clean: bool,
) -> StoredEvidenceSpan {
    let row = db.get_evidence_span(id).unwrap().unwrap();
    let hash = format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex());
    let mut metadata: serde_json::Value =
        serde_json::from_str(row.metadata_json.as_deref().unwrap()).unwrap();
    metadata["canonicalExcerptHash"] = hash.clone().into();
    if !keep_source {
        metadata.as_object_mut().unwrap().remove("cassSource");
    }
    if clean {
        metadata["secretRedactionStatus"] = "clean".into();
        metadata["redactionClasses"] = json!([]);
    }
    db.execute_raw(&format!(
        "UPDATE evidence_spans SET excerpt = {}, content_hash = {}, canonical_excerpt_hash = {}, metadata_json = {}, secret_redaction_status = {}, redaction_classes_json = {} WHERE id = {}",
        sql_text(excerpt), sql_text(&hash), sql_text(&hash), sql_text(&metadata.to_string()),
        sql_text(if clean { "clean" } else { &row.secret_redaction_status }),
        sql_text(if clean { "[]" } else { &row.redaction_classes_json }), sql_text(id),
    )).unwrap();
    db.get_evidence_span(id).unwrap().unwrap()
}

#[test]
fn source_commitment_reconciles_encoded_screening_without_rewriting_provenance() {
    let source = format!(
        r#"{{ "type": "assistant", "message": {{"role":"assistant","content":"Release passed. \u001b label-ghp_{}"}} }}"#,
        "Q".repeat(36),
    );
    let (db, workspace, id, session, spans) = fixture(1, &[&source]);
    let evidence_id = stable_evidence_id(&id, &spans[0].cass_span_id);
    // This is the v0.17.0 screen_scanning_view representation. Retain a
    // source-aware producer's commitment while changing only the old encoding.
    let old_excerpt = crate::policy::redact_git_capture_text(&source).content;
    assert_ne!(old_excerpt, spans[0].excerpt);
    let old = historical_excerpt(&db, &evidence_id, &old_excerpt, true, false);
    let old_audits = db
        .list_audit_by_target("evidence_span", &evidence_id, None)
        .unwrap();
    completed(&db, &stable_search_index_job_id(&workspace, &id));

    let first = refresh_session(&db, &workspace, &id, &session, &spans).unwrap();
    assert!(first.changed);
    assert!(first.added_lines.is_empty());
    assert_eq!(db.get_evidence_span(&evidence_id).unwrap().unwrap(), old);
    let audits = db
        .list_audit_by_target("evidence_span", &evidence_id, None)
        .unwrap();
    for original in &old_audits {
        assert!(
            audits.contains(original),
            "original redaction audit must survive"
        );
    }
    let reconciliation = audits
        .iter()
        .find(|audit| audit.action == "cass.evidence.screening_reconciled")
        .unwrap();
    let details: serde_json::Value =
        serde_json::from_str(reconciliation.details.as_deref().unwrap()).unwrap();
    assert_eq!(details["sourceProof"], "captured_source_commitment");
    assert_eq!(details["historicalEvidenceRewritten"], false);
    assert!(
        !reconciliation
            .details
            .as_deref()
            .unwrap()
            .contains(&"Q".repeat(36))
    );
    let job = first.index_job_id.unwrap();
    let stored_session = db.get_session(&id).unwrap();
    for _ in 0..2 {
        let retry = refresh_session(&db, &workspace, &id, &session, &spans).unwrap();
        assert!(!retry.changed);
        assert_eq!(retry.index_job_id.as_deref(), Some(job.as_str()));
        assert_eq!(db.get_evidence_span(&evidence_id).unwrap().unwrap(), old);
        assert_eq!(db.get_session(&id).unwrap(), stored_session);
        assert_eq!(
            db.list_audit_by_target("evidence_span", &evidence_id, None)
                .unwrap(),
            audits
        );
    }
    completed(&db, &job);
    assert!(
        refresh_session(&db, &workspace, &id, &session, &spans)
            .unwrap()
            .index_job_id
            .is_none()
    );
}

#[test]
fn v017_complete_clean_source_can_reconcile_a_new_withholding_representation() {
    let source = r#"{"type":"assistant","content":"Historical notation \uD800"}"#;
    let (db, workspace, id, session, spans) = fixture(1, &[source]);
    assert!(spans[0].excerpt.contains("external_ingestion_withheld"));
    let evidence_id = stable_evidence_id(&id, &spans[0].cass_span_id);
    let old = historical_excerpt(&db, &evidence_id, source, false, true);
    let first = refresh_session(&db, &workspace, &id, &session, &spans).unwrap();
    assert!(first.changed);
    assert!(first.added_lines.is_empty());
    assert_eq!(db.get_evidence_span(&evidence_id).unwrap().unwrap(), old);
    let audits = db
        .list_audit_by_target("evidence_span", &evidence_id, None)
        .unwrap();
    let audit = audits
        .iter()
        .find(|a| a.action == "cass.evidence.screening_reconciled")
        .unwrap();
    let details: serde_json::Value =
        serde_json::from_str(audit.details.as_deref().unwrap()).unwrap();
    assert_eq!(details["sourceProof"], "retained_complete_excerpt");
    assert!(!old.metadata_json.as_deref().unwrap().contains("cassSource"));
    assert!(
        !refresh_session(&db, &workspace, &id, &session, &spans)
            .unwrap()
            .changed
    );
    // A digest observed during upgrade never gets written back as a purported
    // import-time commitment, even after another retry.
    assert_eq!(db.get_evidence_span(&evidence_id).unwrap().unwrap(), old);
}

#[test]
fn legacy_withholding_digest_reconciles_once_and_rejects_untrusted_markers() {
    let source = json!({
        "type": "assistant",
        "message": {"role": "assistant", "content": [
            {"type": "thinking", "thinking": "Review the observed build result."},
            {"type": "text", "text": format!(
                "{}original-source-tail", "Release verification passed. ".repeat(4000)
            )},
        ]},
    })
    .to_string();
    let digest = format!("blake3:{}", blake3::hash(source.as_bytes()).to_hex());
    let marker = json!({
        "type": "external_ingestion_withheld",
        "reason": "external_ingestion_oversized",
        "sourceDigest": digest,
        "redaction": "[REDACTED:external_ingestion_oversized]",
    })
    .to_string();
    // A legacy producer withheld this oversized mixed-content envelope.
    // Today's projector preserves its thinking block and bounds visible text.
    let (db, workspace, id, session, spans) =
        legacy_withholding_fixture(&source, &marker, &["external_ingestion_oversized"]);
    assert!(spans[0].excerpt.contains("[TRUNCATED]"));
    assert!(!spans[0].excerpt.contains("external_ingestion_withheld"));
    let evidence_id = stable_evidence_id(&id, &spans[0].cass_span_id);
    let old = db.get_evidence_span(&evidence_id).unwrap().unwrap();
    assert_eq!(old.excerpt, marker);
    assert_eq!(old.search_eligibility, "quarantined");
    assert!(!old.metadata_json.as_deref().unwrap().contains("cassSource"));
    let original_audits = db.list_audit_entries(Some(&workspace), None).unwrap();
    let first = refresh_session(&db, &workspace, &id, &session, &spans).unwrap();
    assert!(first.changed);
    assert!(first.added_lines.is_empty());
    let job = first.index_job_id.unwrap();
    let audits = db.list_audit_entries(Some(&workspace), None).unwrap();
    assert!(original_audits.iter().all(|audit| audits.contains(audit)));
    let reconciliations: Vec<_> = audits
        .iter()
        .filter(|audit| audit.action == "cass.evidence.screening_reconciled")
        .collect();
    assert_eq!(reconciliations.len(), 1);
    let details: serde_json::Value =
        serde_json::from_str(reconciliations[0].details.as_deref().unwrap()).unwrap();
    assert_eq!(details["sourceProof"], "retained_withholding_digest");
    assert_eq!(details["sourceContentHash"], digest);
    assert_eq!(details["historicalEvidenceRewritten"], false);
    assert!(
        !reconciliations[0]
            .details
            .as_deref()
            .unwrap()
            .contains("original-source-tail")
    );
    let stored_session = db.get_session(&id).unwrap();
    let jobs = db.list_search_index_jobs(&workspace, None).unwrap();
    for _ in 0..2 {
        let retry = refresh_session(&db, &workspace, &id, &session, &spans).unwrap();
        assert!(!retry.changed);
        assert!(retry.added_lines.is_empty());
        assert_eq!(retry.index_job_id.as_deref(), Some(job.as_str()));
        assert_eq!(db.get_evidence_span(&evidence_id).unwrap().unwrap(), old);
        assert_eq!(db.get_session(&id).unwrap(), stored_session);
        assert_eq!(
            db.list_audit_entries(Some(&workspace), None).unwrap(),
            audits
        );
        assert_eq!(db.list_search_index_jobs(&workspace, None).unwrap(), jobs);
    }
    let changed = span(
        &session,
        1,
        &source.replace("original-source-tail", "modified-source-tail"),
    );
    assert_eq!(changed.excerpt, spans[0].excerpt);
    let error = refresh_session(&db, &workspace, &id, &session, &[changed])
        .err()
        .unwrap();
    assert!(error.to_string().contains("cass_refresh_history_changed"));
    assert_eq!(db.get_evidence_span(&evidence_id).unwrap().unwrap(), old);
    assert_eq!(db.get_session(&id).unwrap(), stored_session);
    assert_eq!(
        db.list_audit_entries(Some(&workspace), None).unwrap(),
        audits
    );
    assert_eq!(db.list_search_index_jobs(&workspace, None).unwrap(), jobs);

    let mut extra: serde_json::Value = serde_json::from_str(&marker).unwrap();
    extra["unexpected"] = true.into();
    for (case, excerpt, classes) in [
        (
            "malformed JSON marker",
            marker.strip_suffix('}').unwrap().to_owned(),
            vec!["external_ingestion_oversized"],
        ),
        (
            "marker with extra field",
            extra.to_string(),
            vec!["external_ingestion_oversized"],
        ),
        ("untrusted upstream marker", marker.clone(), Vec::new()),
    ] {
        let (db, workspace, id, session, spans) =
            legacy_withholding_fixture(&source, &excerpt, &classes);
        let evidence = db.list_evidence_spans_for_session(&id).unwrap();
        if classes.is_empty() {
            assert_eq!(
                evidence[0].redaction_classes_json,
                "[\"inherited_source_redaction\"]"
            );
        }
        let before = db.get_session(&id).unwrap();
        let audits = db.list_audit_entries(Some(&workspace), None).unwrap();
        let jobs = db.list_search_index_jobs(&workspace, None).unwrap();
        for _ in 0..2 {
            let error = refresh_session(&db, &workspace, &id, &session, &spans)
                .err()
                .unwrap();
            assert!(
                error
                    .to_string()
                    .contains("cass_refresh_history_unverifiable"),
                "{case}: {error}"
            );
            assert_eq!(db.list_evidence_spans_for_session(&id).unwrap(), evidence);
            assert_eq!(db.get_session(&id).unwrap(), before);
            assert_eq!(
                db.list_audit_entries(Some(&workspace), None).unwrap(),
                audits
            );
            assert_eq!(db.list_search_index_jobs(&workspace, None).unwrap(), jobs);
        }
    }
}

fn legacy_withholding_fixture(source: &str, excerpt: &str, classes: &[&str]) -> Fixture {
    let (db, workspace, id, session, _) = fixture(1, &[]);
    let current = span(&session, 1, source);
    let mut legacy = current.clone();
    legacy.excerpt = excerpt.to_owned();
    legacy.content_hash = format!("blake3:{}", blake3::hash(excerpt.as_bytes()).to_hex());
    legacy.redacted = !classes.is_empty();
    legacy.redacted_reasons = classes.iter().map(|class| (*class).to_owned()).collect();
    let mut input = evidence_input(&workspace, &id, &legacy);
    let mut metadata: serde_json::Value =
        serde_json::from_str(input.metadata_json.as_deref().unwrap()).unwrap();
    metadata.as_object_mut().unwrap().remove("cassSource");
    input.metadata_json = Some(metadata.to_string());
    let evidence_id = stable_evidence_id(&id, &legacy.cass_span_id);
    db.insert_evidence_span(&evidence_id, &input).unwrap();
    if legacy.redacted {
        db.insert_audit(
            &stable_cass_redaction_audit_id(&evidence_id),
            &cass_redaction_audit_input(&workspace, &id, &evidence_id, &legacy),
        )
        .unwrap();
    }
    (db, workspace, id, session, vec![current])
}

#[test]
fn legacy_canonical_json_cannot_authenticate_unmarked_v017_normalization() {
    let canonical =
        json!({"type": "assistant", "content": "Release verification passed."}).to_string();
    let padded = format!(
        "{}{canonical}",
        " ".repeat(super::super::ingestion::MAX_EXCERPT_BYTES)
    );
    assert!(padded.len() > super::super::ingestion::MAX_EXCERPT_BYTES);
    // Both an oversized source normalized by v0.17 and an originally compact
    // source produce this same clean stored record. The missing commitment
    // cannot be reconstructed from that record or from the current source.
    for original in [&padded, &canonical] {
        let (db, workspace, id, session, spans) = fixture(1, &[original]);
        assert_eq!(spans[0].excerpt, canonical);
        let evidence_id = stable_evidence_id(&id, &spans[0].cass_span_id);
        let old = historical_excerpt(&db, &evidence_id, &canonical, false, true);
        assert!(!old.metadata_json.as_deref().unwrap().contains("cassSource"));
        let before = db.get_session(&id).unwrap();
        let audits = db.list_audit_entries(Some(&workspace), None).unwrap();
        let jobs = db.list_search_index_jobs(&workspace, None).unwrap();
        for observed in [&padded, &canonical] {
            let incoming = vec![span(&session, 1, observed)];
            let error = refresh_session(&db, &workspace, &id, &session, &incoming)
                .err()
                .unwrap();
            assert!(
                error
                    .to_string()
                    .contains("cass_refresh_history_unverifiable")
            );
            assert_eq!(db.get_evidence_span(&evidence_id).unwrap().unwrap(), old);
            assert_eq!(db.get_session(&id).unwrap(), before);
            assert_eq!(
                db.list_audit_entries(Some(&workspace), None).unwrap(),
                audits
            );
            assert_eq!(db.list_search_index_jobs(&workspace, None).unwrap(), jobs);
        }
    }
}

#[test]
fn legacy_canonical_jsonl_cannot_authenticate_unmarked_stream_normalization() {
    let canonical = [
        json!({"type": "assistant", "content": "Release verification passed."}).to_string(),
        json!({"type": "assistant", "content": "No new diagnostics were reported."}).to_string(),
    ]
    .join("\n");
    let padded = format!(
        "{}{canonical}",
        " ".repeat(super::super::ingestion::MAX_EXCERPT_BYTES)
    );
    // The producer on main before source commitments could normalize a whole
    // JSONL window without a marker when its compact representation fit.
    for original in [&padded, &canonical] {
        let (db, workspace, id, session, spans) = fixture(1, &[original]);
        assert_eq!(spans[0].excerpt, canonical);
        let evidence_id = stable_evidence_id(&id, &spans[0].cass_span_id);
        let old = historical_excerpt(&db, &evidence_id, &canonical, false, true);
        let before = db.get_session(&id).unwrap();
        let audits = db.list_audit_entries(Some(&workspace), None).unwrap();
        let jobs = db.list_search_index_jobs(&workspace, None).unwrap();
        for observed in [&padded, &canonical] {
            let incoming = vec![span(&session, 1, observed)];
            let error = refresh_session(&db, &workspace, &id, &session, &incoming)
                .err()
                .unwrap();
            assert!(
                error
                    .to_string()
                    .contains("cass_refresh_history_unverifiable")
            );
            assert_eq!(db.get_evidence_span(&evidence_id).unwrap().unwrap(), old);
            assert_eq!(db.get_session(&id).unwrap(), before);
            assert_eq!(
                db.list_audit_entries(Some(&workspace), None).unwrap(),
                audits
            );
            assert_eq!(db.list_search_index_jobs(&workspace, None).unwrap(), jobs);
        }
    }

    // A clean, unmarked, noncanonical short window cannot be a canonical
    // normalization result and still retains its exact original source.
    let noncanonical = canonical.replace('{', "{ ");
    let (db, workspace, id, session, spans) = fixture(1, &[&noncanonical]);
    assert_eq!(spans[0].excerpt, noncanonical);
    let evidence_id = stable_evidence_id(&id, &spans[0].cass_span_id);
    let old = historical_excerpt(&db, &evidence_id, &noncanonical, false, true);
    assert!(
        !refresh_session(&db, &workspace, &id, &session, &spans)
            .unwrap()
            .changed
    );
    assert_eq!(db.get_evidence_span(&evidence_id).unwrap().unwrap(), old);
}

#[test]
fn changed_source_is_refused_even_when_redaction_or_bounding_hides_the_change() {
    let secret_a = format!("Release passed. label-ghp_{}", "Q".repeat(36));
    let secret_b = format!("Release passed. label-ghp_{}", "R".repeat(36));
    let long_a = json!({"type":"assistant", "message":{"role":"assistant", "content":format!("{}end-A", "Useful release observation. ".repeat(4000))}}).to_string();
    let long_b = long_a.replace("end-A", "end-B");
    for (original, changed) in [(secret_a, secret_b), (long_a, long_b)] {
        let (db, workspace, id, _, old) = fixture(1, &[&original]);
        let latest = discovered(2);
        let altered = span(&latest, 1, &changed);
        assert_eq!(
            altered.excerpt, old[0].excerpt,
            "fixture must hide the source edit in screening"
        );
        assert_ne!(altered.source_hash, old[0].source_hash);
        let before = db.get_session(&id).unwrap();
        let evidence = db.list_evidence_spans_for_session(&id).unwrap();
        let audits = db.list_audit_entries(Some(&workspace), None).unwrap();
        let incoming = vec![
            altered,
            span(&latest, 2, "Must roll back beside changed history."),
        ];
        let error = refresh_session(&db, &workspace, &id, &latest, &incoming)
            .err()
            .unwrap();
        assert!(error.to_string().contains("cass_refresh_history_changed"));
        assert_eq!(db.get_session(&id).unwrap(), before);
        assert_eq!(db.list_evidence_spans_for_session(&id).unwrap(), evidence);
        assert_eq!(
            db.list_audit_entries(Some(&workspace), None).unwrap(),
            audits
        );
        assert_eq!(db.count_table_rows("search_index_jobs").unwrap(), 1);
    }
}

#[test]
fn unverifiable_legacy_redactions_never_acquire_a_fabricated_source_commitment() {
    let source = format!(
        r#"{{"type":"assistant","content":"\u001b label-ghp_{}"}}"#,
        "Q".repeat(36)
    );
    let (db, workspace, id, session, spans) = fixture(1, &[&source]);
    let evidence_id = stable_evidence_id(&id, &spans[0].cass_span_id);
    let old_excerpt = crate::policy::redact_git_capture_text(&source).content;
    let old = historical_excerpt(&db, &evidence_id, &old_excerpt, false, false);
    let before = db.get_session(&id).unwrap();
    let audits = db.list_audit_entries(Some(&workspace), None).unwrap();
    for raw in [&source, &source.replace(&"Q".repeat(36), &"R".repeat(36))] {
        let incoming = vec![span(&session, 1, raw)];
        let error = refresh_session(&db, &workspace, &id, &session, &incoming)
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains("cass_refresh_history_unverifiable")
        );
        assert_eq!(db.get_evidence_span(&evidence_id).unwrap().unwrap(), old);
        assert_eq!(db.get_session(&id).unwrap(), before);
        assert_eq!(
            db.list_audit_entries(Some(&workspace), None).unwrap(),
            audits
        );
    }
}

#[test]
fn utf8_truncated_legacy_prefix_is_not_mistaken_for_complete_source() {
    for retreat in 1..=3 {
        let prefix = "x".repeat(super::super::ingestion::MAX_EXCERPT_BYTES - retreat);
        let source = format!("{prefix}🦀 discarded historical tail");
        let old_excerpt =
            super::super::truncate_excerpt(&source, super::super::ingestion::MAX_EXCERPT_BYTES);
        assert_eq!(old_excerpt, prefix);
        let (db, workspace, id, session, spans) = fixture(1, &[&source]);
        let evidence_id = stable_evidence_id(&id, &spans[0].cass_span_id);
        let old = historical_excerpt(&db, &evidence_id, &old_excerpt, false, true);
        let before = db.get_session(&id).unwrap();
        let audits = db.list_audit_entries(Some(&workspace), None).unwrap();
        // The second source has genuinely lost the tail, yet is byte-equal to
        // the retained prefix. Neither observation can prove the old source.
        for observed in [&source, &prefix] {
            let incoming = vec![span(&session, 1, observed)];
            let error = refresh_session(&db, &workspace, &id, &session, &incoming)
                .err()
                .unwrap();
            assert!(
                error
                    .to_string()
                    .contains("cass_refresh_history_unverifiable")
            );
            assert_eq!(db.get_evidence_span(&evidence_id).unwrap().unwrap(), old);
            assert_eq!(db.get_session(&id).unwrap(), before);
            assert_eq!(
                db.list_audit_entries(Some(&workspace), None).unwrap(),
                audits
            );
        }
    }
}

#[test]
fn reconciliation_audit_and_checkpoint_roll_back_with_a_failed_index_job() {
    let source = r#"{"type":"assistant","content":"Historical notation \uD800"}"#;
    let (db, workspace, id, session, spans) = fixture(1, &[source]);
    let evidence_id = stable_evidence_id(&id, &spans[0].cass_span_id);
    let old = historical_excerpt(&db, &evidence_id, source, false, true);
    let before = db.get_session(&id).unwrap();
    let audits = db.list_audit_entries(Some(&workspace), None).unwrap();
    db.execute_raw("CREATE TRIGGER reject_reconciled_job BEFORE INSERT ON search_index_jobs BEGIN SELECT RAISE(ABORT, 'reconciliation-test-injected-failure'); END").unwrap();
    assert!(refresh_session(&db, &workspace, &id, &session, &spans).is_err());
    assert_eq!(db.get_evidence_span(&evidence_id).unwrap().unwrap(), old);
    assert_eq!(db.get_session(&id).unwrap(), before);
    assert_eq!(
        db.list_audit_entries(Some(&workspace), None).unwrap(),
        audits
    );
    assert_eq!(db.count_table_rows("search_index_jobs").unwrap(), 1);
}

type Fixture = (
    DbConnection,
    String,
    String,
    CassSessionInfo,
    Vec<CassViewSpanForImport>,
);

fn fixture(count: u32, contents: &[&str]) -> Fixture {
    let db = DbConnection::open_memory().unwrap();
    db.migrate().unwrap();
    let workspace = WorkspaceId::from_uuid(uuid::Uuid::from_u128(8701)).to_string();
    db.insert_workspace(
        &workspace,
        &CreateWorkspaceInput {
            path: "/refresh-workspace".to_owned(),
            name: None,
        },
    )
    .unwrap();
    let session = discovered(count);
    let spans: Vec<_> = contents
        .iter()
        .enumerate()
        .map(|(index, content)| {
            span(
                &session,
                u32::try_from(index).expect("fixture line count") + 1,
                content,
            )
        })
        .collect();
    let result =
        super::super::persist_session_import_if_absent(&db, &workspace, &session, &spans).unwrap();
    let super::super::SessionImportPersistResult::Imported { session_id, .. } = result else {
        panic!("fixture must import a new session");
    };
    (db, workspace, session_id, session, spans)
}

fn completed(db: &DbConnection, job: &str) {
    db.execute_raw(&format!(
        "UPDATE search_index_jobs SET status = 'completed' WHERE id = {}",
        sql_text(job)
    ))
    .unwrap();
    assert_eq!(
        db.get_search_index_job(job).unwrap().unwrap().status_enum(),
        Some(SearchIndexJobStatus::Completed)
    );
}

#[test]
fn growing_session_adds_evidence_and_reindexes_without_replacing_old_identity() {
    let (db, workspace, id, old, mut spans) = fixture(1, &["Initial build result."]);
    let original_id = stable_evidence_id(&id, &spans[0].cass_span_id);
    assert!(
        db.get_search_admitted_evidence_span(&original_id, &workspace)
            .unwrap()
            .is_some()
    );
    completed(&db, &stable_search_index_job_id(&workspace, &id));
    let latest = discovered(2);
    spans.push(span(
        &latest,
        2,
        "The quasar release gate now requires cargo fmt.",
    ));

    let report = refresh_session(&db, &workspace, &id, &latest, &spans).unwrap();
    assert!(report.changed);
    assert_eq!(report.added_lines, vec![2]);
    let job = report.index_job_id.expect("new revision needs publication");
    assert_ne!(job, stable_search_index_job_id(&workspace, &id));
    let work = db.get_search_index_job(&job).unwrap().unwrap();
    assert_eq!(work.workspace_id, workspace);
    assert_eq!(work.document_source.as_deref(), Some("session"));
    assert_eq!(work.document_id.as_deref(), Some(id.as_str()));
    assert_ne!(work.status_enum(), Some(SearchIndexJobStatus::Completed));
    let stored = db.get_session(&id).unwrap().unwrap();
    assert_eq!(stored.cass_session_id, old.source_path);
    assert_eq!(stored.message_count, 2);
    assert_eq!(stored.content_hash, latest.content_hash.unwrap());
    assert_eq!(db.list_sessions(&workspace).unwrap().len(), 1);
    assert_eq!(db.list_evidence_spans_for_session(&id).unwrap().len(), 2);
    assert_eq!(
        db.get_evidence_span(&original_id).unwrap().unwrap().excerpt,
        spans[0].excerpt
    );
    assert!(
        db.get_search_admitted_evidence_span(&original_id, &workspace)
            .unwrap()
            .is_some()
    );
    let added_id = stable_evidence_id(&id, &spans[1].cass_span_id);
    assert_eq!(
        db.get_evidence_span(&added_id).unwrap().unwrap().excerpt,
        spans[1].excerpt
    );
}

#[test]
fn metadata_only_import_can_later_capture_the_complete_transcript() {
    let (db, workspace, id, session, _) = fixture(3, &[]);
    completed(&db, &stable_search_index_job_id(&workspace, &id));
    let spans = vec![
        span(&session, 1, "First observation."),
        span(&session, 2, "Second observation."),
        span(&session, 3, "Final observation."),
    ];
    let report = refresh_session(&db, &workspace, &id, &session, &spans).unwrap();
    assert!(report.changed);
    assert_eq!(report.added_lines, vec![1, 2, 3]);
    assert!(report.index_job_id.is_some());
    assert_eq!(db.list_evidence_spans_for_session(&id).unwrap().len(), 3);
}

#[test]
fn completed_snapshot_retry_is_a_noop_and_pending_refresh_is_resumable() {
    let (db, workspace, id, _, mut spans) = fixture(1, &["First observation."]);
    completed(&db, &stable_search_index_job_id(&workspace, &id));
    let latest = discovered(2);
    spans.push(span(&latest, 2, "Second observation."));
    let first = refresh_session(&db, &workspace, &id, &latest, &spans).unwrap();
    let job = first.index_job_id.unwrap();
    let audits = db.list_audit_by_target("session", &id, None).unwrap().len();
    let stored = db.get_session(&id).unwrap().unwrap();
    for status in ["pending", "failed", "cancelled", "running"] {
        db.execute_raw(&format!(
            "UPDATE search_index_jobs SET status = {} WHERE id = {}",
            sql_text(status),
            sql_text(&job)
        ))
        .unwrap();
        let again = refresh_session(&db, &workspace, &id, &latest, &spans).unwrap();
        assert!(!again.changed);
        assert!(again.added_lines.is_empty());
        assert_eq!(again.index_job_id.as_deref(), Some(job.as_str()));
        assert_eq!(db.list_evidence_spans_for_session(&id).unwrap().len(), 2);
        assert_eq!(
            db.list_audit_by_target("session", &id, None).unwrap().len(),
            audits
        );
        assert_eq!(db.get_session(&id).unwrap().unwrap(), stored);
    }
    completed(&db, &job);
    let final_retry = refresh_session(&db, &workspace, &id, &latest, &spans).unwrap();
    assert!(!final_retry.changed);
    assert!(final_retry.index_job_id.is_none());
}

#[test]
fn unchanged_original_snapshot_still_reconciles_its_original_job() {
    let (db, workspace, id, session, spans) = fixture(1, &["Same observation."]);
    let job = stable_search_index_job_id(&workspace, &id);
    let first = refresh_session(&db, &workspace, &id, &session, &spans).unwrap();
    assert!(!first.changed);
    assert!(first.added_lines.is_empty());
    assert_eq!(first.index_job_id.as_deref(), Some(job.as_str()));
    completed(&db, &job);
    let second = refresh_session(&db, &workspace, &id, &session, &spans).unwrap();
    assert!(!second.changed);
    assert!(second.index_job_id.is_none());
}

#[test]
fn changing_or_truncating_retained_history_refuses_the_whole_refresh() {
    let (db, workspace, id, _, original) =
        fixture(2, &["First observation.", "Retained final result."]);
    let before = db.get_session(&id).unwrap().unwrap();
    let latest = discovered(3);
    let additions = span(
        &latest,
        3,
        "New result must not commit beside rewritten history.",
    );
    let mut rewritten = original.clone();
    rewritten[0] = span(&latest, 1, "PRIVATE_REWRITE_SENTINEL");
    rewritten.push(additions.clone());
    let truncated = vec![original[0].clone(), additions];
    for candidates in [rewritten, truncated] {
        let error = refresh_session(&db, &workspace, &id, &latest, &candidates)
            .err()
            .unwrap();
        let text = error.to_string();
        assert!(text.contains("cass_refresh_history_"));
        assert!(!text.contains("PRIVATE_REWRITE_SENTINEL"));
        assert!(!text.contains(&latest.source_path));
        assert_eq!(db.get_session(&id).unwrap().unwrap(), before);
        assert_eq!(db.list_evidence_spans_for_session(&id).unwrap().len(), 2);
        assert_eq!(db.count_table_rows("search_index_jobs").unwrap(), 1);
    }
}

#[test]
fn existing_denial_is_never_replaced_by_fresh_upstream_admission() {
    let (db, workspace, id, _, mut spans) = fixture(1, &["Earlier observation."]);
    let original_id = stable_evidence_id(&id, &spans[0].cass_span_id);
    db.execute_raw(
        "UPDATE evidence_spans SET search_eligibility = 'denied', pack_eligibility = 'denied'",
    )
    .unwrap();
    let latest = discovered(2);
    spans.push(span(&latest, 2, "New independent observation."));
    let report = refresh_session(&db, &workspace, &id, &latest, &spans).unwrap();
    assert_eq!(report.added_lines, vec![2]);
    let original = db.get_evidence_span(&original_id).unwrap().unwrap();
    assert_eq!(original.search_eligibility, "denied");
    assert_eq!(original.pack_eligibility, "denied");
    assert!(
        db.get_search_admitted_evidence_span(&original_id, &workspace)
            .unwrap()
            .is_none()
    );
}

#[test]
fn missing_early_lines_can_be_backfilled_without_minting_a_second_session() {
    let (db, workspace, id, session, _) = fixture(3, &[]);
    let middle = span(&session, 2, "Already captured middle observation.");
    let middle_id = stable_evidence_id(&id, &middle.cass_span_id);
    db.insert_evidence_span(&middle_id, &evidence_input(&workspace, &id, &middle))
        .unwrap();
    let all = vec![
        span(&session, 3, "Last observation."),
        middle.clone(),
        span(&session, 1, "First observation."),
    ];
    let report = refresh_session(&db, &workspace, &id, &session, &all).unwrap();
    assert_eq!(report.added_lines, vec![1, 3]);
    assert_eq!(db.list_sessions(&workspace).unwrap().len(), 1);
    assert_eq!(
        db.get_evidence_span(&middle_id).unwrap().unwrap().excerpt,
        middle.excerpt
    );
}

#[test]
fn redacted_additions_use_the_regular_screening_and_audit_path() {
    let (db, workspace, id, _, mut spans) = fixture(1, &["First safe observation."]);
    let latest = discovered(2);
    let token = format!("ghp_{}", "Q".repeat(36));
    spans.push(span(
        &latest,
        2,
        &format!("Build succeeded. label-{token} Tests passed."),
    ));
    assert!(spans[1].redacted);
    refresh_session(&db, &workspace, &id, &latest, &spans).unwrap();
    let evidence_id = stable_evidence_id(&id, &spans[1].cass_span_id);
    let stored = db.get_evidence_span(&evidence_id).unwrap().unwrap();
    assert!(!stored.excerpt.contains(&token));
    assert_eq!(stored.secret_redaction_status, "redacted");
    let audits = db
        .list_audit_by_target("evidence_span", &evidence_id, None)
        .unwrap();
    assert_eq!(audits.len(), 1);
    assert!(
        audits[0]
            .details
            .as_deref()
            .unwrap()
            .contains("github_token")
    );
    for audit in audits
        .into_iter()
        .chain(db.list_audit_by_target("session", &id, None).unwrap())
    {
        let details = audit.details.unwrap_or_default();
        assert!(!details.contains(&token));
        assert!(!details.contains(&latest.source_path));
    }
}

#[test]
fn changed_discovery_metadata_reindexes_even_without_new_evidence() {
    let (db, workspace, id, mut session, spans) = fixture(1, &["Observation."]);
    session.ended_at = Some("2026-09-20T02:00:00Z".to_owned());
    session.token_count = Some(1234);
    let report = refresh_session(&db, &workspace, &id, &session, &spans).unwrap();
    assert!(report.changed);
    assert!(report.added_lines.is_empty());
    assert!(report.index_job_id.is_some());
    let stored = db.get_session(&id).unwrap().unwrap();
    assert_eq!(stored.ended_at, session.ended_at);
    assert_eq!(stored.token_count, Some(1234));
}

#[test]
fn job_insert_failure_rolls_back_new_evidence_and_metadata() {
    let (db, workspace, id, _, mut spans) = fixture(1, &["Before failure."]);
    let before = db.get_session(&id).unwrap().unwrap();
    db.execute_raw("CREATE TRIGGER reject_refresh_job BEFORE INSERT ON search_index_jobs BEGIN SELECT RAISE(ABORT, 'refresh-test-injected-failure'); END").unwrap();
    let latest = discovered(2);
    spans.push(span(&latest, 2, "Must roll back with job failure."));
    assert!(refresh_session(&db, &workspace, &id, &latest, &spans).is_err());
    assert_eq!(db.get_session(&id).unwrap().unwrap(), before);
    assert_eq!(db.list_evidence_spans_for_session(&id).unwrap().len(), 1);
    assert_eq!(db.count_table_rows("search_index_jobs").unwrap(), 1);
    assert!(
        db.list_audit_by_target("session", &id, None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn foreign_scope_source_and_duplicate_inputs_never_mutate_the_session() {
    let (db, workspace, id, session, spans) = fixture(1, &["Original observation."]);
    let before = db.get_session(&id).unwrap().unwrap();
    let foreign = WorkspaceId::from_uuid(uuid::Uuid::from_u128(8702)).to_string();
    assert!(refresh_session(&db, &foreign, &id, &session, &spans).is_err());
    let mut wrong_source = session.clone();
    wrong_source.source_path = "/private/other-source".to_owned();
    assert!(refresh_session(&db, &workspace, &id, &wrong_source, &spans).is_err());
    let duplicates = vec![spans[0].clone(), spans[0].clone()];
    assert!(refresh_session(&db, &workspace, &id, &session, &duplicates).is_err());
    assert_eq!(db.get_session(&id).unwrap().unwrap(), before);
    assert_eq!(db.list_evidence_spans_for_session(&id).unwrap().len(), 1);
}

#[test]
fn refresh_revision_is_input_order_independent_and_binds_metadata() {
    let session = discovered(3);
    let spans = [
        span(&session, 1, "One observation."),
        span(&session, 2, "Two observations."),
        span(&session, 3, "Three observations."),
    ];
    let ordered: BTreeMap<_, _> = spans.iter().map(|s| (s.cass_span_id.as_str(), s)).collect();
    let reversed: BTreeMap<_, _> = spans
        .iter()
        .rev()
        .map(|s| (s.cass_span_id.as_str(), s))
        .collect();
    let input = session_input("workspace", &session);
    let revision = snapshot_revision("workspace", "session", &input, &ordered);
    assert_eq!(
        revision,
        snapshot_revision("workspace", "session", &input, &reversed)
    );
    assert_ne!(
        revision,
        snapshot_revision("other-workspace", "session", &input, &ordered)
    );
    assert_ne!(
        revision,
        snapshot_revision("workspace", "other-session", &input, &ordered)
    );
    let mut changed = session.clone();
    changed.ended_at = Some("2026-09-20T02:00:00Z".to_owned());
    assert_ne!(
        revision,
        snapshot_revision(
            "workspace",
            "session",
            &session_input("workspace", &changed),
            &ordered
        )
    );
}

#[test]
fn metadata_values_with_quotes_unicode_and_sql_text_roundtrip_as_data() {
    let (db, workspace, id, mut session, spans) = fixture(1, &["Observation."]);
    session.workspace_dir = Some("/private/Zoë's 日本語/'); DROP TABLE sessions; --".to_owned());
    refresh_session(&db, &workspace, &id, &session, &spans).unwrap();
    let stored = db.get_session(&id).unwrap().unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(stored.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        metadata["workspaceDir"].as_str(),
        session.workspace_dir.as_deref()
    );
    assert_eq!(db.list_sessions(&workspace).unwrap().len(), 1);
    let again = refresh_session(&db, &workspace, &id, &session, &spans).unwrap();
    assert!(!again.changed);
}

#[test]
fn revisiting_a_prior_payload_gets_new_work_not_a_completed_old_job() {
    let (db, workspace, id, mut session, spans) = fixture(1, &["Stable transcript."]);
    completed(&db, &stable_search_index_job_id(&workspace, &id));
    let mut jobs = BTreeSet::new();
    for tokens in [100, 200, 100, 200] {
        session.token_count = Some(tokens);
        let report = refresh_session(&db, &workspace, &id, &session, &spans).unwrap();
        assert!(report.changed);
        assert!(report.added_lines.is_empty());
        let job = report
            .index_job_id
            .expect("each transition needs publication");
        assert!(jobs.insert(job.clone()), "must not reuse completed work");
        completed(&db, &job);
        let retry = refresh_session(&db, &workspace, &id, &session, &spans).unwrap();
        assert!(!retry.changed);
        assert!(retry.index_job_id.is_none());
    }
    assert_eq!(db.list_evidence_spans_for_session(&id).unwrap().len(), 1);
    assert_eq!(
        db.list_audit_by_target("session", &id, None).unwrap().len(),
        4
    );
}

#[test]
fn unrelated_session_annotations_survive_refresh_and_do_not_force_writes() {
    let (db, workspace, id, _, mut spans) = fixture(1, &["First result."]);
    let stored = db.get_session(&id).unwrap().unwrap();
    let mut metadata: serde_json::Value =
        serde_json::from_str(stored.metadata_json.as_deref().unwrap()).unwrap();
    metadata["operatorAnnotation"] = json!({"retain": "private note"});
    db.execute_raw(&format!(
        "UPDATE sessions SET metadata_json = {} WHERE id = {}",
        sql_text(&metadata.to_string()),
        sql_text(&id),
    ))
    .unwrap();
    let latest = discovered(2);
    spans.push(span(&latest, 2, "Second result."));
    refresh_session(&db, &workspace, &id, &latest, &spans).unwrap();
    let stored = db.get_session(&id).unwrap().unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(stored.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        metadata["operatorAnnotation"],
        json!({"retain": "private note"})
    );
    assert_eq!(metadata[CHECKPOINT_KEY]["schema"], CHECKPOINT_SCHEMA);
    assert!(
        !refresh_session(&db, &workspace, &id, &latest, &spans)
            .unwrap()
            .changed
    );
}

#[test]
fn an_older_view_cannot_displace_evidence_committed_by_a_newer_refresh() {
    let (db, workspace, id, _, initial) = fixture(1, &["Original result."]);
    let middle = discovered(2);
    let mut middle_spans = initial;
    middle_spans.push(span(&middle, 2, "Second result."));
    let latest = discovered(3);
    let mut latest_spans = middle_spans.clone();
    latest_spans.push(span(&latest, 3, "Already committed third result."));
    refresh_session(&db, &workspace, &id, &latest, &latest_spans).unwrap();
    let before = db.get_session(&id).unwrap().unwrap();
    let error = refresh_session(&db, &workspace, &id, &middle, &middle_spans)
        .err()
        .unwrap();
    assert!(error.to_string().contains("cass_refresh_history_missing"));
    assert_eq!(db.get_session(&id).unwrap().unwrap(), before);
    assert_eq!(db.list_evidence_spans_for_session(&id).unwrap().len(), 3);
}

#[test]
fn malformed_checkpoints_do_not_authorize_a_completed_publication() {
    let (db, workspace, id, session, spans) = fixture(1, &["Original result."]);
    let stored = db.get_session(&id).unwrap().unwrap();
    let mut metadata: serde_json::Value =
        serde_json::from_str(stored.metadata_json.as_deref().unwrap()).unwrap();
    metadata[CHECKPOINT_KEY] =
        json!({"schema": CHECKPOINT_SCHEMA, "indexJobId": "PRIVATE_SENTINEL"});
    db.execute_raw(&format!(
        "UPDATE sessions SET metadata_json = {} WHERE id = {}",
        sql_text(&metadata.to_string()),
        sql_text(&id),
    ))
    .unwrap();
    let error = refresh_session(&db, &workspace, &id, &session, &spans)
        .err()
        .unwrap();
    assert!(
        error
            .to_string()
            .contains("cass_refresh_checkpoint_invalid")
    );
    assert!(!error.to_string().contains("PRIVATE_SENTINEL"));
    assert_eq!(db.list_evidence_spans_for_session(&id).unwrap().len(), 1);
}
