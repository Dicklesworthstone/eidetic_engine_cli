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
    let spans = vec![
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
