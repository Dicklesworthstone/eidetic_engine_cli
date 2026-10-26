use super::*;
use crate::db::{CreateEvidenceSpanInput, CreateSessionInput, CreateWorkspaceInput};
use crate::models::{EvidenceId, SessionId};

const WS: &str = "wsp_01234567890123456789012345";
const OTHER: &str = "wsp_01234567890123456789012346";
const SOURCE: &str = "The cache commits its transaction before publishing the new checksum.";

fn database() -> DbConnection {
    let db = DbConnection::open_memory().expect("database");
    db.migrate().expect("migrate");
    for (id, path) in [
        (WS, "/tmp/session-withdrawal"),
        (OTHER, "/tmp/session-withdrawal-other"),
    ] {
        db.insert_workspace(
            id,
            &CreateWorkspaceInput {
                path: path.to_owned(),
                name: None,
            },
        )
        .expect("workspace");
    }
    db
}

fn session(db: &DbConnection, workspace: &str, seed: u128) -> String {
    let id = SessionId::from_uuid(uuid::Uuid::from_u128(seed)).to_string();
    db.insert_session(
        &id,
        &CreateSessionInput {
            workspace_id: workspace.to_owned(),
            cass_session_id: format!("session-withdrawal-{seed}"),
            source_path: None,
            agent_name: None,
            model: None,
            started_at: None,
            ended_at: None,
            message_count: 0,
            token_count: None,
            content_hash: format!("blake3:{}", blake3::hash(SOURCE.as_bytes())),
            metadata_json: None,
        },
    )
    .expect("session");
    id
}

fn span(
    db: &DbConnection,
    workspace: &str,
    session: &str,
    seed: u32,
    producer: EvidenceProducerKind,
) -> String {
    let id = EvidenceId::from_uuid(uuid::Uuid::from_u128(u128::from(seed))).to_string();
    db.insert_evidence_span(
        &id,
        &CreateEvidenceSpanInput {
            workspace_id: workspace.to_owned(),
            session_id: session.to_owned(),
            memory_id: None,
            producer_kind: producer,
            cass_span_id: format!("{session}:{seed}"),
            span_kind: "message".to_owned(),
            start_line: seed,
            end_line: seed,
            start_byte: None,
            end_byte: None,
            role: Some("assistant".to_owned()),
            content_hash: format!("blake3:{}", blake3::hash(SOURCE.as_bytes())),
            excerpt: SOURCE.to_owned(),
            metadata_json: None,
            inherited_redaction_classes: Vec::new(),
        },
    )
    .expect("source");
    id
}

fn withdraw(db: &DbConnection, session: &str, preview: bool) -> CassSessionWithdrawal {
    db.with_transaction_error(|| {
        db.withdraw_cass_session_in_current_snapshot(
            WS,
            session,
            "Withdraw the retained observations.",
            Some("session-test"),
            preview,
        )
    })
    .expect("withdraw session")
}

#[test]
fn session_withdrawal_is_one_atomic_receipt_and_preserves_all_source_bytes() {
    let db = database();
    let selected = session(&db, WS, 1);
    let unrelated = session(&db, WS, 2);
    let a = span(&db, WS, &selected, 1, EvidenceProducerKind::CassImport);
    let b = span(&db, WS, &selected, 2, EvidenceProducerKind::CassImport);
    let denied = span(&db, WS, &selected, 3, EvidenceProducerKind::CassImport);
    let native = span(&db, WS, &selected, 4, EvidenceProducerKind::DocsBootstrap);
    let other = span(&db, WS, &unrelated, 5, EvidenceProducerKind::CassImport);
    db.with_transaction_error(|| {
        db.withdraw_cass_evidence_in_current_snapshot(
            WS,
            &denied,
            "Earlier withdrawal.",
            None,
            false,
        )
    })
    .expect("earlier withdrawal");
    let before = db
        .list_evidence_spans_for_session(&selected)
        .expect("before");
    let other_before = db.get_evidence_span(&other).expect("other");
    let audits = db.list_audit_entries(Some(WS), None).expect("audits").len();
    let jobs = db.list_search_index_jobs(WS, None).expect("jobs").len();
    let result = withdraw(&db, &selected, false);
    assert_eq!(result.retained_evidence_count, 4);
    assert_eq!(result.cass_evidence_count, 3);
    assert_eq!(result.excluded_producer_count, 1);
    assert_eq!(result.already_withdrawn_count, 1);
    assert_eq!(result.would_withdraw_count, 2);
    assert!(result.persisted);
    assert_eq!(
        db.list_audit_entries(Some(WS), None).unwrap().len(),
        audits + 1
    );
    assert_eq!(db.list_search_index_jobs(WS, None).unwrap().len(), jobs + 1);
    for original in before {
        let current = db.get_evidence_span(&original.id).unwrap().unwrap();
        if original.id == native || original.id == denied {
            assert_eq!(current, original, "untouched source changed");
            continue;
        }
        assert_eq!(current.search_eligibility, "denied");
        assert_eq!(current.pack_eligibility, "denied");
        assert_eq!(current.excerpt, original.excerpt);
        assert_eq!(current.content_hash, original.content_hash);
        assert_eq!(
            current.canonical_excerpt_hash,
            original.canonical_excerpt_hash
        );
        assert_eq!(current.created_at, original.created_at);
        assert_eq!(
            current.canonical_provenance_uri(),
            original.canonical_provenance_uri()
        );
        assert!(
            !current
                .is_class_a_derivation_readable(WS, &db.get_session(&selected).unwrap().unwrap())
        );
    }
    for id in [&a, &b] {
        assert!(
            db.get_search_admitted_evidence_span(id, WS)
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(db.get_evidence_span(&other).unwrap(), other_before);
    let audit = db
        .get_audit(result.audit_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(audit.target_type.as_deref(), Some("session"));
    assert_eq!(audit.target_id.as_deref(), Some(selected.as_str()));
    assert!(audit.before_hash.is_some() && audit.after_hash.is_some());
    assert_ne!(audit.before_hash, audit.after_hash);
    assert!(!audit.details.as_deref().unwrap().contains(SOURCE));
    let job = db
        .get_search_index_job(result.index_job_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(job.document_source.as_deref(), Some("session"));
    assert_eq!(job.document_id.as_deref(), Some(selected.as_str()));
    assert_eq!(job.status, "pending");
    assert_eq!(job.documents_indexed, 0);
}

#[test]
fn session_preview_and_repeats_create_no_receipts_or_generation_changes() {
    let db = database();
    let selected = session(&db, WS, 10);
    span(&db, WS, &selected, 10, EvidenceProducerKind::CassImport);
    let before = db.list_evidence_spans_for_session(&selected).unwrap();
    let generation = db.get_workspace_generation(WS).unwrap();
    let preview = withdraw(&db, &selected, true);
    assert_eq!(preview.would_withdraw_count, 1);
    assert!(!preview.persisted);
    assert!(preview.audit_id.is_none() && preview.index_job_id.is_none());
    assert_eq!(
        db.list_evidence_spans_for_session(&selected).unwrap(),
        before
    );
    assert_eq!(db.get_workspace_generation(WS).unwrap(), generation);
    assert!(db.list_audit_entries(Some(WS), None).unwrap().is_empty());
    assert!(db.list_search_index_jobs(WS, None).unwrap().is_empty());
    withdraw(&db, &selected, false);
    let generation = db.get_workspace_generation(WS).unwrap();
    let audits = db.list_audit_entries(Some(WS), None).unwrap();
    let jobs = db.list_search_index_jobs(WS, None).unwrap();
    let denied = db.list_evidence_spans_for_session(&selected).unwrap();
    for preview in [true, false] {
        let result = withdraw(&db, &selected, preview);
        assert_eq!(result.would_withdraw_count, 0);
        assert_eq!(result.already_withdrawn_count, 1);
        assert!(!result.persisted);
        assert!(result.audit_id.is_none() && result.index_job_id.is_none());
        assert_eq!(
            db.list_evidence_spans_for_session(&selected).unwrap(),
            denied
        );
        assert_eq!(db.get_workspace_generation(WS).unwrap(), generation);
        assert_eq!(db.list_audit_entries(Some(WS), None).unwrap(), audits);
        assert_eq!(db.list_search_index_jobs(WS, None).unwrap(), jobs);
    }
}

#[test]
fn session_withdrawal_refuses_foreign_owners_and_malformed_final_sources() {
    let db = database();
    let foreign = session(&db, OTHER, 20);
    span(&db, OTHER, &foreign, 20, EvidenceProducerKind::CassImport);
    assert!(matches!(
        db.with_transaction_error(|| db.withdraw_cass_session_in_current_snapshot(
            WS,
            &foreign,
            "Not mine.",
            None,
            false,
        )),
        Err(CassEvidenceWithdrawalError::NotFound)
    ));
    let selected = session(&db, WS, 21);
    let valid = span(&db, WS, &selected, 21, EvidenceProducerKind::CassImport);
    let malformed = span(&db, WS, &selected, 22, EvidenceProducerKind::CassImport);
    db.execute_raw(&format!(
        "UPDATE evidence_spans SET metadata_json = '{{}}' WHERE id = '{malformed}'"
    ))
    .unwrap();
    let original = db.get_evidence_span(&valid).unwrap();
    let generation = db.get_workspace_generation(WS).unwrap();
    for preview in [true, false] {
        assert!(matches!(
            db.with_transaction_error(|| db.withdraw_cass_session_in_current_snapshot(
                WS,
                &selected,
                "Withdraw all.",
                None,
                preview,
            )),
            Err(CassEvidenceWithdrawalError::InvalidMetadata)
        ));
        assert_eq!(db.get_evidence_span(&valid).unwrap(), original);
        assert_eq!(db.get_workspace_generation(WS).unwrap(), generation);
        assert!(db.list_audit_entries(Some(WS), None).unwrap().is_empty());
        assert!(db.list_search_index_jobs(WS, None).unwrap().is_empty());
    }
}

#[test]
fn failed_session_receipts_roll_back_every_denial_and_job() {
    for table in ["search_index_jobs", "audit_log"] {
        let db = database();
        let selected = session(&db, WS, 30);
        for seed in 30..35 {
            span(&db, WS, &selected, seed, EvidenceProducerKind::CassImport);
        }
        let before = db.list_evidence_spans_for_session(&selected).unwrap();
        db.execute_raw(&format!("CREATE TRIGGER reject_session_withdrawal BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT, 'session receipt failure'); END")).unwrap();
        let generation = db.get_workspace_generation(WS).unwrap();
        assert!(
            db.with_transaction_error(|| db.withdraw_cass_session_in_current_snapshot(
                WS,
                &selected,
                "Withdraw retained sources.",
                None,
                false,
            ))
            .is_err()
        );
        assert_eq!(
            db.list_evidence_spans_for_session(&selected).unwrap(),
            before
        );
        assert_eq!(db.get_workspace_generation(WS).unwrap(), generation);
        assert!(db.list_audit_entries(Some(WS), None).unwrap().is_empty());
        assert!(db.list_search_index_jobs(WS, None).unwrap().is_empty());
    }
}

#[test]
fn an_empty_or_non_cass_session_is_a_noop_and_later_imports_are_not_disabled() {
    let db = database();
    let selected = session(&db, WS, 40);
    let empty = withdraw(&db, &selected, false);
    assert_eq!(empty.retained_evidence_count, 0);
    assert!(!empty.persisted);
    let native = span(&db, WS, &selected, 40, EvidenceProducerKind::DocsBootstrap);
    let before = db.get_evidence_span(&native).unwrap();
    let excluded = withdraw(&db, &selected, false);
    assert_eq!(excluded.excluded_producer_count, 1);
    assert_eq!(excluded.cass_evidence_count, 0);
    assert!(!excluded.persisted);
    let first = span(&db, WS, &selected, 41, EvidenceProducerKind::CassImport);
    withdraw(&db, &selected, false);
    let later = span(&db, WS, &selected, 42, EvidenceProducerKind::CassImport);
    assert!(
        db.get_search_admitted_evidence_span(&later, WS)
            .unwrap()
            .is_some()
    );
    assert!(
        db.get_search_admitted_evidence_span(&first, WS)
            .unwrap()
            .is_none()
    );
    let next = withdraw(&db, &selected, false);
    assert_eq!(next.would_withdraw_count, 1);
    assert_eq!(next.already_withdrawn_count, 1);
    assert_eq!(db.get_evidence_span(&native).unwrap(), before);
}

#[test]
fn session_audit_redacts_the_reason_and_actor_without_copying_transcripts() {
    let db = database();
    let selected = session(&db, WS, 50);
    span(&db, WS, &selected, 50, EvidenceProducerKind::CassImport);
    let secret = format!("sk-{}", "withdrawtest".repeat(6));
    let reason = format!("Withdraw after api_key={secret}");
    let result = db
        .with_transaction_error(|| {
            db.withdraw_cass_session_in_current_snapshot(
                WS,
                &selected,
                &reason,
                Some(&reason),
                false,
            )
        })
        .unwrap();
    let audit = db
        .get_audit(result.audit_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    assert!(!audit.details.as_deref().unwrap().contains(&secret));
    assert!(!audit.details.as_deref().unwrap().contains(SOURCE));
    assert!(!audit.actor.as_deref().unwrap().contains(&secret));
    let details: serde_json::Value =
        serde_json::from_str(audit.details.as_deref().unwrap()).unwrap();
    assert_eq!(details["scope"], "retained_sources");
    assert_eq!(details["futureImports"], "unchanged");
    assert_eq!(details["withdrawnCount"], 1);
    assert_eq!(details["sourceBytesRetained"], true);
}
